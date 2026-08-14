//! The WASM surface. Spec 12.
//!
//! > The facade crate exposes the same model synchronously for native
//! > consumers. **The WASM layer is a thin async adapter over it.** Design the
//! > Rust API first; do not let WASM ergonomics distort the core.
//!
//! Thin is the operative word. Every method here forwards to
//! [`rasura`] and translates types at the boundary; none of them decides
//! anything. If a rule needs to exist, it belongs one crate down where a CLI
//! and the test suite also get it.
//!
//! # Where the `async` actually comes from
//!
//! §11.1's fourth principle says "everything is `async` at the JS boundary
//! because everything crosses a Worker", and it is worth being precise about
//! what that means here, because it is easy to build the wrong thing.
//!
//! Nothing in this file is async. `wasm-bindgen` can return a `Promise`, and
//! doing so would be pure ceremony: the work is CPU-bound Rust with no I/O to
//! await, so an `async fn` would resolve on the same tick having blocked for
//! exactly as long. The asynchrony a caller sees comes from the **Worker**, and
//! `postMessage` is what makes it real — the main thread does not block because
//! the work is on another thread, not because the function returned a promise.
//!
//! So: synchronous inside the Worker, asynchronous outside it, and the JS
//! wrapper owns the boundary. Building promises in here would add a layer that
//! looks like concurrency and provides none.
//!
//! # Handles, not objects
//!
//! A `Document` holds the file's bytes plus a cache, and neither can cross to
//! JS. So documents live in a registry here and JS holds a `number`. That is
//! also what makes [`close_document`] necessary rather than a courtesy: WASM
//! linear memory is not garbage-collected against JS's heap, so a caller who
//! opens two hundred documents and drops the handles leaks all two hundred.
//!
//! # Errors carry their code
//!
//! §11.5: "Never throw a bare `Error`. Every failure is coded and actionable."
//! Every fallible function returns `Result<_, JsValue>` where the value is an
//! `Error` with a `code` property carrying §11.5's string. A caller writes
//! `if (e.code === 'encrypted-password-required')`, which is the whole point of
//! having codes.

use rasura::{Document, OpenOptions, Recovery, SaveOptions, SessionState};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

mod convert;
use convert::{err, obj, set, set_num, set_str};

// Open documents, by handle.
//
// `thread_local!` rather than a static: the default build is single-threaded
// (§12.1), and a `Mutex` would cost every call an uncontended lock to guard
// against a thread that does not exist. The threaded entry point will need its
// own registry, which is a reason to keep this one honest about what it is.
thread_local! {
    static OPEN: RefCell<Registry> = RefCell::new(Registry::default());
}

/// A document, and the edit session over it if one is open.
///
/// One slot rather than two maps. A [`SessionState`] is only meaningful against
/// the document it was suspended from — an object id means nothing outside the
/// document that issued it — so pairing them here makes it impossible to resume
/// a session against the wrong file. Two maps keyed by the same handle would
/// make it merely unlikely.
struct Slot {
    doc: Document,
    session: Option<SessionState>,
}

#[derive(Default)]
struct Registry {
    documents: HashMap<u32, Slot>,
    next: u32,
}

impl Registry {
    fn insert(&mut self, doc: Document) -> u32 {
        // Handles are never reused, so a stale one from a closed document is an
        // error rather than a reference to somebody else's file.
        self.next += 1;
        self.documents.insert(self.next, Slot { doc, session: None });
        self.next
    }
}

fn missing(handle: u32) -> JsValue {
    err("stale-session", &format!("document {handle} is closed or was never opened"))
}

/// Run `f` with the document a handle names.
fn with<T>(handle: u32, f: impl FnOnce(&Document) -> Result<T, JsValue>) -> Result<T, JsValue> {
    OPEN.with(|r| {
        let registry = r.borrow();
        let slot = registry.documents.get(&handle).ok_or_else(|| missing(handle))?;
        f(&slot.doc)
    })
}

