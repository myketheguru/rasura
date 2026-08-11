//! Recursive object parser over the lexer. ISO 32000-1 §7.3.
//!
//! Two entry points matter: `parse_object` for a direct object (used for
//! trailers, object-stream contents, and nested values) and
//! `parse_indirect_object` for the `N G obj ... endobj` form, which is the only
//! place a stream can appear.

use crate::error::{CosError, Leniency, LeniencyKind, Result};
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, ObjId, Object, Stream, is_whitespace};

/// Guard against adversarial nesting. Deeply nested arrays are a standard
/// fuzzer finding; the parser must decline rather than blow the stack.
const MAX_DEPTH: usize = 256;

/// Supplies `/Length` when a stream declares it indirectly. The document knows
/// how to fetch such an object from its xref offset; during bootstrap there is
/// nothing to ask, and `NoResolve` scans for `endstream` instead.
pub trait LengthResolver {
    fn resolve_length(&self, id: ObjId) -> Option<i64>;
}

pub struct NoResolve;
impl LengthResolver for NoResolve {
    fn resolve_length(&self, _id: ObjId) -> Option<i64> {
        None
    }
}

/// Adapts a closure. A blanket `impl<F: Fn(..)>` would collide with the
/// concrete `NoResolve` impl under coherence, so the wrapper is explicit.
pub struct FnResolver<F>(pub F);
impl<F: Fn(ObjId) -> Option<i64>> LengthResolver for FnResolver<F> {
    fn resolve_length(&self, id: ObjId) -> Option<i64> {
        (self.0)(id)
    }
}

pub struct Parser<'a> {
    lexer: Lexer<'a>,
}

/// An indirect object as found in the file, with the byte span it occupied.
///
/// The span runs from the first digit of the object number through the `endobj`
/// keyword. The writer replays it verbatim for objects nothing touched, which
/// is the mechanism behind invariant I1.
pub struct IndirectObject {
    pub id: ObjId,
    pub object: Object,
    pub span: std::ops::Range<usize>,
}

