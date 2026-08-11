//! Paragraph and style reconstruction. Spec 7.6.
//!
//! A block is a stack of lines that belong together spatially. A *paragraph* is
//! a stack of lines that belong together **semantically**, and the difference is
//! everything the reflow engine in Phase 5 depends on: it reflows a paragraph,
//! so a block wrongly split is text that will not join up, and a block wrongly
//! merged is text that reflows into its neighbour.
//!
//! Four signals split a block, in decreasing order of trustworthiness:
//!
//! 1. an `/MCID` boundary, which is the producer telling you directly;
//! 2. a style change at a line boundary;
//! 3. a first-line indent following a short line;
//! 4. a leading discontinuity.
//!
//! Spec 7.6 makes (1) authoritative: "prefer it over heuristics when
//! `/StructTreeRoot` is present". The others are inference and can be wrong.

use crate::lines::Line;
use crate::{Region, ResolvedRun};
use rasura_content::matrix::Rect;
use rasura_content::state::Colour;
use rasura_cos::Name;
use std::ops::Range;

/// An inter-line gap beyond this multiple of the block's modal gap starts a new
/// paragraph. Spec 7.6 gives 1.3.
const LEADING_DISCONTINUITY: f64 = 1.3;

/// A line indented beyond this multiple of the font size, following a short
/// line, starts a new paragraph.
const INDENT_FACTOR: f64 = 0.8;

/// A line ending this far short of the block's right edge is a paragraph's last
/// line rather than a wrapped one.
const SHORT_LINE_FACTOR: f64 = 2.0;

/// Edge positions within this fraction of the font size count as aligned.
const EDGE_TOLERANCE: f64 = 0.35;

/// How a paragraph's lines line up. Inferred from edge variance, per spec 7.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
    Centre,
    Justified,
    /// One line, or too little evidence to say. Reported rather than guessed:
    /// re-justifying a paragraph that was not justified is a visible change.
    Unknown,
}

/// A contiguous span of glyphs sharing every style attribute.
#[derive(Debug, Clone)]
pub struct StyleRun {
    /// Line index within the paragraph, and glyph range within that line.
    pub line: usize,
    pub glyphs: Range<usize>,
    pub style: Style,
}

/// The attributes that make a style run. Spec 7.6: "contiguous glyph spans
/// sharing font, size, colour, `Tr`, `Ts`".
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub font: Option<Name>,
    pub base_font: String,
    pub size: f64,
    pub colour: Colour,
    pub render_mode: i64,
    pub rise: f64,
}

impl Style {
    /// Whether two styles are the same for run-splitting purposes. Sizes are
    /// compared with a tolerance because a producer that writes `9.9998 Tf` and
    /// `10 Tf` did not intend a style change.
    pub fn matches(&self, other: &Style) -> bool {
        self.font == other.font
            && self.colour == other.colour
            && self.render_mode == other.render_mode
            && (self.size - other.size).abs() < 0.05
            && (self.rise - other.rise).abs() < 0.05
    }
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    /// Lines of the parent block, as a half-open range.
    pub lines: Range<usize>,
    pub bbox: Rect,
    pub alignment: Alignment,
    /// Modal inter-baseline distance. Zero for a single-line paragraph.
    pub leading: f64,
    /// First-line offset relative to the paragraph's left margin. Negative for
    /// a hanging indent.
    pub first_line_indent: f64,
    pub left_margin: f64,
    pub right_margin: f64,
    pub styles: Vec<StyleRun>,
    /// Spec 7.6: record it so the reflow engine can un-hyphenate and
    /// re-hyphenate, and so a round trip that changes nothing changes nothing.
    pub hyphenation_was_present: bool,
    /// The marked-content id, when the block came from a tagged document.
    pub mcid: Option<u32>,
    /// Why this paragraph began.
    pub reason: SplitReason,
}

/// What ended the previous paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitReason {
    BlockStart,
    /// An `/MCID` boundary: the producer said so.
    Mcid,
    StyleChange,
    FirstLineIndent,
    LeadingGap,
}

