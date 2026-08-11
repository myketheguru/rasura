//! Metrics for fonts that supply none. Spec 8.2.
//!
//! Spec 8.2's reason for shipping AFM metrics is "so that layout is correct
//! even without the outlines". This is where that becomes true: it implements
//! `rasura-content`'s [`WidthSource`] hook, composing the standard-14
//! widths from `rasura-font` with the encoding tables that turn a character
//! code into the glyph name those widths are keyed by.
//!
//! It lives here, in the layout crate, because neither ingredient alone is
//! enough — the font layer holds the metrics, this layer holds the encodings,
//! and this layer is already the one that calls into extraction. Content cannot
//! do it itself: the metrics sit above it.
//!
//! **A font that supplies its own `/Widths` is never overridden.** The file is
//! the authority on its own layout, and substituting standard metrics for real
//! ones would move text that renders correctly today. This fires only where
//! there is nothing at all.

use crate::glyphdata::{
    MAC_ROMAN_NAMES, STANDARD_NAMES, SYMBOL_NAMES, WIN_ANSI_NAMES, ZAPF_DINGBATS_NAMES,
};
use rasura_content::font::WidthSource;
use rasura_cos::{Dictionary, Document, Object};
use std::collections::HashMap;

/// Supplies standard-14 metrics.
#[derive(Debug, Default, Clone, Copy)]
pub struct Standard14Widths;

