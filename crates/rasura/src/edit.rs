//! Editing, and the fidelity contract. Spec 11.4.
//!
//! > ```ts
//! > const r = await session.replaceText(para.id, { start: 0, end: 10 }, 'Q4 net revenue');
//! > r.fidelity;      // 'exact' | 'reembedded' | 'substituted' | 'overlaid'
//! > if (r.fidelity !== 'exact') await session.undo();
//! > ```
//!
//! # `requireFidelity` is the whole point
//!
//! > A strict caller sets `session.requireFidelity('exact')` and every operation
//! > that cannot meet it fails instead of degrading. A contract-redlining tool
//! > sets `'exact'`; a form-filler accepts `'substituted'`. **That single knob
//! > is worth more than any feature in this document.**
//!
//! It is implemented as [`Session::require`], and the reason it matters is that
//! the two callers want opposite things from the same operation. A form-filler
//! would rather have the field filled in a substituted typeface than not filled.
//! A redlining tool would rather fail loudly than hand a lawyer a document whose
//! amended clause is set in a font the original never used — because the second
//! outcome is invisible in a diff and visible in court.
//!
//! Without the knob, a library has to choose one of them and be wrong for the
//! other. With it, both get what they need from one code path.
//!
//! # The ladder, and where this crate currently stands on it
//!
//! §11.4 defines four rungs. Two are reachable today:
//!
//! | Rung | Meaning | State |
//! |---|---|---|
//! | `exact` | original glyphs, metrics and mechanism | yes |
//! | `reembedded` | glyphs injected into the embedded font | yes — §8.4 |
//! | `substituted` | a different typeface was used | §8.5 exists; not wired to editing |
//! | `overlaid` | original masked, new text drawn on top | not built |
//!
//! [`Fidelity::Substituted`] and [`Fidelity::Overlaid`] are in the enum and
//! never produced. That is deliberate: a caller writing `require(Exact)` today
//! must keep working unchanged when substitution lands, and an enum that grew a
//! variant later would silently change the meaning of every `match` written
//! against it.

use crate::error::{Code, Error, Result};
use crate::ops::{EditOp, OpLog};
use crate::page::{ImageId, Page, ParagraphId};
use rasura_edit::session::{Compromise, Fidelity as RawFidelity};
use std::ops::Range;

pub use rasura_edit::reflow::{Breaking, Overflow};

/// A block operation's refusal, with its reason kept.
///
/// InsideForm is the one worth a distinct message: the image is drawn inside
/// a form XObject, whose spans address the form's own stream, and a form may be
/// invoked from several pages — so editing it changes all of them.
fn block_error(e: rasura_edit::blocks::BlockError) -> Error {
    Error::from_layer(Code::Malformed, "the image could not be edited", e)
}

fn page_error(e: rasura_edit::pages::PageError) -> Error {
    Error::from_layer(Code::Malformed, "the page operation was refused", e)
}

fn annot_error(e: rasura_edit::AnnotError) -> Error {
    Error::from_layer(Code::Malformed, "the annotation could not be written", e)
}

fn place_error(e: rasura_edit::place::PlaceError) -> Error {
    use rasura_edit::place::PlaceError as P;
    let code = match e {
        // Nothing in the text could be drawn by a standard face: the same code
        // `replace_text` uses when no font can write the characters asked for.
        P::Unencodable(_) => Code::FontUnavailable,
        _ => Code::Malformed,
    };
    Error::from_layer(code, "the content could not be placed", e)
}

/// Where placed text goes. Spec 9.2.
#[derive(Debug, Clone)]
pub enum PlaceAnchor {
    /// An absolute page-space origin: the left end of the text baseline.
    At { x: f64, y: f64 },
    /// Just after a paragraph whose text contains `label`, by `gap` units — the
    /// common case on a flat form, where the field to fill is the space to the
    /// right of a printed caption.
    NearLabel { label: String, gap: f64 },
}

/// How faithful an operation managed to be. Spec 11.4.
///
/// Ordered worst to best so a required floor is a comparison rather than a
/// match: `Exact` is the strictest and sorts highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fidelity {
    /// Original content was masked and new text drawn over it. The last resort,
    /// and not implemented — see the module note.
    Overlaid,
    /// A different typeface was used. Not implemented.
    Substituted,
    /// Glyphs were injected into the document's own embedded font from a
    /// registered source. The document still uses one typeface; it now contains
    /// more of it.
    Reembedded,
    /// Original embedded glyphs, original metrics, original mechanism.
    Exact,
}

impl Fidelity {
    pub fn as_str(self) -> &'static str {
        match self {
            Fidelity::Exact => "exact",
            Fidelity::Reembedded => "reembedded",
            Fidelity::Substituted => "substituted",
            Fidelity::Overlaid => "overlaid",
        }
    }

    /// Derive the rung from what the edit layer reported it did.
    ///
    /// Injection is the only compromise that changes *which glyphs exist*, so
    /// it is the only one that moves the rung. Everything else — regenerated
    /// kerning, re-broken lines, an overflowed block — is reported in
    /// [`Outcome::warnings`] and leaves the fidelity at `Exact`, because the
    /// glyphs drawn are the document's own.
    fn from_raw(raw: &RawFidelity) -> Fidelity {
        match raw {
            RawFidelity::Exact => Fidelity::Exact,
            RawFidelity::Degraded(list) => {
                if list.iter().any(|c| matches!(c, Compromise::GlyphsInjected { .. })) {
                    Fidelity::Reembedded
                } else {
                    Fidelity::Exact
                }
            }
        }
    }
}

/// What an operation did, beyond succeeding. Spec 11.4.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub fidelity: Fidelity,
    /// Characters the font could not write and no injection could supply.
    pub missing_glyphs: Vec<String>,
    /// How many lines the paragraph has now, when re-breaking changed it.
    pub reflowed_lines: Option<usize>,
    /// Everything else worth knowing, in the layer's own words.
    pub warnings: Vec<String>,
}

impl Outcome {
    fn from_raw(raw: &RawFidelity) -> Outcome {
        let mut missing_glyphs = Vec::new();
        let mut reflowed_lines = None;
        let mut warnings = Vec::new();

        if let RawFidelity::Degraded(list) = raw {
            for c in list {
                match c {
                    Compromise::GlyphUnavailable { text } => missing_glyphs.push(text.clone()),
                    Compromise::LinesRebroken { after, .. } => reflowed_lines = Some(*after),
                    other => warnings.push(format!("{other:?}")),
                }
            }
        }
        Outcome { fidelity: Fidelity::from_raw(raw), missing_glyphs, reflowed_lines, warnings }
    }
}

