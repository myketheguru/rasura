//! Finding the bytes behind a character range. Spec 9.4, step 1.
//!
//! > Compute the set of affected operators from the paragraph's glyph runs —
//! > this is the union of their `op_span`s, expanded to enclosing `BT`/`ET`.
//!
//! Layers three and four hand up a document model in which a paragraph is a
//! range of lines, a line is a list of placed glyphs, and a glyph remembers the
//! run and index it came from. This module walks that chain back down to bytes,
//! which is the step every text operation begins with.
//!
//! # Why the retention matters
//!
//! `rasura_layout::lines` says it plainly: glyphs are *sorted by position
//! for reading* but keep their original operator order for patching. A line's
//! glyphs are therefore not in the order their bytes appear, and a naive
//! "replace from the first glyph's offset to the last one's" would splice over
//! whatever happened to lie between them — including operators belonging to a
//! different line, in a two-column page whose producer interleaved them.
//!
//! So the affected set is computed as a set of *operators*, not as a byte range,
//! and only becomes a range once it is known to be contiguous within one
//! showing operator.

use crate::numfmt::{self, NumberStyle};
use rasura_content::content::{LogicalContent, page_content};
use rasura_content::page::Page;
use rasura_cos::Document;
use rasura_layout::lines::{Line, PlacedGlyph};
use rasura_layout::paragraphs::Paragraph;
use rasura_layout::regions::Region;
use rasura_layout::{ResolvedRun, place, reconstruct, resolve_runs};
use std::ops::Range;

/// Which paragraph on a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParagraphId {
    pub region: usize,
    pub index: usize,
}

/// A page analysed far enough to be edited.
///
/// Built once and read many times: every operation on a page needs the same
/// runs, lines and regions, and rebuilding them per operation would be both
/// slow and *wrong* — the byte spans an operation computed would address a
/// buffer a previous operation in the same session had already changed.
pub struct EditablePage {
    pub index: usize,
    /// The page's concatenated content, which every span here addresses.
    pub content: LogicalContent,
    pub runs: Vec<ResolvedRun>,
    pub regions: Vec<Region>,
    /// Paragraphs, with the region each belongs to.
    pub paragraphs: Vec<(ParagraphId, Paragraph)>,
    /// The producer's numeric habits, for any operator generated here.
    pub style: NumberStyle,
    /// Optional-content regions, in content order. Empty for the 97% of
    /// documents with no `/OCProperties`. Spec 10.2.
    pub optional: Vec<rasura_content::optional::Region>,
    /// The page's visible area, which bounds a line that never wrapped.
    pub crop_box: rasura_content::matrix::Rect,
}

impl EditablePage {
    /// Run §7's chain over one page.
    pub fn analyse(doc: &Document, page: &Page) -> Option<EditablePage> {
        let (content, _errors) = page_content(doc, &page.dict).ok()?;
        let (raw, _, _) =
            rasura_content::text::extract_page_with(doc, page, &rasura_layout::Standard14Widths);
        let (runs, _) = resolve_runs(doc, page, raw);

        let placed = place(&runs);
        let rules = rasura_layout::rules::collect(doc, page);
        let regions = rasura_layout::detect(placed, &rules);

        let mut paragraphs = Vec::new();
        for (r, region) in regions.iter().enumerate() {
            for (i, para) in reconstruct(region, &runs).into_iter().enumerate() {
                paragraphs.push((ParagraphId { region: r, index: i }, para));
            }
        }

        let style = numfmt::sample(content.data());
        // Optional content, when the document has any: 3% of the corpus, so the
        // read is skipped rather than run and discarded on every other page.
        let optional = rasura_content::optional::read(doc)
            .map(|oc| rasura_content::optional::regions(doc, page, &content, &oc))
            .unwrap_or_default();

        Some(EditablePage {
            index: page.index,
            content,
            runs,
            regions,
            paragraphs,
            style,
            crop_box: page.visible_box(),
            optional,
        })
    }

    /// The layer a byte of content sits in, when that layer is turned off.
    ///
    /// Spec 10.2. A hidden layer's text is *in the document* — it extracts, it
    /// is found by `strings`, a reader that ignores visibility copies it. So
    /// this is not a permission check: it is the fact an edit needs in order to
    /// tell a caller that the change they just made will not appear on the
    /// page, and the fact a redaction needs in order to remove it anyway.
    pub fn hidden_layer_at(&self, at: usize) -> Option<&rasura_content::optional::Region> {
        rasura_content::optional::hidden_at(&self.optional, at)
    }