impl WidthSource for Standard14Widths {
    fn widths_for(&self, doc: &Document, font: &Dictionary) -> Option<HashMap<u32, f64>> {
        let base = font
            .get("BaseFont")
            .and_then(Object::as_name)
            .and_then(|n| n.as_str())
            .unwrap_or_default();

        // Resolved, not defaulted. `resolve` returns nothing for a name with no
        // recognisable family, and inventing Helvetica's metrics for an unknown
        // display face would lay it out confidently wrong -- worse than
        // reporting no metrics, which the caller already handles.
        let face = rasura_font::metrics::resolve(base)?;
        debug_assert_eq!(crate::metrics_face(font), Some(face.name));

        let names = code_to_name(doc, font, face.is_symbolic(), face.name);
        let mut out = HashMap::new();
        for (code, name) in names {
            if let Some(w) = face.width(&name) {
                out.insert(code, w as f64);
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// The code-to-glyph-name mapping for a simple font.
///
/// Follows ISO 32000-1 §9.6.6: a base encoding, overridden by `/Differences`.
/// A symbolic face uses its own built-in encoding, which for Symbol and
/// ZapfDingbats is neither Standard nor WinAnsi and is why those two needed
/// their own tables.
fn code_to_name(
    doc: &Document,
    font: &Dictionary,
    symbolic: bool,
    face: &str,
) -> HashMap<u32, String> {
    let encoding = doc.get_entry(font, "Encoding").ok().flatten();
    let named = encoding.as_ref().and_then(|e| e.as_name()).and_then(|n| n.as_str());
    let dict = encoding.as_ref().and_then(|e| e.as_dict());
    let base_name = named.or_else(|| {
        dict.and_then(|d| d.get("BaseEncoding")).and_then(Object::as_name).and_then(|n| n.as_str())
    });

    let base: &[Option<&str>; 256] = match base_name {
        Some("WinAnsiEncoding") => &WIN_ANSI_NAMES,
        Some("MacRomanEncoding") => &MAC_ROMAN_NAMES,
        Some("StandardEncoding") => &STANDARD_NAMES,
        _ if face == "Symbol" => &SYMBOL_NAMES,
        _ if face == "ZapfDingbats" => &ZAPF_DINGBATS_NAMES,
        // A symbolic face with no named base encoding uses its own, which for
        // anything but those two is in the font program and not here.
        _ if symbolic => &STANDARD_NAMES,
        _ => &STANDARD_NAMES,
    };

    let mut out: HashMap<u32, String> = HashMap::new();
    for (code, name) in base.iter().enumerate() {
        if let Some(name) = name {
            out.insert(code as u32, (*name).to_string());
        }
    }

    // `/Differences`: a sequence of numbers, each followed by the glyph names
    // that start at that code.
    if let Some(dict) = dict
        && let Ok(Some(diff)) = doc.get_entry(dict, "Differences")
        && let Some(items) = diff.as_array()
    {
        let mut code: i64 = 0;
        for item in items {
            let Ok(item) = doc.resolve(item) else { continue };
            match &*item {
                Object::Integer(n) => code = *n,
                Object::Real(v) => code = *v as i64,
                Object::Name(n) => {
                    if let (Ok(c), Some(name)) = (u32::try_from(code), n.as_str()) {
                        out.insert(c, name.to_string());
                    }
                    code += 1;
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn font_dict(entries: &str) -> (Document, Dictionary) {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(5, entries)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).expect("open");
        let dict = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        (doc, dict)
    }

    #[test]
    fn helvetica_without_widths_gets_the_afm_metrics() {
        let (doc, dict) = font_dict(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        // The values every PDF implementer knows: space 278, A 667, W 944.
        assert_eq!(w.get(&32), Some(&278.0));
        assert_eq!(w.get(&65), Some(&667.0));
        assert_eq!(w.get(&87), Some(&944.0));
    }

    #[test]
    fn an_alias_resolves_to_the_matching_face() {
        let (doc, dict) = font_dict(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Arial,Bold /Encoding /WinAnsiEncoding >>",
        );
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        // Helvetica-Bold's A is 722, against Helvetica's 667.
        assert_eq!(w.get(&65), Some(&722.0));
    }

    #[test]
    fn courier_is_uniform() {
        let (doc, dict) = font_dict("<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        assert_eq!(w.get(&32), Some(&600.0));
        assert_eq!(w.get(&87), Some(&600.0));
    }

    #[test]
    fn symbol_uses_its_own_built_in_encoding() {
        // The gap carried since Phase 2: Symbol's encoding is neither Standard
        // nor WinAnsi, so without its own table there is no glyph name and no
        // width.
        let (doc, dict) = font_dict("<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>");
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        // 0x61 is alpha in Symbol, 631 units; in StandardEncoding it would be
        // `a`, which Symbol does not have at all.
        assert_eq!(w.get(&0x61), Some(&631.0));
        assert!(w.len() > 100, "most of the face is covered: {}", w.len());
    }

    #[test]
    fn zapf_dingbats_uses_its_own_built_in_encoding() {
        let (doc, dict) = font_dict("<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>");
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        assert!(w.len() > 100, "{}", w.len());
        assert_eq!(w.get(&32), Some(&278.0), "space is still space");
    }

    #[test]
    fn differences_override_the_base_encoding() {
        let (doc, dict) = font_dict(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /W 66 /space] >> >>",
        );
        let w = Standard14Widths.widths_for(&doc, &dict).expect("metrics");
        assert_eq!(w.get(&65), Some(&944.0), "code 65 was remapped to W");
        assert_eq!(w.get(&66), Some(&278.0), "code 66 was remapped to space");
        assert_eq!(w.get(&67), Some(&722.0), "C is untouched");
    }

    #[test]
    fn an_unrecognisable_face_supplies_nothing() {
        // Spec 2: reported, not guessed. Inventing Helvetica's metrics for an
        // unknown display face lays it out confidently wrong.
        let (doc, dict) =
            font_dict("<< /Type /Font /Subtype /Type1 /BaseFont /SomePrivateDisplayFace >>");
        assert!(Standard14Widths.widths_for(&doc, &dict).is_none());
    }

    #[test]
    fn a_font_with_no_basefont_supplies_nothing() {
        let (doc, dict) = font_dict("<< /Type /Font /Subtype /Type1 >>");
        assert!(Standard14Widths.widths_for(&doc, &dict).is_none());
    }
}