/// What a session remembers between operations.
///
/// Separated from [`Session`] for the reason
/// [`rasura_edit::SessionState`] is separated from its own session: a
/// borrow cannot be parked. A caller holding documents in a map — the WASM
/// handle table, an FFI boundary, a server — stores this and hands it back with
/// the document when the next operation arrives.
#[derive(Debug, Clone)]
pub struct SessionState {
    inner: rasura_edit::SessionState,
    require: Fidelity,
    policy: rasura_edit::reflow::Policy,
    /// The forward op log: what was done, as replayable data. Spec 9.1, as data.
    ///
    /// Parked here with the rest of the session's memory so it survives a
    /// suspend/resume round trip through a handle table exactly as the undo log
    /// does — a serialised edit that forgot half its operations on the way to
    /// storage would be worse than none.
    ops: Vec<EditOp>,
    /// Operations undone but not yet superseded, newest last — the forward
    /// log's mirror of the undo stack's redo half.
    ops_redo: Vec<EditOp>,
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState {
            inner: rasura_edit::SessionState::default(),
            // The floor a caller gets without asking is the bottom rung: an
            // operation that had to degrade still happens, and says so. A
            // default of `Exact` would make the common case fail for callers
            // who never thought about fidelity at all.
            require: Fidelity::Overlaid,
            policy: rasura_edit::reflow::Policy::default(),
            ops: Vec::new(),
            ops_redo: Vec::new(),
        }
    }
}

impl SessionState {
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn redo_len(&self) -> usize {
        self.inner.redo_len()
    }

    /// Refuse any operation that cannot reach this rung. Spec 11.4.
    pub fn require(&mut self, floor: Fidelity) -> &mut Self {
        self.require = floor;
        self
    }

    /// How to re-break a paragraph that no longer fits. Spec 9.3.
    pub fn reflow(&mut self, breaking: Breaking, overflow: Overflow) -> &mut Self {
        self.policy = rasura_edit::reflow::Policy { breaking, overflow };
        self
    }

    /// The forward op log as it stands, without resuming the session. Spec 9.1.
    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }
}

/// An edit in progress. Spec 9.1, 11.4.
///
/// Nothing reaches the document until [`Session::commit`]. A session that is
/// dropped without committing leaves the document exactly as it was.
pub struct Session<'a> {
    inner: rasura_edit::EditSession<'a>,
    require: Fidelity,
    policy: rasura_edit::reflow::Policy,
    registry: Vec<crate::supply::Registered>,
    /// The forward op log. See [`SessionState::ops`].
    ops: Vec<EditOp>,
    ops_redo: Vec<EditOp>,
}

