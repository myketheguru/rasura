//! Content-stream tokenizer. Spec 6.1 and 6.2.
//!
//! Reuses `rasura_cos::lexer::Lexer`, because content streams and object
//! syntax share their lexical rules exactly -- same whitespace, same
//! delimiters, same string and name escapes. What differs is the grammar above
//! the tokens: operands accumulate until a keyword, which is the operator, and
//! there are no indirect references.
//!
//! That last point matters. The object parser treats `1 0 R` as a reference;
//! a content stream has no `R` operator, but writing a content-specific operand
//! collector rather than reusing `Parser::parse_object` means the question never
//! arises, and a stream containing `1 0 R` through corruption tokenises as two
//! numbers and an unknown operator instead of silently becoming a reference.

use crate::op::{InlineImage, Op, OpKind};
use rasura_cos::error::{Leniency, LeniencyKind};
use rasura_cos::lexer::{Lexer, Token};
use rasura_cos::object::{Dictionary, Name, Object, is_whitespace};
use smallvec::SmallVec;

/// Operand lists longer than this are a malformed stream, not a real operator.
/// `TJ` arrays are a single operand, so nothing legitimate comes close.
const MAX_OPERANDS: usize = 64;

/// Guard against runaway nesting inside a `TJ` array or a `BDC` property
/// dictionary.
const MAX_DEPTH: usize = 32;

pub struct Tokenizer<'a> {
    lexer: Lexer<'a>,
    buf: &'a [u8],
    leniencies: Vec<Leniency>,
    /// Depth of `BX`/`EX` nesting. Unknown operators inside are skipped
    /// silently, per spec 6.1; outside they are recorded.
    compat_depth: usize,
    /// Set once the tokenizer decides the stream is unusable, so `next` stops
    /// rather than looping.
    exhausted: bool,
}

