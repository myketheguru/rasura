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
        }
    }

    /// Park the session and release the document.
    pub fn suspend(self) -> SessionState {
        SessionState { inner: self.inner.suspend(), require: self.require, policy: self.policy }
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
        self.replace_text(page, id, at..at, text)
    }

    /// Delete a character range.
    pub fn delete_range(
        &mut self,
        page: &Page,
        id: ParagraphId,
        range: Range<usize>,
    ) -> Result<Outcome> {
        self.replace_text(page, id, range, "")
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
        self.stage(page, "move image", edit)
    }

    /// Scale an image about its own origin. Spec 9.2.
    pub fn scale_image(&mut self, page: &Page, id: ImageId, sx: f64, sy: f64) -> Result<Outcome> {
        let image = self.image_of(page, id)?;
        let edit =
            rasura_edit::blocks::scale_image(&page.editable, image, sx, sy).map_err(block_error)?;
        self.stage(page, "scale image", edit)
    }

    /// Remove an image from the page. Spec 9.2.
    ///
    /// The drawing operator goes; the image object stays until a full rewrite
    /// drops it as unreferenced. That is the difference between an edit and a
    /// purge, and a caller who needs the second wants
    /// [`crate::SaveOptions::mode`] set to a full rewrite.
    pub fn delete_image(&mut self, page: &Page, id: ImageId) -> Result<Outcome> {
        let image = self.image_of(page, id)?;
        let edit = rasura_edit::blocks::delete_image(&page.editable, image).map_err(block_error)?;
        self.stage(page, "delete image", edit)
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
        let table = page
            .tables()
            .get(table)
            .ok_or_else(|| Error::new(Code::Malformed, format!("no table {table} on this page")))?;
        let edit = rasura_edit::tables::set_cell(
            self.inner.document(),
            &page.editable,
            table,
            row,
            column,
            text,
            self.policy,
        )
        .map_err(|e| Error::from_layer(Code::Malformed, "the cell could not be set", e))?;
        self.stage(page, "set cell", edit)
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
        self.stage_objects("delete page", edit)
    }

    /// Move a page to a new position, with the same fix-up. Spec 9.2.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<Outcome> {
        let pages = rasura_content::page::pages(self.inner.document())?;
        let edit = rasura_edit::pages::move_page(self.inner.document(), &pages, from, to)
            .map_err(page_error)?;
        self.stage_objects("move page", edit)
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
        self.stage_annot("add annotation", edit)
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
        self.stage_annot("delete annotation", edit)
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
        Ok(outcome)
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
        Ok(self.inner.undo()?)
    }

    pub fn redo(&mut self) -> Result<bool> {
        Ok(self.inner.redo()?)
    }

    /// Abandon every staged operation. The document is left as it was found.
    pub fn rollback(&mut self) -> Result<()> {
        Ok(self.inner.rollback()?)
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
