//! Graphics and text state. ISO 32000-1 §8.4 and §9.3, spec 6.3.
//!
//! # What lives where
//!
//! The text state parameters -- `Tc`, `Tw`, `Tz`, `TL`, `Tf`, `Tfs`, `Tr`, `Ts`
//! -- are part of the *graphics* state, so `q`/`Q` saves and restores them. The
//! text matrix and line matrix are not: they exist only between `BT` and `ET`
//! and are reset to identity by `BT`. Conflating the two is a classic bug, and
//! it shows up as text that drifts after an unrelated `Q`.

use crate::matrix::{Matrix, Point, Rect};
use crate::op::{Op, OpKind};
use rasura_cos::error::{Leniency, LeniencyKind};
use rasura_cos::{Name, Object};
use smallvec::SmallVec;

/// Nesting deeper than this is a malformed stream. Real content rarely exceeds
/// a dozen.
const MAX_STACK: usize = 256;

/// A colour in whatever space was selected.
#[derive(Debug, Clone, PartialEq)]
pub enum Colour {
    Gray(f64),
    Rgb(f64, f64, f64),
    Cmyk(f64, f64, f64, f64),
    /// Components in a colour space this layer did not resolve -- an ICCBased,
    /// Indexed, Separation or DeviceN space, which needs `/Resources` to
    /// interpret. Carried rather than guessed.
    Unresolved {
        space: Option<Name>,
        components: SmallVec<[f64; 4]>,
        /// The pattern name, for `scn` with a pattern colour space.
        pattern: Option<Name>,
    },
}

impl Default for Colour {
    fn default() -> Self {
        // ISO 32000-1 §8.6.8: the initial colour is black in DeviceGray.
        Colour::Gray(0.0)
    }
}

impl Colour {
    /// Best-effort conversion for callers that need something to draw with.
    ///
    /// The CMYK conversion is the naive one, which is wrong for any real ICC
    /// workflow and right for a preview. Unresolved spaces return `None` rather
    /// than a guess -- spec 2 says degradation is reported, not assumed.
    pub fn to_rgb(&self) -> Option<(f64, f64, f64)> {
        match self {
            Colour::Gray(g) => Some((*g, *g, *g)),
            Colour::Rgb(r, g, b) => Some((*r, *g, *b)),
            Colour::Cmyk(c, m, y, k) => {
                Some(((1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)))
            }
            Colour::Unresolved { components, .. } => match components.len() {
                1 => Some((components[0], components[0], components[0])),
                3 => Some((components[0], components[1], components[2])),
                _ => None,
            },
        }
    }
}

/// ISO 32000-1 §9.3. Every field here is part of the graphics state.
#[derive(Debug, Clone, PartialEq)]
pub struct TextState {
    /// `Tc`, in unscaled text-space units.
    pub char_spacing: f64,
    /// `Tw`, in unscaled text-space units. Applies only to single-byte code 32.
    pub word_spacing: f64,
    /// `Tz`, as a percentage. 100 is normal.
    pub horizontal_scale: f64,
    /// `TL`, in unscaled text-space units.
    pub leading: f64,
    /// The resource name given to `Tf`, resolved against `/Font` by the caller.
    pub font: Option<Name>,
    /// `Tfs`.
    pub font_size: f64,
    /// `Tr`. 3 is invisible, 7 is clip-only.
    pub render_mode: i64,
    /// `Ts`.
    pub rise: f64,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 100.0,
            leading: 0.0,
            font: None,
            font_size: 0.0,
            render_mode: 0,
            rise: 0.0,
        }
    }
}

impl TextState {
    /// Horizontal glyph displacement. Spec 6.3:
    ///
    /// ```text
    /// tx = ((w0 - Tj/1000) x Tfs + Tc + Tw) x (Tz/100)
    /// ```
    ///
    /// `w0` is the glyph's width in text-space units (i.e. `/Widths` divided by
    /// 1000 for a simple font). `adjustment` is the raw number from a `TJ`
    /// array, applied before scaling. `word_spacing_applies` must come from
    /// [`word_spacing_applies`], not from "is this a space character".
    pub fn displacement(&self, w0: f64, adjustment: f64, word_spacing_applies: bool) -> f64 {
        let tw = if word_spacing_applies { self.word_spacing } else { 0.0 };
        ((w0 - adjustment / 1000.0) * self.font_size + self.char_spacing + tw)
            * (self.horizontal_scale / 100.0)
    }

    /// Vertical glyph displacement, for `/WMode 1`. ISO 32000-1 §9.4.4.
    ///
    /// Note what is missing: the horizontal scale does **not** apply in
    /// vertical writing mode. Applying it is a subtle way to make every CJK
    /// vertical document wrong.
    pub fn displacement_vertical(
        &self,
        w1: f64,
        adjustment: f64,
        word_spacing_applies: bool,
    ) -> f64 {
        let tw = if word_spacing_applies { self.word_spacing } else { 0.0 };
        (w1 - adjustment / 1000.0) * self.font_size + self.char_spacing + tw
    }

    /// True when the glyph is drawn at all. `Tr 3` is the standard way to put
    /// invisible text under a scanned image, so callers doing extraction want
    /// it and callers doing rendering do not.
    pub fn is_visible(&self) -> bool {
        self.render_mode != 3 && self.render_mode != 7
    }
}

