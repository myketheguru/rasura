//! The transaction model. Spec 9.1.
//!
//! > All mutation goes through an `EditSession`. Operations are accumulated,
//! > each returning a report; nothing touches the document until `commit()`.
//! >
//! > - Operations are recorded in an op log with inverses, giving undo/redo for
//! >   free.
//! > - `commit()` is atomic: either all patches apply or none do.
//! > - A session holds a snapshot version; concurrent sessions on the same
//! >   document conflict and the second `commit()` fails with
//! >   `EditError::StaleSession`.
//!
//! # Why the inverse is a byte image and not an instruction
//!
//! "Record the inverse operation" is the textbook design: undo an insertion by
//! deleting, undo a delete by re-inserting. It is also how undo goes subtly
//! wrong, because the inverse is only exact if replaying it reproduces every
//! incidental byte the original operation touched — the `/Length`, the
//! compression level, the operand spacing, the number formatting. Invariant I5
//! is not "undo restores the text":
//!
//! > **I5 — Undo exactness.** Any operation followed by `undo()` restores the
//! > exact prior byte state.
//!
//! So the inverse recorded here is the object's prior value, and undo puts it
//! back. That costs memory proportional to what was edited, which is the right
//! trade for a guarantee that cannot drift. Where a document is large and the
//! edit small, the thing retained is small too: content streams are stored
//! decoded and only for objects an operation actually wrote.

use crate::patch::Patch;
use crate::stream::{self, StreamError};
use rasura_content::content::LogicalContent;
use rasura_cos::object::Object;
use rasura_cos::{Document, ObjId, SaveOptions, SaveResult};
use std::collections::BTreeMap;

/// How exactly an operation achieved what was asked.
///
/// Spec §2's second property: "Fidelity is reported, never assumed. When the
/// engine cannot make an exact edit, it says so in a typed result."
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fidelity {
    /// Every glyph is in the font, positioned as the producer would have.
    Exact,
    /// The result is correct but some incidental property was not reproduced.
    Degraded(Vec<Compromise>),
}

impl Fidelity {
    pub fn is_exact(&self) -> bool {
        matches!(self, Fidelity::Exact)
    }

    /// Combine two fidelities, keeping every compromise from both.
    pub fn and(self, other: Fidelity) -> Fidelity {
        match (self, other) {
            (Fidelity::Exact, f) | (f, Fidelity::Exact) => f,
            (Fidelity::Degraded(mut a), Fidelity::Degraded(b)) => {
                a.extend(b);
                a.sort();
                a.dedup();
                Fidelity::Degraded(a)
            }
        }
    }
}

