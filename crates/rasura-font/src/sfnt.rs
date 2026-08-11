//! The sfnt container: TrueType and OpenType. Spec 8.2.
//!
//! Modelled as a **table directory over borrowed ranges**, not as parsed
//! structures. Spec 8.4's injection procedure appends to `glyf`, rebuilds
//! `loca`, extends `hmtx` and bumps counters in `hhea` and `maxp` — all of that
//! is byte surgery on tables that must otherwise survive untouched, and §2
//! forbids rewriting what an edit did not need to touch. A model that parsed
//! everything into structs and re-serialised would rewrite every table on every
//! save.
//!
//! The tables that *are* decoded here are the small ones injection has to
//! agree with: `head` (for `indexToLocFormat`), `maxp` (`numGlyphs`), `hhea`
//! (`numberOfHMetrics`), `loca` and `hmtx`.

use crate::error::{FontError, Result};

/// A parsed sfnt table directory.
#[derive(Debug, Clone)]
pub struct Sfnt {
    /// Tag, offset and length of every table, in directory order.
    pub tables: Vec<([u8; 4], usize, usize)>,
    pub num_glyphs: u16,
    /// `head.indexToLocFormat`: 0 for short `loca`, 1 for long.
    pub index_to_loc_format: i16,
    /// `head.unitsPerEm`, the scale every metric is in.
    pub units_per_em: u16,
    /// `hhea.numberOfHMetrics`.
    pub number_of_h_metrics: u16,
    /// Glyph offsets from `loca`, `num_glyphs + 1` entries when present.
    pub loca: Vec<u32>,
    /// Byte position of the table directory within the data.
    ///
    /// Zero for an ordinary font. For a **TrueType Collection** the directory
    /// sits past the `ttcf` header, and every offset it contains is still
    /// measured from byte zero of the whole file — so the buffer must not be
    /// resliced to the directory, only indexed from it. Anything rebuilding the
    /// font also needs this to find the sub-font's real sfnt version tag, which
    /// is *not* the `ttcf` at the front.
    pub directory: usize,
}

impl Sfnt {
    pub fn parse(data: &[u8]) -> Result<Sfnt> {
        if data.len() < 12 {
            return Err(FontError::Truncated("sfnt header"));
        }
        // A TrueType Collection: use the first font in it. Editing a collection
        // in place is out of scope, but reading one should not fail outright.
        //
        // The directory is *located*, not sliced to. A collection's table
        // offsets are absolute from byte zero of the file, so reslicing to the
        // directory would shift every one of them by the header's length --
        // silently, since the shifted bytes still decode as plausible numbers.
        let directory = if &data[..4] == b"ttcf" { collection_directory(data)? } else { 0 };

        let num_tables = u16::from_be_bytes([data[directory + 4], data[directory + 5]]) as usize;
        if directory + 12 + num_tables * 16 > data.len() {
            return Err(FontError::Malformed("sfnt table count"));
        }

        let mut tables = Vec::with_capacity(num_tables);
        for i in 0..num_tables {
            let rec = directory + 12 + i * 16;
            let tag: [u8; 4] = data[rec..rec + 4].try_into().expect("four bytes");
            let offset =
                u32::from_be_bytes(data[rec + 8..rec + 12].try_into().expect("four")) as usize;
            let length =
                u32::from_be_bytes(data[rec + 12..rec + 16].try_into().expect("four")) as usize;
            // Clamped, not rejected: a truncated font is usually still usable,
            // and refusing it loses a document that every viewer renders.
            let offset = offset.min(data.len());
            let length = length.min(data.len() - offset);
            tables.push((tag, offset, length));
        }

        let head = find(&tables, b"head").and_then(|(o, l)| data.get(o..o + l));
        let (units_per_em, index_to_loc_format) = match head {
            Some(h) if h.len() >= 54 => {
                (u16::from_be_bytes([h[18], h[19]]), i16::from_be_bytes([h[50], h[51]]))
            }
            // 1000 is the CFF convention and a safer default than zero, which
            // would make every metric infinite.
            _ => (1000, 0),
        };

        let num_glyphs = find(&tables, b"maxp")
            .and_then(|(o, l)| data.get(o..o + l))
            .filter(|m| m.len() >= 6)
            .map(|m| u16::from_be_bytes([m[4], m[5]]))
            .unwrap_or(0);

        let number_of_h_metrics = find(&tables, b"hhea")
            .and_then(|(o, l)| data.get(o..o + l))
            .filter(|h| h.len() >= 36)
            .map(|h| u16::from_be_bytes([h[34], h[35]]))
            .unwrap_or(0);

        let loca = find(&tables, b"loca")
            .and_then(|(o, l)| data.get(o..o + l))
            .map(|l| read_loca(l, index_to_loc_format, num_glyphs))
            .unwrap_or_default();

        Ok(Sfnt {
            tables,
            num_glyphs,
            index_to_loc_format,
            units_per_em,
            number_of_h_metrics,
            loca,
            directory,
        })
    }

