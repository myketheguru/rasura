//! Images and vector art. Spec 7.8's `ImageBlock` and `VectorBlock`.
//!
//! Everything up to here has been about glyphs. A page is not only glyphs, and
//! a document model that omits the figures would let an edit reflow text
//! straight through a photograph.
//!
//! Neither of these is *interpreted*. An image is located, measured and
//! identified; a vector region is bounded. Spec 7.8's rule for `Block::Unknown`
//! applies to both in spirit — they are rendered and moved, never reflowed —
//! so the useful output is a faithful bounding box and a reference back to the
//! object, not an analysis of the artwork.

use rasura_content::matrix::{Matrix, Point, Rect};
use rasura_content::op::OpKind;
use rasura_content::page::Page;
use rasura_content::state::{Colour, StateMachine};
use rasura_content::walker::{ContentVisitor, Flow, WalkContext, walk_page};
use rasura_cos::{Document, Name, ObjId, Object};
use std::ops::Range;

/// Vector paths closer than this multiple of the page's smaller dimension are
/// treated as one drawing. A figure is made of hundreds of strokes; grouping
/// them is the difference between one `VectorBlock` and eight hundred.
const CLUSTER_FRACTION: f64 = 0.02;

/// Beyond this many path boxes, clustering is abandoned and the page's vector
/// content is reported as a single region. A map with 50,000 paths is not
/// something the document model can meaningfully decompose, and the quadratic
/// pass would be the slowest thing in the library.
const MAX_PATHS: usize = 4000;

/// A placed image: an image XObject drawn by `Do`, or an inline image.
#[derive(Debug, Clone)]
pub struct ImageBlock {
    /// Device-space bounds of the part that shows.
    ///
    /// An image is always drawn into the unit square, so this begins as the CTM
    /// applied to it — which means a rotated or sheared image yields the
    /// bounding box of the parallelogram, not the parallelogram — and is then
    /// reduced by the clip in force. `docs/flow-model.md` names the alternative
    /// as a hole: "Clipping is not modelled, so a clipped figure reports its
    /// unclipped extent."
    ///
    /// Empty when the clip excludes the image entirely. The block is still
    /// reported: the drawing operator is in the file, and an edit that renumbers
    /// or moves objects has to know about it.
    pub bbox: Rect,
    /// Where the image would be with no clip.
    ///
    /// Kept alongside `bbox` because the two answer different questions: `bbox`
    /// is what the reader sees, and this is what the operators say. An edit that
    /// moves the image needs the second.
    pub unclipped_bbox: Rect,
    /// The clip in force when it was drawn, if any.
    pub clip: Option<Rect>,
    /// The transform in force, kept because the bounding box alone cannot say
    /// whether the image was rotated or flipped, and an edit that moves it must
    /// preserve that.
    pub ctm: Matrix,
    /// The XObject's name in `/Resources`, absent for an inline image.
    pub name: Option<Name>,
    /// The object drawn, absent for an inline image.
    pub id: Option<ObjId>,
    /// Pixel dimensions from `/Width` and `/Height`, when they could be read.
    pub pixels: Option<(u32, u32)>,
    /// Whether this is an image mask (`/ImageMask true`), which paints the fill
    /// colour through a stencil rather than carrying its own colour.
    pub is_mask: bool,
    /// Inline images live in the content stream rather than in an object, so
    /// they are identified by position instead.
    pub inline: bool,
    /// The drawing operator's byte span — the `Do`, or the whole inline image.
    ///
    /// Retained for the same reason `PlacedGlyph` retains its span: an edit has
    /// to get back to the bytes. Without it a caller knows *where the image is*
    /// and not *which operator put it there*, and would have to re-walk the
    /// page to find out.
    pub span: Range<usize>,
    /// The content stream the span addresses.
    ///
    /// Not always the page's. An image drawn inside a form XObject has a span
    /// into the *form's* stream, and `depth` says so. That distinction is
    /// load-bearing for editing: a form may be invoked many times, so moving
    /// "the image" inside one would move every instance of it.
    pub source: Option<ObjId>,
    /// Form XObject nesting; 0 when drawn directly on the page.
    pub depth: usize,
}

impl ImageBlock {
    /// Effective resolution in pixels per point, the smaller of the two axes.
    /// Below about 1.0 an image will look soft in print.
    pub fn resolution(&self) -> Option<f64> {
        let (w, h) = self.pixels?;
        let (dw, dh) = (self.bbox.width(), self.bbox.height());
        if dw <= 0.0 || dh <= 0.0 {
            return None;
        }
        Some((w as f64 / dw).min(h as f64 / dh))
    }
}

/// How a path was painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Fill {
        even_odd: bool,
    },
    Stroke,
    FillStroke {
        even_odd: bool,
    },
    /// `sh`, which paints the current clip region rather than a path.
    Shading,
}

impl Paint {
    pub fn fills(self) -> bool {
        matches!(self, Paint::Fill { .. } | Paint::FillStroke { .. } | Paint::Shading)
    }

    pub fn strokes(self) -> bool {
        matches!(self, Paint::Stroke | Paint::FillStroke { .. })
    }
}

/// One segment of a subpath, in device space.
///
/// Curves are kept as curves. Flattening them to line segments here would be
/// lossy in a way nothing downstream could undo, and it would have to choose a
/// tolerance — which is a rendering decision, and this layer does not render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Line(Point),
    Curve { c1: Point, c2: Point, to: Point },
}

