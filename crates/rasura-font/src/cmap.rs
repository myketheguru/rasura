//! The sfnt `cmap` table. Spec 8.2, and spec 7.2's step 5.
//!
//! `cmap` maps character codes to glyph ids. §7.2 needs it **backwards**: a PDF
//! gives a code, the font turns it into a glyph, and the question is what
//! character that glyph represents. For a symbolic TrueType font with no
//! `/ToUnicode` and no usable `/Differences`, reversing the font's own Unicode
//! subtable is the only route left before guessing.
//!
//! Four formats are read: 0 (byte), 4 (BMP segments, the common one), 6
//! (trimmed) and 12 (full UCS-4). Format 2 is high-byte CJK legacy and is
//! recognised but not decoded — a font using it is old enough that its codes
//! come through a `/Encoding` CMap anyway.

use crate::sfnt::Sfnt;

/// Beyond this many entries a subtable is a font covering most of Unicode, and
/// enumerating it wholesale is not what any caller wants.
const MAX_MAPPINGS: usize = 200_000;

/// One `cmap` subtable.
#[derive(Debug, Clone, Copy)]
pub struct Subtable {
    pub platform: u16,
    pub encoding: u16,
    pub format: u16,
    /// Absolute offset of the subtable within the font.
    offset: usize,
}

/// A font's `cmap`.
#[derive(Debug, Clone, Default)]
pub struct Cmap {
    pub subtables: Vec<Subtable>,
}

impl Cmap {
    pub fn parse(data: &[u8], sfnt: &Sfnt) -> Option<Cmap> {
        let (table_at, table_len) = sfnt.table(b"cmap")?;
        let table = data.get(table_at..table_at + table_len)?;
        if table.len() < 4 {
            return None;
        }
        let count = u16::from_be_bytes([table[2], table[3]]) as usize;
        let mut subtables = Vec::new();
        for i in 0..count {
            let rec = 4 + i * 8;
            let Some(r) = table.get(rec..rec + 8) else { break };
            let platform = u16::from_be_bytes([r[0], r[1]]);
            let encoding = u16::from_be_bytes([r[2], r[3]]);
            let rel = u32::from_be_bytes([r[4], r[5], r[6], r[7]]) as usize;
            let Some(sub) = table.get(rel..rel + 2) else { continue };
            subtables.push(Subtable {
                platform,
                encoding,
                format: u16::from_be_bytes([sub[0], sub[1]]),
                offset: table_at + rel,
            });
        }
        (!subtables.is_empty()).then_some(Cmap { subtables })
    }

    fn find(&self, platform: u16, encoding: u16) -> Option<&Subtable> {
        self.subtables.iter().find(|s| s.platform == platform && s.encoding == encoding)
    }

    /// The best subtable whose codes are Unicode.
    ///
    /// Preference order is the one every shaper uses: Windows UCS-4, then
    /// Windows BMP, then any Unicode-platform table. A (3,0) symbol table is
    /// deliberately excluded — its codes are in the 0xF000 private-use block
    /// and are not characters.
    pub fn best_unicode(&self) -> Option<&Subtable> {
        self.find(3, 10)
            .or_else(|| self.find(3, 1))
            .or_else(|| self.subtables.iter().find(|s| s.platform == 0))
    }

    /// The (3,0) symbol subtable, whose codes are `0xF000 + byte`.
    pub fn symbol(&self) -> Option<&Subtable> {
        self.find(3, 0)
    }

    /// The (1,0) Macintosh subtable, indexed by a single byte.
    pub fn mac_roman(&self) -> Option<&Subtable> {
        self.find(1, 0)
    }

    /// Glyph id for a character code in a **simple** font.
    ///
    /// ISO 32000-1 §9.6.6.4 gives the order: a symbolic font's code goes
    /// through the (3,0) table, with the 0xF000 offset applied because that is
    /// where such fonts put their codes, then through (1,0). Only then is the
    /// Unicode table tried.
    pub fn simple_glyph(&self, data: &[u8], code: u32) -> Option<u16> {
        if let Some(sym) = self.symbol() {
            // Both spellings: producers disagree about whether the offset is
            // already in the code.
            for candidate in [0xF000 + (code & 0xFF), code] {
                if let Some(gid) = sym.lookup(data, candidate).filter(|g| *g != 0) {
                    return Some(gid);
                }
            }
        }
        if let Some(mac) = self.mac_roman()
            && let Some(gid) = mac.lookup(data, code).filter(|g| *g != 0)
        {
            return Some(gid);
        }
        self.best_unicode()?.lookup(data, code).filter(|g| *g != 0)
    }

