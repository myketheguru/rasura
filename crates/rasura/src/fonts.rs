//! What fonts a document needs. Spec 11.3.
//!
//! > The browser cannot see system fonts. Make this explicit and pleasant.
//! >
//! > `fontRequirements()` run immediately after `open()` lets a consuming
//! > application fetch exactly the fonts it needs before the user starts
//! > typing. It turns the worst constraint of the platform into a solvable,
//! > visible task. **Lead with it in the docs.**
//!
//! So, leading with it: a browser has no font directory. Everything an editor
//! can draw, it must already have, and the file it is editing usually contains
//! only the letters it happened to use — a document that says "Hamburg" carries
//! seven glyphs and cannot type an eighth. That is not a deficiency in this
//! library; it is the platform, and pretending otherwise means an editor that
//! silently substitutes and produces a document that looks nearly right.
//!
//! [`survey`] is what makes it a task instead of a surprise. Called after
//! opening, it says which fonts are embedded, which are subset, and how much of
//! the Latin alphabet each can actually write — so an application can fetch the
//! three it needs before the cursor appears rather than discovering the problem
//! on the user's first keystroke.
//!
//! # What "coverage" is measured against
//!
//! Basic Latin, `U+0020`–`U+007E`. Not because that is what a document needs —
//! a Greek document needs Greek — but because it is the alphabet an editor's
//! *user* is most likely to type next, and a number has to mean something
//! specific to be worth reporting. [`FontInfo::writable`] carries the raw count
//! for anyone measuring against a different alphabet.

use rasura_content::font::{FontKind, LoadedFont};
use rasura_cos::object::{Dictionary, Object};
use rasura_cos::{Document, ObjId};
use std::collections::BTreeSet;

/// The alphabet [`FontInfo::latin_coverage`] is measured against.
const LATIN: std::ops::RangeInclusive<char> = ' '..='~';

/// How much of an alphabet a font can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every character. Editing is unconstrained.
    Full,
    /// Some. Editing works until the user types the wrong letter.
    Partial,
    /// None, or the font could not be read well enough to say.
    Unknown,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Coverage::Full => "full",
            Coverage::Partial => "partial",
            Coverage::Unknown => "unknown",
        }
    }
}

/// One font the document uses. Spec 11.3.
#[derive(Debug, Clone)]
pub struct FontInfo {
    /// `/BaseFont`, subset prefix and all — `ABCDEF+MinionPro-Regular`. Kept
    /// whole because the prefix is how two subsets of one typeface are told
    /// apart, and stripping it merges fonts that are genuinely different.
    pub name: String,
    /// The typeface name with any subset prefix removed, for matching against
    /// a font a caller might supply.
    pub family: String,
    /// Whether the program is in the file. A font that is not embedded cannot
    /// be edited with confidence: the reader substitutes something, and what
    /// the author saw is unknowable.
    pub embedded: bool,
    /// Whether the embedded program is a subset — the `ABCDEF+` prefix.
    pub subset: bool,
    /// Simple, composite, or Type 3.
    pub kind: FontKind,
    /// How much of Basic Latin this font can currently write.
    pub latin_coverage: Coverage,
    /// How many of Basic Latin's 95 characters are writable.
    pub writable: usize,
    /// True when the document's own text could not be resolved to Unicode,
    /// which usually means no `/ToUnicode` and no usable glyph names.
    pub text_unresolvable: bool,
}

impl FontInfo {
    /// Whether an application should offer to supply this font.
    ///
    /// The question §11.3 exists to answer. Yes when the font is missing
    /// outright, and yes when it is embedded but too incomplete to type into —
    /// the second case is the one that surprises people, because the font *is*
    /// there and still cannot write the alphabet.
    pub fn needs_supplying(&self) -> bool {
        !self.embedded || self.latin_coverage != Coverage::Full
    }
}