/// A specific thing the engine could not reproduce exactly.
///
/// Each variant names something a *user* would notice or care about, not an
/// internal detail. A list of these is what a caller shows when it asks "accept
/// these changes?", so a variant nobody can act on does not belong here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Compromise {
    /// A glyph was injected into the embedded font.
    ///
    /// Not a defect — it is the feature — but it grows the file and changes a
    /// font program, so it is reported.
    GlyphsInjected { count: usize },
    /// A character has no glyph in the font and none could be injected.
    GlyphUnavailable { text: String },
    /// The producer's kerning could not be preserved and was regenerated from
    /// the font.
    KerningRegenerated,
    /// The paragraph re-broke at different points than the original.
    LinesRebroken { before: usize, after: usize },
    /// The text no longer fits its block.
    Overflowed { lines_over: usize },
    /// Destinations that named a deleted page were pointed somewhere else.
    ///
    /// The alternative was dropping them, which silently loses an outline entry
    /// the user can see in the sidebar. Retargeting keeps the entry and changes
    /// where it goes, so it is reported rather than assumed acceptable.
    DestinationsRetargeted { count: usize },
    /// A replacement image was stretched to fit a differently-shaped space.
    ImageDistorted,
    /// `/PageLabels` no longer describes the page order.
    ///
    /// The number tree is keyed by page index, so any reorder invalidates it.
    /// Renumbering is not attempted; saying so is better than a tree that looks
    /// authoritative and is wrong.
    PageLabelsStale,
    /// Redacted glyphs remain in the embedded font subset.
    ///
    /// Spec 10.6 step 6. A subset's glyph inventory leaks the alphabet a
    /// document used: removing a name from a page whose subset still holds its
    /// letters tells a reader those letters appeared. Reported on every
    /// redaction until the subset is rebuilt, so a caller is never told a
    /// redaction was exact when this is outstanding.
    FontSubsetRetained,
    /// Some annotations could not be flattened and stayed interactive.
    ///
    /// Almost always because they carry no appearance stream. Inventing one is
    /// a different and much less safe operation than preserving one.
    AnnotationsLeftInteractive { count: usize },
    /// The edit landed in an optional-content layer that is turned off.
    ///
    /// Spec 10.2. The edit is real and the bytes changed; the page does not,
    /// because no viewer draws that layer in the default configuration. Without
    /// this a caller sees a successful, exact result and a document that looks
    /// exactly as it did — and concludes the library is broken.
    ///
    /// Not an error: turning the layer on is the user's to do, and a CAD
    /// drawing's dimension text is worth editing whether or not it is showing.
    EditedHiddenLayer { layer: String },
    /// Embedded fonts were pruned to the glyphs the document draws. Spec 8.6.
    ///
    /// The point of the operation and a real loss at once: a glyph the document
    /// does not currently use is gone, so a later edit that needs one has to
    /// inject it again. Reported because "never default to it" and "say what it
    /// cost" are the same instruction.
    FontSubsetCompacted { fonts: usize, bytes_saved: usize },
}

/// Why an edit could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The document changed under the session.
    ///
    /// Spec 9.1. A session records the document's dirty state when it opens;
    /// if something else has written since, the spans this session holds no
    /// longer address the bytes it measured them against.
    #[error("the document was modified since this session began")]
    StaleSession,

    /// The session has already been committed or rolled back.
    #[error("this session is closed")]
    Closed,

    #[error(transparent)]
    Stream(#[from] StreamError),

    #[error("{0}")]
    Cos(String),
}

/// One entry in the op log.
#[derive(Debug, Clone)]
struct LoggedOp {
    /// What the caller asked for, for reporting and for redo.
    label: String,
    /// Every object this operation wrote, with its value beforehand.
    ///
    /// `None` means the object did not exist, so undoing means deleting it.
    prior: BTreeMap<ObjId, Option<Object>>,
    fidelity: Fidelity,
}

/// What an operation did.
#[derive(Debug, Clone)]
pub struct EditReport {
    pub fidelity: Fidelity,
    /// Objects the operation wrote.
    pub objects_touched: Vec<ObjId>,
    /// Net change in decoded content bytes, summed over touched streams.
    pub bytes_delta: isize,
}

/// A session's memory, with no document attached.
///
/// Everything an [`EditSession`] knows apart from the document it is editing:
/// the op log, the redo stack, and whether the document was already dirty. All
/// of it owned, none of it borrowed — which is the point.
///
/// # Why this is a separate type
///
/// `EditSession` holds `&mut Document`, and a borrow cannot outlive the call
/// that created it. That is exactly right for a caller who has the document in
/// hand, and impossible for one who does not: a WASM handle table, an FFI
/// boundary, or a server holding documents in a map cannot store a struct that
/// borrows another entry in the same map, and Rust has no way to express it
/// without `unsafe` or a self-referential-struct crate.
///
/// So the state is separable. [`EditSession::suspend`] takes it out,
/// [`EditSession::resume`] puts it back, and in between the caller holds a
/// plain value they can park anywhere. The log survives verbatim, so undo stays
/// byte-exact across the gap — invariant I5 does not know a suspension
/// happened.
#[derive(Debug, Clone, Default)]
pub struct SessionState {
    log: Vec<LoggedOp>,
    redo: Vec<LoggedOp>,
    opened_dirty: bool,
    closed: bool,
}