impl<'a> Parser<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Parser { lexer: Lexer::new(buf) }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Parser { lexer: Lexer::at(buf, pos) }
    }

    pub fn pos(&self) -> usize {
        self.lexer.pos()
    }

    pub fn seek(&mut self, pos: usize) {
        self.lexer.seek(pos);
    }

    pub fn buf(&self) -> &'a [u8] {
        self.lexer.buf()
    }

    pub fn take_leniencies(&mut self) -> Vec<Leniency> {
        self.lexer.take_leniencies()
    }

    pub fn at_eof(&mut self) -> bool {
        self.lexer.at_eof()
    }

    /// Parse one direct object. Does not accept `stream`.
    pub fn parse_object(&mut self) -> Result<Object> {
        self.parse_object_inner(0)
    }

    fn parse_object_inner(&mut self, depth: usize) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Err(CosError::malformed(self.lexer.pos(), "object nesting too deep"));
        }
        let st = self.lexer.next_token();
        match st.token {
            Token::Eof => Err(CosError::UnexpectedEof { offset: st.span.start }),
            Token::Integer(v) => Ok(self.maybe_reference(v, st.span.start)),
            Token::Real(v) => Ok(Object::Real(v)),
            Token::String(s) => Ok(Object::String(s)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::ArrayOpen => self.parse_array(depth),
            Token::DictOpen => Ok(Object::Dictionary(self.parse_dict_body(depth)?)),
            Token::ArrayClose | Token::DictClose => {
                Err(CosError::malformed(st.span.start, "unexpected close delimiter"))
            }
            Token::BraceOpen | Token::BraceClose => {
                Err(CosError::malformed(st.span.start, "brace outside a Type 4 function"))
            }
            Token::Keyword(kw) => match &*kw {
                b"true" => Ok(Object::Bool(true)),
                b"false" => Ok(Object::Bool(false)),
                b"null" => Ok(Object::Null),
                // A missing value is common in damaged files; treat an
                // unrecognised keyword in value position as null and record it.
                other => {
                    self.lexer.note(
                        LeniencyKind::UnknownKeyword,
                        st.span.start,
                        format!("keyword in value position: {}", String::from_utf8_lossy(other)),
                    );
                    Ok(Object::Null)
                }
            },
        }
    }

    /// After an integer, look for `G R`. Restores the cursor if it is not there.
    fn maybe_reference(&mut self, first: i64, start: usize) -> Object {
        let save = self.lexer.pos();
        if first >= 0 && first <= u32::MAX as i64 {
            let t1 = self.lexer.next_token();
            if let Token::Integer(generation) = t1.token
                && (0..=u16::MAX as i64).contains(&generation)
            {
                let t2 = self.lexer.next_token();
                if let Token::Keyword(kw) = &t2.token
                    && &**kw == b"R"
                {
                    return Object::Reference(ObjId::new(first as u32, generation as u16));
                }
            }
        }
        let _ = start;
        self.lexer.seek(save);
        Object::Integer(first)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Object> {
        let mut items = Vec::new();
        loop {
            let peek = self.lexer.peek_token();
            match peek.token {
                Token::ArrayClose => {
                    self.lexer.next_token();
                    return Ok(Object::Array(items));
                }
                Token::Eof => {
                    return Err(CosError::UnexpectedEof { offset: peek.span.start });
                }
                // A `>>` inside an array means the array was never closed.
                Token::DictClose => {
                    self.lexer.note(
                        LeniencyKind::UnknownKeyword,
                        peek.span.start,
                        "array closed by '>>'",
                    );
                    return Ok(Object::Array(items));
                }
                _ => items.push(self.parse_object_inner(depth + 1)?),
            }
        }
    }

    /// Parse a dictionary body; the opening `<<` is already consumed.
    fn parse_dict_body(&mut self, depth: usize) -> Result<Dictionary> {
        let mut dict = Dictionary::new();
        loop {
            let st = self.lexer.next_token();
            match st.token {
                Token::DictClose => return Ok(dict),
                Token::Eof => return Err(CosError::UnexpectedEof { offset: st.span.start }),
                Token::Name(key) => {
                    let value = self.parse_object_inner(depth + 1)?;
                    if dict.get_name(&key).is_some() {
                        self.lexer.note(
                            LeniencyKind::DuplicateDictKey,
                            st.span.start,
                            format!("duplicate key {key:?}, last wins"),
                        );
                    }
                    dict.insert(key, value);
                }
                // A non-name in key position: skip the token and continue,
                // which recovers more files than bailing does.
                _ => {
                    self.lexer.note(
                        LeniencyKind::UnknownKeyword,
                        st.span.start,
                        "non-name in dictionary key position, skipped",
                    );
                }
            }
        }
    }

    /// Parse `N G obj ... endobj` at the current cursor.
    pub fn parse_indirect_object(
        &mut self,
        resolver: &dyn LengthResolver,
    ) -> Result<IndirectObject> {
        self.lexer.skip_whitespace();
        let start = self.lexer.pos();

        let num = match self.lexer.next_token().token {
            Token::Integer(v) if v >= 0 => v as u32,
            _ => return Err(CosError::malformed(start, "expected object number")),
        };
        let generation = match self.lexer.next_token().token {
            Token::Integer(v) if (0..=u16::MAX as i64).contains(&v) => v as u16,
            _ => return Err(CosError::malformed(start, "expected generation number")),
        };
        match self.lexer.next_token().token {
            Token::Keyword(kw) if &*kw == b"obj" => {}
            _ => return Err(CosError::malformed(start, "expected 'obj' keyword")),
        }

        let id = ObjId::new(num, generation);
        let object = self.parse_object_inner(0)?;

        // A stream follows only when the object was a dictionary.
        let after_value = self.lexer.pos();
        let peek = self.lexer.peek_token();
        let object = if let Token::Keyword(kw) = &peek.token
            && &**kw == b"stream"
            && let Object::Dictionary(dict) = object
        {
            self.lexer.next_token();
            self.parse_stream_body(dict, id, resolver)?
        } else {
            self.lexer.seek(after_value);
            object
        };

        // Consume `endobj` if present. Its absence is survivable.
        let before_end = self.lexer.pos();
        match self.lexer.next_token().token {
            Token::Keyword(kw) if &*kw == b"endobj" => {}
            _ => {
                self.lexer.seek(before_end);
                self.lexer.note(
                    LeniencyKind::UnknownKeyword,
                    before_end,
                    format!("object {id} not terminated by 'endobj'"),
                );
            }
        }

        Ok(IndirectObject { id, object, span: start..self.lexer.pos() })
    }

    /// The `stream` keyword is consumed; the cursor sits just after it.
    fn parse_stream_body(
        &mut self,
        dict: Dictionary,
        id: ObjId,
        resolver: &dyn LengthResolver,
    ) -> Result<Object> {
        // Spec 5.2: `stream` is followed by CRLF or LF, never a bare CR.
        // Files that use a bare CR exist; accept and record.
        let buf = self.lexer.buf();
        let mut p = self.lexer.pos();
        if buf.get(p) == Some(&b'\r') {
            if buf.get(p + 1) == Some(&b'\n') {
                p += 2;
            } else {
                p += 1;
                self.lexer.note(
                    LeniencyKind::BareCrAfterStream,
                    p,
                    "bare CR after 'stream' keyword",
                );
            }
        } else if buf.get(p) == Some(&b'\n') {
            p += 1;
        } else {
            // Some producers write no EOL at all. Skip any other whitespace.
            while buf.get(p).is_some_and(|&b| is_whitespace(b)) {
                p += 1;
            }
        }
        let data_start = p;

        let declared = match dict.get("Length") {
            Some(Object::Integer(v)) if *v >= 0 => Some(*v),
            Some(Object::Reference(rid)) => resolver.resolve_length(*rid),
            _ => None,
        };

        let data_end = match declared {
            Some(len)
                if len >= 0
                    && data_start + len as usize <= buf.len()
                    && endstream_follows(buf, data_start + len as usize) =>
            {
                data_start + len as usize
            }
            other => {
                // Spec 5.2: scan forward for `endstream` and use that length.
                let scanned = scan_for_endstream(buf, data_start).ok_or_else(|| {
                    CosError::malformed(data_start, format!("stream {id} has no 'endstream'"))
                })?;
                self.lexer.note(
                    LeniencyKind::LengthRecovered,
                    data_start,
                    match other {
                        Some(len) => format!(
                            "stream {id}: declared /Length {len} is wrong, recovered {}",
                            scanned - data_start
                        ),
                        None => format!(
                            "stream {id}: /Length unresolvable, recovered {}",
                            scanned - data_start
                        ),
                    },
                );
                scanned
            }
        };

        let raw = buf[data_start..data_end].to_vec();
        self.lexer.seek(data_end);

        // Consume the trailing `endstream`.
        let before = self.lexer.pos();
        match self.lexer.next_token().token {
            Token::Keyword(kw) if &*kw == b"endstream" => {}
            _ => self.lexer.seek(before),
        }

        Ok(Object::Stream(Stream::new(dict, raw)))
    }
}