impl<'a> Session<'a> {
    pub(crate) fn new(
        doc: &'a mut rasura_cos::Document,
        registry: Vec<crate::supply::Registered>,
    ) -> Session<'a> {
        Session::resume(doc, SessionState::default(), registry)
    }

    /// Reattach parked state to a document. Spec 9.1.
    pub fn resume(
        doc: &'a mut rasura_cos::Document,
        state: SessionState,
        registry: Vec<crate::supply::Registered>,
    ) -> Session<'a> {
        Session {
            inner: rasura_edit::EditSession::resume(doc, state.inner),
            require: state.require,
            policy: state.policy,
            registry,
            ops: state.ops,
            ops_redo: state.ops_redo,
        }
    }

    /// Park the session and release the document.
    pub fn suspend(self) -> SessionState {
        SessionState {
            inner: self.inner.suspend(),
            require: self.require,
            policy: self.policy,
            ops: self.ops,
            ops_redo: self.ops_redo,
        }
    }

    /// Record a forward operation. Called at each public mutating method once
    /// the operation has actually succeeded, so a refused edit logs nothing.
    ///
    /// A new operation clears the redo half, for the same reason the undo log
    /// does: the history has branched, and the ops a redo would replay are no
    /// longer reachable from here.
    fn record(&mut self, op: EditOp) {
        self.ops.push(op);
        self.ops_redo.clear();
    }

    /// The forward op log: every operation this session has applied and not
    /// undone, as replayable data. Spec 9.1.
    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }

    /// The op log stamped with the library version that produced it.
    ///
    /// This is the serialisable artefact: store it beside the original bytes
    /// and [`apply`](Self::apply) reconstructs the edited document from the two.
    pub fn op_log(&self) -> OpLog {
        OpLog::new(self.ops.clone())
    }

    /// Re-issue one recorded operation against this session. Spec 9.1.
    ///
    /// Replay is defined as exact against the document the op was captured on:
    /// the flat coordinates it carries are resolved against a freshly analysed
    /// page, which is deterministic for identical bytes. A coordinate that does
    /// not resolve — a paragraph index past the end, a page that is not there —
    /// is refused with a code by the same guard the live method uses, never
    /// applied to whatever else sits at that index.
    ///
    /// Applying an op records it again, so replaying a log reproduces that log.
    pub fn apply(&mut self, op: &EditOp) -> Result<Outcome> {
        match op {
            EditOp::ReplaceText { page, paragraph, start, end, text } => {
                let pg = self.page(*page)?;
                let id = paragraph_at(&pg, *paragraph)?;
                self.replace_text(&pg, id, *start..*end, text)
            }
            EditOp::InsertText { page, paragraph, at, text } => {
                let pg = self.page(*page)?;
                let id = paragraph_at(&pg, *paragraph)?;
                self.insert_text(&pg, id, *at, text)
            }
            EditOp::DeleteRange { page, paragraph, start, end } => {
                let pg = self.page(*page)?;
                let id = paragraph_at(&pg, *paragraph)?;
                self.delete_range(&pg, id, *start..*end)
            }
            EditOp::MoveImage { page, image, dx, dy } => {
                let pg = self.page(*page)?;
                let id = image_at(&pg, *image)?;
                self.move_image(&pg, id, *dx, *dy)
            }
            EditOp::ScaleImage { page, image, sx, sy } => {
                let pg = self.page(*page)?;
                let id = image_at(&pg, *image)?;
                self.scale_image(&pg, id, *sx, *sy)
            }
            EditOp::DeleteImage { page, image } => {
                let pg = self.page(*page)?;
                let id = image_at(&pg, *image)?;
                self.delete_image(&pg, id)
            }
            EditOp::SetCell { page, table, row, column, text } => {
                let pg = self.page(*page)?;
                self.set_cell(&pg, *table, *row, *column, text)
            }
            EditOp::DeletePage { index } => self.delete_page(*index),
            EditOp::MovePage { from, to } => self.move_page(*from, *to),
            EditOp::SetFieldValue { name, value } => self.set_field_value(name, value),
            EditOp::FlattenForms { page } => {
                let pg = self.page(*page)?;
                self.flatten_forms(&pg)
            }
            EditOp::AddAnnotation { page, annotation } => {
                let pg = self.page(*page)?;
                self.add_annotation(&pg, annotation)
            }
            EditOp::DeleteAnnotation { page, id } => {
                let pg = self.page(*page)?;
                self.delete_annotation(&pg, *id)
            }
            EditOp::PlaceText { page, x, y, size, text, background } => {
                let pg = self.page(*page)?;
                self.place_text(&pg, &PlaceAnchor::At { x: *x, y: *y }, text, *size, *background)
            }
            EditOp::CreateField { page, rect, name, value, size } => {
                let pg = self.page(*page)?;
                let rect = crate::Rect { x0: rect.0, y0: rect.1, x1: rect.2, y1: rect.3 };
                self.create_field(&pg, rect, name, value, *size)
            }
        }
    }

    /// Replay a whole log onto this session. Spec 9.1.
    ///
    /// Stops at the first operation that is refused, with that operation's
    /// error — a half-applied replay is the silent corruption this whole format
    /// exists to avoid, so the caller learns which op failed rather than
    /// inheriting a document that is neither the original nor the edited one.
    pub fn apply_all(&mut self, ops: &[EditOp]) -> Result<()> {
        for op in ops {
            self.apply(op)?;
        }
        Ok(())
    }

    /// Refuse any operation that cannot reach this rung. Spec 11.4.
    pub fn require(&mut self, floor: Fidelity) -> &mut Self {
        self.require = floor;
        self
    }

    /// How to re-break a paragraph that no longer fits. Spec 9.3.
    pub fn reflow(&mut self, breaking: Breaking, overflow: Overflow) -> &mut Self {
        self.policy = rasura_edit::reflow::Policy { breaking, overflow };
        self
    }

    /// Which glyph run a paragraph's text is drawn by.
    ///
    /// The first glyph's run, because a font is a property of the run and a
    /// paragraph that spans two of them is one `replace_text` already declines.
    fn run_of(&self, page: &Page, id: ParagraphId) -> Option<usize> {
        page.editable
            .lines_of(id.inner())?
            .iter()
            .flat_map(|line| line.glyphs.iter())
            .next()
            .map(|g| g.run)
    }

    /// A page, as this session has left it.
    ///
    /// Re-analysed on every call rather than cached, and that is the point: a
    /// paragraph's byte spans address the content stream *now*, and a second
    /// edit built against a page analysed before the first one would splice
    /// into a layout that no longer exists. Re-walking a page tree is cheap
    /// beside the corruption the alternative produces.
    pub fn page(&self, index: usize) -> Result<Page> {
        let doc = self.inner.document();
        let pages = rasura_content::page::pages(doc)?;
        let raw = pages
            .pages
            .get(index)
            .ok_or_else(|| Error::new(Code::Malformed, format!("no page {index}")))?;
        Page::analyse(doc, raw, None)
    }

    /// Replace a character range of a paragraph. Spec 9.2.
    ///
    /// The range is in characters of [`crate::Paragraph::text`], which is what
    /// a caller can see and count. Byte offsets would be the obvious choice and
    /// the wrong one: the text a caller reads is Unicode, and an offset into
    /// its UTF-8 encoding is a number they have no way to compute.
    pub fn replace_text(
        &mut self,
        page: &Page,
        id: ParagraphId,
        range: Range<usize>,
        replacement: &str,
    ) -> Result<Outcome> {
        let (start, end) = (range.start, range.end);
        let outcome = self.replace_text_raw(page, id, range, replacement)?;
        if let Some(paragraph) = paragraph_index(page, id) {
            self.record(EditOp::ReplaceText {
                page: page.index(),
                paragraph,
                start,
                end,
                text: replacement.to_string(),
            });
        }
        Ok(outcome)
    }

    /// The text-replacement machinery, without recording an op.
    ///
    /// Separated so the recording wrapper above and the `insert`/`delete`
    /// convenience methods can each log the operation the *caller* named —
    /// `insertText`, not the `replaceText` it reduces to — rather than every
    /// one of them appearing in the op log as a replacement.
    fn replace_text_raw(
        &mut self,
        page: &Page,
        id: ParagraphId,
        range: Range<usize>,
        replacement: &str,
    ) -> Result<Outcome> {
        let attempt = |session: &Self| {
            rasura_edit::replace_text(
                session.inner.document(),
                &page.editable,
                id.inner(),
                range.clone(),
                replacement,
                session.policy,
            )
        };

        // Asked of the *font program*, before the edit is attempted, and not
        // inferred from the encoder refusing. The encoder inverts `/Encoding`,
        // so a WinAnsi font accepts every character in Latin-1 whether or not
        // its embedded subset has an outline for one — see
        // [`crate::supply::missing_glyphs`], which is the note this cost.
        let mut injected = 0usize;
        let mut unsupplied: Vec<char> = Vec::new();
        if let Some(run) = self.run_of(page, id) {
            let missing =
                crate::supply::missing_glyphs(self.inner.document(), page, run, replacement);
            if !missing.is_empty() {
                match self.supply_for(page, run, &missing) {
                    Ok(n) => injected = n,
                    // No registered font could help. Not an error: the edit can
                    // still be made, and §11.4 models a missing glyph as its own
                    // field beside the fidelity rather than as a failure. What
                    // must not happen is silence — the page would draw
                    // `.notdef` where a letter belongs and nothing would say so.
                    Err(_) => unsupplied = missing,
                }
            }
        }

        // Attempted after any injection, so the encoder sees the codes that now
        // exist and the widths that now describe them.
        let edit = attempt(self).map_err(Error::from)?;

        let mut outcome = Outcome::from_raw(&edit.fidelity);
        if injected > 0 {
            // §11.4's second rung: the document still uses one typeface and now
            // contains more of it. Reported, because a caller who required
            // `exact` must not be handed this silently.
            outcome.fidelity = Fidelity::Reembedded;
            outcome.warnings.push(format!("{injected} glyph(s) injected from a registered font"));
        }
        for c in &unsupplied {
            outcome.missing_glyphs.push(c.to_string());
        }
        self.enforce(&outcome)?;

        // A floor above the bottom rung means every character has to actually
        // draw. `requireFidelity` is the knob for "fail rather than degrade",
        // and text that renders as `.notdef` is the most degraded outcome
        // there is — it just is not one of the four rungs, because the rungs
        // describe *how* text was drawn and this is text that was not.
        if !unsupplied.is_empty() && self.require > Fidelity::Overlaid {
            return Err(Error::new(
                Code::FontUnavailable,
                format!(
                    "no glyph for {} and no registered font supplies it",
                    unsupplied.iter().collect::<String>()
                ),
            ));
        }

        self.inner.patch_content(
            "replace text",
            &page.editable.content,
            &edit.patches,
            edit.fidelity,
        )?;
        Ok(outcome)
    }

    /// Inject the characters a paragraph's font cannot write. Spec 11.3, 8.4.
    ///
    /// Staged as its own logged operation rather than folded into the text
    /// edit, so undoing the edit leaves the font enlarged. That is deliberate:
    /// the injected glyph is not part of what the caller typed, and an undo
    /// that also removed it would renumber the font under any *other* edit that
    /// had since used the new code.
    fn supply_for(&mut self, page: &Page, run: usize, wanted: &[char]) -> Result<usize> {
        let mut next_object = self.inner.document().next_object_number();
        let supplied = crate::supply::supply(
            self.inner.document(),
            page,
            run,
            wanted,
            &self.registry,
            &mut next_object,
        )?;

        let changes: Vec<(rasura_cos::ObjId, Option<rasura_cos::Object>)> =
            supplied.changes.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();
        self.inner.set_objects(
            "supply glyphs",
            &changes,
            rasura_edit::Fidelity::Degraded(vec![rasura_edit::Compromise::GlyphsInjected {
                count: supplied.characters.len(),
            }]),
        )?;
        Ok(supplied.characters.len())
    }

    /// Insert text at a character offset.
    pub fn insert_text(
        &mut self,
        page: &Page,
        id: ParagraphId,
        at: usize,
        text: &str,
    ) -> Result<Outcome> {
        let outcome = self.replace_text_raw(page, id, at..at, text)?;
        if let Some(paragraph) = paragraph_index(page, id) {
            self.record(EditOp::InsertText {
                page: page.index(),
                paragraph,
                at,
                text: text.to_string(),
            });
        }
        Ok(outcome)
    }

    /// Delete a character range.
    pub fn delete_range(
        &mut self,
        page: &Page,
        id: ParagraphId,
        range: Range<usize>,
    ) -> Result<Outcome> {
        let (start, end) = (range.start, range.end);
        let outcome = self.replace_text_raw(page, id, range, "")?;
        if let Some(paragraph) = paragraph_index(page, id) {
            self.record(EditOp::DeleteRange { page: page.index(), paragraph, start, end });
        }
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Images. Spec 9.2, 10.4.
    //
    // Every one of these wraps the drawing operator in `q … cm … Q` rather than
    // rewriting the transform that positioned it. There is no single "the `cm`"
    // to edit — a CTM is accumulated from the page matrix, enclosing `q` blocks
    // and form `/Matrix` entries — and 36% of the corpus's images are rotated,
    // so an implementation that rewrote a bounding box would flatten every one
    // of them.
    // -----------------------------------------------------------------------

    /// Move an image by a device-space offset. Spec 9.2.
    pub fn move_image(&mut self, page: &Page, id: ImageId, dx: f64, dy: f64) -> Result<Outcome> {
        let image = self.image_of(page, id)?;
        let edit =
            rasura_edit::blocks::move_image(&page.editable, image, dx, dy).map_err(block_error)?;
        let outcome = self.stage(page, "move image", edit)?;
        if let Some(image) = image_index(page, id) {
            self.record(EditOp::MoveImage { page: page.index(), image, dx, dy });
        }
        Ok(outcome)
    }

    /// Scale an image about its own origin. Spec 9.2.
    pub fn scale_image(&mut self, page: &Page, id: ImageId, sx: f64, sy: f64) -> Result<Outcome> {
        let image = self.image_of(page, id)?;
        let edit =
            rasura_edit::blocks::scale_image(&page.editable, image, sx, sy).map_err(block_error)?;
        let outcome = self.stage(page, "scale image", edit)?;
        if let Some(image) = image_index(page, id) {
            self.record(EditOp::ScaleImage { page: page.index(), image, sx, sy });
        }
        Ok(outcome)
    }

    /// Remove an image from the page. Spec 9.2.
    ///
    /// The drawing operator goes; the image object stays until a full rewrite
    /// drops it as unreferenced. That is the difference between an edit and a
    /// purge, and a caller who needs the second wants
    /// [`crate::SaveOptions::mode`] set to a full rewrite.
    pub fn delete_image(&mut self, page: &Page, id: ImageId) -> Result<Outcome> {
        let image_block = self.image_of(page, id)?;
        let edit =
            rasura_edit::blocks::delete_image(&page.editable, image_block).map_err(block_error)?;
        let outcome = self.stage(page, "delete image", edit)?;
        if let Some(image) = image_index(page, id) {
            self.record(EditOp::DeleteImage { page: page.index(), image });
        }
        Ok(outcome)
    }

    /// Replace a cell's text in a detected table. Spec 9.2.
    ///
    /// An ordinary text edit with a cell-shaped address. The five *structural*
    /// table operations are not here and will not be: they move content on a
    /// grid that was inferred, and a misdetected column edge becomes a visibly
    /// broken table.
    pub fn set_cell(
        &mut self,
        page: &Page,
        table: usize,
        row: usize,
        column: usize,
        text: &str,
    ) -> Result<Outcome> {
        let table_ref = page
            .tables()
            .get(table)
            .ok_or_else(|| Error::new(Code::Malformed, format!("no table {table} on this page")))?;
        let edit = rasura_edit::tables::set_cell(
            self.inner.document(),
            &page.editable,
            table_ref,
            row,
            column,
            text,
            self.policy,
        )
        .map_err(|e| Error::from_layer(Code::Malformed, "the cell could not be set", e))?;
        let outcome = self.stage(page, "set cell", edit)?;
        self.record(EditOp::SetCell {
            page: page.index(),
            table,
            row,
            column,
            text: text.to_string(),
        });
        Ok(outcome)
    }

    /// The image block behind a handle, refusing the ones this layer cannot
    /// address before any work is done.
    fn image_of<'p>(
        &self,
        page: &'p Page,
        id: ImageId,
    ) -> Result<&'p rasura_layout::graphics::ImageBlock> {
        page.blocks
            .get(id.0)
            .ok_or_else(|| Error::new(Code::Malformed, "no such image on this page"))
    }

    // -----------------------------------------------------------------------
    // Pages. Spec 9.2, 10.9.
    // -----------------------------------------------------------------------

    /// Remove a page, retargeting everything that pointed at it. Spec 9.2.
    ///
    /// **Refuses outright if any destination could not be retargeted.** A
    /// half-fixed document is the silent corruption §10.9 warns about, and
    /// leaving a dangling outline entry would be worse than not deleting.
    pub fn delete_page(&mut self, index: usize) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let edit = rasura_edit::pages::delete_page(self.inner.document(), &pages, index)
            .map_err(page_error)?;
        let outcome = self.stage_objects("delete page", edit)?;
        self.record(EditOp::DeletePage { index });
        Ok(outcome)
    }

    /// Move a page to a new position, with the same fix-up. Spec 9.2.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let edit = rasura_edit::pages::move_page(self.inner.document(), &pages, from, to)
            .map_err(page_error)?;
        let outcome = self.stage_objects("move page", edit)?;
        self.record(EditOp::MovePage { from, to });
        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Annotations and forms. Spec 10.7, 10.4, 10.8.
    // -----------------------------------------------------------------------

    /// Everything annotated on a page. Spec 10.7.
    pub fn annotations(&self, page: &Page) -> Result<Vec<rasura_edit::Annotation>> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let raw = pages
            .pages
            .get(page.index())
            .ok_or_else(|| Error::new(Code::Malformed, "no such page"))?;
        Ok(rasura_edit::annots::read(self.inner.document(), raw))
    }

    /// Add an annotation. Spec 10.7.
    ///
    /// Only the types whose appearance is *determined* by their own geometry —
    /// squares, circles, lines, ink, and the four quad-based markup types. A
    /// note icon or a stamp is a design decision no specification makes, and
    /// inventing one produces a document that looks like no other reader would
    /// have drawn it.
    pub fn add_annotation(
        &mut self,
        page: &Page,
        new: &rasura_edit::NewAnnotation,
    ) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let raw = pages
            .pages
            .get(page.index())
            .ok_or_else(|| Error::new(Code::Malformed, "no such page"))?
            .clone();
        let edit =
            rasura_edit::annots::create(self.inner.document(), &raw, new, &page.editable.style)
                .map_err(annot_error)?;
        let outcome = self.stage_annot("add annotation", edit)?;
        self.record(EditOp::AddAnnotation { page: page.index(), annotation: new.clone() });
        Ok(outcome)
    }

    /// Remove an annotation and unlink it from the page. Spec 10.7.
    pub fn delete_annotation(&mut self, page: &Page, id: rasura_cos::ObjId) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let raw = pages
            .pages
            .get(page.index())
            .ok_or_else(|| Error::new(Code::Malformed, "no such page"))?
            .clone();
        let edit =
            rasura_edit::annots::delete(self.inner.document(), &raw, id).map_err(annot_error)?;
        let outcome = self.stage_annot("delete annotation", edit)?;
        self.record(EditOp::DeleteAnnotation { page: page.index(), id });
        Ok(outcome)
    }

    /// Fill a form field, regenerating its appearance. Spec 10.4.
    ///
    /// Both `/V` and `/AP` are written, because neither alone is sufficient: a
    /// viewer honouring `/NeedAppearances` re-renders and is fine without a
    /// stream, and one ignoring it shows whatever `/AP` says — which, left
    /// alone, is the *old* value.
    pub fn set_field_value(&mut self, name: &str, value: &str) -> Result<Outcome> {
        let form = rasura_edit::forms::read(self.inner.document());
        let edit = rasura_edit::forms::set_text_value(
            self.inner.document(),
            &form,
            name,
            value,
            &rasura_edit::numfmt::NumberStyle::default(),
        )
        .map_err(|e| match e {
            rasura_edit::forms::FormError::Xfa => {
                Error::from_layer(Code::XfaUnsupported, "this is an XFA form", e)
            }
            other => Error::from_layer(Code::Malformed, "the field could not be set", other),
        })?;
        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.set_objects("set field value", &edit.changes, edit.fidelity)?;
        self.record(EditOp::SetFieldValue { name: name.to_string(), value: value.to_string() });
        Ok(outcome)
    }

    /// Turn widget appearances into page content and remove the fields. Spec 10.8.
    ///
    /// Draws the existing `/AP` `/N` rather than re-rendering `/V`: the
    /// appearance is what the person filling the form saw and approved, and
    /// alignment, comb spacing, a chosen radio glyph and an ink signature are
    /// none of them in `/V`.
    pub fn flatten_forms(&mut self, page: &Page) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let raw = pages
            .pages
            .get(page.index())
            .ok_or_else(|| Error::new(Code::Malformed, "no such page"))?
            .clone();
        let edit = rasura_edit::flatten::flatten_annotations(
            self.inner.document(),
            &raw,
            page.editable.content.data().len(),
            &page.editable.style,
        )
        .map_err(|e| Error::from_layer(Code::Malformed, "flattening failed", e))?;

        let changes: Vec<(rasura_cos::ObjId, Option<rasura_cos::Object>)> =
            edit.changes.iter().map(|(id, o)| (*id, o.clone())).collect();
        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        if !edit.patches.is_empty() {
            self.inner.patch_content(
                "flatten forms",
                &page.editable.content,
                &edit.patches,
                edit.fidelity.clone(),
            )?;
        }
        if !changes.is_empty() {
            self.inner.set_objects("flatten forms", &changes, edit.fidelity)?;
        }
        self.record(EditOp::FlattenForms { page: page.index() });
        Ok(outcome)
    }

    /// Draw new text onto a page in a standard face. Spec 9.2.
    ///
    /// For *adding* a value to a page that has no field for it — a flat form, a
    /// stamp, a caption. The text is Helvetica, not the document's own typeface,
    /// which is the right choice for text being added rather than a compromise:
    /// a value typed onto a form reads as typed in.
    ///
    /// `background`, when given, fills a rectangle behind the text in that RGB
    /// colour, for covering what was there before.
    pub fn place_text(
        &mut self,
        page: &Page,
        anchor: &PlaceAnchor,
        text: &str,
        size: f64,
        background: Option<(f64, f64, f64)>,
    ) -> Result<Outcome> {
        let (x, y) = match anchor {
            PlaceAnchor::At { x, y } => (*x, *y),
            PlaceAnchor::NearLabel { label, gap } => {
                let para = page
                    .paragraphs()
                    .iter()
                    .find(|p| p.text.contains(label.as_str()))
                    .ok_or_else(|| {
                        place_error(rasura_edit::place::PlaceError::LabelNotFound(label.clone()))
                    })?;
                // Just past the caption's right edge, on its baseline.
                (para.box_.x1 + gap, para.box_.y0)
            }
        };

        let raw = self.raw_page(page.index())?;
        let edit = rasura_edit::place::place_text(
            self.inner.document(),
            &raw,
            x,
            y,
            size,
            text,
            background,
            &page.editable.style,
        )
        .map_err(place_error)?;

        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.set_objects("place text", &edit.changes, edit.fidelity)?;
        // Recorded with the resolved origin, which is exact against this
        // document — a `near_label` anchor is a convenience for finding it, not
        // a coordinate worth replaying a second label search for.
        self.record(EditOp::PlaceText {
            page: page.index(),
            x,
            y,
            size,
            text: text.to_string(),
            background,
        });
        Ok(outcome)
    }

    /// Create a text field over a rectangle. Spec 10.4.
    ///
    /// The missing primitive for a "prepare this flat PDF to be signed" flow:
    /// where a form has no field, this makes one — a merged widget that is both
    /// the AcroForm field and the page annotation, named so
    /// [`set_field_value`](Self::set_field_value) can fill it afterwards.
    pub fn create_field(
        &mut self,
        page: &Page,
        rect: crate::Rect,
        name: &str,
        value: &str,
        size: f64,
    ) -> Result<Outcome> {
        let raw = self.raw_page(page.index())?;
        let edit =
            rasura_edit::place::create_text_field(self.inner.document(), &raw, rect, name, value, size)
                .map_err(place_error)?;

        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.set_objects("create field", &edit.changes, edit.fidelity)?;
        self.record(EditOp::CreateField {
            page: page.index(),
            rect: (rect.x0, rect.y0, rect.x1, rect.y1),
            name: name.to_string(),
            value: value.to_string(),
            size,
        });
        Ok(outcome)
    }

    /// The raw page dictionary behind an analysed page, for the object-level
    /// operations that build new content against it.
    fn raw_page(&self, index: usize) -> Result<rasura_content::page::Page> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        Ok(pages
            .pages
            .get(index)
            .ok_or_else(|| Error::new(Code::Malformed, format!("no page {index}")))?
            .clone())
    }

    /// Stage an operation that writes whole objects.
    fn stage_objects(
        &mut self,
        label: &str,
        edit: rasura_edit::pages::PageEdit,
    ) -> Result<Outcome> {
        let changes: Vec<(rasura_cos::ObjId, Option<rasura_cos::Object>)> =
            edit.changes.iter().map(|(id, o)| (*id, o.clone())).collect();
        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.set_objects(label, &changes, edit.fidelity)?;
        Ok(outcome)
    }

    fn stage_annot(&mut self, label: &str, edit: rasura_edit::AnnotEdit) -> Result<Outcome> {
        let changes: Vec<(rasura_cos::ObjId, Option<rasura_cos::Object>)> =
            edit.changes.iter().map(|(id, o)| (*id, o.clone())).collect();
        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.set_objects(label, &changes, edit.fidelity)?;
        Ok(outcome)
    }

    /// Stage a content-stream edit and report what it cost.
    fn stage(&mut self, page: &Page, label: &str, edit: rasura_edit::Edit) -> Result<Outcome> {
        let outcome = Outcome::from_raw(&edit.fidelity);
        self.enforce(&outcome)?;
        self.inner.patch_content(label, &page.editable.content, &edit.patches, edit.fidelity)?;
        Ok(outcome)
    }

    /// Undo the last operation, restoring the exact prior bytes. Invariant I5.
    ///
    /// Returns whether there was anything to undo.
    pub fn undo(&mut self) -> Result<bool> {
        let undone = self.inner.undo()?;
        // The forward log mirrors the undo in step: one operation comes off and
        // waits on the redo half. An operation that enlarged the font to supply
        // a glyph is a separate entry on the *inner* log but not on this one, so
        // an undo that unwinds only that internal step finds nothing here to
        // move — which is correct, since the caller issued no such op.
        if undone {
            if let Some(op) = self.ops.pop() {
                self.ops_redo.push(op);
            }
        }
        Ok(undone)
    }

    pub fn redo(&mut self) -> Result<bool> {
        let redone = self.inner.redo()?;
        if redone {
            if let Some(op) = self.ops_redo.pop() {
                self.ops.push(op);
            }
        }
        Ok(redone)
    }

    /// Abandon every staged operation. The document is left as it was found.
    pub fn rollback(&mut self) -> Result<()> {
        self.inner.rollback()?;
        self.ops.clear();
        self.ops_redo.clear();
        Ok(())
    }

    /// How many operations are staged.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The worst rung any staged operation reached.
    pub fn fidelity(&self) -> Fidelity {
        Fidelity::from_raw(&self.inner.fidelity())
    }

    /// Apply everything and write the document out. Spec 9.5.
    pub fn commit(&mut self, opts: &crate::SaveOptions) -> Result<crate::Saved> {
        let cos = rasura_cos::SaveOptions {
            mode: opts.mode,
            accept_signature_destruction: opts.accept_signature_destruction,
        };
        let out = self.inner.commit(&cos)?;
        Ok(crate::Saved {
            bytes: out.bytes,
            mode: out.mode,
            bytes_appended: out.bytes_appended,
            warnings: out.warnings,
        })
    }

    /// Fail rather than degrade, when the caller asked for that.
    ///
    /// Checked *before* the patch is staged, so a refused operation leaves the
    /// session untouched rather than needing an undo — which matters because a
    /// caller who set a floor is telling us they would rather have nothing.
    fn enforce(&self, outcome: &Outcome) -> Result<()> {
        if outcome.fidelity >= self.require {
            return Ok(());
        }
        Err(Error::new(
            Code::FidelityBelowRequired,
            format!(
                "the operation reached {} and {} was required",
                outcome.fidelity.as_str(),
                self.require.as_str()
            ),
        ))
    }
}

