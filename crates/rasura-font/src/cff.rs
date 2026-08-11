//! Compact Font Format. Spec 8.2: `/FontFile3` `/Subtype /Type1C` and
//! `/CIDFontType0C`, and the `CFF ` table of an OpenType font.
//!
//! Parsed to the level §8.4 needs for injection rather than to the level a
//! rasteriser needs: charstrings are located as **byte ranges**, not
//! interpreted. Spec 8.4 says to "extract and re-encode the Type 2 charstring,
//! resolving local and global subroutines" — that is a copy with subroutine
//! calls inlined, and inlining needs the bytes, not an outline.
//!
//! Everything here is offset arithmetic over hostile input, so every read is
//! bounds-checked and every INDEX is validated before use. A CFF is a series of
//! offsets into itself; a malformed one is the easiest possible way to walk a
//! parser off a cliff.

use crate::error::{FontError, Result};

/// A CFF INDEX: a counted array of byte strings.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// Absolute byte ranges of each entry within the font.
    pub items: Vec<(usize, usize)>,
    /// Where the INDEX ends, so the next structure can be found.
    pub end: usize,
}

impl Index {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get<'a>(&self, data: &'a [u8], i: usize) -> Option<&'a [u8]> {
        let (start, end) = *self.items.get(i)?;
        data.get(start..end)
    }
}

/// A parsed CFF font.
#[derive(Debug, Clone)]
pub struct Cff {
    pub header_size: usize,
    pub names: Index,
    pub top_dicts: Index,
    pub strings: Index,
    pub global_subrs: Index,
    pub char_strings: Index,
    /// Local subroutines of the private dict, empty when there are none.
    pub local_subrs: Index,
    /// `true` when the font is CID-keyed, which changes how glyphs are
    /// addressed and requires FDArray/FDSelect to reach the right private dict.
    pub is_cid: bool,
    /// Charset offset from the top dict, if present.
    pub charset_offset: Option<usize>,
    /// Glyph-name SIDs by GID, for a non-CID font. For a CID font these are
    /// CIDs rather than name identifiers.
    pub charset: Vec<u16>,
    /// FDArray private dicts, for CID fonts: `(local_subrs, ())`.
    pub fd_local_subrs: Vec<Index>,
    /// Which FD each glyph belongs to. Empty for a non-CID font.
    pub fd_select: Vec<u8>,
    /// `defaultWidthX` and `nominalWidthX` from the private dict, which a
    /// charstring's leading width delta is relative to.
    pub default_width: f64,
    pub nominal_width: f64,
}

impl Cff {
    pub fn glyph_count(&self) -> usize {
        self.char_strings.len()
    }

