//! Ruling lines. Spec 7.5.
//!
//! > Collect stroked paths from the content stream before cutting. A horizontal
//! > rule is a strong cut hint; a rectangle grid is a table signal.
//!
//! Two shapes count as a rule, because producers use both interchangeably:
//!
//! * a stroked segment -- `m`/`l` then `S`;
//! * a filled rectangle thin enough to read as a line -- `re` then `f`. This is
//!   how Word, InDesign and most HTML-to-PDF converters draw borders, so a
//!   collector that only looked at strokes would miss most real tables.
//!
//! Everything is recorded in device space, so a rule inside a rotated form
//! XObject lands where it is drawn rather than where its operands say.

use rasura_content::matrix::{Matrix, Point, Rect};
use rasura_content::op::{Op, OpKind};
use rasura_content::page::Page;
use rasura_content::state::StateMachine;
use rasura_content::walker::{ContentVisitor, Flow, WalkContext, walk_page};
use rasura_cos::document::Document;

/// A shape this thin, in device points, reads as a line rather than as a box.
const MAX_THICKNESS: f64 = 3.0;

/// Shorter than this and it is a tick or a bullet, not a rule.
const MIN_LENGTH: f64 = 8.0;

/// A ruling line, in device space.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub bbox: Rect,
    pub horizontal: bool,
    /// The thin dimension.
    pub thickness: f64,
    /// True when this came from a filled rectangle rather than a stroke.
    pub filled: bool,
}

impl Rule {
    /// Length along the rule's own direction.
    pub fn length(&self) -> f64 {
        if self.horizontal { self.bbox.width() } else { self.bbox.height() }
    }

    /// Position across the rule: its `y` for a horizontal rule, `x` for a
    /// vertical one.
    pub fn position(&self) -> f64 {
        if self.horizontal {
            (self.bbox.y0 + self.bbox.y1) / 2.0
        } else {
            (self.bbox.x0 + self.bbox.x1) / 2.0
        }
    }

    /// Whether this rule spans most of `region`, which is what makes it a cut
    /// hint rather than an underline under one word.
    pub fn spans(&self, region: &Rect, fraction: f64) -> bool {
        if self.horizontal {
            self.length() >= region.width() * fraction
        } else {
            self.length() >= region.height() * fraction
        }
    }
}

/// Collect every ruling line on a page.
pub fn collect(doc: &Document, page: &Page) -> Vec<Rule> {
    let mut visitor = RuleCollector::default();
    walk_page(doc, page, &mut visitor);
    visitor.rules
}

#[derive(Default)]
struct RuleCollector {
    rules: Vec<Rule>,
    /// Straight segments of the path under construction, in device space.
    ///
    /// Segments rather than polylines, because a curve's endpoints say nothing
    /// about the path between them. Recording only points and joining
    /// consecutive ones turns a Bézier arc from (72,700) to (500,700) into a
    /// horizontal rule, which it very much is not.
    segments: Vec<(Point, Point)>,
    /// Rectangles, whose shape is known exactly and needs no analysis.
    rects: Vec<Rect>,
    current: Option<Point>,
    subpath_start: Option<Point>,
}

impl RuleCollector {
    fn clear_path(&mut self) {
        self.segments.clear();
        self.rects.clear();
        self.current = None;
        self.subpath_start = None;
    }

    /// Turn the current path into rules, if the paint operator drew anything.
    fn paint(&mut self, stroked: bool, filled: bool, line_width: f64, ctm: &Matrix) {
        // A stroke's thickness is its line width in device space; a fill's is
        // the shape's own thin dimension.
        let (sx, sy) = ctm.expansion();
        let stroke_thickness = line_width * ((sx + sy) / 2.0).max(0.0);

        let rects = std::mem::take(&mut self.rects);
        for r in rects {
            if filled {
                self.emit_rect(r, true);
            } else if stroked {
                // A stroked rectangle is a box outline, which is four rules and
                // the strongest table signal there is.
                self.emit_box_edges(r, stroke_thickness);
            }
        }

        // Only strokes: filling a zero-area segment draws nothing, and a filled
        // sliver would need polygon analysis that `re` + `f` already covers.
        if stroked {
            let segments = std::mem::take(&mut self.segments);
            for (a, b) in segments {
                self.emit_segment(Rect::new(a.x, a.y, b.x, b.y), stroke_thickness);
            }
        }
    }

    fn emit_segment(&mut self, seg: Rect, thickness: f64) {
        let horizontal = seg.height() <= seg.width();
        let length = if horizontal { seg.width() } else { seg.height() };
        let across = if horizontal { seg.height() } else { seg.width() };
        // A stroked segment is thin by construction; what disqualifies it is
        // being diagonal, which `across` catches.
        if length < MIN_LENGTH || across > MAX_THICKNESS {
            return;
        }
        let t = thickness.clamp(0.1, MAX_THICKNESS);
        let bbox = if horizontal {
            Rect::new(seg.x0, seg.y0 - t / 2.0, seg.x1, seg.y0 + t / 2.0)
        } else {
            Rect::new(seg.x0 - t / 2.0, seg.y0, seg.x0 + t / 2.0, seg.y1)
        };
        self.rules.push(Rule { bbox, horizontal, thickness: t, filled: false });
    }