/// ISO 32000-1 §9.3.3: word spacing "shall be applied to every occurrence of
/// the single-byte character code 32 in a string when using a simple font or a
/// composite font that defines code 32 as a single-byte code. It shall not
/// apply to occurrences of the byte value 32 in multiple-byte codes."
///
/// Getting this wrong misplaces every glyph after a space on the line, and it
/// misplaces them by a plausible amount, so the output looks almost right. Most
/// CID fonts use two-byte codes, where code 32 is not a space and word spacing
/// must not apply.
pub fn word_spacing_applies(code: u32, code_byte_len: usize) -> bool {
    code == 32 && code_byte_len == 1
}

/// ISO 32000-1 §8.4. Saved and restored by `q`/`Q`.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicsState {
    pub ctm: Matrix,
    pub text: TextState,
    pub stroke_colour: Colour,
    pub fill_colour: Colour,
    pub stroke_space: Option<Name>,
    pub fill_space: Option<Name>,
    pub line_width: f64,
    pub line_cap: i64,
    pub line_join: i64,
    pub miter_limit: f64,
    pub dash_array: SmallVec<[f64; 4]>,
    pub dash_phase: f64,
    pub rendering_intent: Option<Name>,
    pub flatness: f64,
    /// The `/ExtGState` resource name last applied by `gs`. Resolving it needs
    /// `/Resources`, which this layer does not have.
    pub ext_gstate: Option<Name>,
    /// The clipping region, in device space. `None` means unclipped.
    ///
    /// A bounding box, not a path. An exact clip needs path intersection with
    /// winding rules, and the question every consumer of this actually asks —
    /// *how much of the page can this operator mark?* — is answered by a box
    /// that is never smaller than the true region. Being too generous is safe
    /// in one direction only, which is why it is a box rather than, say, the
    /// path's first subpath: a caller may act on "this cannot be outside here"
    /// and must never act on "this covers all of here".
    ///
    /// `Some(rect)` where the rectangle is empty means the clip excludes
    /// everything, which is a real state and different from `None`.
    pub clip: Option<Rect>,
    /// Whether [`clip`](Self::clip) is the clipping region exactly, rather than
    /// a box around it.
    ///
    /// True when every clip path applied was a single axis-aligned rectangle,
    /// which is what essentially every producer emits — a `re W n` around a
    /// column, a figure or a table cell. False once any other shape has been
    /// intersected in, and then the box is an *over-estimate*: content the box
    /// admits may still be clipped away.
    ///
    /// Reported rather than resolved. Making it exact for the general case
    /// means intersecting arbitrary paths under two winding rules with curves
    /// in them, which is a polygon clipper — a rendering component, and this
    /// layer does not render. A consumer that needs certainty can check this
    /// flag and decline; one that needs a bound can use the box either way.
    /// True when there is no clip at all, because "everything" is exact.
    pub clip_exact: bool,
}

impl Default for GraphicsState {
    fn default() -> Self {
        GraphicsState {
            ctm: Matrix::IDENTITY,
            text: TextState::default(),
            stroke_colour: Colour::default(),
            fill_colour: Colour::default(),
            stroke_space: None,
            fill_space: None,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash_array: SmallVec::new(),
            dash_phase: 0.0,
            rendering_intent: None,
            flatness: 0.0,
            ext_gstate: None,
            clip: None,
            clip_exact: true,
        }
    }
}

/// Drives the graphics and text state through a stream of operators.
///
/// Text-showing operators are *not* interpreted here: turning a string into
/// positioned glyphs needs font metrics, which arrive with the resource layer.
/// This machine tracks everything else and exposes the hooks that extraction
/// needs -- [`StateMachine::text_rendering_matrix`] and
/// [`StateMachine::advance_text`].
pub struct StateMachine {
    current: GraphicsState,
    stack: Vec<GraphicsState>,
    /// `Tm`. Meaningful only between `BT` and `ET`.
    text_matrix: Matrix,
    /// `Tlm`, the line matrix that `Td` and `T*` translate.
    line_matrix: Matrix,
    in_text: bool,
    leniencies: Vec<Leniency>,
    /// Device-space bounds of the path under construction.
    ///
    /// Tracked here rather than in each visitor because the clip is graphics
    /// state and a visitor that wanted it would have to reimplement path
    /// construction to get it — which is what `graphics::Collector` was doing,
    /// and it still does for the geometry it needs beyond a box.
    path: Option<Rect>,
    /// A `W` or `W*` was seen; the clip changes at the next painting operator.
    pending_clip: bool,
    /// Whether the path under construction is a single axis-aligned rectangle.
    ///
    /// Tracked as the operators arrive rather than derived from the bounding
    /// box afterwards, because a box cannot tell a rectangle from a triangle
    /// that happens to fit inside it.
    path_is_rect: bool,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new(Matrix::IDENTITY)
    }
}

impl StateMachine {
    /// `base_ctm` maps default user space to device space -- for a page, the
    /// flip and translate implied by `/MediaBox` and `/Rotate`.
    pub fn new(base_ctm: Matrix) -> Self {
        StateMachine {
            current: GraphicsState { ctm: base_ctm, ..Default::default() },
            stack: Vec::new(),
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            in_text: false,
            leniencies: Vec::new(),
            path: None,
            pending_clip: false,
            path_is_rect: false,
        }
    }

    pub fn state(&self) -> &GraphicsState {
        &self.current
    }

    /// Device-space bounds of the path under construction, if any.
    ///
    /// Cleared by every painting operator and by `n`. Useful to a visitor that
    /// wants the extent of the path an operator is about to paint without
    /// tracking the construction operators itself.
    pub fn path_bounds(&self) -> Option<Rect> {
        self.path
    }

    /// The clip in force, as a device-space bounding box.
    pub fn clip(&self) -> Option<Rect> {
        self.current.clip
    }