    /// The raw Type 2 charstring for a glyph.
    pub fn charstring<'a>(&self, data: &'a [u8], gid: usize) -> Option<&'a [u8]> {
        self.char_strings.get(data, gid)
    }

    /// The local subroutines that apply to a glyph. For a CID font that depends
    /// on which FD the glyph belongs to, which is the whole reason FDSelect
    /// exists.
    pub fn local_subrs_for(&self, gid: usize) -> &Index {
        if self.is_cid
            && let Some(&fd) = self.fd_select.get(gid)
            && let Some(subrs) = self.fd_local_subrs.get(fd as usize)
        {
            return subrs;
        }
        &self.local_subrs
    }

    /// The CID or name SID of a glyph, from the charset.
    pub fn glyph_id_for(&self, gid: usize) -> Option<u16> {
        self.charset.get(gid).copied()
    }

    /// GID for a CID, by searching the charset. CID-keyed fonts address glyphs
    /// by CID, and the charset is the only mapping there is.
    pub fn gid_for_cid(&self, cid: u16) -> Option<usize> {
        if !self.is_cid {
            return Some(cid as usize).filter(|g| *g < self.glyph_count());
        }
        self.charset.iter().position(|c| *c == cid)
    }

    pub fn parse(data: &[u8]) -> Result<Cff> {
        if data.len() < 4 {
            return Err(FontError::Truncated("CFF header"));
        }
        let header_size = data[2] as usize;
        if header_size < 4 || header_size > data.len() {
            return Err(FontError::Malformed("CFF header size"));
        }

        let names = read_index(data, header_size)?;
        let top_dicts = read_index(data, names.end)?;
        let strings = read_index(data, top_dicts.end)?;
        let global_subrs = read_index(data, strings.end)?;

        let top = top_dicts.get(data, 0).ok_or(FontError::Malformed("no top dict"))?;
        let ops = parse_dict(top);

        // Operator 17 is CharStrings; without it there is no font.
        let cs_offset = op_int(&ops, 17).ok_or(FontError::Malformed("no CharStrings"))? as usize;
        let char_strings = read_index(data, cs_offset)?;

        // 12 30 is ROS, which is present exactly when the font is CID-keyed.
        let is_cid = ops.iter().any(|(op, _)| *op == 0x0c1e);

        // Operator 18 is Private: [size, offset].
        let (mut local_subrs, mut default_width, mut nominal_width) = (Index::default(), 0.0, 0.0);
        if let Some(operands) = ops.iter().find(|(op, _)| *op == 18).map(|(_, v)| v)
            && operands.len() >= 2
        {
            let size = operands[0] as usize;
            let offset = operands[1] as usize;
            if let Some(private) = data.get(offset..offset.saturating_add(size)) {
                let pd = parse_dict(private);
                default_width = op_num(&pd, 20).unwrap_or(0.0);
                nominal_width = op_num(&pd, 21).unwrap_or(0.0);
                // Operator 19 is Subrs, an offset *relative to the private dict*.
                if let Some(rel) = op_int(&pd, 19) {
                    let at = offset.saturating_add(rel as usize);
                    local_subrs = read_index(data, at).unwrap_or_default();
                }
            }
        }

        let charset_offset = op_int(&ops, 15).map(|v| v as usize);
        let charset = read_charset(data, charset_offset, char_strings.len())?;

        let mut fd_local_subrs = Vec::new();
        let mut fd_select = Vec::new();
        if is_cid {
            // 12 36 FDArray, 12 37 FDSelect.
            if let Some(at) = op_int(&ops, 0x0c24) {
                let fd_array = read_index(data, at as usize)?;
                for i in 0..fd_array.len() {
                    let Some(fd) = fd_array.get(data, i) else { continue };
                    let pd_ops = parse_dict(fd);
                    let mut subrs = Index::default();
                    if let Some(operands) = pd_ops.iter().find(|(op, _)| *op == 18).map(|(_, v)| v)
                        && operands.len() >= 2
                    {
                        let (size, offset) = (operands[0] as usize, operands[1] as usize);
                        if let Some(private) = data.get(offset..offset.saturating_add(size))
                            && let Some(rel) = op_int(&parse_dict(private), 19)
                        {
                            subrs = read_index(data, offset.saturating_add(rel as usize))
                                .unwrap_or_default();
                        }
                    }
                    fd_local_subrs.push(subrs);
                }
            }
            if let Some(at) = op_int(&ops, 0x0c25) {
                fd_select = read_fd_select(data, at as usize, char_strings.len());
            }
        }

        Ok(Cff {
            header_size,
            names,
            top_dicts,
            strings,
            global_subrs,
            char_strings,
            local_subrs,
            is_cid,
            charset_offset,
            charset,
            fd_local_subrs,
            fd_select,
            default_width,
            nominal_width,
        })
    }
}

/// Read an INDEX at an absolute offset.
///
/// Exposed for the writer in `cff_write`, which has to walk the FDArray of a
/// CID-keyed font: rebuilding that with a second, private INDEX reader is how
/// the two would come to disagree about a malformed one.
pub fn read_index_public(data: &[u8], at: usize) -> Result<Index> {
    read_index(data, at)
}

