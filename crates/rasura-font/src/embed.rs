//! The PDF-level half of glyph injection. Spec 8.4.
//!
//! Rebuilding the font program is only half the job. The PDF dictionaries
//! describing it have to agree, or the new glyph is present in the file and
//! unreachable from the page:
//!
//! - `/Widths`, with `/FirstChar` and `/LastChar` adjusted;
//! - `/Differences`, so a code reaches the glyph;
//! - `/FontBBox`, widened if the new glyph exceeds it;
//! - `/ToUnicode`, **always**.
//!
//! That last one is the strongest claim in §8.4 and the easiest to skip,
//! because nothing renders differently when it is missing. A font injected into
//! without extending `/ToUnicode` produces a document whose new text cannot be
//! searched, copied or extracted — this library would have made the file worse
//! at the thing this library exists to do.

use crate::error::{FontError, Result};
use rasura_cos::{Dictionary, Document, Name, Object};

/// A glyph being added to a simple font.
#[derive(Debug, Clone)]
pub struct Added {
    /// The character code that will reach it.
    pub code: u8,
    /// Its name, for `/Differences`.
    pub name: String,
    /// Advance width in glyph space (1/1000 em).
    pub width: f64,
    /// The text it represents. Required, not optional: see the module note.
    pub unicode: String,
    /// Glyph bounding box in font units, if known, for `/FontBBox`.
    pub bbox: Option<[i16; 4]>,
}

/// The dictionaries an injection produces, ready to be written back.
#[derive(Debug, Clone)]
pub struct FontUpdate {
    pub font: Dictionary,
    pub descriptor: Dictionary,
    /// A complete `/ToUnicode` CMap, to be written as a stream.
    pub to_unicode: Vec<u8>,
    /// Codes that already had a glyph and were left alone.
    pub skipped: Vec<u8>,
}