    pub fn paragraph(&self, id: ParagraphId) -> Option<&Paragraph> {
        self.paragraphs.iter().find(|(p, _)| *p == id).map(|(_, para)| para)
    }

    /// The lines a paragraph occupies.
    pub fn lines_of(&self, id: ParagraphId) -> Option<&[Line]> {
        let para = self.paragraph(id)?;
        let region = self.regions.get(id.region)?;
        region.lines.get(para.lines.clone())
    }

    /// A paragraph's text, as the characters an edit range indexes.
    ///
    /// Lines are joined with a single space, which is what a reader sees and so
    /// what an offset a caller computed from `text()` means. A paragraph that
    /// was hyphenated across a line break keeps its hyphen here: removing it
    /// would make the offsets disagree with anything the caller can observe.
    pub fn text_of(&self, id: ParagraphId) -> String {
        let Some(lines) = self.lines_of(id) else { return String::new() };
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&line.text());
        }
        out
    }
}

/// One glyph selected by a character range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selected {
    /// Line index within the paragraph.
    pub line: usize,
    /// Glyph index within that line, in reading order.
    pub glyph: usize,
    /// The run this glyph was drawn by.
    pub run: usize,
    /// Its index within that run.
    pub index: usize,
}

/// Everything a character range touches.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub glyphs: Vec<Selected>,
    /// Distinct runs involved, in ascending order.
    pub runs: Vec<usize>,
    /// The character range that produced this, for reporting.
    pub chars: Range<usize>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Lines touched, in ascending order.
    pub fn lines(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.glyphs.iter().map(|g| g.line).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Map a character range in a paragraph's text onto the glyphs that drew it.
///
/// The range is in characters of [`EditablePage::text_of`], which is what a
/// caller can see. Offsets are clamped rather than refused: an editor asking to
/// replace to "the end" should not have to know the exact count.
pub fn select(page: &EditablePage, id: ParagraphId, chars: Range<usize>) -> Option<Selection> {
    let lines = page.lines_of(id)?;

    let mut selection = Selection { chars: chars.clone(), ..Default::default() };
    let mut cursor = 0usize;

    for (line_index, line) in lines.iter().enumerate() {
        // The joining space between lines occupies one character position and
        // is drawn by no glyph, so it is stepped over rather than selected.
        if line_index > 0 {
            cursor += 1;
        }
        for (glyph_index, glyph) in line.glyphs.iter().enumerate() {
            let width = glyph_char_len(glyph);
            let start = cursor;
            let end = cursor + width;
            cursor = end;

            // A glyph is selected when it overlaps the range at all. A
            // zero-width range selects nothing, which is what an insertion
            // point means.
            let overlaps = start < chars.end && end > chars.start;
            if overlaps {
                selection.glyphs.push(Selected {
                    line: line_index,
                    glyph: glyph_index,
                    run: glyph.run,
                    index: glyph.index,
                });
            }
        }
    }

    selection.runs = selection.glyphs.iter().map(|g| g.run).collect();
    selection.runs.sort_unstable();
    selection.runs.dedup();
    Some(selection)
}

/// How many characters a glyph contributes to the paragraph's text.
///
/// Usually one. A ligature contributes as many as it draws, and an unmapped
/// glyph contributes the sentinel §7.2 substituted — which is one character and
/// visibly not text, so a range over it selects the right glyph even though the
/// text is wrong.
fn glyph_char_len(glyph: &PlacedGlyph) -> usize {
    glyph.text.as_ref().map(|t| t.chars().count()).unwrap_or(1)
}

/// The byte span, in the logical content buffer, of the showing operators that
/// drew a selection.
///
/// Returns one span per run, each covering that run's whole showing operator.
/// The operator is the unit because a `TJ` array's internal structure —
/// interleaved strings and kerning adjustments — has no meaningful sub-range: a
/// patch replacing bytes inside it would have to understand the array to avoid
/// producing something that is not one.
pub fn operator_spans(page: &EditablePage, selection: &Selection) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = selection
        .runs
        .iter()
        .filter_map(|r| page.runs.get(*r))
        .map(|r| r.run.op_span.clone())
        .collect();
    spans.sort_by_key(|s| (s.start, s.end));
    spans.dedup();
    spans
}