impl SessionState {
    /// How many operations are on the log.
    pub fn len(&self) -> usize {
        self.log.len()
    }

    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }

    /// Whether a commit or rollback has already closed this session.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// How many operations can be redone.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// The combined fidelity of every operation on the log.
    pub fn fidelity(&self) -> Fidelity {
        self.log.iter().fold(Fidelity::Exact, |acc, op| acc.and(op.fidelity.clone()))
    }
}

/// An accumulating set of edits over one document. Spec 9.1.
pub struct EditSession<'a> {
    doc: &'a mut Document,
    log: Vec<LoggedOp>,
    /// Operations undone but not yet superseded, newest last.
    redo: Vec<LoggedOp>,
    /// Whether the document was already dirty when this session opened.
    ///
    /// A session cannot claim to restore "the prior byte state" if someone else
    /// had staged changes it never saw.
    opened_dirty: bool,
    closed: bool,
}

impl<'a> EditSession<'a> {
    /// Begin a session over `doc`.
    pub fn new(doc: &'a mut Document) -> EditSession<'a> {
        let opened_dirty = doc.is_dirty();
        EditSession { doc, log: Vec::new(), redo: Vec::new(), opened_dirty, closed: false }
    }

    /// Reattach parked state to a document. Spec 9.1.
    ///
    /// The document must be the one the state was suspended from. Nothing here
    /// can check that — an object id means nothing outside the document that
    /// issued it — so resuming against a *different* document would undo into
    /// objects that never had those values. The caller owns that pairing, which
    /// is why the WASM layer keeps the two in one slot rather than two maps.
    pub fn resume(doc: &'a mut Document, state: SessionState) -> EditSession<'a> {
        EditSession {
            doc,
            log: state.log,
            redo: state.redo,
            opened_dirty: state.opened_dirty,
            closed: state.closed,
        }
    }

    /// Park the session's memory and release the document.
    pub fn suspend(self) -> SessionState {
        SessionState {
            log: self.log,
            redo: self.redo,
            opened_dirty: self.opened_dirty,
            closed: self.closed,
        }
    }

    /// The document, for reading. Callers extract pages and measure text
    /// through this; nothing here hands out a mutable borrow.
    pub fn document(&self) -> &Document {
        self.doc
    }

    /// How many operations are on the log.
    pub fn len(&self) -> usize {
        self.log.len()
    }

    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }

    /// Whether this session was opened over a document someone else had
    /// already modified.
    pub fn opened_dirty(&self) -> bool {
        self.opened_dirty
    }

    /// Apply patches to a page's content streams as one logged operation.
    ///
    /// This is the primitive every text operation reaches: reflow decides what
    /// bytes to write, and this records what they replaced.
    pub fn patch_content(
        &mut self,
        label: impl Into<String>,
        content: &LogicalContent,
        patches: &[Patch],
        fidelity: Fidelity,
    ) -> Result<EditReport, EditError> {
        if self.closed {
            return Err(EditError::Closed);
        }

        // The prior value of every object this will touch, captured *before*
        // anything is written. `stream::apply` is atomic, so if it fails these
        // were never needed; if it succeeds they are the exact undo.
        let targets = stream::localise(content, patches)?;
        let mut prior: BTreeMap<ObjId, Option<Object>> = BTreeMap::new();
        for id in targets.keys() {
            let before = match self.doc.get(*id) {
                Ok(object) => Some((*object).clone()),
                Err(_) => None,
            };
            prior.insert(*id, before);
        }

        let edit = stream::apply(self.doc, content, patches)?;

        let bytes_delta: isize =
            edit.touched.iter().map(|(_, before, after)| *after as isize - *before as isize).sum();
        let objects_touched: Vec<ObjId> = edit.touched.iter().map(|(id, _, _)| *id).collect();

        self.log.push(LoggedOp { label: label.into(), prior, fidelity: fidelity.clone() });
        // A new operation invalidates the redo stack: the state those entries
        // would restore is no longer reachable from here.
        self.redo.clear();

        Ok(EditReport { fidelity, objects_touched, bytes_delta })
    }

    /// Write whole objects as one logged operation.
    ///
    /// The sibling of [`patch_content`](Self::patch_content) for edits that are
    /// not about bytes inside a content stream: a page removed from a `/Kids`
    /// array, an outline destination retargeted, a `/Count` corrected. Those
    /// change the *shape* of the document rather than the marks on a page, and
    /// there is no span to splice.
    ///
    /// A `None` value deletes the object. Both directions are captured for
    /// undo, so a page operation is as reversible as a text one.
    ///
    /// Atomic in the same sense: every prior value is read before any new one
    /// is written, so a caller cannot observe a half-applied operation and an
    /// undo cannot restore one.
    pub fn set_objects(
        &mut self,
        label: impl Into<String>,
        changes: &[(ObjId, Option<Object>)],
        fidelity: Fidelity,
    ) -> Result<EditReport, EditError> {
        if self.closed {
            return Err(EditError::Closed);
        }
        if changes.is_empty() {
            return Ok(EditReport { fidelity, objects_touched: Vec::new(), bytes_delta: 0 });
        }

        let mut prior: BTreeMap<ObjId, Option<Object>> = BTreeMap::new();
        for (id, _) in changes {
            prior.insert(*id, self.doc.get(*id).ok().map(|o| (*o).clone()));
        }

        for (id, value) in changes {
            match value {
                Some(object) => self.doc.set(*id, object.clone()),
                None => self.doc.delete(*id),
            }
        }

        let objects_touched: Vec<ObjId> = changes.iter().map(|(id, _)| *id).collect();
        self.log.push(LoggedOp { label: label.into(), prior, fidelity: fidelity.clone() });
        self.redo.clear();

        // Object edits are not measured in content bytes; the writer reports
        // what the save actually appended.
        Ok(EditReport { fidelity, objects_touched, bytes_delta: 0 })
    }

    /// Undo the most recent operation.
    ///
    /// Restores each touched object's prior value byte for byte. Invariant I5.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        if self.closed {
            return Err(EditError::Closed);
        }
        let Some(entry) = self.log.pop() else { return Ok(false) };

