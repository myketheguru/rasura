//! Font metrics as the *PDF* describes them. ISO 32000-1 §9.6 and §9.7.
//!
//! Positioning needs three things from a font, and all three are available from
//! the font dictionary without parsing a single byte of the embedded font
//! program:
//!
//! 1. how to split a string into character codes (`/Encoding` CMap),
//! 2. what CID each code maps to,
//! 3. how wide each glyph is (`/Widths`, or `/W` and `/DW`).
//!
//! That is deliberate: the font *engine* is Phase 4, and Phase 2 must not need
//! it. What this module cannot supply, it reports rather than guesses -- a
//! non-embedded standard-14 font with no `/Widths` has no metrics here, and
//! `missing_widths` says so instead of silently returning zero and stacking
//! every glyph on the same spot.

use crate::cmap::{CMap, encoding_cmap};
use crate::matrix::Matrix;
use rasura_cos::document::Document;
use rasura_cos::{Dictionary, Object};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    /// Type1, MMType1, TrueType: single-byte codes.
    Simple,
    /// Type0: codes come from the `/Encoding` CMap.
    Composite,
    /// Type3: glyphs are content streams and widths are in glyph space.
    Type3,
}

#[derive(Debug, Clone)]
enum Widths {
    Simple {
        first_char: i64,
        widths: Vec<f64>,
        missing: f64,
    },
    /// `/W` and `/DW` on the descendant font, keyed by CID.
    Cid {
        default: f64,
        singles: HashMap<u32, f64>,
        ranges: Vec<(u32, u32, f64)>,
    },
    /// Supplied by a higher layer for a font whose dictionary carries no
    /// metrics, keyed by character code.
    Supplied(HashMap<u32, f64>),
    None,
}

/// Metrics for a font whose dictionary supplies none.
///
/// A PDF may name `/Helvetica` and embed nothing, on the reasoning that every
/// viewer has one — and then omit `/Widths` too, because the metrics of the
/// standard 14 are part of the standard. Without them every advance is a guess
/// and every line ends in the wrong place.
///
/// This layer cannot resolve that itself. The metrics live in
/// `rasura-font`, which sits *above* the content layer, and reaching
/// upwards would invert the dependency the whole workspace is arranged around.
/// So the content layer states what it needs and a higher layer supplies it.
///
/// The supplier is handed the font dictionary rather than a name, because
/// turning a character code into a width needs the encoding — `/Differences`,
/// a base encoding, or the font's own — and that is more than a name conveys.
pub trait WidthSource {
    /// Widths by character code, in glyph space (1/1000 em), or `None` if the
    /// supplier does not recognise the font.
    fn widths_for(&self, doc: &Document, font: &Dictionary) -> Option<HashMap<u32, f64>>;
}

/// A font as far as the content layer needs it.
#[derive(Debug, Clone)]
pub struct LoadedFont {
    pub kind: FontKind,
    pub base_font: String,
    /// How codes are read and mapped to CIDs.
    pub cmap: CMap,
    /// `/ToUnicode`, when present. §7.2's chain builds on this; here it is the
    /// only strategy, and its absence is reported rather than worked around.
    pub to_unicode: Option<CMap>,
    /// `/FontMatrix`, for Type 3 fonts whose glyph space is not 1/1000.
    pub font_matrix: Option<Matrix>,
    /// True when no width information could be found at all.
    pub missing_widths: bool,
    /// True when the widths came from a [`WidthSource`] rather than from the
    /// file. Reported because a substituted metric is a fidelity claim, and
    /// spec §2 requires those to be visible.
    pub supplied_widths: bool,
    /// True when the `/Encoding` CMap had to be approximated.
    pub approximate_cmap: bool,
    widths: Widths,
}

impl LoadedFont {
    /// Read what the content layer needs from a font dictionary.
    pub fn load(doc: &Document, font: &Dictionary) -> LoadedFont {
        Self::load_with(doc, font, None)
    }

