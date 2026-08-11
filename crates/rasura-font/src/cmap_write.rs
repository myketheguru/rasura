//! Extending a font's `cmap` so an injected glyph is reachable. Spec 8.4.
//!
//! Spec 8.4's PDF-level list — `/Widths`, `/FirstChar`, `/LastChar`,
//! `/Differences`, `/ToUnicode` — is complete for a **Type 1** font, where a
//! `/Differences` name addresses a charstring by name and nothing else is
//! needed.
//!
//! It is not complete for TrueType. ISO 32000-1 §9.6.6.4 resolves a simple
//! TrueType font's code to a glyph *through the font's own tables*: the name
//! becomes a character through the Adobe Glyph List, and the character becomes
//! a glyph id through the `cmap`. A name the `cmap` cannot reach draws nothing.
//!
//! That gap survived every structural check. `qpdf --check` passed, pdf.js
//! opened the file and extracted the right text — because extraction reads
//! `/ToUnicode`, which was correct — and only rendering the page showed the
//! glyph was never drawn. It is the clearest argument in this project for
//! §14.3's pixel diff being a different kind of test rather than a slower one.

use crate::cmap::Cmap;
use crate::error::Result;
use crate::inject::rebuild;
use crate::sfnt::Sfnt;
use std::collections::BTreeMap;

/// Add character-to-glyph mappings to a font's `cmap`, rebuilding it.
///
/// The result carries a single format 4 `(3, 1)` subtable holding the font's
/// existing Unicode mappings plus the new ones. One subtable rather than
/// several: a font that reached its glyphs through a `(1, 0)` Macintosh table
/// keeps working, because the Windows table is the one every reader consults
/// first and the two are not required to agree.
///
/// Codes outside the Basic Multilingual Plane are dropped. Format 4 cannot
/// express them, and a font needing them wants a format 12 table, which is a
/// different write.
pub fn add_mappings(data: &[u8], font: &Sfnt, added: &[(u32, u16)]) -> Result<Vec<u8>> {
    let mut pairs: BTreeMap<u16, u16> = BTreeMap::new();

    // Whatever the font already maps, so no character stops working.
    if let Some(cmap) = Cmap::parse(data, font)
        && let Some(unicode) = cmap.best_unicode()
    {
        for (code, gid) in unicode.mappings(data) {
            if let Ok(code) = u16::try_from(code) {
                pairs.insert(code, gid);
            }
        }
    }
    for (code, gid) in added {
        if let Ok(code) = u16::try_from(*code) {
            pairs.insert(code, *gid);
        }
    }

    let subtable = format4(&pairs);
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes()); // version
    cmap.extend_from_slice(&1u16.to_be_bytes()); // one subtable
    cmap.extend_from_slice(&3u16.to_be_bytes()); // Windows
    cmap.extend_from_slice(&1u16.to_be_bytes()); // BMP
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&subtable);

    Ok(rebuild(data, font, &[(*b"cmap", cmap)]))
}