    /// Whether [`clip`](Self::clip) is the region exactly rather than a box
    /// around it. See [`GraphicsState::clip_exact`].
    pub fn clip_is_exact(&self) -> bool {
        self.current.clip_exact
    }

    /// `bbox` reduced to the part the clip can actually show.
    ///
    /// The operation every consumer of the clip wants: an image drawn under a
    /// clip that hides four fifths of it should report the fifth that shows.
    /// `None` means the clip excludes it entirely.
    pub fn clipped(&self, bbox: Rect) -> Option<Rect> {
        match self.current.clip {
            Some(clip) => bbox.intersect(&clip),
            None => Some(bbox),
        }
    }

    fn extend_path(&mut self, x: f64, y: f64) {
        let p = self.current.ctm.apply(Point { x, y });
        let point = Rect { x0: p.x, y0: p.y, x1: p.x, y1: p.y };
        self.path = Some(match self.path {
            Some(existing) => existing.union(&point),
            None => point,
        });
    }

    /// Finish the current path: install a pending clip, then discard it.
    ///
    /// The clip narrows at the *painting* operator, which is what ISO 32000-1
    /// §8.5.4 specifies — `W` records the intention and the painting operator
    /// carries it out. A page that says `re W n` clips to the rectangle and
    /// paints nothing, and treating `W` itself as the change would clip
    /// everything between the `W` and the `n`, which is nothing at all.
    fn end_path(&mut self) {
        if self.pending_clip {
            // An empty intersection is kept as an empty rectangle rather than
            // discarded: "this clip shows nothing" is information, and folding
            // it to `None` would say the opposite.
            let region = self.path.unwrap_or(Rect { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 });
            self.current.clip = Some(match self.current.clip {
                Some(existing) => existing.intersect(&region).unwrap_or(Rect {
                    x0: region.x0,
                    y0: region.y0,
                    x1: region.x0,
                    y1: region.y0,
                }),
                None => region,
            });
            // Exactness is a property of the whole accumulated clip, so it can
            // only ever be lost. One rounded-corner panel anywhere in the stack
            // makes every box below it an over-estimate.
            self.current.clip_exact &= self.path_is_rect;
        }
        self.pending_clip = false;
        self.path = None;
        self.path_is_rect = false;
    }

    pub fn state_mut(&mut self) -> &mut GraphicsState {
        &mut self.current
    }

    pub fn text(&self) -> &TextState {
        &self.current.text
    }

    pub fn ctm(&self) -> Matrix {
        self.current.ctm
    }

    pub fn text_matrix(&self) -> Matrix {
        self.text_matrix
    }

    pub fn line_matrix(&self) -> Matrix {
        self.line_matrix
    }

