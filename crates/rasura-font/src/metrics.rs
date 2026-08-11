//! The standard 14 fonts, and the metrics a document gets when it embeds
//! nothing. Spec 8.2.
//!
//! A PDF may name `/Helvetica` and embed no font program at all, on the
//! reasoning that every viewer has one. Layout still has to be right: without
//! widths, every advance is a guess and every line ends in the wrong place.
//!
//! This is also the fix for a gap carried since Phase 2. `rasura-layout`
//! could only measure text whose font dictionary supplied `/Widths`, and the
//! standard 14 routinely do not — the whole point of naming them is to leave
//! the metrics out.
//!
//! Name matching is deliberately generous. Almost nothing writes `/Arial,Bold`
//! or `/TimesNewRoman` expecting failure, and ISO 32000-1 §9.6.2.2 tells a
//! consumer to treat unembedded fonts by "a reasonable substitute". A refusal
//! here shows up as a page laid out at zero width.

use crate::standard14::{STANDARD_14, StandardFont};

/// Look up one of the 14 by its exact PostScript name.
pub fn exact(name: &str) -> Option<&'static StandardFont> {
    STANDARD_14.iter().find(|f| f.name == name)
}

/// Resolve any font name to one of the 14, applying the aliases every producer
/// relies on.
///
/// Returns `None` only for a name with no recognisable family, where guessing
/// would be worse than reporting that nothing is known.
pub fn resolve(name: &str) -> Option<&'static StandardFont> {
    // A subset prefix -- six capitals and a plus -- says nothing about which
    // face this is.
    let name = strip_subset_prefix(name);
    if let Some(f) = exact(name) {
        return Some(f);
    }

    let lower = name.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    // Symbolic faces have no bold or italic variants to choose between.
    if compact.contains("zapfdingbats") || compact.contains("dingbats") {
        return exact("ZapfDingbats");
    }
    if compact.contains("symbol") {
        return exact("Symbol");
    }

    let bold = compact.contains("bold")
        || compact.contains("black")
        || compact.contains("heavy")
        || compact.contains("semibold");
    let italic = compact.contains("italic") || compact.contains("oblique");

    // Ordered so the more specific families win: "couriernew" contains neither
    // "times" nor "helvetica", but "timesnewroman" must not be read as "roman".
    let family = if compact.contains("courier") || compact.contains("mono") {
        Family::Courier
    } else if compact.contains("times")
        || compact.contains("georgia")
        || compact.contains("garamond")
        || compact.contains("book")
        || compact.contains("roman")
        || compact.contains("serif") && !compact.contains("sansserif")
    {
        Family::Times
    } else if compact.contains("helvetica")
        || compact.contains("arial")
        || compact.contains("verdana")
        || compact.contains("tahoma")
        || compact.contains("sans")
        || compact.contains("gothic")
    {
        Family::Helvetica
    } else {
        return None;
    };

    exact(family.face(bold, italic))
}

/// Resolve, falling back to Helvetica.
///
/// Spec 2 requires fidelity to be *reported*, so this is separate from
/// `resolve`: a caller that needs some metrics rather than none asks for the
/// fallback explicitly, and can tell that it happened.
pub fn resolve_or_default(name: &str) -> (&'static StandardFont, bool) {
    match resolve(name) {
        Some(f) => (f, false),
        None => (exact("Helvetica").expect("Helvetica is one of the 14"), true),
    }
}

#[derive(Clone, Copy)]
enum Family {
    Courier,
    Helvetica,
    Times,
}

impl Family {
    fn face(self, bold: bool, italic: bool) -> &'static str {
        match (self, bold, italic) {
            (Family::Courier, false, false) => "Courier",
            (Family::Courier, true, false) => "Courier-Bold",
            (Family::Courier, false, true) => "Courier-Oblique",
            (Family::Courier, true, true) => "Courier-BoldOblique",
            (Family::Helvetica, false, false) => "Helvetica",
            (Family::Helvetica, true, false) => "Helvetica-Bold",
            (Family::Helvetica, false, true) => "Helvetica-Oblique",
            (Family::Helvetica, true, true) => "Helvetica-BoldOblique",
            // Times is the one family whose regular face is not the bare name.
            (Family::Times, false, false) => "Times-Roman",
            (Family::Times, true, false) => "Times-Bold",
            (Family::Times, false, true) => "Times-Italic",
            (Family::Times, true, true) => "Times-BoldItalic",
        }
    }
}

/// Strip a subset prefix such as `ABCDEF+`.
pub fn strip_subset_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase()) {
        &name[7..]
    } else {
        name
    }
}

impl StandardFont {
    /// Advance width of a glyph, in 1/1000 em.
    pub fn width(&self, glyph: &str) -> Option<i32> {
        if let Some(w) = self.fixed_width {
            // Every glyph in a fixed-pitch face is the same width, including
            // ones the table does not list.
            return Some(w);
        }
        self.widths.binary_search_by(|(g, _)| (*g).cmp(glyph)).ok().map(|i| self.widths[i].1)
    }

    /// Whether this face is one of the two symbolic ones, whose built-in
    /// encoding is the font's own rather than Standard or WinAnsi.
    pub fn is_symbolic(&self) -> bool {
        self.name == "Symbol" || self.name == "ZapfDingbats"
    }