impl<'a> Tokenizer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Tokenizer {
            lexer: Lexer::new(buf),
            buf,
            leniencies: Vec::new(),
            compat_depth: 0,
            exhausted: false,
        }
    }

    pub fn take_leniencies(&mut self) -> Vec<Leniency> {
        let mut out = std::mem::take(&mut self.leniencies);
        out.extend(self.lexer.take_leniencies());
        out
    }

    /// Collect every operator in the stream.
    pub fn collect_ops(mut self) -> (Vec<Op>, Vec<Leniency>) {
        let mut ops = Vec::new();
        while let Some(op) = self.next_op() {
            ops.push(op);
        }
        let leniencies = self.take_leniencies();
        (ops, leniencies)
    }

    /// The next operator, or `None` at end of stream.
    pub fn next_op(&mut self) -> Option<Op> {
        if self.exhausted {
            return None;
        }
        let mut operands: SmallVec<[Object; 4]> = SmallVec::new();
        // The span starts at the first operand, not at the operator, so that
        // replacing an operator replaces its arguments too.
        let mut start: Option<usize> = None;

        loop {
            let st = self.lexer.next_token();
            match st.token {
                Token::Eof => {
                    if !operands.is_empty() {
                        self.note(
                            LeniencyKind::UnknownKeyword,
                            st.span.start,
                            format!("{} trailing operand(s) with no operator", operands.len()),
                        );
                    }
                    self.exhausted = true;
                    return None;
                }

                Token::Keyword(kw) => {
                    let span_start = start.unwrap_or(st.span.start);

                    // `BI` has to be handled before the keyword table, because
                    // it is not an operator at all: it opens an inline image
                    // whose parameters are key/value pairs and whose payload is
                    // binary. The table would classify it as Unknown.
                    if &*kw == b"BI" {
                        return Some(self.read_inline_image(span_start));
                    }

                    let kind = OpKind::from_keyword(&kw);
                    match kind {
                        OpKind::BeginCompat => self.compat_depth += 1,
                        OpKind::EndCompat => {
                            self.compat_depth = self.compat_depth.saturating_sub(1)
                        }
                        OpKind::Unknown => {
                            // ISO 32000-1 §7.8.2: unknown operators inside
                            // BX/EX are to be ignored silently. Outside, they
                            // are worth telling the caller about.
                            if self.compat_depth == 0 {
                                self.note(
                                    LeniencyKind::UnknownKeyword,
                                    st.span.start,
                                    format!("unknown operator '{}'", String::from_utf8_lossy(&kw)),
                                );
                            }
                            let mut op =
                                Op::new(OpKind::Unknown, operands, span_start..st.span.end);
                            op.raw_keyword = Some(kw);
                            return Some(op);
                        }
                        _ => {}
                    }

                    return Some(Op::new(kind, operands, span_start..st.span.end));
                }

                Token::ArrayOpen => {
                    start.get_or_insert(st.span.start);
                    let arr = self.read_array(0);
                    self.push_operand(&mut operands, Object::Array(arr), st.span.start);
                }
                Token::DictOpen => {
                    start.get_or_insert(st.span.start);
                    let d = self.read_dict(0);
                    self.push_operand(&mut operands, Object::Dictionary(d), st.span.start);
                }
                Token::ArrayClose | Token::DictClose => {
                    // A stray close delimiter. Skip it rather than stop.
                    self.note(
                        LeniencyKind::UnknownKeyword,
                        st.span.start,
                        "stray close delimiter in content stream",
                    );
                }
                Token::BraceOpen | Token::BraceClose => {
                    // Legal inside a Type 4 function, which is not a content
                    // stream. Skip.
                    start.get_or_insert(st.span.start);
                }

                Token::Integer(v) => {
                    start.get_or_insert(st.span.start);
                    self.push_operand(&mut operands, Object::Integer(v), st.span.start);
                }
                Token::Real(v) => {
                    start.get_or_insert(st.span.start);
                    self.push_operand(&mut operands, Object::Real(v), st.span.start);
                }
                Token::String(s) => {
                    start.get_or_insert(st.span.start);
                    self.push_operand(&mut operands, Object::String(s), st.span.start);
                }
                Token::Name(n) => {
                    start.get_or_insert(st.span.start);
                    self.push_operand(&mut operands, Object::Name(n), st.span.start);
                }
            }
        }
    }

    fn push_operand(&mut self, operands: &mut SmallVec<[Object; 4]>, value: Object, at: usize) {
        if operands.len() >= MAX_OPERANDS {
            // Drop the oldest rather than the newest: operators read their
            // arguments from the end, so the tail is what matters.
            operands.remove(0);
            self.note(
                LeniencyKind::UnknownKeyword,
                at,
                format!("more than {MAX_OPERANDS} operands before an operator"),
            );
        }
        operands.push(value);
    }

    fn read_array(&mut self, depth: usize) -> Vec<Object> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            return out;
        }
        loop {
            let st = self.lexer.next_token();
            match st.token {
                Token::ArrayClose | Token::Eof => return out,
                Token::Integer(v) => out.push(Object::Integer(v)),
                Token::Real(v) => out.push(Object::Real(v)),
                Token::String(s) => out.push(Object::String(s)),
                Token::Name(n) => out.push(Object::Name(n)),
                Token::ArrayOpen => out.push(Object::Array(self.read_array(depth + 1))),
                Token::DictOpen => out.push(Object::Dictionary(self.read_dict(depth + 1))),
                Token::Keyword(kw) => match &*kw {
                    b"true" => out.push(Object::Bool(true)),
                    b"false" => out.push(Object::Bool(false)),
                    b"null" => out.push(Object::Null),
                    // An operator inside an array means the array never closed.
                    _ => return out,
                },
                // A `>>` inside an array likewise means it never closed.
                Token::DictClose => return out,
                Token::BraceOpen | Token::BraceClose => {}
            }
        }
    }

    fn read_dict(&mut self, depth: usize) -> Dictionary {
        let mut out = Dictionary::new();
        if depth > MAX_DEPTH {
            return out;
        }
        loop {
            let st = self.lexer.next_token();
            let key = match st.token {
                Token::DictClose | Token::Eof => return out,
                Token::Name(n) => n,
                _ => continue,
            };
            let vt = self.lexer.next_token();
            let value = match vt.token {
                Token::DictClose | Token::Eof => return out,
                Token::Integer(v) => Object::Integer(v),
                Token::Real(v) => Object::Real(v),
                Token::String(s) => Object::String(s),
                Token::Name(n) => Object::Name(n),
                Token::ArrayOpen => Object::Array(self.read_array(depth + 1)),
                Token::DictOpen => Object::Dictionary(self.read_dict(depth + 1)),
                Token::Keyword(kw) => match &*kw {
                    b"true" => Object::Bool(true),
                    b"false" => Object::Bool(false),
                    _ => Object::Null,
                },
                _ => Object::Null,
            };
            out.insert(key, value);
        }
    }

    /// `BI` is consumed; read key/value pairs to `ID`, then the payload to `EI`.
    fn read_inline_image(&mut self, span_start: usize) -> Op {
        let mut dict = Dictionary::new();

        // Parameters, up to `ID`.
        loop {
            let st = self.lexer.next_token();
            match st.token {
                Token::Keyword(kw) if &*kw == b"ID" => break,
                Token::Eof => {
                    self.note(
                        LeniencyKind::UnknownKeyword,
                        span_start,
                        "inline image has no ID keyword",
                    );
                    self.exhausted = true;
                    let mut op =
                        Op::new(OpKind::InlineImage, SmallVec::new(), span_start..self.buf.len());
                    op.inline_image =
                        Some(Box::new(InlineImage { dict, data: self.buf.len()..self.buf.len() }));
                    return op;
                }
                Token::Name(key) => {
                    let vt = self.lexer.next_token();
                    let value = match vt.token {
                        Token::Integer(v) => Object::Integer(v),
                        Token::Real(v) => Object::Real(v),
                        Token::Name(n) => Object::Name(n),
                        Token::String(s) => Object::String(s),
                        Token::ArrayOpen => Object::Array(self.read_array(0)),
                        Token::DictOpen => Object::Dictionary(self.read_dict(0)),
                        Token::Keyword(kw) => match &*kw {
                            b"true" => Object::Bool(true),
                            b"false" => Object::Bool(false),
                            _ => Object::Null,
                        },
                        _ => Object::Null,
                    };
                    dict.insert(key, value);
                }
                _ => {}
            }
        }

        // ISO 32000-1 §8.9.7: exactly one whitespace byte follows `ID`.
        let mut data_start = self.lexer.pos();
        if self.buf.get(data_start).is_some_and(|&b| is_whitespace(b)) {
            data_start += 1;
        }

        let data_end = self.find_inline_image_end(&dict, data_start);
        let data = data_start..data_end;

        // Step over the payload and the `EI`.
        self.lexer.seek(data_end);
        let before = self.lexer.pos();
        match self.lexer.next_token().token {
            Token::Keyword(kw) if &*kw == b"EI" => {}
            _ => {
                self.lexer.seek(before);
                self.note(
                    LeniencyKind::UnknownKeyword,
                    data_end,
                    "inline image payload not followed by EI",
                );
            }
        }

        let mut op = Op::new(OpKind::InlineImage, SmallVec::new(), span_start..self.lexer.pos());
        op.inline_image = Some(Box::new(InlineImage { dict, data }));
        op
    }

    /// Find where an inline image's payload ends.
    ///
    /// This is the one genuinely ambiguous construct in a content stream: the
    /// payload is raw binary, and `EI` can appear inside it by chance. Spec 6.1
    /// calls for length heuristics plus a whitespace-delimited check, and that
    /// is what this does, in order of decreasing reliability:
    ///
    /// 1. `/L` or `/Length`, which ISO 32000-2 added precisely to end the
    ///    guessing. Trusted when it lands on a plausible `EI`.
    /// 2. For an *unfiltered* image, the exact byte count computed from
    ///    `/W`, `/H`, `/BPC` and the colour space. This is arithmetic, not a
    ///    guess, and covers the common uncompressed case exactly.
    /// 3. Otherwise scan for whitespace-delimited `EI` followed by something
    ///    that looks like the stream continuing.
    fn find_inline_image_end(&mut self, dict: &Dictionary, data_start: usize) -> usize {
        let get = |keys: [&str; 2]| -> Option<i64> {
            keys.iter().find_map(|k| dict.get(k).and_then(Object::as_i64))
        };

        // 1. An explicit length.
        if let Some(len) = get(["L", "Length"])
            && len >= 0
        {
            let end = data_start.saturating_add(len as usize);
            if end <= self.buf.len() && self.ei_follows(end) {
                return end;
            }
            self.note(
                LeniencyKind::LengthRecovered,
                data_start,
                "inline image /L does not land on an EI; scanning instead",
            );
        }

        // 2. Exact arithmetic, only when nothing is compressing the payload.
        let filtered = dict.contains_key("F") || dict.contains_key("Filter");
        if !filtered
            && let (Some(w), Some(h)) = (get(["W", "Width"]), get(["H", "Height"]))
            && w > 0
            && h > 0
        {
            let bpc = get(["BPC", "BitsPerComponent"]).unwrap_or(8).clamp(1, 16);
            let components = self.inline_image_components(dict);
            if get(["IM", "ImageMask"]).is_none()
                || dict.get("IM").and_then(Object::as_bool) != Some(true)
            {
                let row_bits = w.saturating_mul(bpc).saturating_mul(components);
                // `div_ceil` is not stable for signed integers.
                let row_bytes = (row_bits + 7) / 8;
                let total = row_bytes.saturating_mul(h);
                if total > 0 {
                    let end = data_start.saturating_add(total as usize);
                    if end <= self.buf.len() && self.ei_follows(end) {
                        return end;
                    }
                }
            }
        }

        // 3. Scan.
        let mut at = data_start;
        while at < self.buf.len() {
            let Some(rel) = find_bytes(&self.buf[at..], b"EI") else { break };
            let candidate = at + rel;
            // `EI` must be a whole token: whitespace before, and delimiter or
            // whitespace after.
            let before_ok = candidate > data_start && is_whitespace(self.buf[candidate - 1]);
            let after = self.buf.get(candidate + 2);
            let after_ok = after.is_none_or(|&b| !rasura_cos::object::is_regular(b));
            if before_ok && after_ok && self.stream_resumes_after(candidate + 2) {
                // The whitespace before EI is a delimiter, not payload.
                return candidate - 1;
            }
            at = candidate + 2;
        }

        self.note(
            LeniencyKind::LengthRecovered,
            data_start,
            "inline image has no locatable EI; taking the rest of the stream",
        );
        self.buf.len()
    }

    /// How many colour components an inline image's colour space implies.
    fn inline_image_components(&self, dict: &Dictionary) -> i64 {
        if dict.get("IM").and_then(Object::as_bool) == Some(true) {
            return 1;
        }
        let cs = dict
            .get("CS")
            .or_else(|| dict.get("ColorSpace"))
            .and_then(Object::as_name)
            .map(|n| n.as_bytes().to_vec());
        match cs.as_deref() {
            Some(b"DeviceRGB" | b"RGB" | b"CalRGB") => 3,
            Some(b"DeviceCMYK" | b"CMYK") => 4,
            Some(b"DeviceGray" | b"G" | b"CalGray") => 1,
            // An indexed or named colour space resolves through /Resources,
            // which the tokenizer does not have. One component is the common
            // case for indexed; if the arithmetic is wrong the `EI` check
            // rejects it and the scan takes over.
            Some(_) => 1,
            None => 1,
        }
    }

    /// Whether a whole-token `EI` sits at or just after `at`.
    fn ei_follows(&self, at: usize) -> bool {
        let mut p = at;
        let mut budget = 4;
        while budget > 0 && self.buf.get(p).is_some_and(|&b| is_whitespace(b)) {
            p += 1;
            budget -= 1;
        }
        self.buf.get(p..p + 2) == Some(b"EI")
    }

    /// After a candidate `EI`, does what follows look like content-stream text
    /// rather than more binary?
    ///
    /// Cheap and effective: binary image data is full of bytes that cannot
    /// appear in a content stream outside a string.
    fn stream_resumes_after(&self, at: usize) -> bool {
        let window = &self.buf[at.min(self.buf.len())..(at + 16).min(self.buf.len())];
        if window.is_empty() {
            return true; // End of stream is a fine place for EI to be.
        }
        window.iter().all(|&b| is_whitespace(b) || b.is_ascii_graphic())
    }

    fn note(&mut self, kind: LeniencyKind, offset: usize, detail: impl Into<String>) {
        self.leniencies.push(Leniency::new(kind, offset, detail));
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Tokenize a whole content stream.
pub fn tokenize(buf: &[u8]) -> (Vec<Op>, Vec<Leniency>) {
    Tokenizer::new(buf).collect_ops()
}

/// Convenience for tests and for the `Name` operand of `Tf`, `Do`, `gs`.
pub fn operand_name(op: &Op, i: usize) -> Option<Name> {
    op.operands.get(i).and_then(Object::as_name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &[u8]) -> Vec<OpKind> {
        tokenize(src).0.into_iter().map(|o| o.kind).collect()
    }

    #[test]
    fn tokenizes_a_simple_text_block() {
        let src = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
        assert_eq!(
            kinds(src),
            vec![
                OpKind::BeginText,
                OpKind::SetFont,
                OpKind::TextMove,
                OpKind::ShowText,
                OpKind::EndText
            ]
        );
    }

    #[test]
    fn spans_cover_operands_and_operator() {
        // The load-bearing property from spec 6.2: replacing the span replaces
        // the whole operator including its arguments.
        let src = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
        let (ops, _) = tokenize(src);
        let tf = &ops[1];
        assert_eq!(&src[tf.span.clone()], b"/F1 24 Tf");
        let td = &ops[2];
        assert_eq!(&src[td.span.clone()], b"72 700 Td");
        let tj = &ops[3];
        assert_eq!(&src[tj.span.clone()], b"(Hello) Tj");
    }

    #[test]
    fn spans_of_operand_free_operators_cover_just_the_operator() {
        let src = b"q Q";
        let (ops, _) = tokenize(src);
        assert_eq!(&src[ops[0].span.clone()], b"q");
        assert_eq!(&src[ops[1].span.clone()], b"Q");
    }

    #[test]
    fn spans_are_contiguous_and_ordered() {
        let src = b"0.5 w 1 0 0 1 10 20 cm 0 0 100 50 re f BT /F1 12 Tf (x) Tj ET";
        let (ops, _) = tokenize(src);
        let mut last_end = 0;
        for op in &ops {
            assert!(op.span.start >= last_end, "{:?} overlaps previous", op.kind);
            assert!(op.span.end > op.span.start);
            assert!(op.span.end <= src.len());
            last_end = op.span.end;
        }
    }

    #[test]
    fn tj_array_is_one_operand() {
        let src = b"[(A) -200 (B) 300 (C)] TJ";
        let (ops, _) = tokenize(src);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::ShowTextAdjusted);
        assert_eq!(ops[0].operands.len(), 1);
        let arr = ops[0].operands[0].as_array().unwrap();
        assert_eq!(arr.len(), 5);
        assert_eq!(arr[1].as_f64(), Some(-200.0));
        assert_eq!(&src[ops[0].span.clone()], src);
    }

    #[test]
    fn bdc_property_dictionary_is_one_operand() {
        let src = b"/Span << /MCID 3 /Lang (en) >> BDC";
        let (ops, _) = tokenize(src);
        assert_eq!(ops[0].kind, OpKind::BeginMarkedProps);
        assert_eq!(ops[0].operands.len(), 2);
        let d = ops[0].operands[1].as_dict().unwrap();
        assert_eq!(d.get("MCID").unwrap().as_i64(), Some(3));
    }

    #[test]
    fn unknown_operator_is_preserved_and_reported() {
        let (ops, len) = tokenize(b"1 2 zz 3 4 l");
        assert_eq!(ops[0].kind, OpKind::Unknown);
        assert_eq!(ops[0].raw_keyword.as_deref(), Some(b"zz".as_slice()));
        assert_eq!(ops[0].operands.len(), 2);
        assert_eq!(ops[1].kind, OpKind::LineTo);
        assert!(len.iter().any(|l| l.kind == LeniencyKind::UnknownKeyword));
    }

    #[test]
    fn unknown_operator_inside_bx_ex_is_silent() {
        // Spec 6.1: skipped silently inside BX/EX, recorded outside.
        let (ops, len) = tokenize(b"BX 1 2 futureop EX");
        assert!(ops.iter().any(|o| o.kind == OpKind::Unknown));
        assert!(
            !len.iter().any(|l| l.kind == LeniencyKind::UnknownKeyword),
            "compatibility sections must not generate noise: {len:?}"
        );
    }

    #[test]
    fn no_reference_lookahead_in_content_streams() {
        // `1 0 R` is a reference in object syntax but must not be here.
        let (ops, _) = tokenize(b"1 0 R");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Unknown);
        assert_eq!(ops[0].operands.len(), 2);
    }

    #[test]
    fn inline_image_with_explicit_length() {
        let payload = b"\x00\x01\x02EI \xff\xfe";
        let mut src = Vec::new();
        src.extend_from_slice(b"BI /W 2 /H 2 /BPC 8 /CS /G /F /AHx /L 8 ID ");
        src.extend_from_slice(payload);
        src.extend_from_slice(b" EI Q");
        let (ops, _) = tokenize(&src);
        assert_eq!(ops[0].kind, OpKind::InlineImage);
        let img = ops[0].inline_image.as_ref().unwrap();
        assert_eq!(&src[img.data.clone()], payload, "the payload contains 'EI' and must survive");
        assert_eq!(ops[1].kind, OpKind::Restore);
    }

    #[test]
    fn unfiltered_inline_image_length_is_computed_exactly() {
        // 4x2 DeviceRGB at 8bpc = 4*3*2 = 24 bytes, one of which is 'E','I'.
        let payload: Vec<u8> = (0..24u8).map(|i| if i == 10 { b'E' } else { i }).collect();
        let mut src = Vec::new();
        src.extend_from_slice(b"BI /W 4 /H 2 /BPC 8 /CS /RGB ID ");
        src.extend_from_slice(&payload);
        src.extend_from_slice(b" EI");
        let (ops, _) = tokenize(&src);
        let img = ops[0].inline_image.as_ref().unwrap();
        assert_eq!(img.data.len(), 24);
        assert_eq!(&src[img.data.clone()], &payload[..]);
    }

    #[test]
    fn inline_image_falls_back_to_scanning() {
        // No length, and filtered so the arithmetic does not apply.
        let mut src = Vec::new();
        src.extend_from_slice(b"BI /W 4 /H 2 /F /Fl ID ");
        src.extend_from_slice(b"compressedbytes");
        src.extend_from_slice(b" EI 1 0 0 1 0 0 cm");
        let (ops, _) = tokenize(&src);
        assert_eq!(ops[0].kind, OpKind::InlineImage);
        let img = ops[0].inline_image.as_ref().unwrap();
        assert_eq!(&src[img.data.clone()], b"compressedbytes");
        assert_eq!(ops[1].kind, OpKind::Concat);
    }

    #[test]
    fn inline_image_dict_keeps_abbreviated_keys() {
        let (ops, _) = tokenize(b"BI /W 1 /H 1 /BPC 8 /CS /G ID \x00 EI");
        let img = ops[0].inline_image.as_ref().unwrap();
        assert_eq!(img.dict.get("W").unwrap().as_i64(), Some(1));
        assert_eq!(img.dict.get("CS").unwrap().as_name().unwrap().as_bytes(), b"G");
    }

    #[test]
    fn quote_operators_parse() {
        let (ops, _) = tokenize(b"(one) ' 1 2 (two) \"");
        assert_eq!(ops[0].kind, OpKind::NextLineShowText);
        assert_eq!(ops[1].kind, OpKind::NextLineSetSpacingShowText);
        assert_eq!(ops[1].operands.len(), 3);
    }

    #[test]
    fn all_path_and_colour_operators_recognised() {
        let src = b"1 1 m 2 2 l 3 3 4 4 5 5 c 6 6 7 7 v 8 8 9 9 y h 0 0 1 1 re \
                    S s f F f* B B* b b* n W W* \
                    /DeviceRGB CS /DeviceGray cs 1 SC 1 SCN 0 sc 0 scn \
                    0.5 G 0.5 g 1 0 0 RG 0 1 0 rg 0 0 0 1 K 1 1 1 0 k";
        let k = kinds(src);
        assert!(!k.contains(&OpKind::Unknown), "{k:?}");
        assert_eq!(k.len(), 31);
    }

    #[test]
    fn empty_and_whitespace_streams_yield_nothing() {
        assert!(tokenize(b"").0.is_empty());
        assert!(tokenize(b"   \n\r\n  ").0.is_empty());
        assert!(tokenize(b"% just a comment\n").0.is_empty());
    }

    #[test]
    fn trailing_operands_without_an_operator_are_reported() {
        let (ops, len) = tokenize(b"BT 1 2 3");
        assert_eq!(ops.len(), 1);
        assert!(len.iter().any(|l| l.detail.contains("trailing operand")));
    }

    #[test]
    fn deeply_nested_arrays_do_not_blow_the_stack() {
        let src = [b"[".repeat(200), b"]".repeat(200), b" TJ".to_vec()].concat();
        let (ops, _) = tokenize(&src);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::ShowTextAdjusted);
    }

    #[test]
    fn unterminated_constructs_terminate() {
        // Regression guard: none of these may hang.
        for src in [
            b"[(unclosed array".to_vec(),
            b"<< /Unclosed dict".to_vec(),
            b"BI /W 1 ID no ei here".to_vec(),
            b"(unclosed string".to_vec(),
            vec![b'['; 5000],
        ] {
            let _ = tokenize(&src);
        }
    }

    #[test]
    fn operand_flood_is_capped() {
        let src = [b"1 ".repeat(500), b"Tj".to_vec()].concat();
        let (ops, len) = tokenize(&src);
        assert_eq!(ops.len(), 1);
        assert!(ops[0].operands.len() <= MAX_OPERANDS);
        assert!(len.iter().any(|l| l.detail.contains("operands")));
    }
}
