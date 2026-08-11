//! Serialisation. Spec 5.6.
//!
//! # `SaveMode::Incremental` (default)
//!
//! ```text
//! <original file bytes, unmodified>
//! <updated and new indirect objects>
//! <xref section covering only changed objects>
//! <trailer with /Prev -> previous startxref>
//! startxref
//! %%EOF
//! ```
//!
//! The original bytes are copied, never regenerated. When nothing changed there
//! is nothing to append, and `save()` returns the input verbatim -- that is
//! invariant I1, and it holds by construction rather than by careful
//! re-serialisation.
//!
//! The cross-reference style of the original is reproduced: a file written with
//! xref streams gets an xref stream, a classic file gets a classic table. The
//! format is never "upgraded", because upgrading changes how every downstream
//! tool sees the file for no benefit the caller asked for.
//!
//! # `SaveMode::FullRewrite`
//!
//! Serialises the whole document fresh, dropping unreferenced objects. Forced
//! for documents opened in recovery mode -- appending onto a cross-reference
//! table you had to guess is not safe, and that is enforced here rather than
//! documented and hoped for.

use crate::crypt::Cipher;
use crate::document::{Document, LoadMode, ProtectionChange};
use crate::error::{CosError, Result, Warning};
use crate::object::{Dictionary, Name, ObjId, Object, PdfString, Stream, format_real};
use crate::xref::{XrefEntry, XrefStyle};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Append a revision, leaving every original byte in place.
    Incremental,
    /// Rewrite from scratch. Compacts and drops unreferenced objects.
    /// Invalidates byte identity by design, so callers must ask for it.
    FullRewrite,
}

#[derive(Debug, Clone, Default)]
pub struct SaveOptions {
    /// `None` means "incremental unless the document forces otherwise".
    pub mode: Option<SaveMode>,
    /// Acknowledge that a full rewrite destroys an existing signature.
    /// Without this, saving such a document fails rather than silently
    /// invalidating it.
    pub accept_signature_destruction: bool,
}

impl SaveOptions {
    pub fn incremental() -> Self {
        SaveOptions { mode: Some(SaveMode::Incremental), ..Default::default() }
    }

    pub fn full_rewrite() -> Self {
        SaveOptions { mode: Some(SaveMode::FullRewrite), ..Default::default() }
    }
}

/// What a save did. Spec 9.5.
#[derive(Debug, Clone)]
pub struct SaveResult {
    pub bytes: Vec<u8>,
    pub mode: SaveMode,
    pub bytes_appended: usize,
    pub objects_written: usize,
    pub warnings: Vec<Warning>,
}

/// What saving does to any signature the document carries. Spec 9.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureImpact {
    /// No `/Sig` field with a `/ByteRange` was found.
    None,
    /// An incremental append leaves the signed byte range intact. A validator
    /// reports "signed version available, document modified afterwards", which
    /// is the correct and honest outcome.
    PriorRevisionPreserved,
    /// A full rewrite destroys the signature irrecoverably.
    Destroyed,
}

/// Report the signature consequence of a save *before* performing it.
pub fn signature_impact(doc: &Document, mode: SaveMode) -> SignatureImpact {
    if !document_has_signature(doc) {
        return SignatureImpact::None;
    }
    match mode {
        SaveMode::Incremental => SignatureImpact::PriorRevisionPreserved,
        SaveMode::FullRewrite => SignatureImpact::Destroyed,
    }
}

fn document_has_signature(doc: &Document) -> bool {
    let Ok(catalog) = doc.catalog() else { return false };
    let Some(dict) = catalog.as_dict() else { return false };
    let Ok(Some(acroform)) = doc.get_entry(dict, "AcroForm") else { return false };
    let Some(form) = acroform.as_dict() else { return false };
    let Ok(Some(fields)) = doc.get_entry(form, "Fields") else { return false };
    let Some(items) = fields.as_array() else { return false };
    items.iter().any(|f| {
        doc.resolve(f)
            .ok()
            .and_then(|o| o.as_dict().and_then(|d| d.get("FT").and_then(Object::as_name).cloned()))
            .is_some_and(|ft| ft.as_bytes() == b"Sig")
    })
}

/// Which mode a save will actually use, given what the caller asked for and
/// what the document allows.
pub fn effective_mode(doc: &Document, opts: &SaveOptions) -> SaveMode {
    match opts.mode {
        // Spec 10.6 step 7: redaction forces a full rewrite, "non-negotiable
        // and must be enforced in code, not documentation". An incremental
        // append leaves the original bytes in the file, so a redacted document
        // saved incrementally still contains what was redacted -- the removal
        // would be cosmetic, and cosmetic is the exact failure the whole
        // section exists to prevent.
        //
        // Checked before the caller's own request rather than after, so that
        // asking for `Incremental` explicitly cannot defeat it.
        _ if doc.is_redacted() => SaveMode::FullRewrite,
        // Spec 5.5, Phase 8. Checked in the same place and for a related but
        // distinct reason: an incremental append leaves every prior object
        // encrypted under the *old* key, or under none, and a reader has only
        // one file key for the whole document. Adding protection incrementally
        // does not make a weakly protected file; it makes an unreadable one.
        _ if doc.protection_change().is_change() => SaveMode::FullRewrite,
        Some(SaveMode::Incremental) | None if doc.load_mode() == LoadMode::Reconstructed => {
            // Spec 5.3: recovery forces a full rewrite.
            SaveMode::FullRewrite
        }
        Some(m) => m,
        None => SaveMode::Incremental,
    }
}