    fn emit_rect(&mut self, r: Rect, filled: bool) {
        let (w, h) = (r.width(), r.height());
        let horizontal = h <= w;
        let (length, thickness) = if horizontal { (w, h) } else { (h, w) };
        if length < MIN_LENGTH || thickness > MAX_THICKNESS {
            return;
        }
        self.rules.push(Rule { bbox: r, horizontal, thickness, filled });
    }

    /// The four edges of a stroked box, each a rule in its own right.
    fn emit_box_edges(&mut self, r: Rect, thickness: f64) {
        let t = thickness.clamp(0.1, MAX_THICKNESS);
        for (bbox, horizontal) in [
            (Rect::new(r.x0, r.y0 - t / 2.0, r.x1, r.y0 + t / 2.0), true),
            (Rect::new(r.x0, r.y1 - t / 2.0, r.x1, r.y1 + t / 2.0), true),
            (Rect::new(r.x0 - t / 2.0, r.y0, r.x0 + t / 2.0, r.y1), false),
            (Rect::new(r.x1 - t / 2.0, r.y0, r.x1 + t / 2.0, r.y1), false),
        ] {
            let length = if horizontal { bbox.width() } else { bbox.height() };
            if length >= MIN_LENGTH {
                self.rules.push(Rule { bbox, horizontal, thickness: t, filled: false });
            }
        }
    }
}

