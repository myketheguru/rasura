//! CMaps. ISO 32000-1 §9.7.5 and §9.10.3.
//!
//! Two different things share this syntax, and both belong here:
//!
//! * an `/Encoding` CMap on a composite font, which defines the **codespace
//!   ranges** and maps codes to CIDs. Without the codespace ranges you cannot
//!   even split a string into character codes, so this is squarely a
//!   content-layer concern -- positioning depends on it.
//! * a `/ToUnicode` CMap, which maps codes to text.
//!
//! What is *not* here is §7.2's seven-strategy Unicode derivation chain. That
//! belongs in `rasura-layout`, and it uses this module as its first step.
//! Parsing a CMap is mechanical; deciding what to do when it is missing or wrong
//! is reconstruction.

use rasura_cos::object::{Dictionary, Object, is_whitespace};
use std::collections::HashMap;

/// One `begincodespacerange` entry: how many bytes a code takes, and the range
/// of values that length covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodespaceRange {
    pub bytes: usize,
    pub low: u32,
    pub high: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CMap {
    codespaces: Vec<CodespaceRange>,
    /// `cidchar` entries.
    cid_single: HashMap<u32, u32>,
    /// `cidrange` entries as (low, high, first cid).
    cid_ranges: Vec<(u32, u32, u32)>,
    /// `bfchar` and `bfrange` entries.
    unicode: HashMap<u32, String>,
    /// `/WMode`: 0 horizontal, 1 vertical.
    pub wmode: u8,
    /// True for `Identity-H`/`Identity-V`, where CID equals code.
    pub identity: bool,
    /// A `usecmap` this parser did not follow.
    pub uses_external: Option<String>,
}

impl CMap {
    /// `Identity-H`: two-byte codes, CID equal to code.
    pub fn identity(wmode: u8) -> Self {
        CMap {
            codespaces: vec![CodespaceRange { bytes: 2, low: 0, high: 0xffff }],
            wmode,
            identity: true,
            ..Default::default()
        }
    }

    /// A single-byte CMap, which is what a simple font behaves like.
    pub fn single_byte() -> Self {
        CMap {
            codespaces: vec![CodespaceRange { bytes: 1, low: 0, high: 0xff }],
            identity: true,
            ..Default::default()
        }
    }