/// Run `f` with an open edit session, parking it again afterwards.
///
/// The take-and-put-back is what makes a session survive a JS call. Between
/// operations the state sits in the slot as a plain value; for the duration of
/// one it is a real [`rasura::Session`] holding a borrow. The state is
/// restored even when `f` fails, because a failed operation must not throw away
/// the four successful ones before it.
fn with_session<T>(
    handle: u32,
    f: impl FnOnce(&mut rasura::Session<'_>) -> Result<T, JsValue>,
) -> Result<T, JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;

        let state = slot.session.take().unwrap_or_default();
        if state.is_closed() {
            slot.session = Some(state);
            return Err(err(
                "stale-session",
                "this edit session has been committed or rolled back",
            ));
        }

        let mut session = slot.doc.resume(state);
        let outcome = f(&mut session);
        slot.session = Some(session.suspend());
        outcome
    })
}

/// Open a document. Spec 11.2's `Pdf.open`.
///
/// The bytes are taken by value. `wasm-bindgen` copies a `Uint8Array` into
/// linear memory here, which is unavoidable — a JS `ArrayBuffer` and WASM
/// memory are different allocations — and is why §12.2 says to *transfer* the
/// buffer to the Worker rather than copy it there too. One copy is the floor;
/// two is a choice.
#[wasm_bindgen(js_name = openDocument)]
pub fn open_document(
    bytes: Vec<u8>,
    password: Option<String>,
    recovery: Option<String>,
) -> Result<u32, JsValue> {
    let opts = OpenOptions {
        password: password.unwrap_or_default(),
        recovery: match recovery.as_deref() {
            Some("never") => Recovery::Never,
            _ => Recovery::Auto,
        },
        eager: false,
    };
    let doc = Document::open_with(bytes, &opts).map_err(convert::from_error)?;
    Ok(OPEN.with(|r| r.borrow_mut().insert(doc)))
}

/// Compose a document. Spec 11's `create`, unimplemented until now.
///
/// `content` is an array of `{ kind, text }` — or `{ kind: "list", items }` —
/// rather than a typed enum, because a `JsValue` is what crosses and inventing
/// a class for three shapes would put a constructor on the JS side that has to
/// stay in step with this one.
///
/// The typeface is required and is passed as bytes. There is no default: a
/// document set in a font nobody embedded looks like whatever the reader has
/// installed, which is the one thing a PDF is supposed to prevent. Returns the
/// handle and what composing had to approximate.
#[wasm_bindgen(js_name = createDocument)]
pub fn create_document(
    content: JsValue,
    font: Vec<u8>,
    options: JsValue,
) -> Result<JsValue, JsValue> {
    let blocks = convert::to_content(&content)?;
    let opts = convert::to_create_options(&options, font)?;
    let (doc, report) = Document::create(&blocks, &opts).map_err(convert::from_error)?;

    let out = obj();
    set_num(&out, "handle", f64::from(OPEN.with(|r| r.borrow_mut().insert(doc))));
    set_num(&out, "pages", report.pages as f64);
    set_num(&out, "lines", report.lines as f64);
    set_num(&out, "approximated", report.approximated as f64);
    set_str(&out, "baseFont", &report.base_font);
    set(&out, "composite", &JsValue::from_bool(report.composite));
    set(&out, "stemVEstimated", &JsValue::from_bool(report.stem_v_estimated));
    // The characters the typeface could not draw, as a string. They were
    // dropped, not substituted, and a caller who does not look at this will
    // ship a document with holes in it.
    set_str(&out, "missing", &report.missing.iter().collect::<String>());
    Ok(out.into())
}

/// Release a document's memory. Spec 12.5.
///
/// Returns whether there was anything to close, so a double close is visible to
/// a caller who cares and harmless to one who does not.
#[wasm_bindgen(js_name = closeDocument)]
pub fn close_document(handle: u32) -> bool {
    OPEN.with(|r| r.borrow_mut().documents.remove(&handle).is_some())
}

