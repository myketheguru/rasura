//! Table detection. Spec 7.7.
//!
//! **Table detection is a page-level operation, not a block-level one.** This is
//! not a style preference: §7.5's XY-cut sees a table's column gutters as
//! exactly the vertical valleys it exists to cut on, so by the time blocks are
//! available a 3×3 table has already become nine blocks. Asking "is this block
//! a table?" can therefore only ever answer no. Detection re-flattens the blocks
//! back to glyphs -- which is sound precisely because §7.5 guarantees blocks
//! partition them -- and works from the page.
//!
//! Two independent routes, because producers split roughly in half on which
//! evidence they leave behind:
//!
//! 1. **Ruled grids.** Word, InDesign and most HTML-to-PDF converters stroke or
//!    fill the borders, so the grid is literally in the content stream. This is
//!    the reliable route: the producer drew the table.
//! 2. **Column alignment.** LaTeX `tabular` without `\hline`, booktabs (which
//!    rules horizontally and never vertically), and every financial report set
//!    with tabs. Spec 7.7 gives the rule: at least three lines sharing at least
//!    two aligned column edges.
//!
//! `Table::origin` records which route fired, because a caller deciding whether
//! to offer cell editing should know whether the grid was drawn or inferred.

use crate::lines::{Line, PlacedGlyph};
use crate::paragraphs::{Paragraph, reconstruct};
use crate::rules::Rule;
use crate::{Region, ResolvedRun};
use rasura_content::matrix::Rect;

/// Two edges within this many points count as the same column edge. Generous
/// enough for the sub-point jitter of a real typesetter, tight enough not to
/// merge adjacent columns.
const EDGE_TOLERANCE: f64 = 2.0;

/// Spec 7.7: "≥3 lines sharing ≥2 aligned column edges".
const MIN_ROWS: usize = 3;
const MIN_COLUMNS: usize = 2;

/// A ruled grid needs this many distinct rules on each axis, giving at least a
/// 2×2 table. Two and two is a box around a paragraph.
const MIN_GRID_LINES: usize = 3;

/// A column edge must be preceded by whitespace of at least this multiple of
/// the font size. An ordinary inter-word space is about 0.25, so this is what
/// separates a column gutter from prose.
const GUTTER_FACTOR: f64 = 0.9;

/// Mean words per cell above which the region is prose in columns, not a table.
const MAX_WORDS_PER_CELL: f64 = 5.0;

/// Fraction of cells that must hold something. A grid where nine cells in ten
/// are empty is scattered text that happens to line up, not a table -- and it
/// is what an upper bound on density alone lets through.
const MIN_FILL: f64 = 0.5;

/// The same test for a drawn grid, set lower: the producer drew it, and a form
/// with many blank cells is still a table.
const RULED_MIN_FILL: f64 = 0.25;

/// Rules are clustered pairwise; beyond this many on one page the page is not a
/// table, it is a diagram or a map, and the quadratic pass is skipped.
const MAX_RULES: usize = 2000;

/// How the grid was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableOrigin {
    /// The producer drew the borders. Trustworthy.
    RuledGrid,
    /// Inferred from column edges that line up. Plausible, not certain.
    ColumnAlignment,
}

/// A cell. Empty cells are real cells with no content, because a table with
/// holes in its indexing is worse than useless to an editor.
#[derive(Debug, Clone)]
pub struct Cell {
    pub row: usize,
    pub column: usize,
    pub bbox: Rect,
    pub paragraphs: Vec<Paragraph>,
    pub lines: Vec<Line>,
}