impl Paragraph {
    /// Whether the split that produced this paragraph is authoritative or
    /// inferred. Callers deciding whether to trust the structure want this.
    pub fn is_authoritative(&self) -> bool {
        matches!(self.reason, SplitReason::Mcid | SplitReason::BlockStart)
    }
}

/// Split a block into paragraphs and infer their properties.
pub fn reconstruct(block: &Region, runs: &[ResolvedRun]) -> Vec<Paragraph> {
    let lines = &block.lines;
    if lines.is_empty() {
        return Vec::new();
    }

    let modal_gap = modal_gap(lines);
    let right_edge = lines.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let modal_left = modal(&mut lines.iter().map(|l| l.bbox.x0).collect::<Vec<_>>());

    let mut starts: Vec<(usize, SplitReason)> = vec![(0, SplitReason::BlockStart)];
    for i in 1..lines.len() {
        if let Some(reason) = split_before(lines, runs, i, modal_gap, modal_left, right_edge) {
            starts.push((i, reason));
        }
    }

    let mut out = Vec::new();
    for (n, &(start, reason)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map(|(s, _)| *s).unwrap_or(lines.len());
        out.push(build(block, runs, start..end, reason));
    }
    out
}

/// Whether a paragraph boundary falls before line `i`.
fn split_before(
    lines: &[Line],
    runs: &[ResolvedRun],
    i: usize,
    modal_gap: f64,
    modal_left: f64,
    right_edge: f64,
) -> Option<SplitReason> {
    let prev = &lines[i - 1];
    let line = &lines[i];
    let size = line.size.max(prev.size).max(1.0);

    // 1. An /MCID boundary is the producer telling you directly, so it is
    //    checked first and no heuristic can override it.
    let (a, b) = (line_mcid(prev, runs), line_mcid(line, runs));
    if a.is_some() && b.is_some() && a != b {
        return Some(SplitReason::Mcid);
    }

    // 2. A style change at a line boundary. Compared on the *dominant* style of
    //    each line: a single italic word does not start a paragraph, but a
    //    heading in a different font does.
    let (sa, sb) = (dominant_style(prev, runs), dominant_style(line, runs));
    if let (Some(sa), Some(sb)) = (sa, sb)
        && !sa.matches(&sb)
    {
        return Some(SplitReason::StyleChange);
    }

    // 3. A first-line indent, but only where the previous line ended short.
    //    Both conditions are needed: an indent after a full-width line is a
    //    wrapped line in a paragraph with a hanging indent, not a new
    //    paragraph.
    let indented = line.bbox.x0 > modal_left + INDENT_FACTOR * size;
    let prev_short = prev.bbox.x1 < right_edge - SHORT_LINE_FACTOR * size;
    if indented && prev_short {
        return Some(SplitReason::FirstLineIndent);
    }

    // 4. A leading discontinuity.
    let gap = line.baseline - prev.baseline;
    if modal_gap > 0.0 && gap > modal_gap * LEADING_DISCONTINUITY {
        return Some(SplitReason::LeadingGap);
    }

    None
}

fn build(
    block: &Region,
    runs: &[ResolvedRun],
    range: Range<usize>,
    reason: SplitReason,
) -> Paragraph {
    let lines = &block.lines[range.clone()];

    let mut bbox = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for l in lines {
        bbox.x0 = bbox.x0.min(l.bbox.x0);
        bbox.y0 = bbox.y0.min(l.bbox.y0);
        bbox.x1 = bbox.x1.max(l.bbox.x1);
        bbox.y1 = bbox.y1.max(l.bbox.y1);
    }
    if lines.is_empty() {
        bbox = Rect::default();
    }

    // Margins come from the *body* lines, excluding the first: an indented or
    // outdented first line is what `first_line_indent` records, and letting it
    // set the margin would make every indented paragraph look flush.
    let body: Vec<&Line> =
        if lines.len() > 1 { lines[1..].iter().collect() } else { lines.iter().collect() };
    let left_margin = body.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let right_margin = body.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let first_line_indent = lines.first().map(|l| l.bbox.x0 - left_margin).unwrap_or(0.0);

    Paragraph {
        alignment: infer_alignment(lines),
        leading: modal_gap(lines),
        first_line_indent,
        left_margin,
        right_margin,
        styles: style_runs(lines, runs),
        hyphenation_was_present: has_hyphenation(lines),
        mcid: lines.first().and_then(|l| line_mcid(l, runs)),
        bbox,
        lines: range,
        reason,
    }
}

