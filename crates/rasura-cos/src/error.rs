//! Errors and the leniency log.
//!
//! Spec 11.5: never throw a bare error. Every failure is coded and actionable.
//! Spec 5.2: every deviation from spec the parser tolerates is recorded, because
//! that log is the honest answer to "why did this file behave oddly".

use crate::object::ObjId;
use std::fmt;

pub type Result<T> = std::result::Result<T, CosError>;

/// Stable machine-readable code. Maps onto `PdfErrorCode` at the JS boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Malformed,
    EncryptedPasswordRequired,
    EncryptedUnsupported,
    UnsupportedFilter,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Malformed => "malformed",
            ErrorCode::EncryptedPasswordRequired => "encrypted-password-required",
            ErrorCode::EncryptedUnsupported => "encrypted-unsupported",
            ErrorCode::UnsupportedFilter => "unsupported-filter",
            ErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CosError {
    #[error("malformed PDF at byte {offset}: {reason}")]
    Malformed { offset: usize, reason: String },

    #[error("not a PDF: missing %PDF- header")]
    NotAPdf,

    #[error("unexpected end of input at byte {offset}")]
    UnexpectedEof { offset: usize },

    #[error("object {id} not found in the cross-reference table")]
    MissingObject { id: ObjId },

    #[error("object {id} is not of the expected type: {expected}")]
    TypeMismatch { id: ObjId, expected: &'static str },

    #[error("reference cycle while resolving {id}")]
    ReferenceCycle { id: ObjId },

    #[error("document is encrypted and the supplied password was rejected")]
    PasswordRequired,

    #[error("unsupported encryption: {0}")]
    UnsupportedEncryption(String),

    #[error("unsupported filter /{0}")]
    UnsupportedFilter(String),

    #[error("filter /{filter} failed to decode: {reason}")]
    FilterFailed { filter: String, reason: String },

    #[error("cross-reference recovery failed: {0}")]
    RecoveryFailed(String),

    #[error("internal invariant violated: {0}")]
    Internal(String),
}

impl CosError {
    pub fn code(&self) -> ErrorCode {
        match self {
            CosError::PasswordRequired => ErrorCode::EncryptedPasswordRequired,
            CosError::UnsupportedEncryption(_) => ErrorCode::EncryptedUnsupported,
            CosError::UnsupportedFilter(_) => ErrorCode::UnsupportedFilter,
            CosError::Internal(_) => ErrorCode::Internal,
            _ => ErrorCode::Malformed,
        }
    }

    /// Construct a `Malformed` error. Public because the layers above this one
    /// find malformed structure too -- a page tree that is not a dictionary is
    /// exactly this error, discovered by `rasura-content`.
    pub fn malformed(offset: usize, reason: impl Into<String>) -> Self {
        CosError::Malformed { offset, reason: reason.into() }
    }
}

/// A spec deviation the parser tolerated.
///
/// Recorded rather than silently swallowed. `Document::leniencies()` exposes the
/// whole list; a caller auditing a suspicious file reads this first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leniency {
    pub kind: LeniencyKind,
    /// Byte offset in the original file where the deviation was observed.
    pub offset: usize,
    pub detail: String,
}

impl Leniency {
    pub fn new(kind: LeniencyKind, offset: usize, detail: impl Into<String>) -> Self {
        Leniency { kind, offset, detail: detail.into() }
    }
}

impl fmt::Display for Leniency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at {}: {}", self.kind, self.offset, self.detail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeniencyKind {
    /// `/Length` was absent, indirect-and-unresolvable, or wrong; the real end
    /// of the stream was found by scanning for `endstream`.
    LengthRecovered,
    /// The whole cross-reference structure was rebuilt by scanning the file.
    XrefReconstructed,
    /// `startxref` pointed somewhere that did not contain a cross-reference.
    BadStartxref,
    /// A real number used exponent notation, which ISO 32000-1 does not permit.
    ExponentInReal,
    /// A numeric token had a shape the spec forbids (`--5`, `1.2.3`, `-`).
    MalformedNumber,
    /// An integer exceeded the representable range and was clamped.
    IntegerClamped,
    /// A hex string had an odd number of digits; the final digit was padded.
    OddHexDigit,
    /// `stream` was followed by a bare CR, which the spec forbids.
    BareCrAfterStream,
    /// An operator or keyword outside `BX`/`EX` was not recognised.
    UnknownKeyword,
    /// A cross-reference entry pointed at an object with a different number.
    XrefOffsetMismatch,
    /// A cross-reference subsection header was malformed but recoverable.
    MalformedXrefSection,
    /// The trailer lacked `/Root`; a `/Type /Catalog` object was located instead.
    CatalogScanned,
    /// A dictionary key was repeated; the last occurrence won.
    DuplicateDictKey,
    /// The file had leading bytes before `%PDF-`; all offsets are shifted.
    HeaderNotAtStart,
    /// The document declared a filter chain longer than its `/DecodeParms`.
    DecodeParmsMismatch,
    /// An object stream declared more objects than its contents provided.
    ObjStmTruncated,
    /// Generation number in the file disagreed with the xref entry.
    GenerationMismatch,
}

/// A non-fatal condition the caller ought to know about after an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The original file was linearised; appending a revision breaks that.
    LinearizationBroken,
    /// The document is encrypted; new content was encrypted with the file key.
    ReencryptedWithExistingKey,
    /// Objects were promoted out of an object stream to top level.
    ObjectsPromotedFromObjStm { count: usize },
    /// A full rewrite dropped objects nothing referenced.
    UnreferencedObjectsDropped { count: usize },
    /// The output was written with a *new* file key. Spec 5.5.
    ///
    /// Worth a warning rather than silence because the old password stops
    /// working at this moment, and a caller who has copies of the document
    /// elsewhere now holds two files that disagree about how to open them.
    ProtectionReplaced,
    /// The output was written with no protection at all. Spec 5.5.
    ProtectionRemoved,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::LinearizationBroken => {
                write!(f, "the appended revision breaks the original linearisation")
            }
            Warning::ReencryptedWithExistingKey => {
                write!(f, "new strings and streams were encrypted with the existing file key")
            }
            Warning::ObjectsPromotedFromObjStm { count } => {
                write!(f, "{count} object(s) promoted out of object streams to top level")
            }
            Warning::UnreferencedObjectsDropped { count } => {
                write!(f, "{count} unreferenced object(s) dropped by the full rewrite")
            }
            Warning::ProtectionReplaced => {
                write!(
                    f,
                    "the output was encrypted with a new file key; the old password no longer opens it"
                )
            }
            Warning::ProtectionRemoved => {
                write!(f, "the output is not encrypted; it opens without a password")
            }
        }
    }
}