    /// As [`load`](Self::load), consulting a [`WidthSource`] when the
    /// dictionary carries no metrics.
    ///
    /// The source is asked *only* on that path. A font that supplies its own
    /// `/Widths` keeps them even where they disagree with the standard
    /// metrics — the file is the authority on its own layout, and overriding it
    /// would move text that renders correctly today.
    pub fn load_with(
        doc: &Document,
        font: &Dictionary,
        source: Option<&dyn WidthSource>,
    ) -> LoadedFont {
        let subtype = font.get("Subtype").and_then(Object::as_name).map(|n| n.as_bytes().to_vec());
        let kind = match subtype.as_deref() {
            Some(b"Type0") => FontKind::Composite,
            Some(b"Type3") => FontKind::Type3,
            _ => FontKind::Simple,
        };
        let base_font = font
            .get("BaseFont")
            .and_then(Object::as_name)
            .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
            .unwrap_or_default();

        let to_unicode = font
            .get("ToUnicode")
            .and_then(Object::as_reference)
            .and_then(|id| doc.decoded_stream(id).ok())
            .map(|data| CMap::parse(&data))
            .filter(|c| c.unicode_entries() > 0);

        // `encoding_cmap` reports whether the map is *exact*; the field records
        // the opposite. A simple font's single-byte codespace is exact by
        // definition, so only composite fonts can be approximate.
        let (cmap, approximate_cmap) = match kind {
            FontKind::Composite => {
                let (c, exact) = encoding_cmap(doc, font);
                (c, !exact)
            }
            _ => (CMap::single_byte(), false),
        };

        let font_matrix = if kind == FontKind::Type3 {
            doc.get_entry(font, "FontMatrix")
                .ok()
                .flatten()
                .and_then(|m| m.as_array().and_then(Matrix::from_array))
        } else {
            None
        };

        let mut widths = match kind {
            FontKind::Composite => Self::load_cid_widths(doc, font),
            _ => Self::load_simple_widths(doc, font),
        };

        // Only a simple font can be rescued this way. A composite font's codes
        // run through a CMap to CIDs that mean nothing outside the font, so
        // there is no standard table to fall back on.
        let mut supplied_widths = false;
        if matches!(widths, Widths::None)
            && kind == FontKind::Simple
            && let Some(source) = source
            && let Some(table) = source.widths_for(doc, font)
            && !table.is_empty()
        {
            widths = Widths::Supplied(table);
            supplied_widths = true;
        }
        let missing_widths = matches!(widths, Widths::None);

        LoadedFont {
            kind,
            base_font,
            cmap,
            to_unicode,
            font_matrix,
            missing_widths,
            supplied_widths,
            approximate_cmap,
            widths,
        }
    }

    fn load_simple_widths(doc: &Document, font: &Dictionary) -> Widths {
        let Ok(Some(array)) = doc.get_entry(font, "Widths") else { return Widths::None };
        let Some(items) = array.as_array() else { return Widths::None };
        if items.is_empty() {
            return Widths::None;
        }
        let widths: Vec<f64> = items
            .iter()
            .map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()).unwrap_or(0.0))
            .collect();
        let first_char =
            doc.get_entry(font, "FirstChar").ok().flatten().and_then(|v| v.as_i64()).unwrap_or(0);
        let missing = doc
            .get_entry(font, "FontDescriptor")
            .ok()
            .flatten()
            .and_then(|d| d.as_dict().and_then(|d| d.get("MissingWidth")).and_then(Object::as_f64))
            .unwrap_or(0.0);
        Widths::Simple { first_char, widths, missing }
    }

    /// `/W` is an array of either `c [w1 w2 ...]` or `cFirst cLast w`.
    fn load_cid_widths(doc: &Document, font: &Dictionary) -> Widths {
        let Some(descendant) = descendant(doc, font) else { return Widths::None };
        let default = doc
            .get_entry(&descendant, "DW")
            .ok()
            .flatten()
            .and_then(|v| v.as_f64())
            // ISO 32000-1 §9.7.4.3: /DW defaults to 1000.
            .unwrap_or(1000.0);

        let mut singles = HashMap::new();
        let mut ranges = Vec::new();

        if let Ok(Some(w)) = doc.get_entry(&descendant, "W")
            && let Some(items) = w.as_array()
        {
            let resolved: Vec<Object> = items
                .iter()
                .map(|o| doc.resolve(o).map(|a| (*a).clone()).unwrap_or(Object::Null))
                .collect();
            let mut i = 0usize;
            while i < resolved.len() {
                let Some(first) = resolved[i].as_f64() else {
                    i += 1;
                    continue;
                };
                match resolved.get(i + 1) {
                    Some(Object::Array(list)) => {
                        for (k, item) in list.iter().enumerate() {
                            if let Some(w) = item.as_f64() {
                                singles.insert(first as u32 + k as u32, w);
                            }
                        }
                        i += 2;
                    }
                    Some(second) => {
                        let last = second.as_f64().unwrap_or(first);
                        let w = resolved.get(i + 2).and_then(Object::as_f64).unwrap_or(default);
                        if last >= first {
                            ranges.push((first as u32, last as u32, w));
                        }
                        i += 3;
                    }
                    None => break,
                }
            }
        }
        Widths::Cid { default, singles, ranges }
    }

    /// Split a string into `(code, cid, byte offset, byte length)`.
    pub fn decode(&self, bytes: &[u8]) -> Vec<CodeUnit> {
        self.cmap
            .codes(bytes)
            .into_iter()
            .map(|(code, offset, len)| CodeUnit { code, cid: self.cmap.cid(code), offset, len })
            .collect()
    }

    /// Glyph width in **text space** -- already divided by 1000 for simple and
    /// CID fonts, and through `/FontMatrix` for Type 3.
    ///
    /// `None` when the font supplies no metrics, so the caller can report a
    /// degraded position rather than silently placing every glyph at zero.
    pub fn width(&self, unit: &CodeUnit) -> Option<f64> {
        let raw = match &self.widths {
            Widths::Simple { first_char, widths, missing } => {
                let idx = unit.code as i64 - first_char;
                match usize::try_from(idx).ok().and_then(|i| widths.get(i)) {
                    Some(w) => *w,
                    None => *missing,
                }
            }
            Widths::Cid { default, singles, ranges } => {
                if let Some(w) = singles.get(&unit.cid) {
                    *w
                } else {
                    ranges
                        .iter()
                        .find(|(lo, hi, _)| unit.cid >= *lo && unit.cid <= *hi)
                        .map(|(_, _, w)| *w)
                        .unwrap_or(*default)
                }
            }
            Widths::Supplied(table) => *table.get(&unit.code)?,
            Widths::None => return None,
        };

        Some(match (self.kind, self.font_matrix) {
            // Type 3 glyph space is whatever /FontMatrix says.
            (FontKind::Type3, Some(m)) => raw * m.a,
            (FontKind::Type3, None) => raw * 0.001,
            _ => raw * 0.001,
        })
    }

    /// The text a code maps to, via `/ToUnicode` only.
    ///
    /// This is §7.2 *strategy 1* and nothing else. When it returns `None` the
    /// answer is "this layer does not know", not "there is no text" -- the
    /// remaining six strategies are `rasura-layout`'s job.
    pub fn unicode(&self, unit: &CodeUnit) -> Option<&str> {
        self.to_unicode.as_ref()?.unicode(unit.code)
    }

    /// Vertical writing mode.
    pub fn is_vertical(&self) -> bool {
        self.cmap.wmode == 1
    }
}