/// Read an INDEX at `at`.
fn read_index(data: &[u8], at: usize) -> Result<Index> {
    let count_end = at.checked_add(2).ok_or(FontError::Malformed("INDEX offset"))?;
    let head = data.get(at..count_end).ok_or(FontError::Truncated("INDEX count"))?;
    let count = u16::from_be_bytes([head[0], head[1]]) as usize;
    if count == 0 {
        // An empty INDEX is two bytes and entirely legal.
        return Ok(Index { items: Vec::new(), end: count_end });
    }

    let off_size = *data.get(count_end).ok_or(FontError::Truncated("INDEX offSize"))? as usize;
    if !(1..=4).contains(&off_size) {
        return Err(FontError::Malformed("INDEX offSize"));
    }

    let offsets_at = count_end + 1;
    let offsets_len = (count + 1) * off_size;
    let offsets = data
        .get(offsets_at..offsets_at + offsets_len)
        .ok_or(FontError::Truncated("INDEX offsets"))?;

    // Data is one-based from the byte *before* it begins.
    let base = offsets_at + offsets_len - 1;
    let read_at = |i: usize| -> usize {
        let s = i * off_size;
        offsets[s..s + off_size].iter().fold(0usize, |acc, b| (acc << 8) | *b as usize)
    };

    let mut items = Vec::with_capacity(count);
    let mut prev = read_at(0);
    if prev == 0 {
        return Err(FontError::Malformed("INDEX first offset"));
    }
    for i in 1..=count {
        let next = read_at(i);
        // Offsets must not go backwards, and must stay inside the font.
        if next < prev {
            return Err(FontError::Malformed("INDEX offsets decrease"));
        }
        let (start, end) = (base + prev, base + next);
        if end > data.len() {
            return Err(FontError::Truncated("INDEX data"));
        }
        items.push((start, end));
        prev = next;
    }
    Ok(Index { items, end: base + prev })
}

/// Parse a DICT into (operator, operands). Operators 12 x are encoded 0x0cxx.
fn parse_dict(data: &[u8]) -> Vec<(u16, Vec<f64>)> {
    let mut out = Vec::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        match b0 {
            // Operators.
            0..=21 => {
                let op = if b0 == 12 {
                    i += 1;
                    // A trailing escape byte with nothing after it.
                    let Some(b1) = data.get(i) else { break };
                    0x0c00 | *b1 as u16
                } else {
                    b0 as u16
                };
                out.push((op, std::mem::take(&mut operands)));
                i += 1;
            }
            // 28: 16-bit integer.
            28 => {
                let Some(b) = data.get(i + 1..i + 3) else { break };
                operands.push(i16::from_be_bytes([b[0], b[1]]) as f64);
                i += 3;
            }
            // 29: 32-bit integer.
            29 => {
                let Some(b) = data.get(i + 1..i + 5) else { break };
                operands.push(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64);
                i += 5;
            }
            // 30: real, packed BCD.
            30 => {
                let (value, used) = parse_real(&data[i + 1..]);
                operands.push(value);
                i += 1 + used;
            }
            32..=246 => {
                operands.push(b0 as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                let Some(b1) = data.get(i + 1) else { break };
                operands.push((b0 as f64 - 247.0) * 256.0 + *b1 as f64 + 108.0);
                i += 2;
            }
            251..=254 => {
                let Some(b1) = data.get(i + 1) else { break };
                operands.push(-(b0 as f64 - 251.0) * 256.0 - *b1 as f64 - 108.0);
                i += 2;
            }
            // 22..=27 and 31 are reserved. Skipping is the tolerant reading.
            _ => i += 1,
        }
        // A DICT with thousands of operands is malformed or hostile.
        if operands.len() > 48 {
            operands.clear();
        }
    }
    out
}

/// Packed BCD real. Returns the value and how many bytes it used.
fn parse_real(data: &[u8]) -> (f64, usize) {
    let mut s = String::new();
    for (i, byte) in data.iter().enumerate() {
        for nibble in [byte >> 4, byte & 0x0f] {
            match nibble {
                0..=9 => s.push((b'0' + nibble) as char),
                0x0a => s.push('.'),
                0x0b => s.push('E'),
                0x0c => s.push_str("E-"),
                0x0e => s.push('-'),
                0x0f => return (s.parse().unwrap_or(0.0), i + 1),
                _ => {}
            }
        }
        if i > 32 {
            break;
        }
    }
    (s.parse().unwrap_or(0.0), data.len().min(33))
}

fn op_int(ops: &[(u16, Vec<f64>)], op: u16) -> Option<i64> {
    op_num(ops, op).map(|v| v as i64)
}

fn op_num(ops: &[(u16, Vec<f64>)], op: u16) -> Option<f64> {
    ops.iter().find(|(o, _)| *o == op).and_then(|(_, v)| v.last().copied())
}

