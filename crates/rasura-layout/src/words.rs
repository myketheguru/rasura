//! Word segmentation. Spec 7.3.
//!
//! A PDF has no words. It has positioned glyphs, and frequently no space
//! characters at all -- a typesetter can produce the visual gap between words
//! by moving the pen instead, and many do. So segmentation is geometry plus
//! whatever character evidence happens to exist.
//!
//! Spec 7.3 gives four boundary conditions:
//!
//! 1. an explicit space glyph,
//! 2. a `TJ` negative adjustment beyond a threshold,
//! 3. a positional gap beyond the expected advance,
//! 4. a `Tm`/`Td` that moves the pen non-monotonically.
//!
//! Conditions 2 and 3 are the same measurement here. A `TJ` adjustment moves
//! the pen, and the pen position is what this sees, so a gap test on device
//! coordinates catches both -- and catches them for producers that use `Td`
//! rather than `TJ`, which the adjustment test alone would miss.

use crate::lines::{Line, PlacedGlyph};
use crate::script;
use rasura_content::matrix::Rect;
use std::ops::Range;

/// A gap wider than this fraction of the font size, beyond the expected
/// advance, is a word boundary. Spec 7.3 gives 0.25.
const GAP_THRESHOLD: f64 = 0.25;

/// A backwards jump beyond this fraction of the font size is a non-monotonic
/// pen move. Small negative gaps are kerning, not new words.
const BACKWARD_THRESHOLD: f64 = 0.5;

/// One word within a line.
#[derive(Debug, Clone)]
pub struct Word {
    /// Which glyphs of the line, as a half-open range.
    pub glyphs: Range<usize>,
    pub text: String,
    pub bbox: Rect,
    /// Where the word starts and ends along the line's baseline.
    pub start: f64,
    pub end: f64,
    /// Why the word began. Useful for diagnosing a document that segments
    /// badly, and for spec 7.6's hyphenation detection later.
    pub reason: BoundaryReason,
}

impl Word {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn width(&self) -> f64 {
        self.end - self.start
    }
}

/// What caused a word boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryReason {
    /// The first word of the line.
    LineStart,
    /// An explicit space character.
    SpaceGlyph,
    /// A gap wider than the expected advance. Covers both `TJ` adjustments and
    /// `Td` moves.
    PositionalGap,
    /// The pen moved backwards, so the producer restarted rather than continued.
    NonMonotonic,
}

/// Segment a line into words.
///
/// Returns one word covering everything for scripts that do not separate words
/// with spaces. Spec 7.3: "do not segment; treat the run as a single word and
/// let the shaper handle it." Applying a gap rule to Thai or Japanese invents
/// boundaries that are not there, which is worse than declining to guess.
pub fn segment(line: &Line) -> Vec<Word> {
    if line.glyphs.is_empty() {
        return Vec::new();
    }

    let dominant = script::dominant(&line.text());
    if !dominant.segments_on_geometry() {
        return vec![build_word(line, 0..line.glyphs.len(), BoundaryReason::LineStart)];
    }

    let mut words = Vec::new();
    let mut start = 0usize;
    let mut reason = BoundaryReason::LineStart;

    for i in 0..line.glyphs.len() {
        let g = &line.glyphs[i];

        // An explicit space ends the current word and is not part of any word.
        if is_space_glyph(g) {
            if i > start {
                words.push(build_word(line, start..i, reason));
            }
            start = i + 1;
            reason = BoundaryReason::SpaceGlyph;
            continue;
        }

        if i == start {
            continue;
        }

        if let Some(cause) = boundary_before(line, i)
            && i > start
        {
            words.push(build_word(line, start..i, reason));
            start = i;
            reason = cause;
        }
    }

    if start < line.glyphs.len() {
        words.push(build_word(line, start..line.glyphs.len(), reason));
    }
    words.retain(|w| !w.is_empty() || !w.glyphs.is_empty());
    words
}

/// Whether a boundary falls immediately before glyph `i`.
fn boundary_before(line: &Line, i: usize) -> Option<BoundaryReason> {
    let prev = &line.glyphs[i - 1];
    let g = &line.glyphs[i];

    // Distance the pen actually moved, along the baseline.
    let moved = g.tangent - prev.tangent;
    // What it would have moved had the glyphs been adjacent. `advance` already
    // includes `Tc`, `Tw` and `Tz`, because it came from the displacement
    // formula the state machine applied.
    let expected = prev.advance;
    let gap = moved - expected;

    let size = prev.size.max(g.size).max(1.0);

    if moved < -BACKWARD_THRESHOLD * size {
        // The pen went backwards far enough that this is a fresh start, not
        // kerning. Overlapping text -- a strikethrough drawn as characters, a
        // second pass for boldness -- lands here.
        return Some(BoundaryReason::NonMonotonic);
    }
    if gap > GAP_THRESHOLD * size {
        return Some(BoundaryReason::PositionalGap);
    }
    None
}

