//! # rasura-layout
//!
//! Reconstruction: glyph runs to lines, blocks and paragraphs.
//! Reference: spec §7.
//!
//! This layer and the edit layer are the product. Everything below is table
//! stakes: `rasura-cos` gets the objects out of the file and
//! `rasura-content` says where each glyph landed, but neither knows what a
//! word is.
//!
//! Phase 3 is being built in the order the evidence justifies. Q1 measured that
//! the Adobe Glyph List carries the load in §7.2 -- 300 of the 653 fonts in the
//! corpus with no usable `/ToUnicode` resolve through it and nothing else --
//! so the derivation chain comes first and is measurable immediately against the
//! pdf.js differential.

#![forbid(unsafe_code)]

pub mod agl;
pub mod annots;
pub mod extract;
pub mod frames;
pub mod glyphdata;
pub mod graphics;
pub mod lines;
pub mod model;
pub mod paragraphs;
pub mod regions;
pub mod rules;
pub mod running;
pub mod script;
pub mod structure;
pub mod tables;
pub mod tags;
#[cfg(test)]
pub mod testutil;
pub mod unicode;
pub mod widths;
pub mod words;

pub use annots::{Annotation, Kind as AnnotKind};
pub use extract::{ResolveReport, ResolvedRun, page_text, resolve_page, resolve_runs};
pub use frames::{Frame, FrameSet};
pub use graphics::{Graphics, ImageBlock, VectorBlock};
pub use lines::{Line, PlacedGlyph, assemble, place};
pub use model::{
    Block, BlockId, DeclineReason, DocumentModel, OrderSource, PageInput, PageModel, RawBlock,
};
pub use paragraphs::{Alignment, Paragraph, SplitReason, Style, StyleRun, reconstruct};
pub use regions::{Origin, Region, detect};
pub use rules::Rule;
pub use running::{Footnote, PageRegions, Placement, RunningElement, footnotes, running_elements};
pub use script::Script;
pub use structure::{StructElement, StructTree};
pub use tables::{Cell, Table, TableOrigin, detect_page};
pub use tags::{Finding, TagReport, TaggedStatus, validate as validate_tags};
pub use unicode::{Decoder, Strategy, TextConfidence};
pub use widths::Standard14Widths;

/// Which of the standard 14 a font dictionary names, if any.
///
/// Shared by §7.2's encoding choice and §8.2's metrics: both have to agree on
/// what `/Symbol` means, and resolving it twice by different rules is how they
/// would come to disagree.
pub(crate) fn metrics_face(font_dict: &rasura_cos::Dictionary) -> Option<&'static str> {
    let base =
        font_dict.get("BaseFont").and_then(rasura_cos::Object::as_name).and_then(|n| n.as_str())?;
    rasura_font::metrics::resolve(base).map(|f| f.name)
}
pub use words::{BoundaryReason, Word, line_text, segment};