/// Every font the document declares, deduplicated. Spec 11.3.
///
/// Walks the object graph rather than the pages, so a font declared on page 400
/// is found without analysing 400 pages — which is what makes this callable
/// "immediately after `open()`" as the specification asks.
pub fn survey(doc: &Document) -> Vec<FontInfo> {
    let mut out: Vec<FontInfo> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut descendants: BTreeSet<ObjId> = BTreeSet::new();

    // A composite font's descendant is itself a font dictionary, and reporting
    // both makes one typeface look like two. Collected first so the second pass
    // can skip them wherever they appear in the graph.
    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, 0);
        let Ok(object) = doc.get(id) else { continue };
        let Some(dict) = object.as_dict() else { continue };
        if !is_font(dict) {
            continue;
        }
        if let Ok(Some(list)) = doc.get_entry(dict, "DescendantFonts")
            && let Some(array) = list.as_array()
        {
            descendants.extend(array.iter().filter_map(Object::as_reference));
        }
    }

    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, 0);
        if descendants.contains(&id) {
            continue;
        }
        let Ok(object) = doc.get(id) else { continue };
        let Some(dict) = object.as_dict() else { continue };
        if !is_font(dict) {
            continue;
        }

        let info = describe(doc, dict);
        // Two objects can be the same font: a page tree that repeats a
        // resource dictionary produces one font dictionary per page.
        if seen.insert(info.name.clone()) {
            out.push(info);
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn is_font(dict: &Dictionary) -> bool {
    dict.get("Type").and_then(Object::as_name).and_then(|n| n.as_str()) == Some("Font")
}

fn describe(doc: &Document, dict: &Dictionary) -> FontInfo {
    // With the standard-14 metrics supplied, because a document using Helvetica
    // carries no `/Widths` and is not thereby unable to write the alphabet. The
    // rest of the library measures with this source; a survey that did not
    // would report every unembedded standard font as covering nothing.
    let font = LoadedFont::load_with(doc, dict, Some(&rasura_layout::Standard14Widths));
    let name = dict
        .get("BaseFont")
        .and_then(Object::as_name)
        .and_then(|n| n.as_str())
        .unwrap_or("(unnamed)")
        .to_string();

    // ISO 32000-1 §9.6.4: a subset's name is six uppercase letters, a plus
    // sign, then the typeface name. Checked by shape rather than by looking for
    // '+', because a typeface may legitimately contain one.
    let subset = name.len() > 7
        && name.as_bytes()[6] == b'+'
        && name.as_bytes()[..6].iter().all(|b| b.is_ascii_uppercase());
    let family = if subset { name[7..].to_string() } else { name.clone() };

    let embedded = descriptor_of(doc, dict)
        .is_some_and(|d| ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.get(k).is_some()));

    // The embedded program decides, when there is one. Asking the encoding
    // instead reports a subset holding seven letters as covering all of Latin —
    // every character *has* a code under `/WinAnsiEncoding`, and whether an
    // outline sits at that code is a different question. `registerFont` exists
    // for exactly the gap that answer hides, so a survey that hid it would make
    // the feature undiscoverable.
    let writable = match embedded_program(doc, dict) {
        Some(program) => count_drawable(&program),
        None => count_writable(&font),
    };
    let total = LATIN.count();
    let latin_coverage = match writable {
        0 => Coverage::Unknown,
        w if w >= total => Coverage::Full,
        _ => Coverage::Partial,
    };

    FontInfo {
        name,
        family,
        embedded,
        subset,
        kind: font.kind,
        latin_coverage,
        writable,
        text_unresolvable: font.to_unicode.is_none() && font.kind == FontKind::Composite,
    }
}

/// The descriptor, following a composite font down to its descendant.
fn descriptor_of(doc: &Document, dict: &Dictionary) -> Option<Dictionary> {
    if let Ok(Some(d)) = doc.get_entry(dict, "FontDescriptor")
        && let Some(d) = d.as_dict()
    {
        return Some(d.clone());
    }
    let list = doc.get_entry(dict, "DescendantFonts").ok()??;
    let first = list.as_array()?.first()?;
    let descendant = doc.resolve(first).ok()?;
    let descendant = descendant.as_dict()?;
    let d = doc.get_entry(descendant, "FontDescriptor").ok()??;
    d.as_dict().cloned()
}

/// The embedded TrueType program, if the font carries one.
fn embedded_program(doc: &Document, dict: &Dictionary) -> Option<Vec<u8>> {
    let descriptor = descriptor_of(doc, dict)?;
    let id = descriptor.get("FontFile2")?.as_reference()?;
    doc.decoded_stream(id).ok().map(|b| b.to_vec())
}

