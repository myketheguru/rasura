//! # rasura
//!
//! True PDF editing: read a document as paragraphs and blocks, change it, and
//! write it back with the untouched 99% of the file byte-identical.
//!
//! This is the facade of spec 11.7:
//!
//! > The facade crate exposes the same model **synchronously** for native
//! > consumers (CLI, server, tests). The WASM layer is a thin async adapter
//! > over it. **Design the Rust API first; do not let WASM ergonomics distort
//! > the core.**
//!
//! So there is nothing async here, no `Promise`-shaped return, and no handle
//! table. The WASM crate adds those; a CLI and the test suite should not pay
//! for them.
//!
//! # What this crate is for
//!
//! Underneath sit five crates, each with its own vocabulary: cross-reference
//! tables, glyph runs, logical content buffers, byte spans. They are the right
//! abstractions for each other and the wrong ones for an application. Spec
//! 11.1's second principle:
//!
//! > **No PDF concepts leak by default.** A developer replacing text should
//! > never see the word "xref". A power-user escape hatch (`document.raw`)
//! > exposes the object layer for those who need it.
//!
//! That is the whole design brief. [`Document`] speaks pages, paragraphs and
//! fonts; [`Document::raw`] hands over `rasura_cos::Document` for anyone
//! who needs the layer below, and it is a deliberate cliff rather than a
//! gradient — half-abstracted PDF is worse than either.
//!
//! # Fidelity is a return value
//!
//! Spec 11.1's third principle, and the reason [`edit::Session`]'s operations
//! return a report rather than a bare `Ok(())`:
//!
//! > **Fidelity is a return value, not an exception.** Degradation is normal
//! > and must be handled, not thrown.
//!
//! An edit that had to regenerate kerning succeeded. An edit that overflowed
//! its block succeeded. Both are things the caller has to know, and neither is
//! an error — so they arrive in [`edit::Outcome::fidelity`], where they cannot
//! be ignored by a caller who only checks for `Err`.
//!
//! ```no_run
//! use rasura::{Document, OpenOptions};
//!
//! let doc = Document::open(std::fs::read("input.pdf")?)?;
//! println!("{} pages, {}", doc.page_count(), doc.kind().as_str());
//!
//! let page = doc.page(0)?;
//! for paragraph in page.paragraphs() {
//!     println!("{}", paragraph.text);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

pub mod create;
pub mod edit;
pub mod error;
pub mod fonts;
pub mod kind;
pub mod metadata;
pub mod page;
pub mod supply;

pub use edit::{Outcome, Session, SessionState};
pub use error::{Code, Error, Result};
pub use fonts::{Coverage, FontInfo};
pub use kind::DocumentKind;
pub use metadata::Metadata;
pub use page::{Alignment, Block, Page, Paragraph, ParagraphId, Rect};
pub use supply::{FontHandle, RegisterOptions};

// Re-exported rather than mirrored: these are already free of PDF vocabulary
// and copying them would mean two definitions to keep in step.
pub use rasura_cos::{Leniency, ObjId, Permissions, SaveMode, Warning};
pub use rasura_layout::tags::TaggedStatus;

/// Annotations. Spec 10.4.
///
/// A narrower re-export rather than a mirror, for the reason above — but it is
/// its own module because these names are generic enough (`Kind`, `Annotation`)
/// that a flat re-export would collide with the next thing that needs one.
pub mod annotations {
    pub use rasura_edit::annots::{Annotation, Kind, NewAnnotation};
}

/// AcroForm fields. Spec 10.5.
pub mod forms {
    pub use rasura_edit::forms::{Field, FieldKind};
}

/// Redaction and its verification. Spec 9.6.
pub mod redaction {
    pub use rasura_edit::redact::{Options, Report, Trace};
}

/// Encryption, for documents this library creates or re-keys. Spec 5.
///
/// [`Entropy`] is caller-supplied on purpose: this crate has no RNG and does not
/// want one. A WASM build would have to reach for `crypto.getRandomValues`
/// through a shim, a server has a better source than either, and a library that
/// silently picks one is a library whose key material depends on which target it
/// happened to be compiled for.
pub mod protection {
    pub use rasura_cos::{Entropy, PermissionBits, ProtectionPolicy as Policy, Strength, Weakness};
}

use rasura_content::page::PageTree;

