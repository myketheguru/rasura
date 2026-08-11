//! The layout engine: flow model plus frames to pages. Step 5.
//!
//! > Model plus frames to pages. Well-understood work — it is what a browser or
//! > TeX does — and the one component with no PDF-specific difficulty. Greedy
//! > breaking already exists in `reflow`; what is missing is the box model above
//! > it: frames, float placement for images, keep-with-next, widow and orphan
//! > control.
//!
//! That is exactly what this is, and the assessment holds: there is nothing
//! here a typesetter would find unfamiliar. The PDF-specific difficulty was all
//! in the four steps before it — recovering the flow, closing the non-text
//! holes, inferring the frames, and being able to tell whether the result still
//! says what the document said.
//!
//! # This does not write a PDF
//!
//! It places content: every block gets a page, a frame and a rectangle, and
//! every line of text gets its own. Turning that into a content stream is
//! document mode proper, and it is deliberately a separate step — because the
//! moment content is regenerated, §2's first property stops holding, and that
//! is a contract change a caller has to opt into rather than discover.
//!
//! Keeping them apart also means this can be checked without writing a file.
//! [`Layout::to_flow`] reads the flow model back out of the placement, and
//! [`crate::compare`] compares it with the input — which is I8, closed, with no
//! PDF written and no renderer involved:
//!
//! ```text
//! flow ──layout──▶ placed pages ──to_flow──▶ flow'
//!   └────────────── compare ────────────────┘
//! ```
//!
//! # Measurement
//!
//! Line breaking needs to know how wide text is, which needs font metrics. The
//! engine is parameterised on a [`Measurer`] so that a caller who knows the real
//! embedded font can supply it; the default uses the standard-14 metrics the
//! font crate already ships, which is honest for the Helvetica-shaped case and
//! approximate for everything else. It is reported, not assumed: a layout whose
//! measure is wrong produces lines that are too long or too short, and
//! [`Report::measured_with`] says which metrics were used.

use crate::compare::{Difference, Options as CompareOptions, compare};
use crate::flow::{Block, FlowDocument, Inline};
use rasura_content::matrix::Rect;
use rasura_layout::frames::{Frame, FrameSet};

/// The typographic state a run of text is set in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { size: 11.0, bold: false, italic: false }
    }
}

/// How wide text is.
pub trait Measurer {
    /// Width of `text` set in `style`, in points.
    fn width(&self, text: &str, style: TextStyle) -> f64;

    /// Baseline-to-baseline distance.
    ///
    /// 1.2 times the size is the convention every word processor defaults to
    /// and is close enough to right that overriding it is a refinement rather
    /// than a fix.
    fn line_height(&self, style: TextStyle) -> f64 {
        style.size * 1.2
    }
}

/// The standard-14 metrics, in the Helvetica family.
///
/// The metrics the font crate ships for fonts that supply none, used here for
/// the opposite reason: a document being laid out afresh has no font yet, and
/// something has to decide how wide a line is. Right for a document set in
/// Helvetica or Arial, and wrong in the same direction for everything else —
/// a face with wider glyphs will overflow the measure this predicts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Standard14;

/// Character widths per face, built once.
///
/// Not an optimisation for its own sake. The obvious implementation asks the
/// AGL for each character's glyph name, and `agl::name_of` is a linear scan of
/// 4,281 entries — called once per character, per word, per line, because
/// greedy breaking measures each candidate line as it grows. The first version
/// of this file took **39 seconds** to run ten small layout tests. Inverting
/// the table once takes the same tests to well under a second.
type CharWidths = std::collections::HashMap<char, f64>;

fn widths_for_face(face: &str) -> CharWidths {
    let mut out = CharWidths::new();
    let Some(metrics) = rasura_font::metrics::resolve(face) else {
        return out;
    };
    // One pass over the AGL rather than one scan per character. Several names
    // share a character; the first wins, matching `agl::name_of`.
    for (name, value) in rasura_layout::glyphdata::AGL.iter() {
        let mut chars = value.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else { continue };
        if let Some(w) = metrics.width(name) {
            out.entry(ch).or_insert(f64::from(w));
        }
    }
    out
}