pub fn save(doc: &Document, opts: &SaveOptions) -> Result<SaveResult> {
    let mode = effective_mode(doc, opts);
    // A redacted document that would not be fully rewritten is a bug in this
    // function, not a caller error, and it would ship the redacted bytes. The
    // assertion is cheap and the failure it guards against is unrecoverable.
    debug_assert!(
        !doc.is_redacted() || mode == SaveMode::FullRewrite,
        "spec 10.6 step 7: a redacted document must be fully rewritten"
    );
    debug_assert!(
        !doc.protection_change().is_change() || mode == SaveMode::FullRewrite,
        "spec 5.5: a protection change must be fully rewritten"
    );
    if mode == SaveMode::FullRewrite
        && signature_impact(doc, mode) == SignatureImpact::Destroyed
        && !opts.accept_signature_destruction
    {
        return Err(CosError::Internal(
            "a full rewrite would destroy this document's signature; \
             set accept_signature_destruction to proceed"
                .into(),
        ));
    }
    match mode {
        SaveMode::Incremental => save_incremental(doc),
        SaveMode::FullRewrite => save_full(doc),
    }
}

// ---------------------------------------------------------------------------
// Incremental
// ---------------------------------------------------------------------------

fn save_incremental(doc: &Document) -> Result<SaveResult> {
    let original = doc.bytes();

    // Invariant I1. Nothing changed, so there is nothing to append: the output
    // is the input, byte for byte.
    if !doc.is_dirty() {
        return Ok(SaveResult {
            bytes: original.to_vec(),
            mode: SaveMode::Incremental,
            bytes_appended: 0,
            objects_written: 0,
            warnings: Vec::new(),
        });
    }

    let mut warnings = Vec::new();
    if doc.is_linearized() {
        // Spec 5.6: do not attempt to re-linearise; say so instead.
        warnings.push(Warning::LinearizationBroken);
    }
    if doc.is_encrypted() {
        warnings.push(Warning::ReencryptedWithExistingKey);
    }

    let promoted = count_promoted_from_objstm(doc);
    if promoted > 0 {
        warnings.push(Warning::ObjectsPromotedFromObjStm { count: promoted });
    }

    let mut out = original.to_vec();
    // A revision must start on its own line so the appended bytes cannot run
    // into whatever the previous revision ended with.
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let append_start = out.len();

    // Emit the changed objects.
    let mut new_entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
    let mut objects_written = 0usize;
    for (id, object) in doc.dirty_objects() {
        let at = out.len();
        write_indirect_object(doc, &mut out, *id, object)?;
        new_entries.insert(id.number, XrefEntry::InFile { offset: at, generation: id.generation });
        objects_written += 1;
    }
    for id in doc.deleted_objects() {
        // A deleted object becomes a free entry whose generation is bumped, so
        // a later revision reusing the number cannot be confused with it.
        new_entries.insert(
            id.number,
            XrefEntry::Free { next_free: 0, generation: id.generation.saturating_add(1) },
        );
    }

    let prev_startxref = doc.revisions().first().map(|r| r.xref_offset).unwrap_or(0);
    let size = doc.next_number().max(doc.xref().trailer_size());

    let xref_at = match doc.xref_style() {
        XrefStyle::Classic => {
            let at = out.len();
            write_classic_section(&mut out, &new_entries);
            let trailer = build_trailer(doc, size, Some(prev_startxref), &out);
            out.extend_from_slice(b"trailer\n");
            write_object(&mut out, &Object::Dictionary(trailer));
            out.push(b'\n');
            at
        }
        XrefStyle::Stream => {
            // The xref stream is itself an object and must appear in its own
            // index, so its number is allocated before its offset is known.
            let stream_number = size;
            let at = out.len();
            new_entries.insert(stream_number, XrefEntry::InFile { offset: at, generation: 0 });
            let trailer = build_trailer(doc, stream_number + 1, Some(prev_startxref), &out);
            let stream = build_xref_stream(&new_entries, trailer)?;
            write_synthesised_object(
                doc,
                &mut out,
                ObjId::new(stream_number, 0),
                &Object::Stream(stream),
            )?;
            objects_written += 1;
            at
        }
    };

    out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());

    Ok(SaveResult {
        bytes_appended: out.len() - append_start,
        bytes: out,
        mode: SaveMode::Incremental,
        objects_written,
        warnings,
    })
}

/// Spec 5.6: an object living inside an `/ObjStm` that is modified is promoted
/// to a top-level indirect object. The original object stream is left alone.
fn count_promoted_from_objstm(doc: &Document) -> usize {
    doc.dirty_objects()
        .keys()
        .filter(|id| matches!(doc.xref().get(id.number), Some(XrefEntry::InObjStm { .. })))
        .count()
}

// ---------------------------------------------------------------------------
// Full rewrite
// ---------------------------------------------------------------------------

