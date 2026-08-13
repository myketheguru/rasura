//! Mutation and commit. Spec 9.
//!
//! Layers one to four turn a file into a document model: bytes become objects,
//! objects become operators, operators become positioned glyphs, glyphs become
//! paragraphs. This layer is the only one that goes the other way, and it is
//! where spec §2's three properties are either kept or lost.
//!
//! The shape of the thing, in the order the bytes travel:
//!
//! ```text
//! operation -> reflow -> generated operators -> splice -> re-encode -> commit
//! ```
//!
//! Two rules govern all of it.
//!
//! **Nothing is rewritten that did not need to change.** Not the page, not the
//! stream, not the operator — the *bytes*. [`patch::splice`] copies every
//! unclaimed byte from the original buffer rather than regenerating it from a
//! parsed form, which is the only implementation of §2's first property that
//! cannot drift.
//!
//! **Nothing is guessed silently.** Every operation returns a report saying
//! what fidelity it achieved. When an exact edit is impossible the caller is
//! told, in a typed value, before anything is committed. A substitution that
//! nobody was warned about is a worse outcome than a refusal.
//!
//! # What is here
//!
//! | Spec | Module | State |
//! |---|---|---|
//! | 9.1 transaction model | [`session`] | op log, undo/redo, atomic commit, byte *and* object edits |
//! | 9.4 step 1, affected operators | [`locate`] | paragraph and range to byte spans |
//! | 9.4 step 2, operator generation | [`emit`] | the text operators, and number style |
//! | 9.4 step 3, splicing | [`patch`] | verbatim copy of everything unclaimed |
//! | 9.4 steps 4–5, streams | [`stream`] | logical spans to objects, filters preserved |
//! | 9.4 number formatting | [`numfmt`] | the producer's own precision, by median |
//! | 9.2 text operations | [`text`] | replace, insert, delete |
//! | 9.2 / 10.4 image operations | [`blocks`] | move, scale, delete |
//! | 9.3 reflow | [`reflow`] | greedy breaking, justification mechanism, overflow policy |
//! | §7.2 inverted | [`encode`] | text back to the codes *this* document uses |
//! | 9.2 paragraph geometry, 10.5 vectors, pages | — | not yet |
//!
//! The layering runs bottom-up: [`patch`] knows only bytes, [`stream`] adds the
//! document, [`session`] adds history, and the operations sit on all three.

#![forbid(unsafe_code)]

pub mod annots;
pub mod blocks;
pub mod compact;
pub mod draw;
pub mod emit;
pub mod encode;
pub mod flatten;
pub mod forms;
pub mod images;
pub mod locate;
pub mod numfmt;
pub mod pages;
pub mod patch;
pub mod redact;
pub mod reflow;
pub mod session;
pub mod stream;
pub mod tables;
pub mod text;

pub use annots::{AnnotEdit, AnnotError, Annotation, Kind, NewAnnotation};
pub use blocks::{
    BlockError, Fit, ImageReplacement, Replacement, delete_image, move_image, replace_image,
    scale_image,
};
pub use draw::{Canvas, DrawError};
pub use encode::{Encoder, Unencodable};
pub use flatten::{FlattenError, Flattened, flatten_annotations};
pub use forms::{Field, FieldKind, Form, FormError, set_text_value};
pub use locate::{EditablePage, ParagraphId, Selection};
pub use numfmt::NumberStyle;
pub use pages::{PageEdit, PageError, PageSpec, delete_page, insert_page, move_page};
pub use patch::{Patch, PatchError, Placement, Spliced, splice};
pub use redact::{RedactError, Redaction, verify};
pub use reflow::{Breaking, Justification, Overflow, Policy, Reflowed};
pub use session::{Compromise, EditError, EditReport, EditSession, Fidelity, SessionState};
pub use stream::{StreamEdit, StreamError};
pub use tables::{TableError, set_cell};
pub use text::{Edit, FontContext, TextError, delete_range, insert_text, replace_text};