/// How to open a document. Spec 11.2.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Tried as both the user and the owner password. The empty password is
    /// always attempted regardless, because it is what most encrypted files
    /// use and requiring `Some("")` would be a papercut on every one of them.
    pub password: String,
    /// Whether to rebuild the cross-reference table when it cannot be followed.
    /// On by default: viewers do it, and a file that opens everywhere else
    /// should open here.
    pub recovery: Recovery,
    /// Analyse every page at open time instead of on first use.
    ///
    /// Off by default. Spec 13 budgets `open()` to first page metadata at
    /// 120 ms on a 500-page document, which is only achievable if opening does
    /// not read all 500 pages.
    pub eager: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Recovery {
    #[default]
    Auto,
    Never,
}

/// How to write a document back. Spec 11.2.
#[derive(Debug, Clone, Default)]
pub struct SaveOptions {
    /// `None` lets the document decide, which is what a caller should normally
    /// want: incremental where it is safe, and a full rewrite where the
    /// document requires one — after redaction, a protection change, or a
    /// recovered cross-reference table.
    pub mode: Option<SaveMode>,
    /// Proceed with a save that would invalidate a digital signature.
    ///
    /// Off by default and refused rather than warned about, because a signature
    /// is the one thing in a PDF whose whole purpose is to stop silent change.
    pub accept_signature_destruction: bool,
}

/// What a save produced.
#[derive(Debug, Clone)]
pub struct Saved {
    pub bytes: Vec<u8>,
    pub mode: SaveMode,
    /// How many bytes were appended. Zero for a full rewrite, where the concept
    /// does not apply.
    pub bytes_appended: usize,
    pub warnings: Vec<Warning>,
}

/// An open document. Spec 11.2.
pub struct Document {
    inner: rasura_cos::Document,
    pages: PageTree,
    kinds: Vec<kind::PageKind>,
    tags: rasura_layout::tags::TagReport,
    has_xfa: bool,
    /// Fonts the caller supplied. Spec 11.3.
    registry: Vec<supply::Registered>,
}

impl Document {
    pub fn open(bytes: Vec<u8>) -> Result<Document> {
        Document::open_with(bytes, &OpenOptions::default())
    }

    pub fn open_with(bytes: Vec<u8>, opts: &OpenOptions) -> Result<Document> {
        let cos = rasura_cos::Document::open_with(
            bytes,
            &rasura_cos::OpenOptions {
                password: opts.password.clone(),
                recovery: match opts.recovery {
                    Recovery::Auto => rasura_cos::RecoveryPolicy::Auto,
                    Recovery::Never => rasura_cos::RecoveryPolicy::Never,
                },
            },
        )?;

        let pages = rasura_content::page::pages(&cos)?;
        let tags = rasura_layout::validate_tags(&cos, &pages);
        let has_xfa = rasura_edit::forms::read(&cos).has_xfa;

        // Document kind needs every page, and every page is what `eager` is
        // about. Classifying lazily would make `kind()` the one accessor that
        // could be slow, so it is computed here — but only the cheap half:
        // image geometry and glyph visibility, not paragraph reconstruction.
        let kinds = pages.pages.iter().map(|p| kind::classify_page(&cos, p)).collect();

        Ok(Document { inner: cos, pages, kinds, tags, has_xfa, registry: Vec::new() })
    }

    pub fn page_count(&self) -> usize {
        self.pages.pages.len()
    }

    /// Whether this is a document or a photograph of one. Spec 11.2.
    pub fn kind(&self) -> DocumentKind {
        kind::classify(&self.kinds)
    }

    /// Spec 10.1. `Degraded` is the one worth branching on: a structure tree
    /// that no longer describes its content makes assistive technology read the
    /// document *worse* than no tree at all.
    pub fn tagged_status(&self) -> TaggedStatus {
        self.tags.status
    }

    /// Advisory. Spec 5.5: reported, never enforced — whether to honour a bit
    /// that says "printing not allowed" is the consuming application's legal
    /// and product decision, not a parser's.
    pub fn permissions(&self) -> Permissions {
        self.inner.permissions()
    }

    /// Every deviation from the specification tolerated while reading this file.
    ///
    /// Empty for a well-formed document. Worth surfacing because "why did this
    /// file behave oddly" is otherwise unanswerable.
    pub fn leniencies(&self) -> Vec<Leniency> {
        self.inner.leniencies()
    }

