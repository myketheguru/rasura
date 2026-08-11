//! # rasura-cos
//!
//! The Carousel Object System: the bottom layer of Rasura. Objects, lexer,
//! cross-reference resolution, stream filters, decryption, and the writer.
//! Reference: ISO 32000-1 §7.
//!
//! Nothing in this crate knows what a paragraph, a glyph, or a font is. Nothing
//! above it knows what a cross-reference table is. That separation is the whole
//! reason the layering in spec 4.1 is described as strict.
//!
//! All parsing here is panic-free on adversarial input by construction; the
//! crate forbids `unsafe` outright (spec 14.4).

#![forbid(unsafe_code)]

pub mod crypt;
pub mod document;
pub mod error;
pub mod filters;
pub mod lexer;
pub mod object;
pub mod parser;
pub mod protect;
pub mod recovery;
pub mod testutil;
pub mod writer;
pub mod xref;

pub use crypt::Permissions;
pub use document::{Document, LoadMode, OpenOptions, ProtectionChange, RecoveryPolicy};
pub use error::{CosError, ErrorCode, Leniency, LeniencyKind, Result, Warning};
pub use object::{Dictionary, Name, ObjId, Object, PdfString, Stream, StringForm};
pub use protect::{Entropy, PermissionBits, Policy as ProtectionPolicy, Strength, Weakness};
pub use writer::{SaveMode, SaveOptions, SaveResult, SignatureImpact, save};
pub use xref::{RevisionInfo, XrefStyle};