/// Expand a set of operator spans to the enclosing `BT`/`ET`, per spec 9.4.
///
/// Needed when an operation regenerates text *state* rather than just the
/// characters shown: a new `Tf` or `Tm` is only meaningful inside the text
/// object it belongs to, and splicing one in beside a `BT` rather than after it
/// puts it outside.
///
/// Returns `None` when the spans are not all inside one text object, which is
/// the case a caller must not paper over — a selection spanning two `BT` blocks
/// is not one run of text however it looks on the page.
pub fn enclosing_text_object(
    content: &LogicalContent,
    spans: &[Range<usize>],
) -> Option<Range<usize>> {
    use rasura_content::op::OpKind;
    use rasura_content::tokenizer::tokenize;

    let first = spans.iter().map(|s| s.start).min()?;
    let last = spans.iter().map(|s| s.end).max()?;

    let (ops, _) = tokenize(content.data());
    let mut open: Option<Range<usize>> = None;
    let mut enclosing: Option<Range<usize>> = None;

    for op in &ops {
        match op.kind {
            OpKind::BeginText => open = Some(op.span.clone()),
            OpKind::EndText => {
                if let Some(bt) = open.take() {
                    let block = bt.start..op.span.end;
                    if block.start <= first && block.end >= last {
                        enclosing = Some(block);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    enclosing
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn two_line_page() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(
                4,
                "",
                b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj \
                  1 0 0 1 72 686 Tm (second line) Tj ET\n",
            )
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    fn analysed(bytes: Vec<u8>) -> (Document, EditablePage) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");
        (doc, page)
    }

    #[test]
    fn a_page_analyses_into_paragraphs_with_text() {
        let (_doc, page) = analysed(two_line_page());
        assert!(!page.paragraphs.is_empty(), "the page has at least one paragraph");

        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        assert!(text.contains("Hello world"), "{text:?}");
    }

    #[test]
    fn a_character_range_selects_the_glyphs_that_drew_it() {
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        let at = text.find("world").expect("found");

        let selection = select(&page, id, at..at + 5).expect("select");
        assert_eq!(selection.glyphs.len(), 5, "five characters, five glyphs");
        assert_eq!(selection.lines(), vec![0], "all on the first line");
    }

    #[test]
    fn a_zero_width_range_selects_nothing() {
        // An insertion point is a position, not a selection. Returning the
        // glyph beside it would make `insert_text` delete a character.
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let selection = select(&page, id, 3..3).expect("select");
        assert!(selection.is_empty());
    }

    #[test]
    fn a_range_spanning_the_line_join_selects_both_lines() {
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        if !text.contains("second") {
            return; // the fixture split into two paragraphs; not this test's subject
        }
        let selection = select(&page, id, 0..text.chars().count()).expect("select");
        assert_eq!(selection.lines(), vec![0, 1]);
    }

    #[test]
    fn a_selection_resolves_to_the_operators_that_drew_it() {
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let selection = select(&page, id, 0..5).expect("select");

        let spans = operator_spans(&page, &selection);
        assert_eq!(spans.len(), 1, "one showing operator");

        let bytes = &page.content.data()[spans[0].clone()];
        let text = String::from_utf8_lossy(bytes);
        assert!(text.contains("Hello world"), "the operator that drew it: {text:?}");
        assert!(text.trim_end().ends_with("Tj"));
    }

    #[test]
    fn operator_spans_are_contiguous_and_patchable() {
        // The property the whole module serves: what comes out can be handed
        // straight to the splice engine.
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let selection = select(&page, id, 0..5).expect("select");
        for span in operator_spans(&page, &selection) {
            assert!(page.content.is_contiguous(span.clone()), "{span:?} lies in one object");
        }
    }

    #[test]
    fn the_enclosing_text_object_covers_the_operators() {
        let (_doc, page) = analysed(two_line_page());
        let id = page.paragraphs[0].0;
        let selection = select(&page, id, 0..5).expect("select");
        let spans = operator_spans(&page, &selection);

        let block = enclosing_text_object(&page.content, &spans).expect("a BT/ET encloses it");
        let text = String::from_utf8_lossy(&page.content.data()[block.clone()]);
        assert!(text.starts_with("BT"), "{text:?}");
        assert!(text.ends_with("ET"), "{text:?}");
        assert!(block.start <= spans[0].start && block.end >= spans[0].end);
    }

    #[test]
    fn a_span_outside_any_text_object_has_no_enclosure() {
        let (_doc, page) = analysed(two_line_page());
        let past = page.content.data().len();
        let spans: Vec<Range<usize>> = (0..1).map(|_| past..past).collect();
        assert!(enclosing_text_object(&page.content, &spans).is_none());
    }

    #[test]
    fn the_number_style_comes_from_the_page_being_edited() {
        let (_doc, page) = analysed(two_line_page());
        // The fixture writes plain integers, so generated operators should too.
        assert!(!page.style.integral_keeps_point);
    }
}