    /// True when the document's real content is an XFA payload the AcroForm
    /// only shadows. Spec 3 refuses to edit those, and this is how a caller
    /// finds out before trying.
    pub fn has_xfa(&self) -> bool {
        self.has_xfa
    }

    pub fn is_encrypted(&self) -> bool {
        self.inner.is_encrypted()
    }

    /// The revisions already in the file. A scraped page keeps traces of what
    /// it carried before; this is the list of them.
    pub fn revision_count(&self) -> usize {
        self.inner.revisions().len()
    }

    pub fn page(&self, index: usize) -> Result<Page> {
        let raw = self
            .pages
            .pages
            .get(index)
            .ok_or_else(|| Error::new(Code::Malformed, format!("no page {index}")))?;
        let kind = self.kinds.get(index).copied();
        Page::analyse(&self.inner, raw, kind)
    }

    /// What fonts this document uses and how usable each is. Spec 11.3.
    pub fn fonts(&self) -> Vec<FontInfo> {
        fonts::survey(&self.inner)
    }

    /// Supply a font the document does not have. Spec 11.3.
    ///
    /// Nothing happens now. The bytes are held against the moment an edit needs
    /// a character the document's own embedded font cannot draw, at which point
    /// §8.4's injection takes the outline out of this font and puts it *into*
    /// the document's — so the page carries on using one typeface, and the
    /// result is [`Fidelity::Reembedded`](crate::edit::Fidelity::Reembedded)
    /// rather than a substitution a reader can see.
    ///
    /// ```no_run
    /// # use rasura::{Document, RegisterOptions};
    /// # let mut doc = Document::open(vec![])?;
    /// doc.register_font(
    ///     std::fs::read("MinionPro-Regular.ttf")?,
    ///     &RegisterOptions { match_for: Some("MinionPro-Regular".into()) },
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn register_font(
        &mut self,
        bytes: Vec<u8>,
        opts: &supply::RegisterOptions,
    ) -> supply::FontHandle {
        self.registry.push(supply::Registered { bytes, match_for: opts.match_for.clone() });
        supply::FontHandle(self.registry.len() - 1)
    }

    /// How many fonts have been registered. Spec 11.3.
    pub fn registered_fonts(&self) -> usize {
        self.registry.len()
    }

    /// `/Info` and XMP, with their disagreements exposed. Spec 10.3.
    pub fn metadata(&self) -> Metadata {
        metadata::read(&self.inner)
    }

    /// The document's form fields, with fully-qualified names. Spec 10.4.
    pub fn form_fields(&self) -> Vec<rasura_edit::forms::Field> {
        rasura_edit::forms::read(&self.inner).fields
    }

    // -----------------------------------------------------------------------
    // Document-wide operations. These are not session operations, and the
    // distinction is real: each rewrites the whole file, so none of them can be
    // undone by restoring a handful of objects.
    // -----------------------------------------------------------------------

    /// Remove every occurrence of `text`, and verify it is gone. Spec 10.6.
    ///
    /// Document-wide, not page-scoped: a name removed from page one and left on
    /// page five is the silent failure the whole section is about. Forces a full
    /// rewrite — an incremental append would leave the original bytes, and the
    /// prior revision would still carry what was removed.
    ///
    /// Returns the strings removed, for [`verify_redaction`].
    pub fn redact(&mut self, text: &str) -> Result<Vec<String>> {
        self.redact_with(text, &rasura_edit::redact::Options::default())
    }

    /// Redact, choosing what to do about images that overlap the text.
    ///
    /// By default an overlapping image **refuses** the whole redaction. Image
    /// data is not searched (§10.6 step 2), so a scan of the same words survives
    /// the removal, and the caller who most wants redaction is the least likely
    /// to be reading a field in a return value for that news. Passing
    /// `allow_incomplete` moves the decision to the call site, where it is
    /// visible in review.
    pub fn redact_with(
        &mut self,
        text: &str,
        opts: &rasura_edit::redact::Options,
    ) -> Result<Vec<String>> {
        use rasura_edit::redact::RedactError;
        let redaction =
            rasura_edit::redact::apply_with(&mut self.inner, text, opts).map_err(|e| match e {
                // Its own code, because the caller's response differs: this one
                // is answerable by passing a flag or rasterising the region,
                // and every other redaction failure is not.
                RedactError::ImageOverlap { .. } => Error::from_layer(
                    Code::FidelityBelowRequired,
                    "an image overlaps the text and image data is not searched",
                    e,
                ),
                other => Error::from_layer(Code::Malformed, "the redaction was refused", other),
            })?;
        Ok(redaction.strings)
    }