    /// Recognise a predefined CMap by name.
    ///
    /// Only the Identity maps are built in. The Adobe collections
    /// (`UniJIS-UCS2-H` and the rest) ship as data files that are not vendored;
    /// their codespaces are two-byte in practice, so a two-byte identity is a
    /// serviceable approximation for *positioning*, and the caller is told the
    /// mapping is approximate rather than being left to assume it is exact.
    pub fn predefined(name: &[u8]) -> Option<(CMap, bool)> {
        let wmode = if name.ends_with(b"-V") { 1 } else { 0 };
        match name {
            b"Identity-H" => Some((CMap::identity(0), true)),
            b"Identity-V" => Some((CMap::identity(1), true)),
            // Known Adobe collections, approximated. The bool reports that.
            n if n.starts_with(b"UniJIS")
                || n.starts_with(b"UniGB")
                || n.starts_with(b"UniCNS")
                || n.starts_with(b"UniKS")
                || n.starts_with(b"UniAKR")
                || n.starts_with(b"ETen")
                || n.starts_with(b"90ms")
                || n.starts_with(b"GBK")
                || n.starts_with(b"B5pc") =>
            {
                Some((CMap::identity(wmode), false))
            }
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.codespaces.is_empty() && self.cid_single.is_empty() && self.cid_ranges.is_empty()
    }

    pub fn codespaces(&self) -> &[CodespaceRange] {
        &self.codespaces
    }

    pub fn unicode_entries(&self) -> usize {
        self.unicode.len()
    }

    /// Read the next character code from a string.
    ///
    /// ISO 32000-1 §9.7.6.2: match the longest codespace range whose byte
    /// length the remaining bytes can supply and whose value range contains the
    /// candidate. Falls back to the shortest declared length, then to one byte,
    /// so a malformed CMap still yields forward progress rather than a hang.
    pub fn next_code(&self, bytes: &[u8]) -> (u32, usize) {
        if bytes.is_empty() {
            return (0, 0);
        }
        if self.codespaces.is_empty() {
            return (bytes[0] as u32, 1);
        }

        // Try each length that a codespace declares, shortest first: a 1-byte
        // range must win over a 2-byte one for a code it contains.
        let mut lengths: Vec<usize> = self.codespaces.iter().map(|c| c.bytes).collect();
        lengths.sort_unstable();
        lengths.dedup();

        for &len in &lengths {
            if len == 0 || len > bytes.len() || len > 4 {
                continue;
            }
            let value = be_value(&bytes[..len]);
            if self.codespaces.iter().any(|c| c.bytes == len && value >= c.low && value <= c.high) {
                return (value, len);
            }
        }

        // No range matched. Use the shortest declared length that fits, which
        // is what viewers do, rather than desynchronising the whole string.
        let len = lengths.iter().copied().find(|&l| l > 0 && l <= bytes.len()).unwrap_or(1).min(4);
        (be_value(&bytes[..len.min(bytes.len())]), len.min(bytes.len()).max(1))
    }

    /// Every code in a string, as `(code, byte offset, byte length)`.
    pub fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize, usize)> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let (code, len) = self.next_code(&bytes[i..]);
            let len = len.max(1);
            out.push((code, i, len));
            i += len;
        }
        out
    }

    /// Map a character code to a CID.
    pub fn cid(&self, code: u32) -> u32 {
        if self.identity {
            return code;
        }
        if let Some(&cid) = self.cid_single.get(&code) {
            return cid;
        }
        for &(lo, hi, first) in &self.cid_ranges {
            if code >= lo && code <= hi {
                return first.wrapping_add(code - lo);
            }
        }
        // ISO 32000-1: an unmapped code maps to CID 0, the notdef glyph.
        0
    }

    /// The text a code maps to, from a `/ToUnicode` CMap.
    ///
    /// A destination that decodes to nothing -- `<>`, or a hex string too short
    /// to form a UTF-16 unit -- is reported as *no* mapping rather than as an
    /// empty one. The distinction matters downstream: an empty string counted as
    /// a successful mapping makes a page look fully mapped while producing no
    /// text, which is the worst of both answers. Files doing this exist.
    pub fn unicode(&self, code: u32) -> Option<&str> {
        self.unicode.get(&code).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }

    /// Parse a CMap stream.
    pub fn parse(data: &[u8]) -> CMap {
        let mut out = CMap::default();
        let mut i = 0usize;

        while i < data.len() {
            if let Some(rest) = starts_with_at(data, i, b"begincodespacerange") {
                i = out.read_codespaces(data, rest);
            } else if let Some(rest) = starts_with_at(data, i, b"begincidrange") {
                i = out.read_cid_ranges(data, rest);
            } else if let Some(rest) = starts_with_at(data, i, b"begincidchar") {
                i = out.read_cid_chars(data, rest);
            } else if let Some(rest) = starts_with_at(data, i, b"beginbfrange") {
                i = out.read_bf_ranges(data, rest);
            } else if let Some(rest) = starts_with_at(data, i, b"beginbfchar") {
                i = out.read_bf_chars(data, rest);
            } else if let Some(rest) = starts_with_at(data, i, b"usecmap") {
                // Following it needs the resource that names it; record and move on.
                out.uses_external.get_or_insert_with(|| "usecmap".to_string());
                i = rest;
            } else if let Some(rest) = starts_with_at(data, i, b"/WMode") {
                let mut j = skip_ws(data, rest);
                if data.get(j) == Some(&b'1') {
                    out.wmode = 1;
                }
                j += 1;
                i = j;
            } else {
                i += 1;
            }
        }

        if out.codespaces.is_empty() {
            // A CMap with no codespace range is malformed. Two bytes is the
            // right default for a CID CMap; one byte for a /ToUnicode on a
            // simple font. Infer from the widest code seen.
            let widest = out
                .unicode
                .keys()
                .chain(out.cid_single.keys())
                .copied()
                .chain(out.cid_ranges.iter().map(|r| r.1))
                .max()
                .unwrap_or(0);
            let bytes = if widest > 0xff { 2 } else { 1 };
            out.codespaces.push(CodespaceRange {
                bytes,
                low: 0,
                high: if bytes == 1 { 0xff } else { 0xffff },
            });
        }
        out
    }

    fn read_codespaces(&mut self, data: &[u8], mut i: usize) -> usize {
        loop {
            let Some((lo, next)) = next_hex(data, i) else { return i };
            let Some((hi, next2)) = next_hex(data, next) else { return next };
            if !lo.is_empty() && lo.len() == hi.len() && lo.len() <= 4 {
                self.codespaces.push(CodespaceRange {
                    bytes: lo.len(),
                    low: be_value(&lo),
                    high: be_value(&hi),
                });
            }
            i = next2;
            if ended(data, i, b"endcodespacerange") || i >= data.len() {
                return i;
            }
        }
    }

    fn read_cid_ranges(&mut self, data: &[u8], mut i: usize) -> usize {
        loop {
            let Some((lo, n1)) = next_hex(data, i) else { return i };
            let Some((hi, n2)) = next_hex(data, n1) else { return n1 };
            let (cid, n3) = next_int(data, n2);
            self.cid_ranges.push((be_value(&lo), be_value(&hi), cid));
            i = n3;
            if ended(data, i, b"endcidrange") || i >= data.len() {
                return i;
            }
        }
    }

    fn read_cid_chars(&mut self, data: &[u8], mut i: usize) -> usize {
        loop {
            let Some((code, n1)) = next_hex(data, i) else { return i };
            let (cid, n2) = next_int(data, n1);
            self.cid_single.insert(be_value(&code), cid);
            i = n2;
            if ended(data, i, b"endcidchar") || i >= data.len() {
                return i;
            }
        }
    }

    fn read_bf_chars(&mut self, data: &[u8], mut i: usize) -> usize {
        loop {
            let Some((src, n1)) = next_hex(data, i) else { return i };
            let Some((dst, n2)) = next_hex(data, n1) else { return n1 };
            self.unicode.insert(be_value(&src), utf16be(&dst));
            i = n2;
            if ended(data, i, b"endbfchar") || i >= data.len() {
                return i;
            }
        }
    }

    fn read_bf_ranges(&mut self, data: &[u8], mut i: usize) -> usize {
        loop {
            let Some((lo, n1)) = next_hex(data, i) else { return i };
            let Some((hi, n2)) = next_hex(data, n1) else { return n1 };
            let lo_v = be_value(&lo);
            let hi_v = be_value(&hi);

            let after = skip_ws(data, n2);
            if data.get(after) == Some(&b'[') {
                // One destination per code.
                let mut j = after + 1;
                let mut code = lo_v;
                loop {
                    let k = skip_ws(data, j);
                    if data.get(k) == Some(&b']') {
                        i = k + 1;
                        break;
                    }
                    let Some((dst, n)) = next_hex(data, j) else {
                        i = k;
                        break;
                    };
                    self.unicode.insert(code, utf16be(&dst));
                    code = code.saturating_add(1);
                    j = n;
                    if j >= data.len() {
                        i = j;
                        break;
                    }
                }
            } else {
                let Some((dst, n3)) = next_hex(data, n2) else { return n2 };
                i = n3;
                // The last UTF-16 unit increments across the range.
                let span = hi_v.saturating_sub(lo_v).min(0xffff);
                for k in 0..=span {
                    let mut bytes = dst.clone();
                    if bytes.len() >= 2 {
                        let n = bytes.len();
                        let last = u16::from_be_bytes([bytes[n - 2], bytes[n - 1]]);
                        let bumped = last.wrapping_add(k as u16).to_be_bytes();
                        bytes[n - 2] = bumped[0];
                        bytes[n - 1] = bumped[1];
                    }
                    self.unicode.insert(lo_v.saturating_add(k), utf16be(&bytes));
                }
            }

            if ended(data, i, b"endbfrange") || i >= data.len() {
                return i;
            }
        }
    }
}