/// Resolve a flat paragraph index on a page to its id, refusing an index that
/// is not there rather than addressing whatever sits beyond the end. The guard
/// a replayed op passes through, identical to the one the live wasm surface uses.
fn paragraph_at(page: &Page, index: usize) -> Result<ParagraphId> {
    page.paragraphs()
        .get(index)
        .map(|p| p.id)
        .ok_or_else(|| Error::new(Code::Malformed, format!("no paragraph {index} on this page")))
}

fn image_at(page: &Page, index: usize) -> Result<ImageId> {
    page.images()
        .get(index)
        .map(|i| i.id)
        .ok_or_else(|| Error::new(Code::Malformed, format!("no image {index} on this page")))
}

/// The flat index of a paragraph id within a page, so an op is recorded in the
/// same coordinate a caller addressed it by. `None` only if the id is not from
/// this page, which a caller cannot arrange through the public API.
fn paragraph_index(page: &Page, id: ParagraphId) -> Option<usize> {
    page.paragraphs().iter().position(|p| p.id == id)
}

fn image_index(page: &Page, id: ImageId) -> Option<usize> {
    page.images().iter().position(|i| i.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Document, SaveOptions};
    use rasura_cos::testutil::ClassicBuilder;

    fn page_doc() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    #[test]
    fn an_edit_round_trips_through_the_facade() {
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        let outcome = session.replace_text(&page, id, 0..5, "Howdy").expect("replace");
        assert_eq!(outcome.fidelity, Fidelity::Exact);
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        assert_eq!(after.page(0).expect("page").paragraphs()[0].text, "Howdy world");
    }

    #[test]
    fn undo_restores_the_document_exactly() {
        // Invariant I5, at the surface a caller touches.
        let bytes = page_doc();
        let mut doc = Document::open(bytes.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        session.replace_text(&page, id, 0..5, "Howdy").expect("replace");
        assert_eq!(session.len(), 1);
        assert!(session.undo().expect("undo"), "there was something to undo");
        assert!(session.is_empty());
        let saved = session.commit(&SaveOptions::default()).expect("commit");
        assert_eq!(saved.bytes, bytes, "the file is byte-identical again");
    }

    #[test]
    fn a_required_floor_refuses_rather_than_degrading() {
        // Spec 11.4's knob. A redlining tool would rather fail loudly than hand
        // a lawyer a clause set in a font the original never used.
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        session.require(Fidelity::Exact);
        // Helvetica has no CJK and nothing can inject into a non-embedded font,
        // so this cannot reach any rung at all.
        let err = session
            .replace_text(&page, id, 0..5, "\u{4e00}\u{4e8c}")
            .expect_err("no glyphs for this");
        assert_eq!(err.code(), Code::FontUnavailable);
        assert!(session.is_empty(), "a refused operation staged nothing");
    }

    #[test]
    fn the_default_floor_lets_an_ordinary_edit_through() {
        // A caller who never thought about fidelity must not be blocked by it.
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        assert!(session.replace_text(&page, id, 0..5, "Howdy").is_ok());
    }

    /// A document whose embedded font is a subset of Roboto without `É`.
    ///
    /// Built from the real typeface rather than a synthetic one, because the
    /// thing under test is whether a glyph taken out of a font a third party
    /// made can be put into a subset of that same font and drawn.
    fn subset_document() -> Option<(Vec<u8>, Vec<u8>)> {
        let roboto = std::fs::read("../../corpus/fonts/Roboto-Regular.ttf").ok()?;
        let font = rasura_font::Sfnt::parse(&roboto).ok()?;
        let cmap = rasura_font::Cmap::parse(&roboto, &font)?;
        let table = cmap.best_unicode()?;

        // The producer's subset: only the letters "Hi" uses.
        let present = "Hi";
        let gids: Vec<u16> =
            present.chars().filter_map(|c| table.lookup(&roboto, c as u32)).collect();
        let subset = rasura_font::compact_truetype(&roboto, &gids).ok()?;
        let subset_font = rasura_font::Sfnt::parse(&subset.bytes).ok()?;
        let mapped: Vec<(u32, u16)> =
            present.chars().zip(&gids).map(|(c, g)| (c as u32, subset.mapping[g])).collect();
        let program = rasura_font::add_mappings(&subset.bytes, &subset_font, &mapped).ok()?;

        let per_em = font.units_per_em.max(1) as f64;
        let width = |c: char| {
            let gid = table.lookup(&roboto, c as u32).unwrap_or(0);
            (font.advance(&roboto, gid).unwrap_or(0) as f64 * 1000.0 / per_em).round() as i64
        };

        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 24 Tf 1 0 0 1 72 700 Tm (Hi) Tj ET\n")
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Roboto-Regular \
                     /FirstChar 72 /LastChar 105 /Widths [{} {} {}] \
                     /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
                    width('H'),
                    (73..105).map(|_| "0").collect::<Vec<_>>().join(" "),
                    width('i'),
                ),
            )
            .object(
                6,
                "<< /Type /FontDescriptor /FontName /ABCDEF+Roboto-Regular /Flags 32 \
                 /FontBBox [-737 -271 1148 1056] /ItalicAngle 0 /Ascent 928 /Descent -244 \
                 /CapHeight 711 /StemV 80 /FontFile2 7 0 R >>",
            )
            .stream(7, &format!(" /Length1 {}", program.len()), &program)
            .finish("/Root 1 0 R");

        Some((bytes, roboto))
    }

    #[test]
    fn a_registered_font_supplies_a_glyph_the_subset_lost() {
        // Spec 11.3 and 8.4 together, which is the pair that makes editing a
        // real document possible: the producer embedded seven letters, the user
        // types an eighth, and the outline comes out of a font the caller
        // supplied and goes *into* the document's own.
        let Some((bytes, roboto)) = subset_document() else {
            eprintln!("skipping: corpus/fonts/Roboto-Regular.ttf absent");
            return;
        };

        let mut doc = Document::open(bytes).expect("open");
        // With nothing registered the edit still happens — §11.4 puts a missing
        // glyph in its own field rather than treating it as a failure — but it
        // is *reported*, which is the part that was silently absent before.
        {
            let page = doc.page(0).expect("page");
            let id = page.paragraphs()[0].id;
            let mut session = doc.edit();
            let outcome = session.replace_text(&page, id, 0..1, "É").expect("still edits");
            assert_eq!(
                outcome.missing_glyphs,
                vec!["É".to_string()],
                "the subset has no É and nothing could supply one"
            );
        }

        doc.register_font(
            roboto,
            &crate::RegisterOptions { match_for: Some("Roboto-Regular".into()) },
        );
        assert_eq!(doc.registered_fonts(), 1);

        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        let outcome = session.replace_text(&page, id, 0..1, "É").expect("supplied and replaced");

        // Spec 11.4's second rung, reported rather than passed off as exact.
        assert_eq!(outcome.fidelity, Fidelity::Reembedded, "{outcome:?}");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        let text = after.page(0).expect("page").paragraphs()[0].text.clone();
        assert!(text.starts_with('É'), "the injected character reads back: {text:?}");
    }

    #[test]
    fn requiring_exact_refuses_an_injection() {
        // The rung is below `exact`, so a caller who set that floor must be
        // refused — this is the case the knob exists for, and the one where
        // silently accepting would be worst.
        let Some((bytes, roboto)) = subset_document() else {
            eprintln!("skipping: corpus/fonts/Roboto-Regular.ttf absent");
            return;
        };
        let mut doc = Document::open(bytes).expect("open");
        doc.register_font(roboto, &crate::RegisterOptions::default());

        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        session.require(Fidelity::Exact);
        let err = session.replace_text(&page, id, 0..1, "É").expect_err("below the floor");
        assert_eq!(err.code(), Code::FidelityBelowRequired);
    }

    /// A two-page document with an image on page one and a form field.
    fn richer() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [9 0 R] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> \
                 /Annots [9 0 R] >>",
            )
            .stream(
                4,
                "",
                b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj ET\n\
                  q 200 0 0 100 100 400 cm /Im1 Do Q\n",
            )
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .stream(
                6,
                " /Type /XObject /Subtype /Image /Width 2 /Height 2 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8, 64, 128, 255],
            )
            .object(7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>")
            .stream(8, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Second page) Tj ET\n")
            .object(
                9,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (before) \
                 /Rect [72 600 300 620] /DA (/Helv 10 Tf 0 g) /P 3 0 R >>",
            )
            .finish("/Root 1 0 R")
    }

    #[test]
    fn an_image_moves_through_the_facade() {
        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page");
        let images = page.images();
        assert_eq!(images.len(), 1, "{images:?}");
        assert!(images[0].editable, "not inside a form, so addressable");
        let before = images[0].box_;

        let id = images[0].id;
        let mut session = doc.edit();
        session.move_image(&page, id, 50.0, -30.0).expect("move");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        let moved = after.page(0).expect("page").images()[0].box_;
        assert!((moved.x0 - before.x0 - 50.0).abs() < 0.5, "{before:?} -> {moved:?}");
    }

    #[test]
    fn deleting_an_image_removes_it_and_leaves_the_text() {
        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.images()[0].id;

        let mut session = doc.edit();
        session.delete_image(&page, id).expect("delete");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        let page = after.page(0).expect("page");
        assert!(page.images().is_empty(), "{:?}", page.images());
        assert!(page.paragraphs()[0].text.contains("Hello"), "the text survived");
    }

    #[test]
    fn a_page_can_be_deleted_and_the_rest_survives() {
        let mut doc = Document::open(richer()).expect("open");
        assert_eq!(doc.page_count(), 2);

        let mut session = doc.edit();
        session.delete_page(1).expect("delete page");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        assert_eq!(after.page_count(), 1);
        assert!(after.page(0).expect("page").paragraphs()[0].text.contains("Hello"));
    }

    #[test]
    fn a_form_field_can_be_filled_through_the_facade() {
        let mut doc = Document::open(richer()).expect("open");
        assert_eq!(doc.form_fields().len(), 1);
        assert_eq!(doc.form_fields()[0].name, "name");

        let mut session = doc.edit();
        session.set_field_value("name", "after").expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        assert_eq!(after.form_fields()[0].value.as_deref(), Some("after"));
    }

    #[test]
    fn an_annotation_can_be_added_and_removed() {
        use rasura_edit::{Kind, NewAnnotation};

        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page");
        let before = doc.edit().annotations(&page).expect("read").len();

        let mut session = doc.edit();
        session
            .add_annotation(
                &page,
                &NewAnnotation::new(
                    Kind::Square,
                    crate::Rect { x0: 100.0, y0: 100.0, x1: 200.0, y1: 160.0 },
                ),
            )
            .expect("add");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        let mut after = after;
        let page = after.page(0).expect("page");
        let annots = after.edit().annotations(&page).expect("read");
        assert_eq!(annots.len(), before + 1, "{annots:#?}");
        assert!(annots.iter().any(|a| a.kind == Some(Kind::Square)));
    }

    #[test]
    fn image_and_text_edits_share_one_session_and_one_undo() {
        // The point of putting them all on the same session: a caller changing
        // a caption and nudging the picture it describes commits both or
        // neither.
        let bytes = richer();
        let mut doc = Document::open(bytes.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let para = page.paragraphs()[0].id;
        let image = page.images()[0].id;

        let mut session = doc.edit();
        session.replace_text(&page, para, 0..5, "HELLO").expect("text");
        session.move_image(&page, image, 10.0, 10.0).expect("image");
        assert_eq!(session.len(), 2);

        assert!(session.undo().expect("undo"));
        assert!(session.undo().expect("undo"));
        let saved = session.commit(&SaveOptions::default()).expect("commit");
        assert_eq!(saved.bytes, bytes, "both undone, byte-identical");
    }

    #[test]
    fn the_rungs_are_ordered_so_a_floor_is_a_comparison() {
        assert!(Fidelity::Exact > Fidelity::Reembedded);
        assert!(Fidelity::Reembedded > Fidelity::Substituted);
        assert!(Fidelity::Substituted > Fidelity::Overlaid);
    }

    #[test]
    fn the_op_log_records_the_operation_a_caller_named() {
        // `insertText` reduces to `replaceText` internally, but the forward log
        // records what the *caller* asked for — anything else would make the
        // serialised history a worse description of events than the code that
        // produced it.
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        session.insert_text(&page, id, 0, "X").expect("insert");

        match session.ops() {
            [EditOp::InsertText { page, paragraph, at, text }] => {
                assert_eq!((*page, *paragraph, *at), (0, 0, 0));
                assert_eq!(text, "X");
            }
            other => panic!("expected one InsertText op, got {other:?}"),
        }
        assert_eq!(session.op_log().version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_refused_operation_is_not_in_the_op_log() {
        // The log must describe the document that exists. An operation refused
        // for being below the fidelity floor changed nothing, so an entry a
        // replay would re-attempt — and re-refuse — does not belong on it.
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        session.require(Fidelity::Exact);
        session.replace_text(&page, id, 0..5, "\u{4e00}").expect_err("no glyph for this");
        assert!(session.ops().is_empty(), "a refused op logs nothing");
    }

    #[test]
    fn undo_and_redo_move_operations_on_and_off_the_log() {
        let mut doc = Document::open(page_doc()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;

        let mut session = doc.edit();
        session.replace_text(&page, id, 0..5, "Howdy").expect("replace");
        assert_eq!(session.ops().len(), 1);

        session.undo().expect("undo");
        assert!(session.ops().is_empty(), "undo took the op off the forward log");

        session.redo().expect("redo");
        assert_eq!(session.ops().len(), 1, "redo put it back");
    }

    #[test]
    fn a_captured_log_replays_to_byte_identical_output() {
        // The property the whole format exists for: the original bytes plus the
        // op log reconstruct the edited document exactly, so an editor can store
        // the edit as data instead of as a second copy of the file.
        let bytes = richer();

        let (log, committed) = {
            let mut doc = Document::open(bytes.clone()).expect("open");
            let mut session = doc.edit();
            // Re-analyse the page before each op, as a real caller (and the wasm
            // surface) does: a paragraph's spans address the stream as the last
            // op left it.
            {
                let page = session.page(0).expect("page");
                let para = page.paragraphs()[0].id;
                session.replace_text(&page, para, 0..5, "HELLO").expect("replace");
            }
            {
                let page = session.page(0).expect("page");
                let image = page.images()[0].id;
                session.move_image(&page, image, 10.0, -10.0).expect("move");
            }
            let log = session.ops().to_vec();
            let committed = session.commit(&SaveOptions::default()).expect("commit").bytes;
            (log, committed)
        };
        assert_eq!(log.len(), 2);

        let mut doc = Document::open(bytes).expect("reopen");
        let replayed = {
            let mut session = doc.edit();
            session.apply_all(&log).expect("replay");
            assert_eq!(session.ops().len(), 2, "replaying a log reproduces that log");
            session.commit(&SaveOptions::default()).expect("commit").bytes
        };

        assert_eq!(replayed, committed, "the ops reconstruct the edited bytes");
    }

    #[test]
    fn replay_refuses_a_coordinate_that_does_not_resolve() {
        // Replay is exact against the document the ops were captured on. A
        // coordinate that lands nowhere is refused with a code, never applied to
        // whatever else sits at that index — the silent corruption the version
        // stamp and this guard exist together to prevent.
        let mut doc = Document::open(page_doc()).expect("open");
        let mut session = doc.edit();
        let err = session
            .apply(&EditOp::ReplaceText { page: 0, paragraph: 99, start: 0, end: 1, text: "x".into() })
            .expect_err("no such paragraph");
        assert_eq!(err.code(), Code::Malformed);
        assert!(session.ops().is_empty(), "a refused replay logged nothing");
    }

    #[test]
    fn a_new_field_is_created_and_can_be_filled() {
        // The "prepare a flat PDF to be signed" path: a page with no field for a
        // value gets one, named, and the value goes in afterwards.
        let mut doc = Document::open(richer()).expect("open");
        let before = doc.form_fields().len();

        let page = doc.page(1).expect("page"); // the second page carries no field
        let mut session = doc.edit();
        session
            .create_field(
                &page,
                crate::Rect { x0: 72.0, y0: 500.0, x1: 300.0, y1: 520.0 },
                "signature",
                "",
                10.0,
            )
            .expect("create field");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let mut after = Document::open(saved.bytes).expect("reopen");
        let names: Vec<String> = after.form_fields().iter().map(|f| f.name.clone()).collect();
        assert!(names.contains(&"signature".to_string()), "{names:?}");
        assert_eq!(after.form_fields().len(), before + 1);

        // And the created field fills like any other.
        let page = after.page(1).expect("page");
        let mut session = after.edit();
        drop(page);
        session.set_field_value("signature", "A. Ozdamar").expect("fill");
    }

    #[test]
    fn placed_text_appears_on_the_page() {
        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page");
        let mut session = doc.edit();
        session
            .place_text(&page, &PlaceAnchor::At { x: 72.0, y: 500.0 }, "Placed here", 12.0, None)
            .expect("place");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        let page = after.page(0).expect("page");
        assert!(
            page.paragraphs().iter().any(|p| p.text.contains("Placed")),
            "placed text not found in {:?}",
            page.paragraphs().iter().map(|p| &p.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn text_is_placed_after_a_label() {
        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page"); // its paragraph reads "Hello world"
        let mut session = doc.edit();
        session
            .place_text(
                &page,
                &PlaceAnchor::NearLabel { label: "Hello".into(), gap: 6.0 },
                "Jane Doe",
                12.0,
                None,
            )
            .expect("place near label");
        let saved = session.commit(&SaveOptions::default()).expect("commit");

        let after = Document::open(saved.bytes).expect("reopen");
        assert!(
            after.page(0).expect("page").paragraphs().iter().any(|p| p.text.contains("Jane")),
            "label-anchored text not found"
        );
    }

    #[test]
    fn a_missing_label_is_a_coded_refusal() {
        let mut doc = Document::open(richer()).expect("open");
        let page = doc.page(0).expect("page");
        let mut session = doc.edit();
        let err = session
            .place_text(
                &page,
                &PlaceAnchor::NearLabel { label: "nowhere on this page".into(), gap: 6.0 },
                "x",
                12.0,
                None,
            )
            .expect_err("no such label");
        assert_eq!(err.code(), Code::Malformed);
        assert!(session.is_empty(), "a refused placement staged nothing");
    }

    #[test]
    fn placing_text_undoes_to_byte_identical() {
        // Invariant I5 for the object-level add path, as for every other edit.
        let original = richer();
        let mut doc = Document::open(original.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let mut session = doc.edit();
        session
            .place_text(&page, &PlaceAnchor::At { x: 72.0, y: 500.0 }, "X", 12.0, Some((1.0, 1.0, 1.0)))
            .expect("place");
        assert_eq!(session.ops().len(), 1);
        assert!(session.undo().expect("undo"));
        let saved = session.commit(&SaveOptions::default()).expect("commit");
        assert_eq!(saved.bytes, original, "undo restored the exact prior bytes");
    }

    #[test]
    fn rolling_back_leaves_the_document_untouched() {
        let bytes = page_doc();
        let mut doc = Document::open(bytes.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        {
            let mut session = doc.edit();
            session.replace_text(&page, id, 0..5, "Howdy").expect("replace");
            session.rollback().expect("rollback");
        }
        let saved = doc.save(&SaveOptions::default()).expect("save");
        assert_eq!(saved.bytes, bytes);
    }
}