    /// Re-read saved bytes and assert none of `strings` survives. Spec 10.6.
    ///
    /// Takes bytes rather than a document on purpose: verifying the in-memory
    /// document checks the thing that was edited, while verifying the saved
    /// file checks the thing that will be handed over — and those differ by
    /// exactly the save where an incremental append leaves the original behind.
    pub fn verify_redaction(bytes: &[u8], strings: &[String]) -> rasura_edit::redact::Report {
        rasura_edit::redact::verify(bytes, strings)
    }

    /// Protect the document, or change its password. Spec 5.5.
    ///
    /// `entropy` is 32 random bytes from the caller. This crate has no RNG and
    /// `wasm32-unknown-unknown` provides none — the object layer needing no
    /// filesystem, clock or randomness is what lets it run unchanged in a
    /// Worker, and pulling in `getrandom` for a salt would spend that property.
    ///
    /// Forces a full rewrite on the next save.
    pub fn protect(
        &mut self,
        policy: &rasura_cos::ProtectionPolicy,
        entropy: [u8; 32],
    ) -> Result<Vec<rasura_cos::Weakness>> {
        let entropy = rasura_cos::Entropy::new(entropy)
            .map_err(|e| Error::from_layer(Code::Internal, "the entropy was refused", e))?;
        let report = rasura_cos::protect::protect(&mut self.inner, policy, &entropy)
            .map_err(|e| Error::from_layer(Code::EncryptedUnsupported, "protection failed", e))?;
        Ok(report.weaknesses)
    }

    /// Remove protection, leaving a document anyone can open. Spec 5.5.
    pub fn unprotect(&mut self) -> Result<()> {
        rasura_cos::protect::unprotect(&mut self.inner)
            .map_err(|e| Error::from_layer(Code::EncryptedUnsupported, "unprotect failed", e))
    }

    /// Prune every compactable embedded font to the glyphs the document draws.
    /// Spec 8.6.
    ///
    /// Lossy by design: a glyph the document does not currently use is gone, so
    /// a later edit wanting one has to inject it again. Opt-in for that reason,
    /// and it needs a full rewrite to be worth anything — an incremental append
    /// would leave the old font program in the file.
    pub fn compact_fonts(&mut self) -> Result<usize> {
        let pages = rasura_content::page::pages(&self.inner)?;
        let report = rasura_edit::compact::plan(&self.inner, &pages);
        if report.is_empty() {
            return Ok(0);
        }
        let saved = report.bytes_saved();
        let changes: Vec<(rasura_cos::ObjId, Option<rasura_cos::Object>)> =
            report.changes.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();
        let mut session = rasura_edit::EditSession::new(&mut self.inner);
        session.set_objects("compact fonts", &changes, report.fidelity)?;
        Ok(saved)
    }