fn save_full(doc: &Document) -> Result<SaveResult> {
    let mut warnings = Vec::new();
    match doc.protection_change() {
        // Only true when the key is the one the file arrived with. Saying so on
        // a save that is *changing* the key would be exactly backwards.
        ProtectionChange::Unchanged if doc.is_encrypted() => {
            warnings.push(Warning::ReencryptedWithExistingKey);
        }
        ProtectionChange::Unchanged => {}
        ProtectionChange::Removed => warnings.push(Warning::ProtectionRemoved),
        ProtectionChange::Replaced { .. } => warnings.push(Warning::ProtectionReplaced),
    }

    // Only objects the catalog can reach survive. This is what makes a full
    // rewrite compact.
    let reachable = reachable_objects(doc);
    let all_live: BTreeSet<u32> =
        doc.xref().live_objects().chain(doc.dirty_objects().keys().map(|id| id.number)).collect();
    let dropped = all_live.len().saturating_sub(reachable.len());
    if dropped > 0 {
        warnings.push(Warning::UnreferencedObjectsDropped { count: dropped });
    }

    let mut out = Vec::with_capacity(doc.bytes().len());
    out.extend_from_slice(format!("%PDF-{}\n", doc.version()).as_bytes());
    out.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");

    let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
    let mut objects_written = 0usize;

    for id in &reachable {
        // A reachable object that will not load is a dangling reference in the
        // *input*. ISO 32000-1 §7.3.10 makes such a reference null, so the file
        // is legal; refusing to save it would mean a document that opens cannot
        // be written back out.
        let object = doc.get_or_null(*id);
        if object.is_null() {
            continue;
        }
        let at = out.len();
        // Objects that came from an object stream have no source span, so they
        // are serialised; everything else keeps its original bytes.
        write_indirect_object(doc, &mut out, *id, &object)?;
        entries.insert(id.number, XrefEntry::InFile { offset: at, generation: id.generation });
        objects_written += 1;
    }

    // The `/Encrypt` dictionary, when this save is creating protection.
    //
    // Written here rather than reached through `reachable_objects` because it
    // is not part of the document's object graph: nothing inside the document
    // refers to it, only the trailer does. Adding it to the graph would also
    // mean adding it to the dirty set, where an edit session's undo would find
    // it. It is emitted through the raw writer for the same reason a
    // cross-reference stream is -- a security handler must never encrypt its
    // own dictionary, and `write_object_with` would try.
    if let ProtectionChange::Replaced { id, dict, .. } = doc.protection_change() {
        let at = out.len();
        out.extend_from_slice(format!("{} {} obj\n", id.number, id.generation).as_bytes());
        write_object(&mut out, &Object::Dictionary(dict.clone()));
        out.extend_from_slice(b"\nendobj\n");
        entries.insert(id.number, XrefEntry::InFile { offset: at, generation: id.generation });
        objects_written += 1;
    }

    let size = entries.keys().next_back().map_or(1, |n| n + 1);
    let xref_at = match doc.xref_style() {
        XrefStyle::Classic => {
            let at = out.len();
            // A full table must describe object 0 as the head of the free list.
            entries.insert(0, XrefEntry::Free { next_free: 0, generation: 65535 });
            write_classic_section(&mut out, &entries);
            let trailer = build_trailer(doc, size, None, &out);
            out.extend_from_slice(b"trailer\n");
            write_object(&mut out, &Object::Dictionary(trailer));
            out.push(b'\n');
            at
        }
        XrefStyle::Stream => {
            let stream_number = size;
            let at = out.len();
            entries.insert(0, XrefEntry::Free { next_free: 0, generation: 65535 });
            entries.insert(stream_number, XrefEntry::InFile { offset: at, generation: 0 });
            let trailer = build_trailer(doc, stream_number + 1, None, &out);
            let stream = build_xref_stream(&entries, trailer)?;
            write_synthesised_object(
                doc,
                &mut out,
                ObjId::new(stream_number, 0),
                &Object::Stream(stream),
            )?;
            objects_written += 1;
            at
        }
    };

    out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    Ok(SaveResult {
        bytes_appended: out.len(),
        bytes: out,
        mode: SaveMode::FullRewrite,
        objects_written,
        warnings,
    })
}

/// Everything reachable from `/Root`, `/Info` and `/Encrypt`.
fn reachable_objects(doc: &Document) -> BTreeSet<ObjId> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<ObjId> = Vec::new();

    for key in ["Root", "Info", "Encrypt"] {
        if let Some(Object::Reference(id)) = doc.trailer().get(key) {
            stack.push(*id);
        }
    }

    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(object) = doc.get(id) else { continue };
        collect_references(&object, &mut stack);
    }
    seen
}

