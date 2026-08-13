//! One error type, always coded. Spec 11.5.
//!
//! > Never throw a bare `Error`. Every failure is coded and actionable.
//!
//! The five crates below this one each have their own error enum, and they are
//! the right shape for their own callers: `CosError::Malformed { offset,
//! reason }` is exactly what someone debugging a parser wants. None of them is
//! the right shape for the API surface, because §11.1's second principle says
//! no PDF concepts leak by default — and a byte offset into a cross-reference
//! table is the most PDF concept there is.
//!
//! So everything funnels into [`Error`], which carries a [`Code`] a caller can
//! branch on and a message a caller can show. The underlying error is kept in
//! [`Error::detail`] rather than discarded: the escape hatch exists for people
//! who need it, and throwing the cause away to keep the surface clean would
//! make debugging someone else's PDF impossible.

use std::fmt;

/// What went wrong, in a form a caller can branch on. Spec 11.5.
///
/// The variants are the specification's list. They are not a translation of the
/// layers' own errors — several distinct internal failures map to one code,
/// because a caller's *response* is what the code is for, and "the file is
/// malformed" prompts the same response whichever byte proved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    Malformed,
    EncryptedPasswordRequired,
    EncryptedUnsupported,
    ScannedNoText,
    XfaUnsupported,
    Type3GlyphMissing,
    FontUnavailable,
    Overflow,
    StaleSession,
    FidelityBelowRequired,
    SignatureWouldBeDestroyed,
    UnsupportedFilter,
    /// The caller asked for something that cannot be done as asked — composing
    /// a document with no content in it, a level outside 1 to 6.
    ///
    /// The fourteenth, and the only one not in spec 11.5's original list. Every
    /// code above it describes a condition of a *document*: it is malformed, it
    /// is encrypted, its font lacks a glyph. Composition introduced the first
    /// operations with no document to describe, and reporting "you passed an
    /// empty list" as `internal` would tell a caller their library is broken
    /// when their call is.
    InvalidArgument,
    Internal,
}

impl Code {
    /// The string form, which is the one the TypeScript surface uses.
    ///
    /// Written out rather than derived from the variant name: these strings are
    /// public API, and a rename that silently changed one would break every
    /// caller who branched on it.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Malformed => "malformed",
            Code::EncryptedPasswordRequired => "encrypted-password-required",
            Code::EncryptedUnsupported => "encrypted-unsupported",
            Code::ScannedNoText => "scanned-no-text",
            Code::XfaUnsupported => "xfa-unsupported",
            Code::Type3GlyphMissing => "type3-glyph-missing",
            Code::FontUnavailable => "font-unavailable",
            Code::Overflow => "overflow",
            Code::StaleSession => "stale-session",
            Code::FidelityBelowRequired => "fidelity-below-required",
            Code::SignatureWouldBeDestroyed => "signature-would-be-destroyed",
            Code::UnsupportedFilter => "unsupported-filter",
            Code::InvalidArgument => "invalid-argument",
            Code::Internal => "internal",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure, coded and with its cause kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: Code,
    message: String,
    /// The originating error's own words, for the escape hatch. Empty when this
    /// error was raised here rather than translated.
    detail: String,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Error {
        Error { code, message: message.into(), detail: String::new() }
    }