/// A glyph that resolved to whitespace. Unresolved glyphs are never treated as
/// spaces: `None` means the chain could not read them, not that they are blank.
fn is_space_glyph(g: &PlacedGlyph) -> bool {
    g.text.as_deref().is_some_and(|t| !t.is_empty() && t.chars().all(script::is_space))
}

fn build_word(line: &Line, range: Range<usize>, reason: BoundaryReason) -> Word {
    let glyphs = &line.glyphs[range.clone()];
    let text: String = glyphs
        .iter()
        .filter_map(|g| g.text.as_deref())
        // Zero-width joiners and soft hyphens carry no text of their own.
        .filter(|t| !t.chars().all(script::is_invisible))
        .collect();

    let start = glyphs.first().map(|g| g.tangent).unwrap_or(0.0);
    let end = glyphs.last().map(|g| g.tangent + g.advance).unwrap_or(start);

    let mut bbox = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for g in glyphs {
        let (sin, cos) = g.direction.sin_cos();
        let ascent = g.size * 0.75;
        let descent = g.size * 0.25;
        for (dt, dn) in [(0.0, -ascent), (g.advance, -ascent), (0.0, descent), (g.advance, descent)]
        {
            let x = g.origin.x + dt * cos - dn * sin;
            let y = g.origin.y + dt * sin + dn * cos;
            bbox.x0 = bbox.x0.min(x);
            bbox.y0 = bbox.y0.min(y);
            bbox.x1 = bbox.x1.max(x);
            bbox.y1 = bbox.y1.max(y);
        }
    }
    if glyphs.is_empty() {
        bbox = Rect::default();
    }

    Word { glyphs: range, text, bbox, start, end, reason }
}