/// Build a CMap for a `/Encoding` entry on a Type0 font.
///
/// Returns the map plus whether it is exact. An approximated predefined CMap
/// positions correctly for the two-byte collections but does not map to the
/// right CIDs, so the caller must not present its output as authoritative.
pub fn encoding_cmap(doc: &rasura_cos::document::Document, font: &Dictionary) -> (CMap, bool) {
    match doc.get_entry(font, "Encoding").ok().flatten().as_deref() {
        Some(Object::Name(n)) => {
            CMap::predefined(n.as_bytes()).unwrap_or_else(|| (CMap::identity(0), false))
        }
        Some(Object::Stream(_)) => {
            // An embedded CMap stream.
            if let Some(Object::Reference(id)) = font.get("Encoding")
                && let Ok(data) = doc.decoded_stream(*id)
            {
                let parsed = CMap::parse(&data);
                let exact = parsed.uses_external.is_none();
                return (parsed, exact);
            }
            (CMap::identity(0), false)
        }
        _ => (CMap::identity(0), false),
    }
}

// ---------------------------------------------------------------------------
// Lexical helpers
// ---------------------------------------------------------------------------

fn starts_with_at(data: &[u8], i: usize, kw: &[u8]) -> Option<usize> {
    if data[i..].starts_with(kw) { Some(i + kw.len()) } else { None }
}