/// What a `/Pattern` colour actually paints. ISO 32000-1 §8.7.
///
/// A pattern is not a colour: it is artwork, either a tile drawn from its own
/// content stream or a gradient. Carrying it as `Colour::Unresolved` — which is
/// all the state machine can say without `/Resources` — loses that distinction,
/// and a consumer that reads it as a flat fill will draw a solid rectangle
/// where the page has a gradient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternFill {
    pub name: Name,
    pub kind: PatternKind,
    /// The pattern object, so a consumer can go and read it.
    pub id: Option<ObjId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// `/PatternType 1`: a content stream tiled across the region.
    Tiling,
    /// `/PatternType 2`: a shading.
    Shading,
    /// Named by the content stream and not found in `/Resources`, or carrying
    /// no `/PatternType`. Reported rather than guessed.
    Unknown,
}

/// A connected run of segments.
#[derive(Debug, Clone, PartialEq)]
pub struct SubPath {
    pub start: Point,
    pub segments: Vec<Segment>,
    /// Closed by `h`, or by `re`, which always is.
    pub closed: bool,
}

/// One painted path, with what is needed to draw it again.
///
/// This is step 2's first hole in `docs/flow-model.md`:
///
/// > `VectorBlock` records a bounding box and a path count — no transform, no
/// > path data, no operator spans — so vector artwork cannot currently be
/// > placed by a layout engine at all.
///
/// Geometry is in **device space**, matching every other coordinate this crate
/// produces. `ctm` is kept anyway, because an edit that moves the artwork has
/// to write user-space operators back and cannot invert what it was not given.
#[derive(Debug, Clone)]
pub struct VectorPath {
    pub subpaths: Vec<SubPath>,
    /// Device-space bounds of the path as painted — after clipping, so this is
    /// what the page actually shows rather than what the path describes.
    pub bbox: Rect,
    pub paint: Paint,
    /// Fill colour where the paint fills, stroke colour where it strokes. Both
    /// are `None` when the operation does not use them, so a caller cannot read
    /// a fill colour off a path that only strokes.
    pub fill: Option<Colour>,
    pub stroke: Option<Colour>,
    /// Line width in device space, scaled by the CTM.
    pub line_width: f64,
    pub ctm: Matrix,
    /// The clip in force when the path was painted, if any.
    pub clip: Option<Rect>,
    /// The painting operator's byte span, for the reason `ImageBlock` keeps
    /// one: an edit has to get back to the bytes.
    pub span: Range<usize>,
    /// The whole path: from the first construction operator through the
    /// painting operator.
    ///
    /// `span` names the operator that painted; this names everything that has
    /// to move with it. Wrapping only the `f` in a transform would transform
    /// nothing, because the coordinates are in the `re` before it.
    pub path_span: Range<usize>,
    /// Whether `path_span` contains nothing but path construction and the
    /// painting operator.
    ///
    /// False when a colour, a line width, a `gs`, a `cm` or — the one that
    /// matters — a `W` appears in the middle of the path. An edit that wraps
    /// the range in `q … Q` would scope those to the wrap and silently change
    /// everything after it, so this is the flag that says wrapping is safe.
    pub self_contained: bool,
    /// The pattern this path is painted with, when the colour space is
    /// `/Pattern`.
    pub pattern: Option<PatternFill>,
    /// Whether [`clip`](Self::clip) is the clipping region exactly rather than
    /// a box around it. See `rasura_content::state::GraphicsState::clip_exact`.
    pub clip_exact: bool,
    /// The content stream the span addresses; `None` for the page's own.
    pub source: Option<ObjId>,
    /// Form XObject nesting; 0 when drawn directly on the page.
    pub depth: usize,
    /// The `/Shading` resource name, for `sh`.
    pub shading: Option<Name>,
}

impl VectorPath {
    /// Whether the path is a single axis-aligned rectangle.
    ///
    /// Worth asking because so much PDF vector art is one: rules, table grids,
    /// panel backgrounds and highlight boxes are all a single `re`, and a
    /// consumer that recognises them can draw them without a path renderer.
    pub fn is_rectangle(&self) -> bool {
        let [only] = self.subpaths.as_slice() else { return false };
        if only.segments.len() > 4 {
            return false;
        }
        let mut points = vec![only.start];
        for segment in &only.segments {
            match segment {
                Segment::Line(p) => points.push(*p),
                Segment::Curve { .. } => return false,
            }
        }
        // A `re` emits five points, the last repeating the first.
        if points.len() == 5 && points[4] == points[0] {
            points.pop();
        }
        if points.len() != 4 {
            return false;
        }
        // Axis-aligned means every pair of consecutive corners shares one
        // coordinate. A rotated rectangle is a rectangle and not this one.
        (0..4).all(|i| {
            let (a, b) = (points[i], points[(i + 1) % 4]);
            (a.x - b.x).abs() < 1e-6 || (a.y - b.y).abs() < 1e-6
        })
    }
}

/// A region of vector artwork: a chart, a logo, a diagram.
#[derive(Debug, Clone)]
pub struct VectorBlock {
    pub bbox: Rect,
    /// The paths that went into it, in the order they were painted.
    ///
    /// Empty when the page carried more than [`MAX_PATHS`]; see
    /// [`Graphics::geometry_truncated`]. `count` is right either way, so a
    /// caller can still tell a two-stroke drawing from a map without this being
    /// the thing that makes a dense page expensive to model.
    pub paths: Vec<VectorPath>,
    /// How many painted paths went into it, including any whose geometry was
    /// not retained.
    pub count: usize,
}

/// Everything on a page that is not text.
#[derive(Debug, Clone, Default)]
pub struct Graphics {
    pub images: Vec<ImageBlock>,
    pub vectors: Vec<VectorBlock>,
    /// The page had more than [`MAX_PATHS`] painted paths, so geometry was not
    /// retained for any of them.
    ///
    /// Reported rather than silently applied: a caller that finds
    /// `VectorBlock::paths` empty needs to know whether the page had no
    /// artwork or too much of it, and those want opposite responses.
    pub geometry_truncated: bool,
}