/// Apply spec 8.4's PDF-level updates to a simple font.
///
/// The font dictionary and descriptor are returned modified rather than mutated
/// in place, so a caller can stage them into a transaction and decide when they
/// become visible.
pub fn update_simple_font(
    doc: &Document,
    font: &Dictionary,
    descriptor: &Dictionary,
    added: &[Added],
) -> Result<FontUpdate> {
    if added.is_empty() {
        return Err(FontError::Malformed("nothing to add"));
    }

    let mut font = font.clone();
    let mut descriptor = descriptor.clone();
    let mut skipped = Vec::new();

    // --- /Widths, /FirstChar, /LastChar ---------------------------------
    let old_first = doc
        .get_entry(&font, "FirstChar")
        .ok()
        .flatten()
        .and_then(|o| o.as_i64())
        .unwrap_or(0)
        .clamp(0, 255) as u8;
    let old_widths: Vec<f64> = doc
        .get_entry(&font, "Widths")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .map(|items| {
            items
                .iter()
                .map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()).unwrap_or(0.0))
                .collect()
        })
        .unwrap_or_default();
    let old_last = old_first as usize + old_widths.len().saturating_sub(1);

    // The array is indexed by code, so widening it means covering every code
    // between the new bounds -- including ones the font never had, which get
    // the descriptor's /MissingWidth rather than a made-up number.
    let missing = doc
        .get_entry(&descriptor, "MissingWidth")
        .ok()
        .flatten()
        .and_then(|o| o.as_f64())
        .unwrap_or(0.0);

    let new_first = added.iter().map(|a| a.code).min().unwrap_or(old_first).min(old_first);
    let new_last = added.iter().map(|a| a.code as usize).max().unwrap_or(old_last).max(old_last);

    let mut widths = vec![missing; new_last - new_first as usize + 1];
    for (i, w) in old_widths.iter().enumerate() {
        let code = old_first as usize + i;
        if code >= new_first as usize && code <= new_last {
            widths[code - new_first as usize] = *w;
        }
    }
    for a in added {
        // A code that already resolves to a glyph is left alone. Overwriting it
        // would change text the user never touched.
        let index = a.code as usize - new_first as usize;
        let occupied = (a.code as usize) >= old_first as usize
            && (a.code as usize) <= old_last
            && old_widths.get(a.code as usize - old_first as usize).is_some_and(|w| *w > 0.0);
        if occupied {
            skipped.push(a.code);
            continue;
        }
        widths[index] = a.width;
    }

    font.insert(Name::new("FirstChar"), Object::Integer(new_first as i64));
    font.insert(Name::new("LastChar"), Object::Integer(new_last as i64));
    font.insert(Name::new("Widths"), Object::Array(widths.into_iter().map(Object::Real).collect()));

    // --- /Encoding /Differences -----------------------------------------
    let mut encoding = doc
        .get_entry(&font, "Encoding")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    // A font whose /Encoding was a bare name keeps it as the base, so the codes
    // it already had continue to mean what they meant.
    if let Ok(Some(existing)) = doc.get_entry(&font, "Encoding")
        && let Some(name) = existing.as_name()
        && encoding.get("BaseEncoding").is_none()
    {
        encoding.insert(Name::new("BaseEncoding"), Object::Name(name.clone()));
    }

    let mut differences: Vec<Object> = doc
        .get_entry(&encoding, "Differences")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    for a in added {
        if skipped.contains(&a.code) {
            continue;
        }
        differences.push(Object::Integer(a.code as i64));
        differences.push(Object::Name(Name::new(&a.name)));
    }
    encoding.insert(Name::new("Differences"), Object::Array(differences));
    encoding.insert(Name::new("Type"), Object::Name(Name::new("Encoding")));
    font.insert(Name::new("Encoding"), Object::Dictionary(encoding));

    // --- /FontBBox --------------------------------------------------------
    // Widened if a new glyph exceeds it. /StemV, /Flags and /ItalicAngle are
    // left alone, per spec 8.4: they describe the typeface, and one injected
    // glyph is not evidence about it.
    if let Some(mut bbox) = doc
        .get_entry(&descriptor, "FontBBox")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .filter(|a| a.len() == 4)
        .map(|a| {
            let mut out = [0i64; 4];
            for (i, v) in a.iter().enumerate() {
                out[i] = doc.resolve(v).ok().and_then(|o| o.as_f64()).unwrap_or(0.0) as i64;
            }
            out
        })
    {
        let mut changed = false;
        for a in added.iter().filter_map(|a| a.bbox) {
            // The first two are the lower-left corner and take a minimum; the
            // last two are the upper-right and take a maximum.
            let widened = [
                bbox[0].min(a[0] as i64),
                bbox[1].min(a[1] as i64),
                bbox[2].max(a[2] as i64),
                bbox[3].max(a[3] as i64),
            ];
            if widened != bbox {
                bbox = widened;
                changed = true;
            }
        }
        if changed {
            descriptor.insert(
                Name::new("FontBBox"),
                Object::Array(bbox.iter().map(|v| Object::Integer(*v)).collect()),
            );
        }
    }

    // --- /ToUnicode -------------------------------------------------------
    let mut mappings: Vec<(u32, String)> = Vec::new();
    if let Some(id) = font.get("ToUnicode").and_then(Object::as_reference)
        && let Ok(data) = doc.decoded_stream(id)
    {
        let existing = rasura_content::cmap::CMap::parse(&data);
        for code in 0..=255u32 {
            if let Some(text) = existing.unicode(code) {
                mappings.push((code, text.to_string()));
            }
        }
    }
    for a in added {
        if skipped.contains(&a.code) {
            continue;
        }
        mappings.retain(|(c, _)| *c != a.code as u32);
        mappings.push((a.code as u32, a.unicode.clone()));
    }
    mappings.sort_by_key(|(c, _)| *c);

    Ok(FontUpdate { font, descriptor, to_unicode: to_unicode_cmap(&mappings), skipped })
}

/// A glyph being added to a composite (Type 0) font.
#[derive(Debug, Clone)]
pub struct AddedCid {
    /// The CID that will reach it.
    pub cid: u16,
    /// Its glyph id in the rebuilt font program.
    pub gid: u16,
    /// Advance width in glyph space (1/1000 em).
    pub width: f64,
    /// The text it represents. Required, as for simple fonts.
    pub unicode: String,
}