impl Cell {
    pub fn text(&self) -> String {
        self.lines.iter().map(crate::line_text).collect::<Vec<_>>().join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct Table {
    pub rows: usize,
    pub cols: usize,
    /// Row-major, `rows * cols` entries, including empty ones.
    pub cells: Vec<Cell>,
    pub bbox: Rect,
    pub origin: TableOrigin,
    /// The x positions bounding each column and the y positions bounding each
    /// row, both inclusive of the outer edges. Kept because spec 7.7 makes a
    /// column-width change a separate explicit operation, and it needs these.
    pub column_edges: Vec<f64>,
    pub row_edges: Vec<f64>,
    source_glyphs: usize,
}

impl Table {
    pub fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        if row >= self.rows || column >= self.cols {
            return None;
        }
        self.cells.get(row * self.cols + column)
    }

    /// Whether the producer drew the grid rather than the geometry implying it.
    pub fn is_ruled(&self) -> bool {
        self.origin == TableOrigin::RuledGrid
    }

    /// Total words across all cells, over the cell count.
    fn words_per_cell(&self) -> f64 {
        if self.cells.is_empty() {
            return 0.0;
        }
        let words: usize =
            self.cells.iter().flat_map(|c| c.lines.iter()).map(|l| crate::segment(l).len()).sum();
        words as f64 / self.cells.len() as f64
    }

    /// Fraction of cells holding any text.
    pub fn fill(&self) -> f64 {
        if self.cells.is_empty() {
            return 0.0;
        }
        let filled = self.cells.iter().filter(|c| !c.is_empty()).count();
        filled as f64 / self.cells.len() as f64
    }

    /// Number of glyphs this table was built from. Cells partition exactly
    /// this many; anything else is a bug in the cell assignment.
    pub fn source_glyphs(&self) -> usize {
        self.source_glyphs
    }

    /// Glyphs actually held by cells.
    pub fn cell_glyphs(&self) -> usize {
        self.cells.iter().flat_map(|c| c.lines.iter()).map(|l| l.glyphs.len()).sum()
    }
}

/// Detect every table on a page.
///
/// Deliberately conservative. A false positive turns a paragraph into a grid
/// and makes editing it nonsense, which is worse than missing a table and
/// leaving the text as ordinary lines.
pub fn detect_page(blocks: &[Region], rules: &[Rule], runs: &[ResolvedRun]) -> Vec<Table> {
    // Sound because §7.5 guarantees blocks partition the page's glyphs.
    let glyphs: Vec<PlacedGlyph> =
        blocks.iter().flat_map(|b| b.lines.iter()).flat_map(|l| l.glyphs.iter().cloned()).collect();
    if glyphs.is_empty() {
        return Vec::new();
    }

    let mut out = from_rules(&glyphs, rules, runs);

    // Lines already claimed by a ruled grid are not offered to the weaker
    // route: a table that ruled only some of its borders would otherwise be
    // found twice.
    let claimed: Vec<Rect> = out.iter().map(|t| t.bbox).collect();
    let remaining: Vec<PlacedGlyph> = glyphs
        .iter()
        .filter(|g| !claimed.iter().any(|r| contains(r, g.origin.x, g.origin.y)))
        .cloned()
        .collect();
    if !remaining.is_empty() {
        out.extend(from_alignment(remaining, runs));
    }
    out
}

fn contains(r: &Rect, x: f64, y: f64) -> bool {
    x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
}

// --- route 1: the producer drew the grid ------------------------------------

fn from_rules(glyphs: &[PlacedGlyph], rules: &[Rule], runs: &[ResolvedRun]) -> Vec<Table> {
    if rules.len() > MAX_RULES {
        return Vec::new();
    }
    let mut out = Vec::new();
    for cluster in cluster_rules(rules) {
        let mut verticals: Vec<f64> =
            cluster.iter().filter(|r| !r.horizontal).map(|r| r.position()).collect();
        let mut horizontals: Vec<f64> =
            cluster.iter().filter(|r| r.horizontal).map(|r| r.position()).collect();
        dedupe(&mut verticals);
        dedupe(&mut horizontals);

        if verticals.len() < MIN_GRID_LINES || horizontals.len() < MIN_GRID_LINES {
            continue;
        }

        let region = Rect {
            x0: verticals[0],
            x1: verticals[verticals.len() - 1],
            y0: horizontals[0],
            y1: horizontals[horizontals.len() - 1],
        };
        let inside: Vec<PlacedGlyph> =
            glyphs.iter().filter(|g| contains(&region, g.origin.x, g.origin.y)).cloned().collect();
        if inside.is_empty() {
            continue;
        }
        let table = build(inside, runs, verticals, horizontals, region, TableOrigin::RuledGrid);
        // Gated more loosely than the inferred route, because the producer
        // having drawn the grid is real evidence and a form with empty cells is
        // still a table. But not ungated: `issue12810.pdf` yields an 89x80 grid
        // with seven filled cells out of 7,120 -- a chart's gridlines, which
        // connect into one cluster and satisfy every structural test.
        if table.fill() >= RULED_MIN_FILL {
            out.push(table);
        }
    }
    out
}

/// Group rules into connected clusters, so two tables on one page stay two.
fn cluster_rules(rules: &[Rule]) -> Vec<Vec<Rule>> {
    let n = rules.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, a) in rules.iter().enumerate() {
        for (j, b) in rules.iter().enumerate().skip(i + 1) {
            if touches(a, b) {
                let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<Rule>> = Default::default();
    for (i, rule) in rules.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(rule.clone());
    }
    groups.into_values().collect()
}

/// Whether two rules meet, allowing a small gap for the mitre a stroked corner
/// leaves behind.
fn touches(a: &Rule, b: &Rule) -> bool {
    const SLACK: f64 = 3.0;
    a.bbox.x0 - SLACK <= b.bbox.x1
        && b.bbox.x0 - SLACK <= a.bbox.x1
        && a.bbox.y0 - SLACK <= b.bbox.y1
        && b.bbox.y0 - SLACK <= a.bbox.y1
}

// --- route 2: the columns line up --------------------------------------------

fn from_alignment(glyphs: Vec<PlacedGlyph>, runs: &[ResolvedRun]) -> Vec<Table> {
    // Page-level lines, deliberately: a table row is one line spanning every
    // column, which is the same property §7.5 had to stop relying on.
    let lines = crate::assemble(glyphs);
    if lines.len() < MIN_ROWS {
        return Vec::new();
    }

    // A column edge is a word start preceded by a gutter. Requiring the gutter
    // is what stops prose from qualifying: an inter-word space is about a
    // quarter of the font size, a column gutter is most of it.
    let mut supports: Vec<Vec<f64>> = Vec::with_capacity(lines.len());
    for line in &lines {
        let words = crate::segment(line);
        let gutter = line.size.max(1.0) * GUTTER_FACTOR;
        let mut edges = Vec::new();
        for (i, w) in words.iter().enumerate() {
            if w.is_empty() {
                continue;
            }
            match i.checked_sub(1).and_then(|p| words.get(p)) {
                Some(prev) if w.bbox.x0 - prev.bbox.x1 >= gutter => edges.push(w.bbox.x0),
                // The first word of a line is the row's left edge, which every
                // left-aligned paragraph also has. Not evidence of a column.
                _ => {}
            }
        }
        supports.push(edges);
    }

    // Greedily extend runs of consecutive lines whose column edges agree.
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let mut end = start + 1;
        let mut common = supports[start].clone();
        while end < lines.len() {
            let next = intersect(&common, &supports[end]);
            if next.len() < MIN_COLUMNS {
                break;
            }
            common = next;
            end += 1;
        }

        if end - start >= MIN_ROWS && common.len() >= MIN_COLUMNS {
            if let Some(t) = build_aligned(&lines[start..end], &common, runs) {
                out.push(t);
                start = end;
                continue;
            }
        }
        start += 1;
    }
    out
}

fn intersect(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().filter(|x| b.iter().any(|y| (*x - y).abs() <= EDGE_TOLERANCE)).copied().collect()
}

fn build_aligned(lines: &[Line], columns: &[f64], runs: &[ResolvedRun]) -> Option<Table> {
    let x0 = lines.iter().map(|l| l.bbox.x0).fold(f64::MAX, f64::min);
    let x1 = lines.iter().map(|l| l.bbox.x1).fold(f64::MIN, f64::max);
    let y0 = lines.iter().map(|l| l.bbox.y0).fold(f64::MAX, f64::min);
    let y1 = lines.iter().map(|l| l.bbox.y1).fold(f64::MIN, f64::max);

    let mut column_edges = vec![x0];
    column_edges.extend_from_slice(columns);
    column_edges.push(x1);
    dedupe(&mut column_edges);

    // Each line is a row, split midway between consecutive line boxes.
    let mut row_edges = vec![y0];
    for pair in lines.windows(2) {
        row_edges.push((pair[0].bbox.y1 + pair[1].bbox.y0) / 2.0);
    }
    row_edges.push(y1);
    dedupe(&mut row_edges);

    if column_edges.len() < MIN_COLUMNS + 1 || row_edges.len() < MIN_ROWS + 1 {
        return None;
    }

    let glyphs: Vec<PlacedGlyph> = lines.iter().flat_map(|l| l.glyphs.iter().cloned()).collect();
    let region = Rect { x0, x1, y0, y1 };
    let table = build(glyphs, runs, column_edges, row_edges, region, TableOrigin::ColumnAlignment);

    // Spec 7.7's stated rule -- three lines, two aligned edges -- is necessary
    // but not sufficient, and the corpus proves it: two-column prose satisfies
    // it exactly. Every line has a word starting at the right column's left
    // edge, preceded by the gutter, on every line of the page. What separates a
    // real table is that its cells are *short*; a prose column holds a full
    // wrapped line. This guard is an addition to the spec, and it is the only
    // thing standing between table detection and every two-column paper in the
    // corpus.
    if table.words_per_cell() > MAX_WORDS_PER_CELL {
        return None;
    }
    // And the other end of the same axis. Bounding density only from above
    // admits the opposite failure: a large sparse grid trivially satisfies
    // "few words per cell" while being scattered text, not a table.
    if table.fill() < MIN_FILL {
        return None;
    }
    Some(table)
}

// --- shared construction ------------------------------------------------------

fn build(
    glyphs: Vec<PlacedGlyph>,
    runs: &[ResolvedRun],
    column_edges: Vec<f64>,
    row_edges: Vec<f64>,
    bbox: Rect,
    origin: TableOrigin,
) -> Table {
    let cols = column_edges.len() - 1;
    let rows = row_edges.len() - 1;

    let mut cells: Vec<Cell> = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            cells.push(Cell {
                row: r,
                column: c,
                bbox: Rect {
                    x0: column_edges[c],
                    x1: column_edges[c + 1],
                    y0: row_edges[r],
                    y1: row_edges[r + 1],
                },
                paragraphs: Vec::new(),
                lines: Vec::new(),
            });
        }
    }

    // Assign each *glyph* to a cell, then rebuild lines per cell. Assigning
    // whole lines would put a row's entire content in its first cell -- the
    // same mistake §7.5 made with blocks, for the same reason: a table row is
    // one line spanning every column.
    let source_glyphs = glyphs.len();
    let mut per_cell: Vec<Vec<PlacedGlyph>> = vec![Vec::new(); rows * cols];
    for g in glyphs {
        if let (Some(c), Some(r)) = (slot(&column_edges, g.origin.x), slot(&row_edges, g.origin.y))
        {
            per_cell[r * cols + c].push(g);
        }
    }

    for (i, glyphs) in per_cell.into_iter().enumerate() {
        if glyphs.is_empty() {
            continue;
        }
        let lines = crate::assemble(glyphs);
        let cell_block =
            Region { bbox: cells[i].bbox, lines, origin: crate::Origin::Whole, order: i };
        cells[i].paragraphs = reconstruct(&cell_block, runs);
        cells[i].lines = cell_block.lines;
    }

    Table { rows, cols, cells, bbox, origin, column_edges, row_edges, source_glyphs }
}

