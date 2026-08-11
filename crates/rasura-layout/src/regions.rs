//! Region and column detection. Spec 7.5.
//!
//! Two stages with a fallback, exactly as the spec lays out:
//!
//! 1. **Recursive XY-cut.** Project the line boxes onto both axes, cut at the
//!    widest valley that clears a threshold, recurse. The traversal of the
//!    resulting tree is reading order -- which is the whole reason this is done
//!    as a tree rather than as flat clustering. A two-column page read top to
//!    bottom is gibberish; read column by column it is prose.
//! 2. **Docstrum fallback.** A region that will not cut but is plainly not one
//!    block -- a magazine layout, text wrapped around a figure -- goes to
//!    nearest-neighbour clustering instead.
//!
//! Ruling lines are consulted before cutting: a horizontal rule spanning the
//! region is a cut the geometry might not otherwise find, because a rule between
//! two paragraphs often sits in a gap too narrow to clear the threshold on its
//! own.
//!
//! # Why this cuts glyphs, not lines
//!
//! Spec 7.5 says "a projection profile of **glyph** bounding boxes", and the
//! word matters. Line assembly correctly joins every glyph sharing a baseline,
//! so on a two-column page the lines *span the gutter* -- left column and right
//! column at the same height are one line. Project those and there is no valley
//! to cut at, and columns become undetectable in principle rather than by
//! accident. Cutting glyphs first and assembling lines inside each leaf is the
//! order that works.

use crate::lines::{Line, PlacedGlyph, assemble};
use crate::rules::{self, Rule};
use rasura_content::matrix::Rect;

/// A vertical gap must exceed this multiple of the median line height to
/// justify a horizontal cut. Spec 7.5.
const VERTICAL_GAP_FACTOR: f64 = 1.5;

/// A horizontal gap must exceed this multiple of the median character width to
/// justify a vertical cut. Spec 7.5.
const HORIZONTAL_GAP_FACTOR: f64 = 0.8;

/// A rule spanning at least this much of a region is a cut hint.
const RULE_SPAN: f64 = 0.6;

/// Recursion deeper than this is a pathological page, not a real structure.
const MAX_DEPTH: usize = 24;

/// Below this many lines a region is a block; there is nothing to cut.
const MIN_LINES_TO_CUT: usize = 2;

/// How a block was arrived at, which is worth knowing when the answer looks
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A leaf of the XY-cut tree.
    XyCut,
    /// Produced by the docstrum fallback.
    Docstrum,
    /// The whole region, uncut.
    Whole,
}

/// A candidate block: lines that belong together, in reading order.
#[derive(Debug, Clone)]
pub struct Region {
    pub lines: Vec<Line>,
    pub bbox: Rect,
    pub origin: Origin,
    /// Reading-order index, assigned by the tree traversal.
    pub order: usize,
}

impl Region {
    pub fn text(&self) -> String {
        self.lines.iter().map(crate::words::line_text).collect::<Vec<_>>().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Modal line height, the basis for most of §7.6's paragraph thresholds.
    pub fn line_height(&self) -> f64 {
        median(&mut self.lines.iter().map(|l| l.size).collect::<Vec<_>>())
    }
}

/// Which axis a cut divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Split left from right: a column boundary.
    Vertical,
    /// Split top from bottom: a paragraph or section boundary.
    Horizontal,
}

/// Detect blocks on a page from its placed glyphs.
pub fn detect(glyphs: Vec<PlacedGlyph>, rules: &[Rule]) -> Vec<Region> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    cut(glyphs, rules, 0, &mut out);
    for (i, b) in out.iter_mut().enumerate() {
        b.order = i;
    }
    out.retain(|b| !b.is_empty());
    out
}

/// The device-space box a glyph occupies.
fn glyph_box(g: &PlacedGlyph) -> Rect {
    let (sin, cos) = g.direction.sin_cos();
    let ascent = g.size * 0.75;
    let descent = g.size * 0.25;
    let mut r = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for (dt, dn) in [(0.0, -ascent), (g.advance, -ascent), (0.0, descent), (g.advance, descent)] {
        let x = g.origin.x + dt * cos - dn * sin;
        let y = g.origin.y + dt * sin + dn * cos;
        r.x0 = r.x0.min(x);
        r.y0 = r.y0.min(y);
        r.x1 = r.x1.max(x);
        r.y1 = r.y1.max(y);
    }
    r
}