/// True when `endstream` sits at `at`, allowing the EOL the spec permits before
/// it.
fn endstream_follows(buf: &[u8], at: usize) -> bool {
    let mut p = at;
    // Allow up to a CRLF plus incidental whitespace.
    let mut budget = 4;
    while budget > 0 && buf.get(p).is_some_and(|&b| is_whitespace(b)) {
        p += 1;
        budget -= 1;
    }
    buf[p.min(buf.len())..].starts_with(b"endstream")
}

/// Locate the data end by searching for the next `endstream`, backing off over
/// the EOL that precedes it.
fn scan_for_endstream(buf: &[u8], data_start: usize) -> Option<usize> {
    let pos = find_bytes(&buf[data_start..], b"endstream")? + data_start;
    let mut end = pos;
    // The EOL before `endstream` is not part of the data.
    if end > data_start && buf[end - 1] == b'\n' {
        end -= 1;
        if end > data_start && buf[end - 1] == b'\r' {
            end -= 1;
        }
    } else if end > data_start && buf[end - 1] == b'\r' {
        end -= 1;
    }
    Some(end)
}

/// Straightforward substring search. Deliberately allocation-free and
/// regex-free -- spec 5.3 calls for a regex-free scan and the same routine
/// serves recovery mode.
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        if haystack[i] == first && &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Search backwards. Used to find `startxref` near the end of the file.
pub(crate) fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut i = haystack.len() - needle.len();
    loop {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &[u8]) -> Object {
        Parser::new(src).parse_object().unwrap()
    }

    #[test]
    fn parses_reference_not_two_integers() {
        assert_eq!(parse(b"12 0 R").as_reference(), Some(ObjId::new(12, 0)));
    }

    #[test]
    fn integer_followed_by_integer_is_not_a_reference() {
        let obj = parse(b"[1 2 3]");
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_i64(), Some(1));
    }

    #[test]
    fn reference_inside_array_backtracks_correctly() {
        let obj = parse(b"[1 0 R 2 3 0 R]");
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_reference(), Some(ObjId::new(1, 0)));
        assert_eq!(arr[1].as_i64(), Some(2));
        assert_eq!(arr[2].as_reference(), Some(ObjId::new(3, 0)));
    }

    #[test]
    fn parses_nested_dictionary() {
        let obj = parse(b"<< /Type /Page /Resources << /Font << /F1 5 0 R >> >> >>");
        let d = obj.as_dict().unwrap();
        assert_eq!(d.type_name().unwrap().as_bytes(), b"Page");
        let res = d.get("Resources").unwrap().as_dict().unwrap();
        let font = res.get("Font").unwrap().as_dict().unwrap();
        assert_eq!(font.get("F1").unwrap().as_reference(), Some(ObjId::new(5, 0)));
    }

    #[test]
    fn rejects_runaway_nesting() {
        let src = b"[".repeat(MAX_DEPTH + 10);
        assert!(Parser::new(&src).parse_object().is_err());
    }

    #[test]
    fn parses_stream_with_direct_length() {
        let src = b"1 0 obj\n<< /Length 5 >>\nstream\nHELLO\nendstream\nendobj\n";
        let io = Parser::new(src).parse_indirect_object(&NoResolve).unwrap();
        assert_eq!(io.id, ObjId::new(1, 0));
        assert_eq!(io.object.as_stream().unwrap().raw(), b"HELLO");
        assert_eq!(&src[io.span.clone()], src.trim_ascii_end());
    }

    #[test]
    fn recovers_from_a_wrong_length() {
        let src = b"1 0 obj\n<< /Length 900 >>\nstream\nHELLO\nendstream\nendobj\n";
        let mut p = Parser::new(src);
        let io = p.parse_indirect_object(&NoResolve).unwrap();
        assert_eq!(io.object.as_stream().unwrap().raw(), b"HELLO");
        let l = p.take_leniencies();
        assert!(l.iter().any(|x| x.kind == LeniencyKind::LengthRecovered));
    }

    #[test]
    fn resolves_indirect_length() {
        let src = b"1 0 obj\n<< /Length 7 0 R >>\nstream\nHELLO\nendstream\nendobj\n";
        let resolver = FnResolver(|id: ObjId| if id == ObjId::new(7, 0) { Some(5) } else { None });
        let io = Parser::new(src).parse_indirect_object(&resolver).unwrap();
        assert_eq!(io.object.as_stream().unwrap().raw(), b"HELLO");
    }

    #[test]
    fn bare_cr_after_stream_is_tolerated_and_logged() {
        let src = b"1 0 obj\n<< /Length 5 >>\nstream\rHELLO\nendstream\nendobj\n";
        let mut p = Parser::new(src);
        let io = p.parse_indirect_object(&NoResolve).unwrap();
        assert_eq!(io.object.as_stream().unwrap().raw(), b"HELLO");
        assert!(p.take_leniencies().iter().any(|x| x.kind == LeniencyKind::BareCrAfterStream));
    }

    #[test]
    fn stream_data_may_contain_the_word_endstream() {
        // The declared length must win when it is right, or a stream whose data
        // happens to contain "endstream" would be truncated.
        let data = b"junk endstream junk";
        let src = format!(
            "1 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
            data.len(),
            String::from_utf8_lossy(data)
        );
        let io = Parser::new(src.as_bytes()).parse_indirect_object(&NoResolve).unwrap();
        assert_eq!(io.object.as_stream().unwrap().raw(), data);
    }

    #[test]
    fn duplicate_keys_take_the_last_value() {
        let mut p = Parser::new(b"<< /A 1 /A 2 >>");
        let obj = p.parse_object().unwrap();
        assert_eq!(obj.as_dict().unwrap().get("A").unwrap().as_i64(), Some(2));
        assert!(p.take_leniencies().iter().any(|x| x.kind == LeniencyKind::DuplicateDictKey));
    }
}