/// Everything §11.2 puts on `Document` as readable properties, in one call.
///
/// One call rather than eleven accessors because each crossing has a cost and
/// none of these is expensive to compute — a JS wrapper that read them
/// individually would pay eleven boundary crossings to build the object it was
/// always going to build.
#[wasm_bindgen(js_name = documentInfo)]
pub fn document_info(handle: u32) -> Result<JsValue, JsValue> {
    with(handle, |doc| {
        let out = obj();
        set_num(&out, "pageCount", doc.page_count() as f64);
        set_str(&out, "documentKind", doc.kind().as_str());
        set_str(&out, "taggedStatus", doc.tagged_status().as_str());
        set(&out, "hasXfa", &JsValue::from_bool(doc.has_xfa()));
        set(&out, "encrypted", &JsValue::from_bool(doc.is_encrypted()));
        set_num(&out, "revisionCount", doc.revision_count() as f64);
        set_num(&out, "memoryUsage", doc.memory_usage() as f64);
        set(&out, "permissions", &convert::permissions(&doc.permissions()));
        set(&out, "leniencies", &convert::strings(doc.leniencies().iter().map(|l| l.to_string())));
        Ok(out.into())
    })
}

/// A page's paragraphs and blocks. Spec 11.2.
#[wasm_bindgen(js_name = pageContent)]
pub fn page_content(handle: u32, index: usize) -> Result<JsValue, JsValue> {
    with(handle, |doc| {
        let page = doc.page(index).map_err(convert::from_error)?;
        Ok(convert::page(&page))
    })
}

/// What fonts this document needs. Spec 11.3.
#[wasm_bindgen(js_name = fontRequirements)]
pub fn font_requirements(handle: u32) -> Result<JsValue, JsValue> {
    with(handle, |doc| Ok(convert::fonts(&doc.fonts())))
}

/// Supply a font the document does not have. Spec 11.3.
///
/// Returns the number of fonts now registered. Held until an edit needs a
/// character the document's own embedded font cannot draw, at which point
/// §8.4's injection puts the outline *into* that font — so the page keeps one
/// typeface and the result is `reembedded` rather than a visible substitution.
#[wasm_bindgen(js_name = registerFont)]
pub fn register_font(
    handle: u32,
    bytes: Vec<u8>,
    match_for: Option<String>,
) -> Result<usize, JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        slot.doc.register_font(bytes, &rasura::RegisterOptions { match_for });
        Ok(slot.doc.registered_fonts())
    })
}

/// `/Info` and XMP, with their disagreements. Spec 10.3.
#[wasm_bindgen(js_name = documentMetadata)]
pub fn document_metadata(handle: u32) -> Result<JsValue, JsValue> {
    with(handle, |doc| Ok(convert::metadata(&doc.metadata())))
}

/// Set the session's fidelity floor and reflow policy. Spec 11.4.
///
/// Separate from the operations because it applies to all of them, which is
/// what §11.4 describes: `doc.edit({ overflow: 'refuse', lineBreaking:
/// 'greedy' })` configures a session once and then uses it.
#[wasm_bindgen(js_name = configureSession)]
pub fn configure_session(
    handle: u32,
    require: Option<String>,
    breaking: Option<String>,
    overflow: Option<String>,
) -> Result<JsValue, JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        let mut state = slot.session.take().unwrap_or_default();
        state.require(convert::fidelity_floor(require.as_deref())?);
        state.reflow(
            convert::breaking(breaking.as_deref())?,
            convert::overflow(overflow.as_deref())?,
        );
        let out = convert::session_state(&state);
        slot.session = Some(state);
        Ok(out)
    })
}

