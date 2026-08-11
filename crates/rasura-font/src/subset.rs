//! Subsetting on save. Spec 8.6.
//!
//! > Default: **sparse-preserving.** Keep the original GID numbering, add
//! > injected glyphs at the end, do not renumber. Renumbering would require
//! > rewriting every content stream that references the font — exactly the
//! > non-local change §2 forbids.
//! >
//! > `SubsetPolicy::Compact` (opt-in, `FullRewrite` only) renumbers and prunes
//! > unused glyphs for size. Offer it; never default to it.
//!
//! The sparse-preserving default needs no code: it is what [`crate::inject`]
//! already does by appending. This module is the opt-in half.
//!
//! Compaction returns the **old-to-new glyph mapping**, and it is not a
//! courtesy. Every `Tj` and `TJ` in every content stream that uses the font
//! refers to glyphs by the numbers this pass just changed; a caller that
//! renumbers the font and forgets the streams has produced a document that
//! renders as gibberish. Handing back the mapping makes that step impossible to
//! overlook.

use crate::error::{FontError, Result};
use crate::inject::{components, encode_loca, left_side_bearing, rebuild, renumber_components};
use crate::sfnt::{Sfnt, loca_needs_long_format};
use std::collections::{BTreeSet, HashMap};

/// How much of a font to keep on save. Spec 8.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubsetPolicy {
    /// Keep every glyph and every glyph id. The default, and the only policy
    /// that is safe for an incremental save.
    #[default]
    SparsePreserving,
    /// Renumber and prune. Smaller, and requires rewriting every content stream
    /// that references the font -- so it is `FullRewrite` only.
    Compact,
}