/// Spec 7.6: "Left-aligned = low left variance, high right variance. Justified
/// = both low, with the last line excepted. Centred = both vary, midpoints
/// stable."
fn infer_alignment(lines: &[Line]) -> Alignment {
    if lines.len() < 2 {
        return Alignment::Unknown;
    }
    let size = modal(&mut lines.iter().map(|l| l.size).collect::<Vec<_>>()).max(1.0);
    let tolerance = EDGE_TOLERANCE * size;

    // The last line of a justified paragraph is short by design, and the first
    // line is routinely indented. Each is excluded from the edge it perturbs,
    // and only where two lines remain to measure -- otherwise there is no
    // variance left to speak of.
    //
    // Excluding the first line from the left-edge test is not in spec 7.6's
    // wording, but not doing it contradicts the rest of the section: an
    // indented first line is what `first_line_indent` exists to record, so
    // counting that same indent as evidence against left alignment makes every
    // indented paragraph in a justified book report as right-aligned. It did.
    let body = if lines.len() > 2 { &lines[1..] } else { lines };
    let head = if lines.len() > 2 { &lines[..lines.len() - 1] } else { lines };

    let lefts: Vec<f64> = body.iter().map(|l| l.bbox.x0).collect();
    let rights: Vec<f64> = head.iter().map(|l| l.bbox.x1).collect();
    // Midpoints use every line: a centred paragraph indents neither end.
    let mids: Vec<f64> = lines.iter().map(|l| (l.bbox.x0 + l.bbox.x1) / 2.0).collect();

    let left_flush = spread(&lefts) <= tolerance;
    let right_flush = spread(&rights) <= tolerance;
    let centred = spread(&mids) <= tolerance;

    match (left_flush, right_flush) {
        (true, true) => Alignment::Justified,
        (true, false) => Alignment::Left,
        (false, true) => Alignment::Right,
        // Neither edge is flush. Centred if the midpoints are stable, otherwise
        // there is no answer worth asserting.
        (false, false) => {
            if centred {
                Alignment::Centre
            } else {
                Alignment::Unknown
            }
        }
    }
}

/// Spec 7.6: "a line ending in U+002D/U+00AD where the next line begins
/// lower-case is a soft break".
fn has_hyphenation(lines: &[Line]) -> bool {
    for pair in lines.windows(2) {
        let text = crate::words::line_text(&pair[0]);
        let next = crate::words::line_text(&pair[1]);
        let ends_hyphen =
            text.trim_end().chars().next_back().is_some_and(|c| c == '\u{002d}' || c == '\u{00ad}');
        let next_lower = next.trim_start().chars().next().is_some_and(|c| c.is_lowercase());
        if ends_hyphen && next_lower {
            return true;
        }
    }
    false
}

/// Contiguous glyph spans sharing every style attribute.
fn style_runs(lines: &[Line], runs: &[ResolvedRun]) -> Vec<StyleRun> {
    let mut out: Vec<StyleRun> = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let mut current: Option<(Style, usize)> = None;
        for (gi, g) in line.glyphs.iter().enumerate() {
            // A glyph whose run index does not resolve extends the run it is in
            // rather than opening a hole: the runs must partition the line, and
            // an unattributable glyph is not a style change.
            let Some(style) = style_of(g.run, runs) else { continue };
            match current.take() {
                Some((s, start)) if s.matches(&style) => current = Some((s, start)),
                Some((s, start)) => {
                    out.push(StyleRun { line: li, glyphs: start..gi, style: s });
                    current = Some((style, gi));
                }
                None => current = Some((style, gi)),
            }
        }
        if let Some((s, start)) = current {
            out.push(StyleRun { line: li, glyphs: start..line.glyphs.len(), style: s });
        }
    }
    out
}

