//! Line assembly. Spec 7.4.
//!
//! > Cluster glyphs into lines by baseline in **device space** (after CTM), not
//! > text space -- rotated and skewed text must still form lines.
//!
//! That instruction shapes everything here. A page can set a rotated CTM and
//! then emit perfectly ordinary text-space coordinates, so two glyphs with the
//! same text-space `y` may be nowhere near each other on the page, and two
//! glyphs on the same visual line may have completely different text-space
//! coordinates. Working in device space costs a projection per glyph and makes
//! rotated and skewed text fall out for free.
//!
//! Each glyph is described by two scalars in device space:
//!
//! * `tangent` -- how far along its own baseline it sits, which orders glyphs
//!   within a line;
//! * `normal` -- its perpendicular offset, which is constant along a baseline
//!   and is therefore what lines are clustered on.

use crate::extract::ResolvedRun;
use crate::unicode::Strategy;
use rasura_content::matrix::{Point, Rect};
use std::ops::Range;

/// Lines whose baselines differ by less than this fraction of the font size are
/// the same line. Spec 7.4 gives 0.3.
const BASELINE_TOLERANCE: f64 = 0.3;

/// A size drop beyond this, with a small baseline offset, reads as a
/// super/subscript rather than as a new line. Spec 7.4 gives 20%.
const SUPERSCRIPT_SIZE_DROP: f64 = 0.8;

/// How far off the baseline a super/subscript may sit and still belong to its
/// parent line. Spec 7.4 gives 0.6.
const SUPERSCRIPT_OFFSET: f64 = 0.6;

/// Baseline directions within this many radians are the same direction. About
/// half a degree, which separates deliberately rotated text from the tiny skews
/// that accumulate through nested form matrices.
const DIRECTION_TOLERANCE: f64 = 0.01;

/// One glyph, placed in device space and traceable back to its operator.
#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    /// Resolved text. `None` means the derivation chain could not map it.
    pub text: Option<String>,
    pub code: u32,
    pub strategy: Strategy,
    /// Device-space origin.
    pub origin: Point,
    /// Advance along the baseline, in device space.
    pub advance: f64,
    /// Effective font size in device space.
    pub size: f64,
    /// Baseline direction, radians.
    pub direction: f64,
    /// Distance along the baseline. Orders glyphs within a line.
    pub tangent: f64,
    /// Perpendicular offset. Constant along a baseline.
    pub normal: f64,
    /// `Ts`, non-zero for an explicitly raised or lowered glyph.
    pub rise: f64,
    /// Which `ResolvedRun` this came from, and where in it.
    ///
    /// Spec 7.4: sort by position for reading, but retain the original operator
    /// order for patching. This is that retention -- the edit layer needs to get
    /// back to the exact operator and byte range.
    pub run: usize,
    pub index: usize,
    /// Byte range within the showing operator's string.
    pub span: Range<usize>,
}

impl PlacedGlyph {
    pub fn is_mapped(&self) -> bool {
        self.text.is_some()
    }

    /// True when this glyph sits off its line's baseline, either because `Ts`
    /// says so or because it is small and offset.
    pub fn is_shifted(&self) -> bool {
        self.rise.abs() > f64::EPSILON
    }
}

/// A visual line: glyphs sharing a baseline, in reading order along it.
#[derive(Debug, Clone)]
pub struct Line {
    pub glyphs: Vec<PlacedGlyph>,
    /// Perpendicular position of the baseline in device space.
    pub baseline: f64,
    /// Baseline direction in radians. Zero for ordinary horizontal text.
    pub direction: f64,
    /// Modal font size of the line's glyphs.
    pub size: f64,
    pub bbox: Rect,
}

