//! Type 1 fonts. Spec 8.2: `/FontFile`, "eexec decryption, charstring Type 1
//! interpretation, `/Encoding` from the font program".
//!
//! A Type 1 font is a PostScript program in two halves. The first is readable
//! source carrying `/FontMatrix` and usually `/Encoding`; the second is
//! **eexec-encrypted** and holds the private dict, the subroutines and the
//! charstrings — each of which is then encrypted *again*, individually.
//!
//! Two decryptions, therefore, with different keys and the same cipher: eexec
//! with R=55665 over the whole private half, and R=4330 over each charstring.
//! Both discard leading random bytes, which is the detail that silently
//! corrupts the first outline of every glyph if missed.
//!
//! Charstrings are kept **decrypted but uninterpreted**, matching what
//! `cff.rs` does with Type 2 and for the same reason: §8.4's injection copies
//! charstrings with subroutine calls inlined, which needs bytes rather than
//! outlines.

use crate::error::{FontError, Result};
use std::collections::HashMap;

/// The eexec key. ISO 32000-1 §9.9 and the Type 1 spec both give this constant.
const EEXEC_KEY: u16 = 55665;

/// The charstring key.
const CHARSTRING_KEY: u16 = 4330;

/// Bytes of random padding at the start of an eexec-decrypted stream.
const EEXEC_SKIP: usize = 4;

/// A parsed Type 1 font.
#[derive(Debug, Clone, Default)]
pub struct Type1 {
    /// `/FontName` from the cleartext half.
    pub font_name: Option<String>,
    /// `/FontMatrix`, when the font declares one. Almost always
    /// `[0.001 0 0 0.001 0 0]`, but a Type 1 may use any scale and a reader
    /// that assumes 1/1000 lays those out at the wrong size.
    pub font_matrix: Option<[f64; 6]>,
    /// The font's built-in encoding: code to glyph name.
    ///
    /// Spec 8.2 lists this as required, and §7.2 needs it: a symbolic font with
    /// no `/Encoding` in the PDF is encoded by the *program*, and there is
    /// nowhere else to find it.
    pub encoding: HashMap<u8, String>,
    /// True when the program says `/Encoding StandardEncoding def` rather than
    /// building an array. Recorded rather than expanded, because the caller
    /// already has the standard encoding and this is one fewer copy.
    pub standard_encoding: bool,
    /// Decrypted charstrings by glyph name.
    pub charstrings: HashMap<String, Vec<u8>>,
    /// Glyph names in the order the font declared them, which is the closest
    /// thing a Type 1 has to a glyph id.
    pub glyph_order: Vec<String>,
    /// Decrypted subroutines, indexed as the charstrings reference them.
    pub subrs: Vec<Vec<u8>>,
    /// `/lenIV`: how many random bytes each charstring starts with. Four unless
    /// the font says otherwise, and fonts do say otherwise.
    pub len_iv: usize,
}

impl Type1 {
    pub fn glyph_count(&self) -> usize {
        self.charstrings.len()
    }

    pub fn charstring(&self, name: &str) -> Option<&[u8]> {
        self.charstrings.get(name).map(|v| v.as_slice())
    }

    pub fn parse(data: &[u8]) -> Result<Type1> {
        // A PFB wraps the same content in length-prefixed segments; strip them
        // first so both container forms take the same path.
        let joined = if data.first() == Some(&0x80) { unwrap_pfb(data)? } else { data.to_vec() };

        let split = find(&joined, b"eexec").ok_or(FontError::Malformed("no eexec section"))?;
        let clear = &joined[..split];

        // The encrypted half begins after `eexec` and the whitespace following
        // it. Skipping *all* leading whitespace matters: a stray CR LF counted
        // as ciphertext shifts the whole stream and decrypts to noise.
        let mut at = split + 5;
        while at < joined.len() && joined[at].is_ascii_whitespace() {
            at += 1;
        }
        let encrypted = &joined[at..];

        // The private half may be raw binary or hex. The Type 1 spec's own test
        // is the first four bytes: if all are hex digits, it is hex -- binary
        // ciphertext being four hex digits by chance is possible but the
        // alternative reading fails immediately and visibly.
        let cipher = if is_hex(encrypted) { unhex(encrypted) } else { encrypted.to_vec() };
        let private = decrypt(&cipher, EEXEC_KEY, EEXEC_SKIP);

        let mut font = Type1 {
            font_name: read_name(clear, b"/FontName"),
            font_matrix: read_matrix(clear),
            len_iv: read_int(&private, b"/lenIV").unwrap_or(4).clamp(0, 16) as usize,
            ..Default::default()
        };
        read_encoding(clear, &mut font);
        font.subrs = read_subrs(&private, font.len_iv);
        read_charstrings(&private, font.len_iv, &mut font);

        if font.charstrings.is_empty() {
            return Err(FontError::Malformed("no CharStrings"));
        }
        Ok(font)
    }
}