/// Replace a character range of a paragraph. Spec 9.2, 11.4.
///
/// **Staged, not committed.** The operation joins the session's log and nothing
/// is written until [`commit_session`] — which is what lets [`undo_session`]
/// restore the exact prior bytes, and what lets a caller make four edits and
/// keep or discard them together.
#[wasm_bindgen(js_name = replaceText)]
pub fn replace_text(
    handle: u32,
    page_index: usize,
    paragraph: usize,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        // Analysed against the document *as this session has left it*, so a
        // second edit sees the first one's bytes. Analysing once when the
        // session opened would address a byte layout an earlier operation has
        // already replaced.
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = paragraph_id(&page, paragraph)?;

        let outcome = session
            .replace_text(&page, id, start..end, replacement)
            .map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Insert text at a character offset. Spec 9.2.
#[wasm_bindgen(js_name = insertText)]
pub fn insert_text(
    handle: u32,
    page_index: usize,
    paragraph: usize,
    at: usize,
    text: &str,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = paragraph_id(&page, paragraph)?;
        let outcome = session.insert_text(&page, id, at, text).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Delete a character range. Spec 9.2.
///
/// Not redaction. This removes the glyphs from the page; §9.6's `redact` is
/// what removes every trace of the text from the *file*, and the two are
/// different operations with different guarantees.
#[wasm_bindgen(js_name = deleteRange)]
pub fn delete_range(
    handle: u32,
    page_index: usize,
    paragraph: usize,
    start: usize,
    end: usize,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = paragraph_id(&page, paragraph)?;
        let outcome = session.delete_range(&page, id, start..end).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Resolve JS's paragraph index against a freshly analysed page.
///
/// Shared by every text operation because getting it wrong is not a compile
/// error: an out-of-range index would otherwise reach the facade as a
/// `ParagraphId` from a different page and address something real.
fn paragraph_id(page: &rasura::Page, index: usize) -> Result<rasura::ParagraphId, JsValue> {
    page.paragraphs()
        .get(index)
        .map(|p| p.id)
        .ok_or_else(|| err("malformed", "no such paragraph on this page"))
}

fn image_id(page: &rasura::Page, index: usize) -> Result<rasura::page::ImageId, JsValue> {
    page.images()
        .get(index)
        .map(|i| i.id)
        .ok_or_else(|| err("malformed", "no such image on this page"))
}

/// Move an image by a page-space offset. Spec 11.2's `moveImage`.
#[wasm_bindgen(js_name = moveImage)]
pub fn move_image(
    handle: u32,
    page_index: usize,
    image: usize,
    dx: f64,
    dy: f64,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = image_id(&page, image)?;
        let outcome = session.move_image(&page, id, dx, dy).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Scale an image about its own origin. Spec 11.2's `scaleImage`.
#[wasm_bindgen(js_name = scaleImage)]
pub fn scale_image(
    handle: u32,
    page_index: usize,
    image: usize,
    sx: f64,
    sy: f64,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = image_id(&page, image)?;
        let outcome = session.scale_image(&page, id, sx, sy).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Remove an image from the page. Spec 11.2's `deleteImage`.
///
/// The drawing operator goes; the XObject stays in the file unless nothing else
/// references it and the document is later compacted. A caller who needs the
/// pixels *gone* wants a full rewrite, which is what §9.6 forces.
#[wasm_bindgen(js_name = deleteImage)]
pub fn delete_image(handle: u32, page_index: usize, image: usize) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let id = image_id(&page, image)?;
        let outcome = session.delete_image(&page, id).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Replace the text of one table cell. Spec 7.7, 11.2.
#[wasm_bindgen(js_name = setCell)]
pub fn set_cell(
    handle: u32,
    page_index: usize,
    table: usize,
    row: usize,
    column: usize,
    text: &str,
) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let outcome =
            session.set_cell(&page, table, row, column, text).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Remove a page. Spec 11.2's `deletePage`.
#[wasm_bindgen(js_name = deletePage)]
pub fn delete_page(handle: u32, index: usize) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let outcome = session.delete_page(index).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Reorder a page. Spec 11.2's `movePage`.
#[wasm_bindgen(js_name = movePage)]
pub fn move_page(handle: u32, from: usize, to: usize) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let outcome = session.move_page(from, to).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// The annotations on a page. Spec 10.4.
#[wasm_bindgen(js_name = pageAnnotations)]
pub fn page_annotations(handle: u32, page_index: usize) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let list = session.annotations(&page).map_err(convert::from_error)?;
        Ok(convert::annotations(&list))
    })
}

/// Add an annotation, generating its appearance stream. Spec 10.4.
///
/// The `spec` object is `{ kind, rect, colour?, interior?, borderWidth?,
/// contents?, points? }` — see `convert::to_new_annotation`.
#[wasm_bindgen(js_name = addAnnotation)]
pub fn add_annotation(handle: u32, page_index: usize, spec: JsValue) -> Result<JsValue, JsValue> {
    let new = convert::to_new_annotation(&spec)?;
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let outcome = session.add_annotation(&page, &new).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Remove an annotation by the id `pageAnnotations` reported.
#[wasm_bindgen(js_name = deleteAnnotation)]
pub fn delete_annotation(handle: u32, page_index: usize, id: JsValue) -> Result<JsValue, JsValue> {
    let id = convert::to_obj_id(&id)?;
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let outcome = session.delete_annotation(&page, id).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// The document's form fields, by fully-qualified name. Spec 10.5.
#[wasm_bindgen(js_name = formFields)]
pub fn form_fields(handle: u32) -> Result<JsValue, JsValue> {
    with(handle, |doc| Ok(convert::fields(&doc.form_fields())))
}

/// Fill a form field and regenerate its appearance. Spec 10.5.
#[wasm_bindgen(js_name = setFieldValue)]
pub fn set_field_value(handle: u32, name: &str, value: &str) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let outcome = session.set_field_value(name, value).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Burn a page's widget appearances into its content and drop the fields.
#[wasm_bindgen(js_name = flattenForms)]
pub fn flatten_forms(handle: u32, page_index: usize) -> Result<JsValue, JsValue> {
    with_session(handle, |session| {
        let page = session.page(page_index).map_err(convert::from_error)?;
        let outcome = session.flatten_forms(&page).map_err(convert::from_error)?;
        Ok(convert::outcome(&outcome))
    })
}

/// Remove every trace of a string from the document. Spec 9.6.
///
/// Not a session operation, and deliberately so: redaction forces a full
/// rewrite, which cannot be undone by restoring bytes the way a staged edit can.
/// Returns the strings actually found and removed, so a caller can tell
/// "redacted three occurrences" from "found nothing".
/// llowIncomplete opts past the refusal an overlapping image otherwise
/// produces. Image data is not searched, so a scan of the same words survives
/// the removal; the flag makes that the caller's decision at the call site
/// rather than a field in a result nobody reads.
#[wasm_bindgen(js_name = redactText)]
pub fn redact_text(
    handle: u32,
    text: &str,
    allow_incomplete: Option<bool>,
) -> Result<JsValue, JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        let opts =
            rasura::redaction::Options { allow_incomplete: allow_incomplete.unwrap_or(false) };
        let removed = slot.doc.redact_with(text, &opts).map_err(convert::from_error)?;
        Ok(convert::strings(removed.into_iter()))
    })
}

/// Search saved bytes for text that should no longer be there. Spec 9.6.
///
/// Takes bytes rather than a handle because it verifies the *artefact*. Checking
/// the in-memory document would ask the thing that performed the redaction
/// whether the redaction worked.
#[wasm_bindgen(js_name = verifyRedaction)]
pub fn verify_redaction(bytes: &[u8], strings: Vec<String>) -> JsValue {
    convert::redaction_report(&rasura::Document::verify_redaction(bytes, &strings))
}

/// Encrypt the document, or change its password. Spec 5.
///
/// `entropy` is 32 caller-supplied random bytes. This crate has no RNG and will
/// not grow one — the JS wrapper passes `crypto.getRandomValues(new
/// Uint8Array(32))`, which is a better source than anything a WASM module could
/// bundle, and keeps the key material's provenance visible at the call site.
/// Returns the weaknesses of the policy that was applied.
#[wasm_bindgen(js_name = protectDocument)]
pub fn protect_document(handle: u32, policy: JsValue, entropy: &[u8]) -> Result<JsValue, JsValue> {
    let policy = convert::to_policy(&policy)?;
    let entropy: [u8; 32] =
        entropy.try_into().map_err(|_| err("internal", "entropy must be exactly 32 bytes"))?;

    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        let weaknesses = slot.doc.protect(&policy, entropy).map_err(convert::from_error)?;
        Ok(convert::weaknesses(&weaknesses))
    })
}

