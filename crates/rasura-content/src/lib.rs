//! # rasura-content
//!
//! Content-stream tokenizer, graphics state machine, and serializer.
//! Reference: ISO 32000-1 §8 (graphics) and §9 (text).
//!
//! This layer knows what an operator is, what the graphics state is, and where
//! a glyph lands. It does not know what a paragraph is -- that is
//! `rasura-layout` -- and it does not know what a cross-reference table is,
//! which is `rasura-cos` below it.
//!
//! The load-bearing requirement here is span preservation (spec 6.2): every
//! operator retains its byte range in the decoded stream, so the edit layer can
//! splice replacement bytes at exactly the spans it needs and leave every other
//! byte alone.

#![forbid(unsafe_code)]

pub mod cmap;
pub mod content;
pub mod dest;
pub mod font;
pub mod matrix;
pub mod op;
pub mod optional;
pub mod page;
pub mod resources;
pub mod state;
pub mod text;
pub mod tokenizer;
pub mod walker;

pub use cmap::CMap;
pub use content::{ContentPart, LogicalContent, page_content};
pub use dest::{Carrier, Destination, Navigation};
pub use font::{CodeUnit, FontKind, LoadedFont};
pub use matrix::{Matrix, Point, Rect};
pub use op::{InlineImage, Op, OpKind};
pub use optional::{Layer, OptionalContent, Region as OptionalRegion};
pub use page::{Page, PageTree, pages};
pub use resources::ResourceStack;
pub use state::{Colour, GraphicsState, StateMachine, TextState, word_spacing_applies};
pub use text::{GlyphRun, PositionedGlyph, TextExtractor, TextReport, extract_page, page_text};
pub use tokenizer::{Tokenizer, tokenize};
pub use walker::{
    ContentVisitor, Flow, FnVisitor, Source, WalkContext, WalkReport, walk_page, walk_stream,
};