/// How many Basic Latin characters the embedded program can actually draw.
///
/// Asked of the font's own `cmap`, which is the same question
/// [`crate::supply::missing_glyphs`] asks and the reason both exist: a glyph
/// the program does not contain cannot be drawn however many dictionaries
/// describe it.
fn count_drawable(program: &[u8]) -> usize {
    let Ok(font) = rasura_font::Sfnt::parse(program) else { return 0 };
    let Some(cmap) = rasura_font::Cmap::parse(program, &font) else { return 0 };
    let Some(table) = cmap.best_unicode() else { return 0 };
    LATIN.filter(|c| table.lookup(program, *c as u32).is_some_and(|gid| gid != 0)).count()
}

/// How many Basic Latin characters this font can currently write.
///
/// Measured by asking the font for a width, which is the same question the
/// editor asks when it lays text out: a character the font has no metric for is
/// one it cannot place, whatever its program contains. That is deliberately
/// stricter than "is there an outline" — a glyph with no width is not usable
/// even when it exists.
fn count_writable(font: &LoadedFont) -> usize {
    let mut n = 0usize;
    for c in LATIN {
        let code = c as u32;
        // Simple fonts are indexed by byte code; a composite font's codes come
        // from its CMap and a bare character number is not one of them, so this
        // measures what the *encoding* reaches rather than what the program
        // holds.
        let bytes = if font.kind == FontKind::Composite {
            (code as u16).to_be_bytes().to_vec()
        } else {
            vec![code as u8]
        };
        let units = font.decode(&bytes);
        if units.iter().all(|u| font.width(u).is_some()) && !units.is_empty() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn doc_with(fonts: &[(u32, &str)]) -> Document {
        let mut b = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        for (n, body) in fonts {
            b = b.object(*n, body);
        }
        Document::open(b.finish("/Root 1 0 R")).expect("open")
    }

    #[test]
    fn a_standard_font_is_not_embedded_and_covers_latin() {
        let doc = doc_with(&[(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        )]);
        let fonts = survey(&doc);
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].name, "Helvetica");
        assert!(!fonts[0].embedded);
        assert!(!fonts[0].subset);
        assert_eq!(fonts[0].latin_coverage, Coverage::Full, "{:?}", fonts[0]);
        // Not embedded, so an application should still offer to supply it: the
        // reader will substitute something and what the author saw is unknown.
        assert!(fonts[0].needs_supplying());
    }

    #[test]
    fn a_subset_prefix_is_recognised_and_the_family_recovered() {
        let doc = doc_with(&[(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+MinionPro-Regular >>",
        )]);
        let fonts = survey(&doc);
        assert!(fonts[0].subset, "{:?}", fonts[0]);
        assert_eq!(fonts[0].family, "MinionPro-Regular");
        assert_eq!(fonts[0].name, "ABCDEF+MinionPro-Regular");
    }

    #[test]
    fn a_plus_sign_that_is_not_a_subset_prefix_is_left_alone() {
        // Checked by shape rather than by looking for '+', because a typeface
        // name may legitimately contain one.
        let doc = doc_with(&[(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Foo+Bar >>")]);
        let fonts = survey(&doc);
        assert!(!fonts[0].subset, "{:?}", fonts[0]);
        assert_eq!(fonts[0].family, "Foo+Bar");
    }

    #[test]
    fn a_composite_fonts_descendant_is_not_reported_as_a_second_font() {
        // One typeface must not look like two.
        let doc = doc_with(&[
            (
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /Probe /Encoding /Identity-H \
                 /DescendantFonts [6 0 R] >>",
            ),
            (6, "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Probe /DW 500 >>"),
        ]);
        let fonts = survey(&doc);
        assert_eq!(fonts.len(), 1, "{fonts:#?}");
        assert_eq!(fonts[0].kind, FontKind::Composite);
    }

    #[test]
    fn an_embedded_program_is_reported_as_embedded() {
        let doc = doc_with(&[
            (
                5,
                "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Probe \
                 /FontDescriptor 6 0 R >>",
            ),
            (
                6,
                "<< /Type /FontDescriptor /FontName /ABCDEF+Probe /Flags 4 \
                 /FontFile2 7 0 R >>",
            ),
        ]);
        let fonts = survey(&doc);
        assert!(fonts[0].embedded, "{:?}", fonts[0]);
    }

    #[test]
    fn a_document_with_no_fonts_surveys_empty() {
        let doc = doc_with(&[]);
        assert!(survey(&doc).is_empty());
    }
}
