//! Byte-level lexer over the whole file buffer. Spec 5.2.
//!
//! The lexer never allocates for structure, only for the token payloads that
//! genuinely need it (strings and names, which carry their raw bytes). It is
//! shared by the object parser, the xref parser, and -- once `rasura-content`
//! exists -- the content-stream tokenizer, which has the same lexical rules.

use crate::error::{Leniency, LeniencyKind};
use crate::object::{Name, PdfString, is_regular, is_whitespace};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Integer(i64),
    Real(f64),
    String(PdfString),
    Name(Name),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    /// `{` / `}`. Illegal in ordinary object syntax, legal inside a Type 4
    /// (PostScript calculator) function stream.
    BraceOpen,
    BraceClose,
    /// A bare keyword: `obj`, `endobj`, `stream`, `R`, `true`, `xref`, ...
    Keyword(Box<[u8]>),
    Eof,
}

impl PartialEq for PdfString {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes() && self.form() == other.form()
    }
}

#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Range<usize>,
}

pub struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Deviations tolerated while lexing. Drained by the caller into the
    /// document-level log.
    leniencies: Vec<Leniency>,
}

impl<'a> Lexer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Lexer { buf, pos: 0, leniencies: Vec::new() }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Lexer { buf, pos, leniencies: Vec::new() }
    }

    pub fn buf(&self) -> &'a [u8] {
        self.buf
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.buf.len());
    }

    pub fn at_eof(&mut self) -> bool {
        self.skip_whitespace();
        self.pos >= self.buf.len()
    }

    pub fn take_leniencies(&mut self) -> Vec<Leniency> {
        std::mem::take(&mut self.leniencies)
    }

    pub(crate) fn note(&mut self, kind: LeniencyKind, offset: usize, detail: impl Into<String>) {
        self.leniencies.push(Leniency::new(kind, offset, detail));
    }

    fn peek_byte(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// Skip whitespace and comments. A comment runs from `%` to end of line;
    /// `%PDF-x.y` and `%%EOF` are ordinary comments as far as lexing is
    /// concerned.
    pub fn skip_whitespace(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b) if is_whitespace(b) => self.pos += 1,
                Some(b'%') => {
                    while let Some(b) = self.peek_byte() {
                        if b == b'\n' || b == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// Consume the end-of-line sequence at the cursor, if any. All three
    /// conventions are accepted. Returns true if something was consumed.
    pub fn skip_eol(&mut self) -> bool {
        match self.peek_byte() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek_byte() == Some(b'\n') {
                    self.pos += 1;
                }
                true
            }
            Some(b'\n') => {
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    pub fn next_token(&mut self) -> SpannedToken {
        self.skip_whitespace();
        let start = self.pos;
        let Some(b) = self.peek_byte() else {
            return SpannedToken { token: Token::Eof, span: start..start };
        };

        let token = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose
            }
            b'{' => {
                self.pos += 1;
                Token::BraceOpen
            }
            b'}' => {
                self.pos += 1;
                Token::BraceClose
            }
            b'<' => {
                if self.buf.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Token::DictOpen
                } else {
                    self.lex_hex_string()
                }
            }
            b'>' => {
                if self.buf.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Token::DictClose
                } else {
                    // A stray `>`; skip it and carry on rather than deadlocking.
                    self.pos += 1;
                    self.note(LeniencyKind::UnknownKeyword, start, "stray '>'");
                    return self.next_token();
                }
            }
            b'(' => self.lex_literal_string(),
            b'/' => self.lex_name(),
            b')' => {
                // Unbalanced close paren outside a string. Skip.
                self.pos += 1;
                self.note(LeniencyKind::UnknownKeyword, start, "stray ')'");
                return self.next_token();
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.lex_number(),
            _ => self.lex_keyword(),
        };

        SpannedToken { token, span: start..self.pos }
    }

    /// Look at the next token without consuming it.
    pub fn peek_token(&mut self) -> SpannedToken {
        let save = self.pos;
        let t = self.next_token();
        self.pos = save;
        t
    }

    fn lex_literal_string(&mut self) -> Token {
        debug_assert_eq!(self.peek_byte(), Some(b'('));
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(b) = self.peek_byte() {
            match b {
                b'\\' => {
                    // Skip the escaped byte wholesale so an escaped paren does
                    // not affect nesting depth.
                    self.pos += 2;
                    continue;
                }
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let raw = &self.buf[start..self.pos];
                        self.pos += 1;
                        return Token::String(PdfString::from_raw_literal(raw));
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        // Unterminated: take what we have.
        let end = self.buf.len();
        self.note(LeniencyKind::UnknownKeyword, start, "unterminated literal string");
        self.pos = end;
        Token::String(PdfString::from_raw_literal(&self.buf[start..end]))
    }

    fn lex_hex_string(&mut self) -> Token {
        debug_assert_eq!(self.peek_byte(), Some(b'<'));
        self.pos += 1;
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if b == b'>' {
                break;
            }
            self.pos += 1;
        }
        let raw = &self.buf[start..self.pos];
        let (s, padded) = PdfString::from_raw_hex(raw);
        if padded {
            self.note(LeniencyKind::OddHexDigit, start, "odd hex digit count, padded with 0");
        }
        if self.peek_byte() == Some(b'>') {
            self.pos += 1;
        }
        Token::String(s)
    }

    fn lex_name(&mut self) -> Token {
        debug_assert_eq!(self.peek_byte(), Some(b'/'));
        self.pos += 1;
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
        }
        Token::Name(Name::from_raw(&self.buf[start..self.pos]))
    }

    fn lex_number(&mut self) -> Token {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
        }
        let text = &self.buf[start..self.pos];
        self.classify_number(text, start)
    }

    fn classify_number(&mut self, text: &[u8], offset: usize) -> Token {
        let has_dot = text.contains(&b'.');
        let has_exp = text.iter().any(|&b| b == b'e' || b == b'E');

        if !has_dot && !has_exp {
            // Integer. May carry a leading sign; may exceed i64.
            if let Some(v) = parse_int(text) {
                return Token::Integer(v);
            }
            // Out of range: clamp per spec 5.2 rather than panic.
            let negative = text.first() == Some(&b'-');
            if text.iter().skip(1).all(|b| b.is_ascii_digit())
                && text.iter().any(|b| b.is_ascii_digit())
            {
                self.note(
                    LeniencyKind::IntegerClamped,
                    offset,
                    format!("integer out of range: {}", String::from_utf8_lossy(text)),
                );
                return Token::Integer(if negative { i64::MIN } else { i64::MAX });
            }
        }

        if has_exp {
            // Illegal in ISO 32000-1 but common enough to accept.
            self.note(
                LeniencyKind::ExponentInReal,
                offset,
                format!("exponent notation: {}", String::from_utf8_lossy(text)),
            );
        }

        match parse_real(text) {
            Some(v) => Token::Real(v),
            None => {
                self.note(
                    LeniencyKind::MalformedNumber,
                    offset,
                    format!("unparseable number: {}", String::from_utf8_lossy(text)),
                );
                Token::Integer(0)
            }
        }
    }

    fn lex_keyword(&mut self) -> Token {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            // A delimiter we do not otherwise handle. Consume it so the caller
            // cannot spin.
            self.pos += 1;
        }
        Token::Keyword(self.buf[start..self.pos].into())
    }

    /// Read exactly `n` bytes from the cursor, as stream data does.
    pub fn take_bytes(&mut self, n: usize) -> &'a [u8] {
        let end = (self.pos + n).min(self.buf.len());
        let out = &self.buf[self.pos..end];
        self.pos = end;
        out
    }
}