    pub fn in_text_object(&self) -> bool {
        self.in_text
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn take_leniencies(&mut self) -> Vec<Leniency> {
        std::mem::take(&mut self.leniencies)
    }

    /// The matrix mapping glyph space to device space, ISO 32000-1 §9.4.4:
    ///
    /// ```text
    /// Trm = | Tfs x Th   0    0 |
    ///       | 0          Tfs  0 |  x  Tm  x  CTM
    ///       | 0          Ts   1 |
    /// ```
    pub fn text_rendering_matrix(&self) -> Matrix {
        let t = &self.current.text;
        let params = Matrix::new(
            t.font_size * (t.horizontal_scale / 100.0),
            0.0,
            0.0,
            t.font_size,
            0.0,
            t.rise,
        );
        params.then(&self.text_matrix).then(&self.current.ctm)
    }

    /// Move the text matrix along the baseline by `tx` (and `ty` in vertical
    /// mode). Called once per glyph by the extraction layer.
    pub fn advance_text(&mut self, tx: f64, ty: f64) {
        self.text_matrix = Matrix::translate(tx, ty).then(&self.text_matrix);
    }

    /// Apply one operator.
    ///
    /// Text-showing operators update the line matrix where they should (`'` and
    /// `"` perform `T*` first) but do not advance the text matrix, because that
    /// needs glyph widths.
    pub fn apply(&mut self, op: &Op) {
        match op.kind {
            // --- Graphics state ---
            OpKind::Save => {
                if self.stack.len() >= MAX_STACK {
                    self.note(op, "graphics state stack overflow; ignoring q");
                    return;
                }
                self.stack.push(self.current.clone());
            }
            OpKind::Restore => match self.stack.pop() {
                Some(s) => self.current = s,
                None => self.note(op, "Q with no matching q"),
            },
            OpKind::Concat => {
                if let Some(m) = Matrix::from_array(&op.operands) {
                    if m.is_finite() {
                        self.current.ctm = m.then(&self.current.ctm);
                    } else {
                        self.note(op, "cm with a non-finite matrix");
                    }
                } else {
                    self.note(op, "cm without six numeric operands");
                }
            }
            // --- Path construction and clipping ---
            //
            // Tracked as a bounding box only. `graphics::Collector` keeps the
            // geometry it needs for vector provenance; what belongs here is the
            // part that is graphics *state*, which is the clip.
            OpKind::MoveTo | OpKind::LineTo => {
                self.path_is_rect = false;
                self.extend_path(op.num_or_zero(0), op.num_or_zero(1));
            }
            // A Bézier lies within the convex hull of its control points, so
            // taking the points overstates the box slightly and never
            // understates it — the right direction for a clip, which must not
            // claim to exclude something it does not.
            OpKind::CurveTo => {
                self.path_is_rect = false;
                for i in [0, 2, 4] {
                    self.extend_path(op.num_or_zero(i), op.num_or_zero(i + 1));
                }
            }
            OpKind::CurveToInitialReplicated | OpKind::CurveToFinalReplicated => {
                self.path_is_rect = false;
                for i in [0, 2] {
                    self.extend_path(op.num_or_zero(i), op.num_or_zero(i + 1));
                }
            }
            OpKind::Rectangle => {
                // The first e on an empty path makes it a rectangle; a
                // second one makes it two, and a box around two rectangles is
                // not either of them.
                self.path_is_rect = self.path.is_none();
                let (x, y) = (op.num_or_zero(0), op.num_or_zero(1));
                let (w, h) = (op.num_or_zero(2), op.num_or_zero(3));
                for (dx, dy) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
                    self.extend_path(x + dx, y + dy);
                }
            }
            OpKind::ClosePath => {}
            OpKind::Clip | OpKind::ClipEvenOdd => self.pending_clip = true,
            OpKind::Stroke
            | OpKind::CloseStroke
            | OpKind::Fill
            | OpKind::FillObsolete
            | OpKind::FillEvenOdd
            | OpKind::FillStroke
            | OpKind::FillStrokeEvenOdd
            | OpKind::CloseFillStroke
            | OpKind::CloseFillStrokeEvenOdd
            | OpKind::EndPath => self.end_path(),

            OpKind::SetLineWidth => self.current.line_width = op.num_or_zero(0),
            OpKind::SetLineCap => self.current.line_cap = op.num_or_zero(0) as i64,
            OpKind::SetLineJoin => self.current.line_join = op.num_or_zero(0) as i64,
            OpKind::SetMiterLimit => self.current.miter_limit = op.num_or_zero(0),
            OpKind::SetDash => {
                self.current.dash_array = op
                    .operands
                    .first()
                    .and_then(Object::as_array)
                    .map(|a| a.iter().filter_map(Object::as_f64).collect())
                    .unwrap_or_default();
                self.current.dash_phase = op.num(1).unwrap_or(0.0);
            }
            OpKind::SetRenderingIntent => {
                self.current.rendering_intent = op.name(0).cloned();
            }
            OpKind::SetFlatness => self.current.flatness = op.num_or_zero(0),
            OpKind::SetExtGState => self.current.ext_gstate = op.name(0).cloned(),

            // --- Colour ---
            OpKind::SetStrokeColorSpace => {
                self.current.stroke_space = op.name(0).cloned();
                self.current.stroke_colour = initial_colour_for(op.name(0));
            }
            OpKind::SetFillColorSpace => {
                self.current.fill_space = op.name(0).cloned();
                self.current.fill_colour = initial_colour_for(op.name(0));
            }
            OpKind::SetStrokeColor | OpKind::SetStrokeColorN => {
                self.current.stroke_colour = read_colour(op, self.current.stroke_space.as_ref());
            }
            OpKind::SetFillColor | OpKind::SetFillColorN => {
                self.current.fill_colour = read_colour(op, self.current.fill_space.as_ref());
            }
            OpKind::SetStrokeGray => {
                self.current.stroke_colour = Colour::Gray(op.num_or_zero(0));
                self.current.stroke_space = None;
            }
            OpKind::SetFillGray => {
                self.current.fill_colour = Colour::Gray(op.num_or_zero(0));
                self.current.fill_space = None;
            }
            OpKind::SetStrokeRgb => {
                self.current.stroke_colour =
                    Colour::Rgb(op.num_or_zero(0), op.num_or_zero(1), op.num_or_zero(2));
                self.current.stroke_space = None;
            }
            OpKind::SetFillRgb => {
                self.current.fill_colour =
                    Colour::Rgb(op.num_or_zero(0), op.num_or_zero(1), op.num_or_zero(2));
                self.current.fill_space = None;
            }
            OpKind::SetStrokeCmyk => {
                self.current.stroke_colour = Colour::Cmyk(
                    op.num_or_zero(0),
                    op.num_or_zero(1),
                    op.num_or_zero(2),
                    op.num_or_zero(3),
                );
                self.current.stroke_space = None;
            }
            OpKind::SetFillCmyk => {
                self.current.fill_colour = Colour::Cmyk(
                    op.num_or_zero(0),
                    op.num_or_zero(1),
                    op.num_or_zero(2),
                    op.num_or_zero(3),
                );
                self.current.fill_space = None;
            }

            // --- Text objects ---
            OpKind::BeginText => {
                if self.in_text {
                    self.note(op, "BT inside a text object");
                }
                self.in_text = true;
                // ISO 32000-1 §9.4.1: BT resets both matrices to identity.
                self.text_matrix = Matrix::IDENTITY;
                self.line_matrix = Matrix::IDENTITY;
            }
            OpKind::EndText => {
                if !self.in_text {
                    self.note(op, "ET with no matching BT");
                }
                self.in_text = false;
            }

            // --- Text state ---
            OpKind::SetCharSpacing => self.current.text.char_spacing = op.num_or_zero(0),
            OpKind::SetWordSpacing => self.current.text.word_spacing = op.num_or_zero(0),
            OpKind::SetHorizontalScale => self.current.text.horizontal_scale = op.num_or_zero(0),
            OpKind::SetLeading => self.current.text.leading = op.num_or_zero(0),
            OpKind::SetFont => {
                self.current.text.font = op.name(0).cloned();
                self.current.text.font_size = op.num(1).unwrap_or(0.0);
            }
            OpKind::SetRenderMode => self.current.text.render_mode = op.num_or_zero(0) as i64,
            OpKind::SetRise => self.current.text.rise = op.num_or_zero(0),

            // --- Text positioning ---
            OpKind::TextMove => {
                let [tx, ty] = op.trailing_nums::<2>().unwrap_or([0.0, 0.0]);
                self.text_move(tx, ty);
            }
            OpKind::TextMoveSetLeading => {
                let [tx, ty] = op.trailing_nums::<2>().unwrap_or([0.0, 0.0]);
                // TD is Td with the leading set to -ty first.
                self.current.text.leading = -ty;
                self.text_move(tx, ty);
            }
            OpKind::SetTextMatrix => {
                if let Some(m) = Matrix::from_array(&op.operands)
                    && m.is_finite()
                {
                    // Tm replaces both matrices outright; it does not compose.
                    self.text_matrix = m;
                    self.line_matrix = m;
                } else {
                    self.note(op, "Tm without six finite numeric operands");
                }
            }
            OpKind::NextLine => self.next_line(),

            // --- Text showing ---
            // `'` is T* then Tj.
            OpKind::NextLineShowText => self.next_line(),
            // `"` sets Tw and Tc, then behaves as `'`.
            OpKind::NextLineSetSpacingShowText => {
                if let Some([aw, ac]) =
                    op.operands.len().checked_sub(3).and_then(|_| Some([op.num(0)?, op.num(1)?]))
                {
                    self.current.text.word_spacing = aw;
                    self.current.text.char_spacing = ac;
                }
                self.next_line();
            }
            OpKind::ShowText | OpKind::ShowTextAdjusted => {}

            _ => {}
        }
    }