impl ContentVisitor for RuleCollector {
    fn visit(&mut self, op: &Op, state: &mut StateMachine, _ctx: &WalkContext<'_>) -> Flow {
        let ctm = state.ctm();
        let at = |x: f64, y: f64| ctm.apply(Point::new(x, y));

        match op.kind {
            OpKind::MoveTo => {
                if let Some([x, y]) = op.trailing_nums::<2>() {
                    let p = at(x, y);
                    self.current = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            OpKind::LineTo => {
                if let Some([x, y]) = op.trailing_nums::<2>() {
                    let p = at(x, y);
                    if let Some(c) = self.current {
                        self.segments.push((c, p));
                    }
                    self.current = Some(p);
                }
            }
            // Curves move the pen but contribute no straight segment, so the
            // endpoint is recorded and nothing is joined to it.
            OpKind::CurveTo => {
                if let Some([_, _, _, _, x, y]) = op.trailing_nums::<6>() {
                    self.current = Some(at(x, y));
                }
            }
            OpKind::CurveToInitialReplicated | OpKind::CurveToFinalReplicated => {
                if let Some([_, _, x, y]) = op.trailing_nums::<4>() {
                    self.current = Some(at(x, y));
                }
            }
            OpKind::ClosePath => {
                if let (Some(c), Some(s)) = (self.current, self.subpath_start) {
                    self.segments.push((c, s));
                    self.current = Some(s);
                }
            }
            OpKind::Rectangle => {
                if let Some([x, y, w, h]) = op.trailing_nums::<4>() {
                    // Transform all four corners: a rotated CTM turns the
                    // rectangle into a parallelogram, and the bounding box is
                    // the honest approximation.
                    let corners = [at(x, y), at(x + w, y), at(x + w, y + h), at(x, y + h)];
                    let mut r = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
                    for c in corners {
                        r.x0 = r.x0.min(c.x);
                        r.y0 = r.y0.min(c.y);
                        r.x1 = r.x1.max(c.x);
                        r.y1 = r.y1.max(c.y);
                    }
                    self.rects.push(r);
                }
            }

            OpKind::Stroke | OpKind::CloseStroke => {
                let lw = state.state().line_width;
                self.paint(true, false, lw, &ctm);
                self.clear_path();
            }
            OpKind::Fill | OpKind::FillObsolete | OpKind::FillEvenOdd => {
                self.paint(false, true, 0.0, &ctm);
                self.clear_path();
            }
            OpKind::FillStroke
            | OpKind::FillStrokeEvenOdd
            | OpKind::CloseFillStroke
            | OpKind::CloseFillStrokeEvenOdd => {
                let lw = state.state().line_width;
                self.paint(true, true, lw, &ctm);
                self.clear_path();
            }
            // `n` ends the path without painting -- usually after `W` to set a
            // clip. Nothing was drawn, so nothing is a rule.
            OpKind::EndPath => self.clear_path(),

            _ => {}
        }
        Flow::Continue
    }
}

/// Horizontal rules that span most of a region, sorted top to bottom. These are
/// the cut hints spec 7.5 asks for.
pub fn horizontal_cuts(rules: &[Rule], region: &Rect, fraction: f64) -> Vec<f64> {
    let mut out: Vec<f64> = rules
        .iter()
        .filter(|r| r.horizontal && r.spans(region, fraction))
        .filter(|r| r.position() > region.y0 && r.position() < region.y1)
        .map(|r| r.position())
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::page;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    fn rules_of(content: &str) -> Vec<Rule> {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", content.as_bytes())
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        collect(&doc, &p)
    }

    #[test]
    fn a_stroked_horizontal_line_is_a_rule() {
        let rules = rules_of("1 w 72 700 m 500 700 l S");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].horizontal);
        assert!(!rules[0].filled);
        assert!((rules[0].length() - 428.0).abs() < 1e-6);
        // Device space flips y: user 700 on an 800-tall page is device 100.
        assert!((rules[0].position() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn a_stroked_vertical_line_is_a_rule() {
        let rules = rules_of("1 w 100 200 m 100 700 l S");
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].horizontal);
        assert!((rules[0].length() - 500.0).abs() < 1e-6);
        assert!((rules[0].position() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn a_thin_filled_rectangle_is_a_rule() {
        // How most producers actually draw a border.
        let rules = rules_of("72 700 428 1 re f");
        assert_eq!(rules.len(), 1);
        assert!(rules[0].horizontal);
        assert!(rules[0].filled);
        assert!((rules[0].thickness - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_thick_filled_rectangle_is_not_a_rule() {
        // A shaded box behind a paragraph is not a line.
        assert!(rules_of("72 600 428 100 re f").is_empty());
    }

    #[test]
    fn a_short_mark_is_not_a_rule() {
        assert!(rules_of("1 w 72 700 m 76 700 l S").is_empty(), "a 4pt tick");
        assert!(rules_of("72 700 4 1 re f").is_empty());
    }

    #[test]
    fn a_diagonal_stroke_is_not_a_rule() {
        assert!(rules_of("1 w 72 200 m 500 700 l S").is_empty());
    }

    #[test]
    fn a_stroked_box_yields_four_edges() {
        // The strongest table signal there is.
        let rules = rules_of("1 w 100 100 400 300 re S");
        assert_eq!(rules.len(), 4);
        assert_eq!(rules.iter().filter(|r| r.horizontal).count(), 2);
        assert_eq!(rules.iter().filter(|r| !r.horizontal).count(), 2);
    }

    #[test]
    fn an_unpainted_path_produces_nothing() {
        // `W n` sets a clip and draws nothing.
        assert!(rules_of("72 700 m 500 700 l W n").is_empty());
        assert!(rules_of("1 w 72 700 m 500 700 l n").is_empty());
    }

    #[test]
    fn a_polyline_yields_a_rule_per_qualifying_segment() {
        // Two horizontal legs and one vertical, all long enough.
        let rules = rules_of("1 w 72 700 m 300 700 l 300 500 l 500 500 l S");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules.iter().filter(|r| r.horizontal).count(), 2);
    }

    #[test]
    fn the_ctm_is_applied_to_rule_geometry() {
        // A rule drawn inside a scaled coordinate system lands where it is
        // drawn, not where its operands say.
        let plain = rules_of("1 w 100 700 m 300 700 l S");
        let scaled = rules_of("2 0 0 2 0 0 cm 1 w 50 350 m 150 350 l S");
        assert!((plain[0].length() - 200.0).abs() < 1e-6);
        assert!((scaled[0].length() - 200.0).abs() < 1e-6, "{}", scaled[0].length());
        assert!((plain[0].position() - scaled[0].position()).abs() < 1e-6);
    }

    #[test]
    fn curves_do_not_become_rules() {
        assert!(rules_of("1 w 72 700 m 100 750 400 650 500 700 c S").is_empty());
    }

    #[test]
    fn spans_measures_against_a_region() {
        let rules = rules_of("1 w 72 700 m 500 700 l S");
        let page = Rect::new(0.0, 0.0, 600.0, 800.0);
        assert!(rules[0].spans(&page, 0.5), "428 of 600 is more than half");
        assert!(!rules[0].spans(&page, 0.9));
    }

    #[test]
    fn horizontal_cuts_are_sorted_and_deduplicated() {
        let content = "1 w 72 700 m 500 700 l S \
                       1 w 72 400 m 500 400 l S \
                       1 w 72 700.5 m 500 700.5 l S";
        let rules = rules_of(content);
        let region = Rect::new(0.0, 0.0, 600.0, 800.0);
        let cuts = horizontal_cuts(&rules, &region, 0.5);
        assert_eq!(cuts.len(), 2, "the two rules half a point apart are one cut: {cuts:?}");
        assert!(cuts[0] < cuts[1]);
    }

    #[test]
    fn a_page_with_no_paths_has_no_rules() {
        assert!(rules_of("").is_empty());
        assert!(rules_of("BT ET").is_empty());
    }
}