/// The result of compacting a font.
#[derive(Debug, Clone)]
pub struct Subset {
    pub bytes: Vec<u8>,
    /// Old glyph id to new. Every content stream referencing this font must be
    /// rewritten through it.
    pub mapping: HashMap<u16, u16>,
    pub glyphs_before: u16,
    pub glyphs_after: u16,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

impl Subset {
    /// Proportion of the original size retained.
    pub fn size_ratio(&self) -> f64 {
        if self.bytes_before == 0 {
            return 1.0;
        }
        self.bytes_after as f64 / self.bytes_before as f64
    }
}

/// Prune a TrueType font to the glyphs actually used, renumbering compactly.
///
/// `.notdef` is always kept, whether or not it was asked for: a font without
/// glyph 0 is malformed, and every reader falls back to it.
pub fn compact_truetype(data: &[u8], used: &[u16]) -> Result<Subset> {
    let font = Sfnt::parse(data)?;
    if !font.has(b"glyf") || font.loca.is_empty() {
        return Err(FontError::MissingTable("glyf/loca"));
    }

    // Transitive closure over composite components: a kept glyph needs the
    // glyphs it is built from, or it draws nothing.
    let mut keep: BTreeSet<u16> = BTreeSet::new();
    keep.insert(0);
    for &gid in used {
        collect(&font, data, gid, 0, &mut keep)?;
    }

    // Ascending original order, so the mapping is deterministic and the new
    // font's glyphs sit in the same relative order as the old one's.
    let mapping: HashMap<u16, u16> =
        keep.iter().enumerate().map(|(i, &old)| (old, i as u16)).collect();
    let new_count =
        u16::try_from(keep.len()).map_err(|_| FontError::Malformed("more than 65535 glyphs"))?;

    let mut glyf = Vec::new();
    let mut offsets = vec![0u32];
    for &old in &keep {
        let mut body = font.glyph_data(data, old).unwrap_or_default().to_vec();
        renumber_components(&mut body, &mapping)?;
        glyf.extend_from_slice(&body);
        while glyf.len() % 2 != 0 {
            glyf.push(0);
        }
        offsets.push(glyf.len() as u32);
    }

    let long = loca_needs_long_format(glyf.len()) || font.index_to_loc_format != 0;
    let loca = encode_loca(&offsets, long);

    let mut hmtx = Vec::with_capacity(keep.len() * 4);
    for &old in &keep {
        hmtx.extend_from_slice(&font.advance(data, old).unwrap_or(0).to_be_bytes());
        hmtx.extend_from_slice(&left_side_bearing(&font, data, old).to_be_bytes());
    }

    let mut hhea = font.table_data(data, b"hhea").unwrap_or_default().to_vec();
    if hhea.len() >= 36 {
        hhea[34..36].copy_from_slice(&new_count.to_be_bytes());
    }
    let mut maxp = font.table_data(data, b"maxp").unwrap_or_default().to_vec();
    if maxp.len() >= 6 {
        maxp[4..6].copy_from_slice(&new_count.to_be_bytes());
    }
    let mut head = font.table_data(data, b"head").unwrap_or_default().to_vec();
    if head.len() >= 52 {
        head[50..52].copy_from_slice(&(if long { 1i16 } else { 0i16 }).to_be_bytes());
    }

    // `cmap` and `post` are dropped rather than renumbered.
    //
    // Both map to glyph ids this pass has just invalidated, and both are dead
    // weight in an embedded subset: a PDF reaches glyphs through `/Encoding`
    // and `/Widths`, never through the font's own character map. Renumbering
    // them would be work in service of tables nothing reads; leaving them
    // stale would be worse than dropping them, because a reader that *did*
    // consult one would get the wrong glyph rather than none.
    let mut replacements: Vec<([u8; 4], Vec<u8>)> =
        vec![(*b"glyf", glyf), (*b"loca", loca), (*b"hmtx", hmtx)];
    for (tag, body) in [(*b"hhea", hhea), (*b"maxp", maxp), (*b"head", head)] {
        if !body.is_empty() {
            replacements.push((tag, body));
        }
    }

    let dropped: &[&[u8; 4]] = &[b"cmap", b"post", b"GSUB", b"GPOS", b"kern"];
    let kept_tables: Vec<([u8; 4], usize, usize)> =
        font.tables.iter().filter(|(t, _, _)| !dropped.contains(&t)).copied().collect();
    let pruned = Sfnt { tables: kept_tables, ..font.clone() };

    let bytes = rebuild(data, &pruned, &replacements);
    Ok(Subset {
        glyphs_before: font.num_glyphs,
        glyphs_after: new_count,
        bytes_before: data.len(),
        bytes_after: bytes.len(),
        bytes,
        mapping,
    })
}

/// Every glyph a glyph needs, itself included.
fn collect(
    font: &Sfnt,
    data: &[u8],
    gid: u16,
    depth: usize,
    out: &mut BTreeSet<u16>,
) -> Result<()> {
    if depth > 8 {
        return Err(FontError::Malformed("composite glyph nests too deeply"));
    }
    if gid >= font.num_glyphs {
        return Err(FontError::Malformed("glyph id past the end of the font"));
    }
    if !out.insert(gid) {
        return Ok(());
    }
    let Some(glyph) = font.glyph_data(data, gid) else { return Ok(()) };
    for (_, component) in components(glyph) {
        collect(font, data, component, depth + 1, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::truetype;

    #[test]
    fn the_default_policy_is_sparse_preserving() {
        // Spec 8.6: "Offer it; never default to it." A caller that does not
        // choose gets the policy that cannot break a document.
        assert_eq!(SubsetPolicy::default(), SubsetPolicy::SparsePreserving);
    }

    #[test]
    fn unused_glyphs_are_pruned_and_the_rest_renumbered() {
        let font = truetype(8, 1000);
        let out = compact_truetype(&font, &[3, 5]).expect("compact");

        assert_eq!(out.glyphs_before, 8);
        assert_eq!(out.glyphs_after, 3, ".notdef plus the two asked for");
        // Ascending original order, so glyph 3 becomes 1 and glyph 5 becomes 2.
        assert_eq!(out.mapping.get(&0), Some(&0));
        assert_eq!(out.mapping.get(&3), Some(&1));
        assert_eq!(out.mapping.get(&5), Some(&2));
        assert_eq!(out.mapping.get(&4), None, "not kept");
    }

    #[test]
    fn the_kept_outlines_are_the_originals() {
        let font = truetype(8, 1000);
        let before = Sfnt::parse(&font).unwrap();
        let out = compact_truetype(&font, &[3, 5]).unwrap();
        let after = Sfnt::parse(&out.bytes).expect("the subset parses");

        for (&old, &new) in &out.mapping {
            assert_eq!(
                after.glyph_data(&out.bytes, new),
                before.glyph_data(&font, old),
                "glyph {old} -> {new} changed"
            );
        }
    }

    #[test]
    fn metrics_follow_the_renumbering() {
        let font = truetype(8, 1000);
        let before = Sfnt::parse(&font).unwrap();
        let out = compact_truetype(&font, &[6]).unwrap();
        let after = Sfnt::parse(&out.bytes).unwrap();

        assert_eq!(after.number_of_h_metrics, out.glyphs_after);
        let new = out.mapping[&6];
        assert_eq!(after.advance(&out.bytes, new), before.advance(&font, 6));
    }

    #[test]
    fn notdef_is_kept_even_when_not_asked_for() {
        // A font without glyph 0 is malformed, and every reader falls back to it.
        let font = truetype(5, 1000);
        let out = compact_truetype(&font, &[2]).unwrap();
        assert_eq!(out.mapping.get(&0), Some(&0));
        assert_eq!(out.glyphs_after, 2);
    }

    #[test]
    fn asking_for_nothing_still_yields_a_usable_font() {
        let font = truetype(5, 1000);
        let out = compact_truetype(&font, &[]).unwrap();
        assert_eq!(out.glyphs_after, 1, "just .notdef");
        assert!(Sfnt::parse(&out.bytes).is_ok());
    }

    #[test]
    fn compaction_makes_the_font_smaller() {
        let font = truetype(40, 1000);
        let out = compact_truetype(&font, &[1, 2]).unwrap();
        assert!(out.bytes_after < out.bytes_before, "{} vs {}", out.bytes_after, out.bytes_before);
        assert!(out.size_ratio() < 1.0);
    }

    #[test]
    fn tables_that_index_glyphs_are_dropped_rather_than_left_stale() {
        // `cmap` maps characters to the glyph ids this pass just invalidated.
        // Leaving it would give a reader that consults it the *wrong* glyph,
        // which is worse than giving it none.
        let font = truetype(6, 1000);
        assert!(Sfnt::parse(&font).unwrap().has(b"cmap"), "the fixture has one to drop");

        let out = compact_truetype(&font, &[1]).unwrap();
        let after = Sfnt::parse(&out.bytes).unwrap();
        assert!(!after.has(b"cmap"));
        // The tables a PDF actually needs survive.
        assert!(after.has(b"glyf") && after.has(b"loca") && after.has(b"hmtx"));
        assert!(after.has(b"head") && after.has(b"maxp") && after.has(b"hhea"));
    }

    #[test]
    fn a_glyph_past_the_end_is_refused() {
        let font = truetype(4, 1000);
        assert!(compact_truetype(&font, &[99]).is_err());
    }

    #[test]
    fn a_font_with_no_outlines_is_refused() {
        assert!(compact_truetype(&[0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0], &[]).is_err());
    }
}