/// A format 4 subtable for the given code-to-glyph pairs.
///
/// Segments are runs of consecutive codes whose glyph ids advance in step, so
/// each can use `idDelta` and needs no glyph array. That is more segments than
/// an optimal packing would produce and no reader minds; the alternative is
/// `idRangeOffset` arithmetic, which is the part of this format everyone gets
/// wrong at least once.
fn format4(pairs: &BTreeMap<u16, u16>) -> Vec<u8> {
    let mut segments: Vec<(u16, u16, u16)> = Vec::new(); // start, end, delta
    for (&code, &gid) in pairs {
        let delta = gid.wrapping_sub(code);
        match segments.last_mut() {
            // Extend the run when the code is the next one and the delta holds.
            Some((_, end, d)) if *end + 1 == code && *d == delta => *end = code,
            _ => segments.push((code, code, delta)),
        }
    }
    // Every format 4 table ends with a segment mapping 0xFFFF, which readers
    // rely on as a terminator.
    segments.push((0xFFFF, 0xFFFF, 1));

    let seg_count = segments.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // length, patched below
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&(seg_count * 2).to_be_bytes());

    // searchRange and friends are a binary-search hint. Readers that use it
    // will land on the wrong record if it disagrees with segCount.
    let entry_selector = (u16::BITS - 1 - seg_count.leading_zeros()) as u16;
    let search_range = (1u16 << entry_selector) * 2;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((seg_count * 2).wrapping_sub(search_range)).to_be_bytes());

    for (_, end, _) in &segments {
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for (start, _, _) in &segments {
        out.extend_from_slice(&start.to_be_bytes());
    }
    for (_, _, delta) in &segments {
        out.extend_from_slice(&delta.to_be_bytes());
    }
    for _ in &segments {
        out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
    }

    let len = out.len() as u16;
    out[2..4].copy_from_slice(&len.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::truetype;

    fn mappings_of(bytes: &[u8]) -> BTreeMap<u32, u16> {
        let font = Sfnt::parse(bytes).expect("sfnt");
        let cmap = Cmap::parse(bytes, &font).expect("cmap");
        cmap.best_unicode().expect("unicode subtable").mappings(bytes).into_iter().collect()
    }

    #[test]
    fn a_new_mapping_becomes_reachable() {
        let font = truetype(3, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x43, 3)]).expect("rebuild");

        let m = mappings_of(&out);
        assert_eq!(m.get(&0x43), Some(&3), "C now reaches glyph 3");
    }

    #[test]
    fn the_existing_mappings_survive() {
        // A font whose other characters stopped working would be a worse
        // outcome than one whose new glyph never drew.
        let font = truetype(3, 1000);
        let before = mappings_of(&font);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x43, 3)]).unwrap();
        let after = mappings_of(&out);

        for (code, gid) in &before {
            assert_eq!(after.get(code), Some(gid), "code {code:#x} changed");
        }
    }

    #[test]
    fn consecutive_mappings_share_one_segment() {
        // A run whose glyph ids advance in step needs one segment, not one
        // each, and the reader has to get the same answer either way.
        let font = truetype(2, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x42, 2), (0x43, 3), (0x44, 4)]).unwrap();
        let m = mappings_of(&out);
        assert_eq!(m.get(&0x42), Some(&2));
        assert_eq!(m.get(&0x43), Some(&3));
        assert_eq!(m.get(&0x44), Some(&4));
    }

    #[test]
    fn a_scattered_set_still_resolves() {
        let font = truetype(2, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x100, 9), (0x2000, 3), (0x50, 7)]).unwrap();
        let m = mappings_of(&out);
        assert_eq!(m.get(&0x100), Some(&9));
        assert_eq!(m.get(&0x2000), Some(&3));
        assert_eq!(m.get(&0x50), Some(&7));
    }

    #[test]
    fn the_terminator_is_not_a_mapping() {
        let font = truetype(2, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x43, 2)]).unwrap();
        assert!(!mappings_of(&out).contains_key(&0xFFFF));
    }

    #[test]
    fn characters_past_the_bmp_are_dropped_not_mangled() {
        // Format 4 cannot express them. Truncating to 16 bits would map some
        // unrelated BMP character to the glyph instead.
        let font = truetype(2, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x1F600, 5)]).unwrap();
        let m = mappings_of(&out);
        assert_eq!(m.get(&0x1F600), None);
        assert_eq!(m.get(&0xF600), None, "not truncated into the BMP either");
    }

    #[test]
    fn the_rest_of_the_font_is_untouched() {
        let font = truetype(4, 1000);
        let parsed = Sfnt::parse(&font).unwrap();
        let out = add_mappings(&font, &parsed, &[(0x50, 3)]).unwrap();
        let after = Sfnt::parse(&out).unwrap();

        for tag in [b"glyf", b"loca", b"hmtx", b"head", b"maxp"] {
            assert_eq!(
                after.table_data(&out, tag),
                parsed.table_data(&font, tag),
                "{} changed",
                String::from_utf8_lossy(tag)
            );
        }
    }
}