    /// `Td`: translate the *line* matrix, then set the text matrix from it.
    fn text_move(&mut self, tx: f64, ty: f64) {
        self.line_matrix = Matrix::translate(tx, ty).then(&self.line_matrix);
        self.text_matrix = self.line_matrix;
    }

    /// `T*` is defined as `0 -TL Td`.
    fn next_line(&mut self) {
        let leading = self.current.text.leading;
        self.text_move(0.0, -leading);
    }

    fn note(&mut self, op: &Op, detail: impl Into<String>) {
        self.leniencies.push(Leniency::new(LeniencyKind::UnknownKeyword, op.span.start, detail));
    }
}

/// ISO 32000-1 §8.6.8: the initial colour depends on the space.
fn initial_colour_for(space: Option<&Name>) -> Colour {
    match space.map(|n| n.as_bytes().to_vec()).as_deref() {
        Some(b"DeviceRGB" | b"CalRGB" | b"Lab") => Colour::Rgb(0.0, 0.0, 0.0),
        Some(b"DeviceCMYK") => Colour::Cmyk(0.0, 0.0, 0.0, 1.0),
        Some(b"DeviceGray" | b"CalGray") => Colour::Gray(0.0),
        Some(b"Pattern") => {
            Colour::Unresolved { space: space.cloned(), components: SmallVec::new(), pattern: None }
        }
        _ => {
            Colour::Unresolved { space: space.cloned(), components: SmallVec::new(), pattern: None }
        }
    }
}