/// Which interval of `edges` contains `v`, clamped at both ends.
///
/// Clamping matters: the region is bounded by *rules* or by glyph boxes, while
/// this is called with a glyph *origin*, which is the baseline point and sits
/// below the box. A glyph a hair outside must land in the table, not vanish.
fn slot(edges: &[f64], v: f64) -> Option<usize> {
    if edges.len() < 2 {
        return None;
    }
    if v <= edges[1] {
        return Some(0);
    }
    if v >= edges[edges.len() - 2] {
        return Some(edges.len() - 2);
    }
    edges.windows(2).position(|w| v > w[0] && v <= w[1])
}

/// Sort and merge values within `EDGE_TOLERANCE`.
fn dedupe(values: &mut Vec<f64>) {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<f64> = Vec::with_capacity(values.len());
    for v in values.iter() {
        match out.last() {
            Some(last) if (v - last).abs() <= EDGE_TOLERANCE => {}
            _ => out.push(*v),
        }
    }
    *values = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{build_page, page_source};

    /// A 3x3 grid of ruled cells with a word in each.
    fn ruled_grid() -> String {
        let mut c = String::new();
        for x in [72.0, 172.0, 272.0, 372.0f64] {
            c.push_str(&format!("{x} 600 m {x} 700 l S\n"));
        }
        for y in [600.0, 633.0, 666.0, 700.0f64] {
            c.push_str(&format!("72 {y} m 372 {y} l S\n"));
        }
        for (r, y) in [680.0, 647.0, 614.0f64].iter().enumerate() {
            for (col, x) in [80.0, 180.0, 280.0f64].iter().enumerate() {
                c.push_str(&format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm (r{r}c{col}) Tj ET\n"));
            }
        }
        c
    }

    fn tables_of(content: &str) -> Vec<Table> {
        let (blocks, rules, runs) = build_page(content);
        detect_page(&blocks, &rules, &runs)
    }

    #[test]
    fn a_ruled_grid_is_a_table() {
        let tables = tables_of(&ruled_grid());
        assert_eq!(tables.len(), 1, "expected one table");
        assert!(tables[0].is_ruled());
        assert_eq!((tables[0].rows, tables[0].cols), (3, 3));
    }

    #[test]
    fn a_table_survives_the_xy_cut_having_split_it() {
        // The blocks are already nine separate boxes by the time detection
        // runs; the table is recovered from the page, not from a block.
        let (blocks, ..) = build_page(&ruled_grid());
        assert!(blocks.len() > 1, "the cut really does split a table apart");
        assert_eq!(tables_of(&ruled_grid()).len(), 1);
    }

    #[test]
    fn ruled_cells_hold_the_right_text() {
        let t = &tables_of(&ruled_grid())[0];
        for r in 0..3 {
            for c in 0..3 {
                let cell = t.cell(r, c).expect("cell in range");
                assert_eq!(cell.text().replace(' ', ""), format!("r{r}c{c}"));
            }
        }
    }

    #[test]
    fn a_row_spanning_line_is_split_across_cells_not_dumped_in_one() {
        let t = &tables_of(&ruled_grid())[0];
        for c in 0..3 {
            assert!(!t.cell(0, c).unwrap().is_empty(), "column {c} of row 0 is empty");
        }
    }

    #[test]
    fn a_missing_entry_is_still_an_indexed_cell() {
        // A table with a hole in its *indexing* is worse than useless to an
        // editor, so a blank entry must still be a cell at (r, c).
        let mut c = String::new();
        for x in [72.0, 172.0, 272.0, 372.0f64] {
            c.push_str(&format!("{x} 600 m {x} 700 l S\n"));
        }
        for y in [600.0, 633.0, 666.0, 700.0f64] {
            c.push_str(&format!("72 {y} m 372 {y} l S\n"));
        }
        for (r, y) in [680.0, 647.0, 614.0f64].iter().enumerate() {
            for (col, x) in [80.0, 180.0, 280.0f64].iter().enumerate() {
                if (r, col) == (1, 1) {
                    continue; // the hole
                }
                c.push_str(&format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm (r{r}c{col}) Tj ET\n"));
            }
        }
        let t = &tables_of(&c)[0];
        assert_eq!(t.cells.len(), 9);
        assert!(t.cell(1, 1).unwrap().is_empty(), "the hole is an empty cell, not a gap");
        assert_eq!(t.cell(2, 2).unwrap().text().replace(' ', ""), "r2c2");
    }

    #[test]
    fn a_charts_gridlines_are_not_a_table() {
        // issue12810.pdf: an 89x80 grid with seven filled cells out of 7,120.
        // Structurally a perfect grid; semantically a chart.
        let mut c = String::new();
        for i in 0..20 {
            let x = 72.0 + i as f64 * 20.0;
            c.push_str(&format!("{x} 400 m {x} 700 l S\n"));
        }
        for i in 0..15 {
            let y = 400.0 + i as f64 * 20.0;
            c.push_str(&format!("72 {y} m 452 {y} l S\n"));
        }
        c.push_str("BT /F1 8 Tf 1 0 0 1 80 500 Tm (1) Tj ET\n");
        c.push_str("BT /F1 8 Tf 1 0 0 1 200 560 Tm (2) Tj ET\n");
        assert!(tables_of(&c).is_empty(), "gridlines with almost no content are a chart");
    }

    #[test]
    fn a_box_around_a_paragraph_is_not_a_table() {
        let mut c = String::from(
            "72 600 m 72 700 l S\n372 600 m 372 700 l S\n\
             72 600 m 372 600 l S\n72 700 m 372 700 l S\n",
        );
        for i in 0..4 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 80 {} Tm (ordinary prose inside a box) Tj ET\n",
                680 - i * 12
            ));
        }
        assert!(tables_of(&c).is_empty(), "a border is not a grid");
    }

    #[test]
    fn two_tables_on_one_page_stay_two() {
        let mut c = String::new();
        for (top, bottom) in [(600.0, 700.0f64), (300.0, 400.0f64)] {
            for x in [72.0, 172.0, 272.0f64] {
                c.push_str(&format!("{x} {top} m {x} {bottom} l S\n"));
            }
            let step = (bottom - top) / 3.0;
            for k in 0..4 {
                let y = top + step * k as f64;
                c.push_str(&format!("72 {y} m 272 {y} l S\n"));
            }
            for r in 0..3 {
                for x in [80.0, 180.0f64] {
                    let y = top + step * r as f64 + 8.0;
                    c.push_str(&format!("BT /F1 8 Tf 1 0 0 1 {x} {y} Tm (v) Tj ET\n"));
                }
            }
        }
        assert_eq!(tables_of(&c).len(), 2, "connected clustering keeps them apart");
    }

    // --- the alignment route ------------------------------------------------

    #[test]
    fn aligned_columns_without_rules_are_a_table() {
        // LaTeX tabular without \hline, or a report set with tabs.
        let mut c = String::new();
        for i in 0..4 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {y} Tm (alpha) Tj \
                 1 0 0 1 200 {y} Tm (beta) Tj 1 0 0 1 320 {y} Tm (gamma) Tj ET\n",
                y = 700 - i * 14
            ));
        }
        let tables = tables_of(&c);
        assert_eq!(tables.len(), 1, "three aligned column edges over four rows");
        assert_eq!(tables[0].origin, TableOrigin::ColumnAlignment);
        assert!(tables[0].cols >= 3, "cols = {}", tables[0].cols);
    }

    #[test]
    fn prose_is_not_a_table() {
        // The single most important negative: a false positive turns a
        // paragraph into a grid and makes editing it nonsense.
        let words = [
            "the quick brown fox jumps over a lazy dog",
            "a second line of ordinary running prose here",
            "and a third with different words entirely now",
            "wrapping onward through several more lines of text",
            "until the paragraph finally comes to its end",
            "with nothing resembling a column anywhere in it",
        ];
        let c: String = words
            .iter()
            .enumerate()
            .map(|(i, w)| format!("BT /F1 10 Tf 1 0 0 1 72 {} Tm ({w}) Tj ET\n", 700 - i * 12))
            .collect();
        assert!(tables_of(&c).is_empty());
    }

    #[test]
    fn repeated_identical_prose_lines_are_not_a_table() {
        // Pathological but instructive: identical lines align *perfectly* at
        // every word start, so only the gutter requirement rejects them.
        let mut c = String::new();
        for i in 0..8 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm (identical prose on every line) Tj ET\n",
                700 - i * 12
            ));
        }
        assert!(tables_of(&c).is_empty(), "word spaces are not column gutters");
    }

    #[test]
    fn two_column_prose_is_not_a_table() {
        // Spec 7.7's stated rule is satisfied exactly by this page: every line
        // has a word starting at the right column's left edge, preceded by the
        // gutter. Only the words-per-cell guard rejects it.
        let mut c = String::new();
        for i in 0..12 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {y} Tm \
                 (a full wrapped line of prose in the left column) Tj ET\n\
                 BT /F1 10 Tf 1 0 0 1 330 {y} Tm \
                 (and a full wrapped line in the right column) Tj ET\n",
                y = 700 - i * 12
            ));
        }
        assert!(tables_of(&c).is_empty(), "two-column prose is not a table");
    }

    #[test]
    fn a_left_aligned_list_is_not_a_table() {
        // One aligned edge -- the left margin -- which every paragraph has.
        let c = page_source(
            &(0..5).map(|i| (72.0, 700.0 - i as f64 * 14.0, "a list item")).collect::<Vec<_>>(),
        );
        assert!(tables_of(&c).is_empty());
    }

    #[test]
    fn two_aligned_rows_are_not_enough() {
        // Spec 7.7 says three lines. Two aligned rows happen by chance.
        let mut c = String::new();
        for i in 0..2 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {y} Tm (alpha) Tj \
                 1 0 0 1 200 {y} Tm (beta) Tj 1 0 0 1 320 {y} Tm (gamma) Tj ET\n",
                y = 700 - i * 14
            ));
        }
        assert!(tables_of(&c).is_empty());
    }

    // --- invariants ------------------------------------------------------------

    #[test]
    fn cells_partition_the_glyphs_of_their_table() {
        {
            let (blocks, rules, runs) = build_page(&ruled_grid());
            let tables = detect_page(&blocks, &rules, &runs);
            let inside: usize = blocks
                .iter()
                .flat_map(|b| b.lines.iter())
                .flat_map(|l| l.glyphs.iter())
                .filter(|g| tables.iter().any(|t| contains(&t.bbox, g.origin.x, g.origin.y)))
                .count();
            let out: usize = tables
                .iter()
                .flat_map(|t| t.cells.iter())
                .flat_map(|c| c.lines.iter())
                .map(|l| l.glyphs.len())
                .sum();
            assert_eq!(inside, out, "cells must partition the glyphs in the table region");
        }
    }

    #[test]
    fn out_of_range_cells_are_none() {
        let t = &tables_of(&ruled_grid())[0];
        assert!(t.cell(3, 0).is_none());
        assert!(t.cell(0, 3).is_none());
    }

    #[test]
    fn an_empty_page_has_no_tables() {
        assert!(detect_page(&[], &[], &[]).is_empty());
    }
}