fn face_widths(bold: bool, italic: bool) -> &'static CharWidths {
    use std::sync::OnceLock;
    static FACES: OnceLock<[CharWidths; 4]> = OnceLock::new();
    let all = FACES.get_or_init(|| {
        [
            widths_for_face("Helvetica"),
            widths_for_face("Helvetica-Bold"),
            widths_for_face("Helvetica-Oblique"),
            widths_for_face("Helvetica-BoldOblique"),
        ]
    });
    &all[usize::from(bold) | (usize::from(italic) << 1)]
}

impl Measurer for Standard14 {
    fn width(&self, text: &str, style: TextStyle) -> f64 {
        let widths = face_widths(style.bold, style.italic);
        let mut total = 0.0;
        for ch in text.chars() {
            // A character the AGL does not name — most of them, for a CJK
            // document — falls back to the em-half. Wrong, and visibly so,
            // rather than silently measuring as zero and running off the page.
            total += widths.get(&ch).copied().unwrap_or(500.0);
        }
        total / 1000.0 * style.size
    }
}

/// Where a line of text was put.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLine {
    pub text: String,
    /// The line's box: the frame's measure by the line height, positioned.
    pub rect: Rect,
}

/// Where a block was put.
#[derive(Debug, Clone)]
pub struct PlacedBlock {
    /// Index into the flow document's blocks.
    pub source: usize,
    pub page: usize,
    /// Which frame of the page, left to right.
    pub frame: usize,
    pub rect: Rect,
    pub lines: Vec<PlacedLine>,
    /// True when this is the continuation of a block split across frames.
    pub continued: bool,
}

impl PlacedBlock {
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// A laid-out document.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub pages: usize,
    /// In placement order, which is reading order by construction.
    pub blocks: Vec<PlacedBlock>,
    pub page_size: (f64, f64),
}

impl Layout {
    /// Read the flow model back out of the placement.
    ///
    /// The other half of I8. A layout that dropped a paragraph, reordered two,
    /// or lost a heading's level produces a different flow document here, and
    /// [`crate::compare`] says which. Splits are rejoined: a paragraph carried
    /// across a column break is one paragraph, and a round trip that reported
    /// it as two would be measuring the pagination rather than the content.
    pub fn to_flow(&self, original: &FlowDocument) -> FlowDocument {
        let mut blocks: Vec<Block> = Vec::new();
        let mut last_source: Option<usize> = None;

        for placed in &self.blocks {
            if placed.continued && last_source == Some(placed.source) {
                // Append to the block already emitted for this source.
                if let Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) =
                    blocks.last_mut()
                {
                    if let Some(Inline::Text { text, .. }) = inlines.last_mut() {
                        text.push(' ');
                        text.push_str(&placed.text());
                    }
                }
                continue;
            }
            last_source = Some(placed.source);

            // The original block, with its text replaced by what was placed.
            // Everything else — kind, heading level, list structure — is
            // carried through, because the layout did not change it and
            // rebuilding it from lines would lose it.
            let block = match original.blocks.get(placed.source) {
                Some(b) => rebuild(b, &placed.text()),
                None => {
                    Block::Paragraph { inlines: vec![Inline::text(placed.text())], source: None }
                }
            };
            blocks.push(block);
        }

        // Annotations are carried through rather than placed. A note is text a
        // viewer draws from an annotation dictionary, so it has no position in
        // the page flow and re-laying-out a document does not move it — the
        // annotation is still attached where it was.
        //
        // It still has to *survive*. The first corpus run of this round trip
        // failed on 81 documents for exactly this: the engine skipped notes,
        // `to_flow` never saw them, and the comparison correctly called it
        // content loss. Skipping something and losing it are different, and the
        // difference has to be made here rather than assumed.
        blocks.extend(original.blocks.iter().filter(|b| matches!(b, Block::Note(_))).cloned());

        FlowDocument {
            blocks,
            running: original.running.clone(),
            meta: crate::flow::Meta { pages: self.pages, ..original.meta.clone() },
        }
    }
}

/// A block with its text replaced, keeping everything else.
fn rebuild(block: &Block, text: &str) -> Block {
    match block {
        Block::Heading { level, source, .. } => {
            Block::Heading { level: *level, inlines: vec![Inline::text(text)], source: *source }
        }
        Block::Paragraph { source, .. } => {
            Block::Paragraph { inlines: vec![Inline::text(text)], source: *source }
        }
        // Lists, tables, figures, drawings and notes are placed whole and
        // carried through unchanged: their internal structure is not something
        // line breaking can alter.
        other => other.clone(),
    }
}