/// Collect images and vector artwork from a page.
pub fn collect(doc: &Document, page: &Page) -> Graphics {
    let mut visitor = Collector {
        images: Vec::new(),
        paths: Vec::new(),
        current: Vec::new(),
        start: None,
        path_start: None,
        path_pure: true,
    };
    walk_page(doc, page, &mut visitor);

    let span = (page.media_box.width().abs()).min(page.media_box.height().abs()).max(1.0);
    let truncated = visitor.paths.len() > MAX_PATHS;
    Graphics {
        images: visitor.images,
        vectors: cluster(visitor.paths, span * CLUSTER_FRACTION),
        geometry_truncated: truncated,
    }
}

struct Collector {
    images: Vec<ImageBlock>,
    /// Painted paths, in the order they were painted.
    paths: Vec<VectorPath>,
    /// Subpaths of the path under construction.
    current: Vec<SubPath>,
    /// Where the current subpath began, and the pen's position.
    start: Option<Point>,
    /// Byte offset of the first construction operator of the path being built.
    path_start: Option<usize>,
    /// Whether only construction operators have been seen since it began.
    path_pure: bool,
}

impl Collector {
    fn move_to(&mut self, ctm: &Matrix, x: f64, y: f64) {
        let p = ctm.apply(Point { x, y });
        self.current.push(SubPath { start: p, segments: Vec::new(), closed: false });
        self.start = Some(p);
    }

    /// Append a segment, opening a subpath if the producer did not.
    ///
    /// A path may legally begin with `l` when the pen is already somewhere —
    /// after `re`, or after a `Q` restored it. Rejecting that would drop real
    /// artwork, so a subpath is started at the pen's last known position, or at
    /// the origin if there is none.
    fn segment(&mut self, segment: Segment) {
        if self.current.is_empty() {
            let start = self.start.unwrap_or(Point { x: 0.0, y: 0.0 });
            self.current.push(SubPath { start, segments: Vec::new(), closed: false });
        }
        if let Some(sub) = self.current.last_mut() {
            sub.segments.push(segment);
        }
    }

    fn line_to(&mut self, ctm: &Matrix, x: f64, y: f64) {
        self.segment(Segment::Line(ctm.apply(Point { x, y })));
    }

    fn rectangle(&mut self, ctm: &Matrix, x: f64, y: f64, w: f64, h: f64) {
        let corner = |dx: f64, dy: f64| ctm.apply(Point { x: x + dx, y: y + dy });
        let start = corner(0.0, 0.0);
        self.current.push(SubPath {
            start,
            segments: vec![
                Segment::Line(corner(w, 0.0)),
                Segment::Line(corner(w, h)),
                Segment::Line(corner(0.0, h)),
                Segment::Line(start),
            ],
            closed: true,
        });
        self.start = Some(start);
    }

    fn close(&mut self) {
        if let Some(sub) = self.current.last_mut() {
            sub.closed = true;
        }
    }

    /// Emit the constructed path as painted, and reset.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        paint: Paint,
        state: &StateMachine,
        span: Range<usize>,
        source: Option<ObjId>,
        depth: usize,
        shading: Option<Name>,
        pattern: Option<PatternFill>,
    ) {
        let subpaths = std::mem::take(&mut self.current);
        let gs = state.state();

        let bbox = match paint {
            // `sh` paints the clip, not a path. With no clip it paints
            // everything the page can show, which is the honest answer and also
            // the reason a shading with no clip is rare in practice.
            Paint::Shading => match gs.clip {
                Some(clip) => clip,
                None => {
                    self.reset_path();
                    return;
                }
            },
            _ => {
                let Some(bounds) = bounds_of(&subpaths) else {
                    self.reset_path();
                    return;
                };
                // Clipped before it is recorded. A figure drawn ten times the
                // size of its clip is, on the page, the size of the clip —
                // which is the hole `docs/flow-model.md` names: "Clipping is
                // not modelled, so a clipped figure reports its unclipped
                // extent."
                match state.clipped(bounds) {
                    Some(visible) => visible,
                    None => {
                        self.reset_path();
                        return;
                    }
                }
            }
        };
        if !bbox.x0.is_finite() || !bbox.y0.is_finite() {
            self.reset_path();
            return;
        }

        // A stroke's width is in user space; the CTM scales it. The geometric
        // mean of the two axis scales is what a non-uniform transform does to a
        // line, and it is exactly right for the uniform case that is almost
        // always what a page has.
        let scale = ((gs.ctm.a * gs.ctm.d - gs.ctm.b * gs.ctm.c).abs()).sqrt();

        // A `sh` has no path, so its own operator is the whole of it.
        let path_span = match paint {
            Paint::Shading => span.clone(),
            _ => self.path_start.unwrap_or(span.start)..span.end,
        };

        self.paths.push(VectorPath {
            subpaths,
            bbox,
            paint,
            fill: paint.fills().then(|| gs.fill_colour.clone()),
            stroke: paint.strokes().then(|| gs.stroke_colour.clone()),
            line_width: gs.line_width * scale,
            ctm: gs.ctm,
            clip: gs.clip,
            clip_exact: state.clip_is_exact(),
            span,
            path_span,
            self_contained: self.path_pure,
            pattern,
            source,
            depth,
            shading,
        });
        self.reset_path();
    }

    fn reset_path(&mut self) {
        self.start = None;
        self.path_start = None;
        self.path_pure = true;
    }

    /// Note the byte offset a path began at.
    fn began(&mut self, at: usize) {
        if self.path_start.is_none() {
            self.path_start = Some(at);
            self.path_pure = true;
        }
    }
}