    /// Begin an edit. Nothing reaches the document until [`Session::commit`].
    pub fn edit(&mut self) -> Session<'_> {
        let registry = self.registry.clone();
        Session::new(&mut self.inner, registry)
    }

    /// Resume a parked session over this document. Spec 9.1.
    ///
    /// For a caller who cannot hold a borrow between operations — a handle
    /// table, an FFI boundary, a worker. See [`SessionState`].
    pub fn resume(&mut self, state: SessionState) -> Session<'_> {
        let registry = self.registry.clone();
        Session::resume(&mut self.inner, state, registry)
    }

    pub fn save(&self, opts: &SaveOptions) -> Result<Saved> {
        let cos = rasura_cos::SaveOptions {
            mode: opts.mode,
            accept_signature_destruction: opts.accept_signature_destruction,
        };
        // The one save failure that is a *policy* refusal rather than a defect,
        // so it gets its own code instead of arriving as `internal`.
        if !opts.accept_signature_destruction
            && rasura_cos::writer::effective_mode(&self.inner, &cos) == SaveMode::FullRewrite
            && rasura_cos::writer::signature_impact(&self.inner, SaveMode::FullRewrite)
                == rasura_cos::SignatureImpact::Destroyed
        {
            return Err(Error::new(
                Code::SignatureWouldBeDestroyed,
                "this save would invalidate the document's signature; \
                 set accept_signature_destruction to proceed",
            ));
        }

        let out = rasura_cos::save(&self.inner, &cos)?;
        Ok(Saved {
            bytes: out.bytes,
            mode: out.mode,
            bytes_appended: out.bytes_appended,
            warnings: out.warnings,
        })
    }

    /// Bytes held, for spec 12.5's budget.
    pub fn memory_usage(&self) -> usize {
        self.inner.memory_usage()
    }

    /// The object layer, for callers who need it. Spec 11.1's escape hatch.
    ///
    /// A cliff rather than a gradient. Everything above speaks pages and
    /// paragraphs; this speaks dictionaries and object numbers, and there is
    /// deliberately nothing in between — half-abstracted PDF is worse than
    /// either, because it looks safe and is not.
    pub fn raw(&self) -> &rasura_cos::Document {
        &self.inner
    }

    pub fn raw_mut(&mut self) -> &mut rasura_cos::Document {
        &mut self.inner
    }
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("pages", &self.page_count())
            .field("kind", &self.kind())
            .field("tagged", &self.tagged_status())
            .field("encrypted", &self.is_encrypted())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn simple() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(6, "<< /Title (A test document) /Author (Nobody) >>")
            .finish("/Root 1 0 R /Info 6 0 R")
    }

    #[test]
    fn a_document_opens_and_reports_itself_without_pdf_vocabulary() {
        let doc = Document::open(simple()).expect("open");
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.kind(), DocumentKind::BornDigital);
        assert_eq!(doc.tagged_status(), TaggedStatus::Untagged);
        assert!(!doc.has_xfa());
        assert!(!doc.is_encrypted());
        assert!(doc.leniencies().is_empty(), "a well-formed file tolerated nothing");
    }

    #[test]
    fn a_page_reads_back_as_paragraphs() {
        let doc = Document::open(simple()).expect("open");
        let page = doc.page(0).expect("page");
        let paragraphs = page.paragraphs();
        assert_eq!(paragraphs.len(), 1, "{paragraphs:?}");
        assert_eq!(paragraphs[0].text, "Hello world");
    }

    #[test]
    fn asking_for_a_page_that_is_not_there_is_a_coded_error() {
        let doc = Document::open(simple()).expect("open");
        let err = doc.page(7).expect_err("no page 7");
        assert_eq!(err.code(), Code::Malformed);
    }

    #[test]
    fn saving_an_unedited_document_returns_the_input_byte_for_byte() {
        // Invariant I1 at the facade. The property the whole library is built
        // around should hold at the surface a caller actually touches.
        let bytes = simple();
        let doc = Document::open(bytes.clone()).expect("open");
        let out = doc.save(&SaveOptions::default()).expect("save");
        assert_eq!(out.bytes, bytes);
        assert_eq!(out.bytes_appended, 0);
    }

    #[test]
    fn the_escape_hatch_reaches_the_object_layer() {
        // Spec 11.1: a cliff, on purpose. Everything above speaks paragraphs;
        // this speaks object numbers.
        let doc = Document::open(simple()).expect("open");
        assert!(doc.raw().catalog().is_ok());
        assert!(doc.memory_usage() > 0);
    }

    #[test]
    fn a_wrong_password_says_so_rather_than_saying_malformed() {
        let mut doc = Document::open(simple()).expect("open");
        let entropy = rasura_cos::Entropy::new(std::array::from_fn(|i| {
            (i as u8).wrapping_mul(37).wrapping_add(11)
        }))
        .expect("entropy");
        rasura_cos::protect::protect(
            doc.raw_mut(),
            &rasura_cos::ProtectionPolicy { user_password: "hunter2".into(), ..Default::default() },
            &entropy,
        )
        .expect("protect");
        let protected = doc.save(&SaveOptions::default()).expect("save").bytes;

        let err = Document::open(protected.clone()).expect_err("needs a password");
        assert_eq!(err.code(), Code::EncryptedPasswordRequired);

        let opts = OpenOptions { password: "hunter2".into(), ..OpenOptions::default() };
        let reopened = Document::open_with(protected, &opts).expect("opens");
        assert!(reopened.is_encrypted());
    }
}