    /// Glyph id to character, from the font's own Unicode table.
    ///
    /// This is §7.2 step 5. Where two codes map to one glyph the lower wins,
    /// which is the convention every other implementation uses and keeps the
    /// answer stable across runs.
    pub fn glyph_to_char(&self, data: &[u8]) -> std::collections::HashMap<u16, char> {
        let mut out = std::collections::HashMap::new();
        let Some(unicode) = self.best_unicode() else { return out };
        for (code, gid) in unicode.mappings(data) {
            if gid == 0 {
                continue;
            }
            let Some(ch) = char::from_u32(code) else { continue };
            out.entry(gid).or_insert(ch);
        }
        out
    }
}

impl Subtable {
    /// Every code-to-glyph mapping in this subtable.
    pub fn mappings(&self, data: &[u8]) -> Vec<(u32, u16)> {
        let at = self.offset;
        let mut out = Vec::new();
        match self.format {
            0 => {
                let Some(body) = data.get(at + 6..at + 6 + 256) else { return out };
                for (code, &gid) in body.iter().enumerate() {
                    if gid != 0 {
                        out.push((code as u32, gid as u16));
                    }
                }
            }
            4 => self.format4(data, &mut out),
            6 => {
                let Some(h) = data.get(at + 6..at + 10) else { return out };
                let first = u16::from_be_bytes([h[0], h[1]]) as u32;
                let count = u16::from_be_bytes([h[2], h[3]]) as usize;
                for i in 0..count.min(MAX_MAPPINGS) {
                    let Some(g) = data.get(at + 10 + i * 2..at + 12 + i * 2) else { break };
                    let gid = u16::from_be_bytes([g[0], g[1]]);
                    if gid != 0 {
                        out.push((first + i as u32, gid));
                    }
                }
            }
            12 => {
                let Some(h) = data.get(at + 12..at + 16) else { return out };
                let groups = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
                for i in 0..groups {
                    let rec = at + 16 + i * 12;
                    let Some(g) = data.get(rec..rec + 12) else { break };
                    let start = u32::from_be_bytes([g[0], g[1], g[2], g[3]]);
                    let end = u32::from_be_bytes([g[4], g[5], g[6], g[7]]);
                    let gid = u32::from_be_bytes([g[8], g[9], g[10], g[11]]);
                    if end < start {
                        continue;
                    }
                    for k in 0..=(end - start).min(0xFFFF) {
                        if out.len() >= MAX_MAPPINGS {
                            return out;
                        }
                        if let Ok(g) = u16::try_from(gid + k) {
                            out.push((start + k, g));
                        }
                    }
                }
            }
            // Format 2 is high-byte CJK legacy; those codes arrive through a
            // /Encoding CMap rather than through here.
            _ => {}
        }
        out
    }

    fn format4(&self, data: &[u8], out: &mut Vec<(u32, u16)>) {
        let at = self.offset;
        let Some(h) = data.get(at + 6..at + 8) else { return };
        let seg_count = u16::from_be_bytes([h[0], h[1]]) as usize / 2;
        if seg_count == 0 {
            return;
        }
        let ends = at + 14;
        let starts = ends + seg_count * 2 + 2;
        let deltas = starts + seg_count * 2;
        let ranges = deltas + seg_count * 2;

        let read = |p: usize| -> Option<u16> {
            data.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
        };

        for i in 0..seg_count {
            let (Some(end), Some(start), Some(delta), Some(range)) = (
                read(ends + i * 2),
                read(starts + i * 2),
                read(deltas + i * 2),
                read(ranges + i * 2),
            ) else {
                return;
            };
            if start > end {
                continue;
            }
            for code in start..=end {
                if out.len() >= MAX_MAPPINGS {
                    return;
                }
                // 0xFFFF terminates the table and is not a real mapping.
                if code == 0xFFFF {
                    continue;
                }
                let gid = if range == 0 {
                    code.wrapping_add(delta)
                } else {
                    // The offset is measured from the idRangeOffset entry
                    // itself, which is the piece of this format everyone gets
                    // wrong at least once.
                    let p = ranges + i * 2 + range as usize + (code - start) as usize * 2;
                    match read(p) {
                        Some(0) | None => continue,
                        Some(g) => g.wrapping_add(delta),
                    }
                };
                if gid != 0 {
                    out.push((code as u32, gid));
                }
            }
        }
    }