    /// A table's byte range, by tag.
    pub fn table(&self, tag: &[u8; 4]) -> Option<(usize, usize)> {
        find(&self.tables, tag)
    }

    pub fn table_data<'a>(&self, data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
        let (offset, length) = self.table(tag)?;
        data.get(offset..offset + length)
    }

    pub fn has(&self, tag: &[u8; 4]) -> bool {
        self.table(tag).is_some()
    }

    /// Whether the outlines are `glyf` or `CFF `.
    pub fn is_cff(&self) -> bool {
        self.has(b"CFF ")
    }

    /// The raw outline bytes of a glyph, from `glyf` via `loca`.
    ///
    /// Returns an empty slice for a glyph with no outline: `loca[n] ==
    /// loca[n+1]` means a blank glyph such as `space`, which is not an error.
    pub fn glyph_data<'a>(&self, data: &'a [u8], gid: u16) -> Option<&'a [u8]> {
        let (start, end) =
            (*self.loca.get(gid as usize)? as usize, *self.loca.get(gid as usize + 1)? as usize);
        if end < start {
            return None;
        }
        let (glyf_at, glyf_len) = self.table(b"glyf")?;
        data.get(glyf_at + start..glyf_at + end.min(glyf_len))
    }

    /// Glyph names from the `post` table, indexed by glyph id.
    ///
    /// Only version 2.0 carries names; 3.0 declares that it has none, which is
    /// what most modern fonts ship because names cost space and only text
    /// extraction wants them. An empty result therefore means "this font
    /// declines to say", not "parsing failed".
    ///
    /// Indices below 258 name one of the standard Macintosh glyphs rather than
    /// carrying a string — without that table two-thirds of a typical font's
    /// names are unreadable.
    pub fn post_names(&self, data: &[u8]) -> Vec<String> {
        let Some(post) = self.table_data(data, b"post") else { return Vec::new() };
        if post.len() < 34
            || u32::from_be_bytes([post[0], post[1], post[2], post[3]]) != 0x0002_0000
        {
            return Vec::new();
        }
        let count = u16::from_be_bytes([post[32], post[33]]) as usize;
        let Some(indices) = post.get(34..34 + count * 2) else { return Vec::new() };

        // The Pascal strings that follow, in order.
        let mut extra: Vec<String> = Vec::new();
        let mut at = 34 + count * 2;
        while at < post.len() {
            let len = post[at] as usize;
            at += 1;
            let Some(s) = post.get(at..at + len) else { break };
            extra.push(String::from_utf8_lossy(s).into_owned());
            at += len;
        }

        (0..count)
            .map(|i| {
                let idx = u16::from_be_bytes([indices[i * 2], indices[i * 2 + 1]]) as usize;
                match idx {
                    0..=257 => crate::standard14::MAC_GLYPH_ORDER
                        .get(idx)
                        .map(|s| (*s).to_string())
                        .unwrap_or_default(),
                    _ => extra.get(idx - 258).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Advance width of a glyph, in font units.
    ///
    /// `hmtx` stores full metrics only for the first `numberOfHMetrics`
    /// glyphs; every glyph after that repeats the last advance. That is the
    /// format's compression for fonts whose tail is monospaced, and reading it
    /// as an out-of-range error loses the widths of most CJK fonts.
    pub fn advance(&self, data: &[u8], gid: u16) -> Option<u16> {
        let hmtx = self.table_data(data, b"hmtx")?;
        let n = self.number_of_h_metrics as usize;
        if n == 0 {
            return None;
        }
        let i = (gid as usize).min(n - 1);
        let at = i * 4;
        hmtx.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
}

fn find(tables: &[([u8; 4], usize, usize)], tag: &[u8; 4]) -> Option<(usize, usize)> {
    tables.iter().find(|(t, _, _)| t == tag).map(|(_, o, l)| (*o, *l))
}

/// Where the first font's table directory begins in a TrueType Collection.
///
/// A position, not a slice. The directory's table offsets are absolute within
/// the whole file, so the buffer has to stay whole; returning `&data[offset..]`
/// would leave every offset short by the header's length. That is a quiet kind
/// of wrong — the shifted bytes still decode as numbers, so `maxp` yields a
/// glyph count and `head` yields an em size, both plausible and both from the
/// wrong place.
fn collection_directory(data: &[u8]) -> Result<usize> {
    // ttcf header: tag, version, numFonts, then an offset per font.
    if data.len() < 16 {
        return Err(FontError::Truncated("ttc header"));
    }
    let offset = u32::from_be_bytes(data[12..16].try_into().expect("four")) as usize;
    // Room for the sub-font's own twelve-byte header, not merely in bounds.
    if offset + 12 > data.len() {
        return Err(FontError::Malformed("ttc offset"));
    }
    Ok(offset)
}

fn read_loca(data: &[u8], format: i16, num_glyphs: u16) -> Vec<u32> {
    let want = num_glyphs as usize + 1;
    let mut out = Vec::with_capacity(want);
    if format == 0 {
        // Short format stores offsets divided by two.
        for i in 0..want {
            let at = i * 2;
            match data.get(at..at + 2) {
                Some(b) => out.push(u16::from_be_bytes([b[0], b[1]]) as u32 * 2),
                None => break,
            }
        }
    } else {
        for i in 0..want {
            let at = i * 4;
            match data.get(at..at + 4) {
                Some(b) => out.push(u32::from_be_bytes([b[0], b[1], b[2], b[3]])),
                None => break,
            }
        }
    }
    out
}

/// Whether a `loca` table needs the long format.
///
/// Spec 8.4 calls this out by name: injection must "widen to long format if the
/// table crosses the 32k short-format limit -- this is a common silent
/// corruption". The short format stores offsets halved in 16 bits, so it cannot
/// address beyond 128 KB of `glyf`, and a font that crosses that boundary
/// mid-injection silently points every later glyph at the wrong bytes.
pub fn loca_needs_long_format(glyf_length: usize) -> bool {
    glyf_length > 0x1FFFE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an sfnt from (tag, data) pairs.
    fn build(magic: &[u8; 4], tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let n = tables.len();
        let mut out = Vec::new();
        out.extend_from_slice(magic);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);

        let mut offset = 12 + n * 16;
        for (tag, data) in tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            // Tables are four-byte aligned in a real font; padding keeps the
            // offsets honest.
            offset += data.len().next_multiple_of(4);
        }
        for (_, data) in tables {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    fn head(units_per_em: u16, loc_format: i16) -> Vec<u8> {
        let mut h = vec![0u8; 54];
        h[18..20].copy_from_slice(&units_per_em.to_be_bytes());
        h[50..52].copy_from_slice(&loc_format.to_be_bytes());
        h
    }

    fn maxp(num_glyphs: u16) -> Vec<u8> {
        let mut m = vec![0u8; 32];
        m[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
        m
    }

    fn hhea(n: u16) -> Vec<u8> {
        let mut h = vec![0u8; 36];
        h[34..36].copy_from_slice(&n.to_be_bytes());
        h
    }

    /// Three glyphs: 0 is empty, 1 is four bytes, 2 is four bytes.
    fn simple_font() -> Vec<u8> {
        let loca: Vec<u8> = [0u16, 0, 2, 4].iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut hmtx = Vec::new();
        for w in [500u16, 600, 700] {
            hmtx.extend_from_slice(&w.to_be_bytes());
            hmtx.extend_from_slice(&0u16.to_be_bytes());
        }
        build(
            &[0x00, 0x01, 0x00, 0x00],
            &[
                (b"head", head(2048, 0)),
                (b"maxp", maxp(3)),
                (b"hhea", hhea(3)),
                (b"loca", loca),
                (b"glyf", b"AABB".to_vec()),
                (b"hmtx", hmtx),
            ],
        )
    }

    /// Wrap a font in a `ttcf` header, the way a collection carries one.
    ///
    /// The sub-font's directory moves, but its table offsets stay absolute from
    /// byte zero of the whole file — which is the whole trap, so the fixture
    /// has to rewrite them rather than leave them alone.
    fn as_collection(font: &[u8]) -> Vec<u8> {
        let header_len = 16usize;
        let mut out = Vec::new();
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version 1.0
        out.extend_from_slice(&1u32.to_be_bytes()); // numFonts
        out.extend_from_slice(&(header_len as u32).to_be_bytes()); // offset of font 0
        out.extend_from_slice(font);

        // Shift every table offset by the header, since they are measured from
        // the start of the file and the file just got longer at the front.
        let n = u16::from_be_bytes([font[4], font[5]]) as usize;
        for i in 0..n {
            let rec = header_len + 12 + i * 16;
            let old = u32::from_be_bytes(out[rec + 8..rec + 12].try_into().unwrap());
            out[rec + 8..rec + 12].copy_from_slice(&(old + header_len as u32).to_be_bytes());
        }
        out
    }

    #[test]
    fn a_collection_reads_the_same_font_as_the_bare_sfnt() {
        // The failure this pins was silent. Reslicing the buffer to the
        // sub-font's directory leaves every table offset short by the header,
        // and the bytes at the wrong place still decode: a real 19,398-glyph
        // MSMincho in the corpus reported 510 glyphs and nothing complained,
        // because 510 is a perfectly plausible number.
        let bare = simple_font();
        let collection = as_collection(&bare);

        let a = Sfnt::parse(&bare).expect("the bare font parses");
        let b = Sfnt::parse(&collection).expect("the collection parses");

        assert_eq!(b.directory, 16, "the directory is located, not sliced to");
        assert_eq!(b.num_glyphs, a.num_glyphs, "maxp came from the right place");
        assert_eq!(b.units_per_em, a.units_per_em, "head came from the right place");
        assert_eq!(b.number_of_h_metrics, a.number_of_h_metrics);
        assert_eq!(b.loca, a.loca);
        assert_eq!(
            b.glyph_data(&collection, 1),
            a.glyph_data(&bare, 1),
            "and the outlines are the same bytes"
        );
    }

    #[test]
    fn rebuilding_a_collection_produces_a_single_font() {
        // A rebuild emits one font, so it must not inherit the `ttcf` tag: that
        // announces a collection and then supplies a plain table directory,
        // which no reader can open -- this crate's parser included, which is
        // how the corpus surfaced it.
        let collection = as_collection(&simple_font());
        let font = Sfnt::parse(&collection).expect("parses");
        let out = crate::inject::rebuild(&collection, &font, &[]);

        assert_ne!(&out[..4], b"ttcf", "the output is not labelled a collection");
        let after = Sfnt::parse(&out).expect("the rebuilt font parses");
        assert_eq!(after.directory, 0);
        assert_eq!(after.num_glyphs, font.num_glyphs);
        assert_eq!(after.glyph_data(&out, 1), font.glyph_data(&collection, 1));
    }

    #[test]
    fn the_table_directory_parses() {
        let bytes = simple_font();
        let f = Sfnt::parse(&bytes).expect("parse");
        assert_eq!(f.num_glyphs, 3);
        assert_eq!(f.units_per_em, 2048);
        assert_eq!(f.index_to_loc_format, 0);
        assert_eq!(f.number_of_h_metrics, 3);
        assert!(f.has(b"glyf") && !f.is_cff());
    }

    #[test]
    fn short_loca_offsets_are_doubled() {
        let bytes = simple_font();
        let f = Sfnt::parse(&bytes).unwrap();
        // Stored as 0, 0, 2, 4; the short format halves, so these are the
        // doubled values.
        assert_eq!(f.loca, vec![0, 0, 4, 8]);
    }

    #[test]
    fn an_empty_glyph_is_not_an_error() {
        // loca[0] == loca[1] means a blank glyph such as `space`.
        let bytes = simple_font();
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.glyph_data(&bytes, 0).map(|g| g.len()), Some(0));
    }

    #[test]
    fn glyph_data_comes_from_loca_ranges() {
        let bytes = simple_font();
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.glyph_data(&bytes, 1), Some(&b"AABB"[..]));
        assert_eq!(f.glyph_data(&bytes, 9), None);
    }

    #[test]
    fn advances_come_from_hmtx() {
        let bytes = simple_font();
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.advance(&bytes, 0), Some(500));
        assert_eq!(f.advance(&bytes, 2), Some(700));
    }

    #[test]
    fn glyphs_past_the_metric_count_repeat_the_last_advance() {
        // hmtx stores full metrics only for numberOfHMetrics glyphs; the rest
        // share the last advance. Reading that as out-of-range would lose the
        // widths of most CJK fonts.
        let loca: Vec<u8> = [0u16; 6].iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut hmtx = Vec::new();
        hmtx.extend_from_slice(&1000u16.to_be_bytes());
        hmtx.extend_from_slice(&0u16.to_be_bytes());
        let bytes = build(
            &[0x00, 0x01, 0x00, 0x00],
            &[
                (b"head", head(1000, 0)),
                (b"maxp", maxp(5)),
                (b"hhea", hhea(1)),
                (b"loca", loca),
                (b"glyf", vec![0; 4]),
                (b"hmtx", hmtx),
            ],
        );
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.advance(&bytes, 0), Some(1000));
        assert_eq!(f.advance(&bytes, 4), Some(1000), "the tail repeats the last advance");
    }

    #[test]
    fn long_loca_is_read_unscaled() {
        let loca: Vec<u8> = [0u32, 8, 16].iter().flat_map(|v| v.to_be_bytes()).collect();
        let bytes = build(
            &[0x00, 0x01, 0x00, 0x00],
            &[
                (b"head", head(1000, 1)),
                (b"maxp", maxp(2)),
                (b"loca", loca),
                (b"glyf", vec![0; 16]),
            ],
        );
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.loca, vec![0, 8, 16]);
    }

    #[test]
    fn the_short_loca_limit_is_where_the_spec_says() {
        // Spec 8.4 names this as "a common silent corruption": short loca
        // halves offsets into 16 bits, so it tops out just under 128 KB.
        assert!(!loca_needs_long_format(0x1FFFE));
        assert!(loca_needs_long_format(0x1FFFF));
        assert!(!loca_needs_long_format(0));
    }

    #[test]
    fn an_otto_font_reports_cff_outlines() {
        let bytes = build(b"OTTO", &[(b"head", head(1000, 0)), (b"CFF ", vec![1, 0, 4, 1])]);
        let f = Sfnt::parse(&bytes).unwrap();
        assert!(f.is_cff() && !f.has(b"glyf"));
    }

    #[test]
    fn a_font_without_head_gets_usable_defaults() {
        // 1000 rather than zero: zero unitsPerEm would make every metric
        // infinite, and a missing head is recoverable.
        let bytes = build(&[0x00, 0x01, 0x00, 0x00], &[(b"maxp", maxp(1))]);
        let f = Sfnt::parse(&bytes).unwrap();
        assert_eq!(f.units_per_em, 1000);
    }

    #[test]
    fn a_table_overrunning_the_file_is_clamped() {
        let mut bytes = simple_font();
        // Point glyf's length past the end.
        let rec = 12 + 4 * 16;
        bytes[rec + 12..rec + 16].copy_from_slice(&0xFFFFFFFFu32.to_be_bytes());
        let f = Sfnt::parse(&bytes).expect("still parses");
        let (offset, length) = f.table(b"glyf").unwrap();
        assert!(offset + length <= bytes.len());
    }

    #[test]
    fn an_impossible_table_count_is_rejected() {
        let mut bytes = simple_font();
        bytes[4..6].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert!(Sfnt::parse(&bytes).is_err());
    }

    #[test]
    fn a_truncated_header_is_rejected() {
        assert!(Sfnt::parse(&[]).is_err());
        assert!(Sfnt::parse(&[0, 1, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn post_names_read_both_standard_and_custom_entries() {
        let mut post = vec![0u8; 32];
        post[0..4].copy_from_slice(&0x0002_0000u32.to_be_bytes());
        post.extend_from_slice(&3u16.to_be_bytes()); // numGlyphs
        for idx in [0u16, 36, 258] {
            post.extend_from_slice(&idx.to_be_bytes());
        }
        post.push(6);
        post.extend_from_slice(b"custom");

        let bytes = build(&[0x00, 0x01, 0x00, 0x00], &[(b"maxp", maxp(3)), (b"post", post)]);
        let f = Sfnt::parse(&bytes).unwrap();
        let names = f.post_names(&bytes);
        assert_eq!(names[0], ".notdef", "index 0 is the standard Macintosh name");
        assert_eq!(names[1], "A", "index 36 likewise");
        assert_eq!(names[2], "custom", "258 and up index the Pascal strings");
    }

    #[test]
    fn a_version_three_post_declines_rather_than_fails() {
        // Most modern fonts ship 3.0: names cost space and only text extraction
        // wants them. An empty result means "declines to say".
        let mut post = vec![0u8; 32];
        post[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
        let bytes = build(&[0x00, 0x01, 0x00, 0x00], &[(b"maxp", maxp(3)), (b"post", post)]);
        assert!(Sfnt::parse(&bytes).unwrap().post_names(&bytes).is_empty());
    }

    #[test]
    fn a_truncated_post_does_not_panic() {
        let mut post = vec![0u8; 32];
        post[0..4].copy_from_slice(&0x0002_0000u32.to_be_bytes());
        post.extend_from_slice(&500u16.to_be_bytes()); // claims 500 glyphs
        let bytes = build(&[0x00, 0x01, 0x00, 0x00], &[(b"maxp", maxp(3)), (b"post", post)]);
        let _ = Sfnt::parse(&bytes).unwrap().post_names(&bytes);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0xDEADBEEFu32;
        for _ in 0..2000 {
            let mut bytes = vec![0x00, 0x01, 0x00, 0x00];
            for _ in 0..80 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push((seed >> 24) as u8);
            }
            if let Ok(f) = Sfnt::parse(&bytes) {
                for gid in 0..f.num_glyphs.min(64) {
                    let _ = f.glyph_data(&bytes, gid);
                    let _ = f.advance(&bytes, gid);
                }
            }
        }
    }
}