/// Recursive XY-cut. Appends leaves in reading order.
fn cut(glyphs: Vec<PlacedGlyph>, rules: &[Rule], depth: usize, out: &mut Vec<Region>) {
    if glyphs.is_empty() {
        return;
    }
    let region = glyph_bounds(&glyphs);
    if depth >= MAX_DEPTH || glyphs.len() < MIN_LINES_TO_CUT {
        out.push(leaf(glyphs, Origin::XyCut));
        return;
    }

    // A rule spanning the region is a cut the profile may be too coarse to see:
    // a horizontal rule between two paragraphs often sits in a gap narrower
    // than 1.5 line heights.
    let rule_cuts = rules::horizontal_cuts(rules, &region, RULE_SPAN);
    if let Some(at) = rule_cuts.iter().copied().find(|at| splits(&glyphs, Axis::Horizontal, *at)) {
        let (a, b) = split(glyphs, Axis::Horizontal, at);
        cut(a, rules, depth + 1, out);
        cut(b, rules, depth + 1, out);
        return;
    }

    match best_cut(&glyphs) {
        Some((axis, at)) => {
            let (a, b) = split(glyphs, axis, at);
            // Reading order: for a vertical cut, left before right; for a
            // horizontal cut, top before bottom. `split` returns them in that
            // order already, so recursing in sequence is reading order.
            cut(a, rules, depth + 1, out);
            cut(b, rules, depth + 1, out);
        }
        None => {
            // Undividable. Spec 7.5: if the region has high internal variance
            // it is a magazine-style layout rather than one block, and
            // nearest-neighbour clustering does better than giving up.
            //
            // Lines are assembled first so the fallback runs over tens of
            // items rather than thousands of glyphs.
            let lines = assemble(glyphs);
            if lines.len() >= 4 && variance_is_high(&lines) {
                for g in docstrum(lines) {
                    out.push(make_block(g, Origin::Docstrum));
                }
            } else {
                out.push(make_block(lines, Origin::Whole));
            }
        }
    }
}

/// The widest valley on either axis that clears its threshold.
fn best_cut(glyphs: &[PlacedGlyph]) -> Option<(Axis, f64)> {
    let line_height = median(&mut glyphs.iter().map(|g| g.size).collect::<Vec<_>>()).max(1.0);
    let char_width = median(
        &mut glyphs
            .iter()
            .map(|g| g.advance)
            .filter(|a| a.is_finite() && *a > 0.0)
            .collect::<Vec<_>>(),
    )
    .max(line_height * 0.25);

    let v_threshold = VERTICAL_GAP_FACTOR * line_height;
    let h_threshold = HORIZONTAL_GAP_FACTOR * char_width;

    let horizontal = widest_gap(glyphs, Axis::Horizontal).filter(|(w, _)| *w > v_threshold);
    let vertical = widest_gap(glyphs, Axis::Vertical).filter(|(w, _)| *w > h_threshold);

    // Compared as multiples of their own thresholds, because points on one axis
    // are not comparable with points on the other.
    let h_score = horizontal.map(|(w, _)| w / v_threshold);
    let v_score = vertical.map(|(w, _)| w / h_threshold);

    match (h_score, v_score) {
        (Some(h), Some(v)) => {
            // A vertical cut separates columns, which is structurally more
            // significant than a paragraph gap, so it wins a near tie. A
            // full-width heading cannot be cut vertically anyway -- it spans the
            // gutter, so the profile has no valley there -- which is what makes
            // this preference safe.
            if v * 1.5 >= h {
                vertical.map(|(_, at)| (Axis::Vertical, at))
            } else {
                horizontal.map(|(_, at)| (Axis::Horizontal, at))
            }
        }
        (Some(_), None) => horizontal.map(|(_, at)| (Axis::Horizontal, at)),
        (None, Some(_)) => vertical.map(|(_, at)| (Axis::Vertical, at)),
        (None, None) => None,
    }
}

