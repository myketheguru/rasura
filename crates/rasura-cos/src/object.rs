//! The PDF object model. ISO 32000-1 §7.3.
//!
//! # Byte preservation (spec 5.1)
//!
//! `PdfString` and `Name` carry their original encoded bytes alongside the
//! decoded value. Re-serialising an unmodified object reproduces its input bytes
//! exactly -- including which escape sequences the producer chose and which
//! characters it `#`-escaped in names. This is load-bearing: it is what lets a
//! dictionary with one changed key round-trip every *other* value untouched, and
//! it is half of what makes invariant I1 achievable. (The other half is the
//! writer emitting unmodified indirect objects from their original byte span;
//! see `writer.rs`.)

use indexmap::IndexMap;
use std::fmt;

/// Indirect object identifier: (object number, generation number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjId {
    pub number: u32,
    pub generation: u16,
}

impl ObjId {
    pub const fn new(number: u32, generation: u16) -> Self {
        ObjId { number, generation }
    }
}

impl fmt::Display for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

// ---------------------------------------------------------------------------
// Name
// ---------------------------------------------------------------------------

/// A PDF name. ISO 32000-1 §7.3.5.
///
/// Equality and hashing use the *decoded* bytes, which is the correct semantic:
/// `/Name` and `/N#61me` denote the same name. Serialisation uses the *raw*
/// bytes, so a document that wrote `/N#61me` gets `/N#61me` back.
#[derive(Clone)]
pub struct Name {
    raw: Box<[u8]>,
    decoded: Box<[u8]>,
}

impl Name {
    /// Build from the raw bytes that appeared after `/` in the file.
    pub fn from_raw(raw: &[u8]) -> Self {
        let decoded = decode_name(raw);
        Name { raw: raw.into(), decoded: decoded.into_boxed_slice() }
    }

    /// Build from a decoded value, choosing a minimal `#`-escaping.
    pub fn new(decoded: impl AsRef<[u8]>) -> Self {
        let decoded = decoded.as_ref();
        let raw = encode_name(decoded);
        Name { raw: raw.into_boxed_slice(), decoded: decoded.into() }
    }

    /// The decoded bytes, `#xx` resolved.
    pub fn as_bytes(&self) -> &[u8] {
        &self.decoded
    }

    /// The bytes exactly as they appeared after `/` in the source file.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// The decoded value as UTF-8, if it happens to be valid UTF-8. Names are
    /// nominally arbitrary bytes; in practice they are ASCII.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.decoded).ok()
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(b'/');
        out.extend_from_slice(&self.raw);
    }
}

fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        // A lone `#` not followed by two hex digits is kept literally: spec 5.2
        // requires handling names containing `#` itself.
        if raw[i] == b'#'
            && i + 2 < raw.len()
            && let (Some(hi), Some(lo)) = (hex_val(raw[i + 1]), hex_val(raw[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn encode_name(decoded: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(decoded.len());
    for &b in decoded {
        if b == b'#' || b <= 0x20 || b >= 0x7f || is_delimiter(b) {
            out.push(b'#');
            out.push(HEX_UPPER[(b >> 4) as usize]);
            out.push(HEX_UPPER[(b & 0x0f) as usize]);
        } else {
            out.push(b);
        }
    }
    out
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.decoded == other.decoded
    }
}
impl Eq for Name {}
impl std::hash::Hash for Name {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.decoded.hash(state);
    }
}
impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Name {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.decoded.cmp(&other.decoded)
    }
}
impl PartialEq<[u8]> for Name {
    fn eq(&self, other: &[u8]) -> bool {
        &*self.decoded == other
    }
}
impl PartialEq<&[u8]> for Name {
    fn eq(&self, other: &&[u8]) -> bool {
        &*self.decoded == *other
    }
}
impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        &*self.decoded == other.as_bytes()
    }
}
impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(s) => write!(f, "/{s}"),
            None => write!(f, "/{:?}", &*self.decoded),
        }
    }
}
impl From<&str> for Name {
    fn from(s: &str) -> Self {
        Name::new(s)
    }
}

