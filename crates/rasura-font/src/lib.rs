//! # rasura-font
//!
//! Font parsing, metrics, shaping and glyph injection.
//! Reference: spec §8.
//!
//! Spec 8.1 calls this "the hard part", and the reason is that embedded fonts
//! are almost always **subset**: they contain only the glyphs the document
//! already used. Type a character outside that set and there is no glyph to
//! draw. Every competitor resolves that silently and badly. The job of this
//! layer is to resolve it explicitly — inject the glyph when a source font is
//! registered, substitute and *say so* when one is not.
//!
//! Phase 4 is being built parsing-first, because nothing above it can be
//! measured until the font programs in the corpus can actually be read.

#![forbid(unsafe_code)]

pub mod cff;
pub mod cff_write;
pub mod charstring;
pub mod cmap;
pub mod cmap_write;
pub mod embed;
pub mod error;
pub mod fixture;
pub mod gsub;
pub mod inject;
pub mod metrics;
pub mod program;
pub mod reshape;
pub mod sfnt;
pub mod shape;
pub mod standard14;
pub mod subset;
pub mod substitute;
pub mod type1;
pub mod type3;

pub use cff::Cff;
pub use cff_write::inject_cff;
pub use cmap::Cmap;
pub use cmap_write::add_mappings;
pub use embed::{Added, FontUpdate, to_unicode_cmap, update_simple_font};
pub use error::{FontError, Result};
pub use gsub::{LigatureCoverage, ligature_coverage};
pub use inject::{Injection, inject_truetype};
pub use program::{Flavour, Program, Slot};
pub use reshape::{KerningSource, ReshapeSpan, kerning_source, span_for};
pub use sfnt::Sfnt;
pub use shape::{Direction, ShapeRequest, ShapedGlyph, shape};
pub use standard14::{STANDARD_14, StandardFont};
pub use subset::{Subset, SubsetPolicy, compact_truetype};
pub use substitute::{Candidate, Descriptor, Score, Substitution, best_match};
pub use type1::Type1;
pub use type3::{Type3, Type3GlyphMissing};