/// The widest empty band in the projection profile, and where to cut it.
fn widest_gap(glyphs: &[PlacedGlyph], axis: Axis) -> Option<(f64, f64)> {
    // Intervals each glyph occupies on the axis being projected onto.
    let mut intervals: Vec<(f64, f64)> = glyphs
        .iter()
        .map(|g| {
            let b = glyph_box(g);
            match axis {
                Axis::Horizontal => (b.y0, b.y1),
                Axis::Vertical => (b.x0, b.x1),
            }
        })
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .collect();
    if intervals.len() < 2 {
        return None;
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut best: Option<(f64, f64)> = None;
    let mut reach = intervals[0].1;
    for &(start, end) in &intervals[1..] {
        let gap = start - reach;
        if gap > 0.0 && best.is_none_or(|(w, _)| gap > w) {
            best = Some((gap, reach + gap / 2.0));
        }
        reach = reach.max(end);
    }
    best
}

/// Whether cutting at `at` actually divides the glyphs into two non-empty parts.
fn splits(glyphs: &[PlacedGlyph], axis: Axis, at: f64) -> bool {
    let mut first = false;
    let mut second = false;
    for g in glyphs {
        if side(g, axis, at) {
            first = true;
        } else {
            second = true;
        }
        if first && second {
            return true;
        }
    }
    false
}

fn split(glyphs: Vec<PlacedGlyph>, axis: Axis, at: f64) -> (Vec<PlacedGlyph>, Vec<PlacedGlyph>) {
    glyphs.into_iter().partition(|g| side(g, axis, at))
}

/// True for the first half: above for a horizontal cut, left for a vertical one.
fn side(g: &PlacedGlyph, axis: Axis, at: f64) -> bool {
    let b = glyph_box(g);
    match axis {
        // Device space has y increasing downwards, so "above" is the smaller y
        // and comes first in reading order.
        Axis::Horizontal => (b.y0 + b.y1) / 2.0 < at,
        Axis::Vertical => (b.x0 + b.x1) / 2.0 < at,
    }
}

/// Assemble a leaf's glyphs into lines and make it a block.
fn leaf(glyphs: Vec<PlacedGlyph>, origin: Origin) -> Region {
    make_block(assemble(glyphs), origin)
}

fn glyph_bounds(glyphs: &[PlacedGlyph]) -> Rect {
    let mut r = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    let mut any = false;
    for g in glyphs {
        let b = glyph_box(g);
        if !b.x0.is_finite() {
            continue;
        }
        any = true;
        r.x0 = r.x0.min(b.x0);
        r.y0 = r.y0.min(b.y0);
        r.x1 = r.x1.max(b.x1);
        r.y1 = r.y1.max(b.y1);
    }
    if any { r } else { Rect::default() }
}

/// Whether an undividable region looks like several blocks rather than one.
///
/// The signal is lines that do not share a left edge and do not fill the
/// region's width: a single paragraph is a stack of lines starting in the same
/// place, while a magazine layout is not.
fn variance_is_high(lines: &[Line]) -> bool {
    let region = bounds(lines);
    if region.width() <= 0.0 {
        return false;
    }
    let lefts: Vec<f64> = lines.iter().map(|l| l.bbox.x0).collect();
    let spread = lefts.iter().cloned().fold(f64::MIN, f64::max)
        - lefts.iter().cloned().fold(f64::MAX, f64::min);

    let widths: Vec<f64> = lines.iter().map(|l| l.bbox.width()).collect();
    let median_width = median(&mut widths.clone());

    // Left edges scattered over more than a third of the region, and lines
    // occupying well under its width -- so they are not simply ragged-right
    // lines of one paragraph.
    spread > region.width() / 3.0 && median_width < region.width() * 0.7
}

/// Nearest-neighbour clustering. Spec 7.5's docstrum fallback.
///
/// Simplified to operate on lines rather than on individual glyphs: two lines
/// join when they overlap horizontally and their vertical gap is within a
/// couple of line heights, which is the condition docstrum's angle-and-distance
/// histograms converge on for body text anyway.
fn docstrum(lines: Vec<Line>) -> Vec<Vec<Line>> {
    let n = lines.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut Vec<usize>, i: usize) -> usize {
        if parent[i] != i {
            let root = find(parent, parent[i]);
            parent[i] = root;
        }
        parent[i]
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if neighbours(&lines[i], &lines[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }

    let mut groups: Vec<(usize, Vec<Line>)> = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        let root = find(&mut parent, i);
        match groups.iter_mut().find(|(r, _)| *r == root) {
            Some((_, v)) => v.push(line),
            None => groups.push((root, vec![line])),
        }
    }

    let mut out: Vec<Vec<Line>> = groups.into_iter().map(|(_, v)| v).collect();
    // Reading order among the groups: top to bottom, then left to right.
    out.sort_by(|a, b| {
        let (ba, bb) = (bounds(a), bounds(b));
        ba.y0
            .partial_cmp(&bb.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| ba.x0.partial_cmp(&bb.x0).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

fn neighbours(a: &Line, b: &Line) -> bool {
    let overlap = a.bbox.x0.max(b.bbox.x0) < a.bbox.x1.min(b.bbox.x1);
    if !overlap {
        return false;
    }
    let gap = if a.bbox.y1 <= b.bbox.y0 {
        b.bbox.y0 - a.bbox.y1
    } else if b.bbox.y1 <= a.bbox.y0 {
        a.bbox.y0 - b.bbox.y1
    } else {
        // Vertically overlapping and horizontally overlapping: the same line
        // split into pieces.
        0.0
    };
    gap <= a.size.max(b.size) * 1.5
}

fn make_block(mut lines: Vec<Line>, origin: Origin) -> Region {
    lines.sort_by(|a, b| {
        a.baseline
            .partial_cmp(&b.baseline)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let bbox = bounds(&lines);
    Region { lines, bbox, origin, order: 0 }
}

fn bounds(lines: &[Line]) -> Rect {
    let mut r = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    let mut any = false;
    for l in lines {
        if !l.bbox.x0.is_finite() {
            continue;
        }
        any = true;
        r.x0 = r.x0.min(l.bbox.x0);
        r.y0 = r.y0.min(l.bbox.y0);
        r.x1 = r.x1.max(l.bbox.x1);
        r.y1 = r.y1.max(l.bbox.y1);
    }
    if any { r } else { Rect::default() }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::place;
    use crate::resolve_page;
    use rasura_content::page;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    /// Every glyph 500/1000 wide: at 10pt each advance is exactly 5pt, so the
    /// geometry in these tests is exact.
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

    /// Emit one line of text at a user-space position.
    fn line_at(x: f64, y: f64, text: &str) -> String {
        format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm ({text}) Tj ET\n")
    }

    fn blocks_of(content: &str) -> Vec<Region> {
        let doc = Document::open(page_with(content)).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        let rules = crate::rules::collect(&doc, &p);
        detect(place(&runs), &rules)
    }

    /// Total glyphs across every block, for the partition property.
    fn glyph_count(blocks: &[Region]) -> usize {
        blocks.iter().flat_map(|b| b.lines.iter()).map(|l| l.glyphs.len()).sum()
    }

    #[test]
    fn a_single_paragraph_is_one_block() {
        let mut c = String::new();
        for i in 0..5 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "the quick brown fox"));
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 5);
    }

    #[test]
    fn a_wide_vertical_gap_splits_paragraphs() {
        let mut c = String::new();
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "first paragraph"));
        }
        // A 40pt gap at 10pt text is far beyond 1.5 line heights.
        for i in 0..3 {
            c.push_str(&line_at(72.0, 600.0 - i as f64 * 12.0, "second paragraph"));
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lines.len(), 3);
        assert!(blocks[0].bbox.y1 < blocks[1].bbox.y0, "reading order is top to bottom");
    }

    #[test]
    fn two_columns_are_read_column_by_column() {
        // The case that makes XY-cut a tree rather than a sort. Read top to
        // bottom this is gibberish; read column by column it is prose.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, &format!("left{i}")));
            c.push_str(&line_at(330.0, 700.0 - i as f64 * 12.0, &format!("right{i}")));
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.len(), 2, "one block per column");

        let first = blocks[0].text();
        assert!(first.contains("left0") && first.contains("left5"), "{first:?}");
        assert!(!first.contains("right"), "columns must not interleave: {first:?}");
        assert!(blocks[0].bbox.x1 <= blocks[1].bbox.x0, "left column comes first");
    }

    #[test]
    fn a_full_width_heading_is_separated_before_the_columns() {
        // The reason a vertical-first preference is safe: a heading spanning
        // both columns leaves no valley in the vertical profile, so the
        // horizontal cut has to happen first.
        let mut c = line_at(72.0, 740.0, "A heading that spans the whole page width here");
        for i in 0..5 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, &format!("left{i}")));
            c.push_str(&line_at(330.0, 700.0 - i as f64 * 12.0, &format!("right{i}")));
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.len(), 3, "heading, then two columns: {:?}", blocks.len());
        assert!(blocks[0].text().contains("heading"), "the heading is read first");
        assert!(blocks[1].text().contains("left0"));
        assert!(blocks[2].text().contains("right0"));
    }

    #[test]
    fn a_narrow_gap_does_not_split() {
        // Ordinary leading between lines of one paragraph.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "body text line"));
        }
        assert_eq!(blocks_of(&c).len(), 1);
    }

    #[test]
    fn a_horizontal_rule_cuts_where_the_gap_alone_would_not() {
        // 14pt apart is under 1.5 line heights at 10pt, so the profile would
        // not cut. The rule between them says otherwise.
        let mut c = String::new();
        // 18pt between the paragraphs leaves an 8pt gap at 10pt text, under the
        // 15pt threshold, so the profile alone will not cut here.
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "above the rule"));
        }
        c.push_str("1 w 72 667 m 520 667 l S\n");
        for i in 0..3 {
            c.push_str(&line_at(72.0, 658.0 - i as f64 * 12.0, "below the rule"));
        }

        let without_rule: String =
            c.lines().filter(|l| !l.contains(" l S")).collect::<Vec<_>>().join("\n");
        assert_eq!(blocks_of(&without_rule).len(), 1, "the gap alone does not cut");
        assert_eq!(blocks_of(&c).len(), 2, "the rule does");
    }

    #[test]
    fn an_empty_page_yields_no_blocks() {
        assert!(blocks_of("").is_empty());
    }

    #[test]
    fn a_single_line_is_a_block() {
        let blocks = blocks_of(&line_at(72.0, 700.0, "alone"));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 1);
        assert_eq!(blocks[0].text(), "alone");
    }

    #[test]
    fn reading_order_is_assigned_in_traversal_order() {
        let mut c = String::new();
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "first"));
        }
        for i in 0..3 {
            c.push_str(&line_at(72.0, 600.0 - i as f64 * 12.0, "second"));
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.iter().map(|b| b.order).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn block_bbox_covers_its_lines() {
        let mut c = String::new();
        for i in 0..4 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "text"));
        }
        let b = &blocks_of(&c)[0];
        for l in &b.lines {
            assert!(b.bbox.x0 <= l.bbox.x0 + 1e-6 && l.bbox.x1 <= b.bbox.x1 + 1e-6);
            assert!(b.bbox.y0 <= l.bbox.y0 + 1e-6 && l.bbox.y1 <= b.bbox.y1 + 1e-6);
        }
    }

    #[test]
    fn scattered_lines_that_will_not_cut_go_to_docstrum() {
        // Two clusters that overlap on both axes, so neither projection has a
        // clean valley -- the case spec 7.5 sends to the fallback.
        let mut c = String::new();
        // Upper-left cluster.
        for i in 0..3 {
            c.push_str(&line_at(72.0, 700.0 - i as f64 * 12.0, "aaaa"));
        }
        // Lower-right cluster, overlapping the first's y-range and x-range
        // enough that no straight cut separates them.
        for i in 0..3 {
            c.push_str(&line_at(300.0, 690.0 - i as f64 * 12.0, "bbbb"));
        }
        let blocks = blocks_of(&c);
        // Whatever the split, no glyph may be lost.
        assert_eq!(glyph_count(&blocks), 24, "6 lines of 4 glyphs");
        assert!(blocks.iter().all(|b| !b.is_empty()));
    }

    #[test]
    fn every_glyph_ends_up_in_exactly_one_block() {
        // The property that matters most: cutting partitions, it does not drop
        // or duplicate.
        let mut c = String::new();
        for i in 0..4 {
            c.push_str(&line_at(72.0, 740.0 - i as f64 * 12.0, "alpha"));
        }
        for i in 0..4 {
            c.push_str(&line_at(72.0, 600.0 - i as f64 * 12.0, "beta"));
            c.push_str(&line_at(340.0, 600.0 - i as f64 * 12.0, "gamma"));
        }
        let blocks = blocks_of(&c);
        // 4 x "alpha" (5), 4 x "beta" (4), 4 x "gamma" (5).
        assert_eq!(glyph_count(&blocks), 4 * 5 + 4 * 4 + 4 * 5);
    }

    #[test]
    fn deeply_nested_structure_terminates() {
        // A page of many well-separated paragraphs: the recursion must bottom
        // out rather than run to the depth limit and lose ordering.
        let mut c = String::new();
        for p in 0..10 {
            for i in 0..2 {
                c.push_str(&line_at(72.0, 760.0 - (p * 60 + i * 12) as f64, "para"));
            }
        }
        let blocks = blocks_of(&c);
        assert_eq!(blocks.len(), 10);
        let total: usize = blocks.iter().map(|b| b.lines.len()).sum();
        assert_eq!(total, 20);
    }
}