fn collect_references(object: &Object, out: &mut Vec<ObjId>) {
    match object {
        Object::Reference(id) => out.push(*id),
        Object::Array(items) => {
            for i in items {
                collect_references(i, out);
            }
        }
        Object::Dictionary(d) => {
            for (_, v) in d.iter() {
                collect_references(v, out);
            }
        }
        Object::Stream(s) => {
            for (_, v) in s.dict.iter() {
                collect_references(v, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Trailer and cross-reference emission
// ---------------------------------------------------------------------------

fn build_trailer(
    doc: &Document,
    size: u32,
    prev: Option<usize>,
    written_so_far: &[u8],
) -> Dictionary {
    let mut trailer = Dictionary::new();
    trailer.insert(Name::new("Size"), Object::Integer(size as i64));

    // Spec 5.6: preserve /Root, /Info and /Encrypt references unless changed.
    for key in ["Root", "Info", "Encrypt"] {
        if let Some(v) = doc.trailer().get(key) {
            trailer.insert(Name::new(key), v.clone());
        }
    }

    // Spec 5.5: and /Encrypt *has* changed if protection was added, replaced or
    // removed. Overwriting after the loop rather than filtering inside it keeps
    // the "preserve unless changed" rule in one readable piece.
    match doc.protection_change() {
        ProtectionChange::Unchanged => {}
        ProtectionChange::Removed => {
            trailer.remove("Encrypt");
        }
        ProtectionChange::Replaced { id, .. } => {
            trailer.insert(Name::new("Encrypt"), Object::Reference(*id));
        }
    }

    // /ID[0] identifies the document across its whole life and is preserved.
    // /ID[1] identifies this particular revision and is regenerated.
    let id0 = doc
        .trailer()
        .get("ID")
        .and_then(Object::as_array)
        .and_then(|a| a.first())
        .and_then(Object::as_string)
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_else(|| derive_id(&[], written_so_far));
    let id1 = derive_id(&id0, written_so_far);
    trailer.insert(
        Name::new("ID"),
        Object::Array(vec![
            Object::String(PdfString::new_hex(&id0)),
            Object::String(PdfString::new_hex(&id1)),
        ]),
    );

    if let Some(p) = prev {
        trailer.insert(Name::new("Prev"), Object::Integer(p as i64));
    }
    trailer
}

/// There is no RNG in this crate, and `wasm32-unknown-unknown` has none by
/// default. Deriving `/ID[1]` from the revision's own content gives a value that
/// is unique per distinct output -- which is exactly what the identifier is for
/// -- and has the side benefit that saving the same edits twice produces
/// identical bytes, so the test suite can assert on output.
fn derive_id(seed: &[u8], content: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(seed);
    h.update((content.len() as u64).to_be_bytes());
    // Hash a bounded window rather than the whole file: on a 200 MB document
    // the tail is what changed, and hashing all of it would dominate save time.
    let tail_from = content.len().saturating_sub(64 * 1024);
    h.update(&content[tail_from..]);
    h.finalize()[..16].to_vec()
}

fn write_classic_section(out: &mut Vec<u8>, entries: &BTreeMap<u32, XrefEntry>) {
    out.extend_from_slice(b"xref\n");
    let numbers: Vec<u32> = entries.keys().copied().collect();
    let mut i = 0usize;
    while i < numbers.len() {
        let mut j = i;
        while j + 1 < numbers.len() && numbers[j + 1] == numbers[j] + 1 {
            j += 1;
        }
        out.extend_from_slice(format!("{} {}\n", numbers[i], j - i + 1).as_bytes());
        for &n in &numbers[i..=j] {
            match entries[&n] {
                XrefEntry::InFile { offset, generation } => {
                    // Exactly 20 bytes per entry, as ISO 32000-1 §7.5.4 requires.
                    out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
                }
                XrefEntry::Free { next_free, generation } => {
                    out.extend_from_slice(
                        format!("{next_free:010} {generation:05} f \n").as_bytes(),
                    );
                }
                XrefEntry::InObjStm { .. } => {
                    // Cannot be expressed in a classic table. The object was
                    // promoted to top level before reaching here.
                    out.extend_from_slice(b"0000000000 65535 f \n");
                }
            }
        }
        i = j + 1;
    }
}

fn build_xref_stream(
    entries: &BTreeMap<u32, XrefEntry>,
    mut trailer: Dictionary,
) -> Result<Stream> {
    // /W [1 4 2] covers every file below 4 GB, which is every file.
    let mut data = Vec::with_capacity(entries.len() * 7);
    let mut index: Vec<Object> = Vec::new();

    let numbers: Vec<u32> = entries.keys().copied().collect();
    let mut i = 0usize;
    while i < numbers.len() {
        let mut j = i;
        while j + 1 < numbers.len() && numbers[j + 1] == numbers[j] + 1 {
            j += 1;
        }
        index.push(Object::Integer(numbers[i] as i64));
        index.push(Object::Integer((j - i + 1) as i64));
        for &n in &numbers[i..=j] {
            let (kind, f2, f3): (u8, u32, u16) = match entries[&n] {
                XrefEntry::Free { next_free, generation } => (0, next_free, generation),
                XrefEntry::InFile { offset, generation } => (
                    1,
                    u32::try_from(offset).map_err(|_| {
                        CosError::Internal("file exceeds the 4 GB xref-stream limit".into())
                    })?,
                    generation,
                ),
                XrefEntry::InObjStm { container, index } => {
                    (2, container, u16::try_from(index).unwrap_or(u16::MAX))
                }
            };
            data.push(kind);
            data.extend_from_slice(&f2.to_be_bytes());
            data.extend_from_slice(&f3.to_be_bytes());
        }
        i = j + 1;
    }

    // A cross-reference stream is never encrypted, so it is safe to compress.
    let chain = crate::filters::FilterChain::build(Some(&Object::name("FlateDecode")), None);
    let encoded = crate::filters::encode(&chain, &data, 1)?;

    trailer.insert(Name::new("Type"), Object::name("XRef"));
    trailer.insert(
        Name::new("W"),
        Object::Array(vec![Object::Integer(1), Object::Integer(4), Object::Integer(2)]),
    );
    trailer.insert(Name::new("Index"), Object::Array(index));
    trailer.insert(Name::new("Filter"), Object::name("FlateDecode"));
    trailer.insert(Name::new("Length"), Object::Integer(encoded.len() as i64));

    // Reorder so /Type leads, which is what every producer emits and what makes
    // the stream readable in a hex dump.
    let mut ordered = Dictionary::new();
    for key in ["Type", "Size", "Index", "W", "Root", "Info", "Encrypt", "ID", "Prev"] {
        if let Some(v) = trailer.get(key) {
            ordered.insert(Name::new(key), v.clone());
        }
    }
    for key in ["Filter", "Length"] {
        if let Some(v) = trailer.get(key) {
            ordered.insert(Name::new(key), v.clone());
        }
    }
    Ok(Stream::new(ordered, encoded))
}

// ---------------------------------------------------------------------------
// Object serialisation
// ---------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
enum Provenance {
    /// The object came from the input file and may be replayed verbatim.
    FromInput,
    /// The writer built this object. Its number is freshly allocated and can
    /// legitimately collide with an input object that is not being written, so
    /// the input's bytes must never be consulted for it.
    Synthesised,
}

/// Write `N G obj ... endobj` for an object that came from the input.
///
/// When the object is unmodified, its original bytes are replayed rather than
/// regenerated. That is what keeps a full rewrite from churning the formatting
/// of every object nobody touched.
fn write_indirect_object(
    doc: &Document,
    out: &mut Vec<u8>,
    id: ObjId,
    object: &Object,
) -> Result<()> {
    write_object_with(doc, out, id, object, Provenance::FromInput)
}

/// Write an object this writer built, such as a cross-reference stream.
///
/// Replaying a source span here would emit some unrelated input object under
/// the new object's number -- a corruption with no local symptom, which only
/// shows up as a file that will not reopen.
fn write_synthesised_object(
    doc: &Document,
    out: &mut Vec<u8>,
    id: ObjId,
    object: &Object,
) -> Result<()> {
    write_object_with(doc, out, id, object, Provenance::Synthesised)
}

fn write_object_with(
    doc: &Document,
    out: &mut Vec<u8>,
    id: ObjId,
    object: &Object,
    provenance: Provenance,
) -> Result<()> {
    let is_dirty = doc.dirty_objects().contains_key(&id);
    // The verbatim path copies the object's original bytes, which are only
    // still correct if nothing about how they are encoded has changed. A
    // protection change is exactly such a change and is invisible in the bytes:
    // after `unprotect` the source bytes are ciphertext and the file will claim
    // to have none, which reads as a corrupt document rather than an insecure
    // one.
    if provenance == Provenance::FromInput
        && !is_dirty
        && !doc.is_encrypted()
        && !doc.protection_change().is_change()
        && let Some(span) = doc.source_span(id)
        && span.end <= doc.bytes().len()
    {
        out.extend_from_slice(&doc.bytes()[span]);
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        return Ok(());
    }

    out.extend_from_slice(format!("{} {} obj\n", id.number, id.generation).as_bytes());
    match object {
        Object::Stream(stream) => write_stream(doc, out, id, stream)?,
        other => {
            let encrypted;
            let to_write = if doc.output_decryptor().is_some() {
                encrypted = encrypt_strings(doc, id, other)?;
                &encrypted
            } else {
                other
            };
            write_object(out, to_write);
        }
    }
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

/// Whether the security handler applies to this stream on the way out.
///
/// Mirrors `Document::stream_is_encrypted` on the read side, against the
/// *output* handler — which is a different one whenever protection is being
/// added, removed or replaced. The asymmetry is worth stating because getting
/// it wrong is silent: a cross-reference stream must be readable *before* the
/// file key exists, so it is never encrypted -- and when the trailer lives
/// inside one, its `/ID` strings go with it. Encrypting those changes the input
/// to the key derivation, and the file then rejects its own password.
fn stream_takes_encryption(doc: &Document, id: ObjId, stream: &Stream) -> bool {
    let Some(dec) = doc.output_decryptor() else { return false };
    if dec.encrypt_ref == Some(id) {
        return false;
    }
    match stream.dict.type_name().map(|t| t.as_bytes().to_vec()).as_deref() {
        Some(b"XRef") => false,
        Some(b"Metadata") if !dec.encrypt_metadata => false,
        // An explicit /Crypt /Identity filter opts a stream out, on the way
        // back as on the way in.
        _ => {
            !matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_bytes() == b"Crypt")
        }
    }
}

fn write_stream(doc: &Document, out: &mut Vec<u8>, id: ObjId, stream: &Stream) -> Result<()> {
    let changing_protection = doc.protection_change().is_change();

    // Spec 5.4: content that did not change is re-emitted verbatim. Only a
    // stream whose decoded bytes were replaced is re-encoded, and then with the
    // same filter chain it arrived with.
    //
    // A protection change adds a third case. The raw bytes are ciphertext under
    // the key the file arrived with, so they cannot be copied through — but
    // they also do not need decompressing, because encryption sits *outside*
    // the filter chain. Peeling off one layer and putting the new one back
    // leaves the compressed bytes exactly as they were, which is both faster
    // than a re-encode and free of the filter drift one would introduce.
    let body = match stream.pending_decoded() {
        Some(decoded) => {
            let chain = doc.filter_chain(&stream.dict)?;
            let steps = chain.decodable_prefix();
            crate::filters::encode(&chain, decoded, steps)?
        }
        None if changing_protection && doc.stream_is_encrypted(id, &stream.dict) => {
            match doc.decryptor() {
                Some(dec) => dec.decrypt_stream(id, stream.raw())?,
                None => stream.raw().to_vec(),
            }
        }
        None => stream.raw().to_vec(),
    };

    let takes_encryption = stream_takes_encryption(doc, id, stream);

    // Everything on this path now holds plaintext: either it was replaced, or
    // it was just decrypted, or the document was never encrypted. Only the
    // untouched-stream-in-an-unchanged-document case is already ciphertext.
    let body = if takes_encryption && (stream.pending_decoded().is_some() || changing_protection) {
        match doc.output_decryptor() {
            Some(dec) if dec.stream_cipher() != Cipher::None => dec.encrypt_stream(id, &body)?,
            _ => body,
        }
    } else {
        body
    };

    let mut dict = stream.dict.clone();
    dict.insert(Name::new("Length"), Object::Integer(body.len() as i64));
    if takes_encryption {
        let d = encrypt_strings(doc, id, &Object::Dictionary(dict))?;
        dict = d.as_dict().cloned().unwrap_or_default();
    }

    write_object(out, &Object::Dictionary(dict));
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&body);
    out.extend_from_slice(b"\nendstream");
    Ok(())
}

/// Encrypt an object's strings with the handler the output will use.
///
/// The object handed in always holds *plaintext* strings: `Document::get`
/// decrypts them on the way out of the cache, and an object promoted from an
/// object stream was decrypted with its container. So this is an encryption
/// step, never a re-encryption, whether or not the key is changing.
fn encrypt_strings(doc: &Document, id: ObjId, object: &Object) -> Result<Object> {
    let Some(dec) = doc.output_decryptor() else { return Ok(object.clone()) };
    if dec.string_cipher() == Cipher::None || dec.encrypt_ref == Some(id) {
        return Ok(object.clone());
    }
    Ok(match object {
        Object::String(s) => {
            Object::String(PdfString::new_hex(dec.encrypt_string(id, s.as_bytes())?))
        }
        Object::Array(items) => Object::Array(
            items.iter().map(|i| encrypt_strings(doc, id, i)).collect::<Result<Vec<_>>>()?,
        ),
        Object::Dictionary(d) => {
            let mut out = Dictionary::new();
            for (k, v) in d.iter() {
                out.insert(k.clone(), encrypt_strings(doc, id, v)?);
            }
            Object::Dictionary(out)
        }
        other => other.clone(),
    })
}

/// Serialise a direct object.
///
/// Number formatting mirrors PDF conventions rather than Rust's: no exponent
/// notation, shortest round-tripping decimal. Spec 9.4 makes this a fidelity
/// requirement, because diffs are how users audit the library.
pub fn write_object(out: &mut Vec<u8>, object: &Object) {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Integer(v) => out.extend_from_slice(v.to_string().as_bytes()),
        Object::Real(v) => out.extend_from_slice(format_real(*v).as_bytes()),
        Object::String(s) => s.write_to(out),
        Object::Name(n) => n.write_to(out),
        Object::Reference(id) => {
            out.extend_from_slice(format!("{} {} R", id.number, id.generation).as_bytes());
        }
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(out, item);
            }
            out.push(b']');
        }
        Object::Dictionary(d) => write_dict(out, d),
        Object::Stream(s) => write_dict(out, &s.dict),
    }
}

fn write_dict(out: &mut Vec<u8>, d: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        out.push(b' ');
        k.write_to(out);
        // A name is self-delimiting, so `/Type/Page` needs no separator. The
        // space is for humans reading the diff.
        out.push(b' ');
        write_object(out, v);
    }
    out.extend_from_slice(b" >>");
}