/// Spec 8.4's PDF-level updates for a composite font.
///
/// > Composite fonts: extend `/W`, `/CIDToGIDMap` (if a stream, rewrite; if
/// > `/Identity`, no change), `/DW` unchanged.
///
/// Takes the *descendant* CIDFont dictionary, which is where `/W` and
/// `/CIDToGIDMap` live; `/ToUnicode` belongs to the Type 0 parent and is
/// returned for the caller to attach there.
pub fn update_composite_font(
    doc: &Document,
    parent: &Dictionary,
    descendant: &Dictionary,
    added: &[AddedCid],
) -> Result<CompositeUpdate> {
    if added.is_empty() {
        return Err(FontError::Malformed("nothing to add"));
    }
    let mut descendant = descendant.clone();

    // --- /W ---------------------------------------------------------------
    // Appended as one `cid [w]` group per glyph rather than merged into the
    // existing runs. Merging would mean rewriting groups the edit did not
    // touch, and a `cfirst clast w` run cannot absorb a new CID without
    // changing what it says about the CIDs already in it.
    let mut w: Vec<Object> = doc
        .get_entry(&descendant, "W")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    for a in added {
        w.push(Object::Integer(a.cid as i64));
        w.push(Object::Array(vec![Object::Real(a.width)]));
    }
    descendant.insert(Name::new("W"), Object::Array(w));

    // --- /CIDToGIDMap ------------------------------------------------------
    // `/Identity` means CID equals GID, so an injection that keeps that true
    // needs no change -- and one that breaks it needs a stream, which is a
    // different edit from this one.
    // The *raw* entry, not the resolved one: `get_entry` follows the reference
    // and hands back the stream, from which the object id -- the thing needed
    // to read the existing table -- is no longer recoverable.
    let raw_entry = descendant.get("CIDToGIDMap").cloned();
    let map_entry = doc.get_entry(&descendant, "CIDToGIDMap").ok().flatten();
    let is_identity = map_entry
        .as_ref()
        .and_then(|o| o.as_name())
        .and_then(|n| n.as_str())
        .is_some_and(|n| n == "Identity");
    let mut cid_to_gid = None;

    if is_identity || map_entry.is_none() {
        // Identity is only still true if every added CID equals its GID.
        // Where it is not, the map has to become a stream, because leaving
        // `/Identity` in place would silently point the CID at the wrong glyph.
        if let Some(bad) = added.iter().find(|a| a.cid != a.gid) {
            let max = added.iter().map(|a| a.cid).max().unwrap_or(0);
            let mut table = vec![0u8; (max as usize + 1) * 2];
            // Identity for everything below, then the real mapping.
            for cid in 0..=max {
                let gid = added.iter().find(|a| a.cid == cid).map(|a| a.gid).unwrap_or(cid);
                table[cid as usize * 2..cid as usize * 2 + 2].copy_from_slice(&gid.to_be_bytes());
            }
            let _ = bad;
            cid_to_gid = Some(table);
        }
    } else if let Some(id) = raw_entry.as_ref().and_then(Object::as_reference)
        && let Ok(existing) = doc.decoded_stream(id)
    {
        // A stream: rewritten, extended to cover the new CIDs.
        let max = added.iter().map(|a| a.cid).max().unwrap_or(0);
        let mut table = existing.to_vec();
        table.resize(((max as usize + 1) * 2).max(table.len()), 0);
        for a in added {
            table[a.cid as usize * 2..a.cid as usize * 2 + 2].copy_from_slice(&a.gid.to_be_bytes());
        }
        cid_to_gid = Some(table);
    }

    // --- /ToUnicode --------------------------------------------------------
    // Composite codes are two bytes, so the CMap's codespace and its entries
    // are four hex digits. Emitting one-byte codes here produces a CMap that
    // parses and maps the wrong things.
    let mut mappings: Vec<(u32, String)> = Vec::new();
    if let Some(id) = parent.get("ToUnicode").and_then(Object::as_reference)
        && let Ok(data) = doc.decoded_stream(id)
    {
        let existing = rasura_content::cmap::CMap::parse(&data);
        for code in 0..=0xFFFFu32 {
            if let Some(text) = existing.unicode(code) {
                mappings.push((code, text.to_string()));
            }
        }
    }
    for a in added {
        mappings.retain(|(c, _)| *c != a.cid as u32);
        mappings.push((a.cid as u32, a.unicode.clone()));
    }
    mappings.sort_by_key(|(c, _)| *c);

    Ok(CompositeUpdate { descendant, to_unicode: to_unicode_cmap_with(&mappings, 2), cid_to_gid })
}