/// Resolve a `/Pattern` fill against the resources in scope.
///
/// `Colour::Unresolved` carries the pattern's *name*, which is all the state
/// machine can know: resolving it needs `/Resources`, and the state machine
/// does not have them. The walker does.
fn pattern_of(colour: Colour, ctx: &WalkContext<'_>, fills: bool) -> Option<PatternFill> {
    if !fills {
        return None;
    }
    let Colour::Unresolved { pattern: Some(name), .. } = colour else {
        return None;
    };

    let kind = ctx
        .resources
        .lookup(ctx.doc, "Pattern", &name)
        .and_then(|object| {
            let dict = match &*object {
                Object::Stream(s) => &s.dict,
                other => other.as_dict()?,
            };
            dict.get("PatternType").and_then(Object::as_i64)
        })
        .map_or(PatternKind::Unknown, |t| match t {
            1 => PatternKind::Tiling,
            2 => PatternKind::Shading,
            _ => PatternKind::Unknown,
        });

    let id = ctx.resources.lookup_id(ctx.doc, "Pattern", &name);
    Some(PatternFill { name, kind, id })
}

/// Device-space bounds of a constructed path.
///
/// A Bézier lies within the convex hull of its control points, so taking the
/// points overstates the box slightly and never understates it. For something
/// that decides where content may not go, that is the right way to be wrong.
fn bounds_of(subpaths: &[SubPath]) -> Option<Rect> {
    let mut out: Option<Rect> = None;
    let mut extend = |p: Point| {
        let point = Rect { x0: p.x, y0: p.y, x1: p.x, y1: p.y };
        out = Some(match out {
            Some(existing) => existing.union(&point),
            None => point,
        });
    };
    for sub in subpaths {
        extend(sub.start);
        for segment in &sub.segments {
            match segment {
                Segment::Line(p) => extend(*p),
                Segment::Curve { c1, c2, to } => {
                    extend(*c1);
                    extend(*c2);
                    extend(*to);
                }
            }
        }
    }
    // A single point is a degenerate path that paints nothing measurable. The
    // old code required two points for the same reason and the threshold is
    // kept: a lone `m f` is not artwork.
    let bounds = out?;
    let has_extent = subpaths.iter().map(|s| s.segments.len()).sum::<usize>() > 0;
    has_extent.then_some(bounds)
}

fn number(op: &rasura_content::op::Op, i: usize) -> f64 {
    op.operands.get(i).and_then(Object::as_f64).unwrap_or(0.0)
}

/// Whether an operator belongs to the path itself: construction, closing, or
/// the painting operator that ends it.
fn is_path_op(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::MoveTo
            | OpKind::LineTo
            | OpKind::CurveTo
            | OpKind::CurveToInitialReplicated
            | OpKind::CurveToFinalReplicated
            | OpKind::Rectangle
            | OpKind::ClosePath
            | OpKind::Stroke
            | OpKind::CloseStroke
            | OpKind::Fill
            | OpKind::FillObsolete
            | OpKind::FillEvenOdd
            | OpKind::FillStroke
            | OpKind::FillStrokeEvenOdd
            | OpKind::CloseFillStroke
            | OpKind::CloseFillStrokeEvenOdd
            | OpKind::EndPath
    )
}