/// Read the operands of `sc`/`scn`/`SC`/`SCN`, which vary with the colour space.
fn read_colour(op: &Op, space: Option<&Name>) -> Colour {
    // `scn` in a pattern space takes a name as its last operand.
    let pattern = op.operands.last().and_then(Object::as_name).cloned();
    let components: SmallVec<[f64; 4]> = op.operands.iter().filter_map(Object::as_f64).collect();

    if pattern.is_some() {
        return Colour::Unresolved { space: space.cloned(), components, pattern };
    }
    // With a device space selected, the component count is unambiguous.
    match space.map(|n| n.as_bytes().to_vec()).as_deref() {
        Some(b"DeviceGray" | b"CalGray") if components.len() == 1 => Colour::Gray(components[0]),
        Some(b"DeviceRGB" | b"CalRGB") if components.len() == 3 => {
            Colour::Rgb(components[0], components[1], components[2])
        }
        Some(b"DeviceCMYK") if components.len() == 4 => {
            Colour::Cmyk(components[0], components[1], components[2], components[3])
        }
        // Otherwise the space needs /Resources to interpret. Carrying the
        // components unresolved is honest; guessing by count would silently
        // turn a one-component Separation into a grey.
        _ => Colour::Unresolved { space: space.cloned(), components, pattern: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenize;

    fn run(src: &[u8]) -> StateMachine {
        let mut sm = StateMachine::default();
        for op in tokenize(src).0 {
            sm.apply(&op);
        }
        sm
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // --- the Tw rule, spec 6.3's headline warning ---------------------------

    #[test]
    fn word_spacing_applies_only_to_single_byte_code_32() {
        assert!(word_spacing_applies(32, 1), "a simple-font space");
        assert!(!word_spacing_applies(32, 2), "code 32 in a two-byte CID font must not");
        assert!(!word_spacing_applies(0x0020, 2), "same value, two bytes, still not");
        assert!(!word_spacing_applies(65, 1), "'A' is not a space");
        // The byte value 32 appearing inside a multi-byte code.
        assert!(!word_spacing_applies(0x2020, 2));
    }

    #[test]
    fn word_spacing_changes_the_displacement_it_applies_to() {
        let ts = TextState { font_size: 10.0, word_spacing: 5.0, ..Default::default() };
        let with = ts.displacement(0.5, 0.0, true);
        let without = ts.displacement(0.5, 0.0, false);
        assert!(close(with - without, 5.0));
        assert!(close(without, 5.0));
    }

    // --- the displacement formula -------------------------------------------

    #[test]
    fn displacement_matches_the_specified_formula() {
        // tx = ((w0 - Tj/1000) x Tfs + Tc + Tw) x (Tz/100)
        let ts = TextState {
            font_size: 12.0,
            char_spacing: 1.0,
            word_spacing: 2.0,
            horizontal_scale: 50.0,
            ..Default::default()
        };
        // w0=0.5, adjustment=-200, so (0.5 + 0.2) * 12 + 1 + 2 = 11.4, halved.
        let tx = ts.displacement(0.5, -200.0, true);
        assert!(close(tx, ((0.5 + 0.2) * 12.0 + 1.0 + 2.0) * 0.5), "{tx}");
    }

    #[test]
    fn tj_adjustment_is_applied_before_scaling() {
        let ts = TextState { font_size: 10.0, horizontal_scale: 200.0, ..Default::default() };
        // The adjustment must be inside the Tz multiplication, not outside.
        let tx = ts.displacement(0.0, -1000.0, false);
        assert!(close(tx, 1.0 * 10.0 * 2.0), "{tx}");
    }

    #[test]
    fn horizontal_scale_does_not_apply_vertically() {
        let ts = TextState {
            font_size: 10.0,
            horizontal_scale: 50.0,
            char_spacing: 1.0,
            ..Default::default()
        };
        // Vertical displacement ignores Tz entirely.
        let ty = ts.displacement_vertical(1.0, 0.0, false);
        assert!(close(ty, 1.0 * 10.0 + 1.0), "{ty}");
    }

    #[test]
    fn invisible_render_modes_are_flagged() {
        let mut ts = TextState::default();
        assert!(ts.is_visible());
        ts.render_mode = 3;
        assert!(!ts.is_visible(), "Tr 3 is the OCR-under-image convention");
        ts.render_mode = 7;
        assert!(!ts.is_visible());
    }

    // --- text matrices -------------------------------------------------------

    #[test]
    fn bt_resets_both_text_matrices() {
        let sm = run(b"1 0 0 1 50 50 Tm BT");
        assert_eq!(sm.text_matrix(), Matrix::IDENTITY);
        assert_eq!(sm.line_matrix(), Matrix::IDENTITY);
        assert!(sm.in_text_object());
    }

    #[test]
    fn tm_replaces_rather_than_composes() {
        let sm = run(b"BT 2 0 0 2 10 10 Tm 3 0 0 3 20 20 Tm");
        // If Tm composed, the scale would be 6.
        assert_eq!(sm.text_matrix(), Matrix::new(3.0, 0.0, 0.0, 3.0, 20.0, 20.0));
    }

    #[test]
    fn td_translates_the_line_matrix_cumulatively() {
        let sm = run(b"BT 10 20 Td 5 5 Td");
        assert_eq!(sm.text_matrix(), Matrix::translate(15.0, 25.0));
        assert_eq!(sm.line_matrix(), Matrix::translate(15.0, 25.0));
    }

    #[test]
    fn td_moves_from_the_line_matrix_not_the_text_matrix() {
        // This is the distinction that matters: after showing text the text
        // matrix has advanced, but Td must start from the line matrix.
        let mut sm = StateMachine::default();
        for op in tokenize(b"BT 10 100 Td").0 {
            sm.apply(&op);
        }
        sm.advance_text(200.0, 0.0); // as if a long string had been shown
        assert_eq!(sm.text_matrix(), Matrix::translate(210.0, 100.0));
        for op in tokenize(b"0 -14 Td").0 {
            sm.apply(&op);
        }
        // Back to x=10, not x=210.
        assert_eq!(sm.text_matrix(), Matrix::translate(10.0, 86.0));
    }

    #[test]
    fn capital_td_sets_leading_to_negative_ty() {
        let sm = run(b"BT 0 -14 TD");
        assert!(close(sm.text().leading, 14.0));
        assert_eq!(sm.text_matrix(), Matrix::translate(0.0, -14.0));
    }

    #[test]
    fn t_star_is_zero_minus_leading_td() {
        let sm = run(b"BT 20 TL 100 700 Td T* T*");
        assert_eq!(sm.text_matrix(), Matrix::translate(100.0, 660.0));
    }

    #[test]
    fn quote_operator_performs_next_line() {
        let sm = run(b"BT 15 TL 10 700 Td (a) '");
        assert_eq!(sm.text_matrix(), Matrix::translate(10.0, 685.0));
    }

    #[test]
    fn double_quote_sets_spacing_then_next_line() {
        let sm = run(b"BT 12 TL 10 700 Td 3 1 (a) \"");
        assert!(close(sm.text().word_spacing, 3.0));
        assert!(close(sm.text().char_spacing, 1.0));
        assert_eq!(sm.text_matrix(), Matrix::translate(10.0, 688.0));
    }

    #[test]
    fn text_rendering_matrix_folds_size_scale_and_rise() {
        let sm = run(b"BT /F1 10 Tf 200 Tz 5 Ts 1 0 0 1 100 700 Tm");
        let trm = sm.text_rendering_matrix();
        // Horizontal scale doubles the x scale; rise shifts y by 5.
        assert!(close(trm.a, 20.0), "{trm:?}");
        assert!(close(trm.d, 10.0), "{trm:?}");
        assert!(close(trm.e, 100.0), "{trm:?}");
        assert!(close(trm.f, 705.0), "{trm:?}");
    }

    // --- graphics state stack -----------------------------------------------

    #[test]
    fn q_and_q_save_and_restore_text_state() {
        // Text state parameters are part of the graphics state, so they must
        // survive q/Q. Getting this wrong makes text drift after an unrelated Q.
        let sm = run(b"2 Tc q 9 Tc Q");
        assert!(close(sm.text().char_spacing, 2.0));
    }

    #[test]
    fn q_and_q_save_and_restore_the_ctm() {
        let sm = run(b"q 2 0 0 2 0 0 cm Q");
        assert_eq!(sm.ctm(), Matrix::IDENTITY);
        assert_eq!(sm.depth(), 0);
    }

    // --- clipping -----------------------------------------------------------

    #[test]
    fn a_rectangle_clip_narrows_the_region_at_the_painting_operator() {
        // `re W n` is how essentially every producer clips. The clip must not
        // take effect at `W` — everything between `W` and `n` would be clipped
        // by a region the page had not finished describing.
        let mut sm = StateMachine::default();
        for op in tokenize(b"100 100 200 50 re W").0 {
            sm.apply(&op);
        }
        assert_eq!(sm.clip(), None, "W records the intention; it does not act");

        for op in tokenize(b"n").0 {
            sm.apply(&op);
        }
        assert_eq!(sm.clip(), Some(Rect::new(100.0, 100.0, 300.0, 150.0)));
        assert_eq!(sm.path_bounds(), None, "the path is consumed by the painting op");
    }

    #[test]
    fn clips_intersect_rather_than_replace() {
        let sm = run(b"0 0 400 400 re W n 100 100 500 500 re W n");
        // The second clip extends past the first and cannot widen it.
        assert_eq!(sm.clip(), Some(Rect::new(100.0, 100.0, 400.0, 400.0)));
    }

    #[test]
    fn a_clip_is_graphics_state_and_q_restores_it() {
        // The reason clipping belongs in this struct rather than in a visitor:
        // it is saved and restored with everything else, and a visitor tracking
        // it separately would have to reimplement the stack to get this right.
        let sm = run(b"0 0 100 100 re W n q 0 0 10 10 re W n Q");
        assert_eq!(sm.clip(), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
    }

    #[test]
    fn a_clip_that_excludes_everything_is_not_the_same_as_no_clip() {
        // Two disjoint clips leave nothing visible. Folding that to `None`
        // would say the opposite of what the page says.
        let sm = run(b"0 0 10 10 re W n 100 100 10 10 re W n");
        let clip = sm.clip().expect("still clipped, to nothing");
        assert!(clip.is_empty(), "{clip:?}");
        assert_eq!(sm.clipped(Rect::new(0.0, 0.0, 5.0, 5.0)), None, "nothing shows through");
    }

    #[test]
    fn clipped_reduces_a_box_to_the_part_that_shows() {
        let sm = run(b"0 0 100 100 re W n");
        assert_eq!(
            sm.clipped(Rect::new(50.0, 50.0, 500.0, 500.0)),
            Some(Rect::new(50.0, 50.0, 100.0, 100.0)),
            "a figure clipped to a quarter reports the quarter"
        );
        assert_eq!(sm.clipped(Rect::new(200.0, 200.0, 300.0, 300.0)), None);

        let unclipped = StateMachine::default();
        let whole = Rect::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(unclipped.clipped(whole), Some(whole), "no clip changes nothing");
    }

    #[test]
    fn painting_without_a_clip_request_leaves_the_clip_alone() {
        // The mirror of the first test: an ordinary fill must not become a clip
        // just because it constructed a path.
        let sm = run(b"0 0 100 100 re f");
        assert_eq!(sm.clip(), None);
    }

    #[test]
    fn a_rectangular_clip_is_exact_and_anything_else_is_not() {
        // The distinction the flag exists for. Both clips produce a box; only
        // the first one *is* the region, and a consumer that treats the second
        // as the region will believe content is visible that a rounded corner
        // or a triangle clips away.
        let rect = run(b"100 100 200 200 re W n");
        assert!(rect.clip_is_exact());

        let triangle = run(b"100 100 m 300 100 l 200 300 l h W n");
        assert!(!triangle.clip_is_exact(), "a triangle is not its bounding box");
        assert_eq!(
            triangle.clip(),
            Some(Rect::new(100.0, 100.0, 300.0, 300.0)),
            "and the box is still offered, because it is still a bound"
        );

        assert!(StateMachine::default().clip_is_exact(), "no clip is exactly everything");
    }

    #[test]
    fn exactness_is_lost_by_any_clip_in_the_stack_and_restored_by_q() {
        // Only ever lost, never regained: intersecting a rectangle with a
        // triangle does not give back a rectangle.
        let sm = run(b"0 0 400 400 re W n 100 100 m 300 100 l 200 300 l h W n 0 0 50 50 re W n");
        assert!(!sm.clip_is_exact());

        // But it is graphics state, so a `Q` puts back what was true before.
        let restored = run(b"0 0 400 400 re W n q 10 10 m 20 20 l 30 10 l h W n Q");
        assert!(restored.clip_is_exact());
    }

    #[test]
    fn two_rectangles_in_one_clip_path_are_not_a_rectangle() {
        // `re re W n` clips to the union of two boxes, and the bounding box of
        // that union includes the gap between them.
        let sm = run(b"0 0 10 10 re 100 100 10 10 re W n");
        assert!(!sm.clip_is_exact());
    }

    #[test]
    fn the_clip_path_is_measured_in_device_space() {
        // A `cm` before the clip path moves it. Tracking the clip in user space
        // would put it in the wrong place for every page that scales.
        let sm = run(b"2 0 0 2 10 10 cm 0 0 50 50 re W n");
        assert_eq!(sm.clip(), Some(Rect::new(10.0, 10.0, 110.0, 110.0)));
    }

    #[test]
    fn text_matrices_are_not_part_of_the_graphics_state() {
        // Tm is not saved by q, so a Q must not restore it.
        let mut sm = StateMachine::default();
        for op in tokenize(b"BT 1 0 0 1 10 10 Tm q 1 0 0 1 99 99 Tm Q").0 {
            sm.apply(&op);
        }
        assert_eq!(sm.text_matrix(), Matrix::translate(99.0, 99.0));
    }

    #[test]
    fn unbalanced_restore_is_reported_not_fatal() {
        let mut sm = StateMachine::default();
        for op in tokenize(b"Q Q 1 0 0 1 5 5 cm").0 {
            sm.apply(&op);
        }
        assert_eq!(sm.ctm(), Matrix::translate(5.0, 5.0));
        assert_eq!(sm.take_leniencies().len(), 2);
    }

    #[test]
    fn nesting_is_bounded() {
        let src = b"q ".repeat(MAX_STACK + 50);
        let mut sm = StateMachine::default();
        for op in tokenize(&src).0 {
            sm.apply(&op);
        }
        assert!(sm.depth() <= MAX_STACK);
        assert!(!sm.take_leniencies().is_empty());
    }

    #[test]
    fn ctm_composes_in_the_right_order() {
        // ISO 32000-1 8.4.4: `cm` *prepends*, so the newest matrix applies
        // first to subsequent coordinates. Scale by 2, then translate by 10,
        // means a point at 1 is translated to 11 and then scaled to 22 -- not
        // scaled to 2 and translated to 12.
        let sm = run(b"2 0 0 2 0 0 cm 1 0 0 1 10 0 cm");
        let p = sm.ctm().apply(crate::Point::new(1.0, 0.0));
        assert!(close(p.x, 22.0), "{p:?}");

        // Reversing the two operators gives the other answer, which is the
        // check that this is testing order and not arithmetic.
        let sm = run(b"1 0 0 1 10 0 cm 2 0 0 2 0 0 cm");
        let p = sm.ctm().apply(crate::Point::new(1.0, 0.0));
        assert!(close(p.x, 12.0), "{p:?}");
    }

    #[test]
    fn base_ctm_is_respected() {
        let base = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0);
        let mut sm = StateMachine::new(base);
        for op in tokenize(b"1 0 0 1 10 20 cm").0 {
            sm.apply(&op);
        }
        let p = sm.ctm().apply(crate::Point::new(0.0, 0.0));
        assert!(close(p.x, 10.0) && close(p.y, 772.0), "{p:?}");
    }

    // --- colour ---------------------------------------------------------------

    #[test]
    fn device_colour_operators_are_read() {
        let sm = run(b"1 0 0 RG 0 1 0 rg 0.5 G 0 0 0 1 K");
        assert_eq!(sm.state().fill_colour, Colour::Rgb(0.0, 1.0, 0.0));
        assert_eq!(sm.state().stroke_colour, Colour::Cmyk(0.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn scn_in_a_device_space_resolves() {
        let sm = run(b"/DeviceRGB cs 0.1 0.2 0.3 scn");
        assert_eq!(sm.state().fill_colour, Colour::Rgb(0.1, 0.2, 0.3));
    }

    #[test]
    fn scn_in_an_unknown_space_stays_unresolved() {
        // A Separation space has one component but is not a grey. Guessing
        // would silently produce the wrong colour.
        let sm = run(b"/MySpot cs 0.4 scn");
        match &sm.state().fill_colour {
            Colour::Unresolved { components, space, .. } => {
                assert_eq!(components.as_slice(), &[0.4]);
                assert_eq!(space.as_ref().unwrap().as_bytes(), b"MySpot");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pattern_colour_keeps_its_name() {
        let sm = run(b"/Pattern cs /P1 scn");
        match &sm.state().fill_colour {
            Colour::Unresolved { pattern: Some(p), .. } => assert_eq!(p.as_bytes(), b"P1"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cmyk_to_rgb_is_available_but_unresolved_is_not_guessed() {
        assert_eq!(Colour::Cmyk(0.0, 0.0, 0.0, 0.0).to_rgb(), Some((1.0, 1.0, 1.0)));
        assert_eq!(Colour::Gray(0.5).to_rgb(), Some((0.5, 0.5, 0.5)));
        let unresolved = Colour::Unresolved {
            space: None,
            components: SmallVec::from_slice(&[0.1, 0.2]),
            pattern: None,
        };
        assert_eq!(unresolved.to_rgb(), None, "two components is not a colour we can name");
    }

    // --- malformed input -------------------------------------------------------

    #[test]
    fn malformed_operators_do_not_corrupt_state() {
        let sm = run(b"1 2 cm 1 0 0 1 7 7 cm");
        // The short cm is ignored; the good one applies.
        assert_eq!(sm.ctm(), Matrix::translate(7.0, 7.0));
    }

    #[test]
    fn overflowing_numbers_are_sanitised_before_they_reach_the_matrix() {
        // The lexer clamps a number that overflows to 0 and records it, so an
        // infinity never reaches this layer. The resulting zero scale is legal
        // PDF -- it is how content is made invisible -- so it is applied rather
        // than rejected. The `is_finite` guard in `apply` is defensive only.
        let (ops, leniencies) = tokenize(b"1e400 0 0 1 0 0 cm");
        assert!(
            leniencies.iter().any(|l| l.kind == LeniencyKind::MalformedNumber),
            "{leniencies:?}"
        );
        let mut sm = StateMachine::default();
        for op in ops {
            sm.apply(&op);
        }
        assert_eq!(sm.ctm(), Matrix::new(0.0, 0.0, 0.0, 1.0, 0.0, 0.0));
        assert!(sm.ctm().is_finite());
        assert!(sm.ctm().invert().is_none(), "a zero scale is singular, and legally so");
    }
}