/// What a composite-font injection produces.
#[derive(Debug, Clone)]
pub struct CompositeUpdate {
    /// The descendant CIDFont, with `/W` and `/CIDToGIDMap` updated.
    pub descendant: Dictionary,
    /// A `/ToUnicode` CMap for the Type 0 parent.
    pub to_unicode: Vec<u8>,
    /// A `/CIDToGIDMap` stream, when one is needed. `None` means `/Identity`
    /// still holds and the entry should be left alone.
    pub cid_to_gid: Option<Vec<u8>>,
}

/// Emit a `/ToUnicode` CMap for the given code-to-text mappings.
///
/// Written as `bfchar` entries throughout rather than compressed into
/// `bfrange`s: the saving is a few hundred bytes on a font, and a range whose
/// endpoints are computed wrongly maps a span of codes to the wrong characters
/// silently.
pub fn to_unicode_cmap(mappings: &[(u32, String)]) -> Vec<u8> {
    to_unicode_cmap_with(mappings, 1)
}

/// As [`to_unicode_cmap`], for codes of a given width in bytes.
///
/// A composite font's codes are two bytes wide, and its CMap's codespace and
/// entries must say so. A one-byte codespace in a Type 0 font's `/ToUnicode`
/// parses cleanly and maps the wrong things.
pub fn to_unicode_cmap_with(mappings: &[(u32, String)], code_bytes: usize) -> Vec<u8> {
    let digits = code_bytes * 2;
    let low = "0".repeat(digits);
    let high = "F".repeat(digits);
    let mut out = format!(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n<{low}> <{high}>\nendcodespacerange\n"
    );

    // A CMap allows at most 100 entries per block.
    for chunk in mappings.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, text) in chunk {
            out.push_str(&format!("<{code:0digits$X}> <", digits = digits));
            for unit in text.encode_utf16() {
                out.push_str(&format!("{unit:04X}"));
            }
            out.push_str(">\n");
        }
        out.push_str("endbfchar\n");
    }

    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn load(font_entries: &str, descriptor_entries: &str) -> (Document, Dictionary, Dictionary) {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(5, font_entries)
            .object(6, descriptor_entries)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).expect("open");
        let font = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let desc = doc.get(rasura_cos::ObjId::new(6, 0)).unwrap().as_dict().unwrap().clone();
        (doc, font, desc)
    }

    fn added(code: u8, name: &str, unicode: &str, width: f64) -> Added {
        Added { code, name: name.into(), width, unicode: unicode.into(), bbox: None }
    }

    fn widths_of(font: &Dictionary) -> Vec<f64> {
        font.get("Widths")
            .and_then(Object::as_array)
            .map(|a| a.iter().map(|o| o.as_f64().unwrap_or(0.0)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn widths_extend_and_the_bounds_follow() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 67 \
             /Widths [100 200 300] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(70, "eacute", "é", 555.0)]).unwrap();

        assert_eq!(u.font.get("FirstChar").and_then(Object::as_i64), Some(65));
        assert_eq!(u.font.get("LastChar").and_then(Object::as_i64), Some(70));
        let w = widths_of(&u.font);
        assert_eq!(w.len(), 6, "65..=70");
        assert_eq!(&w[..3], &[100.0, 200.0, 300.0], "the originals are untouched");
        assert_eq!(w[5], 555.0, "the new glyph's width lands at its code");
    }

    #[test]
    fn a_code_below_firstchar_moves_the_lower_bound() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 66 \
             /Widths [100 200] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(32, "space", " ", 250.0)]).unwrap();
        assert_eq!(u.font.get("FirstChar").and_then(Object::as_i64), Some(32));
        let w = widths_of(&u.font);
        assert_eq!(w.len(), 35, "32..=66");
        assert_eq!(w[0], 250.0);
        assert_eq!(w[33], 100.0, "the original A is still at code 65");
    }

    #[test]
    fn codes_the_font_never_had_get_missing_width_not_a_guess() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X /MissingWidth 42 >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(70, "x", "x", 500.0)]).unwrap();
        let w = widths_of(&u.font);
        assert_eq!(w[1], 42.0, "code 66 was never defined; it gets /MissingWidth");
        assert_eq!(w[0], 100.0);
        assert_eq!(w[5], 500.0);
    }

    #[test]
    fn an_occupied_code_is_left_alone() {
        // Overwriting it would change text the user never touched.
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 66 \
             /Widths [100 200] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(65, "other", "Z", 999.0)]).unwrap();
        assert_eq!(u.skipped, vec![65]);
        assert_eq!(widths_of(&u.font)[0], 100.0, "unchanged");
    }

    #[test]
    fn differences_gain_an_entry_for_each_new_code() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(
            &doc,
            &font,
            &desc,
            &[added(70, "eacute", "é", 500.0), added(71, "ccedilla", "ç", 500.0)],
        )
        .unwrap();

        let enc = u.font.get("Encoding").and_then(Object::as_dict).expect("encoding dict");
        let diffs = enc.get("Differences").and_then(Object::as_array).expect("differences");
        let rendered: Vec<String> = diffs
            .iter()
            .map(|o| match o {
                Object::Integer(n) => n.to_string(),
                Object::Name(n) => format!("/{}", n.as_str().unwrap_or("?")),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(rendered, vec!["70", "/eacute", "71", "/ccedilla"]);
    }

    #[test]
    fn a_named_encoding_becomes_the_base_encoding() {
        // The codes the font already had must keep meaning what they meant.
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /Encoding /WinAnsiEncoding \
             /FirstChar 65 /LastChar 65 /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(70, "eacute", "é", 500.0)]).unwrap();
        let enc = u.font.get("Encoding").and_then(Object::as_dict).unwrap();
        assert_eq!(
            enc.get("BaseEncoding").and_then(Object::as_name).and_then(|n| n.as_str()),
            Some("WinAnsiEncoding")
        );
    }

    #[test]
    fn existing_differences_are_preserved() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R \
             /Encoding << /Type /Encoding /Differences [65 /A] >> >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(70, "eacute", "é", 500.0)]).unwrap();
        let enc = u.font.get("Encoding").and_then(Object::as_dict).unwrap();
        let diffs = enc.get("Differences").and_then(Object::as_array).unwrap();
        assert_eq!(diffs.len(), 4, "the original pair plus the new one");
    }

    #[test]
    fn the_font_bbox_widens_only_when_exceeded() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X /FontBBox [-100 -200 900 800] /StemV 80 >>",
        );
        let mut a = added(70, "tall", "T", 500.0);
        a.bbox = Some([-150, -200, 900, 1000]);
        let u = update_simple_font(&doc, &font, &desc, &[a]).unwrap();

        let bbox: Vec<i64> = u
            .descriptor
            .get("FontBBox")
            .and_then(Object::as_array)
            .unwrap()
            .iter()
            .map(|o| o.as_i64().unwrap())
            .collect();
        assert_eq!(bbox, vec![-150, -200, 900, 1000]);
        // Spec 8.4: /StemV and friends describe the typeface, and one glyph is
        // not evidence about it.
        assert_eq!(u.descriptor.get("StemV").and_then(Object::as_i64), Some(80));
    }

    #[test]
    fn a_glyph_inside_the_bbox_leaves_it_alone() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X /FontBBox [-100 -200 900 800] >>",
        );
        let mut a = added(70, "small", "s", 500.0);
        a.bbox = Some([0, 0, 400, 400]);
        let u = update_simple_font(&doc, &font, &desc, &[a]).unwrap();
        let bbox: Vec<i64> = u
            .descriptor
            .get("FontBBox")
            .and_then(Object::as_array)
            .unwrap()
            .iter()
            .map(|o| o.as_i64().unwrap())
            .collect();
        assert_eq!(bbox, vec![-100, -200, 900, 800]);
    }

    // --- /ToUnicode -----------------------------------------------------------

    #[test]
    fn to_unicode_is_always_produced() {
        // Spec 8.4: "Extend /ToUnicode with the new mappings. Always." Nothing
        // renders differently when it is missing, which is exactly why it is
        // easy to skip -- and skipping it makes the document unsearchable.
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
             /Widths [100] /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        let u = update_simple_font(&doc, &font, &desc, &[added(70, "eacute", "é", 500.0)]).unwrap();
        let text = String::from_utf8(u.to_unicode.clone()).unwrap();

        assert!(text.contains("beginbfchar"), "{text}");
        assert!(text.contains("<46> <00E9>"), "code 0x46 maps to U+00E9: {text}");
        assert!(text.contains("endcmap"));
    }

    #[test]
    fn an_existing_to_unicode_is_extended_not_replaced() {
        let cmap = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
                     1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
                     1 beginbfchar\n<41> <0041>\nendbfchar\nendcmap\nend\nend";
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                "<< /Type /Font /Subtype /TrueType /BaseFont /X /FirstChar 65 /LastChar 65 \
                 /Widths [100] /FontDescriptor 6 0 R /ToUnicode 7 0 R >>",
            )
            .object(6, "<< /Type /FontDescriptor /FontName /X >>")
            .stream(7, "", cmap)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let font = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let desc = doc.get(rasura_cos::ObjId::new(6, 0)).unwrap().as_dict().unwrap().clone();

        let u = update_simple_font(&doc, &font, &desc, &[added(70, "eacute", "é", 500.0)]).unwrap();
        let text = String::from_utf8(u.to_unicode).unwrap();
        assert!(text.contains("<41> <0041>"), "the existing mapping survived: {text}");
        assert!(text.contains("<46> <00E9>"), "and the new one was added");
    }

    #[test]
    fn a_supplementary_character_is_written_as_a_surrogate_pair() {
        // /ToUnicode values are UTF-16BE, so anything past the BMP takes two
        // units. Writing the code point directly yields a CMap readers reject.
        let cmap = to_unicode_cmap(&[(0x41, "\u{1F600}".to_string())]);
        let text = String::from_utf8(cmap).unwrap();
        assert!(text.contains("<41> <D83DDE00>"), "{text}");
    }

    #[test]
    fn a_ligature_maps_to_several_characters() {
        let cmap = to_unicode_cmap(&[(0x01, "ffi".to_string())]);
        let text = String::from_utf8(cmap).unwrap();
        assert!(text.contains("<01> <006600660069>"), "{text}");
    }

    #[test]
    fn more_than_a_hundred_mappings_are_split_into_blocks() {
        // A CMap allows at most 100 entries per block; one longer is rejected.
        let mappings: Vec<(u32, String)> =
            (0..250u32).map(|i| (i, char::from_u32(0x41 + i).unwrap().to_string())).collect();
        let text = String::from_utf8(to_unicode_cmap(&mappings)).unwrap();
        assert_eq!(text.matches("beginbfchar").count(), 3);
        assert_eq!(text.matches("endbfchar").count(), 3);
        assert!(text.contains("100 beginbfchar"));
        assert!(text.contains("50 beginbfchar"));
    }

    // --- composite fonts ------------------------------------------------------

    fn composite(
        descendant_entries: &str,
        parent_extra: &str,
    ) -> (Document, Dictionary, Dictionary) {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
                     /DescendantFonts [6 0 R] {parent_extra} >>"
                ),
            )
            .object(6, descendant_entries)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).expect("open");
        let parent = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let desc = doc.get(rasura_cos::ObjId::new(6, 0)).unwrap().as_dict().unwrap().clone();
        (doc, parent, desc)
    }

    fn added_cid(cid: u16, gid: u16, width: f64, unicode: &str) -> AddedCid {
        AddedCid { cid, gid, width, unicode: unicode.into() }
    }

    #[test]
    fn the_w_array_gains_a_group_and_leaves_the_others_alone() {
        // Appended rather than merged: a `cfirst clast w` run cannot absorb a
        // new CID without changing what it says about the ones already in it.
        let (doc, parent, desc) = composite(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /DW 1000 \
             /W [1 [500 600] 10 20 750] /CIDSystemInfo << /Registry (Adobe) \
             /Ordering (Identity) /Supplement 0 >> >>",
            "",
        );
        let u =
            update_composite_font(&doc, &parent, &desc, &[added_cid(99, 99, 555.0, "Z")]).unwrap();

        let w = u.descendant.get("W").and_then(Object::as_array).unwrap();
        // The original five entries survive verbatim.
        assert_eq!(w[0].as_i64(), Some(1));
        assert_eq!(w[2].as_i64(), Some(10));
        assert_eq!(w[3].as_i64(), Some(20));
        assert_eq!(w[4].as_f64(), Some(750.0));
        // Then the new group.
        assert_eq!(w[5].as_i64(), Some(99));
        assert_eq!(w[6].as_array().and_then(|a| a.first()).and_then(Object::as_f64), Some(555.0));
        // /DW is untouched, per spec 8.4.
        assert_eq!(u.descendant.get("DW").and_then(Object::as_i64), Some(1000));
    }

    #[test]
    fn identity_cid_to_gid_stays_identity_when_it_still_holds() {
        let (doc, parent, desc) = composite(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDToGIDMap /Identity \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            "",
        );
        // CID equals GID, so the identity claim is still true.
        let u =
            update_composite_font(&doc, &parent, &desc, &[added_cid(42, 42, 500.0, "A")]).unwrap();
        assert!(u.cid_to_gid.is_none(), "no stream needed");
    }

    #[test]
    fn identity_becomes_a_stream_when_the_cid_and_gid_diverge() {
        // Leaving /Identity in place would point the CID at the wrong glyph --
        // silently, since nothing errors and a glyph is still drawn.
        let (doc, parent, desc) = composite(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDToGIDMap /Identity \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            "",
        );
        let u =
            update_composite_font(&doc, &parent, &desc, &[added_cid(5, 40, 500.0, "A")]).unwrap();
        let table = u.cid_to_gid.expect("a stream is required now");
        assert_eq!(&table[10..12], &40u16.to_be_bytes(), "CID 5 -> GID 40");
        assert_eq!(&table[6..8], &3u16.to_be_bytes(), "and the rest stayed identity");
    }

    #[test]
    fn an_existing_cid_to_gid_stream_is_extended_not_replaced() {
        let mut existing = vec![0u8; 8];
        existing[4..6].copy_from_slice(&77u16.to_be_bytes()); // CID 2 -> GID 77
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
                 /DescendantFonts [6 0 R] >>",
            )
            .object(
                6,
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDToGIDMap 7 0 R \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            )
            .stream(7, "", &existing)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let parent = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let desc = doc.get(rasura_cos::ObjId::new(6, 0)).unwrap().as_dict().unwrap().clone();

        let u =
            update_composite_font(&doc, &parent, &desc, &[added_cid(9, 88, 500.0, "B")]).unwrap();
        let table = u.cid_to_gid.expect("a stream");
        assert_eq!(&table[4..6], &77u16.to_be_bytes(), "the original mapping survived");
        assert_eq!(&table[18..20], &88u16.to_be_bytes(), "and CID 9 was added");
    }

    #[test]
    fn a_composite_to_unicode_uses_two_byte_codes() {
        // A one-byte codespace in a Type 0 font parses cleanly and maps the
        // wrong things.
        let (doc, parent, desc) = composite(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            "",
        );
        let u = update_composite_font(&doc, &parent, &desc, &[added_cid(0x1234, 5, 500.0, "Q")])
            .unwrap();
        let text = String::from_utf8(u.to_unicode).unwrap();
        assert!(text.contains("<0000> <FFFF>"), "two-byte codespace: {text}");
        assert!(text.contains("<1234> <0051>"), "{text}");
    }

    #[test]
    fn adding_nothing_to_a_composite_is_refused() {
        let (doc, parent, desc) = composite(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>",
            "",
        );
        assert!(update_composite_font(&doc, &parent, &desc, &[]).is_err());
    }

    #[test]
    fn adding_nothing_is_refused() {
        let (doc, font, desc) = load(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X /FontDescriptor 6 0 R >>",
            "<< /Type /FontDescriptor /FontName /X >>",
        );
        assert!(update_simple_font(&doc, &font, &desc, &[]).is_err());
    }
}