fn ended(data: &[u8], i: usize, kw: &[u8]) -> bool {
    let j = skip_ws(data, i);
    data.get(j..).is_some_and(|s| s.starts_with(kw))
}

fn skip_ws(data: &[u8], mut i: usize) -> usize {
    while i < data.len() && is_whitespace(data[i]) {
        i += 1;
    }
    i
}

/// The next `<...>` hex string. Stops at a letter so a missing `>` cannot run
/// past the end of a section into the next keyword.
fn next_hex(data: &[u8], mut i: usize) -> Option<(Vec<u8>, usize)> {
    while i < data.len() && data[i] != b'<' {
        if data[i].is_ascii_alphabetic() || data[i] == b'[' || data[i] == b']' {
            return None;
        }
        i += 1;
    }
    if i >= data.len() {
        return None;
    }
    let start = i + 1;
    let mut j = start;
    while j < data.len() && data[j] != b'>' {
        j += 1;
    }
    if j >= data.len() {
        return None;
    }
    let mut bytes = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in &data[start..j] {
        let Some(v) = hex_val(b) else { continue };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                bytes.push(h << 4 | v);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        bytes.push(h << 4);
    }
    Some((bytes, j + 1))
}

fn next_int(data: &[u8], mut i: usize) -> (u32, usize) {
    i = skip_ws(data, i);
    let start = i;
    while i < data.len() && data[i].is_ascii_digit() {
        i += 1;
    }
    let v = std::str::from_utf8(&data[start..i]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    (v, i)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn be_value(bytes: &[u8]) -> u32 {
    bytes.iter().take(4).fold(0u32, |acc, &b| acc << 8 | b as u32)
}

/// Decode a UTF-16BE destination, resolving surrogate pairs.
fn utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    if units.is_empty() {
        return bytes.iter().map(|&b| b as char).collect();
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_code_to_itself_in_two_bytes() {
        let c = CMap::identity(0);
        assert_eq!(c.next_code(&[0x00, 0x41, 0x00, 0x42]), (0x0041, 2));
        assert_eq!(c.cid(0x0041), 0x0041);
        assert_eq!(c.codes(&[0x00, 0x41, 0x00, 0x42]).len(), 2);
    }

    #[test]
    fn single_byte_cmap_reads_one_byte_at_a_time() {
        let c = CMap::single_byte();
        let codes = c.codes(b"AB");
        assert_eq!(codes, vec![(0x41, 0, 1), (0x42, 1, 1)]);
    }

    #[test]
    fn mixed_codespace_lengths_are_resolved_shortest_first() {
        // A real mixed CMap: 0x00-0x80 is one byte, 0x8140-0x9ffc is two.
        let src = b"2 begincodespacerange\n<00> <80>\n<8140> <9ffc>\nendcodespacerange";
        let c = CMap::parse(src);
        assert_eq!(c.codespaces().len(), 2);
        // A byte below 0x81 is a one-byte code.
        assert_eq!(c.next_code(&[0x41, 0x42]), (0x41, 1));
        // A byte in the lead range starts a two-byte code.
        assert_eq!(c.next_code(&[0x81, 0x40]), (0x8140, 2));
    }

    #[test]
    fn cid_ranges_and_chars_map() {
        let src = b"1 begincidrange\n<0020> <007e> 1\nendcidrange\n\
                    1 begincidchar\n<00ff> 500\nendcidchar";
        let c = CMap::parse(src);
        assert!(!c.identity);
        assert_eq!(c.cid(0x20), 1);
        assert_eq!(c.cid(0x21), 2);
        assert_eq!(c.cid(0x7e), 95);
        assert_eq!(c.cid(0xff), 500);
        // Unmapped codes are notdef, not a guess.
        assert_eq!(c.cid(0x1000), 0);
    }

    #[test]
    fn bfchar_and_bfrange_map_to_text() {
        let src = b"1 beginbfchar\n<01> <0041>\nendbfchar\n\
                    1 beginbfrange\n<10> <12> <0061>\nendbfrange";
        let c = CMap::parse(src);
        assert_eq!(c.unicode(0x01), Some("A"));
        assert_eq!(c.unicode(0x10), Some("a"));
        assert_eq!(c.unicode(0x11), Some("b"));
        assert_eq!(c.unicode(0x12), Some("c"));
        assert_eq!(c.unicode(0x13), None);
    }

    #[test]
    fn bfrange_with_an_array_destination() {
        let src = b"1 beginbfrange\n<10> <12> [<0041> <0042> <0043>]\nendbfrange";
        let c = CMap::parse(src);
        assert_eq!(c.unicode(0x10), Some("A"));
        assert_eq!(c.unicode(0x12), Some("C"));
    }

    #[test]
    fn surrogate_pairs_and_ligatures_decode() {
        let src = b"2 beginbfchar\n<01> <D835DC00>\n<02> <00660069>\nendbfchar";
        let c = CMap::parse(src);
        assert_eq!(c.unicode(0x01), Some("\u{1d400}"));
        assert_eq!(c.unicode(0x02), Some("fi"));
    }

    #[test]
    fn an_empty_destination_is_no_mapping_not_an_empty_one() {
        // Real files map codes to `<>`. Counting that as a successful mapping
        // makes a page report as fully mapped while producing no text at all --
        // found by the pdf.js differential on issue11922_reduced.pdf.
        let c = CMap::parse(b"2 beginbfchar\n<01> <>\n<02> <0041>\nendbfchar");
        assert_eq!(c.unicode(0x01), None);
        assert_eq!(c.unicode(0x02), Some("A"));
    }

    #[test]
    fn wmode_is_read() {
        assert_eq!(CMap::parse(b"/WMode 1 def").wmode, 1);
        assert_eq!(CMap::parse(b"/WMode 0 def").wmode, 0);
        assert_eq!(CMap::identity(1).wmode, 1);
    }

    #[test]
    fn predefined_identity_is_exact_and_collections_are_not() {
        let (c, exact) = CMap::predefined(b"Identity-H").unwrap();
        assert!(exact && c.identity && c.wmode == 0);
        let (c, exact) = CMap::predefined(b"Identity-V").unwrap();
        assert!(exact && c.wmode == 1);
        // A collection CMap is approximated, and says so.
        let (c, exact) = CMap::predefined(b"UniJIS-UCS2-H").unwrap();
        assert!(!exact, "an approximation must not be reported as exact");
        assert_eq!(c.codespaces()[0].bytes, 2);
        assert_eq!(CMap::predefined(b"UniJIS-UCS2-V").unwrap().0.wmode, 1);
        assert!(CMap::predefined(b"NotARealCMap").is_none());
    }

    #[test]
    fn a_cmap_with_no_codespace_infers_one() {
        // Common in /ToUnicode maps written by hand.
        let one = CMap::parse(b"1 beginbfchar\n<41> <0041>\nendbfchar");
        assert_eq!(one.codespaces()[0].bytes, 1);
        let two = CMap::parse(b"1 beginbfchar\n<0141> <0041>\nendbfchar");
        assert_eq!(two.codespaces()[0].bytes, 2);
    }

    #[test]
    fn codes_always_make_progress() {
        // Regression guard: a malformed CMap must not stall the iterator.
        for src in [
            b"0 begincodespacerange endcodespacerange".to_vec(),
            b"garbage".to_vec(),
            b"begincodespacerange <> <> endcodespacerange".to_vec(),
        ] {
            let c = CMap::parse(&src);
            let codes = c.codes(b"abcdef");
            assert!(!codes.is_empty());
            assert!(codes.iter().all(|(_, _, len)| *len >= 1));
            let total: usize = codes.iter().map(|(_, _, l)| *l).sum();
            assert!(total >= 6, "every byte must be consumed");
        }
    }

    #[test]
    fn a_trailing_partial_code_is_still_consumed() {
        // An odd-length string in a two-byte encoding, which damaged files have.
        let c = CMap::identity(0);
        let codes = c.codes(&[0x00, 0x41, 0x00]);
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[1].2, 1, "the stray byte is consumed, not looped on");
    }

    #[test]
    fn usecmap_is_recorded_rather_than_silently_ignored() {
        let c = CMap::parse(b"/Adobe-Japan1-UCS2 usecmap\n1 beginbfchar\n<01> <0041>\nendbfchar");
        assert!(c.uses_external.is_some());
    }
}
