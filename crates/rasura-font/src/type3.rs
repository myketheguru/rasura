//! Type 3 fonts. Spec 8.7.
//!
//! > Glyph procedures are content streams. Reading works. Editing text in a
//! > Type 3 font is supported only when every needed glyph already exists;
//! > there is no sensible way to synthesise a new procedure. Return
//! > `EditError::Type3GlyphMissing` with the list of missing codes.
//!
//! A Type 3 glyph is an arbitrary program — it can stroke paths, place images,
//! invoke XObjects, set colour. Nothing about "the letter é" tells you what
//! that program should contain, and a synthesised outline would not match the
//! rest of the face in weight, width or style. Every other font format has an
//! answer to "give me a glyph for this character"; Type 3 does not, and the
//! honest response is to say which codes cannot be typed rather than to invent
//! marks.
//!
//! So this module answers one question — *which of these codes does the font
//! already have?* — and the edit layer turns the answer into a refusal.

use rasura_cos::{Dictionary, Document, Object};

/// A Type 3 font's glyph procedures, as far as editing needs them.
#[derive(Debug, Clone, Default)]
pub struct Type3 {
    /// Glyph names the font defines a procedure for.
    pub procedures: Vec<String>,
    /// Code to glyph name, from `/Encoding` `/Differences`. A Type 3 font has
    /// no built-in encoding, so `/Differences` is the only way its codes reach
    /// its procedures -- a font without one can draw nothing at all.
    pub encoding: Vec<(u8, String)>,
}

