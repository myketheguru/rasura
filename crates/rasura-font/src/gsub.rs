//! `GSUB` ligature coverage, for feature inference. Spec 8.3.
//!
//! > When reshaping, derive features from the original where inferable: if the
//! > original sequence contains a GID that the font's `GSUB` maps only under
//! > `liga`, enable `liga`.
//!
//! This is the half of §8.3's feature inference that kerning already had. The
//! question it answers is narrow and precise: **is this glyph one the font can
//! only produce by applying a ligature feature?** If the original run contains
//! `ﬁ`, the producer had ligatures on, and reshaping the edited word without
//! them would silently un-ligate text the user did not touch. If it does not,
//! turning `liga` on would ligate text that was deliberately left plain.
//!
//! Only the ligature substitution shape is read — `GSUB` lookup type 4, plus
//! type 7's extension wrapper around it. The other lookup types substitute one
//! glyph for one glyph or handle context, and neither tells you a run *was*
//! ligated; a single-substitution output is indistinguishable from a glyph the
//! producer chose directly.

use crate::sfnt::Sfnt;
use std::collections::{HashMap, HashSet};

/// Beyond this many lookups or coverage entries the font is a stress test, not
/// a document's font, and the walk stops rather than growing without bound.
const MAX_ENTRIES: usize = 100_000;

/// Which glyphs a font can only produce through a ligature feature.
#[derive(Debug, Clone, Default)]
pub struct LigatureCoverage {
    /// Glyph id to the feature tags that produce it, e.g. `liga`, `dlig`.
    pub produced_by: HashMap<u16, HashSet<[u8; 4]>>,
}

impl LigatureCoverage {
    pub fn is_empty(&self) -> bool {
        self.produced_by.is_empty()
    }

    /// Whether a glyph is a ligature the font produces under some feature.
    pub fn is_ligature(&self, gid: u16) -> bool {
        self.produced_by.contains_key(&gid)
    }

    /// The features that would have to be on for a run to contain these glyphs.
    ///
    /// Spec 8.3's rule, applied: a glyph reachable *only* under `dlig` implies
    /// discretionary ligatures were on, which is a stronger claim than `liga`
    /// and is worth carrying separately -- turning `dlig` on for a run that
    /// merely used `fi` would introduce ligatures the producer never asked for.
    pub fn features_for(&self, glyphs: &[u16]) -> Vec<[u8; 4]> {
        let mut out: Vec<[u8; 4]> = Vec::new();
        for gid in glyphs {
            let Some(tags) = self.produced_by.get(gid) else { continue };
            // A glyph produced under several features implies only the most
            // common one; `liga` is preferred when it is among them, because
            // claiming `dlig` from a glyph `liga` also produces would enable a
            // feature on no evidence.
            let chosen =
                if tags.contains(b"liga") { *b"liga" } else { *tags.iter().min().unwrap() };
            if !out.contains(&chosen) {
                out.push(chosen);
            }
        }
        out.sort_unstable();
        out
    }
}

/// Read the ligature substitutions a font's `GSUB` can perform.
///
/// Returns an empty coverage for a font with no `GSUB`, which is not a failure:
/// most subset fonts in PDFs have none, and a font that cannot ligate tells you
/// the run was not ligated.
pub fn ligature_coverage(data: &[u8], font: &Sfnt) -> LigatureCoverage {
    let mut out = LigatureCoverage::default();
    let Some(gsub) = font.table_data(data, b"GSUB") else { return out };
    if gsub.len() < 10 {
        return out;
    }

    let scripts_at = be16(gsub, 4) as usize;
    let features_at = be16(gsub, 6) as usize;
    let lookups_at = be16(gsub, 8) as usize;
    let _ = scripts_at;

    // FeatureList: each record names a tag and a feature table listing the
    // lookups it uses. Walking from features to lookups -- rather than reading
    // the lookups alone -- is what lets a glyph be attributed to `liga` or
    // `dlig` rather than merely to "some substitution".
    let Some(features) = gsub.get(features_at..) else { return out };
    let count = be16(features, 0) as usize;

    for i in 0..count.min(MAX_ENTRIES) {
        let rec = 2 + i * 6;
        let Some(r) = features.get(rec..rec + 6) else { break };
        let tag: [u8; 4] = [r[0], r[1], r[2], r[3]];
        // Only the ligature features. `rlig` is required ligatures, which a
        // shaper applies whether or not asked -- included because a glyph it
        // produces still identifies the run as ligated.
        if !matches!(&tag, b"liga" | b"dlig" | b"clig" | b"hlig" | b"rlig") {
            continue;
        }
        let table_at = features_at + be16(r, 4) as usize;
        let Some(table) = gsub.get(table_at..) else { continue };
        let n = be16(table, 2) as usize;

        for k in 0..n.min(MAX_ENTRIES) {
            let Some(index) = table.get(4 + k * 2..6 + k * 2).map(|b| be16(b, 0)) else { break };
            for gid in lookup_outputs(gsub, lookups_at, index as usize, 0) {
                out.produced_by.entry(gid).or_default().insert(tag);
            }
        }
    }
    out
}