impl Line {
    /// The line's text, unresolved glyphs omitted.
    pub fn text(&self) -> String {
        self.glyphs.iter().filter_map(|g| g.text.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Where the line starts and ends along its own baseline.
    pub fn extent(&self) -> (f64, f64) {
        let first = self.glyphs.first().map(|g| g.tangent).unwrap_or(0.0);
        let last = self.glyphs.last().map(|g| g.tangent + g.advance).unwrap_or(first);
        (first, last)
    }

    /// True when the line is horizontal in device space, which is the case for
    /// the overwhelming majority of content and permits simpler downstream
    /// geometry.
    pub fn is_horizontal(&self) -> bool {
        self.direction.abs() < DIRECTION_TOLERANCE
    }
}

/// Flatten resolved runs into device-space glyphs.
///
/// Device advances are derived from the text rendering matrix, never measured
/// from the distance between consecutive origins -- see the comment on `ratio`
/// below for why the obvious approach is wrong.
pub fn place(runs: &[ResolvedRun]) -> Vec<PlacedGlyph> {
    let mut out = Vec::new();

    for (run_index, resolved) in runs.iter().enumerate() {
        let run = &resolved.run;
        let trm = run.trm;

        // Spec 7.4 clusters by *baseline*, and in vertical writing mode the
        // baseline runs down the page — a fact the text matrix does not carry.
        // `/WMode 1` is a property of the font's CMap, so an unrotated matrix
        // and a vertical font produce glyphs that share an x and differ in y:
        // clustering them as though the baseline were horizontal makes every
        // glyph its own line, and a CJK page becomes one line per character.
        //
        // Rotating the basis by a quarter turn fixes both halves at once.
        // `tangent` becomes the distance down the column, which is reading
        // order within it; `normal` becomes `-x`, which is constant down a
        // column and *ascends leftward* — so the existing "sort lines by
        // ascending normal" orders columns right to left, which is the reading
        // order vertical Japanese and Chinese use.
        let direction = if run.vertical {
            trm.rotation() + std::f64::consts::FRAC_PI_2
        } else {
            trm.rotation()
        };
        let (sin, cos) = direction.sin_cos();

        // Font size as it appears on the page, not as `Tf` set it: a form
        // XObject or a page CTM can scale text arbitrarily.
        let (scale_x, scale_y) = trm.expansion();
        let device_size = if scale_y.is_finite() && scale_y > 0.0 { scale_y } else { run.size };

        // Text-space advances scale into device space by whatever the text
        // matrix and CTM do, which is the rendering matrix with the font size
        // and horizontal scale divided back out -- both are folded into it.
        //
        // Deliberately computed rather than measured from consecutive origins.
        // Measuring looks attractive and is wrong: a `TJ` adjustment between two
        // glyphs inflates the distance between them, so the "measured" ratio
        // absorbs the adjustment and the gap it created then vanishes -- which
        // is exactly the gap word segmentation exists to detect.
        //
        // Vertically, the advance is a `ty` and `Tz` does not apply to it
        // (§9.4.4), so the parameters divided back out are different ones.
        // Using the horizontal pair on a vertical run scales every advance by
        // the horizontal scale the spec says to ignore.
        let (params, scale) = if run.vertical {
            (run.size, scale_y)
        } else {
            (run.size * (run.horizontal_scale / 100.0), scale_x)
        };
        let ratio =
            if params.abs() > f64::EPSILON && scale.is_finite() { scale / params } else { 1.0 };

        for (i, glyph) in run.glyphs.iter().enumerate() {
            let p = glyph.origin;
            out.push(PlacedGlyph {
                text: resolved.text.get(i).cloned().flatten(),
                code: glyph.code,
                strategy: resolved.strategies.get(i).copied().unwrap_or(Strategy::Failed),
                origin: p,
                advance: glyph.advance * ratio,
                size: device_size,
                direction,
                tangent: p.x * cos + p.y * sin,
                normal: -p.x * sin + p.y * cos,
                rise: run.rise,
                run: run_index,
                index: i,
                span: glyph.span.clone(),
            });
        }
    }
    out
}

/// Assemble glyphs into lines.
pub fn assemble(glyphs: Vec<PlacedGlyph>) -> Vec<Line> {
    if glyphs.is_empty() {
        return Vec::new();
    }

    // Group by baseline direction first. Text at different angles cannot share
    // a line however close the glyphs are, and comparing normals across
    // directions is meaningless.
    let mut buckets: Vec<(f64, Vec<PlacedGlyph>)> = Vec::new();
    for g in glyphs {
        match buckets.iter_mut().find(|(d, _)| angle_close(*d, g.direction)) {
            Some((_, v)) => v.push(g),
            None => buckets.push((g.direction, vec![g])),
        }
    }

    let mut lines = Vec::new();
    for (_, mut bucket) in buckets {
        bucket.sort_by(|a, b| a.normal.partial_cmp(&b.normal).unwrap_or(std::cmp::Ordering::Equal));
        lines.extend(cluster_bucket(bucket));
    }

    // Reading order across lines: down the page, then along. Device space has y
    // increasing downwards, so ascending normal is top to bottom for horizontal
    // text.
    lines.sort_by(|a, b| {
        a.baseline.partial_cmp(&b.baseline).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
            a.extent().0.partial_cmp(&b.extent().0).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    lines
}

/// Cluster one direction's glyphs, already sorted by normal.
fn cluster_bucket(glyphs: Vec<PlacedGlyph>) -> Vec<Line> {
    let mut clusters: Vec<Vec<PlacedGlyph>> = Vec::new();
    let mut current: Vec<PlacedGlyph> = Vec::new();

    for g in glyphs {
        match current.last() {
            None => current.push(g),
            Some(prev) => {
                // Tolerance scales with the *larger* of the two sizes, so a
                // small glyph next to a large one does not split a line that a
                // reader sees as one.
                let tolerance = BASELINE_TOLERANCE * prev.size.max(g.size).max(1.0);
                if (g.normal - prev.normal).abs() <= tolerance {
                    current.push(g);
                } else {
                    clusters.push(std::mem::take(&mut current));
                    current.push(g);
                }
            }
        }
    }
    if !current.is_empty() {
        clusters.push(current);
    }

    merge_superscripts(&mut clusters);

    clusters
        .into_iter()
        .filter(|c| !c.is_empty())
        .map(|c| build_line(in_reading_order(c)))
        .collect()
}

/// Order one line's glyphs for reading, keeping each show operation whole.
///
/// Spec 7.4 asks for position order within a line, because producers emit text
/// out of visual order routinely: footnote markers, ligature fixups, columns
/// written bottom-up. That was read as "sort the glyphs", and sorting glyphs
/// across run boundaries interleaves them the moment two runs overlap in x.
///
/// A real form did exactly that. `Carmen Fari(n~a),` was drawn at x=50 in bold
/// and `Chancellor` at x=112 in oblique, and the first run is about 65pt wide,
/// so its comma sits at x≈113 -- past where the second run starts. Sorted by
/// position alone the comma falls after the `C` and the page reads
/// `Carmen Fari(n~a)C,hancellor`. Every overlapping label on that form came out
/// the same way, and the confidence stayed `exact` throughout, because every
/// glyph *was* mapped exactly. Only their order was wrong.
///
/// The unit of reading order is the run, not the character. One show operation
/// lays its glyphs out itself and they are contiguous by construction, so
/// nothing is gained by re-sorting inside one and correctness is lost. Runs are
/// ordered by where they start, which is the original intent, and `index`
/// orders within a run, which is what the producer said.
fn in_reading_order(glyphs: Vec<PlacedGlyph>) -> Vec<PlacedGlyph> {
    // Where each run begins along the baseline, and where it first appeared, so
    // two runs starting at the same place keep a deterministic order.
    let mut starts: Vec<(usize, f64, usize)> = Vec::new();
    for (seen, g) in glyphs.iter().enumerate() {
        match starts.iter_mut().find(|(r, _, _)| *r == g.run) {
            Some((_, at, _)) => *at = at.min(g.tangent),
            None => starts.push((g.run, g.tangent, seen)),
        }
    }

    let key = |g: &PlacedGlyph| {
        starts
            .iter()
            .find(|(r, _, _)| *r == g.run)
            .map(|(_, at, seen)| (*at, *seen))
            .unwrap_or((g.tangent, 0))
    };

    let mut out = glyphs;
    out.sort_by(|a, b| {
        let (at_a, seen_a) = key(a);
        let (at_b, seen_b) = key(b);
        at_a.partial_cmp(&at_b)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| seen_a.cmp(&seen_b))
            .then_with(|| a.index.cmp(&b.index))
    });
    out
}

/// Fold small offset clusters into the line they belong to. Spec 7.4:
/// superscripts and subscripts join the parent line as a styled run, not as a
/// separate line.
fn merge_superscripts(clusters: &mut Vec<Vec<PlacedGlyph>>) {
    if clusters.len() < 2 {
        return;
    }
    let mut merged = vec![false; clusters.len()];

    for i in 0..clusters.len() {
        if merged[i] || clusters[i].is_empty() {
            continue;
        }
        let size = modal_size(&clusters[i]);
        let normal = median_normal(&clusters[i]);

        for j in 0..clusters.len() {
            if i == j || merged[j] || clusters[j].is_empty() {
                continue;
            }
            let other_size = modal_size(&clusters[j]);
            let other_normal = median_normal(&clusters[j]);

            let is_smaller = other_size < size * SUPERSCRIPT_SIZE_DROP;
            let is_close = (other_normal - normal).abs() < SUPERSCRIPT_OFFSET * size;
            // An explicit `Ts` is decisive; the size test is the fallback for
            // producers that shift the baseline with `Td` instead.
            let explicit = clusters[j].iter().all(|g| g.is_shifted());

            if is_close && (is_smaller || explicit) {
                let taken = std::mem::take(&mut clusters[j]);
                clusters[i].extend(taken);
                merged[j] = true;
            }
        }
    }
    clusters.retain(|c| !c.is_empty());
}

fn build_line(glyphs: Vec<PlacedGlyph>) -> Line {
    let size = modal_size(&glyphs);
    let direction = glyphs.first().map(|g| g.direction).unwrap_or(0.0);
    let baseline = median_normal(&glyphs);

    let mut bbox = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for g in &glyphs {
        // A glyph box from its origin: the advance along the baseline, and
        // roughly the em box above it. Exact outlines are Phase 4.
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

    Line { glyphs, baseline, direction, size, bbox }
}

/// The most common size, falling back to the median. Modal rather than mean
/// because one large drop-cap should not redefine the line's size.
fn modal_size(glyphs: &[PlacedGlyph]) -> f64 {
    if glyphs.is_empty() {
        return 0.0;
    }
    let mut sizes: Vec<f64> = glyphs.iter().map(|g| g.size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut best = sizes[0];
    let mut best_count = 0usize;
    let mut i = 0;
    while i < sizes.len() {
        let mut j = i;
        // Sizes within a twentieth of a point are the same size.
        while j < sizes.len() && (sizes[j] - sizes[i]).abs() < 0.05 {
            j += 1;
        }
        if j - i > best_count {
            best_count = j - i;
            best = sizes[i];
        }
        i = j;
    }
    best
}

fn median_normal(glyphs: &[PlacedGlyph]) -> f64 {
    if glyphs.is_empty() {
        return 0.0;
    }
    let mut n: Vec<f64> = glyphs.iter().map(|g| g.normal).collect();
    n.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    n[n.len() / 2]
}

fn angle_close(a: f64, b: f64) -> bool {
    let mut d = (a - b).abs() % std::f64::consts::TAU;
    if d > std::f64::consts::PI {
        d = std::f64::consts::TAU - d;
    }
    d < DIRECTION_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve_page;
    use rasura_content::page;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page with a fixed-width font whose every glyph is 500/1000 wide, so
    /// positions are exactly predictable.
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

    fn lines_of(content: &str) -> Vec<Line> {
        let doc = Document::open(page_with(content)).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        assemble(place(&runs))
    }

    /// A page whose font is `/Identity-V`: two-byte CIDs, written downward.
    ///
    /// `/W` gives every CID an advance of 1000, so a glyph occupies a full em
    /// down the column and the positions are exactly predictable.
    fn vertical_page(content: &str) -> Vec<u8> {
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
                "<< /Type /Font /Subtype /Type0 /BaseFont /Mincho /Encoding /Identity-V \
                 /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>",
            )
            .object(
                6,
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Mincho \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /DW 1000 /DW2 [880 -1000] >>",
            )
            .stream(
                7,
                "",
                b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                  1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                  3 beginbfchar\n<0001> <3042>\n<0002> <3044>\n<0003> <3046>\nendbfchar\n\
                  endcmap CMapName currentdict /CMap defineresource pop end end",
            )
            .finish("/Root 1 0 R")
    }

    fn vertical_lines(content: &str) -> Vec<Line> {
        let doc = Document::open(vertical_page(content)).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        assemble(place(&runs))
    }

    #[test]
    fn a_vertical_column_is_one_line_not_one_per_glyph() {
        // The failure this fixes. `/WMode 1` is a property of the font's CMap,
        // not of the text matrix, so an unrotated matrix and a vertical font
        // give glyphs that share an x and differ in y -- and clustering by a
        // horizontal baseline makes every character its own line.
        let lines = vertical_lines("BT /F1 20 Tf 1 0 0 1 300 700 Tm <000100020003> Tj ET");
        assert_eq!(lines.len(), 1, "one column, not three lines: {lines:#?}");
        assert_eq!(lines[0].text(), "\u{3042}\u{3044}\u{3046}");
        assert!(!lines[0].is_horizontal(), "the baseline runs down the page");
    }

    #[test]
    fn vertical_columns_are_ordered_right_to_left() {
        // Which is how vertical Japanese and Chinese are read, and the opposite
        // of the order the same x-positions would give horizontal text.
        let lines = vertical_lines(
            "BT /F1 20 Tf 1 0 0 1 300 700 Tm <00010002> Tj \
                          1 0 0 1 260 700 Tm <00030001> Tj ET",
        );
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert_eq!(lines[0].text(), "\u{3042}\u{3044}", "the rightmost column first");
        assert_eq!(lines[1].text(), "\u{3046}\u{3042}");
    }

    #[test]
    fn glyphs_within_a_vertical_column_read_downward() {
        // Emitted out of order on purpose: reading order is by position, and
        // "position" down a column is the opposite direction to a page's y.
        let lines = vertical_lines(
            "BT /F1 20 Tf 1 0 0 1 300 660 Tm <0003> Tj \
                          1 0 0 1 300 700 Tm <0001> Tj \
                          1 0 0 1 300 680 Tm <0002> Tj ET",
        );
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert_eq!(lines[0].text(), "\u{3042}\u{3044}\u{3046}", "top of the column first");
    }

    #[test]
    fn one_showing_operator_is_one_line() {
        let lines = lines_of("BT /F1 10 Tf 72 700 Td (Hello world) Tj ET");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Hello world");
        assert!(lines[0].is_horizontal());
        assert!((lines[0].size - 10.0).abs() < 1e-6);
    }

    #[test]
    fn separate_baselines_are_separate_lines_in_reading_order() {
        let lines = lines_of(
            "BT /F1 10 Tf 72 700 Td (first) Tj 0 -20 Td (second) Tj 0 -20 Td (third) Tj ET",
        );
        assert_eq!(lines.len(), 3);
        let texts: Vec<String> = lines.iter().map(|l| l.text()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn glyphs_from_different_operators_join_one_line() {
        // The "one Tj per character" pattern some producers emit. Each glyph is
        // its own operator; they must still form a single line.
        let mut content = String::from("BT /F1 10 Tf 72 700 Td");
        for (i, c) in "ABCDE".chars().enumerate() {
            content.push_str(&format!(" 1 0 0 1 {} 700 Tm ({c}) Tj", 72.0 + i as f64 * 5.0));
        }
        content.push_str(" ET");
        let lines = lines_of(&content);
        assert_eq!(lines.len(), 1, "five operators, one visual line");
        assert_eq!(lines[0].text(), "ABCDE");
    }

    #[test]
    fn glyphs_are_ordered_visually_not_by_operator() {
        // A producer emitting the middle of a line last -- a footnote marker or
        // a ligature fixup. Reading order is by position.
        let content = "BT /F1 10 Tf \
                       1 0 0 1 100 700 Tm (C) Tj \
                       1 0 0 1 72 700 Tm (A) Tj \
                       1 0 0 1 86 700 Tm (B) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "ABC");
        // And the operator order is retained for patching.
        let order: Vec<usize> = lines[0].glyphs.iter().map(|g| g.run).collect();
        assert_eq!(order, vec![1, 2, 0], "original run indices survive the sort");
    }

    #[test]
    fn rotated_text_still_forms_a_line() {
        // The reason spec 7.4 insists on device space: these glyphs have
        // identical text-space y but sit on a diagonal on the page.
        let lines =
            lines_of("BT /F1 10 Tf 0.7071 0.7071 -0.7071 0.7071 100 400 Tm (Diagonal) Tj ET");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Diagonal");
        assert!(!lines[0].is_horizontal(), "the baseline is rotated");
        assert!(lines[0].direction.abs() > 0.1);
    }

    #[test]
    fn text_at_different_angles_does_not_share_a_line() {
        let content = "BT /F1 10 Tf 1 0 0 1 100 400 Tm (flat) Tj \
                       0 1 -1 0 100 400 Tm (turned) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 2, "different baseline directions are different lines");
    }

    #[test]
    fn vertically_offset_text_at_the_same_x_forms_separate_lines() {
        let lines =
            lines_of("BT /F1 10 Tf 1 0 0 1 72 700 Tm (top) Tj 1 0 0 1 72 680 Tm (bottom) Tj ET");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "top");
        assert_eq!(lines[1].text(), "bottom");
        assert!(lines[0].baseline < lines[1].baseline, "reading order is down the page");
    }

    #[test]
    fn a_small_baseline_wobble_does_not_split_a_line() {
        // Half a point of drift at 10pt is well inside the 0.3 tolerance.
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (a) Tj \
                       1 0 0 1 80 700.5 Tm (b) Tj 1 0 0 1 88 699.6 Tm (c) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "abc");
    }

    #[test]
    fn a_superscript_joins_its_parent_line() {
        // Spec 7.4: a raised, smaller run is part of the line, not a new one.
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (x) Tj \
                       /F1 6 Tf 1 0 0 1 78 704 Tm (2) Tj \
                       /F1 10 Tf 1 0 0 1 82 700 Tm ( plus) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 1, "superscript must not become its own line");
        assert_eq!(lines[0].text(), "x2 plus");
        assert!((lines[0].size - 10.0).abs() < 1e-6, "the modal size is the body size");
    }

    #[test]
    fn a_subscript_joins_its_parent_line() {
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (H) Tj \
                       /F1 6 Tf 1 0 0 1 78 697 Tm (2) Tj \
                       /F1 10 Tf 1 0 0 1 82 700 Tm (O) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "H2O");
    }

    #[test]
    fn a_genuinely_separate_small_line_is_not_absorbed() {
        // Small text far from the body line is a line of its own, not a
        // subscript. 30pt at 10pt body is well beyond 0.6 x size.
        let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (body) Tj \
                       /F1 6 Tf 1 0 0 1 72 660 Tm (caption) Tj ET";
        let lines = lines_of(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "body");
        assert_eq!(lines[1].text(), "caption");
    }

    #[test]
    fn line_bbox_covers_its_glyphs() {
        let lines = lines_of("BT /F1 10 Tf 72 700 Td (Hello) Tj ET");
        let bbox = lines[0].bbox;
        for g in &lines[0].glyphs {
            assert!(
                bbox.x0 <= g.origin.x + 1e-6 && g.origin.x <= bbox.x1 + 1e-6,
                "{:?} outside {bbox:?}",
                g.origin
            );
        }
        // Five glyphs at 5pt each.
        assert!((bbox.x1 - bbox.x0 - 25.0).abs() < 0.5, "{bbox:?}");
    }

    #[test]
    fn device_advances_survive_a_scaling_ctm() {
        // A CTM that doubles everything must double the advances, or every
        // downstream gap measurement is wrong by a factor of two.
        let plain = lines_of("BT /F1 10 Tf 1 0 0 1 72 700 Tm (AB) Tj ET");
        let scaled = lines_of("2 0 0 2 0 0 cm BT /F1 10 Tf 1 0 0 1 36 350 Tm (AB) Tj ET");
        let a = plain[0].glyphs[0].advance;
        let b = scaled[0].glyphs[0].advance;
        assert!((b - 2.0 * a).abs() < 1e-6, "{a} then {b}");
        assert!((scaled[0].size - 20.0).abs() < 1e-6, "device size doubles too");
    }

    #[test]
    fn a_tj_adjustment_does_not_contaminate_the_advance() {
        // Regression. An earlier version derived the text-to-device advance
        // ratio by measuring consecutive origins. A `TJ` adjustment inflates
        // that distance, so the ratio absorbed the adjustment, the advance came
        // out equal to the gap, and the gap word segmentation exists to detect
        // vanished. The ratio comes from the matrix now.
        let plain = lines_of("BT /F1 10 Tf 72 700 Td (ab) Tj ET");
        let adjusted = lines_of("BT /F1 10 Tf 72 700 Td [(a) -900 (b)] TJ ET");
        assert!((plain[0].glyphs[0].advance - 5.0).abs() < 1e-6);
        assert!(
            (adjusted[0].glyphs[0].advance - 5.0).abs() < 1e-6,
            "the advance is the glyph's width, not the distance to the next glyph: {}",
            adjusted[0].glyphs[0].advance
        );
        // And the gap is therefore visible: 9pt of adjustment on a 5pt advance.
        let g = &adjusted[0].glyphs;
        assert!((g[1].tangent - g[0].tangent - g[0].advance - 9.0).abs() < 1e-6);
    }

    #[test]
    fn horizontal_scale_is_divided_out_of_the_advance() {
        // Tz is folded into the rendering matrix along with the font size, so
        // both have to come back out or every advance is scaled twice.
        let plain = lines_of("BT /F1 10 Tf 1 0 0 1 72 700 Tm (ab) Tj ET");
        let stretched = lines_of("BT /F1 10 Tf 200 Tz 1 0 0 1 72 700 Tm (ab) Tj ET");
        // Tz doubles the advance in device space.
        assert!((plain[0].glyphs[0].advance - 5.0).abs() < 1e-6);
        assert!(
            (stretched[0].glyphs[0].advance - 10.0).abs() < 1e-6,
            "{}",
            stretched[0].glyphs[0].advance
        );
        // And the measured gap between glyphs matches.
        let g = &stretched[0].glyphs;
        assert!((g[1].tangent - g[0].tangent - 10.0).abs() < 1e-6);
    }

    #[test]
    fn an_empty_page_yields_no_lines() {
        assert!(lines_of("").is_empty());
        assert!(lines_of("BT ET").is_empty());
    }

    #[test]
    fn unmapped_glyphs_are_still_placed() {
        // Positions are geometry and do not depend on the derivation chain.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 72 700 Td (AB) Tj ET")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /X /FontDescriptor 6 0 R >>")
            .object(6, "<< /Type /FontDescriptor /Flags 4 >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _) = resolve_page(&doc, &p);
        let lines = assemble(place(&runs));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].glyphs.len(), 2);
        assert!(lines[0].glyphs.iter().all(|g| !g.is_mapped()));
        assert_eq!(lines[0].text(), "");
    }
}