impl Type3 {
    /// Read what the edit layer needs from a Type 3 font dictionary.
    pub fn from_dict(doc: &Document, font: &Dictionary) -> Type3 {
        let mut out = Type3::default();

        if let Ok(Some(procs)) = doc.get_entry(font, "CharProcs")
            && let Some(dict) = procs.as_dict()
        {
            for (name, _) in dict.iter() {
                if let Some(name) = name.as_str() {
                    out.procedures.push(name.to_string());
                }
            }
        }

        if let Ok(Some(encoding)) = doc.get_entry(font, "Encoding")
            && let Some(enc) = encoding.as_dict()
            && let Ok(Some(diffs)) = doc.get_entry(enc, "Differences")
            && let Some(items) = diffs.as_array()
        {
            let mut code: i64 = 0;
            for item in items {
                let Ok(item) = doc.resolve(item) else { continue };
                match &*item {
                    Object::Integer(n) => code = *n,
                    Object::Real(v) => code = *v as i64,
                    Object::Name(n) => {
                        if let (Ok(c), Some(name)) = (u8::try_from(code), n.as_str()) {
                            out.encoding.push((c, name.to_string()));
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// The glyph name a code reaches, if any.
    pub fn glyph_for(&self, code: u8) -> Option<&str> {
        self.encoding.iter().find(|(c, _)| *c == code).map(|(_, n)| n.as_str())
    }

    /// Whether a code can be drawn: it maps to a name, and that name has a
    /// procedure.
    pub fn can_draw(&self, code: u8) -> bool {
        self.glyph_for(code).is_some_and(|n| self.procedures.iter().any(|p| p == n))
    }

    /// Which of `codes` this font cannot draw.
    ///
    /// Spec 8.7 wants the *list*, not a boolean: a caller told only that an
    /// edit is impossible can do nothing about it, whereas one told which
    /// characters are missing can offer to drop them, spell them differently,
    /// or switch fonts.
    ///
    /// Duplicates are collapsed and the result is ordered, so the message a
    /// user sees does not depend on the order the text happened to be scanned.
    pub fn missing(&self, codes: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = codes.iter().copied().filter(|c| !self.can_draw(*c)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The content stream of one glyph's procedure.
    ///
    /// Reading works, which is spec 8.7's first sentence: the procedure is an
    /// ordinary content stream and `rasura-content` walks it like any other.
    pub fn procedure(&self, doc: &Document, font: &Dictionary, name: &str) -> Option<Vec<u8>> {
        let procs = doc.get_entry(font, "CharProcs").ok().flatten()?;
        let id = procs.as_dict()?.get(name).and_then(Object::as_reference)?;
        doc.decoded_stream(id).ok().map(|d| d.to_vec())
    }
}

/// Whether a font dictionary is Type 3.
pub fn is_type3(font: &Dictionary) -> bool {
    font.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str()) == Some("Type3")
}

/// `/FontMatrix`, which a Type 3 font must supply and whose scale is arbitrary.
///
/// Every other font format measures glyph space in 1/1000 em. A Type 3 font
/// says what its units are, and a reader that assumes the usual scale lays the
/// text out at the wrong size — often by a factor of a thousand.
pub fn font_matrix(doc: &Document, font: &Dictionary) -> Option<[f64; 6]> {
    let m = doc.get_entry(font, "FontMatrix").ok().flatten()?;
    let values: Vec<f64> = m
        .as_array()?
        .iter()
        .map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()).unwrap_or(0.0))
        .collect();
    values.get(..6).and_then(|v| v.try_into().ok())
}

/// The name a Type 3 edit refuses under. Spec 8.7's `EditError::Type3GlyphMissing`.
///
/// Defined here rather than in the edit layer because the condition is a
/// property of the font, and the edit layer will re-export it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type3GlyphMissing {
    pub codes: Vec<u8>,
}

impl std::fmt::Display for Type3GlyphMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the Type 3 font has no glyph procedure for code(s) ")?;
        for (i, c) in self.codes.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Type3GlyphMissing {}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn load(font_entries: &str, extra: &[(u32, &str)]) -> (Document, Dictionary) {
        let mut b = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(5, font_entries);
        for (id, body) in extra {
            b = b.stream(*id, "", body.as_bytes());
        }
        let bytes = b.finish("/Root 1 0 R");
        let doc = Document::open(bytes).expect("open");
        let font = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        (doc, font)
    }

    /// A Type 3 font with procedures for `square` and `triangle` at codes 97, 98.
    fn two_glyph_font() -> (Document, Dictionary) {
        load(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 750 750] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /square 6 0 R /triangle 7 0 R >> \
             /Encoding << /Type /Encoding /Differences [97 /square /triangle] >> \
             /FirstChar 97 /LastChar 98 /Widths [750 750] >>",
            &[(6, "750 0 0 0 750 750 d1\n0 0 750 750 re f\n"), (7, "750 0 d0\n0 0 m 750 0 l f\n")],
        )
    }

    #[test]
    fn a_type3_font_is_recognised() {
        let (_d, font) = two_glyph_font();
        assert!(is_type3(&font));
    }

    #[test]
    fn procedures_and_encoding_are_read() {
        let (doc, font) = two_glyph_font();
        let t3 = Type3::from_dict(&doc, &font);
        assert_eq!(t3.procedures.len(), 2);
        assert_eq!(t3.glyph_for(97), Some("square"));
        assert_eq!(t3.glyph_for(98), Some("triangle"));
        assert_eq!(t3.glyph_for(99), None);
    }

    #[test]
    fn reading_a_procedure_works() {
        // Spec 8.7's first sentence. The procedure is an ordinary content
        // stream, and it comes back as one.
        let (doc, font) = two_glyph_font();
        let t3 = Type3::from_dict(&doc, &font);
        let body = t3.procedure(&doc, &font, "square").expect("the procedure");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("d1"), "{text}");
        assert!(text.contains("re f"), "{text}");
        assert!(t3.procedure(&doc, &font, "nonexistent").is_none());
    }

    #[test]
    fn the_missing_codes_are_listed_not_merely_counted() {
        // Spec 8.7 wants the list. A caller told only "impossible" can do
        // nothing; one told which characters are missing can drop them, spell
        // them differently, or change fonts.
        let (doc, font) = two_glyph_font();
        let t3 = Type3::from_dict(&doc, &font);

        assert!(t3.missing(&[97, 98]).is_empty(), "both exist");
        assert_eq!(t3.missing(&[97, 65, 98, 66]), vec![65, 66]);
    }

    #[test]
    fn the_missing_list_is_ordered_and_deduplicated() {
        // The message a user sees must not depend on the order the text
        // happened to be scanned.
        let (doc, font) = two_glyph_font();
        let t3 = Type3::from_dict(&doc, &font);
        assert_eq!(t3.missing(&[70, 65, 70, 65, 66]), vec![65, 66, 70]);
    }

    #[test]
    fn a_code_named_but_without_a_procedure_cannot_be_drawn() {
        // /Differences promising a glyph the font never defines is a real
        // shape of broken font, and it must not read as "available".
        let (doc, font) = load(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 750 750] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /square 6 0 R >> \
             /Encoding << /Type /Encoding /Differences [97 /square /promised] >> >>",
            &[(6, "750 0 d0\n")],
        );
        let t3 = Type3::from_dict(&doc, &font);
        assert_eq!(t3.glyph_for(98), Some("promised"), "the name is there");
        assert!(!t3.can_draw(98), "but the procedure is not");
        assert_eq!(t3.missing(&[97, 98]), vec![98]);
    }

    #[test]
    fn a_font_with_no_encoding_can_draw_nothing() {
        // A Type 3 font has no built-in encoding: /Differences is the only way
        // its codes reach its procedures.
        let (doc, font) = load(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 750 750] \
             /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /square 6 0 R >> >>",
            &[(6, "750 0 d0\n")],
        );
        let t3 = Type3::from_dict(&doc, &font);
        assert_eq!(t3.procedures.len(), 1);
        assert!(t3.encoding.is_empty());
        assert_eq!(t3.missing(&[97]), vec![97]);
    }

    #[test]
    fn the_font_matrix_is_read_because_its_scale_is_arbitrary() {
        // Every other format is 1/1000 em. A reader assuming that here lays
        // the text out at the wrong size, often by a factor of a thousand.
        let (doc, font) = load(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] \
             /FontMatrix [1 0 0 1 0 0] /CharProcs << >> >>",
            &[],
        );
        assert_eq!(font_matrix(&doc, &font), Some([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));

        let (doc, font) = two_glyph_font();
        assert_eq!(font_matrix(&doc, &font), Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]));
    }

    #[test]
    fn the_refusal_names_the_codes() {
        let e = Type3GlyphMissing { codes: vec![65, 66] };
        let text = e.to_string();
        assert!(text.contains("65"), "{text}");
        assert!(text.contains("66"), "{text}");
    }
}