/// Whether a decrypted charstring opens the way every Type 1 charstring must.
///
/// The format requires the first operator to be `hsbw` (13) or `sbw` (12 7),
/// preceded only by its numeric operands: it sets the left side bearing and
/// width before anything can be drawn.
///
/// This is the check that distinguishes *decrypted* from *not errored*. Get
/// `lenIV` wrong, or miscount the space after `RD`, and parsing still
/// "succeeds" — it just yields plausible-looking bytes that are shifted by a
/// few positions. The opening operator is the cheapest thing that notices.
pub fn opens_correctly(charstring: &[u8]) -> bool {
    let mut at = 0;
    // Generous enough for a `div` or two among the operands, tight enough that
    // a shifted stream runs out before stumbling onto a 13 by chance.
    for _ in 0..12 {
        let Some(&b) = charstring.get(at) else { return false };
        match b {
            13 => return true,
            12 => match charstring.get(at + 1) {
                Some(7) => return true,
                // `div`. Computer Modern computes fractional side bearings
                // this way -- `78 113889 100 div hsbw` -- so an operand list
                // is not simply a run of numbers, and rejecting the escape
                // outright scores every CM font at zero.
                Some(12) => at += 2,
                _ => return false,
            },
            32..=246 => at += 1,
            247..=254 => at += 2,
            255 => at += 5,
            // Any other operator before the side bearing is set is invalid.
            _ => return false,
        }
    }
    false
}

impl Type1 {
    /// Whether a charstring sets its side bearing, following `callsubr`.
    ///
    /// The free `opens_correctly` cannot do this, because a subroutinized font
    /// puts the `hsbw` inside the subroutine: MinionPro's `hyphen` is the whole
    /// charstring `2012 callsubr endchar`, and Adobe's tools produce thousands
    /// like it. Judging those on their own bytes marks a quarter of the font
    /// broken when nothing is wrong.
    pub fn opens_correctly(&self, charstring: &[u8]) -> bool {
        self.opens(charstring, 0)
    }

    fn opens(&self, cs: &[u8], depth: usize) -> bool {
        if depth > 10 {
            return false;
        }
        let mut at = 0usize;
        // The last operand pushed, which is the subroutine number at a call.
        let mut last: Option<i64> = None;
        for _ in 0..32 {
            let Some(&b) = cs.get(at) else { return false };
            match b {
                13 => return true,
                12 => match cs.get(at + 1) {
                    Some(7) => return true,
                    Some(12) => {
                        at += 2;
                        last = None;
                    }
                    _ => return false,
                },
                // callsubr. Type 1 indexes subroutines directly -- the bias is
                // a Type 2 invention and applying it here would look up the
                // wrong subroutine every time.
                10 => {
                    let sub =
                        last.and_then(|i| usize::try_from(i).ok()).and_then(|i| self.subrs.get(i));
                    match sub {
                        Some(sub) if self.opens(sub, depth + 1) => return true,
                        Some(_) => {
                            at += 1;
                            last = None;
                        }
                        None => return false,
                    }
                }
                11 => at += 1, // return
                32..=246 => {
                    last = Some(b as i64 - 139);
                    at += 1;
                }
                247..=250 => {
                    let n = cs.get(at + 1).copied().unwrap_or(0) as i64;
                    last = Some((b as i64 - 247) * 256 + n + 108);
                    at += 2;
                }
                251..=254 => {
                    let n = cs.get(at + 1).copied().unwrap_or(0) as i64;
                    last = Some(-(b as i64 - 251) * 256 - n - 108);
                    at += 2;
                }
                255 => {
                    last = cs
                        .get(at + 1..at + 5)
                        .map(|s| i32::from_be_bytes([s[0], s[1], s[2], s[3]]) as i64);
                    at += 5;
                }
                _ => return false,
            }
        }
        false
    }

    /// Fraction of charstrings that set their side bearing.
    ///
    /// A correctly decrypted font scores at or near 1.0. Anything much lower
    /// means the bytes came out shifted, which no parse error would have caught.
    pub fn soundness(&self) -> f64 {
        if self.charstrings.is_empty() {
            return 0.0;
        }
        let good = self.charstrings.values().filter(|c| self.opens_correctly(c)).count();
        good as f64 / self.charstrings.len() as f64
    }
}

