//! Edits as data. Spec 9.1's op log, carried one level up.
//!
//! The session already keeps an op log — [`rasura_edit::session`] — but what it
//! records is the *inverse* of each operation as a byte image, which is the
//! right thing for `undo()` and the wrong thing for anyone who wants to store
//! what a person did and replay it later. That log says "object 4 used to hold
//! these bytes"; it cannot say "the first paragraph's first five characters
//! became `Q4`".
//!
//! This is the second log. It records the *forward* operation, as data a caller
//! can serialise, diff, transmit, and replay. An editor that stores its history
//! as these rather than as edited bytes can carry a document's whole revision as
//! a few hundred bytes of JSON instead of a few hundred kilobytes of PDF, show a
//! reviewer what changed in words, and — because the operations are the same
//! ones a live session issues — reconstruct the edited file by [replaying] them.
//!
//! [replaying]: crate::Session::apply
//!
//! # What a coordinate means here, and why replay is scoped to one document
//!
//! A [`crate::ParagraphId`] is opaque on purpose: it is a region and a paragraph
//! index underneath, and those numbers mean what they mean only for one exact
//! analysis of one exact document. So an op carries the *flat* index a caller
//! already used — the position in `page.paragraphs()` — which is the same
//! coordinate the public API addresses, and it is resolved against a freshly
//! analysed page at replay time.
//!
//! That resolution is deterministic for identical bytes and only for identical
//! bytes. Paragraph detection could legitimately improve between versions and
//! renumber the paragraphs on a page; an op log replayed against a *different*
//! document, or produced by a *different* library version, could then address
//! the wrong paragraph and corrupt silently — the one failure this project
//! refuses to let happen quietly. So the log is stamped with the version that
//! produced it, replay is defined as exact against the document the ops were
//! captured on, and every coordinate that does not resolve is refused with a
//! code rather than applied to whatever happens to be at that index.

use rasura_cos::ObjId;
use rasura_edit::annots::NewAnnotation;

/// One recorded operation, as replayable data.
///
/// The variants mirror the mutating methods of [`crate::Session`] one for one.
/// Coordinates are the flat, caller-facing ones: a page index, and a position
/// in `page.paragraphs()`, `page.images()` or `page.tables()` — never the
/// opaque internal id, which would not survive serialisation with its meaning
/// intact.
#[derive(Debug, Clone)]
pub enum EditOp {
    ReplaceText { page: usize, paragraph: usize, start: usize, end: usize, text: String },
    InsertText { page: usize, paragraph: usize, at: usize, text: String },
    DeleteRange { page: usize, paragraph: usize, start: usize, end: usize },
    MoveImage { page: usize, image: usize, dx: f64, dy: f64 },
    ScaleImage { page: usize, image: usize, sx: f64, sy: f64 },
    DeleteImage { page: usize, image: usize },
    SetCell { page: usize, table: usize, row: usize, column: usize, text: String },
    DeletePage { index: usize },
    MovePage { from: usize, to: usize },
    SetFieldValue { name: String, value: String },
    FlattenForms { page: usize },
    AddAnnotation { page: usize, annotation: NewAnnotation },
    DeleteAnnotation { page: usize, id: ObjId },
}

impl EditOp {
    /// The operation's name, matching the method and the JS op kind.
    ///
    /// Stable, because it is what a serialised op is keyed by. A rename here is
    /// a change to the wire format, not a tidy-up.
    pub fn kind(&self) -> &'static str {
        match self {
            EditOp::ReplaceText { .. } => "replaceText",
            EditOp::InsertText { .. } => "insertText",
            EditOp::DeleteRange { .. } => "deleteRange",
            EditOp::MoveImage { .. } => "moveImage",
            EditOp::ScaleImage { .. } => "scaleImage",
            EditOp::DeleteImage { .. } => "deleteImage",
            EditOp::SetCell { .. } => "setCell",
            EditOp::DeletePage { .. } => "deletePage",
            EditOp::MovePage { .. } => "movePage",
            EditOp::SetFieldValue { .. } => "setFieldValue",
            EditOp::FlattenForms { .. } => "flattenForms",
            EditOp::AddAnnotation { .. } => "addAnnotation",
            EditOp::DeleteAnnotation { .. } => "deleteAnnotation",
        }
    }
}

/// A sequence of operations with the library version that produced it.
///
/// The version is not decoration. These coordinates are exact against one
/// analysis of one document; a reader replaying them is entitled to know which
/// library's analysis they were written against, and to refuse — or at least
/// warn — when it does not match its own. [`crate::Session::apply`] does not
/// enforce the version itself, because what to do on a mismatch is the caller's
/// policy, not this layer's; the field is here so the caller can have one.
#[derive(Debug, Clone)]
pub struct OpLog {
    /// The `rasura` version that recorded these ops.
    pub version: String,
    pub ops: Vec<EditOp>,
}

impl OpLog {
    pub(crate) fn new(ops: Vec<EditOp>) -> OpLog {
        OpLog { version: env!("CARGO_PKG_VERSION").to_string(), ops }
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}