    pub fn glyph_count(&self) -> usize {
        self.widths.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fourteen_are_present() {
        assert_eq!(STANDARD_14.len(), 14);
        for name in [
            "Courier",
            "Courier-Bold",
            "Courier-BoldOblique",
            "Courier-Oblique",
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-BoldOblique",
            "Helvetica-Oblique",
            "Times-Roman",
            "Times-Bold",
            "Times-BoldItalic",
            "Times-Italic",
            "Symbol",
            "ZapfDingbats",
        ] {
            assert!(exact(name).is_some(), "{name} is missing");
        }
    }

    #[test]
    fn widths_are_sorted_so_binary_search_is_valid() {
        for f in STANDARD_14 {
            for pair in f.widths.windows(2) {
                assert!(pair[0].0 < pair[1].0, "{}: {} then {}", f.name, pair[0].0, pair[1].0);
            }
        }
    }

    #[test]
    fn known_widths_match_the_afm_values() {
        // Spot values every PDF implementer knows by heart.
        let helv = exact("Helvetica").unwrap();
        assert_eq!(helv.width("space"), Some(278));
        assert_eq!(helv.width("A"), Some(667));
        assert_eq!(helv.width("i"), Some(222));
        assert_eq!(helv.width("W"), Some(944));

        let times = exact("Times-Roman").unwrap();
        assert_eq!(times.width("space"), Some(250));
        assert_eq!(times.width("A"), Some(722));
        assert_eq!(times.width("period"), Some(250));
    }

    #[test]
    fn a_fixed_pitch_face_reports_one_width_for_everything() {
        let c = exact("Courier").unwrap();
        assert_eq!(c.width("space"), Some(600));
        assert_eq!(c.width("W"), Some(600));
        // Including a glyph the table never lists: in a monospaced font that is
        // still the right answer, and returning None would break layout.
        assert_eq!(c.width("nonexistentglyphname"), Some(600));
    }

    #[test]
    fn an_unknown_glyph_in_a_proportional_face_is_reported_missing() {
        assert_eq!(exact("Helvetica").unwrap().width("nosuchglyph"), None);
    }

    #[test]
    fn subset_prefixes_are_stripped() {
        assert_eq!(strip_subset_prefix("ABCDEF+Helvetica"), "Helvetica");
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
        // Only six uppercase letters then a plus. Anything else is a real name.
        assert_eq!(strip_subset_prefix("ABC+Helvetica"), "ABC+Helvetica");
        assert_eq!(strip_subset_prefix("abcdef+Helvetica"), "abcdef+Helvetica");
        assert_eq!(strip_subset_prefix("ABCDEF+"), "ABCDEF+");
    }

    #[test]
    fn the_common_aliases_resolve() {
        assert_eq!(resolve("Arial").unwrap().name, "Helvetica");
        assert_eq!(resolve("Arial,Bold").unwrap().name, "Helvetica-Bold");
        assert_eq!(resolve("Arial-BoldItalicMT").unwrap().name, "Helvetica-BoldOblique");
        assert_eq!(resolve("TimesNewRoman").unwrap().name, "Times-Roman");
        assert_eq!(resolve("TimesNewRomanPS-BoldMT").unwrap().name, "Times-Bold");
        assert_eq!(resolve("CourierNew").unwrap().name, "Courier");
        assert_eq!(resolve("CourierNewPS-ItalicMT").unwrap().name, "Courier-Oblique");
    }

    #[test]
    fn times_new_roman_is_not_read_as_two_families() {
        // "TimesNewRoman" contains "roman", and "roman" must not out-rank
        // "times" -- both point the same way here, but "Courier New" contains
        // neither and must still find Courier.
        assert_eq!(resolve("TimesNewRomanPSMT").unwrap().name, "Times-Roman");
        assert_eq!(resolve("Courier New").unwrap().name, "Courier");
    }

    #[test]
    fn a_subset_prefixed_alias_resolves() {
        assert_eq!(resolve("ABCDEF+Arial-Bold").unwrap().name, "Helvetica-Bold");
    }

    #[test]
    fn the_symbolic_faces_have_no_bold_variant() {
        // "Symbol-Bold" is not a thing; the face is the face.
        assert_eq!(resolve("Symbol").unwrap().name, "Symbol");
        assert_eq!(resolve("Symbol,Bold").unwrap().name, "Symbol");
        assert_eq!(resolve("ZapfDingbats").unwrap().name, "ZapfDingbats");
        assert_eq!(resolve("Dingbats").unwrap().name, "ZapfDingbats");
        assert!(exact("Symbol").unwrap().is_symbolic());
        assert!(!exact("Helvetica").unwrap().is_symbolic());
    }

    #[test]
    fn an_unrecognisable_name_resolves_to_nothing() {
        // Reported rather than guessed: `resolve_or_default` is where a caller
        // opts into a fallback, and it says so.
        assert!(resolve("Wingdings3").is_none());
        assert!(resolve("SomePrivateDisplayFace").is_none());

        let (font, fell_back) = resolve_or_default("SomePrivateDisplayFace");
        assert_eq!(font.name, "Helvetica");
        assert!(fell_back, "the caller must be able to tell");

        let (font, fell_back) = resolve_or_default("Arial");
        assert_eq!(font.name, "Helvetica");
        assert!(!fell_back);
    }

    #[test]
    fn basic_metrics_are_carried_where_the_source_has_them() {
        let helv = exact("Helvetica").unwrap();
        assert!(helv.ascent.is_some() && helv.descent.is_some());
        assert!(helv.descent.unwrap() < 0.0, "descent points downward");
        // Symbol has no meaningful cap height, and the source says so rather
        // than inventing one.
        assert!(exact("Symbol").unwrap().cap_height.is_none());
    }

    #[test]
    fn every_face_has_glyphs() {
        for f in STANDARD_14 {
            if f.fixed_width.is_none() {
                assert!(f.glyph_count() > 100, "{} has only {}", f.name, f.glyph_count());
            }
        }
    }
}