/// Serialise a direct object to a fresh buffer.
pub fn object_to_bytes(object: &Object) -> Vec<u8> {
    let mut out = Vec::new();
    write_object(&mut out, object);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::parser::Parser;
    use crate::testutil;

    fn round_trip(object: &Object) -> Object {
        let bytes = object_to_bytes(object);
        Parser::new(&bytes)
            .parse_object()
            .unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(&bytes)))
    }

    #[test]
    fn every_object_kind_survives_serialisation() {
        let cases = vec![
            Object::Null,
            Object::Bool(true),
            Object::Bool(false),
            Object::Integer(0),
            Object::Integer(-42),
            Object::Integer(i64::MAX),
            Object::Real(0.5),
            Object::Real(-0.002),
            Object::Real(72.0),
            Object::name("Type"),
            Object::Name(Name::from_raw(b"A#20B")),
            Object::String(PdfString::new_literal("plain")),
            Object::String(PdfString::new_literal("with (parens) and \\ backslash")),
            Object::String(PdfString::new_hex([0x00, 0xff, 0x10])),
            Object::Reference(ObjId::new(12, 3)),
            Object::Array(vec![Object::Integer(1), Object::name("X"), Object::Null]),
            Object::Array(vec![]),
            Object::Dictionary(testutil::dict(&[
                ("Type", Object::name("Page")),
                ("Count", Object::Integer(3)),
                ("Kids", Object::Array(vec![Object::Reference(ObjId::new(4, 0))])),
            ])),
            Object::Dictionary(Dictionary::new()),
        ];
        for case in cases {
            let back = round_trip(&case);
            assert_eq!(
                object_to_bytes(&back),
                object_to_bytes(&case),
                "{case:?} did not survive a round trip"
            );
        }
    }

    #[test]
    fn names_and_strings_keep_their_original_encoding() {
        // The producer's choices survive, so an untouched value in a modified
        // dictionary does not churn.
        let n = Name::from_raw(b"Weird#20Name");
        let s = PdfString::from_raw_literal(br"octal \101 and \n escape");
        let d =
            Object::Dictionary(testutil::dict(&[("K", Object::Name(n)), ("S", Object::String(s))]));
        assert_eq!(object_to_bytes(&round_trip(&d)), object_to_bytes(&d));
        assert!(String::from_utf8_lossy(&object_to_bytes(&d)).contains("Weird#20Name"));
        assert!(String::from_utf8_lossy(&object_to_bytes(&d)).contains(r"\101"));
    }

    // --- Invariant I1 ------------------------------------------------------

    #[test]
    fn i1_no_op_save_is_byte_identical_classic() {
        let original = testutil::minimal_classic();
        let doc = Document::open(original.clone()).unwrap();
        let result = save(&doc, &SaveOptions::default()).unwrap();
        assert_eq!(result.bytes, original);
        assert_eq!(result.bytes_appended, 0);
    }

    #[test]
    fn i1_no_op_save_is_byte_identical_xref_stream() {
        let original = testutil::xref_stream_with_objstm();
        let doc = Document::open(original.clone()).unwrap();
        assert_eq!(save(&doc, &SaveOptions::default()).unwrap().bytes, original);
    }

    #[test]
    fn i1_holds_after_reading_everything() {
        // Reading must not perturb the document. Decoding streams, walking the
        // page tree and resolving references are all read-only operations.
        let original = testutil::classic_with_flate_content();
        let doc = Document::open(original.clone()).unwrap();
        for n in doc.xref().live_objects().collect::<Vec<_>>() {
            let obj = doc.get(ObjId::new(n, 0)).unwrap();
            if obj.as_stream().is_some() {
                let _ = doc.decoded_stream(ObjId::new(n, 0)).unwrap();
            }
        }
        assert_eq!(save(&doc, &SaveOptions::default()).unwrap().bytes, original);
    }

    // --- Incremental append -------------------------------------------------

    #[test]
    fn an_edit_appends_and_leaves_the_original_bytes_alone() {
        let original = testutil::minimal_classic();
        let mut doc = Document::open(original.clone()).unwrap();

        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let mut d = page.as_dict().unwrap().clone();
        d.insert(Name::new("Rotate"), Object::Integer(90));
        doc.set(ObjId::new(3, 0), Object::Dictionary(d));

        let result = save(&doc, &SaveOptions::default()).unwrap();
        assert_eq!(result.mode, SaveMode::Incremental);
        assert_eq!(result.objects_written, 1);
        assert!(
            result.bytes.starts_with(&original),
            "an incremental save must not touch a single original byte"
        );
        assert!(result.bytes_appended > 0);

        // And the appended revision is readable.
        let reopened = Document::open(result.bytes).unwrap();
        assert_eq!(reopened.revisions().len(), 2);
        let page = reopened.get(ObjId::new(3, 0)).unwrap();
        assert_eq!(page.as_dict().unwrap().get("Rotate").unwrap().as_i64(), Some(90));
        // Untouched objects still resolve, through the /Prev chain.
        assert!(reopened.catalog().is_ok());
    }

    #[test]
    fn incremental_save_reproduces_the_xref_stream_style() {
        let mut doc = Document::open(testutil::xref_stream_with_objstm()).unwrap();
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let mut d = page.as_dict().unwrap().clone();
        d.insert(Name::new("Rotate"), Object::Integer(180));
        doc.set(ObjId::new(3, 0), Object::Dictionary(d));

        let result = save(&doc, &SaveOptions::default()).unwrap();
        // Spec 5.6: the format is reproduced, not upgraded.
        assert!(
            !result.bytes[doc.bytes().len()..].starts_with(b"xref"),
            "a stream-xref file must not gain a classic table"
        );
        let reopened = Document::open(result.bytes).unwrap();
        assert_eq!(reopened.xref_style(), XrefStyle::Stream);
        assert_eq!(
            reopened
                .get(ObjId::new(3, 0))
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Rotate")
                .unwrap()
                .as_i64(),
            Some(180)
        );
    }

    #[test]
    fn editing_an_object_stream_member_promotes_it() {
        let mut doc = Document::open(testutil::xref_stream_with_objstm()).unwrap();
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let mut d = page.as_dict().unwrap().clone();
        d.insert(Name::new("Rotate"), Object::Integer(270));
        doc.set(ObjId::new(3, 0), Object::Dictionary(d));

        let result = save(&doc, &SaveOptions::default()).unwrap();
        assert!(
            result
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::ObjectsPromotedFromObjStm { count: 1 })),
            "promotion out of an object stream must be reported: {:?}",
            result.warnings
        );
        // The original object stream is untouched.
        assert!(result.bytes.starts_with(doc.bytes()));

        let reopened = Document::open(result.bytes).unwrap();
        match reopened.xref().get(3) {
            Some(XrefEntry::InFile { .. }) => {}
            other => panic!("expected a top-level entry, got {other:?}"),
        }
    }

    #[test]
    fn id0_is_preserved_and_id1_is_regenerated() {
        let mut doc = Document::open(testutil::classic_with_flate_content()).unwrap();
        let before = doc.trailer().get("ID").unwrap().as_array().unwrap().to_vec();
        doc.set(ObjId::new(5, 0), Object::Integer(0));

        let result = save(&doc, &SaveOptions::default()).unwrap();
        let reopened = Document::open(result.bytes).unwrap();
        let after = reopened.trailer().get("ID").unwrap().as_array().unwrap().to_vec();

        assert_eq!(
            after[0].as_string().unwrap().as_bytes(),
            before[0].as_string().unwrap().as_bytes(),
            "/ID[0] identifies the document for life"
        );
        assert_ne!(
            after[1].as_string().unwrap().as_bytes(),
            before[1].as_string().unwrap().as_bytes(),
            "/ID[1] identifies the revision"
        );
    }

    #[test]
    fn deleting_an_object_writes_a_free_entry() {
        let mut doc = Document::open(testutil::classic_with_flate_content()).unwrap();
        doc.delete(ObjId::new(5, 0));
        let result = save(&doc, &SaveOptions::default()).unwrap();
        let reopened = Document::open(result.bytes).unwrap();
        assert!(matches!(reopened.xref().get(5), Some(XrefEntry::Free { generation: 1, .. })));
        assert!(reopened.get_or_null(ObjId::new(5, 0)).is_null());
    }

    // --- Full rewrite -------------------------------------------------------

    #[test]
    fn full_rewrite_produces_a_readable_file() {
        let doc = Document::open(testutil::classic_with_flate_content()).unwrap();
        let result = save(&doc, &SaveOptions::full_rewrite()).unwrap();
        assert_eq!(result.mode, SaveMode::FullRewrite);

        let reopened = Document::open(result.bytes).unwrap();
        assert!(reopened.catalog().is_ok());
        let content = reopened.decoded_stream(ObjId::new(4, 0)).unwrap();
        assert!(String::from_utf8_lossy(&content).contains("Hello"));
        assert!(reopened.leniencies().is_empty(), "{:?}", reopened.leniencies());
    }

    #[test]
    fn a_synthesised_xref_stream_is_never_served_from_the_input() {
        // Regression. A full rewrite drops unreachable objects, so the new
        // cross-reference stream's freshly allocated number can collide with an
        // input object that is not being written -- here object 4, the original
        // /ObjStm. Replaying that object's source span emitted the old object
        // stream under the xref stream's number: a file whose startxref points
        // at an /ObjStm, which reopens only through recovery.
        let doc = Document::open(testutil::xref_stream_with_objstm()).unwrap();
        let out = save(&doc, &SaveOptions::full_rewrite()).unwrap().bytes;

        let reopened = Document::open(out).unwrap();
        assert_eq!(
            reopened.load_mode(),
            LoadMode::Xref,
            "the rewritten file must open through its own table, not recovery: {:?}",
            reopened.leniencies()
        );
        assert_eq!(reopened.xref_style(), XrefStyle::Stream);
        assert!(reopened.leniencies().is_empty(), "{:?}", reopened.leniencies());
        assert!(reopened.catalog().is_ok());
    }

    #[test]
    fn full_rewrite_drops_unreferenced_objects() {
        let mut doc = Document::open(testutil::minimal_classic()).unwrap();
        doc.add(Object::String(PdfString::new_literal("nothing points at me")));
        let result = save(&doc, &SaveOptions::full_rewrite()).unwrap();
        assert!(
            result.warnings.iter().any(|w| matches!(w, Warning::UnreferencedObjectsDropped { .. })),
            "dropping objects is a real change and must be reported"
        );
        assert!(!String::from_utf8_lossy(&result.bytes).contains("nothing points at me"));
    }

    #[test]
    fn recovery_mode_forces_a_full_rewrite() {
        // Spec 5.3 requires this in code, not documentation.
        let mut bytes = testutil::minimal_classic();
        let s = String::from_utf8_lossy(&bytes).replace("startxref\n9", "startxref\n999999999\n%");
        bytes = s.into_bytes();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.load_mode(), LoadMode::Reconstructed);

        assert_eq!(effective_mode(&doc, &SaveOptions::incremental()), SaveMode::FullRewrite);
        let result = save(&doc, &SaveOptions::incremental()).unwrap();
        assert_eq!(result.mode, SaveMode::FullRewrite);
        assert!(Document::open(result.bytes).unwrap().catalog().is_ok());
    }

    #[test]
    fn a_full_rewrite_of_an_unsigned_document_needs_no_acknowledgement() {
        let doc = Document::open(testutil::minimal_classic()).unwrap();
        assert_eq!(signature_impact(&doc, SaveMode::FullRewrite), SignatureImpact::None);
        assert!(save(&doc, &SaveOptions::full_rewrite()).is_ok());
    }

    #[test]
    fn repeated_edit_and_save_cycles_stay_readable() {
        // Three appended revisions, each one readable and each preserving the
        // bytes of the one before.
        let mut bytes = testutil::minimal_classic();
        for angle in [90i64, 180, 270] {
            let previous = bytes.clone();
            let mut doc = Document::open(bytes).unwrap();
            let page = doc.get(ObjId::new(3, 0)).unwrap();
            let mut d = page.as_dict().unwrap().clone();
            d.insert(Name::new("Rotate"), Object::Integer(angle));
            doc.set(ObjId::new(3, 0), Object::Dictionary(d));
            bytes = save(&doc, &SaveOptions::default()).unwrap().bytes;
            assert!(bytes.starts_with(&previous));
        }
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.revisions().len(), 4);
        assert_eq!(
            doc.get(ObjId::new(3, 0)).unwrap().as_dict().unwrap().get("Rotate").unwrap().as_i64(),
            Some(270)
        );
    }
}