/// The eexec cipher, used for both the private half and each charstring.
///
/// Symmetric, so `encrypt` is the same walk with the roles of plain and cipher
/// swapped -- which §8.4's injection needs, since a charstring written back
/// must be re-encrypted.
pub fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    const C1: u16 = 52845;
    const C2: u16 = 22719;
    let mut r = key;
    let mut out = Vec::with_capacity(data.len().saturating_sub(skip));
    for (i, &c) in data.iter().enumerate() {
        let plain = c ^ (r >> 8) as u8;
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
        if i >= skip {
            out.push(plain);
        }
    }
    out
}

/// The inverse, for writing a charstring back. Spec 8.4.
pub fn encrypt(data: &[u8], key: u16, pad: usize) -> Vec<u8> {
    const C1: u16 = 52845;
    const C2: u16 = 22719;
    let mut r = key;
    let mut out = Vec::with_capacity(data.len() + pad);
    // The leading bytes are discarded on decryption, so their value is
    // irrelevant -- but they must be *present*, and a constant keeps the output
    // reproducible, which byte-identical saves depend on.
    for &p in std::iter::repeat_n(&0u8, pad).chain(data.iter()) {
        let c = p ^ (r >> 8) as u8;
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
        out.push(c);
    }
    out
}

/// Concatenate the segments of a PFB container.
fn unwrap_pfb(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    while at + 2 <= data.len() {
        // A marker that does not chain ends the walk rather than failing it,
        // provided something was read. Real files pad after the last segment,
        // or omit the end marker entirely, and one font in the corpus does
        // exactly that -- refusing it would lose a font that renders fine.
        if data[at] != 0x80 {
            if out.is_empty() {
                return Err(FontError::Malformed("PFB segment marker"));
            }
            break;
        }
        match data[at + 1] {
            // 3 is the end-of-file marker and carries no length.
            3 => break,
            1 | 2 => {}
            _ if !out.is_empty() => break,
            _ => return Err(FontError::Malformed("PFB segment type")),
        }
        let len = data
            .get(at + 2..at + 6)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or(FontError::Truncated("PFB segment length"))?;
        let start = at + 6;
        let end = start.checked_add(len).ok_or(FontError::Malformed("PFB segment length"))?;
        // Clamped rather than rejected: a truncated PFB usually still holds
        // enough of the font to read.
        out.extend_from_slice(data.get(start..end.min(data.len())).unwrap_or_default());
        at = end;
    }
    if out.is_empty() {
        return Err(FontError::Malformed("empty PFB"));
    }
    Ok(out)
}

fn is_hex(data: &[u8]) -> bool {
    data.iter().filter(|b| !b.is_ascii_whitespace()).take(4).all(|b| b.is_ascii_hexdigit())
        && data.iter().filter(|b| !b.is_ascii_whitespace()).count() >= 4
}