impl ContentVisitor for Collector {
    fn visit(
        &mut self,
        op: &rasura_content::op::Op,
        state: &mut StateMachine,
        ctx: &WalkContext<'_>,
    ) -> Flow {
        let ctm = state.state().ctm;

        // Anything that is not path construction or painting, arriving while a
        // path is being built, means the path's operator range is not just the
        // path. `W` is the case that matters: an edit wrapping the range in
        // `q … Q` would undo the clip at the `Q` and change every operator
        // after it.
        if self.path_start.is_some() && !is_path_op(op.kind) {
            self.path_pure = false;
        }

        match op.kind {
            OpKind::MoveTo => {
                self.began(op.span.start);
                self.move_to(&ctm, number(op, 0), number(op, 1));
            }
            OpKind::LineTo => {
                self.began(op.span.start);
                self.line_to(&ctm, number(op, 0), number(op, 1));
            }
            OpKind::CurveTo => {
                self.began(op.span.start);
                self.segment(Segment::Curve {
                    c1: ctm.apply(Point { x: number(op, 0), y: number(op, 1) }),
                    c2: ctm.apply(Point { x: number(op, 2), y: number(op, 3) }),
                    to: ctm.apply(Point { x: number(op, 4), y: number(op, 5) }),
                });
            }
            // `v` uses the current point as the first control point and `y` uses
            // the endpoint as the second. Neither is given explicitly, so the
            // nearest available point stands in — which bounds the curve
            // correctly and is not the same curve. Recorded rather than
            // approximated silently would be better; recorded as a cubic whose
            // hull still contains the true curve is what this does.
            OpKind::CurveToInitialReplicated => {
                self.began(op.span.start);
                let c = ctm.apply(Point { x: number(op, 0), y: number(op, 1) });
                let to = ctm.apply(Point { x: number(op, 2), y: number(op, 3) });
                self.segment(Segment::Curve { c1: c, c2: c, to });
            }
            OpKind::CurveToFinalReplicated => {
                self.began(op.span.start);
                let c = ctm.apply(Point { x: number(op, 0), y: number(op, 1) });
                let to = ctm.apply(Point { x: number(op, 2), y: number(op, 3) });
                self.segment(Segment::Curve { c1: c, c2: to, to });
            }
            OpKind::Rectangle => {
                self.began(op.span.start);
                self.rectangle(&ctm, number(op, 0), number(op, 1), number(op, 2), number(op, 3));
            }
            OpKind::ClosePath => self.close(),

            // Every painting operator. A path that is only clipped or discarded
            // draws nothing and is not artwork.
            OpKind::Stroke
            | OpKind::CloseStroke
            | OpKind::Fill
            | OpKind::FillObsolete
            | OpKind::FillEvenOdd
            | OpKind::FillStroke
            | OpKind::FillStrokeEvenOdd
            | OpKind::CloseFillStroke
            | OpKind::CloseFillStrokeEvenOdd => {
                if matches!(
                    op.kind,
                    OpKind::CloseStroke | OpKind::CloseFillStroke | OpKind::CloseFillStrokeEvenOdd
                ) {
                    self.close();
                }
                let paint = match op.kind {
                    OpKind::Stroke | OpKind::CloseStroke => Paint::Stroke,
                    OpKind::Fill | OpKind::FillObsolete => Paint::Fill { even_odd: false },
                    OpKind::FillEvenOdd => Paint::Fill { even_odd: true },
                    OpKind::FillStroke | OpKind::CloseFillStroke => {
                        Paint::FillStroke { even_odd: false }
                    }
                    _ => Paint::FillStroke { even_odd: true },
                };
                let source = ctx.content.source_of(op.span.start);
                let pattern = pattern_of(state.state().fill_colour.clone(), ctx, paint.fills());
                self.paint(paint, state, op.span.clone(), source, ctx.depth, None, pattern);
            }
            OpKind::EndPath => {
                self.current.clear();
                self.reset_path();
            }

            // `sh` fills the current clip with a gradient. It has no path, so
            // nothing below this ever saw it — which is the second of step 2's
            // holes: "Shading operators are tokenised and modelled nowhere."
            OpKind::Shading => {
                let name = op.operands.first().and_then(Object::as_name).cloned();
                let source = ctx.content.source_of(op.span.start);
                self.paint(Paint::Shading, state, op.span.clone(), source, ctx.depth, name, None);
            }

            OpKind::InlineImage => {
                let dict = op.inline_image.as_ref().map(|i| &i.dict);
                let placed = unit_square(&ctm);
                self.images.push(ImageBlock {
                    bbox: state.clipped(placed).unwrap_or(Rect {
                        x0: placed.x0,
                        y0: placed.y0,
                        x1: placed.x0,
                        y1: placed.y0,
                    }),
                    unclipped_bbox: placed,
                    clip: state.state().clip,
                    ctm,
                    name: None,
                    id: None,
                    pixels: dict.and_then(|d| {
                        // Inline images use abbreviated keys, with the long
                        // forms permitted; both are accepted.
                        let w = d.get("W").or_else(|| d.get("Width"))?.as_i64()?;
                        let h = d.get("H").or_else(|| d.get("Height"))?.as_i64()?;
                        Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?))
                    }),
                    is_mask: dict
                        .and_then(|d| d.get("IM").or_else(|| d.get("ImageMask")))
                        .and_then(Object::as_bool)
                        .unwrap_or(false),
                    inline: true,
                    span: op.span.clone(),
                    source: ctx.content.source_of(op.span.start),
                    depth: ctx.depth,
                });
            }

            OpKind::InvokeXObject => {
                let Some(name) = op.operands.first().and_then(Object::as_name) else {
                    return Flow::Continue;
                };
                let Some(xobj) = ctx.resources.xobject(ctx.doc, name) else {
                    return Flow::Continue;
                };
                let Some(stream) = xobj.as_stream() else { return Flow::Continue };
                if stream.dict.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str())
                    != Some("Image")
                {
                    // A form XObject; the walker descends into it and its own
                    // content arrives here in due course.
                    return Flow::Continue;
                }
                let read = |k: &str| {
                    stream.dict.get(k).and_then(Object::as_i64).and_then(|v| u32::try_from(v).ok())
                };
                let placed = unit_square(&ctm);
                self.images.push(ImageBlock {
                    bbox: state.clipped(placed).unwrap_or(Rect {
                        x0: placed.x0,
                        y0: placed.y0,
                        x1: placed.x0,
                        y1: placed.y0,
                    }),
                    unclipped_bbox: placed,
                    clip: state.state().clip,
                    ctm,
                    name: Some(name.clone()),
                    id: ctx.resources.xobject_id(ctx.doc, name),
                    pixels: read("Width").zip(read("Height")),
                    is_mask: stream
                        .dict
                        .get("ImageMask")
                        .and_then(Object::as_bool)
                        .unwrap_or(false),
                    inline: false,
                    span: op.span.clone(),
                    source: ctx.content.source_of(op.span.start),
                    depth: ctx.depth,
                });
            }
            _ => {}
        }
        Flow::Continue
    }
}

/// The CTM applied to the unit square, which is where every image is drawn.
fn unit_square(ctm: &Matrix) -> Rect {
    let corners = [
        ctm.apply(Point { x: 0.0, y: 0.0 }),
        ctm.apply(Point { x: 1.0, y: 0.0 }),
        ctm.apply(Point { x: 1.0, y: 1.0 }),
        ctm.apply(Point { x: 0.0, y: 1.0 }),
    ];
    let mut b = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for p in corners {
        b.x0 = b.x0.min(p.x);
        b.y0 = b.y0.min(p.y);
        b.x1 = b.x1.max(p.x);
        b.y1 = b.y1.max(p.y);
    }
    b
}

