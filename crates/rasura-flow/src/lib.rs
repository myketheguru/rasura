//! # rasura-flow
//!
//! The flow model: a PDF's reconstructed document as content that follows
//! content, and export from it.
//!
//! This is step 1 of `docs/flow-model.md`, and it is worth being precise about
//! what that document claims, because the claim is unusual for a PDF library:
//!
//! > A PDF is the *output* of a layout process, not an input to one. Every
//! > glyph carries an absolute position; the block on page 4 is not positioned
//! > relative to anything on page 3. The flow that a word processor used — this
//! > paragraph follows that one, and if the first grows the second moves — was
//! > discarded when the file was written.
//!
//! So there is no flow to read. There is only a flow to *reconstruct*, and the
//! honest form of that is a separate model with a separate contract rather than
//! a mode bolted onto the editing API.
//!
//! # Two modes, one stack
//!
//! Everything else in this workspace is **surgical**: byte spans in a content
//! stream, original bytes plus a patch, and §2's first property — an edit
//! changes no byte it did not need to touch — holding absolutely. This crate is
//! the beginning of **document** mode, where that property cannot hold because
//! the output is regenerated rather than patched.
//!
//! The two share everything below the edit layer and this crate adds nothing to
//! it. That is the architectural claim `docs/flow-model.md` makes and the
//! reason it is safe to start here: if document mode turns out to be a bad
//! idea, nothing built for surgical mode was spent on it.
//!
//! # What this crate does and does not do
//!
//! It converts [`rasura_layout::model::DocumentModel`] — blocks with bounding
//! boxes, on pages, in a reading order — into [`FlowDocument`], which has no
//! coordinates and no pages. Headings, lists and tables come from
//! `/StructTreeRoot` where the producer supplied one and from typography where
//! it did not, and **every inference is counted in [`Report`]**.
//!
//! It does *not* lay anything out. Frame inference and a layout engine are
//! steps 3 and 5 of that document; export is step 1 precisely because it needs
//! neither, and because an export is read by a person who will notice a
//! scrambled paragraph immediately. Getting this part judged before building a
//! layout engine on top of it is the whole point of the ordering.
//!
//! ```no_run
//! use rasura_flow::{markdown, to_flow};
//!
//! let doc = rasura_cos::Document::open(std::fs::read("input.pdf")?)?;
//! let (flow, report) = to_flow(&doc)?;
//!
//! for line in report.lines() {
//!     eprintln!("note: {line}");
//! }
//! print!("{}", markdown::render(&flow, &markdown::Options::default()));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

pub mod build;
pub mod compare;
pub mod emit;
pub mod flow;
pub mod layout;
pub mod markdown;
pub mod report;

#[cfg(test)]
mod tests;

pub use build::{Options, flow};
pub use compare::{Difference, Drift};
pub use flow::{Block, Emphasis, FlowDocument, Inline, Provenance};
pub use layout::{Layout, Measurer, Standard14};
pub use report::{Guess, Report};

/// Analyse a document and convert it in one step.
///
/// The convenience over [`flow`] for callers who have bytes rather than a
/// model. Anyone who already built a [`rasura_layout::model::DocumentModel`] —
/// to read it as well as flow it — should call [`flow`] and not analyse twice.
pub fn to_flow(doc: &rasura_cos::Document) -> Result<(FlowDocument, Report), rasura_cos::CosError> {
    let model = rasura_layout::model::analyse(doc)?;
    Ok(flow(&model, &Options::default()))
}

/// Analyse, convert and render as Markdown.
pub fn to_markdown(doc: &rasura_cos::Document) -> Result<(String, Report), rasura_cos::CosError> {
    let (document, report) = to_flow(doc)?;
    Ok((markdown::render(&document, &markdown::Options::default()), report))
}