/// Remove the document's encryption. Spec 5.
#[wasm_bindgen(js_name = unprotectDocument)]
pub fn unprotect_document(handle: u32) -> Result<(), JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        slot.doc.unprotect().map_err(convert::from_error)
    })
}

/// Drop unused glyphs from every embedded font. Spec 8.6.
///
/// Returns the number of fonts compacted. Worth doing after heavy editing, and
/// pointless before it: a font compacted down to the glyphs the page uses has
/// nothing spare left for the next insertion.
#[wasm_bindgen(js_name = compactFonts)]
pub fn compact_fonts(handle: u32) -> Result<usize, JsValue> {
    OPEN.with(|r| {
        let mut registry = r.borrow_mut();
        let slot = registry.documents.get_mut(&handle).ok_or_else(|| missing(handle))?;
        slot.doc.compact_fonts().map_err(convert::from_error)
    })
}

/// Undo the last staged operation. Invariant I5.
#[wasm_bindgen(js_name = undo)]
pub fn undo_session(handle: u32) -> Result<bool, JsValue> {
    with_session(handle, |session| session.undo().map_err(convert::from_error))
}

/// Redo the most recently undone operation.
#[wasm_bindgen(js_name = redo)]
pub fn redo_session(handle: u32) -> Result<bool, JsValue> {
    with_session(handle, |session| session.redo().map_err(convert::from_error))
}