/// `+17`, `-3`, `42`. Rejects anything else so the caller can fall through to
/// the lenient paths.
fn parse_int(text: &[u8]) -> Option<i64> {
    let (sign, digits) = match text.first()? {
        b'+' => (1i64, &text[1..]),
        b'-' => (-1i64, &text[1..]),
        _ => (1i64, text),
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for &d in digits {
        v = v.checked_mul(10)?.checked_add((d - b'0') as i64)?;
    }
    Some(sign * v)
}

/// `.5`, `-.002`, `4.`, `34.5`, `+17`, and leniently `6.02e23`, `--5`, `1.2.3`.
fn parse_real(text: &[u8]) -> Option<f64> {
    let mut cleaned = String::with_capacity(text.len() + 2);
    let mut seen_dot = false;
    let mut seen_digit = false;
    let mut seen_exp = false;
    let mut i = 0;

    // Leading signs: take the last one, which is how every real-world parser
    // treats the `--5` that some producers emit.
    let mut negative = false;
    while let Some(&b) = text.get(i) {
        match b {
            b'-' => {
                negative = !negative;
                i += 1;
            }
            b'+' => i += 1,
            _ => break,
        }
    }
    if negative {
        cleaned.push('-');
    }

    while let Some(&b) = text.get(i) {
        match b {
            b'0'..=b'9' => {
                cleaned.push(b as char);
                seen_digit = true;
            }
            b'.' if !seen_dot && !seen_exp => {
                cleaned.push('.');
                seen_dot = true;
            }
            // A second dot ends the number; `1.2.3` reads as `1.2`.
            b'.' => break,
            b'e' | b'E' if seen_digit && !seen_exp => {
                cleaned.push('e');
                seen_exp = true;
                // The exponent carries its own optional sign.
                if let Some(&s @ (b'+' | b'-')) = text.get(i + 1) {
                    cleaned.push(s as char);
                    i += 1;
                }
            }
            _ => break,
        }
        i += 1;
    }

    if !seen_digit {
        return None;
    }
    if cleaned.ends_with('.') {
        cleaned.push('0');
    }
    if cleaned.ends_with('e') || cleaned.ends_with("e-") || cleaned.ends_with("e+") {
        while !cleaned.ends_with(|c: char| c.is_ascii_digit()) {
            cleaned.pop();
        }
    }
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(src: &[u8]) -> Vec<Token> {
        let mut lx = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let t = lx.next_token();
            if t.token == Token::Eof {
                break;
            }
            out.push(t.token);
        }
        out
    }

    #[test]
    fn skips_comments_including_header() {
        let t = tokens(b"%PDF-1.7\n% a comment\n42");
        assert_eq!(t, vec![Token::Integer(42)]);
    }

    #[test]
    fn handles_all_real_forms() {
        let t = tokens(b".5 -.002 4. 34.5 +17 -0");
        assert_eq!(
            t,
            vec![
                Token::Real(0.5),
                Token::Real(-0.002),
                Token::Real(4.0),
                Token::Real(34.5),
                Token::Integer(17),
                Token::Integer(0),
            ]
        );
    }

    #[test]
    fn accepts_exponent_and_records_leniency() {
        let mut lx = Lexer::new(b"6.02e23");
        let t = lx.next_token();
        assert!(matches!(t.token, Token::Real(v) if (v - 6.02e23).abs() < 1e15));
        let l = lx.take_leniencies();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].kind, LeniencyKind::ExponentInReal);
    }

    #[test]
    fn clamps_oversized_integer_without_panicking() {
        let mut lx = Lexer::new(b"99999999999999999999999");
        assert_eq!(lx.next_token().token, Token::Integer(i64::MAX));
        assert_eq!(lx.take_leniencies()[0].kind, LeniencyKind::IntegerClamped);
    }

    #[test]
    fn nested_parens_and_escapes_in_literal_strings() {
        let t = tokens(br"(a (nested (deep)) b) (esc\) close)");
        match &t[0] {
            Token::String(s) => assert_eq!(s.as_bytes(), b"a (nested (deep)) b"),
            other => panic!("{other:?}"),
        }
        match &t[1] {
            Token::String(s) => assert_eq!(s.as_bytes(), b"esc) close"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn distinguishes_dict_open_from_hex_string() {
        let t = tokens(b"<< /A <414243> >>");
        assert_eq!(t.len(), 4);
        assert!(matches!(t[0], Token::DictOpen));
        assert!(matches!(t[1], Token::Name(_)));
        match &t[2] {
            Token::String(s) => assert_eq!(s.as_bytes(), b"ABC"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(t[3], Token::DictClose));
    }

    #[test]
    fn name_terminates_at_delimiter() {
        let t = tokens(b"/Type/Page");
        assert_eq!(t.len(), 2);
        match (&t[0], &t[1]) {
            (Token::Name(a), Token::Name(b)) => {
                assert_eq!(a.as_bytes(), b"Type");
                assert_eq!(b.as_bytes(), b"Page");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keywords_lex_whole() {
        let t = tokens(b"1 0 obj endobj stream R true");
        assert_eq!(
            &t[2..],
            &[
                Token::Keyword(b"obj".to_vec().into_boxed_slice()),
                Token::Keyword(b"endobj".to_vec().into_boxed_slice()),
                Token::Keyword(b"stream".to_vec().into_boxed_slice()),
                Token::Keyword(b"R".to_vec().into_boxed_slice()),
                Token::Keyword(b"true".to_vec().into_boxed_slice()),
            ]
        );
    }

    #[test]
    fn stray_delimiters_do_not_spin() {
        // Regression guard: a malformed file must not put the lexer in a loop.
        let t = tokens(b") > ) 5");
        assert_eq!(t, vec![Token::Integer(5)]);
    }

    #[test]
    fn all_three_eol_conventions() {
        assert_eq!(
            tokens(b"1\r2\n3\r\n4"),
            vec![Token::Integer(1), Token::Integer(2), Token::Integer(3), Token::Integer(4)]
        );
    }
}