/// A line's text with word boundaries made explicit as spaces.
///
/// This is what a caller wanting readable text wants, as against `Line::text`,
/// which is the glyphs as the producer emitted them and often has no spaces at
/// all.
pub fn line_text(line: &Line) -> String {
    let words = segment(line);
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 && !w.text.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&w.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{assemble, place};
    use crate::resolve_page;
    use crate::script::Script;
    use rasura_content::page;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    /// Every glyph 500/1000 wide, so at 10pt every advance is exactly 5pt and
    /// the geometry in these tests is exact rather than approximate.
    fn page_with(content: &str) -> Vec<u8> {
        let widths = "500 ".repeat(95);
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                      /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{widths}] >>"
                ),
            )
            .finish("/Root 1 0 R")
    }

    fn first_line(content: &str) -> Line {
        let doc = Document::open(page_with(content)).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        assemble(place(&runs)).remove(0)
    }

    fn words_of(content: &str) -> Vec<String> {
        segment(&first_line(content)).iter().map(|w| w.text.clone()).collect()
    }

    #[test]
    fn explicit_spaces_separate_words() {
        assert_eq!(
            words_of("BT /F1 10 Tf 72 700 Td (one two three) Tj ET"),
            vec!["one", "two", "three"]
        );
    }

    #[test]
    fn the_space_glyph_belongs_to_no_word() {
        let line = first_line("BT /F1 10 Tf 72 700 Td (ab cd) Tj ET");
        let words = segment(&line);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].glyphs, 0..2);
        assert_eq!(words[1].glyphs, 3..5, "the space at index 2 is in neither word");
    }

    #[test]
    fn a_positional_gap_separates_words_with_no_space_character() {
        // The case that makes segmentation necessary: the producer moved the
        // pen instead of emitting a space. 5pt advance plus a 4pt gap.
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (one) Tj 1 0 0 1 96 700 Tm (two) Tj ET";
        assert_eq!(words_of(content), vec!["one", "two"]);
    }

    #[test]
    fn a_tj_adjustment_wide_enough_to_be_a_space_separates_words() {
        // -500/1000 x 10pt = 5pt of extra space, well beyond 0.25 x 10.
        assert_eq!(words_of("BT /F1 10 Tf 72 700 Td [(one) -500 (two)] TJ ET"), vec!["one", "two"]);
    }

    #[test]
    fn ordinary_kerning_does_not_split_a_word() {
        // Spec 7.3 warns about this: -200 adjustments are routine kerning, not
        // word boundaries. At 10pt that is 2pt, under the 2.5pt threshold.
        assert_eq!(words_of("BT /F1 10 Tf 72 700 Td [(Wa) -200 (ter)] TJ ET"), vec!["Water"]);
    }

    #[test]
    fn the_threshold_scales_with_font_size() {
        // The same -200 adjustment at 40pt is 8pt, which is a real gap; the
        // threshold is 10pt, so it still is not a boundary. At -400 it is 16pt
        // and clearly is.
        assert_eq!(words_of("BT /F1 40 Tf 72 700 Td [(Wa) -200 (ter)] TJ ET"), vec!["Water"]);
        assert_eq!(words_of("BT /F1 40 Tf 72 700 Td [(Wa) -400 (ter)] TJ ET"), vec!["Wa", "ter"]);
    }

    #[test]
    fn a_backwards_pen_move_starts_a_new_word() {
        // Overlapping text: a second pass drawn over the first, which some
        // producers use for faux-bold.
        let content = "BT /F1 10 Tf 1 0 0 1 100 700 Tm (bbb) Tj 1 0 0 1 72 700 Tm (aaa) Tj ET";
        let line = first_line(content);
        let words = segment(&line);
        // Sorted visually, "aaa" comes first, then a gap to "bbb".
        assert_eq!(words.iter().map(|w| w.text.clone()).collect::<Vec<_>>(), vec!["aaa", "bbb"]);
    }

    #[test]
    fn a_boundary_records_why_it_happened() {
        let line = first_line("BT /F1 10 Tf 72 700 Td (a b) Tj ET");
        let words = segment(&line);
        assert_eq!(words[0].reason, BoundaryReason::LineStart);
        assert_eq!(words[1].reason, BoundaryReason::SpaceGlyph);

        let line = first_line("BT /F1 10 Tf 72 700 Td [(a) -900 (b)] TJ ET");
        let words = segment(&line);
        assert_eq!(words[1].reason, BoundaryReason::PositionalGap);
    }

    #[test]
    fn cjk_is_not_segmented_on_geometry() {
        // Spec 7.3: treat the run as a single word. CJK has no inter-word
        // spacing, so every gap rule would invent boundaries.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 72 700 Td <00010002000300040005> Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H \
                  /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>",
            )
            .object(6, "<< /Type /Font /Subtype /CIDFontType2 /DW 1000 >>")
            .stream(
                7,
                "",
                b"1 begincodespacerange\n<0000> <ffff>\nendcodespacerange\n\
                  1 beginbfrange\n<0001> <0005> <65e5>\nendbfrange",
            )
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        let line = assemble(place(&runs)).remove(0);
        assert_eq!(script::dominant(&line.text()), Script::CjkIdeographic);

        let words = segment(&line);
        assert_eq!(words.len(), 1, "CJK stays one word: {:?}", words);
        assert_eq!(words[0].glyphs, 0..5);
    }

    #[test]
    fn word_extents_and_boxes_are_sane() {
        let line = first_line("BT /F1 10 Tf 72 700 Td (ab cd) Tj ET");
        let words = segment(&line);
        // Two glyphs at 5pt each.
        assert!((words[0].width() - 10.0).abs() < 1e-6, "{}", words[0].width());
        assert!(words[0].end <= words[1].start, "words do not overlap");
        assert!(words[0].bbox.x1 <= words[1].bbox.x0 + 1e-6);
    }

    #[test]
    fn line_text_inserts_the_spaces_the_producer_omitted() {
        // The whole point: the glyphs carry no space, the geometry does.
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (Hello) Tj 1 0 0 1 110 700 Tm (world) Tj ET";
        let line = first_line(content);
        assert_eq!(line.text(), "Helloworld", "no space in the glyph stream");
        assert_eq!(line_text(&line), "Hello world");
    }

    #[test]
    fn line_text_does_not_double_a_space_that_is_already_there() {
        let line = first_line("BT /F1 10 Tf 72 700 Td (one two) Tj ET");
        assert_eq!(line_text(&line), "one two");
    }

    #[test]
    fn an_empty_line_segments_to_nothing() {
        let line = Line {
            glyphs: Vec::new(),
            baseline: 0.0,
            direction: 0.0,
            size: 10.0,
            bbox: Rect::default(),
        };
        assert!(segment(&line).is_empty());
        assert_eq!(line_text(&line), "");
    }

    #[test]
    fn runs_of_spaces_do_not_produce_empty_words() {
        let words = words_of("BT /F1 10 Tf 72 700 Td (a   b) Tj ET");
        assert_eq!(words, vec!["a", "b"], "collapsed, not padded with empties");
    }

    #[test]
    fn unmapped_glyphs_are_not_mistaken_for_spaces() {
        // `None` means the chain could not read the glyph, which is not the
        // same as the glyph being blank. Treating it as a space would invent a
        // word boundary in the middle of a word.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 72 700 Td (abc) Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /X /FontDescriptor 6 0 R \
                  /FirstChar 97 /LastChar 99 /Widths [500 500 500] >>",
            )
            .object(6, "<< /Type /FontDescriptor /Flags 4 >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        let line = assemble(place(&runs)).remove(0);
        let words = segment(&line);
        assert_eq!(words.len(), 1, "three unresolved glyphs are one word, not three");
        assert_eq!(words[0].glyphs, 0..3);
    }
}