/// What is staged, without changing anything.
#[wasm_bindgen(js_name = sessionStatus)]
pub fn session_status(handle: u32) -> Result<JsValue, JsValue> {
    OPEN.with(|r| {
        let registry = r.borrow();
        let slot = registry.documents.get(&handle).ok_or_else(|| missing(handle))?;
        let idle = rasura::SessionState::default();
        Ok(convert::session_state(slot.session.as_ref().unwrap_or(&idle)))
    })
}

/// Undo everything staged and close the session. Spec 9.1.
#[wasm_bindgen(js_name = rollbackSession)]
pub fn rollback_session(handle: u32) -> Result<(), JsValue> {
    with_session(handle, |session| session.rollback().map_err(convert::from_error))
}

/// Apply everything staged and write the document out. Spec 9.5.
#[wasm_bindgen(js_name = commitSession)]
pub fn commit_session(handle: u32, full_rewrite: Option<bool>) -> Result<JsValue, JsValue> {
    let saved = with_session(handle, |session| {
        let opts = SaveOptions {
            mode: match full_rewrite {
                Some(true) => Some(rasura::SaveMode::FullRewrite),
                Some(false) => Some(rasura::SaveMode::Incremental),
                None => None,
            },
            accept_signature_destruction: false,
        };
        session.commit(&opts).map_err(convert::from_error)
    })?;

    let out = obj();
    set(&out, "bytes", &convert::bytes(&saved.bytes));
    set_str(&out, "mode", convert::save_mode(saved.mode));
    set_num(&out, "bytesAppended", saved.bytes_appended as f64);
    set(&out, "warnings", &convert::strings(saved.warnings.iter().map(|w| w.to_string())));
    Ok(out.into())
}