// ---------------------------------------------------------------------------
// PdfString
// ---------------------------------------------------------------------------

/// Which delimiter form the string was written with. Preserved so a re-emitted
/// string looks byte-for-byte like the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringForm {
    /// `( ... )`
    Literal,
    /// `< ... >`
    Hex,
}

/// A PDF string. ISO 32000-1 §7.3.4.
#[derive(Clone)]
pub struct PdfString {
    /// Bytes between the delimiters, exactly as they appeared.
    raw: Box<[u8]>,
    decoded: Box<[u8]>,
    form: StringForm,
}

impl PdfString {
    /// Build from the raw inner bytes of a literal `( ... )` string.
    pub fn from_raw_literal(raw: &[u8]) -> Self {
        let decoded = decode_literal(raw);
        PdfString {
            raw: raw.into(),
            decoded: decoded.into_boxed_slice(),
            form: StringForm::Literal,
        }
    }

    /// Build from the raw inner bytes of a hex `< ... >` string. Returns the
    /// string plus whether an odd trailing digit had to be padded.
    pub fn from_raw_hex(raw: &[u8]) -> (Self, bool) {
        let (decoded, padded) = decode_hex(raw);
        (
            PdfString {
                raw: raw.into(),
                decoded: decoded.into_boxed_slice(),
                form: StringForm::Hex,
            },
            padded,
        )
    }

    /// Build a new literal string, escaping minimally.
    pub fn new_literal(decoded: impl AsRef<[u8]>) -> Self {
        let decoded = decoded.as_ref();
        let raw = encode_literal(decoded);
        PdfString {
            raw: raw.into_boxed_slice(),
            decoded: decoded.into(),
            form: StringForm::Literal,
        }
    }

    /// Build a new hex string.
    pub fn new_hex(decoded: impl AsRef<[u8]>) -> Self {
        let decoded = decoded.as_ref();
        let mut raw = Vec::with_capacity(decoded.len() * 2);
        for &b in decoded {
            raw.push(HEX_UPPER[(b >> 4) as usize]);
            raw.push(HEX_UPPER[(b & 0x0f) as usize]);
        }
        PdfString { raw: raw.into_boxed_slice(), decoded: decoded.into(), form: StringForm::Hex }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.decoded
    }

    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub fn form(&self) -> StringForm {
        self.form
    }

    /// Interpret as a PDF *text string* (ISO 32000-1 §7.9.2.2): UTF-16BE when
    /// prefixed with a BOM, UTF-8 when prefixed with the ISO 32000-2 UTF-8 BOM,
    /// otherwise PDFDocEncoding.
    pub fn as_text(&self) -> String {
        let b = &*self.decoded;
        if b.len() >= 2 && b[0] == 0xfe && b[1] == 0xff {
            let units: Vec<u16> =
                b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        } else if b.len() >= 3 && b[..3] == [0xef, 0xbb, 0xbf] {
            String::from_utf8_lossy(&b[3..]).into_owned()
        } else {
            b.iter().map(|&c| pdf_doc_encoding_char(c)).collect()
        }
    }

    /// Replace the contents, keeping the delimiter form. Used by the crypt layer
    /// when decrypting in place; the raw bytes are regenerated because the
    /// decrypted value has no meaningful original encoding.
    pub(crate) fn replace_decoded(&mut self, decoded: Vec<u8>) {
        let raw = match self.form {
            StringForm::Literal => encode_literal(&decoded),
            StringForm::Hex => {
                let mut r = Vec::with_capacity(decoded.len() * 2);
                for &b in &decoded {
                    r.push(HEX_UPPER[(b >> 4) as usize]);
                    r.push(HEX_UPPER[(b & 0x0f) as usize]);
                }
                r
            }
        };
        self.raw = raw.into_boxed_slice();
        self.decoded = decoded.into_boxed_slice();
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self.form {
            StringForm::Literal => {
                out.push(b'(');
                out.extend_from_slice(&self.raw);
                out.push(b')');
            }
            StringForm::Hex => {
                out.push(b'<');
                out.extend_from_slice(&self.raw);
                out.push(b'>');
            }
        }
    }
}