fn style_of(run: usize, runs: &[ResolvedRun]) -> Option<Style> {
    let r = runs.get(run)?;
    Some(Style {
        font: r.run.font_name.clone(),
        base_font: r.run.base_font.clone(),
        size: r.run.size,
        colour: r.run.fill.clone(),
        render_mode: r.run.render_mode,
        rise: r.run.rise,
    })
}

/// The style most of a line's glyphs share.
fn dominant_style(line: &Line, runs: &[ResolvedRun]) -> Option<Style> {
    let mut counts: Vec<(Style, usize)> = Vec::new();
    for g in &line.glyphs {
        let Some(s) = style_of(g.run, runs) else { continue };
        match counts.iter_mut().find(|(c, _)| c.matches(&s)) {
            Some((_, n)) => *n += 1,
            None => counts.push((s, 1)),
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s)
}

fn line_mcid(line: &Line, runs: &[ResolvedRun]) -> Option<u32> {
    line.glyphs.first().and_then(|g| runs.get(g.run)).and_then(|r| r.run.mcid)
}

/// Modal inter-baseline distance across a set of lines.
fn modal_gap(lines: &[Line]) -> f64 {
    if lines.len() < 2 {
        return 0.0;
    }
    let mut gaps: Vec<f64> = lines
        .windows(2)
        .map(|p| p[1].baseline - p[0].baseline)
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    modal(&mut gaps)
}

/// The most common value, to within half a point, falling back to the median.
///
/// Modal rather than mean because one large gap -- a figure, a heading -- should
/// not redefine what ordinary leading is in this block.
fn modal(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best = values[0];
    let mut best_count = 0usize;
    let mut i = 0;
    while i < values.len() {
        let mut j = i;
        while j < values.len() && (values[j] - values[i]).abs() < 0.5 {
            j += 1;
        }
        if j - i > best_count {
            best_count = j - i;
            best = values[i];
        }
        i = j;
    }
    best
}

fn spread(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let max = values.iter().cloned().fold(f64::MIN, f64::max);
    let min = values.iter().cloned().fold(f64::MAX, f64::min);
    max - min
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::place;
    use crate::resolve_page;
    use rasura_content::page;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    /// Two fonts so style changes can be tested, both fixed-width at 500/1000.
    fn page_with(content: &str) -> Vec<u8> {
        let widths = "500 ".repeat(95);
        let font = |base: &str| {
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding \
                  /FirstChar 32 /LastChar 126 /Widths [{widths}] >>"
            )
        };
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(5, &font("Helvetica"))
            .object(6, &font("Times-Roman"))
            .finish("/Root 1 0 R")
    }

    fn line_at(x: f64, y: f64, text: &str) -> String {
        format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm ({text}) Tj ET\n")
    }

    /// All paragraphs of the first block.
    fn paragraphs_of(content: &str) -> (Vec<Paragraph>, Vec<ResolvedRun>, Region) {
        let doc = Document::open(page_with(content)).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        let rules = crate::rules::collect(&doc, &p);
        let mut blocks = crate::detect(place(&runs), &rules);
        let block = blocks.remove(0);
        let paras = reconstruct(&block, &runs);
        (paras, runs, block)
    }

    #[test]
    fn a_uniform_block_is_one_paragraph() {
        let mut c = String::new();
        for i in 0..5 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "wrapped body text here"));
        }
        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].lines, 0..5);
        assert_eq!(paras[0].reason, SplitReason::BlockStart);
        assert!((paras[0].leading - 12.0).abs() < 1e-6);
    }

    #[test]
    fn a_leading_discontinuity_splits() {
        let mut c = String::new();
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "first para line"));
        }
        // 18pt where the modal gap is 12: 1.5x, beyond the 1.3 threshold. Still
        // small enough that block detection keeps it as one block.
        for i in 0..3 {
            c.push_str(&line_at(72.0, 658.0 - i as f64 * 12.0, "second para line"));
        }
        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(
            paras.len(),
            2,
            "{:?}",
            paras.iter().map(|p| p.lines.clone()).collect::<Vec<_>>()
        );
        assert_eq!(paras[1].reason, SplitReason::LeadingGap);
    }

    #[test]
    fn an_indent_after_a_short_line_splits() {
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "a full width line of body text here ok"));
        // Short line ends the paragraph.
        c.push_str(&line_at(72.0, 688.0, "short"));
        // Indented first line of the next.
        c.push_str(&line_at(92.0, 676.0, "the next paragraph starts here now"));
        c.push_str(&line_at(72.0, 664.0, "and continues on this second line ok"));

        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[1].reason, SplitReason::FirstLineIndent);
        assert_eq!(paras[1].lines, 2..4);
    }

    #[test]
    fn an_indent_after_a_full_line_does_not_split() {
        // A hanging indent inside one paragraph: the previous line ran to the
        // right edge, so the indent is wrapping, not a new paragraph.
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "a full width line of body text here ok"));
        c.push_str(&line_at(92.0, 688.0, "an indented continuation of the same"));
        c.push_str(&line_at(92.0, 676.0, "and another continuation line here ok"));
        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(paras.len(), 1, "an indent after a full line is not a boundary");
    }

    #[test]
    fn a_style_change_splits() {
        // A heading in a different font above body text.
        let mut c =
            String::from("BT /F2 10 Tf 1 0 0 1 72 700 Tm (A heading in another face) Tj ET\n");
        for i in 0..3 {
            c.push_str(&line_at(72.0, 688.0 - i as f64 * 12.0, "body text line here"));
        }
        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[1].reason, SplitReason::StyleChange);
        assert_eq!(paras[0].lines, 0..1);
    }

    #[test]
    fn a_size_change_splits() {
        let mut c = String::from("BT /F1 16 Tf 1 0 0 1 72 700 Tm (Bigger heading) Tj ET\n");
        for i in 0..3 {
            c.push_str(&line_at(72.0, 684.0 - i as f64 * 12.0, "body text line here"));
        }
        let (paras, _, _) = paragraphs_of(&c);
        assert!(paras.len() >= 2);
        assert_eq!(paras[0].lines, 0..1);
    }

    #[test]
    fn one_word_in_another_style_does_not_split_a_paragraph() {
        // Spec 7.6 compares at line boundaries; an italic word mid-line is a
        // style *run*, not a paragraph break.
        let c = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (plain ) Tj /F2 10 Tf (italic) Tj \
                 /F1 10 Tf ( plain) Tj ET\n"
            .to_string()
            + &line_at(72.0, 688.0, "the following line continues here")
            + &line_at(72.0, 676.0, "and here as well to make a block");
        let (paras, _, _) = paragraphs_of(&c);
        assert_eq!(paras.len(), 1, "a mid-line style change is not a paragraph break");
        assert!(paras[0].styles.len() >= 3, "but it does produce style runs");
    }

    #[test]
    fn style_runs_are_contiguous_and_cover_every_glyph() {
        let c = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (aa) Tj /F2 10 Tf (bb) Tj /F1 10 Tf (cc) Tj ET";
        let (paras, _, block) = paragraphs_of(c);
        let runs = &paras[0].styles;
        assert_eq!(runs.len(), 3);
        let covered: usize = runs.iter().map(|r| r.glyphs.len()).sum();
        assert_eq!(covered, block.lines[0].glyphs.len(), "every glyph is in a run");
        // And the ranges are contiguous.
        for pair in runs.windows(2) {
            assert_eq!(pair[0].glyphs.end, pair[1].glyphs.start);
        }
    }

    #[test]
    fn colour_is_part_of_a_style_run() {
        let c = "BT /F1 10 Tf 1 0 0 1 72 700 Tm 1 0 0 rg (red) Tj 0 0 1 rg (blue) Tj ET";
        let (paras, _, _) = paragraphs_of(c);
        assert_eq!(paras[0].styles.len(), 2, "same font, different colour, two runs");
        assert_ne!(paras[0].styles[0].style.colour, paras[0].styles[1].style.colour);
    }

    // --- alignment ---------------------------------------------------------

    fn aligned(lines: &[(f64, &str)]) -> Alignment {
        let mut c = String::new();
        for (i, (x, text)) in lines.iter().enumerate() {
            c.push_str(&line_at(*x, 700.0 - i as f64 * 12.0, text));
        }
        paragraphs_of(&c).0[0].alignment
    }

    #[test]
    fn left_aligned_is_detected() {
        // Flush left, ragged right.
        assert_eq!(
            aligned(&[(72.0, "aaaaaaaaaaaaaaaa"), (72.0, "bbbbbbbb"), (72.0, "cccccccccccc")]),
            Alignment::Left
        );
    }

    #[test]
    fn justified_is_detected() {
        // Both edges flush; every line the same width.
        assert_eq!(
            aligned(&[(72.0, "aaaaaaaaaaaa"), (72.0, "bbbbbbbbbbbb"), (72.0, "cccccccccccc")]),
            Alignment::Justified
        );
    }

    #[test]
    fn right_aligned_is_detected() {
        // Ragged left, flush right: 4 glyphs at 5pt from x=112 ends at 132,
        // as does 12 glyphs from x=72.
        assert_eq!(
            aligned(&[(72.0, "aaaaaaaaaaaa"), (112.0, "bbbb"), (92.0, "cccccccc")]),
            Alignment::Right
        );
    }

    #[test]
    fn centred_is_detected() {
        // Midpoints stable, neither edge flush.
        assert_eq!(
            aligned(&[(72.0, "aaaaaaaaaaaa"), (92.0, "bbbb"), (82.0, "cccccccc")]),
            Alignment::Centre
        );
    }

    #[test]
    fn a_single_line_paragraph_has_unknown_alignment() {
        // Spec 2: report rather than guess. Re-justifying a paragraph that was
        // never justified is a visible change.
        let (paras, _, _) = paragraphs_of(&line_at(72.0, 700.0, "one line only"));
        assert_eq!(paras[0].alignment, Alignment::Unknown);
    }

    #[test]
    fn a_justified_paragraphs_short_last_line_is_excepted() {
        // The last line of a justified paragraph is short by design and must
        // not make the paragraph look left-aligned.
        assert_eq!(
            aligned(&[
                (72.0, "aaaaaaaaaaaa"),
                (72.0, "bbbbbbbbbbbb"),
                (72.0, "cccccccccccc"),
                (72.0, "dddd"),
            ]),
            Alignment::Justified
        );
    }

    #[test]
    fn an_indented_first_line_does_not_make_a_justified_paragraph_look_right_aligned() {
        // Found on freeculture.pdf: body paragraphs of a justified book, each
        // with a 14pt first-line indent, all reported Right. The indent is what
        // `first_line_indent` records; it is not evidence about alignment.
        assert_eq!(
            aligned(&[
                (86.0, "aaaaaaaaa"),
                (72.0, "bbbbbbbbbbbb"),
                (72.0, "cccccccccccc"),
                (72.0, "dddd"),
            ]),
            Alignment::Justified
        );
    }

    #[test]
    fn an_indented_first_line_still_reads_as_left_aligned() {
        assert_eq!(
            aligned(&[
                (86.0, "aaaaaaaaa"),
                (72.0, "bbbbbbbbbbbb"),
                (72.0, "bbbb"),
                (72.0, "cccccccc"),
            ]),
            Alignment::Left
        );
    }

    // --- indents and margins ------------------------------------------------

    #[test]
    fn a_first_line_indent_is_measured_against_the_body_margin() {
        let mut c = String::new();
        c.push_str(&line_at(92.0, 700.0, "indented first line of the paragraph"));
        c.push_str(&line_at(72.0, 688.0, "flush continuation line here ok"));
        c.push_str(&line_at(72.0, 676.0, "another flush continuation line"));
        let (paras, _, _) = paragraphs_of(&c);
        assert!((paras[0].first_line_indent - 20.0).abs() < 1e-6);
        assert!((paras[0].left_margin - 72.0).abs() < 1e-6);
    }

    #[test]
    fn a_hanging_indent_is_a_negative_first_line_indent() {
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "outdented first line of the paragraph"));
        c.push_str(&line_at(92.0, 688.0, "indented continuation line here"));
        c.push_str(&line_at(92.0, 676.0, "another indented continuation"));
        let (paras, _, _) = paragraphs_of(&c);
        assert!(paras[0].first_line_indent < 0.0, "{}", paras[0].first_line_indent);
    }

    // --- hyphenation ---------------------------------------------------------

    #[test]
    fn a_soft_hyphen_break_is_recorded() {
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "this line ends in a hyphen-"));
        c.push_str(&line_at(72.0, 688.0, "ated word continuing here ok"));
        c.push_str(&line_at(72.0, 676.0, "and a third line to make it a block"));
        let (paras, _, _) = paragraphs_of(&c);
        assert!(paras[0].hyphenation_was_present);
    }

    #[test]
    fn a_hyphen_before_a_capital_is_not_a_soft_break() {
        // A dash ending a line before a proper noun is real punctuation.
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "a line ending in a dash-"));
        c.push_str(&line_at(72.0, 688.0, "Capitalised continues here ok"));
        c.push_str(&line_at(72.0, 676.0, "and a third line to make a block"));
        let (paras, _, _) = paragraphs_of(&c);
        assert!(!paras[0].hyphenation_was_present);
    }

    #[test]
    fn an_unhyphenated_paragraph_records_none() {
        let mut c = String::new();
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "no hyphens in this text"));
        }
        let (paras, _, _) = paragraphs_of(&c);
        assert!(!paras[0].hyphenation_was_present);
    }

    // --- MCID ----------------------------------------------------------------

    #[test]
    fn an_mcid_boundary_splits_and_is_authoritative() {
        // Spec 7.6: prefer the producer's own structure over heuristics. These
        // lines are uniformly spaced and styled, so no heuristic would split
        // them.
        let c = "/P << /MCID 0 >> BDC BT /F1 10 Tf 1 0 0 1 72 700 Tm (first para) Tj ET EMC\n\
                 /P << /MCID 0 >> BDC BT /F1 10 Tf 1 0 0 1 72 688 Tm (still first) Tj ET EMC\n\
                 /P << /MCID 1 >> BDC BT /F1 10 Tf 1 0 0 1 72 676 Tm (second para) Tj ET EMC\n\
                 /P << /MCID 1 >> BDC BT /F1 10 Tf 1 0 0 1 72 664 Tm (still second) Tj ET EMC";
        let (paras, _, _) = paragraphs_of(c);
        assert_eq!(
            paras.len(),
            2,
            "{:?}",
            paras.iter().map(|p| p.lines.clone()).collect::<Vec<_>>()
        );
        assert_eq!(paras[1].reason, SplitReason::Mcid);
        assert_eq!(paras[0].mcid, Some(0));
        assert_eq!(paras[1].mcid, Some(1));
        assert!(paras[1].is_authoritative());
        assert!(
            !Paragraph { reason: SplitReason::LeadingGap, ..paras[1].clone() }.is_authoritative()
        );
    }

    #[test]
    fn paragraphs_partition_their_block() {
        // Every line belongs to exactly one paragraph, contiguously.
        let mut c = String::new();
        c.push_str(&line_at(72.0, 700.0, "first paragraph line one here"));
        c.push_str(&line_at(72.0, 688.0, "short"));
        c.push_str(&line_at(92.0, 676.0, "second paragraph indented start"));
        c.push_str(&line_at(72.0, 664.0, "second paragraph continues here"));
        let (paras, _, block) = paragraphs_of(&c);

        let mut next = 0usize;
        for p in &paras {
            assert_eq!(p.lines.start, next, "paragraphs must be contiguous");
            next = p.lines.end;
        }
        assert_eq!(next, block.lines.len(), "and cover every line");
    }

    #[test]
    fn an_empty_block_yields_no_paragraphs() {
        let block = Region {
            lines: Vec::new(),
            bbox: Rect::default(),
            origin: crate::Origin::Whole,
            order: 0,
        };
        assert!(reconstruct(&block, &[]).is_empty());
    }
}