/// Glyph ids a lookup can produce, if it is a ligature substitution.
fn lookup_outputs(gsub: &[u8], lookups_at: usize, index: usize, depth: usize) -> Vec<u16> {
    let mut out = Vec::new();
    if depth > 4 {
        return out;
    }
    let Some(list) = gsub.get(lookups_at..) else { return out };
    if index >= be16(list, 0) as usize {
        return out;
    }
    let Some(offset) = list.get(2 + index * 2..4 + index * 2).map(|b| be16(b, 0)) else {
        return out;
    };
    let table_at = lookups_at + offset as usize;
    let Some(table) = gsub.get(table_at..) else { return out };

    let kind = be16(table, 0);
    let subtables = be16(table, 4) as usize;

    for s in 0..subtables.min(MAX_ENTRIES) {
        let Some(rel) = table.get(6 + s * 2..8 + s * 2).map(|b| be16(b, 0)) else { break };
        let sub_at = table_at + rel as usize;
        match kind {
            // 4: ligature substitution.
            4 => collect_ligatures(gsub, sub_at, &mut out),
            // 7: an extension wrapper, present when a table would otherwise
            // exceed a 16-bit offset. Skipping it loses the ligatures of every
            // large font, which is most of the ones that have interesting ones.
            7 => {
                let Some(ext) = gsub.get(sub_at..sub_at + 8) else { continue };
                let real_kind = be16(ext, 2);
                let jump = u32::from_be_bytes([ext[4], ext[5], ext[6], ext[7]]) as usize;
                if real_kind == 4 {
                    collect_ligatures(gsub, sub_at + jump, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// The output glyphs of a LigatureSubst subtable.
fn collect_ligatures(gsub: &[u8], at: usize, out: &mut Vec<u16>) {
    let Some(sub) = gsub.get(at..) else { return };
    if be16(sub, 0) != 1 {
        return; // only format 1 is defined
    }
    let set_count = be16(sub, 4) as usize;

    for i in 0..set_count.min(MAX_ENTRIES) {
        let Some(set_rel) = sub.get(6 + i * 2..8 + i * 2).map(|b| be16(b, 0)) else { break };
        let set_at = at + set_rel as usize;
        let Some(set) = gsub.get(set_at..) else { continue };
        let ligatures = be16(set, 0) as usize;

        for k in 0..ligatures.min(MAX_ENTRIES) {
            let Some(rel) = set.get(2 + k * 2..4 + k * 2).map(|b| be16(b, 0)) else { break };
            // The ligature table's first field is the glyph it produces.
            if let Some(lig) = gsub.get(set_at + rel as usize..set_at + rel as usize + 2) {
                out.push(be16(lig, 0));
            }
            if out.len() >= MAX_ENTRIES {
                return;
            }
        }
    }
}

fn be16(data: &[u8], at: usize) -> u16 {
    data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `GSUB` with one ligature feature producing the given glyph ids.
    ///
    /// Laid out by hand because every offset in the table is relative to a
    /// different base, and a builder that got one of those wrong would make the
    /// test agree with the bug.
    fn gsub_with(tag: &[u8; 4], produced: &[u16], extension: bool) -> Vec<u8> {
        // LigatureSubst (format 1): one LigatureSet holding one Ligature per
        // output glyph.
        let mut lig_tables = Vec::new();
        let mut lig_offsets = Vec::new();
        for &gid in produced {
            lig_offsets.push(lig_tables.len());
            lig_tables.extend_from_slice(&gid.to_be_bytes()); // ligatureGlyph
            lig_tables.extend_from_slice(&2u16.to_be_bytes()); // componentCount
            lig_tables.extend_from_slice(&1u16.to_be_bytes()); // one component
        }
        let set_header = 2 + produced.len() * 2;
        let mut lig_set = Vec::new();
        lig_set.extend_from_slice(&(produced.len() as u16).to_be_bytes());
        for off in &lig_offsets {
            lig_set.extend_from_slice(&((set_header + off) as u16).to_be_bytes());
        }
        lig_set.extend_from_slice(&lig_tables);

        let subst_header = 6 + 2; // format, coverage, setCount, one set offset
        let mut subst = Vec::new();
        subst.extend_from_slice(&1u16.to_be_bytes()); // format
        subst.extend_from_slice(&0u16.to_be_bytes()); // coverage offset (unread)
        subst.extend_from_slice(&1u16.to_be_bytes()); // ligatureSetCount
        subst.extend_from_slice(&(subst_header as u16).to_be_bytes());
        subst.extend_from_slice(&lig_set);

        // Lookup: type 4 directly, or type 7 wrapping it.
        let mut lookup = Vec::new();
        let payload_at;
        if extension {
            lookup.extend_from_slice(&7u16.to_be_bytes()); // lookupType
            lookup.extend_from_slice(&0u16.to_be_bytes()); // lookupFlag
            lookup.extend_from_slice(&1u16.to_be_bytes()); // subTableCount
            lookup.extend_from_slice(&8u16.to_be_bytes()); // subtable offset
            // ExtensionSubst: format, extensionLookupType, 32-bit offset.
            lookup.extend_from_slice(&1u16.to_be_bytes());
            lookup.extend_from_slice(&4u16.to_be_bytes());
            lookup.extend_from_slice(&8u32.to_be_bytes());
            payload_at = lookup.len();
            lookup.extend_from_slice(&subst);
        } else {
            // lookupType, lookupFlag, subTableCount and one offset are exactly
            // eight bytes, so the subtable begins at 8 with nothing between.
            lookup.extend_from_slice(&4u16.to_be_bytes());
            lookup.extend_from_slice(&0u16.to_be_bytes());
            lookup.extend_from_slice(&1u16.to_be_bytes());
            lookup.extend_from_slice(&8u16.to_be_bytes());
            payload_at = lookup.len();
            assert_eq!(payload_at, 8, "the subtable must land where the offset says");
            lookup.extend_from_slice(&subst);
        }
        let _ = payload_at;

        let mut lookup_list = Vec::new();
        lookup_list.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        lookup_list.extend_from_slice(&4u16.to_be_bytes()); // offset to the lookup
        lookup_list.extend_from_slice(&lookup);

        // FeatureList: one record, whose table lists lookup 0.
        let mut feature = Vec::new();
        feature.extend_from_slice(&0u16.to_be_bytes()); // featureParams
        feature.extend_from_slice(&1u16.to_be_bytes()); // lookupIndexCount
        feature.extend_from_slice(&0u16.to_be_bytes()); // lookup 0
        let mut feature_list = Vec::new();
        feature_list.extend_from_slice(&1u16.to_be_bytes()); // featureCount
        feature_list.extend_from_slice(tag);
        feature_list.extend_from_slice(&8u16.to_be_bytes()); // offset to the table
        feature_list.extend_from_slice(&feature);

        // The header's offsets are from the table's own start.
        let header = 10usize;
        let script_list = vec![0u8, 0]; // scriptCount 0
        let features_at = header + script_list.len();
        let lookups_at = features_at + feature_list.len();

        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
        out.extend_from_slice(&(header as u16).to_be_bytes());
        out.extend_from_slice(&(features_at as u16).to_be_bytes());
        out.extend_from_slice(&(lookups_at as u16).to_be_bytes());
        out.extend_from_slice(&script_list);
        out.extend_from_slice(&feature_list);
        out.extend_from_slice(&lookup_list);
        out
    }

    /// Wrap a `GSUB` in a minimal sfnt.
    fn font_with_gsub(gsub: Vec<u8>) -> Vec<u8> {
        let mut maxp = vec![0u8; 32];
        maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&100u16.to_be_bytes());

        let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"GSUB", gsub), (b"maxp", maxp)];
        let n = tables.len();
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + n * 16;
        for (tag, body) in &tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            offset += body.len().next_multiple_of(4);
        }
        for (_, body) in &tables {
            out.extend_from_slice(body);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    fn coverage_of(tag: &[u8; 4], produced: &[u16], extension: bool) -> LigatureCoverage {
        let bytes = font_with_gsub(gsub_with(tag, produced, extension));
        let font = Sfnt::parse(&bytes).expect("sfnt");
        ligature_coverage(&bytes, &font)
    }

    #[test]
    fn a_liga_lookups_output_glyphs_are_found() {
        let cov = coverage_of(b"liga", &[300, 301], false);
        assert!(cov.is_ligature(300));
        assert!(cov.is_ligature(301));
        assert!(!cov.is_ligature(42));
    }

    #[test]
    fn the_feature_tag_travels_with_the_glyph() {
        // Which feature matters: enabling `dlig` on the evidence of an `fi`
        // would introduce ligatures the producer never asked for.
        let cov = coverage_of(b"dlig", &[300], false);
        assert_eq!(cov.features_for(&[300]), vec![*b"dlig"]);
        assert!(cov.features_for(&[42]).is_empty(), "a plain glyph implies nothing");
    }

    #[test]
    fn an_extension_lookup_is_followed() {
        // Type 7 wraps a real lookup when the table would exceed a 16-bit
        // offset -- which is most fonts large enough to have interesting
        // ligatures. Skipping it loses all of them.
        let cov = coverage_of(b"liga", &[500], true);
        assert!(cov.is_ligature(500), "the extension was not followed");
    }

    #[test]
    fn liga_is_preferred_when_a_glyph_has_several_features() {
        // Claiming `dlig` from a glyph `liga` also produces would enable a
        // feature on no evidence.
        let mut cov = LigatureCoverage::default();
        cov.produced_by.entry(7).or_default().insert(*b"dlig");
        cov.produced_by.entry(7).or_default().insert(*b"liga");
        assert_eq!(cov.features_for(&[7]), vec![*b"liga"]);
    }

    #[test]
    fn a_run_with_no_ligatures_asks_for_no_features() {
        // The other half of spec 8.3's rule. Turning `liga` on here would
        // ligate text that was deliberately left plain.
        let cov = coverage_of(b"liga", &[300], false);
        assert!(cov.features_for(&[65, 66, 67]).is_empty());
    }

    #[test]
    fn a_font_without_gsub_reports_no_ligatures() {
        // Not a failure: most subset fonts in PDFs have none, and a font that
        // cannot ligate tells you the run was not ligated.
        let bytes = font_with_gsub(Vec::new());
        let font = Sfnt::parse(&bytes).unwrap();
        let cov = ligature_coverage(&bytes, &font);
        assert!(cov.is_empty());
        assert!(cov.features_for(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn a_non_ligature_feature_is_ignored() {
        // A single substitution's output is indistinguishable from a glyph the
        // producer chose directly, so it says nothing about whether a run was
        // ligated.
        let cov = coverage_of(b"smcp", &[300], false);
        assert!(cov.is_empty());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0x515B_1234u32;
        for _ in 0..2000 {
            let mut gsub = vec![0, 1, 0, 0];
            for _ in 0..64 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                gsub.push((seed >> 24) as u8);
            }
            let bytes = font_with_gsub(gsub);
            if let Ok(font) = Sfnt::parse(&bytes) {
                let cov = ligature_coverage(&bytes, &font);
                let _ = cov.features_for(&[1, 2, 3]);
            }
        }
    }
}
