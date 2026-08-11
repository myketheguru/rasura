//! Errors from font parsing.
//!
//! Every variant names *what* was wrong, not just that something was. A font
//! that fails to parse is a font whose text cannot be edited, so "why" is the
//! difference between a fixable bug and a file that was always broken.

/// A font program could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FontError {
    /// The structure ran off the end of the data.
    #[error("truncated font: {0} runs past the end of the data")]
    Truncated(&'static str),

    /// The structure is present but self-inconsistent.
    #[error("malformed font: {0}")]
    Malformed(&'static str),

    /// The bytes are not a font program this layer knows.
    #[error("unrecognised font program")]
    Unrecognised,

    /// A required table is absent.
    #[error("font is missing the {0} table")]
    MissingTable(&'static str),

    /// The font is already as large as its format allows.
    ///
    /// Distinct from `Malformed` on purpose. A malformed font is a defect in
    /// the file; this is a well-formed font sitting on a ceiling written into
    /// the container spec, and no amount of fixing either side will move it.
    /// The caller's recourse is different too — substitute or start a second
    /// font, not repair — so it has to be tellable apart from a parse failure.
    #[error("{what} is full: {have} of a maximum {limit}, so nothing more can be added")]
    Full { what: &'static str, have: usize, limit: usize },
}

pub type Result<T> = std::result::Result<T, FontError>;