/// Group nearby paths into drawings.
fn cluster(paths: Vec<VectorPath>, gap: f64) -> Vec<VectorBlock> {
    if paths.is_empty() {
        return Vec::new();
    }
    if paths.len() > MAX_PATHS {
        // Reported as one region rather than silently dropped: the page really
        // does contain that artwork, and a document model that omits it would
        // let an edit reflow text through a map.
        //
        // The geometry goes, though — retaining 50,000 paths would make one
        // pathological page cost more memory than the rest of the document, and
        // §12.5 budgets that. `Graphics::geometry_truncated` says so.
        let mut b = paths[0].bbox;
        for p in &paths {
            b = b.union(&p.bbox);
        }
        return vec![VectorBlock { bbox: b, paths: Vec::new(), count: paths.len() }];
    }

    let n = paths.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, a) in paths.iter().enumerate() {
        for (j, b) in paths.iter().enumerate().skip(i + 1) {
            if near(&a.bbox, &b.bbox, gap) {
                let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }

    // Roots resolved before the paths are consumed, because `find` needs
    // `parent` mutably and the loop below moves out of `paths`.
    let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();

    let mut groups: std::collections::BTreeMap<usize, VectorBlock> = Default::default();
    for (path, root) in paths.into_iter().zip(roots) {
        match groups.get_mut(&root) {
            Some(block) => {
                block.bbox = block.bbox.union(&path.bbox);
                block.count += 1;
                block.paths.push(path);
            }
            None => {
                groups.insert(root, VectorBlock { bbox: path.bbox, paths: vec![path], count: 1 });
            }
        }
    }
    groups.into_values().collect()
}

fn near(a: &Rect, b: &Rect, gap: f64) -> bool {
    a.x0 - gap <= b.x1 && b.x0 - gap <= a.x1 && a.y0 - gap <= b.y1 && b.y0 - gap <= a.y1
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::page;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page with one 200x100 image XObject and whatever content is given.
    fn page_with_image(content: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /XObject << /Im1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 200 /Height 100 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8; 16],
            )
            .finish("/Root 1 0 R")
    }

    fn graphics_of(bytes: Vec<u8>) -> Graphics {
        let doc = rasura_cos::Document::open(bytes).expect("open");
        let p = page::pages(&doc).expect("pages").pages.remove(0);
        collect(&doc, &p)
    }

    #[test]
    fn an_image_xobject_is_located_and_measured() {
        // 400x200 at (100, 500) in user space.
        let g = graphics_of(page_with_image("q 400 0 0 200 100 500 cm /Im1 Do Q"));
        assert_eq!(g.images.len(), 1);
        let im = &g.images[0];
        assert_eq!(im.pixels, Some((200, 100)));
        assert!(!im.inline);
        assert_eq!(im.name.as_ref().and_then(|n| n.as_str()), Some("Im1"));
        assert!((im.bbox.width() - 400.0).abs() < 1e-6, "{:?}", im.bbox);
        assert!((im.bbox.height() - 200.0).abs() < 1e-6, "{:?}", im.bbox);
    }

    #[test]
    fn image_resolution_is_computed_from_pixels_and_size() {
        // 200 pixels across 400 points is half a pixel per point.
        let g = graphics_of(page_with_image("q 400 0 0 200 100 500 cm /Im1 Do Q"));
        let r = g.images[0].resolution().expect("resolution");
        assert!((r - 0.5).abs() < 1e-6, "{r}");
    }

    #[test]
    fn a_degenerate_image_has_no_resolution() {
        // Zero-area CTM: reported as unknown rather than as a division by zero.
        let g = graphics_of(page_with_image("q 0 0 0 0 100 500 cm /Im1 Do Q"));
        assert_eq!(g.images.len(), 1);
        assert!(g.images[0].resolution().is_none());
    }

    #[test]
    fn a_rotated_image_yields_the_bounding_box_and_keeps_the_matrix() {
        // 90 degrees: a 400x200 image becomes 200x400 on the page, and the
        // bounding box alone can no longer say it was rotated.
        let g = graphics_of(page_with_image("q 0 200 -400 0 500 100 cm /Im1 Do Q"));
        let im = &g.images[0];
        assert!((im.bbox.width() - 400.0).abs() < 1e-6, "{:?}", im.bbox);
        assert!((im.bbox.height() - 200.0).abs() < 1e-6, "{:?}", im.bbox);
        assert!(im.ctm.a.abs() < 1e-9 && im.ctm.b.abs() > 0.0, "the rotation is preserved");
    }

    #[test]
    fn an_inline_image_is_found() {
        let g = graphics_of(page_with_image(
            "q 100 0 0 50 10 10 cm BI /W 4 /H 4 /CS /G /BPC 8 ID \
             \u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0} EI Q",
        ));
        assert_eq!(g.images.len(), 1);
        assert!(g.images[0].inline);
        assert_eq!(g.images[0].pixels, Some((4, 4)));
    }

    #[test]
    fn an_image_mask_is_flagged() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /XObject << /Im1 5 0 R >> >> >>",
            )
            .stream(4, "", b"q 10 0 0 10 0 0 cm /Im1 Do Q")
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ImageMask true \
                 /BitsPerComponent 1",
                &[0u8; 8],
            )
            .finish("/Root 1 0 R");
        let g = graphics_of(bytes);
        assert!(g.images[0].is_mask);
    }

    // --- vector artwork ------------------------------------------------------

    #[test]
    fn nearby_paths_become_one_drawing() {
        // Eight short strokes in a small area are one figure, not eight.
        let mut c = String::new();
        for i in 0..8 {
            let y = 100 + i * 3;
            c.push_str(&format!("100 {y} m 140 {y} l S\n"));
        }
        let g = graphics_of(page_with_image(&c));
        assert_eq!(g.vectors.len(), 1, "expected one clustered drawing");
        assert_eq!(g.vectors[0].count, 8);
    }

    #[test]
    fn distant_drawings_stay_separate() {
        let c = "100 100 m 140 100 l S\n100 105 m 140 105 l S\n\
                 400 700 m 440 700 l S\n400 705 m 440 705 l S\n";
        let g = graphics_of(page_with_image(c));
        assert_eq!(g.vectors.len(), 2, "two figures at opposite corners");
    }

    #[test]
    fn a_clip_path_paints_nothing_itself() {
        // Named for what it checks. It used to be called
        // `a_clipped_path_is_not_artwork`, which claimed more than it tested:
        // the assertion passes because `W n` ends with `n`, the same reason
        // `a_discarded_path_is_not_artwork` below passes, and it would keep
        // passing if clipping were removed from the tokenizer entirely.
        let g = graphics_of(page_with_image("100 100 m 400 400 l W n"));
        assert!(g.vectors.is_empty(), "the clip path itself draws nothing");
    }

    #[test]
    fn content_drawn_under_a_clip_reports_the_part_that_shows() {
        // This test previously asserted the opposite, with a note saying it
        // should fail once clipping was modelled and that the fix was to
        // intersect rather than to delete the test. It failed on the first run
        // after `StateMachine` gained a clip, which is the whole value of
        // writing a known gap down as an assertion.
        let g = graphics_of(page_with_image("100 100 200 200 re W n 0 0 612 792 re f"));
        let block = g.vectors.first().expect("the painted rectangle is artwork");
        assert!(
            block.bbox.width() <= 200.0 + 1e-6,
            "the 200-unit clip bounds it, not the full page: {:?}",
            block.bbox
        );

        // The path's own geometry is untouched: the clip changes what shows,
        // not what the operators said. An edit moving this artwork needs the
        // second, and reading it off `bbox` would move it to the wrong place.
        let path = block.paths.first().expect("geometry retained");
        assert!(path.is_rectangle());
        // Device space: the page is 800 tall and the base CTM flips y, so the
        // clip's user-space 100..300 is 500..700 here.
        assert_eq!(path.clip, Some(Rect::new(100.0, 500.0, 300.0, 700.0)));
        let untouched = bounds_of(&path.subpaths).expect("bounds");
        assert!(untouched.width() > 500.0, "{untouched:?}");
    }

    #[test]
    fn a_path_clipped_away_entirely_is_not_reported_as_artwork() {
        let g = graphics_of(page_with_image("0 0 10 10 re W n 500 500 50 50 re f"));
        assert!(g.vectors.is_empty(), "nothing of it shows: {:?}", g.vectors);
    }

    #[test]
    fn an_image_reports_both_what_shows_and_where_it_was_drawn() {
        // The two answer different questions and a consumer needs both: `bbox`
        // is what a reader sees, `unclipped_bbox` is what the operators say.
        let g = graphics_of(page_with_image(
            "q 100 600 100 100 re W n 200 0 0 100 100 600 cm /Im1 Do Q",
        ));
        let image = g.images.first().expect("the image");
        assert_eq!(image.unclipped_bbox, Rect::new(100.0, 100.0, 300.0, 200.0));
        assert_eq!(image.bbox, Rect::new(100.0, 100.0, 200.0, 200.0), "half of it is clipped away");
        assert_eq!(image.clip, Some(Rect::new(100.0, 100.0, 200.0, 200.0)));
    }

    #[test]
    fn a_painted_path_records_how_it_was_painted() {
        let g = graphics_of(page_with_image("0.2 0.4 0.6 rg 3 w 10 10 100 50 re B"));
        let path = &g.vectors[0].paths[0];
        assert_eq!(path.paint, Paint::FillStroke { even_odd: false });
        assert!(path.fill.is_some() && path.stroke.is_some());
        assert_eq!(path.fill.as_ref().and_then(|c| c.to_rgb()), Some((0.2, 0.4, 0.6)));
        assert_eq!(path.line_width, 3.0);
        assert!(!path.span.is_empty(), "the operator that painted it is addressable");
        assert!(path.is_rectangle());
    }

    #[test]
    fn a_stroke_carries_no_fill_colour() {
        // A caller must not be able to read a fill colour off a path that only
        // strokes: the state has one, and it is not what the page painted.
        let g = graphics_of(page_with_image("1 0 0 rg 0 0 1 RG 10 10 m 90 90 l S"));
        let path = &g.vectors[0].paths[0];
        assert_eq!(path.paint, Paint::Stroke);
        assert!(path.fill.is_none());
        assert_eq!(path.stroke.as_ref().and_then(|c| c.to_rgb()), Some((0.0, 0.0, 1.0)));
    }

    /// A page with a `/Pattern` resource of the given type.
    fn page_with_pattern(pattern_type: i64, content: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Pattern << /P0 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                &format!(
                    "<< /Type /Pattern /PatternType {pattern_type} /Shading \
                     << /ShadingType 2 /ColorSpace /DeviceRGB >> >>"
                ),
            )
            .finish("/Root 1 0 R")
    }

    #[test]
    fn a_pattern_fill_is_artwork_rather_than_a_colour() {
        // `Colour::Unresolved` can only carry the pattern's name: resolving it
        // needs `/Resources`, which the state machine does not have. A consumer
        // reading it as a flat fill draws a solid rectangle where the page has
        // a gradient.
        let g = graphics_of(page_with_pattern(2, "/Pattern cs /P0 scn 10 10 100 50 re f"));
        let path = &g.vectors[0].paths[0];

        let pattern = path.pattern.as_ref().expect("the fill is a pattern");
        assert_eq!(pattern.name.as_str(), Some("P0"));
        assert_eq!(pattern.kind, PatternKind::Shading);
        assert!(pattern.id.is_some(), "and the object can be gone and read");
    }

    #[test]
    fn a_tiling_pattern_is_distinguished_from_a_shading_one() {
        let g = graphics_of(page_with_pattern(1, "/Pattern cs /P0 scn 10 10 100 50 re f"));
        assert_eq!(
            g.vectors[0].paths[0].pattern.as_ref().map(|p| p.kind),
            Some(PatternKind::Tiling)
        );
    }

    #[test]
    fn a_pattern_named_but_not_defined_is_reported_as_unknown() {
        // Named by the content stream and absent from `/Resources`. The fill is
        // still a pattern — the page said so — and what kind is not knowable.
        let g = graphics_of(page_with_image("/Pattern cs /Missing scn 10 10 100 50 re f"));
        let pattern = g.vectors[0].paths[0].pattern.as_ref().expect("still a pattern");
        assert_eq!(pattern.kind, PatternKind::Unknown);
        assert_eq!(pattern.id, None);
    }

    #[test]
    fn an_ordinary_colour_carries_no_pattern() {
        let g = graphics_of(page_with_image("1 0 0 rg 10 10 100 50 re f"));
        assert!(g.vectors[0].paths[0].pattern.is_none());
    }

    #[test]
    fn a_paths_span_covers_its_construction_and_not_just_the_paint() {
        // Wrapping only the `f` in a transform would transform nothing: the
        // coordinates are in the `re` before it.
        let g = graphics_of(page_with_image("10 10 100 50 re f"));
        let path = &g.vectors[0].paths[0];
        assert!(path.path_span.start < path.span.start, "{path:?}");
        assert_eq!(path.path_span.end, path.span.end);
        assert!(path.self_contained);
    }

    #[test]
    fn a_clip_inside_a_path_makes_it_not_self_contained() {
        // The flag an edit checks before wrapping the range in `q … Q`: the
        // `W` would have its clip undone at the `Q`.
        let g = graphics_of(page_with_image("10 10 100 50 re W f"));
        assert!(!g.vectors[0].paths[0].self_contained);
    }

    #[test]
    fn a_path_records_whether_its_clip_is_exact() {
        let rect = graphics_of(page_with_image("0 0 300 300 re W n 10 10 100 50 re f"));
        assert!(rect.vectors[0].paths[0].clip_exact);

        let triangle =
            graphics_of(page_with_image("0 0 m 300 0 l 150 300 l h W n 10 10 100 50 re f"));
        assert!(
            !triangle.vectors[0].paths[0].clip_exact,
            "the box around a triangle admits content the triangle clips away"
        );
    }

    #[test]
    fn curves_survive_as_curves() {
        let g = graphics_of(page_with_image("10 10 m 20 90 80 90 90 10 c S"));
        let path = &g.vectors[0].paths[0];
        let [sub] = path.subpaths.as_slice() else { panic!("one subpath: {path:?}") };
        assert!(matches!(sub.segments.as_slice(), [Segment::Curve { .. }]));
        assert!(!path.is_rectangle());
    }

    #[test]
    fn a_shading_operator_paints_its_clip_and_is_no_longer_invisible() {
        // `sh` has no path, so nothing below this ever saw it -- the second of
        // step 2's holes in `docs/flow-model.md`. It paints the clip region,
        // which is why modelling the clip had to come first.
        let g = graphics_of(page_with_image("q 50 50 200 100 re W n /Sh0 sh Q"));
        let path = g.vectors.first().and_then(|b| b.paths.first()).expect("the shading");
        assert_eq!(path.paint, Paint::Shading);
        assert_eq!(path.bbox, Rect::new(50.0, 650.0, 250.0, 750.0));
        assert_eq!(path.shading.as_ref().and_then(|n| n.as_str()), Some("Sh0"));
    }

    #[test]
    fn an_unclipped_shading_is_declined_rather_than_given_the_whole_page() {
        // With no clip, `sh` paints everything the page can show. Reporting a
        // full-page vector block for it would put a "drawing" behind every
        // block on the page and change how the model classifies all of them.
        let g = graphics_of(page_with_image("/Sh0 sh"));
        assert!(g.vectors.is_empty(), "{:?}", g.vectors);
    }

    #[test]
    fn a_discarded_path_is_not_artwork() {
        let g = graphics_of(page_with_image("100 100 m 400 400 l n"));
        assert!(g.vectors.is_empty());
    }

    #[test]
    fn a_curves_control_points_bound_it() {
        // The curve lies within the hull of its control points, so the box may
        // be generous but is never too small -- which is the right way to be
        // wrong for a region boundary.
        let g = graphics_of(page_with_image("100 100 m 150 300 250 300 300 100 c S"));
        assert_eq!(g.vectors.len(), 1);
        let b = g.vectors[0].bbox;
        assert!(b.x0 <= 100.0 && b.x1 >= 300.0, "{b:?}");
        assert!(b.height() >= 190.0, "the control points are included: {b:?}");
    }

    #[test]
    fn a_rectangle_contributes_all_four_corners() {
        let g = graphics_of(page_with_image("100 100 200 150 re f"));
        assert_eq!(g.vectors.len(), 1);
        assert!((g.vectors[0].bbox.width() - 200.0).abs() < 1e-6);
        assert!((g.vectors[0].bbox.height() - 150.0).abs() < 1e-6);
    }

    #[test]
    fn a_page_with_neither_yields_nothing() {
        let g = graphics_of(page_with_image("BT ET"));
        assert!(g.images.is_empty() && g.vectors.is_empty());
    }
}