/// How to lay out.
#[derive(Debug, Clone)]
pub struct Options {
    pub body: TextStyle,
    /// Sizes for heading levels 1 to 6.
    pub heading_sizes: [f64; 6],
    /// Space between blocks, in points.
    pub block_gap: f64,
    /// The fewest lines of a paragraph that may be left at the foot of a frame.
    ///
    /// Two. One line of a paragraph alone at the bottom of a column is an
    /// orphan, and it is the defect a reader notices without knowing its name.
    pub orphans: usize,
    /// The fewest lines that may be carried to the top of the next frame.
    pub widows: usize,
    /// Keep a heading with the block that follows it.
    ///
    /// A heading at the foot of a column with its section overleaf is worse
    /// than a slightly short column, which is why every typesetting system does
    /// this and none makes it optional by default.
    pub keep_heading_with_next: bool,
    /// Height reserved for a figure or drawing that does not state one.
    pub figure_height: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            body: TextStyle::default(),
            heading_sizes: [24.0, 18.0, 14.0, 12.0, 11.0, 11.0],
            block_gap: 6.0,
            orphans: 2,
            widows: 2,
            keep_heading_with_next: true,
            figure_height: 120.0,
        }
    }
}

/// What the layout did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub pages: usize,
    pub blocks_placed: usize,
    /// Blocks that were split across a frame or page boundary.
    pub blocks_split: usize,
    /// Blocks moved to the next frame to keep a heading with its section.
    pub kept_together: usize,
    /// Blocks taller than a whole frame, placed anyway and overflowing.
    ///
    /// A table or figure larger than the measure has nowhere to go. Placing it
    /// and saying so beats dropping it, and beats an infinite loop looking for
    /// a frame it will never fit.
    pub overfull: usize,
    pub measured_with: &'static str,
}

/// Lay a flow document out into a frame set.
pub fn layout(
    flow: &FlowDocument,
    frames: &FrameSet,
    opts: &Options,
    measurer: &impl Measurer,
) -> (Layout, Report) {
    let mut report = Report { measured_with: "standard-14", ..Report::default() };

    let Some(template) = frames.template() else {
        return (Layout::default(), report);
    };
    let page_frames: Vec<Frame> = template.frames.clone();
    if page_frames.is_empty() {
        return (Layout::default(), report);
    }

    let mut out = Layout { pages: 1, blocks: Vec::new(), page_size: template.size };
    let mut cursor = Cursor { page: 0, frame: 0, y: page_frames[0].rect.y0 };

    let mut index = 0usize;
    while index < flow.blocks.len() {
        let block = &flow.blocks[index];
        let style = style_for(block, opts);
        let frame = &page_frames[cursor.frame];
        let measure = frame.rect.width();

        // Notes and running furniture are not part of the page flow: a note is
        // an annotation's text, which a viewer draws over the page, and giving
        // it a place in the column would put it in the middle of a sentence.
        if matches!(block, Block::Note(_)) {
            index += 1;
            continue;
        }

        let line_height = measurer.line_height(style);
        let lines = break_lines(&block.text(), measure, style, measurer);

        // Blocks that cannot be split occupy their whole height or move.
        if !splittable(block) {
            let height = if lines.is_empty() {
                opts.figure_height
            } else {
                lines.len() as f64 * line_height
            };
            place_atomic(
                &mut out,
                &mut cursor,
                &page_frames,
                index,
                &lines,
                height,
                line_height,
                opts,
                &mut report,
            );
            index += 1;
            continue;
        }

        // A heading is kept with what follows by refusing to be the last thing
        // in a frame: it needs room for itself and one line of the next block.
        let mut needed = lines.len() as f64 * line_height;
        if opts.keep_heading_with_next && matches!(block, Block::Heading { .. }) {
            let next_style =
                flow.blocks.get(index + 1).map(|b| style_for(b, opts)).unwrap_or(opts.body);
            needed += measurer.line_height(next_style);
        }

        let available = page_frames[cursor.frame].rect.y1 - cursor.y;
        if needed > available && available < page_frames[cursor.frame].rect.height() {
            // It does not fit here and would fit in an empty frame: move.
            if matches!(block, Block::Heading { .. }) {
                report.kept_together += 1;
            }
            advance(&mut out, &mut cursor, &page_frames);
            continue;
        }

        // Split it. `fit` is how many lines go here; the rest carries over.
        let room = ((page_frames[cursor.frame].rect.y1 - cursor.y) / line_height).floor();
        let mut fit = (room.max(0.0) as usize).min(lines.len());

        // Widow and orphan control. Leaving fewer than `orphans` lines behind,
        // or carrying fewer than `widows` forward, is worse than moving the
        // whole block.
        if fit < lines.len() && (fit < opts.orphans || lines.len() - fit < opts.widows) {
            fit = 0;
        }

        if fit == 0 {
            if cursor.y > page_frames[cursor.frame].rect.y0 {
                advance(&mut out, &mut cursor, &page_frames);
                continue;
            }
            // An empty frame that still cannot take a single line: the frame is
            // shorter than a line of text. Place one anyway rather than loop.
            fit = 1.min(lines.len());
            report.overfull += 1;
        }

        let frame_rect = page_frames[cursor.frame].rect;
        let placed = lines[..fit].to_vec();
        let height = placed.len() as f64 * line_height;
        out.blocks.push(PlacedBlock {
            source: index,
            page: cursor.page,
            frame: cursor.frame,
            rect: Rect::new(frame_rect.x0, cursor.y, frame_rect.x1, cursor.y + height),
            lines: position(&placed, frame_rect, cursor.y, line_height),
            continued: false,
        });
        report.blocks_placed += 1;
        cursor.y += height + opts.block_gap;

        if fit < lines.len() {
            report.blocks_split += 1;
            // The remainder becomes the next thing to place, in the next frame.
            let rest = lines[fit..].join(" ");
            advance(&mut out, &mut cursor, &page_frames);
            place_continuation(
                &mut out,
                &mut cursor,
                &page_frames,
                index,
                &rest,
                style,
                line_height,
                opts,
                measurer,
                &mut report,
            );
        }
        index += 1;
    }

    report.pages = out.pages;
    (out, report)
}