        // The state to return to on redo is captured before unwinding, so redo
        // is exact for the same reason undo is.
        let mut forward: BTreeMap<ObjId, Option<Object>> = BTreeMap::new();
        for id in entry.prior.keys() {
            forward.insert(*id, self.doc.get(*id).ok().map(|o| (*o).clone()));
        }

        for (id, before) in &entry.prior {
            match before {
                Some(object) => self.doc.set(*id, object.clone()),
                // The object did not exist before this operation. Deleting is
                // the honest inverse of creating it.
                None => self.doc.delete(*id),
            }
        }

        self.redo.push(LoggedOp { label: entry.label, prior: forward, fidelity: entry.fidelity });

        // Restoring an object's *value* is not enough. `Document::set` marks it
        // dirty, and the writer then appends a revision rewriting it to exactly
        // what it already said — so the file grows and I5's "exact prior byte
        // state" fails on a document whose objects are all correct.
        //
        // With the log empty, every operation has been unwound, so every object
        // holds the value it had when the session opened. If the document was
        // clean then, that value is the one on disk and there is nothing left
        // to write. The session holds the only mutable borrow, so no change
        // from elsewhere can be discarded here by mistake.
        if self.log.is_empty() && !self.opened_dirty {
            self.doc.discard_changes();
        }
        Ok(true)
    }

    /// Redo the most recently undone operation.
    pub fn redo(&mut self) -> Result<bool, EditError> {
        if self.closed {
            return Err(EditError::Closed);
        }
        let Some(entry) = self.redo.pop() else { return Ok(false) };

        let mut backward: BTreeMap<ObjId, Option<Object>> = BTreeMap::new();
        for id in entry.prior.keys() {
            backward.insert(*id, self.doc.get(*id).ok().map(|o| (*o).clone()));
        }
        for (id, after) in &entry.prior {
            match after {
                Some(object) => self.doc.set(*id, object.clone()),
                None => self.doc.delete(*id),
            }
        }

        self.log.push(LoggedOp { label: entry.label, prior: backward, fidelity: entry.fidelity });
        Ok(true)
    }

    /// Undo everything this session did, in reverse order.
    ///
    /// A rollback leaves the document as the session found it, which is not
    /// necessarily as the file on disk is — a session opened over a document
    /// someone else had already modified restores *that* state, not the file's.
    pub fn rollback(&mut self) -> Result<(), EditError> {
        while self.undo()? {}
        self.redo.clear();
        self.closed = true;
        Ok(())
    }

    /// The combined fidelity of every operation on the log.
    pub fn fidelity(&self) -> Fidelity {
        self.log.iter().fold(Fidelity::Exact, |acc, op| acc.and(op.fidelity.clone()))
    }

    /// Write the document out. Spec 9.5.
    ///
    /// The session is closed afterwards: its op log describes edits against a
    /// byte layout that saving has just replaced, so continuing to undo through
    /// it would restore objects into a document they no longer describe.
    pub fn commit(&mut self, options: &SaveOptions) -> Result<SaveResult, EditError> {
        if self.closed {
            return Err(EditError::Closed);
        }
        let result =
            rasura_cos::save(self.doc, options).map_err(|e| EditError::Cos(e.to_string()))?;
        self.closed = true;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::classic_with_flate_content;

    fn content_of(doc: &Document) -> LogicalContent {
        let pages = rasura_content::page::pages(doc).expect("pages");
        let (content, errors) =
            rasura_content::content::page_content(doc, &pages.pages[0].dict).expect("content");
        assert!(errors.is_empty(), "{errors:?}");
        content
    }

    fn hello_span(content: &LogicalContent) -> std::ops::Range<usize> {
        // Length taken from the needle rather than written out: the two drifted
        // apart the moment the fixture string changed length, and a window of
        // the wrong size fails with "found" rather than with the reason.
        const NEEDLE: &[u8] = b"(Hello, rasura)";
        let at = content.data().windows(NEEDLE.len()).position(|w| w == NEEDLE).expect("found");
        at..at + NEEDLE.len()
    }

    #[test]
    fn an_edit_then_undo_restores_the_exact_bytes() {
        // Invariant I5, at the level this crate can assert it: the saved output
        // after edit-then-undo is the file that was opened.
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content(
                "replace",
                &content,
                &[Patch::new(span, b"(Howdy, rasura)".to_vec())],
                Fidelity::Exact,
            )
            .expect("patch");
        assert!(session.document().is_dirty());

        assert!(session.undo().expect("undo"));
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        assert_eq!(saved, original, "undo restored the exact prior byte state");
    }

    #[test]
    fn undo_of_several_operations_unwinds_in_reverse() {
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);

        let mut session = EditSession::new(&mut doc);
        for text in [b"(Aaaaa, rasura)", b"(Bbbbb, rasura)", b"(Ccccc, rasura)"] {
            session
                .patch_content(
                    "replace",
                    &content,
                    &[Patch::new(span.clone(), text.to_vec())],
                    Fidelity::Exact,
                )
                .expect("patch");
        }
        assert_eq!(session.len(), 3);

        while session.undo().expect("undo") {}
        assert_eq!(session.len(), 0);

        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;
        assert_eq!(saved, original);
    }

    #[test]
    fn redo_puts_back_what_undo_took_away() {
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content(
                "replace",
                &content,
                &[Patch::new(span, b"(Howdy, rasura)".to_vec())],
                Fidelity::Exact,
            )
            .expect("patch");
        let edited = session.document().decoded_stream(ObjId::new(4, 0)).expect("decoded").to_vec();

        assert!(session.undo().expect("undo"));
        assert!(session.redo().expect("redo"));

        let after = session.document().decoded_stream(ObjId::new(4, 0)).expect("decoded");
        assert_eq!(&*after, &edited[..]);
    }

    #[test]
    fn a_new_operation_clears_the_redo_stack() {
        // The state a redo entry would restore is no longer reachable once the
        // history has branched. Keeping it would let a later redo resurrect
        // bytes from an abandoned timeline.
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content(
                "first",
                &content,
                &[Patch::new(span.clone(), b"(Aaaaa, rasura)".to_vec())],
                Fidelity::Exact,
            )
            .expect("patch");
        assert!(session.undo().expect("undo"));
        session
            .patch_content(
                "second",
                &content,
                &[Patch::new(span, b"(Bbbbb, rasura)".to_vec())],
                Fidelity::Exact,
            )
            .expect("patch");

        assert!(!session.redo().expect("redo"), "the redo stack was cleared");
    }

    #[test]
    fn a_failed_operation_is_not_logged() {
        // Atomicity has a bookkeeping half: an operation that changed nothing
        // must not leave an entry that a later undo would "restore".
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let content = content_of(&doc);
        let past_the_end = content.data().len() + 10;

        let mut session = EditSession::new(&mut doc);
        let err = session
            .patch_content(
                "doomed",
                &content,
                &[Patch::new(past_the_end..past_the_end + 5, b"x".to_vec())],
                Fidelity::Exact,
            )
            .expect_err("out of bounds");
        assert!(matches!(err, EditError::Stream(_)), "{err:?}");

        assert_eq!(session.len(), 0, "nothing was logged");
        assert!(!session.document().is_dirty(), "and nothing was written");
    }

    #[test]
    fn rollback_undoes_everything_and_closes_the_session() {
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content(
                "replace",
                &content,
                &[Patch::new(span, b"(Howdy, rasura)".to_vec())],
                Fidelity::Exact,
            )
            .expect("patch");
        session.rollback().expect("rollback");

        assert!(matches!(session.undo(), Err(EditError::Closed)));
        assert!(matches!(session.commit(&SaveOptions::default()), Err(EditError::Closed)));

        let saved = rasura_cos::save(&doc, &SaveOptions::default()).expect("save").bytes;
        assert_eq!(saved, original);
    }

    #[test]
    fn a_session_over_an_already_dirty_document_says_so() {
        // It can still edit, and its undo is still exact for what *it* did. But
        // "restores the prior byte state" means the state this session found,
        // not the file's, and a caller relying on I5 needs to know which.
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let page = doc.get(ObjId::new(3, 0)).expect("page").as_dict().expect("dict").clone();
        doc.set(ObjId::new(3, 0), Object::Dictionary(page));

        let session = EditSession::new(&mut doc);
        assert!(session.opened_dirty());
    }

    #[test]
    fn an_untouched_session_commits_the_file_it_opened() {
        // Invariant I1 through the session: opening a document, doing nothing,
        // and committing returns the input byte for byte.
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let mut session = EditSession::new(&mut doc);

        assert!(session.is_empty());
        assert_eq!(session.commit(&SaveOptions::default()).expect("commit").bytes, original);
    }

    #[test]
    fn object_edits_undo_as_exactly_as_content_edits() {
        // I5 for the object-level path. A page removed from `/Kids` has no
        // content span to splice, so it takes the other primitive -- and it has
        // to be as reversible.
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");

        let page_id = rasura_cos::ObjId::new(3, 0);
        let mut dict = doc.get(page_id).unwrap().as_dict().unwrap().clone();
        dict.insert(rasura_cos::Name::new("Rotate"), Object::Integer(90));

        let mut session = EditSession::new(&mut doc);
        session
            .set_objects("rotate", &[(page_id, Some(Object::Dictionary(dict)))], Fidelity::Exact)
            .expect("set");
        assert!(session.document().is_dirty());

        assert!(session.undo().expect("undo"));
        assert_eq!(session.commit(&SaveOptions::default()).expect("commit").bytes, original);
    }

    #[test]
    fn deleting_an_object_undoes_to_its_prior_value() {
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let victim = rasura_cos::ObjId::new(5, 0);

        let mut session = EditSession::new(&mut doc);
        session.set_objects("drop the font", &[(victim, None)], Fidelity::Exact).expect("set");
        assert!(session.undo().expect("undo"));
        assert_eq!(session.commit(&SaveOptions::default()).expect("commit").bytes, original);
    }

    #[test]
    fn an_empty_object_edit_is_not_logged() {
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let mut session = EditSession::new(&mut doc);
        session.set_objects("nothing", &[], Fidelity::Exact).expect("set");
        assert_eq!(session.len(), 0, "an operation that changed nothing is not on the log");
        assert!(!session.document().is_dirty());
    }

    #[test]
    fn compromises_accumulate_across_operations() {
        let a = Fidelity::Degraded(vec![Compromise::KerningRegenerated]);
        let b = Fidelity::Degraded(vec![Compromise::GlyphsInjected { count: 2 }]);

        assert!(Fidelity::Exact.and(Fidelity::Exact).is_exact());
        assert_eq!(Fidelity::Exact.and(a.clone()), a);

        let both = a.and(b);
        let Fidelity::Degraded(list) = both else { panic!("expected degraded") };
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn identical_compromises_are_not_reported_twice() {
        let a = Fidelity::Degraded(vec![Compromise::KerningRegenerated]);
        let b = Fidelity::Degraded(vec![Compromise::KerningRegenerated]);
        let Fidelity::Degraded(list) = a.and(b) else { panic!("expected degraded") };
        assert_eq!(list.len(), 1, "one cause, one report");
    }

    #[test]
    fn a_suspended_session_undoes_exactly_as_an_unbroken_one_would() {
        // Invariant I5 across a suspension. The whole point of parking the log
        // is that it is the *same* log: an undo after resuming must restore the
        // same bytes an undo before suspending would have, or the boundary a
        // WASM handle table needs has quietly weakened the guarantee.
        let original = classic_with_flate_content();
        let mut doc = Document::open(original.clone()).expect("open");
        let content = content_of(&doc);
        let span = hello_span(&content);
        let patch = Patch::new(span, b"(Goodbye, rasura)".to_vec());

        let state = {
            let mut session = EditSession::new(&mut doc);
            session
                .patch_content("replace", &content, std::slice::from_ref(&patch), Fidelity::Exact)
                .expect("patch");
            assert_eq!(session.len(), 1);
            session.suspend()
        };

        // The document is free here: no borrow outlives the block above, which
        // is the property a handle table needs and a `&mut` cannot give.
        assert!(doc.is_dirty(), "the edit is still staged while suspended");
        assert_eq!(state.len(), 1);
        assert!(!state.is_closed());

        let mut session = EditSession::resume(&mut doc, state);
        assert!(session.undo().expect("undo"), "the parked log was still there");
        assert!(session.is_empty());
        let saved = session.commit(&SaveOptions::default()).expect("commit");
        assert_eq!(saved.bytes, original, "byte-identical after a round trip through storage");
    }

    #[test]
    fn suspending_and_resuming_preserves_the_redo_stack() {
        // Redo is the half most likely to be dropped by a naive round trip,
        // because it is empty in the common case and nothing notices.
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let content = content_of(&doc);
        let patch = Patch::new(hello_span(&content), b"(Goodbye, rasura)".to_vec());

        let state = {
            let mut session = EditSession::new(&mut doc);
            session
                .patch_content("replace", &content, std::slice::from_ref(&patch), Fidelity::Exact)
                .expect("patch");
            session.undo().expect("undo");
            session.suspend()
        };
        assert_eq!(state.redo_len(), 1, "one operation waiting to be redone");

        let mut session = EditSession::resume(&mut doc, state);
        assert!(session.redo().expect("redo"), "the parked redo stack survived");
        assert_eq!(session.len(), 1);
    }

    #[test]
    fn a_closed_session_stays_closed_across_a_suspension() {
        // Otherwise parking and reloading would be a way to reopen a committed
        // session and undo into a byte layout the save has already replaced.
        let mut doc = Document::open(classic_with_flate_content()).expect("open");
        let state = {
            let mut session = EditSession::new(&mut doc);
            session.commit(&SaveOptions::default()).expect("commit");
            session.suspend()
        };
        assert!(state.is_closed());

        let mut session = EditSession::resume(&mut doc, state);
        assert!(matches!(session.undo(), Err(EditError::Closed)));
    }
}