    /// Glyph id for one code.
    pub fn lookup(&self, data: &[u8], code: u32) -> Option<u16> {
        let at = self.offset;
        match self.format {
            0 => {
                let byte = usize::try_from(code).ok().filter(|c| *c < 256)?;
                data.get(at + 6 + byte).map(|g| *g as u16)
            }
            4 => {
                let code = u16::try_from(code).ok()?;
                let h = data.get(at + 6..at + 8)?;
                let seg_count = u16::from_be_bytes([h[0], h[1]]) as usize / 2;
                let ends = at + 14;
                let starts = ends + seg_count * 2 + 2;
                let deltas = starts + seg_count * 2;
                let ranges = deltas + seg_count * 2;
                let read = |p: usize| -> Option<u16> {
                    data.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
                };

                for i in 0..seg_count {
                    let end = read(ends + i * 2)?;
                    if code > end {
                        continue;
                    }
                    let start = read(starts + i * 2)?;
                    if code < start {
                        return None;
                    }
                    let delta = read(deltas + i * 2)?;
                    let range = read(ranges + i * 2)?;
                    return Some(if range == 0 {
                        code.wrapping_add(delta)
                    } else {
                        let p = ranges + i * 2 + range as usize + (code - start) as usize * 2;
                        match read(p)? {
                            0 => return None,
                            g => g.wrapping_add(delta),
                        }
                    });
                }
                None
            }
            6 => {
                let h = data.get(at + 6..at + 10)?;
                let first = u16::from_be_bytes([h[0], h[1]]) as u32;
                let count = u16::from_be_bytes([h[2], h[3]]) as u32;
                let i = code.checked_sub(first).filter(|i| *i < count)?;
                let p = at + 10 + i as usize * 2;
                data.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
            }
            12 => {
                let h = data.get(at + 12..at + 16)?;
                let groups = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
                for i in 0..groups {
                    let rec = at + 16 + i * 12;
                    let g = data.get(rec..rec + 12)?;
                    let start = u32::from_be_bytes([g[0], g[1], g[2], g[3]]);
                    let end = u32::from_be_bytes([g[4], g[5], g[6], g[7]]);
                    if code >= start && code <= end {
                        let gid = u32::from_be_bytes([g[8], g[9], g[10], g[11]]) + (code - start);
                        return u16::try_from(gid).ok();
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap subtable bytes in a cmap table and an sfnt.
    fn font(subtables: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let n = subtables.len();
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&(n as u16).to_be_bytes());
        let mut body = Vec::new();
        let base = 4 + n * 8;
        for (platform, encoding, data) in subtables {
            cmap.extend_from_slice(&platform.to_be_bytes());
            cmap.extend_from_slice(&encoding.to_be_bytes());
            cmap.extend_from_slice(&((base + body.len()) as u32).to_be_bytes());
            body.extend_from_slice(data);
        }
        cmap.extend_from_slice(&body);

        // A minimal sfnt carrying just this cmap.
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(b"cmap");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&28u32.to_be_bytes());
        out.extend_from_slice(&(cmap.len() as u32).to_be_bytes());
        out.extend_from_slice(&cmap);
        out
    }

    /// Format 4 with one segment covering `start..=end` via idDelta.
    fn format4(start: u16, end: u16, delta: u16) -> Vec<u8> {
        let seg_count = 2u16; // the real segment plus the mandatory 0xFFFF one
        let mut t = Vec::new();
        t.extend_from_slice(&4u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes()); // length, patched below
        t.extend_from_slice(&0u16.to_be_bytes()); // language
        t.extend_from_slice(&(seg_count * 2).to_be_bytes());
        t.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
        t.extend_from_slice(&end.to_be_bytes());
        t.extend_from_slice(&0xFFFFu16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        t.extend_from_slice(&start.to_be_bytes());
        t.extend_from_slice(&0xFFFFu16.to_be_bytes());
        t.extend_from_slice(&delta.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
        t.extend_from_slice(&0u16.to_be_bytes());
        let len = t.len() as u16;
        t[2..4].copy_from_slice(&len.to_be_bytes());
        t
    }

    fn parse(bytes: &[u8]) -> Cmap {
        let sfnt = Sfnt::parse(bytes).expect("sfnt");
        Cmap::parse(bytes, &sfnt).expect("cmap")
    }

    #[test]
    fn format4_maps_through_id_delta() {
        // 'A'..'Z' to gids 1..26: delta = 1 - 0x41.
        let bytes = font(&[(3, 1, format4(0x41, 0x5A, 1u16.wrapping_sub(0x41)))]);
        let cmap = parse(&bytes);
        let sub = cmap.best_unicode().expect("(3,1)");
        assert_eq!(sub.lookup(&bytes, 0x41), Some(1));
        assert_eq!(sub.lookup(&bytes, 0x5A), Some(26));
        assert_eq!(sub.lookup(&bytes, 0x40), None, "below the segment");
        assert_eq!(sub.lookup(&bytes, 0x7A), None, "above the segment");
    }

    #[test]
    fn format4_enumerates_every_mapping() {
        let bytes = font(&[(3, 1, format4(0x41, 0x43, 1u16.wrapping_sub(0x41)))]);
        let cmap = parse(&bytes);
        let m = cmap.best_unicode().unwrap().mappings(&bytes);
        assert_eq!(m, vec![(0x41, 1), (0x42, 2), (0x43, 3)]);
    }

    #[test]
    fn the_terminating_segment_is_not_a_mapping() {
        // Every format 4 table ends with a 0xFFFF..0xFFFF segment. Emitting it
        // would map U+FFFF, a non-character, to a real glyph.
        let bytes = font(&[(3, 1, format4(0x41, 0x41, 0))]);
        let m = parse(&bytes).best_unicode().unwrap().mappings(&bytes);
        assert!(m.iter().all(|(c, _)| *c != 0xFFFF), "{m:?}");
    }

    #[test]
    fn format0_maps_bytes() {
        let mut t = vec![0u8; 6 + 256];
        t[0..2].copy_from_slice(&0u16.to_be_bytes());
        t[6 + 65] = 7;
        let bytes = font(&[(1, 0, t)]);
        let cmap = parse(&bytes);
        assert_eq!(cmap.mac_roman().unwrap().lookup(&bytes, 65), Some(7));
        assert_eq!(cmap.mac_roman().unwrap().mappings(&bytes), vec![(65, 7)]);
    }

    #[test]
    fn format6_maps_a_trimmed_range() {
        let mut t = Vec::new();
        t.extend_from_slice(&6u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&100u16.to_be_bytes()); // firstCode
        t.extend_from_slice(&3u16.to_be_bytes()); // entryCount
        for g in [11u16, 12, 13] {
            t.extend_from_slice(&g.to_be_bytes());
        }
        let bytes = font(&[(3, 1, t)]);
        let sub = *parse(&bytes).best_unicode().unwrap();
        assert_eq!(sub.lookup(&bytes, 101), Some(12));
        assert_eq!(sub.lookup(&bytes, 99), None);
        assert_eq!(sub.lookup(&bytes, 103), None);
    }

    #[test]
    fn format12_maps_beyond_the_bmp() {
        let mut t = Vec::new();
        t.extend_from_slice(&12u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0u32.to_be_bytes()); // length
        t.extend_from_slice(&0u32.to_be_bytes()); // language
        t.extend_from_slice(&1u32.to_be_bytes()); // nGroups
        t.extend_from_slice(&0x1F600u32.to_be_bytes());
        t.extend_from_slice(&0x1F602u32.to_be_bytes());
        t.extend_from_slice(&50u32.to_be_bytes());
        let bytes = font(&[(3, 10, t)]);
        let sub = *parse(&bytes).best_unicode().unwrap();
        assert_eq!(sub.format, 12);
        assert_eq!(sub.lookup(&bytes, 0x1F601), Some(51));
    }

    #[test]
    fn a_symbol_table_is_not_offered_as_unicode() {
        // (3,0) codes live in the 0xF000 private-use block; treating them as
        // characters yields private-use text that looks like a successful
        // mapping and is not one.
        let bytes = font(&[(3, 0, format4(0xF041, 0xF05A, 1u16.wrapping_sub(0xF041)))]);
        let cmap = parse(&bytes);
        assert!(cmap.symbol().is_some());
        assert!(cmap.best_unicode().is_none(), "a symbol table is not a Unicode table");
    }

    #[test]
    fn a_symbolic_font_finds_glyphs_through_the_f000_offset() {
        let bytes = font(&[(3, 0, format4(0xF041, 0xF05A, 1u16.wrapping_sub(0xF041)))]);
        let cmap = parse(&bytes);
        // The PDF supplies code 0x41; the font stores it at 0xF041.
        assert_eq!(cmap.simple_glyph(&bytes, 0x41), Some(1));
    }

    #[test]
    fn a_symbolic_font_that_stores_codes_unshifted_also_works() {
        // Producers disagree about whether the 0xF000 offset is already in the
        // code, so both spellings are tried.
        let bytes = font(&[(3, 0, format4(0x41, 0x5A, 1u16.wrapping_sub(0x41)))]);
        assert_eq!(parse(&bytes).simple_glyph(&bytes, 0x41), Some(1));
    }

    #[test]
    fn the_reverse_map_is_glyph_to_character() {
        let bytes = font(&[(3, 1, format4(0x41, 0x43, 1u16.wrapping_sub(0x41)))]);
        let reverse = parse(&bytes).glyph_to_char(&bytes);
        assert_eq!(reverse.get(&1), Some(&'A'));
        assert_eq!(reverse.get(&3), Some(&'C'));
        assert_eq!(reverse.get(&9), None);
    }

    #[test]
    fn the_lower_code_wins_when_two_map_to_one_glyph() {
        // Both 0x41 and 0x61 to gid 1. Stability across runs matters more than
        // which one is chosen, and the lower code is the convention.
        let mut t = Vec::new();
        t.extend_from_slice(&6u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0x41u16.to_be_bytes());
        t.extend_from_slice(&0x21u16.to_be_bytes()); // 0x41..=0x61
        for i in 0..0x21 {
            let gid: u16 = if i == 0 || i == 0x20 { 1 } else { 99 };
            t.extend_from_slice(&gid.to_be_bytes());
        }
        let bytes = font(&[(3, 1, t)]);
        assert_eq!(parse(&bytes).glyph_to_char(&bytes).get(&1), Some(&'A'));
    }

    #[test]
    fn windows_ucs4_outranks_windows_bmp() {
        let bmp = format4(0x41, 0x41, 0);
        let mut ucs4 = Vec::new();
        ucs4.extend_from_slice(&12u16.to_be_bytes());
        ucs4.extend_from_slice(&0u16.to_be_bytes());
        ucs4.extend_from_slice(&0u32.to_be_bytes());
        ucs4.extend_from_slice(&0u32.to_be_bytes());
        ucs4.extend_from_slice(&0u32.to_be_bytes()); // nGroups 0
        let bytes = font(&[(3, 1, bmp), (3, 10, ucs4)]);
        assert_eq!(parse(&bytes).best_unicode().unwrap().format, 12);
    }

    #[test]
    fn a_font_without_cmap_yields_nothing() {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let sfnt = Sfnt::parse(&out).unwrap();
        assert!(Cmap::parse(&out, &sfnt).is_none());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0xC0FFEEu32;
        for _ in 0..2000 {
            let mut body = Vec::new();
            for _ in 0..96 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                body.push((seed >> 24) as u8);
            }
            let bytes = font(&[(3, 1, body)]);
            if let Ok(sfnt) = Sfnt::parse(&bytes)
                && let Some(cmap) = Cmap::parse(&bytes, &sfnt)
            {
                for sub in &cmap.subtables {
                    let _ = sub.mappings(&bytes);
                    for code in [0u32, 0x41, 0xF041, 0xFFFF, 0x1F600] {
                        let _ = sub.lookup(&bytes, code);
                    }
                }
                let _ = cmap.glyph_to_char(&bytes);
            }
        }
    }
}