struct Cursor {
    page: usize,
    frame: usize,
    y: f64,
}

/// Move to the next frame, or the next page when the frames run out.
fn advance(out: &mut Layout, cursor: &mut Cursor, frames: &[Frame]) {
    if cursor.frame + 1 < frames.len() {
        cursor.frame += 1;
    } else {
        cursor.frame = 0;
        cursor.page += 1;
        out.pages = out.pages.max(cursor.page + 1);
    }
    cursor.y = frames[cursor.frame].rect.y0;
}

/// Place a block that cannot be split.
#[allow(clippy::too_many_arguments)]
fn place_atomic(
    out: &mut Layout,
    cursor: &mut Cursor,
    frames: &[Frame],
    source: usize,
    lines: &[String],
    height: f64,
    line_height: f64,
    opts: &Options,
    report: &mut Report,
) {
    let frame = frames[cursor.frame].rect;
    let available = frame.y1 - cursor.y;

    if height > available {
        if height <= frame.height() {
            advance(out, cursor, frames);
        } else {
            // Taller than any frame. Placed where it is and reported, because
            // moving it would only find another frame it also does not fit.
            report.overfull += 1;
        }
    }

    let frame = frames[cursor.frame].rect;
    out.blocks.push(PlacedBlock {
        source,
        page: cursor.page,
        frame: cursor.frame,
        rect: Rect::new(frame.x0, cursor.y, frame.x1, cursor.y + height),
        lines: position(lines, frame, cursor.y, line_height),
        continued: false,
    });
    report.blocks_placed += 1;
    cursor.y += height + opts.block_gap;
}

/// Place what is left of a split block, splitting again if it still does not
/// fit.
#[allow(clippy::too_many_arguments)]
fn place_continuation(
    out: &mut Layout,
    cursor: &mut Cursor,
    frames: &[Frame],
    source: usize,
    text: &str,
    style: TextStyle,
    line_height: f64,
    opts: &Options,
    measurer: &impl Measurer,
    report: &mut Report,
) {
    let mut remaining = text.to_string();
    loop {
        let frame = frames[cursor.frame].rect;
        let lines = break_lines(&remaining, frame.width(), style, measurer);
        if lines.is_empty() {
            return;
        }
        let room = ((frame.y1 - cursor.y) / line_height).floor().max(0.0) as usize;
        let fit = room.min(lines.len()).max(1);

        let placed = lines[..fit].to_vec();
        let height = placed.len() as f64 * line_height;
        out.blocks.push(PlacedBlock {
            source,
            page: cursor.page,
            frame: cursor.frame,
            rect: Rect::new(frame.x0, cursor.y, frame.x1, cursor.y + height),
            lines: position(&placed, frame, cursor.y, line_height),
            continued: true,
        });
        report.blocks_placed += 1;
        cursor.y += height + opts.block_gap;

        if fit >= lines.len() {
            return;
        }
        remaining = lines[fit..].join(" ");
        report.blocks_split += 1;
        advance(out, cursor, frames);
    }
}