/// One character code read out of a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeUnit {
    pub code: u32,
    pub cid: u32,
    /// Byte offset within the showing operator's string.
    pub offset: usize,
    /// How many bytes the code occupied. Word spacing applies only when this is
    /// 1 and the code is 32.
    pub len: usize,
}

fn descendant(doc: &Document, font: &Dictionary) -> Option<Dictionary> {
    let arr = doc.get_entry(font, "DescendantFonts").ok()??;
    let first = arr.as_array()?.first()?.clone();
    doc.resolve(&first).ok()?.as_dict().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// Widths go through a division by 1000, so exact equality is the wrong
    /// comparison: 700/1000 is not representable.
    fn width_is(got: Option<f64>, want: f64) -> bool {
        got.is_some_and(|g| (g - want).abs() < 1e-9)
    }

    fn load(objects: &[(u32, &str)], font_obj: u32) -> (Document, LoadedFont) {
        let mut b = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R >>");
        for (n, body) in objects {
            b = b.object(*n, body);
        }
        let doc = Document::open(b.finish("/Root 1 0 R")).unwrap();
        let dict = doc.get(rasura_cos::ObjId::new(font_obj, 0)).unwrap();
        let font = LoadedFont::load(&doc, dict.as_dict().unwrap());
        (doc, font)
    }

    #[test]
    fn simple_font_widths_are_indexed_from_firstchar() {
        let (_d, f) = load(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                  /FirstChar 65 /LastChar 67 /Widths [500 600 700] >>",
            )],
            5,
        );
        assert_eq!(f.kind, FontKind::Simple);
        assert!(!f.missing_widths);
        let units = f.decode(b"ABC");
        assert_eq!(units.len(), 3);
        assert!(width_is(f.width(&units[0]), 0.5));
        assert!(width_is(f.width(&units[1]), 0.6));
        assert!(width_is(f.width(&units[2]), 0.7));
    }

    #[test]
    fn a_code_outside_widths_uses_missingwidth() {
        let (_d, f) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type1 /FirstChar 65 /LastChar 65 \
                      /Widths [500] /FontDescriptor 6 0 R >>",
                ),
                (6, "<< /Type /FontDescriptor /MissingWidth 250 >>"),
            ],
            5,
        );
        let units = f.decode(b"AZ");
        assert!(width_is(f.width(&units[0]), 0.5));
        assert!(width_is(f.width(&units[1]), 0.25));
    }

    #[test]
    fn a_font_with_no_widths_reports_it_rather_than_returning_zero() {
        // A non-embedded standard-14 font. Its metrics come from AFM data,
        // which is Phase 4. Saying so beats stacking every glyph at x=0.
        let (_d, f) = load(&[(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")], 5);
        assert!(f.missing_widths);
        assert_eq!(f.width(&f.decode(b"A")[0]), None);
    }

    #[test]
    fn composite_font_reads_two_byte_codes_and_w_widths() {
        let (_d, f) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H \
                      /DescendantFonts [6 0 R] >>",
                ),
                (
                    6,
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /DW 1000 \
                      /W [1 [500 600] 10 20 750] >>",
                ),
            ],
            5,
        );
        assert_eq!(f.kind, FontKind::Composite);
        let units = f.decode(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x0f, 0x00, 0xff]);
        assert_eq!(units.len(), 4);
        // `c [w1 w2]` form.
        assert!(width_is(f.width(&units[0]), 0.5));
        assert!(width_is(f.width(&units[1]), 0.6));
        // `cFirst cLast w` form.
        assert!(width_is(f.width(&units[2]), 0.75));
        // Falls back to /DW.
        assert!(width_is(f.width(&units[3]), 1.0));
    }

    #[test]
    fn dw_defaults_to_1000_when_absent() {
        let (_d, f) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /Encoding /Identity-H \
                      /DescendantFonts [6 0 R] >>",
                ),
                (6, "<< /Type /Font /Subtype /CIDFontType2 >>"),
            ],
            5,
        );
        assert!(width_is(f.width(&f.decode(&[0x00, 0x41])[0]), 1.0));
    }

    #[test]
    fn type3_widths_go_through_the_font_matrix() {
        // Glyph space for a Type 3 font is whatever /FontMatrix says, not 1/1000.
        let (_d, f) = load(
            &[(
                5,
                "<< /Type /Font /Subtype /Type3 /FontMatrix [0.01 0 0 0.01 0 0] \
                  /FirstChar 97 /LastChar 97 /Widths [50] /CharProcs << >> >>",
            )],
            5,
        );
        assert_eq!(f.kind, FontKind::Type3);
        assert!(width_is(f.width(&f.decode(b"a")[0]), 0.5));
    }

    #[test]
    fn code_units_carry_their_byte_length_for_the_word_spacing_rule() {
        let (_d, simple) =
            load(&[(5, "<< /Type /Font /Subtype /Type1 /FirstChar 32 /Widths [250] >>")], 5);
        let u = simple.decode(b" ")[0];
        assert_eq!((u.code, u.len), (32, 1));
        assert!(crate::word_spacing_applies(u.code, u.len));

        let (_d, composite) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /Encoding /Identity-H \
                      /DescendantFonts [6 0 R] >>",
                ),
                (6, "<< /Type /Font /Subtype /CIDFontType2 >>"),
            ],
            5,
        );
        let u = composite.decode(&[0x00, 0x20])[0];
        assert_eq!((u.code, u.len), (32, 2));
        assert!(!crate::word_spacing_applies(u.code, u.len), "code 32 in two bytes is not a space");
    }

    #[test]
    fn vertical_encoding_is_detected() {
        let (_d, f) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /Encoding /Identity-V \
                      /DescendantFonts [6 0 R] >>",
                ),
                (6, "<< /Type /Font /Subtype /CIDFontType2 >>"),
            ],
            5,
        );
        assert!(f.is_vertical());
    }

    #[test]
    fn an_approximated_cmap_is_flagged() {
        let (_d, f) = load(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /Encoding /UniJIS-UCS2-H \
                      /DescendantFonts [6 0 R] >>",
                ),
                (6, "<< /Type /Font /Subtype /CIDFontType0 >>"),
            ],
            5,
        );
        assert!(f.approximate_cmap, "a predefined collection CMap is not exact here");
    }

    #[test]
    fn tounicode_is_read_when_present_and_absent_otherwise() {
        let cmap = b"1 beginbfchar\n<41> <0041>\nendbfchar";
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R >>")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /FirstChar 65 /Widths [500] /ToUnicode 6 0 R >>",
            )
            .stream(6, "", cmap)
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let dict = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap();
        let f = LoadedFont::load(&doc, dict.as_dict().unwrap());
        assert_eq!(f.unicode(&f.decode(b"A")[0]), Some("A"));
        assert_eq!(f.unicode(&f.decode(b"B")[0]), None, "unmapped is None, not a guess");
    }
}