impl fmt::Debug for PdfString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = Vec::new();
        self.write_to(&mut buf);
        write!(f, "{}", String::from_utf8_lossy(&buf))
    }
}

fn decode_literal(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b != b'\\' {
            // An unescaped CRLF or CR inside a literal string means LF.
            if b == b'\r' {
                out.push(b'\n');
                i += if raw.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                continue;
            }
            out.push(b);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = raw.get(i) else { break };
        match e {
            b'n' => {
                out.push(b'\n');
                i += 1;
            }
            b'r' => {
                out.push(b'\r');
                i += 1;
            }
            b't' => {
                out.push(b'\t');
                i += 1;
            }
            b'b' => {
                out.push(0x08);
                i += 1;
            }
            b'f' => {
                out.push(0x0c);
                i += 1;
            }
            b'(' | b')' | b'\\' => {
                out.push(e);
                i += 1;
            }
            // Line continuation: backslash followed by EOL emits nothing.
            b'\n' => i += 1,
            b'\r' => {
                i += 1;
                if raw.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'0'..=b'7' => {
                let mut v: u32 = 0;
                let mut n = 0;
                while n < 3 {
                    match raw.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            v = v * 8 + (d - b'0') as u32;
                            i += 1;
                            n += 1;
                        }
                        _ => break,
                    }
                }
                // ISO 32000-1: high-order overflow is ignored.
                out.push((v & 0xff) as u8);
            }
            // Any other escaped character: the backslash is dropped.
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

fn encode_literal(decoded: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(decoded.len() + 8);
    for &b in decoded {
        match b {
            b'(' => out.extend_from_slice(b"\\("),
            b')' => out.extend_from_slice(b"\\)"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            0x00..=0x1f | 0x7f..=0xff => {
                out.push(b'\\');
                out.push(b'0' + (b >> 6));
                out.push(b'0' + ((b >> 3) & 7));
                out.push(b'0' + (b & 7));
            }
            _ => out.push(b),
        }
    }
    out
}

/// Returns the decoded bytes and whether an odd final digit was padded.
fn decode_hex(raw: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(raw.len() / 2 + 1);
    let mut hi: Option<u8> = None;
    let mut count = 0usize;
    for &b in raw {
        let Some(v) = hex_val(b) else { continue };
        count += 1;
        match hi {
            None => hi = Some(v),
            Some(h) => {
                out.push(h << 4 | v);
                hi = None;
            }
        }
    }
    let padded = hi.is_some();
    if let Some(h) = hi {
        // Spec 5.2: an odd final digit is padded with `0`.
        out.push(h << 4);
    }
    let _ = count;
    (out, padded)
}

// ---------------------------------------------------------------------------
// Dictionary
// ---------------------------------------------------------------------------

/// A PDF dictionary preserving key insertion order.
///
/// Spec 5.1 forbids a plain `HashMap` here: key order is observable in the
/// output bytes, and reordering an untouched dictionary would violate I1.
/// `IndexMap` gives insertion order plus a hash index for O(1) lookup.
#[derive(Clone, Default)]
pub struct Dictionary(IndexMap<Name, Object>);

impl Dictionary {
    pub fn new() -> Self {
        Dictionary(IndexMap::new())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Object> {
        self.0.get(&Name::new(key))
    }

    pub fn get_name(&self, key: &Name) -> Option<&Object> {
        self.0.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Object> {
        self.0.get_mut(&Name::new(key))
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(&Name::new(key))
    }

    /// Insert, preserving the position of an existing key if there is one.
    pub fn insert(&mut self, key: impl Into<Name>, value: Object) -> Option<Object> {
        self.0.insert(key.into(), value)
    }

    pub fn remove(&mut self, key: &str) -> Option<Object> {
        self.0.shift_remove(&Name::new(key))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Name, &mut Object)> {
        self.0.iter_mut()
    }

    pub fn keys(&self) -> impl Iterator<Item = &Name> {
        self.0.keys()
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.0.values_mut()
    }

    /// `/Type` as a name, if present.
    pub fn type_name(&self) -> Option<&Name> {
        self.get("Type").and_then(Object::as_name)
    }
}

impl fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl FromIterator<(Name, Object)> for Dictionary {
    fn from_iter<T: IntoIterator<Item = (Name, Object)>>(iter: T) -> Self {
        Dictionary(iter.into_iter().collect())
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// A stream object: a dictionary plus raw, still-filtered bytes.
///
/// Spec 5.1: the raw bytes are held as they appeared. Decoding is lazy and
/// cached at the `Document` level, so a stream that is never decoded is never
/// re-encoded, and its bytes survive a save untouched.
#[derive(Clone)]
pub struct Stream {
    pub dict: Dictionary,
    raw: Vec<u8>,
    /// Set when the caller replaced the *decoded* content. The writer must then
    /// re-encode with the original filter chain rather than emit `raw`.
    decoded_override: Option<Vec<u8>>,
}

impl Stream {
    pub fn new(dict: Dictionary, raw: Vec<u8>) -> Self {
        Stream { dict, raw, decoded_override: None }
    }

    /// The bytes as stored in the file, still filtered and (if the document is
    /// encrypted) still encrypted unless the crypt layer has processed them.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Replace the stored, still-filtered bytes wholesale.
    ///
    /// Callers that want to change what a stream *says* should use
    /// `set_decoded` instead: it keeps the filter chain and lets the writer
    /// re-encode. This is for the rarer case of substituting an already-encoded
    /// payload, such as swapping a JPEG for another JPEG.
    pub fn set_raw(&mut self, raw: Vec<u8>) {
        self.raw = raw;
        self.decoded_override = None;
    }

    /// Content the caller has replaced, awaiting re-encode on save.
    pub fn pending_decoded(&self) -> Option<&[u8]> {
        self.decoded_override.as_deref()
    }

    /// Replace the stream's logical content. The filter chain is preserved and
    /// re-applied at save time.
    pub fn set_decoded(&mut self, decoded: Vec<u8>) {
        self.decoded_override = Some(decoded);
    }

    /// The declared `/Length`, which may be an indirect reference and may be
    /// wrong; the parser records `LengthRecovered` when it was.
    pub fn declared_length(&self) -> Option<&Object> {
        self.dict.get("Length")
    }
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("dict", &self.dict)
            .field("raw_len", &self.raw.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Object
// ---------------------------------------------------------------------------

/// Spec 5.1.
#[derive(Clone, Debug)]
pub enum Object {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(PdfString),
    Name(Name),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjId),
}

impl Object {
    pub fn name(s: &str) -> Object {
        Object::Name(Name::new(s))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        self.as_i64().and_then(|i| usize::try_from(i).ok())
    }

    /// Numeric value of either an integer or a real.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&Name> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&PdfString> {
        match self {
            Object::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// A dictionary, whether standalone or the dictionary of a stream.
    pub fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_dict_mut(&mut self) -> Option<&mut Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&mut s.dict),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_stream_mut(&mut self) -> Option<&mut Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_reference(&self) -> Option<ObjId> {
        match self {
            Object::Reference(id) => Some(*id),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared byte helpers
// ---------------------------------------------------------------------------

pub(crate) const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

pub(crate) fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// ISO 32000-1 Table 1: the six white-space characters.
///
/// Public because the character classes are part of the file format, not of
/// this crate: `rasura-content` tokenises content streams by the same rules
/// and should not maintain a second copy of them.
pub fn is_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

/// ISO 32000-1 Table 2: the delimiter characters.
pub fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// Anything that is neither whitespace nor a delimiter, i.e. what a token can
/// be made of.
pub fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

/// Format a real the way PDF requires: decimal only, never exponent notation,
/// shortest form that round-trips.
///
/// Producers care about this and so do diffs (spec 9.4). `1e-4` is not a legal
/// PDF number even though Rust will happily print it.
pub fn format_real(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    for precision in 1..=10usize {
        let s = format!("{v:.precision$}");
        if s.parse::<f64>() == Ok(v) {
            return trim_real(s);
        }
    }
    trim_real(format!("{v:.10}"))
}

fn trim_real(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s.is_empty() || s == "-" { "0".to_string() } else { s }
}

/// PDFDocEncoding -> Unicode for the ranges where it differs from Latin-1.
/// ISO 32000-1 Annex D.2.
///
/// Two disjoint ranges, and they need two tables. An earlier version indexed
/// both into `HIGH` — `c - 0x18` for the low range and `c - 0x80 + 8` for the
/// high one — which was wrong twice over: the low range read the high range's
/// characters, and the `+ 8` ran off the end of a 32-entry table from `0x98`
/// upward. Reading a `/Title` containing byte `0x98` panicked.
fn pdf_doc_encoding_char(c: u8) -> char {
    /// 0x18..=0x1F: the accents PDFDocEncoding puts in the control range.
    const LOW: [char; 8] = [
        '\u{02d8}', // breve
        '\u{02c7}', // caron
        '\u{02c6}', // circumflex
        '\u{02d9}', // dotaccent
        '\u{02dd}', // hungarumlaut
        '\u{02db}', // ogonek
        '\u{02da}', // ring
        '\u{02dc}', // tilde
    ];
    /// 0x80..=0x9F, where Latin-1 has C1 controls. The last is unused in
    /// PDFDocEncoding and maps to the replacement character.
    const HIGH: [char; 32] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203a}', '\u{2212}', '\u{2030}', '\u{201e}', '\u{201c}',
        '\u{201d}', '\u{2018}', '\u{2019}', '\u{201a}', '\u{2122}', '\u{fb01}', '\u{fb02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017d}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017e}', '\u{fffd}',
    ];
    match c {
        0x18..=0x1f => LOW[(c - 0x18) as usize],
        0x80..=0x9f => HIGH[(c - 0x80) as usize],
        0xa0 => '\u{20ac}',
        _ => c as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_doc_encoding_covers_every_byte_without_panicking() {
        // Spec 14.4: the parser must never panic. This one did, on any string
        // containing a byte from 0x98 upward -- the high table was indexed
        // eight past where it should have been, and ran off the end.
        for c in 0..=255u8 {
            let _ = pdf_doc_encoding_char(c);
        }
    }

    #[test]
    fn pdf_doc_encoding_maps_annex_d2_correctly() {
        // Spot checks at the boundaries the bug straddled, from ISO 32000-1
        // Annex D.2. The low range is accents; the high range is punctuation
        // and ligatures; 0xA0 is the Euro rather than Latin-1's nbsp.
        assert_eq!(pdf_doc_encoding_char(0x18), '\u{02d8}', "breve, not bullet");
        assert_eq!(pdf_doc_encoding_char(0x1f), '\u{02dc}', "tilde");
        assert_eq!(pdf_doc_encoding_char(0x80), '\u{2022}', "bullet");
        assert_eq!(pdf_doc_encoding_char(0x97), '\u{0160}', "Scaron");
        assert_eq!(pdf_doc_encoding_char(0x98), '\u{0178}', "Ydieresis -- the first that panicked");
        assert_eq!(pdf_doc_encoding_char(0x9e), '\u{017e}', "zcaron");
        assert_eq!(pdf_doc_encoding_char(0x9f), '\u{fffd}', "unused in PDFDocEncoding");
        assert_eq!(pdf_doc_encoding_char(0xa0), '\u{20ac}', "Euro, not nbsp");
        assert_eq!(pdf_doc_encoding_char(b'A'), 'A');
        assert_eq!(pdf_doc_encoding_char(0xe9), '\u{e9}', "Latin-1 elsewhere");
    }

    #[test]
    fn a_string_with_a_high_byte_reads_as_text() {
        // The path the panic actually came through: reading a /Title.
        let s = PdfString::new_literal(vec![b'a', 0x98, 0x9f, b'z']);
        assert_eq!(s.as_text(), "a\u{0178}\u{fffd}z");
    }

    #[test]
    fn name_round_trips_hash_escapes() {
        let n = Name::from_raw(b"A#20B");
        assert_eq!(n.as_bytes(), b"A B");
        let mut out = Vec::new();
        n.write_to(&mut out);
        assert_eq!(out, b"/A#20B");
    }

    #[test]
    fn name_with_bare_hash_is_kept_literal() {
        let n = Name::from_raw(b"a#zz");
        assert_eq!(n.as_bytes(), b"a#zz");
    }

    #[test]
    fn name_equality_is_on_decoded_value() {
        assert_eq!(Name::from_raw(b"N#61me"), Name::from_raw(b"Name"));
    }

    #[test]
    fn literal_string_escapes_decode() {
        let s = PdfString::from_raw_literal(b"a\\(b\\)c\\\\d\\101\\n");
        assert_eq!(s.as_bytes(), b"a(b)c\\dA\n");
    }

    #[test]
    fn literal_string_line_continuation_emits_nothing() {
        let s = PdfString::from_raw_literal(b"one\\\ntwo");
        assert_eq!(s.as_bytes(), b"onetwo");
    }

    #[test]
    fn literal_string_bare_cr_becomes_lf() {
        let s = PdfString::from_raw_literal(b"a\rb\r\nc");
        assert_eq!(s.as_bytes(), b"a\nb\nc");
    }

    #[test]
    fn hex_string_pads_odd_digit() {
        let (s, padded) = PdfString::from_raw_hex(b"901FA");
        assert!(padded);
        assert_eq!(s.as_bytes(), &[0x90, 0x1f, 0xa0]);
    }

    #[test]
    fn hex_string_ignores_whitespace() {
        let (s, padded) = PdfString::from_raw_hex(b"90 1F\nA0");
        assert!(!padded);
        assert_eq!(s.as_bytes(), &[0x90, 0x1f, 0xa0]);
    }

    #[test]
    fn string_serialisation_reproduces_original_bytes() {
        // The producer's escape choices survive, which is the point.
        let raw: &[u8] = b"weird \\x escape and \\053 octal";
        let s = PdfString::from_raw_literal(raw);
        let mut out = Vec::new();
        s.write_to(&mut out);
        assert_eq!(&out[1..out.len() - 1], raw);
    }

    #[test]
    fn text_string_utf16be() {
        let s = PdfString::new_hex([0xfe, 0xff, 0x00, 0x48, 0x00, 0x69]);
        assert_eq!(s.as_text(), "Hi");
    }

    #[test]
    fn dictionary_preserves_insertion_order() {
        let mut d = Dictionary::new();
        d.insert(Name::new("Zebra"), Object::Integer(1));
        d.insert(Name::new("Apple"), Object::Integer(2));
        let keys: Vec<_> = d.keys().map(|k| k.as_str().unwrap().to_string()).collect();
        assert_eq!(keys, vec!["Zebra", "Apple"]);
    }

    #[test]
    fn reinsert_keeps_original_position() {
        let mut d = Dictionary::new();
        d.insert(Name::new("A"), Object::Integer(1));
        d.insert(Name::new("B"), Object::Integer(2));
        d.insert(Name::new("A"), Object::Integer(3));
        let keys: Vec<_> = d.keys().map(|k| k.as_str().unwrap().to_string()).collect();
        assert_eq!(keys, vec!["A", "B"]);
    }

    #[test]
    fn reals_never_use_exponent_notation() {
        assert_eq!(format_real(0.0001), "0.0001");
        assert_eq!(format_real(1e-7), "0.0000001");
        assert_eq!(format_real(72.0), "72");
        assert_eq!(format_real(-0.5), "-0.5");
        assert_eq!(format_real(34.5), "34.5");
        assert!(!format_real(6.02e23).contains('e'));
    }
}