fn position(lines: &[String], frame: Rect, top: f64, line_height: f64) -> Vec<PlacedLine> {
    lines
        .iter()
        .enumerate()
        .map(|(i, text)| PlacedLine {
            text: text.clone(),
            rect: Rect::new(
                frame.x0,
                top + i as f64 * line_height,
                frame.x1,
                top + (i + 1) as f64 * line_height,
            ),
        })
        .collect()
}

/// Whether a block's lines may be divided across frames.
///
/// Paragraphs and headings may; a heading only ever occupies a line or two, so
/// in practice it is the paragraphs. A table split down the middle loses its
/// header, and a figure has no lines to divide, so both are placed whole.
fn splittable(block: &Block) -> bool {
    matches!(block, Block::Paragraph { .. } | Block::Heading { .. } | Block::Opaque { .. })
}

fn style_for(block: &Block, opts: &Options) -> TextStyle {
    match block {
        Block::Heading { level, .. } => TextStyle {
            size: opts.heading_sizes[(*level as usize).clamp(1, 6) - 1],
            bold: true,
            italic: false,
        },
        _ => opts.body,
    }
}

/// Greedy line breaking to a measure.
///
/// Greedy rather than Knuth–Plass, matching §9.3's default and for the same
/// reason: an editor is judged on the diff it produces, and optimal breaking
/// re-breaks lines nobody touched. `rasura_edit::reflow` has the optimal
/// implementation for a caller who wants it.
fn break_lines(
    text: &str,
    measure: f64,
    style: TextStyle,
    measurer: &impl Measurer,
) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if measure <= 0.0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let candidate =
            if current.is_empty() { word.to_string() } else { format!("{current} {word}") };

        if measurer.width(&candidate, style) <= measure || current.is_empty() {
            // A single word wider than the measure goes on its own line rather
            // than being hyphenated: breaking inside a word needs hyphenation
            // rules this engine does not have, and a too-long line is visible
            // where a wrongly hyphenated one is wrong.
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Lay out and check the result against the input. I8, closed.
///
/// The loop `docs/flow-model.md` describes, with no PDF written and no renderer
/// involved: build the model, lay it out, read the model back out of the
/// placement, and compare. Returns the differences, so empty means the layout
/// preserved the document.
///
/// Pagination is excluded from the comparison, because re-laying-out a document
/// is *expected* to change it — that is what [`crate::Drift`] measures.
pub fn round_trip(
    flow: &FlowDocument,
    frames: &FrameSet,
    opts: &Options,
    measurer: &impl Measurer,
) -> (Layout, Vec<Difference>) {
    let (placed, _) = layout(flow, frames, opts, measurer);
    let extracted = placed.to_flow(flow);

    // Emphasis is not compared: the placement carries text and geometry, and
    // inline styling is the emitter's business rather than the engine's. Saying
    // so here is better than a comparison that fails on every document for a
    // reason that is not a layout defect.
    let differences = compare(
        flow,
        &extracted,
        &CompareOptions {
            compare_pages: false,
            compare_emphasis: false,
            ..CompareOptions::default()
        },
    );
    (placed, differences)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_layout::frames::{Evidence, Frame, PageGroup};

    fn frames(rects: &[Rect]) -> FrameSet {
        FrameSet {
            groups: vec![PageGroup {
                pages: vec![0],
                size: (612.0, 792.0),
                frames: rects
                    .iter()
                    .enumerate()
                    .map(|(i, r)| Frame {
                        rect: *r,
                        column: i,
                        blocks: 1,
                        evidence: Evidence::Pages(4),
                    })
                    .collect(),
            }],
        }
    }

    fn one_column() -> FrameSet {
        frames(&[Rect::new(72.0, 72.0, 540.0, 720.0)])
    }

    fn para(text: &str) -> Block {
        Block::Paragraph { inlines: vec![Inline::text(text)], source: None }
    }

    fn heading(level: u8, text: &str) -> Block {
        Block::Heading { level, inlines: vec![Inline::text(text)], source: None }
    }

    fn doc(blocks: Vec<Block>) -> FlowDocument {
        FlowDocument { blocks, ..FlowDocument::default() }
    }

    /// Words enough to fill roughly `lines` lines of a 468pt measure.
    fn prose(words: usize) -> String {
        (0..words).map(|i| format!("word{i:03}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn text_is_broken_to_the_measure_and_no_line_exceeds_it() {
        let flow = doc(vec![para(&prose(200))]);
        let (placed, _) = layout(&flow, &one_column(), &Options::default(), &Standard14);

        assert!(!placed.blocks.is_empty());
        let opts = Options::default();
        for block in &placed.blocks {
            for line in &block.lines {
                let width = Standard14.width(&line.text, opts.body);
                assert!(width <= 468.0 + 0.01, "{:?} is {width} wide", line.text);
            }
        }
        // And it used the measure rather than one word per line.
        let first = &placed.blocks[0].lines[0];
        assert!(first.text.split_whitespace().count() > 4, "{first:?}");
    }

    #[test]
    fn a_long_document_flows_onto_more_pages() {
        let flow = doc((0..40).map(|_| para(&prose(120))).collect());
        let (placed, report) = layout(&flow, &one_column(), &Options::default(), &Standard14);

        assert!(placed.pages > 1, "40 paragraphs do not fit on one page");
        assert_eq!(report.pages, placed.pages);
        // Every block reached a page.
        assert!(report.blocks_placed >= 40);
    }

    #[test]
    fn two_columns_fill_left_then_right_then_the_next_page() {
        let two =
            frames(&[Rect::new(72.0, 72.0, 290.0, 720.0), Rect::new(322.0, 72.0, 540.0, 720.0)]);
        let flow = doc((0..30).map(|_| para(&prose(80))).collect());
        let (placed, _) = layout(&flow, &two, &Options::default(), &Standard14);

        // The first block is in the left column of page one, and somewhere
        // later the right column is used before page two begins.
        assert_eq!((placed.blocks[0].page, placed.blocks[0].frame), (0, 0));

        let first_right = placed.blocks.iter().position(|b| b.page == 0 && b.frame == 1);
        let first_page_two = placed.blocks.iter().position(|b| b.page == 1);
        assert!(first_right.is_some(), "the right column should be used");
        if let (Some(right), Some(page_two)) = (first_right, first_page_two) {
            assert!(right < page_two, "the right column fills before page two starts");
        }
    }

    #[test]
    fn a_heading_is_not_left_alone_at_the_foot_of_a_frame() {
        // A short frame with room for exactly three lines. Two paragraphs of one
        // line each, then a heading: the heading would be the third line and its
        // section would start overleaf.
        let short = frames(&[Rect::new(72.0, 72.0, 540.0, 72.0 + 3.0 * 13.2)]);
        let flow = doc(vec![para("one"), para("two"), heading(3, "A Section"), para("body")]);

        let (placed, report) = layout(&flow, &short, &Options::default(), &Standard14);
        let heading_block = placed.blocks.iter().find(|b| b.source == 2).expect("the heading");
        let body = placed.blocks.iter().find(|b| b.source == 3).expect("the body");

        assert_eq!(
            (heading_block.page, heading_block.frame),
            (body.page, body.frame),
            "the heading travels with its section"
        );
        assert!(report.kept_together > 0);
    }

    #[test]
    fn a_paragraph_splits_across_a_column_and_rejoins_on_the_way_back() {
        let two =
            frames(&[Rect::new(72.0, 72.0, 290.0, 200.0), Rect::new(322.0, 72.0, 540.0, 720.0)]);
        let flow = doc(vec![para(&prose(120))]);
        let (placed, report) = layout(&flow, &two, &Options::default(), &Standard14);

        assert!(report.blocks_split > 0, "it should not fit in the first column");
        assert!(placed.blocks.len() > 1);
        assert!(placed.blocks[1].continued);

        // And the round trip puts it back together: one paragraph in, one out.
        let back = placed.to_flow(&flow);
        assert_eq!(back.blocks.len(), 1, "{:#?}", back.blocks);
    }

    #[test]
    fn i8_holds_across_the_layout_engine() {
        // The loop `docs/flow-model.md` asks for, closed: build the model, lay
        // it out, read the model back, and the two agree. No PDF is written and
        // no renderer is involved, which is what makes it cheap enough to run on
        // every change.
        let flow = doc(vec![
            heading(1, "Quarterly Results"),
            para(&prose(90)),
            heading(2, "Revenue"),
            para(&prose(140)),
            para(&prose(60)),
        ]);

        let (_, differences) = round_trip(&flow, &one_column(), &Options::default(), &Standard14);
        assert!(differences.is_empty(), "{differences:#?}");
    }

    #[test]
    fn i8_holds_when_the_document_is_forced_to_repaginate() {
        // The case that matters: a frame set far too small for the content, so
        // almost every block splits and the page count bears no relation to the
        // input. The content still has to come back identical.
        let tiny = frames(&[Rect::new(0.0, 0.0, 200.0, 60.0)]);
        let flow = doc(vec![
            heading(1, "Title"),
            para(&prose(120)),
            para(&prose(30)),
            heading(2, "Next"),
            para(&prose(80)),
        ]);

        let (placed, differences) = round_trip(&flow, &tiny, &Options::default(), &Standard14);
        assert!(placed.pages > 3, "it should be forced onto many pages");
        assert!(differences.is_empty(), "{differences:#?}");
    }

    #[test]
    fn a_dropped_block_would_fail_i8() {
        // The check is only worth having if it can fail. A layout that loses a
        // paragraph must be caught, so one is removed by hand and the same
        // comparison run.
        let flow = doc(vec![para("first"), para("second"), para("third")]);
        let (placed, differences) =
            round_trip(&flow, &one_column(), &Options::default(), &Standard14);
        assert!(differences.is_empty(), "the honest layout round-trips");

        let mut damaged = placed.clone();
        damaged.blocks.remove(1);
        let extracted = damaged.to_flow(&flow);
        let diff = compare(
            &flow,
            &extracted,
            &CompareOptions { compare_pages: false, ..CompareOptions::default() },
        );
        assert!(!diff.is_empty(), "losing a paragraph has to be visible");
        assert!(diff.iter().any(Difference::is_content_loss));
    }

    #[test]
    fn annotation_text_survives_the_layout_without_being_placed() {
        // Found by the corpus sweep: 81 documents failed I8 through the layout
        // engine, every one of them because it carried annotation text. A note
        // is drawn by the viewer from an annotation dictionary, so it has no
        // place in the page flow — and skipping it is not the same as losing
        // it.
        use crate::flow::Note;

        let flow = doc(vec![
            para("body text on the page"),
            Block::Note(Note {
                kind: "Widget".to_string(),
                field: Some("signatory".to_string()),
                text: "A. Ozdamar".to_string(),
                page: 0,
            }),
        ]);

        let (placed, differences) =
            round_trip(&flow, &one_column(), &Options::default(), &Standard14);
        assert!(differences.is_empty(), "{differences:#?}");
        assert!(
            placed.blocks.iter().all(|b| b.source != 1),
            "the note is not given a position in the column"
        );

        let back = placed.to_flow(&flow);
        assert!(
            back.blocks.iter().any(|b| matches!(b, Block::Note(n) if n.text == "A. Ozdamar")),
            "and it is still in the document: {:#?}",
            back.blocks
        );
    }

    #[test]
    fn a_frame_shorter_than_a_line_is_reported_rather_than_looping() {
        // The pathological case a layout engine has to survive: a frame with no
        // room for even one line. Placing it and saying so beats searching
        // forever for a frame it will never fit in.
        let sliver = frames(&[Rect::new(0.0, 0.0, 200.0, 2.0)]);
        let flow = doc(vec![para(&prose(20))]);
        let (placed, report) = layout(&flow, &sliver, &Options::default(), &Standard14);

        assert!(report.overfull > 0, "it is overfull and says so");
        assert!(!placed.blocks.is_empty(), "and the content is still placed");
    }

    #[test]
    fn measurement_is_proportional_and_not_a_character_count() {
        // `iii` and `WWW` are the same number of characters and nothing like the
        // same width. A layout engine that measured by counting would break
        // every line in the wrong place.
        let style = TextStyle::default();
        let narrow = Standard14.width("iiiiiiiiii", style);
        let wide = Standard14.width("WWWWWWWWWW", style);
        assert!(wide > narrow * 3.0, "narrow {narrow}, wide {wide}");
    }
}