/// Write the document out unchanged, or with whatever has been staged.
#[wasm_bindgen(js_name = saveDocument)]
pub fn save_document(handle: u32, full_rewrite: Option<bool>) -> Result<JsValue, JsValue> {
    with(handle, |doc| {
        let opts = SaveOptions {
            mode: match full_rewrite {
                Some(true) => Some(rasura::SaveMode::FullRewrite),
                Some(false) => Some(rasura::SaveMode::Incremental),
                None => None,
            },
            accept_signature_destruction: false,
        };
        let saved = doc.save(&opts).map_err(convert::from_error)?;

        let out = obj();
        set(&out, "bytes", &convert::bytes(&saved.bytes));
        set_str(&out, "mode", convert::save_mode(saved.mode));
        set_num(&out, "bytesAppended", saved.bytes_appended as f64);
        set(&out, "warnings", &convert::strings(saved.warnings.iter().map(|w| w.to_string())));
        Ok(out.into())
    })
}

/// The library's version, so a page can report which build it loaded.
#[wasm_bindgen(js_name = version)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Whether this build can use threads. Spec 12.1.
///
/// Always false here. `rasura/threaded` is a separate artefact with
/// different build flags, and asking the *default* build whether it is threaded
/// has one honest answer. A caller feature-detects by loading that entry point,
/// not by asking this one to lie.
#[wasm_bindgen(js_name = isThreaded)]
pub fn is_threaded() -> bool {
    false
}

/// These run under `wasm-bindgen-test`, not `cargo test`.
///
/// Everything on this surface returns a `JsValue`, and constructing one calls
/// into JS — which traps on a host target with "cannot call wasm-bindgen
/// imported functions on non-wasm targets". So there is no host-side test of
/// this crate to write, and pretending otherwise would mean testing a shim
/// rather than the thing that ships.
///
/// ```text
/// cargo install wasm-bindgen-cli
/// cargo test -p rasura-wasm --target wasm32-unknown-unknown
/// ```
///
/// The end-to-end check that matters is separate and stronger: `build.sh`
/// compiles the real artefact and `harness/wasm-size/api.mjs` drives it from
/// node against a real PDF. A binding that compiles and cannot open a file
/// passes every test in here and fails that one.
#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A minimal document. Built here rather than shipped, so the size-budgeted
    /// artefact carries no fixtures.
    fn simple_pdf() -> Vec<u8> {
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
            .finish("/Root 1 0 R")
    }

    #[wasm_bindgen_test]
    fn a_handle_is_invalidated_by_closing_and_never_reused() {
        // A reused handle would let a stale reference reach somebody else's
        // document, which is the one failure mode a handle table must not have.
        let first = open_document(simple_pdf(), None, None).expect("open");
        assert!(close_document(first));
        assert!(!close_document(first), "closing twice is visible and harmless");

        let second = open_document(simple_pdf(), None, None).expect("open");
        assert_ne!(first, second);
        assert!(document_info(first).is_err(), "the closed handle stays dead");
        assert!(document_info(second).is_ok());
        close_document(second);
    }

    #[wasm_bindgen_test]
    fn a_stale_handle_is_a_coded_error_not_a_panic() {
        let e = document_info(9999).expect_err("no such document");
        assert_eq!(convert::code_of(&e).as_deref(), Some("stale-session"));
    }

    #[wasm_bindgen_test]
    fn opening_something_that_is_not_a_pdf_carries_its_code() {
        let e = open_document(b"not a pdf at all".to_vec(), None, None).expect_err("not a pdf");
        assert_eq!(convert::code_of(&e).as_deref(), Some("malformed"));
    }
}