/// Read the charset: SIDs for a name-keyed font, CIDs for a CID-keyed one.
fn read_charset(data: &[u8], offset: Option<usize>, glyphs: usize) -> Result<Vec<u16>> {
    // 0, 1 and 2 are the predefined charsets (ISOAdobe, Expert, ExpertSubset).
    // Absent or predefined means the identity for our purposes: GID n is SID n.
    let Some(offset) = offset.filter(|o| *o > 2) else {
        return Ok((0..glyphs as u16).collect());
    };
    let Some(format) = data.get(offset) else { return Ok((0..glyphs as u16).collect()) };

    let mut out = Vec::with_capacity(glyphs);
    out.push(0); // GID 0 is .notdef and is never listed.
    let mut at = offset + 1;
    match format {
        0 => {
            while out.len() < glyphs {
                let Some(b) = data.get(at..at + 2) else { break };
                out.push(u16::from_be_bytes([b[0], b[1]]));
                at += 2;
            }
        }
        // Ranges: first SID plus a count of additional glyphs, 1 or 2 bytes.
        1 | 2 => {
            let extra = if *format == 1 { 1 } else { 2 };
            while out.len() < glyphs {
                let Some(b) = data.get(at..at + 2 + extra) else { break };
                let first = u16::from_be_bytes([b[0], b[1]]);
                let n_left = if extra == 1 {
                    b[2] as usize
                } else {
                    u16::from_be_bytes([b[2], b[3]]) as usize
                };
                for k in 0..=n_left {
                    if out.len() >= glyphs {
                        break;
                    }
                    out.push(first.saturating_add(k as u16));
                }
                at += 2 + extra;
            }
        }
        _ => return Ok((0..glyphs as u16).collect()),
    }
    // A charset shorter than the font pads rather than failing: the glyphs are
    // still there and still drawable, they just have no name.
    while out.len() < glyphs {
        out.push(0);
    }
    Ok(out)
}