    /// Translate a lower layer's error, keeping what it said.
    pub fn from_layer(code: Code, message: impl Into<String>, cause: impl fmt::Display) -> Error {
        Error { code, message: message.into(), detail: cause.to_string() }
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// What the layer underneath said, when this error came from one.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            write!(f, "{}: {}", self.code, self.message)
        } else {
            write!(f, "{}: {} ({})", self.code, self.message, self.detail)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

impl From<rasura_cos::CosError> for Error {
    /// Map the object layer's failures onto §11.5's codes.
    ///
    /// The interesting case is the password one. `CosError::PasswordRequired`
    /// is raised both when no password was supplied and when the wrong one was
    /// — the handler cannot tell the difference, because a wrong password is
    /// indistinguishable from an absent one to the algorithm that checks it.
    /// One code covers both, and the caller's response is the same: ask.
    fn from(e: rasura_cos::CosError) -> Error {
        use rasura_cos::CosError as C;
        let code = match &e {
            C::PasswordRequired => Code::EncryptedPasswordRequired,
            C::UnsupportedEncryption(_) => Code::EncryptedUnsupported,
            C::UnsupportedFilter(_) | C::FilterFailed { .. } => Code::UnsupportedFilter,
            C::Internal(_) => Code::Internal,
            // Everything else is a statement about the bytes: a bad xref, a
            // missing object, a reference cycle, a truncated file. A caller
            // does the same thing with all of them.
            _ => Code::Malformed,
        };
        Error::from_layer(code, "the document could not be read", e)
    }
}

impl From<rasura_edit::EditError> for Error {
    fn from(e: rasura_edit::EditError) -> Error {
        use rasura_edit::EditError as E;
        let code = match &e {
            E::StaleSession | E::Closed => Code::StaleSession,
            _ => Code::Internal,
        };
        Error::from_layer(code, "the edit could not be applied", e)
    }
}

impl From<rasura_edit::TextError> for Error {
    /// The one place where a lower error's *shape* survives translation.
    ///
    /// `Overflow` is not a malformed-document error and must not be flattened
    /// into one: it is the specification's own outcome for an edit that no
    /// longer fits, and §11.4's `overflow: 'refuse'` exists so a caller can ask
    /// for it. Losing the distinction would make that setting unusable.
    fn from(e: rasura_edit::TextError) -> Error {
        use rasura_edit::TextError as T;
        use rasura_edit::reflow::ReflowError as R;
        let code = match &e {
            T::Reflow(R::Overflow { .. }) | T::Reflow(R::Unbreakable { .. }) => Code::Overflow,
            T::Unencodable(_) => Code::FontUnavailable,
            T::NotImplemented(_) => Code::Internal,
            _ => Code::Malformed,
        };
        Error::from_layer(code, "the text could not be replaced", e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_string_the_spec_lists() {
        // These strings are public API. A rename that silently changed one
        // would break every caller branching on it.
        let expected = [
            (Code::Malformed, "malformed"),
            (Code::EncryptedPasswordRequired, "encrypted-password-required"),
            (Code::EncryptedUnsupported, "encrypted-unsupported"),
            (Code::ScannedNoText, "scanned-no-text"),
            (Code::XfaUnsupported, "xfa-unsupported"),
            (Code::Type3GlyphMissing, "type3-glyph-missing"),
            (Code::FontUnavailable, "font-unavailable"),
            (Code::Overflow, "overflow"),
            (Code::StaleSession, "stale-session"),
            (Code::FidelityBelowRequired, "fidelity-below-required"),
            (Code::SignatureWouldBeDestroyed, "signature-would-be-destroyed"),
            (Code::UnsupportedFilter, "unsupported-filter"),
            (Code::Internal, "internal"),
        ];
        for (code, text) in expected {
            assert_eq!(code.as_str(), text);
        }
    }

    #[test]
    fn a_wrong_password_is_the_password_code_not_a_malformed_one() {
        let e: Error = rasura_cos::CosError::PasswordRequired.into();
        assert_eq!(e.code(), Code::EncryptedPasswordRequired);
        // And the cause survives, because debugging someone else's PDF is
        // impossible without it.
        assert!(!e.detail().is_empty());
    }

    #[test]
    fn an_overflow_does_not_become_a_malformed_document() {
        // A caller who set `overflow: refuse` needs to tell "your text does not
        // fit" apart from "your file is broken". Flattening both into
        // `malformed` would make that setting unusable.
        let e: Error = rasura_edit::TextError::Reflow(rasura_edit::reflow::ReflowError::Overflow {
            lines_over: 2,
        })
        .into();
        assert_eq!(e.code(), Code::Overflow);
    }

    #[test]
    fn the_display_form_leads_with_the_code() {
        let e = Error::new(Code::XfaUnsupported, "this is an XFA form");
        assert!(format!("{e}").starts_with("xfa-unsupported: "));
    }
}