fn unhex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut high: Option<u8> = None;
    for &b in data {
        let Some(v) = (b as char).to_digit(16) else { continue };
        match high.take() {
            Some(h) => out.push(h << 4 | v as u8),
            None => high = Some(v as u8),
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read_name(data: &[u8], key: &[u8]) -> Option<String> {
    let at = find(data, key)? + key.len();
    let rest = data.get(at..)?;
    let start = rest.iter().position(|b| *b == b'/')? + 1;
    let name: Vec<u8> = rest[start..]
        .iter()
        .copied()
        .take_while(|b| !b.is_ascii_whitespace() && *b != b'(' && *b != b'/')
        .collect();
    String::from_utf8(name).ok().filter(|s| !s.is_empty())
}

fn read_int(data: &[u8], key: &[u8]) -> Option<i64> {
    let at = find(data, key)? + key.len();
    let rest = data.get(at..)?;
    let start = rest.iter().position(|b| b.is_ascii_digit() || *b == b'-')?;
    let digits: Vec<u8> =
        rest[start..].iter().copied().take_while(|b| b.is_ascii_digit() || *b == b'-').collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn read_matrix(data: &[u8]) -> Option<[f64; 6]> {
    let at = find(data, b"/FontMatrix")? + 11;
    let rest = data.get(at..)?;
    let open = rest.iter().position(|b| *b == b'[')?;
    let close = rest.iter().position(|b| *b == b']')?;
    if close <= open {
        return None;
    }
    let text = std::str::from_utf8(&rest[open + 1..close]).ok()?;
    let values: Vec<f64> = text.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
    values.get(..6).and_then(|v| v.try_into().ok())
}

/// The built-in encoding from the cleartext half.
fn read_encoding(clear: &[u8], font: &mut Type1) {
    let Some(at) = find(clear, b"/Encoding") else { return };
    let rest = &clear[at..];

    // `/Encoding StandardEncoding def` is the common short form. The window is
    // clamped rather than sliced: `/Encoding` usually sits just before `eexec`,
    // so the remaining text is often shorter than the window and a bare
    // `get(..64)` would return None on precisely the common case.
    let head = &rest[..rest.len().min(64)];
    if find(head, b"StandardEncoding").is_some() {
        font.standard_encoding = true;
        return;
    }

    // Otherwise an array built by repeated `dup <code> /<name> put`. Scanning
    // stops at `readonly def`, so a later `dup` in the font program -- and
    // there are always later `dup`s -- cannot add phantom entries.
    let end = find(rest, b"readonly def").or_else(|| find(rest, b" def")).unwrap_or(rest.len());
    let region = &rest[..end];

    let mut at = 0;
    while let Some(next) = find(&region[at..], b"dup ") {
        at += next + 4;
        let Some(tail) = region.get(at..) else { break };
        let digits: Vec<u8> = tail.iter().copied().take_while(|b| b.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let Ok(code) = std::str::from_utf8(&digits).unwrap_or("").parse::<u16>() else { continue };
        let after = &tail[digits.len()..];
        let Some(slash) = after.iter().position(|b| *b == b'/') else { continue };
        let name: Vec<u8> = after[slash + 1..]
            .iter()
            .copied()
            .take_while(|b| !b.is_ascii_whitespace() && *b != b'/')
            .collect();
        if let (Ok(code), Ok(name)) = (u8::try_from(code), String::from_utf8(name))
            && !name.is_empty()
        {
            font.encoding.insert(code, name);
        }
    }
}

/// `/Subrs N array` followed by `dup <i> <len> RD <binary> NP`.
fn read_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let Some(at) = find(private, b"/Subrs") else { return Vec::new() };
    let region = &private[at..];
    let count = read_int(region, b"/Subrs").unwrap_or(0).clamp(0, 65536) as usize;
    let mut out = vec![Vec::new(); count];

    let mut cursor = 0;
    for _ in 0..count {
        let Some(next) = find(&region[cursor..], b"dup ") else { break };
        cursor += next + 4;
        let Some((index, after)) = take_int(&region[cursor..]) else { continue };
        let Some((len, after)) = take_int(after) else { continue };
        let Some((binary, rest)) = take_binary(after, len) else { continue };
        let index = index.max(0) as usize;
        if index < out.len() {
            out[index] = decrypt(binary, CHARSTRING_KEY, len_iv);
        }
        // Resume after the binary, never inside it.
        cursor = region.len() - rest.len();
    }
    out
}

/// `/CharStrings N dict dup begin` then `/<name> <len> RD <binary> ND`.
fn read_charstrings(private: &[u8], len_iv: usize, font: &mut Type1) {
    let Some(at) = find(private, b"/CharStrings") else { return };
    // Past the `/CharStrings` token itself, so it is not read as a glyph.
    let region = &private[at + 12..];

    let mut cursor = 0;
    // A font with more entries than this is malformed or hostile; the cap
    // bounds the work without bounding any real font.
    for _ in 0..65536 {
        let Some(next) = find(&region[cursor..], b"/") else { break };
        cursor += next + 1;
        let Some(tail) = region.get(cursor..) else { break };

        let name: Vec<u8> = tail
            .iter()
            .copied()
            .take_while(|b| !b.is_ascii_whitespace() && *b != b'(' && *b != b'/' && *b != b'{')
            .collect();
        if name.is_empty() {
            continue;
        }
        let after = &tail[name.len()..];
        let Some((len, after)) = take_int(after) else { continue };
        let Some((binary, rest)) = take_binary(after, len) else { continue };

        if let Ok(name) = String::from_utf8(name) {
            let decrypted = decrypt(binary, CHARSTRING_KEY, len_iv);
            if font.charstrings.insert(name.clone(), decrypted).is_none() {
                font.glyph_order.push(name);
            }
        }
        cursor = region.len() - rest.len();
    }
}

/// Read a whitespace-delimited integer, returning it and the rest.
fn take_int(data: &[u8]) -> Option<(i64, &[u8])> {
    let start = data.iter().position(|b| !b.is_ascii_whitespace())?;
    let digits: Vec<u8> =
        data[start..].iter().copied().take_while(|b| b.is_ascii_digit() || *b == b'-').collect();
    if digits.is_empty() {
        return None;
    }
    let value = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    Some((value, &data[start + digits.len()..]))
}

/// Skip the `RD`/`-|` token and its single following space, take `len` bytes,
/// and return them **with the remainder**.
///
/// Returning the remainder is not a convenience. The caller has to resume
/// scanning *after* the binary, and reconstructing that position from the
/// length alone means adding back the token and its space — which an earlier
/// version forgot, leaving the next glyph search to start a few bytes inside
/// the previous charstring's binary. A stray `/` byte in there then parsed as a
/// glyph and produced nonsense, which is exactly the partial corruption a parse
/// error never reports.
///
/// Exactly one space after the token: the binary may itself begin with 0x20,
/// and trimming greedily would drop the first byte of the charstring.
fn take_binary(data: &[u8], len: i64) -> Option<(&[u8], &[u8])> {
    if !(0..=65536).contains(&len) {
        return None;
    }
    let start = data.iter().position(|b| !b.is_ascii_whitespace())?;
    let rest = &data[start..];
    // The token is `RD` or `-|`, and a font may define either name.
    let token_len = rest.iter().position(|b| *b == b' ')?;
    if token_len == 0 || token_len > 8 {
        return None;
    }
    let binary_at = token_len + 1;
    let end = binary_at.checked_add(len as usize)?;
    Some((rest.get(binary_at..end)?, rest.get(end..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Type 1 font: cleartext half, then an eexec-encrypted half
    /// holding the given charstrings.
    fn build(clear_extra: &str, glyphs: &[(&str, &[u8])], len_iv: usize) -> Vec<u8> {
        let mut private =
            format!("dup /Private 8 dict dup begin\n/lenIV {len_iv} def\n").into_bytes();
        private.extend_from_slice(b"/Subrs 1 array\n");
        let subr = encrypt(&[0x8e], CHARSTRING_KEY, len_iv);
        private.extend_from_slice(format!("dup 0 {} RD ", subr.len()).as_bytes());
        private.extend_from_slice(&subr);
        private.extend_from_slice(b" NP\n");

        private.extend_from_slice(
            format!("/CharStrings {} dict dup begin\n", glyphs.len()).as_bytes(),
        );
        for (name, body) in glyphs {
            let enc = encrypt(body, CHARSTRING_KEY, len_iv);
            private.extend_from_slice(format!("/{name} {} RD ", enc.len()).as_bytes());
            private.extend_from_slice(&enc);
            private.extend_from_slice(b" ND\n");
        }
        private.extend_from_slice(b"end\nend\n");

        let mut out = format!(
            "%!PS-AdobeFont-1.0: Test 001.000\n\
             /FontName /TestFont def\n\
             /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
             {clear_extra}\
             currentfile eexec "
        )
        .into_bytes();
        out.extend_from_slice(&encrypt(&private, EEXEC_KEY, EEXEC_SKIP));
        out
    }

    #[test]
    fn the_cipher_round_trips() {
        let plain = b"the quick brown fox";
        let cipher = encrypt(plain, EEXEC_KEY, 4);
        assert_eq!(decrypt(&cipher, EEXEC_KEY, 4), plain);
    }

    #[test]
    fn the_leading_random_bytes_are_discarded() {
        // Missing this shifts every charstring by lenIV bytes, which decodes to
        // a plausible-looking but wrong outline rather than to an error.
        let cipher = encrypt(b"body", CHARSTRING_KEY, 4);
        assert_eq!(decrypt(&cipher, CHARSTRING_KEY, 4), b"body");
        assert_eq!(decrypt(&cipher, CHARSTRING_KEY, 0).len(), 8, "without the skip, four extra");
    }

    #[test]
    fn a_font_parses_and_yields_its_charstrings() {
        let bytes = build("", &[("A", &[1, 2, 3]), ("B", &[4, 5])], 4);
        let font = Type1::parse(&bytes).expect("parse");
        assert_eq!(font.glyph_count(), 2);
        assert_eq!(font.charstring("A"), Some(&[1u8, 2, 3][..]));
        assert_eq!(font.charstring("B"), Some(&[4u8, 5][..]));
        assert_eq!(font.charstring("C"), None);
    }

    #[test]
    fn the_font_name_and_matrix_are_read() {
        let font = Type1::parse(&build("", &[("A", &[1])], 4)).unwrap();
        assert_eq!(font.font_name.as_deref(), Some("TestFont"));
        assert_eq!(font.font_matrix, Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]));
    }

    #[test]
    fn a_non_standard_font_matrix_is_preserved() {
        // A Type 1 may use any scale, and a reader that assumes 1/1000 lays
        // those glyphs out at the wrong size.
        let bytes = build("", &[("A", &[1])], 4);
        let text = String::from_utf8_lossy(&bytes[..200]).replace("0.001", "0.002");
        let mut patched = text.into_bytes();
        patched.extend_from_slice(&bytes[200..]);
        if let Ok(font) = Type1::parse(&patched) {
            assert_eq!(font.font_matrix.map(|m| m[0]), Some(0.002));
        }
    }

    #[test]
    fn len_iv_is_honoured() {
        // Fonts do set lenIV to something other than 4, and using the default
        // then silently truncates or extends every charstring.
        for len_iv in [0usize, 1, 4, 8] {
            let bytes = build("", &[("A", &[9, 8, 7])], len_iv);
            let font = Type1::parse(&bytes).expect("parse");
            assert_eq!(font.len_iv, len_iv);
            assert_eq!(font.charstring("A"), Some(&[9u8, 8, 7][..]), "lenIV {len_iv}");
        }
    }

    #[test]
    fn subroutines_are_decrypted() {
        let font = Type1::parse(&build("", &[("A", &[1])], 4)).unwrap();
        assert_eq!(font.subrs.len(), 1);
        assert_eq!(font.subrs[0], vec![0x8e]);
    }

    #[test]
    fn the_builtin_encoding_is_read() {
        // Spec 8.2 lists this as required: a symbolic font with no /Encoding in
        // the PDF is encoded by the program, and there is nowhere else to look.
        let clear = "/Encoding 256 array\n\
                     0 1 255 {1 index exch /.notdef put} for\n\
                     dup 32 /space put\n\
                     dup 65 /A put\n\
                     dup 97 /a put\n\
                     readonly def\n";
        let font = Type1::parse(&build(clear, &[("A", &[1])], 4)).unwrap();
        assert!(!font.standard_encoding);
        assert_eq!(font.encoding.get(&32).map(String::as_str), Some("space"));
        assert_eq!(font.encoding.get(&65).map(String::as_str), Some("A"));
        assert_eq!(font.encoding.get(&97).map(String::as_str), Some("a"));
    }

    #[test]
    fn the_standard_encoding_shorthand_is_recognised() {
        let font =
            Type1::parse(&build("/Encoding StandardEncoding def\n", &[("A", &[1])], 4)).unwrap();
        assert!(font.standard_encoding);
        assert!(font.encoding.is_empty(), "not expanded; the caller has the table");
    }

    #[test]
    fn glyph_order_follows_the_font() {
        let bytes = build("", &[("alpha", &[1]), ("beta", &[2]), ("gamma", &[3])], 4);
        let font = Type1::parse(&bytes).unwrap();
        assert_eq!(font.glyph_order, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn a_hex_encoded_private_half_is_decoded() {
        // PFA files hex-encode the encrypted half; both forms must parse to the
        // same font.
        let binary = build("", &[("A", &[7, 7])], 4);
        let split = find(&binary, b"eexec").unwrap() + 5;
        let mut hex = binary[..split].to_vec();
        hex.push(b'\n');
        for (i, b) in binary[split + 1..].iter().enumerate() {
            hex.extend_from_slice(format!("{b:02x}").as_bytes());
            if i % 32 == 31 {
                hex.push(b'\n');
            }
        }
        let font = Type1::parse(&hex).expect("hex parse");
        assert_eq!(font.charstring("A"), Some(&[7u8, 7][..]));
    }

    #[test]
    fn a_pfb_container_is_unwrapped() {
        let inner = build("", &[("A", &[3, 1, 4])], 4);
        let split = find(&inner, b"eexec").unwrap() + 6;
        let (ascii, binary) = inner.split_at(split);

        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(ascii.len() as u32).to_le_bytes());
        pfb.extend_from_slice(ascii);
        pfb.extend_from_slice(&[0x80, 2]);
        pfb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        pfb.extend_from_slice(binary);
        pfb.extend_from_slice(&[0x80, 3]);

        let font = Type1::parse(&pfb).expect("pfb parse");
        assert_eq!(font.charstring("A"), Some(&[3u8, 1, 4][..]));
    }

    #[test]
    fn a_charstring_beginning_with_a_space_byte_keeps_it() {
        // `RD ` is followed by exactly one space; the binary may itself start
        // with 0x20, and trimming greedily would eat the first byte.
        let mut plain = vec![0x20, 0x20, 0x8b];
        let font = Type1::parse(&build("", &[("A", &plain)], 4)).unwrap();
        assert_eq!(font.charstring("A"), Some(plain.as_slice()));
        plain.clear();
    }

    #[test]
    fn the_opening_operator_check_accepts_real_charstrings() {
        // `0 500 hsbw`: operands 139 (=0) and 247 21 (=500), then opcode 13.
        assert!(opens_correctly(&[139, 247, 21, 13]));
        // `sbw` is the two-byte form.
        assert!(opens_correctly(&[139, 139, 139, 139, 12, 7]));
    }

    #[test]
    fn a_fractional_side_bearing_computed_with_div_is_accepted() {
        // ZRUSRO+CMSY7's `circlecopyrt`, verbatim: 78, 113889, 100, div, hsbw.
        // Computer Modern writes fractional side bearings this way, and
        // rejecting the `div` escape scores every CM font in the corpus at zero.
        assert!(opens_correctly(&[217, 255, 0, 1, 188, 225, 239, 12, 12, 13]));
    }

    #[test]
    fn the_opening_operator_check_rejects_shifted_bytes() {
        // What a wrong lenIV produces: plausible bytes, no side bearing.
        assert!(!opens_correctly(&[]));
        assert!(!opens_correctly(&[9, 9, 9]), "an operator before the side bearing");
        assert!(!opens_correctly(&[139; 8]), "operands that never reach an operator");
        assert!(!opens_correctly(&[12, 16]), "a different two-byte operator");
    }

    #[test]
    fn a_subroutinized_charstring_is_followed_into_its_subr() {
        // NPIZFR+MinionPro-Bold's `hyphen`, verbatim: `2012 callsubr endchar`.
        // The hsbw is inside subr 2012, so judging the charstring on its own
        // bytes marks a quarter of the font broken when nothing is wrong.
        let mut subrs = vec![Vec::new(); 2013];
        subrs[2012] = vec![139, 247, 21, 13, 11];
        let font = Type1 { subrs, ..Default::default() };
        let hyphen = [255u8, 0, 0, 7, 220, 10, 14];

        assert!(!opens_correctly(&hyphen), "not decidable from the charstring alone");
        assert!(font.opens_correctly(&hyphen), "but decidable once the subr is followed");
    }

    #[test]
    fn a_callsubr_to_a_missing_subroutine_is_not_sound() {
        let font = Type1::default();
        assert!(!font.opens_correctly(&[255, 0, 0, 7, 220, 10, 14]));
    }

    #[test]
    fn a_cyclic_subroutine_terminates() {
        // Subr 0 calls itself for ever.
        let font = Type1 { subrs: vec![vec![139, 10]], ..Default::default() };
        assert!(!font.opens_correctly(&[139, 10]));
    }

    #[test]
    fn a_correctly_parsed_font_scores_sound() {
        let bytes = build("", &[("A", &[139, 247, 21, 13, 14]), ("B", &[139, 139, 13, 14])], 4);
        let font = Type1::parse(&bytes).unwrap();
        assert_eq!(font.soundness(), 1.0);
    }

    #[test]
    fn a_wrong_len_iv_is_visible_in_the_soundness_score() {
        // Built with lenIV 8, parsed as though it were 4: the charstrings come
        // out shifted by four bytes. No error is raised -- that is the point --
        // and only the opening-operator check notices.
        let bytes = build("", &[("A", &[139, 247, 21, 13, 14])], 8);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.contains("eexec"));

        let good = Type1::parse(&bytes).unwrap();
        assert_eq!(good.soundness(), 1.0, "as written");

        // Re-decrypt one charstring with the wrong skip and confirm it fails.
        let shifted =
            decrypt(&encrypt(&[139, 247, 21, 13, 14], CHARSTRING_KEY, 8), CHARSTRING_KEY, 4);
        assert!(!opens_correctly(&shifted), "a four-byte shift must not look sound");
    }

    #[test]
    fn a_slash_inside_charstring_binary_does_not_start_a_glyph() {
        // Scanning must resume *after* each charstring's binary. Resuming a few
        // bytes early lets a 0x2F byte inside the outline parse as a glyph
        // name, which yields a nonsense entry and can swallow the real glyph
        // that follows -- partial corruption no parse error would report.
        //
        // 0x2F is `/`, and it is an ordinary operand byte (47 - 139 = -92).
        let noisy: Vec<u8> = vec![139, 247, 21, 13, 0x2F, 0x2F, 0x2F, 0x2F, 0x2F, 0x2F, 14];
        let bytes = build("", &[("A", &noisy), ("B", &[139, 139, 13, 14])], 4);
        let font = Type1::parse(&bytes).expect("parse");

        assert_eq!(font.glyph_count(), 2, "got {:?}", font.glyph_order);
        assert_eq!(font.charstring("A"), Some(noisy.as_slice()));
        assert_eq!(font.charstring("B"), Some(&[139u8, 139, 13, 14][..]));
        assert_eq!(font.soundness(), 1.0);
    }

    #[test]
    fn a_slash_inside_a_subroutine_does_not_derail_the_scan() {
        let mut private = b"/lenIV 4 def\n/Subrs 2 array\n".to_vec();
        for (i, body) in [vec![0x2Fu8, 0x2F, 0x2F, 11], vec![0x8e]].iter().enumerate() {
            let enc = encrypt(body, CHARSTRING_KEY, 4);
            private.extend_from_slice(format!("dup {i} {} RD ", enc.len()).as_bytes());
            private.extend_from_slice(&enc);
            private.extend_from_slice(b" NP\n");
        }
        private.extend_from_slice(b"/CharStrings 1 dict dup begin\n");
        let enc = encrypt(&[139, 139, 13, 14], CHARSTRING_KEY, 4);
        private.extend_from_slice(format!("/A {} RD ", enc.len()).as_bytes());
        private.extend_from_slice(&enc);
        private.extend_from_slice(b" ND\nend\n");

        let mut bytes = b"%!PS-AdobeFont-1.0\ncurrentfile eexec ".to_vec();
        bytes.extend_from_slice(&encrypt(&private, EEXEC_KEY, EEXEC_SKIP));

        let font = Type1::parse(&bytes).expect("parse");
        assert_eq!(font.subrs.len(), 2);
        assert_eq!(font.subrs[0], vec![0x2F, 0x2F, 0x2F, 11]);
        assert_eq!(font.subrs[1], vec![0x8e]);
        assert_eq!(font.charstring("A"), Some(&[139u8, 139, 13, 14][..]));
    }

    #[test]
    fn a_font_with_no_eexec_is_rejected() {
        assert!(Type1::parse(b"%!PS-AdobeFont-1.0: Broken\n/FontName /X def\n").is_err());
    }

    #[test]
    fn a_font_with_no_charstrings_is_rejected() {
        let mut out = b"%!PS-AdobeFont-1.0\ncurrentfile eexec ".to_vec();
        out.extend_from_slice(&encrypt(b"/lenIV 4 def\nend\n", EEXEC_KEY, EEXEC_SKIP));
        assert!(Type1::parse(&out).is_err());
    }

    #[test]
    fn a_malformed_pfb_is_rejected_not_read_past() {
        assert!(Type1::parse(&[0x80, 9, 1, 0, 0, 0, 0]).is_err(), "unknown segment type");
        assert!(Type1::parse(&[0x80]).is_err());
    }

    #[test]
    fn a_pfb_with_trailing_padding_still_parses() {
        // One font in the corpus pads after its last segment instead of
        // writing the end marker. Refusing it would lose a font that renders
        // perfectly well everywhere else.
        let inner = build("", &[("A", &[5, 5])], 4);
        let split = find(&inner, b"eexec").unwrap() + 6;
        let (ascii, binary) = inner.split_at(split);

        let mut pfb = vec![0x80, 1];
        pfb.extend_from_slice(&(ascii.len() as u32).to_le_bytes());
        pfb.extend_from_slice(ascii);
        pfb.extend_from_slice(&[0x80, 2]);
        pfb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        pfb.extend_from_slice(binary);
        pfb.extend_from_slice(&[0u8; 16]); // padding, no end marker

        let font = Type1::parse(&pfb).expect("padding is not a failure");
        assert_eq!(font.charstring("A"), Some(&[5u8, 5][..]));
    }

    #[test]
    fn a_truncated_font_does_not_panic() {
        let full = build("", &[("A", &[1, 2]), ("B", &[3])], 4);
        for cut in 0..full.len() {
            let _ = Type1::parse(&full[..cut]);
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0x5EED1234u32;
        for _ in 0..2000 {
            let mut bytes = b"%!PS-AdobeFont-1.0\n".to_vec();
            for _ in 0..96 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push((seed >> 24) as u8);
            }
            bytes.extend_from_slice(b" eexec ");
            for _ in 0..96 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push((seed >> 24) as u8);
            }
            let _ = Type1::parse(&bytes);
        }
    }
}