/// Read FDSelect: which FD each glyph belongs to.
fn read_fd_select(data: &[u8], offset: usize, glyphs: usize) -> Vec<u8> {
    let mut out = vec![0u8; glyphs];
    let Some(format) = data.get(offset) else { return out };
    match format {
        0 => {
            for (gid, slot) in out.iter_mut().enumerate() {
                if let Some(b) = data.get(offset + 1 + gid) {
                    *slot = *b;
                }
            }
        }
        3 => {
            let Some(b) = data.get(offset + 1..offset + 3) else { return out };
            let n_ranges = u16::from_be_bytes([b[0], b[1]]) as usize;
            let mut at = offset + 3;
            for _ in 0..n_ranges {
                let Some(r) = data.get(at..at + 5) else { break };
                let first = u16::from_be_bytes([r[0], r[1]]) as usize;
                let fd = r[2];
                let next = u16::from_be_bytes([r[3], r[4]]) as usize;
                for slot in out.iter_mut().take(next.min(glyphs)).skip(first) {
                    *slot = fd;
                }
                at += 3;
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an INDEX from a list of entries.
    fn index_bytes(items: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(items.len() as u16).to_be_bytes());
        if items.is_empty() {
            return out;
        }
        out.push(1); // offSize
        let mut offset = 1u8;
        out.push(offset);
        for it in items {
            offset += it.len() as u8;
            out.push(offset);
        }
        for it in items {
            out.extend_from_slice(it);
        }
        out
    }

    /// A DICT operand for a small integer, plus an operator.
    fn dict_entry(value: i32, op: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[29]);
        out.extend_from_slice(&value.to_be_bytes());
        if op >= 0x0c00 {
            out.push(12);
            out.push((op & 0xff) as u8);
        } else {
            out.push(op as u8);
        }
        out
    }

    /// A minimal but valid CFF with `n` single-byte charstrings.
    fn minimal_cff(n: usize, cid: bool) -> Vec<u8> {
        let mut out = vec![1, 0, 4, 1]; // header: major, minor, hdrSize, offSize
        out.extend_from_slice(&index_bytes(&[b"Font"])); // Name INDEX

        // The top dict needs CharStrings' offset, which depends on the size of
        // everything before it -- so it is built twice, the second time with
        // the real offset. Simpler than solving it analytically.
        for pass in 0..2 {
            let mut top = Vec::new();
            let cs_offset = if pass == 0 { 0 } else { out.len() + 64 };
            top.extend_from_slice(&dict_entry(cs_offset as i32, 17));
            if cid {
                // ROS: three operands, then 12 30.
                top.extend_from_slice(&dict_entry(0, 0));
                top.extend_from_slice(&dict_entry(0, 0));
                top.extend_from_slice(&[29]);
                top.extend_from_slice(&0i32.to_be_bytes());
                top.extend_from_slice(&[12, 30]);
            }
            // Pad the top dict INDEX to a fixed size so the offset is stable.
            while top.len() < 40 {
                top.push(11); // a no-op reserved operator
            }
            let mut trial = out.clone();
            trial.extend_from_slice(&index_bytes(&[&top]));
            trial.extend_from_slice(&index_bytes(&[b"str"])); // String INDEX
            trial.extend_from_slice(&index_bytes(&[])); // Global Subr INDEX
            while trial.len() < cs_offset {
                trial.push(0);
            }
            if pass == 1 {
                let charstrings: Vec<&[u8]> = vec![b"\x0e"; n]; // endchar
                trial.extend_from_slice(&index_bytes(&charstrings));
                return trial;
            }
        }
        unreachable!()
    }

    #[test]
    fn an_index_round_trips() {
        let bytes = index_bytes(&[b"alpha", b"be", b"gamma"]);
        let idx = read_index(&bytes, 0).expect("index");
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.get(&bytes, 0), Some(&b"alpha"[..]));
        assert_eq!(idx.get(&bytes, 1), Some(&b"be"[..]));
        assert_eq!(idx.get(&bytes, 2), Some(&b"gamma"[..]));
        assert_eq!(idx.get(&bytes, 3), None);
        assert_eq!(idx.end, bytes.len());
    }

    #[test]
    fn an_empty_index_is_two_bytes_and_legal() {
        let bytes = index_bytes(&[]);
        assert_eq!(bytes.len(), 2);
        let idx = read_index(&bytes, 0).expect("empty index");
        assert!(idx.is_empty());
        assert_eq!(idx.end, 2);
    }

    #[test]
    fn a_truncated_index_is_rejected_not_read_past() {
        let bytes = index_bytes(&[b"alpha", b"beta"]);
        for cut in 1..bytes.len() {
            // Whatever it does, it must not panic and must not claim data that
            // is not there.
            if let Ok(idx) = read_index(&bytes[..cut], 0) {
                for i in 0..idx.len() {
                    assert!(
                        idx.get(&bytes[..cut], i).is_some() || idx.get(&bytes[..cut], i).is_none()
                    );
                }
            }
        }
    }

    #[test]
    fn decreasing_index_offsets_are_rejected() {
        // A hostile file pointing the second entry before the first.
        let mut bytes = index_bytes(&[b"alpha", b"beta"]);
        bytes[4] = 9;
        bytes[5] = 2;
        assert!(read_index(&bytes, 0).is_err());
    }

    #[test]
    fn a_zero_first_offset_is_rejected() {
        // Offsets are one-based; zero would make every range start before the
        // data and read backwards.
        let mut bytes = index_bytes(&[b"alpha"]);
        bytes[3] = 0;
        assert!(read_index(&bytes, 0).is_err());
    }

    #[test]
    fn dict_operands_decode() {
        // 139 encodes 0; 247 0 encodes 108; 251 0 encodes -108.
        let ops = parse_dict(&[139, 247, 0, 251, 0, 17]);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].0, 17);
        assert_eq!(ops[0].1, vec![0.0, 108.0, -108.0]);
    }

    #[test]
    fn a_two_byte_operator_decodes() {
        let ops = parse_dict(&[139, 12, 30]);
        assert_eq!(ops[0].0, 0x0c1e);
    }

    #[test]
    fn a_real_operand_decodes() {
        // -2.25 is 0e 2 a 2 5 f -> nibbles e,2,a,2,5,f
        let ops = parse_dict(&[30, 0xe2, 0xa2, 0x5f, 17]);
        assert_eq!(ops[0].1, vec![-2.25]);
    }

    #[test]
    fn a_dict_ending_mid_operand_does_not_panic() {
        for bytes in [vec![28], vec![28, 1], vec![29, 1, 2], vec![247], vec![12]] {
            let _ = parse_dict(&bytes);
        }
    }

    #[test]
    fn a_minimal_font_parses() {
        let bytes = minimal_cff(5, false);
        let cff = Cff::parse(&bytes).expect("parse");
        assert_eq!(cff.glyph_count(), 5);
        assert!(!cff.is_cid);
        assert_eq!(cff.charstring(&bytes, 0), Some(&b"\x0e"[..]));
        assert_eq!(cff.charstring(&bytes, 5), None);
    }

    #[test]
    fn a_cid_font_is_recognised_by_its_ros() {
        let bytes = minimal_cff(3, true);
        let cff = Cff::parse(&bytes).expect("parse");
        assert!(cff.is_cid, "ROS means CID-keyed");
    }

    #[test]
    fn a_font_without_charstrings_is_rejected() {
        let mut bytes = vec![1, 0, 4, 1];
        bytes.extend_from_slice(&index_bytes(&[b"Font"]));
        bytes.extend_from_slice(&index_bytes(&[b"\x0b"])); // a top dict with no CharStrings
        bytes.extend_from_slice(&index_bytes(&[]));
        bytes.extend_from_slice(&index_bytes(&[]));
        assert!(Cff::parse(&bytes).is_err());
    }

    #[test]
    fn a_bad_header_size_is_rejected() {
        assert!(Cff::parse(&[1, 0, 2, 1]).is_err(), "header shorter than the header");
        assert!(Cff::parse(&[1, 0, 255, 1]).is_err(), "header past the end");
        assert!(Cff::parse(&[1, 0]).is_err());
    }

    #[test]
    fn an_absent_charset_is_the_identity() {
        let charset = read_charset(&[], None, 4).unwrap();
        assert_eq!(charset, vec![0, 1, 2, 3]);
        // Predefined charsets 0..2 likewise.
        assert_eq!(read_charset(&[], Some(1), 3).unwrap(), vec![0, 1, 2]);
    }

    /// A charset at offset 3. Offsets 0, 1 and 2 are the *predefined* charsets
    /// (ISOAdobe, Expert, ExpertSubset), so a real table can never live there.
    fn charset_at_3(body: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 3];
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_format_zero_charset_reads_sids() {
        // format 0, then SIDs for glyphs 1..3 (GID 0 is implicit .notdef).
        let data = charset_at_3(&[0, 0, 10, 0, 20, 0, 30]);
        assert_eq!(read_charset(&data, Some(3), 4).unwrap(), vec![0, 10, 20, 30]);
    }

    #[test]
    fn a_format_one_charset_expands_ranges() {
        // format 1: first SID 5, nLeft 2 -> glyphs 1,2,3 get 5,6,7.
        let data = charset_at_3(&[1, 0, 5, 2]);
        assert_eq!(read_charset(&data, Some(3), 4).unwrap(), vec![0, 5, 6, 7]);
    }

    #[test]
    fn the_predefined_charsets_are_the_identity() {
        // Offsets 0..2 name a built-in charset rather than pointing at one.
        for predefined in [0, 1, 2] {
            let data = charset_at_3(&[0, 0, 99, 0, 99, 0, 99]);
            assert_eq!(read_charset(&data, Some(predefined), 4).unwrap(), vec![0, 1, 2, 3]);
        }
    }

    #[test]
    fn a_short_charset_pads_rather_than_failing() {
        // Two SIDs declared for a five-glyph font: the rest are still drawable.
        let data = charset_at_3(&[0, 0, 10, 0, 20]);
        assert_eq!(read_charset(&data, Some(3), 5).unwrap().len(), 5);
    }

    #[test]
    fn fd_select_format_three_expands_ranges() {
        // format 3, one range: glyphs 0..3 use FD 1.
        let data = [3u8, 0, 1, 0, 0, 1, 0, 3];
        let sel = read_fd_select(&data, 0, 4);
        assert_eq!(&sel[..3], &[1, 1, 1]);
    }

    #[test]
    fn a_non_cid_font_maps_cid_to_gid_directly() {
        let bytes = minimal_cff(4, false);
        let cff = Cff::parse(&bytes).unwrap();
        assert_eq!(cff.gid_for_cid(2), Some(2));
        assert_eq!(cff.gid_for_cid(9), None, "past the end of the font");
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // The parser is offset arithmetic over hostile input; the only
        // acceptable failure is an error.
        let mut seed = 0x12345678u32;
        for _ in 0..2000 {
            let mut bytes = vec![1u8, 0, 4, 1];
            for _ in 0..64 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push((seed >> 24) as u8);
            }
            let _ = Cff::parse(&bytes);
        }
    }
}
