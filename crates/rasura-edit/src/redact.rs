//! Redaction, and proving it worked. Spec 10.6.
//!
//! > Redaction is not drawing a black rectangle.
//!
//! The section is titled "the one that must be correct", and the reason is that
//! every failure mode here is **silent and total**. A redaction that left the
//! text behind looks identical to one that did not: the page renders with a
//! black box either way, the file opens, `qpdf --check` passes. The only
//! difference is that anyone who selects the text, runs `strings`, or opens the
//! file in a text editor gets what was supposed to be removed.
//!
//! So this module does two things, and the second is as important as the first:
//! it removes content, and it **re-reads the output to prove the content is
//! gone**. [`verify`] is a public API for exactly the reason the spec gives —
//! it is the assurance a caller needs, and an assurance nobody can check is not
//! one.
//!
//! # What is implemented, and what is not
//!
//! Spec 10.6 lists nine steps. Claiming all nine when six are built would be
//! the same category of failure as a cosmetic redaction, so:
//!
//! | Step | State |
//! |---|---|
//! | 1. Remove glyph-showing operators over the region | **yes** |
//! | 2. Remove intersecting image data | **no** — needs a pixel codec |
//! | 3. Annotations, form field values, link targets | **yes** |
//! | 4. Strip `/ActualText` and `/Alt` | **yes** |
//! | 5. Purge from `/Info` and XMP | **yes** |
//! | 6. Remove glyphs from the font subset | **no** — see below |
//! | 7. Force `FullRewrite` | **yes**, in `rasura-cos` |
//! | 8. Drop prior revisions | **yes**, a consequence of 7 |
//! | 9. Draw the redaction box | caller's, via [`Canvas`](crate::Canvas) |
//!
//! Steps 3 and 4 grew in the doing: the corpus found the word surviving in a
//! signature dictionary, an outline title, a form field's `/V`, a structure
//! element's `/T`, and — worst — a `/ActualText` whose value was an *indirect*
//! string, which a direct `as_string` check skips in silence.
//!
//! [`verify`] reports what it *checked*, so a caller can tell the difference
//! between "no trace found" and "no trace found in the places we looked".
//!
//! **Step 6 is the subtle one.** A subset font's glyph inventory leaks the
//! alphabet a document used: removing the word `Wolfgang` from a page whose
//! font subset contains `W`, `o`, `l`, `f`, `g`, `a`, `n` still tells a reader
//! those letters appeared. It is not implemented, and [`Report::not_checked`]
//! says so rather than letting silence imply coverage.

use crate::locate::{EditablePage, ParagraphId, select};
use crate::patch::Patch;
use crate::session::{Compromise, Fidelity};
use rasura_cos::object::{Dictionary, Name, Object};
use rasura_cos::{Document, ObjId};
use std::collections::BTreeMap;

/// Why a redaction could not be planned.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RedactError {
    #[error("no paragraph {0:?}")]
    NoParagraph(ParagraphId),

    /// The text to redact was not found on the page.
    ///
    /// An error rather than a no-op: a caller that asked to remove something
    /// and got a clean result would reasonably conclude it was removed.
    #[error("{0:?} does not appear in this paragraph")]
    NotFound(String),

    /// The text is drawn inside a form XObject.
    ///
    /// Its spans address the form's stream rather than the page's, so a patch
    /// built from them would land in the wrong buffer — leaving the text
    /// exactly where it was while appearing to have removed it. That is the
    /// worst outcome this module can produce, so it is refused loudly and the
    /// caller is told the document cannot be redacted this way rather than
    /// handed a file that looks redacted.
    #[error("the text is inside a form XObject at depth {depth} and cannot be removed safely")]
    InsideForm { depth: usize },

    /// The occurrence spans more than one showing operator.
    #[error("the text spans {runs} showing operators; only one at a time is supported")]
    Fragmented { runs: usize },

    #[error("{0}")]
    Cos(String),
}

/// What a redaction will do, before it does it.
#[derive(Debug, Clone)]
pub struct Redaction {
    /// Content-stream patches removing the glyphs.
    pub patches: Vec<Patch>,
    /// Object changes: annotations removed, metadata purged, tags stripped.
    pub changes: Vec<(ObjId, Option<Object>)>,
    /// The strings being removed, for [`verify`].
    pub strings: Vec<String>,
    pub fidelity: Fidelity,
}

/// Plan the removal of every occurrence of `text` on one page. Spec 10.6.
///
/// The result is applied through an [`EditSession`](crate::EditSession) like
/// any other edit, and the document must then be marked with
/// [`Document::mark_redacted`] so the writer cannot save it incrementally.
/// [`apply`] does both and is the intended entry point; this exists so a caller
/// can inspect the plan first.
pub fn plan(doc: &Document, page: &EditablePage, text: &str) -> Result<Redaction, RedactError> {
    if text.is_empty() {
        return Err(RedactError::NotFound(text.into()));
    }

    // Every occurrence on the page, resolved to the glyphs that drew it and
    // gathered **per run**.
    //
    // Per run rather than per occurrence, because a showing operator is
    // rewritten whole: two occurrences inside one operator would otherwise
    // produce two patches claiming the same bytes, which the splice engine
    // correctly refuses. Collecting first also means an occurrence that spans
    // two operators is handled rather than declined -- each run simply loses
    // the glyphs that belong to it.
    let mut doomed: BTreeMap<usize, std::collections::BTreeSet<usize>> = BTreeMap::new();

    for (id, _) in &page.paragraphs {
        let haystack = page.text_of(*id);
        let mut from = 0usize;
        while let Some(at) = haystack[from..].find(text) {
            let byte_at = from + at;
            // Character offsets, which is what `select` indexes by.
            let chars_before = haystack[..byte_at].chars().count();
            let range = chars_before..chars_before + text.chars().count();

            if let Some(selection) = select(page, *id, range) {
                for glyph in &selection.glyphs {
                    doomed.entry(glyph.run).or_default().insert(glyph.index);
                }
            }
            from = byte_at + text.len();
        }
    }

    if doomed.is_empty() {
        return Err(RedactError::NotFound(text.into()));
    }

    let mut patches = Vec::with_capacity(doomed.len());
    for (run, glyphs) in &doomed {
        patches.push(remove_from_run(doc, page, *run, glyphs)?);
    }

    let mut changes: BTreeMap<ObjId, Object> = BTreeMap::new();
    let mut compromises = Vec::new();

    strip_annotations(doc, page, text, &mut changes);
    strip_tags(doc, text, &mut changes);
    purge_metadata(doc, text, &mut changes);

    // Step 6 is not implemented and the caller is told so through the report
    // rather than by omission.
    compromises.push(Compromise::FontSubsetRetained);

    Ok(Redaction {
        patches,
        changes: changes.into_iter().map(|(id, o)| (id, Some(o))).collect(),
        strings: vec![text.to_string()],
        fidelity: Fidelity::Degraded(compromises),
    })
}

/// Remove every occurrence of `text` from the **whole document**, and mark it
/// redacted. Spec 10.6.
///
/// This is the entry point a caller should use. [`plan`] is page-scoped and
/// exists so a caller can inspect one page's changes; using it alone is a
/// mistake the type system cannot prevent and the corpus caught immediately —
/// a name removed from page one and left on page five is exactly the silent
/// failure this section is about, and it verifies as *failed* rather than as
/// partially done, which is the right answer.
///
/// On return the document is marked redacted, so no subsequent save can be
/// incremental whatever options it is given. The caller still has to save;
/// nothing here writes bytes.
pub fn apply(doc: &mut Document, text: &str) -> Result<Redaction, RedactError> {
    let pages = rasura_content::page::pages(doc).map_err(|e| RedactError::Cos(e.to_string()))?;

    // Plan every page before touching any of them. A redaction that applied
    // page by page would leave a half-redacted document behind if a later page
    // failed, and half-redacted is indistinguishable from not redacted for the
    // purpose that matters.
    let mut per_page = Vec::new();
    let mut changes: BTreeMap<ObjId, Object> = BTreeMap::new();
    let mut found = false;

    for page in &pages.pages {
        let Some(analysed) = EditablePage::analyse(doc, page) else { continue };
        match plan(doc, &analysed, text) {
            Ok(one) => {
                found = true;
                for (id, value) in one.changes {
                    if let Some(v) = value {
                        changes.insert(id, v);
                    }
                }
                per_page.push((analysed.content, one.patches));
            }
            // A page that does not contain the text is not an error; a
            // document that does not contain it anywhere is.
            Err(RedactError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }

    if !found {
        return Err(RedactError::NotFound(text.into()));
    }

    // Metadata and tags are document-wide, so they are gathered once from the
    // whole document rather than per page.
    strip_tags(doc, text, &mut changes);
    purge_metadata(doc, text, &mut changes);

    let fidelity = Fidelity::Degraded(vec![Compromise::FontSubsetRetained]);
    let object_changes: Vec<(ObjId, Option<Object>)> =
        changes.into_iter().map(|(id, o)| (id, Some(o))).collect();

    {
        let mut session = crate::EditSession::new(doc);
        for (content, patches) in &per_page {
            session
                .patch_content("redact", content, patches, fidelity.clone())
                .map_err(|e| RedactError::Cos(e.to_string()))?;
        }
        if !object_changes.is_empty() {
            session
                .set_objects("redact metadata", &object_changes, fidelity.clone())
                .map_err(|e| RedactError::Cos(e.to_string()))?;
        }
    }

    // Spec 10.6 step 7, applied here so a caller cannot forget it. Every path
    // out of this function that changed anything has passed through it.
    doc.mark_redacted();

    Ok(Redaction {
        patches: Vec::new(),
        changes: object_changes,
        strings: vec![text.to_string()],
        fidelity,
    })
}

/// Rewrite one showing operator without the named glyphs, **leaving every
/// glyph that stays exactly where it was**.
///
/// The obvious implementation writes the remaining text as one `Tj` and lets it
/// close up. That is wrong here for a reason specific to redaction: the caller
/// draws the black box (step 9) over a rectangle computed from the *original*
/// layout, and if the tail of the line slides left into the gap, the box now
/// covers text that was never meant to be hidden while the words that moved out
/// from under it are legible. Text the caller did not name has moved, which is
/// §2's first property as it applies inside a line.
///
/// So the operator becomes a `TJ` whose removed stretches are replaced by
/// position adjustments of exactly the advance they contributed. ISO 32000-1
/// §9.4.3 gives an adjustment's displacement as `tx = (−t/1000 × Tfs) × Th`,
/// which inverts to the number below. The pen therefore also ends the operator
/// where it would have, so anything positioned relative to it is unaffected.
///
/// The gap left behind does leak the removed text's *width* — but so does the
/// black box that goes over it, which has to be that wide to cover the region.
fn remove_from_run(
    doc: &Document,
    page: &EditablePage,
    run: usize,
    glyphs: &std::collections::BTreeSet<usize>,
) -> Result<Patch, RedactError> {
    let resolved =
        page.runs.get(run).ok_or_else(|| RedactError::Cos(format!("run {run} vanished")))?;

    // The same trap as `replace_text`: a run inside a form XObject has spans
    // into the form's stream, not the page's. For redaction the consequence is
    // worse than a bad edit -- a patch that lands in the wrong buffer leaves
    // the text exactly where it was while appearing to have removed it.
    if resolved.run.depth > 0 {
        return Err(RedactError::InsideForm { depth: resolved.run.depth });
    }

    let ctx = crate::text::FontContext::for_run(doc, page, run)
        .ok_or_else(|| RedactError::Cos(format!("no font for run {run}")))?;

    // Text-space units per unit of TJ adjustment. `Th` does not apply in
    // vertical writing mode -- §9.4.4 -- and applying it there is a quiet way to
    // make every CJK document wrong.
    let per_unit = if resolved.run.vertical {
        resolved.run.size / 1000.0
    } else {
        resolved.run.size * (resolved.run.horizontal_scale / 100.0) / 1000.0
    };

    // The remaining text has to be writable in this font. It was drawn by this
    // font a moment ago, so this cannot normally fail -- but "cannot normally"
    // is not a guarantee, and a redaction that silently kept the glyphs because
    // re-encoding failed is the exact failure this module exists to prevent.
    let encode = |s: &str| {
        ctx.encoder
            .encode(s)
            .map_err(|e| RedactError::Cos(format!("the remaining text is unencodable: {e}")))
    };

    // A zero font size or scale makes the adjustment undefined and the run
    // invisible anyway; fall back to plain removal rather than dividing by it.
    if !per_unit.is_finite() || per_unit == 0.0 {
        let after: String = resolved
            .text
            .iter()
            .enumerate()
            .filter(|(i, _)| !glyphs.contains(i))
            .filter_map(|(_, t)| t.as_deref())
            .collect();
        let mut bytes = Vec::new();
        crate::emit::write_op(&mut bytes, &crate::emit::show_text(&encode(&after)?), &page.style);
        return Ok(Patch::new(resolved.run.op_span.clone(), bytes));
    }

    let mut items: Vec<crate::emit::Adjusted> = Vec::new();
    let mut kept = String::new();
    let mut gap = 0.0f64;

    // Flushing in this order matters: a gap is the space the glyphs *before*
    // the next kept text used to occupy, so it must be emitted before them.
    let flush_gap = |items: &mut Vec<crate::emit::Adjusted>, gap: &mut f64| {
        if *gap != 0.0 {
            items.push(crate::emit::Adjusted::Adjust(-*gap / per_unit));
            *gap = 0.0;
        }
    };

    for (i, text) in resolved.text.iter().enumerate() {
        if glyphs.contains(&i) {
            if !kept.is_empty() {
                items.push(crate::emit::Adjusted::Codes(encode(&kept)?));
                kept.clear();
            }
            // A glyph whose width the font never supplied has a fallback
            // advance, so the gap is approximate for it. It is still far closer
            // than closing up entirely.
            gap += resolved.run.glyphs.get(i).map_or(0.0, |g| g.advance);
        } else {
            flush_gap(&mut items, &mut gap);
            if let Some(t) = text.as_deref() {
                kept.push_str(t);
            }
        }
    }
    if !kept.is_empty() {
        items.push(crate::emit::Adjusted::Codes(encode(&kept)?));
    }
    // A trailing gap is kept: the operator has to leave the pen where it was,
    // or the next showing operator on the line starts in the wrong place.
    flush_gap(&mut items, &mut gap);

    let mut bytes = Vec::new();
    crate::emit::write_op(&mut bytes, &crate::emit::show_text_adjusted(&items), &page.style);
    Ok(Patch::new(resolved.run.op_span.clone(), bytes))
}

/// Delete annotations whose contents mention the redacted text. Spec 10.6 step 3.
///
/// Whole annotations rather than the matching substring: an annotation's text
/// lives in `/Contents`, in `/RC` rich text, in a `/AP` appearance stream and
/// sometimes in a `/Popup` twin, and editing one of those while missing another
/// is the cosmetic failure again. Removing it entirely is cruder and correct.
fn strip_annotations(
    doc: &Document,
    page: &EditablePage,
    text: &str,
    changes: &mut BTreeMap<ObjId, Object>,
) {
    let Ok(pages) = rasura_content::page::pages(doc) else { return };
    let Some(this) = pages.pages.get(page.index) else { return };
    let Some(annots) = doc.get_entry(&this.dict, "Annots").ok().flatten() else { return };
    let Some(array) = annots.as_array() else { return };

    let mut keep = Vec::new();
    let mut removed = Vec::new();
    for entry in array {
        let mentions = doc
            .resolve(entry)
            .ok()
            .and_then(|o| o.as_dict().cloned())
            .is_some_and(|d| dictionary_mentions(doc, &d, text, 0));
        if mentions {
            if let Some(id) = entry.as_reference() {
                removed.push(id);
            }
        } else {
            keep.push(entry.clone());
        }
    }
    if removed.is_empty() {
        return;
    }

    let mut updated = this.dict.clone();
    updated.insert(Name::new("Annots"), Object::Array(keep));
    changes.insert(this.id, Object::Dictionary(updated));
    for id in removed {
        // Blanked rather than deleted: an annotation may be referenced from a
        // /Popup or a field tree, and a dangling reference resolves to null in
        // a way that varies between viewers.
        changes.insert(id, Object::Dictionary(Dictionary::new()));
    }
}

/// Whether any string anywhere in a dictionary contains the text.
fn dictionary_mentions(doc: &Document, dict: &Dictionary, text: &str, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    dict.iter().any(|(_, value)| value_mentions(doc, value, text, depth))
}

fn value_mentions(doc: &Document, value: &Object, text: &str, depth: usize) -> bool {
    match value {
        Object::String(s) => contains(s.as_bytes(), text.as_bytes()) || s.as_text().contains(text),
        Object::Array(items) => items.iter().any(|v| value_mentions(doc, v, text, depth + 1)),
        Object::Dictionary(d) => dictionary_mentions(doc, d, text, depth + 1),
        Object::Reference(_) => doc.resolve(value).ok().is_some_and(|o| match o.as_dict() {
            Some(d) => dictionary_mentions(doc, d, text, depth + 1),
            None => false,
        }),
        _ => false,
    }
}

/// Strip `/ActualText` and `/Alt` that repeat the redacted text. Spec 10.6 step 4.
///
/// These exist so a screen reader says something other than what the glyphs
/// draw, which means they are a *second* copy of the text — and one that
/// survives every content-stream edit.
fn strip_tags(doc: &Document, text: &str, changes: &mut BTreeMap<ObjId, Object>) {
    /// Keys whose value is user-visible text rather than structure.
    ///
    /// Every one of these was found by the corpus rather than reasoned about:
    /// `/V` and `/DV` are spec 10.6 step 3's "form field values", `/Title` is an
    /// outline entry, and `/Reason` and friends are a signature dictionary's
    /// human-readable fields. Each held a redacted word in some real file while
    /// the page it came from was correctly cleaned.
    ///
    /// Only *string* values are touched, which is what makes the list safe: a
    /// page's `/Contents` is a reference and an annotation's is text, and the
    /// type check tells them apart without this needing to know which object it
    /// is looking at. `/T`, the field's own name, is deliberately absent —
    /// removing it detaches the field from its parent.
    const TEXT_KEYS: [&str; 13] = [
        "ActualText",
        "Alt",
        "TU",
        "V",
        "DV",
        "RV",
        "Title",
        "Contents",
        "Subj",
        "RC",
        "Reason",
        "Location",
        "ContactInfo",
    ];

    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        let Ok(object) = doc.get(id) else { continue };
        let Some(dict) = object.as_dict() else { continue };

        // `/T` means two different things. On a form field it is the partial
        // name, and removing it detaches the field from its parent; on a
        // structure element it is the human-readable title, which is text and
        // has to go. The type tells them apart.
        let is_struct_elem = dict
            .get("Type")
            .and_then(Object::as_name)
            .and_then(|n| n.as_str())
            .is_some_and(|t| t == "StructElem");

        let mut updated = dict.clone();
        let mut touched = false;
        for key in TEXT_KEYS.iter().copied().chain(is_struct_elem.then_some("T")) {
            let Some(raw) = dict.get(key) else { continue };

            // The value may be a string *or a reference to one*. Checking only
            // the direct form silently skips the indirect one, which is how
            // `/ActualText 94 0 R` survived a redaction that reported clean --
            // the text sat in a standalone string object nothing had rewritten.
            let resolved = doc.resolve(raw).ok();
            let mentions = resolved
                .as_deref()
                .and_then(Object::as_string)
                .is_some_and(|s| s.as_text().contains(text));
            if !mentions {
                continue;
            }

            match raw.as_reference() {
                // Blank the object the key points at: dropping the key alone
                // would leave the string reachable from anywhere else that
                // references it, and from a full walk of the file.
                Some(target) => {
                    changes.insert(
                        target,
                        Object::String(rasura_cos::object::PdfString::new_literal(Vec::new())),
                    );
                }
                None => {
                    updated.remove(key);
                    touched = true;
                }
            }
        }
        if touched {
            changes.insert(id, Object::Dictionary(updated));
        }
    }
}

/// Purge the text from `/Info` and the XMP stream. Spec 10.6 step 5.
///
/// A title or subject that quotes the redacted phrase is invisible on the page
/// and sits in plain text near the front of the file.
fn purge_metadata(doc: &Document, text: &str, changes: &mut BTreeMap<ObjId, Object>) {
    if let Some(info) = doc.trailer().get("Info")
        && let Some(id) = info.as_reference()
        && let Ok(object) = doc.get(id)
        && let Some(dict) = object.as_dict()
    {
        let mut updated = dict.clone();
        let mut touched = false;
        let keys: Vec<Name> = dict.keys().cloned().collect();
        for key in keys {
            let mentions = dict
                .get_name(&key)
                .and_then(Object::as_string)
                .is_some_and(|s| s.as_text().contains(text));
            if mentions {
                updated.remove(key.as_str().unwrap_or(""));
                touched = true;
            }
        }
        if touched {
            changes.insert(id, Object::Dictionary(updated));
        }
    }

    // XMP is a stream of XML. Rather than parse it, the matching text is
    // replaced byte for byte -- which is what a purge means here, and cannot
    // reintroduce the string through a re-serialisation.
    if let Ok(catalog) = doc.catalog()
        && let Some(catalog) = catalog.as_dict()
        && let Some(meta) = catalog.get("Metadata")
        && let Some(id) = meta.as_reference()
        && let Ok(decoded) = doc.decoded_stream(id)
        && contains(&decoded, text.as_bytes())
        && let Ok(object) = doc.get(id)
        && let Some(stream) = object.as_stream()
    {
        let scrubbed = replace_all(&decoded, text.as_bytes());
        let mut updated = stream.clone();
        updated.set_decoded(scrubbed);
        changes.insert(id, Object::Stream(updated));
    }
}

fn generation_of(doc: &Document, number: u32) -> u16 {
    match doc.xref().get(number) {
        Some(rasura_cos::xref::XrefEntry::InFile { generation, .. }) => generation,
        // Objects inside an object stream are always generation 0.
        _ => 0,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every occurrence replaced by the same number of `X` bytes.
///
/// Same length so that offsets inside the stream -- an XMP packet declares its
/// own byte length in places -- stay valid.
fn replace_all(haystack: &[u8], needle: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend(std::iter::repeat_n(b'X', needle.len()));
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// What [`verify`] looked at and what it found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Where a redacted string was still found. Empty means clean.
    pub traces: Vec<Trace>,
    /// How many objects were examined.
    pub objects_checked: usize,
    /// How many streams were decoded and searched.
    pub streams_checked: usize,
    /// Places this check does **not** look, so a clean report is not read as
    /// more than it is.
    pub not_checked: Vec<&'static str>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.traces.is_empty()
    }
}

/// One surviving occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    pub string: String,
    pub where_found: String,
}

/// Re-parse output and assert none of `strings` survives. Spec 10.6.
///
/// > Add a `verify_redaction(doc, strings)` that re-parses the output and
/// > asserts none of the redacted strings appear in any object, decoded stream,
/// > or metadata field. Ship it as a public API — it is the assurance a legal
/// > customer needs.
///
/// Takes **bytes rather than a `Document`**, deliberately. Verifying the
/// in-memory document would check the thing that was edited; verifying the
/// saved file checks the thing that will be handed over, and those differ by a
/// save — which is exactly where an incremental append would have left the
/// original text behind.
///
/// The raw file is searched too, not only the parsed objects. A string can
/// survive in a place no object model reaches: a stale cross-reference table, a
/// comment, the gap between objects, the tail of a stream whose `/Length` is
/// short.
pub fn verify(bytes: &[u8], strings: &[String]) -> Report {
    let mut report = Report {
        not_checked: vec![
            "glyphs left in embedded font subsets (spec 10.6 step 6)",
            "pixels inside image data (spec 10.6 step 2)",
        ],
        ..Report::default()
    };

    for needle in strings {
        if needle.is_empty() {
            continue;
        }
        // The whole file, before anything is parsed. This catches what an
        // object walk cannot: bytes that are in the file and in no object.
        if contains(bytes, needle.as_bytes()) {
            report
                .traces
                .push(Trace { string: needle.clone(), where_found: "the raw file bytes".into() });
        }
    }

    let Ok(doc) = Document::open(bytes.to_vec()) else {
        report.traces.push(Trace {
            string: String::new(),
            where_found: "the output did not reopen, so nothing could be verified".into(),
        });
        return report;
    };

    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(&doc, number));
        let Ok(object) = doc.get(id) else { continue };
        report.objects_checked += 1;

        for needle in strings {
            if needle.is_empty() {
                continue;
            }
            if object_mentions(&object, needle, 0) {
                report.traces.push(Trace {
                    string: needle.clone(),
                    where_found: format!("object {id} ({})", describe(&object)),
                });
            }
        }

        if object.as_stream().is_some()
            && let Ok(decoded) = doc.decoded_stream(id)
        {
            report.streams_checked += 1;
            for needle in strings {
                if !needle.is_empty() && contains(&decoded, needle.as_bytes()) {
                    report.traces.push(Trace {
                        string: needle.clone(),
                        where_found: format!(
                            "the decoded stream of object {id} ({})",
                            describe(&object)
                        ),
                    });
                }
            }
        }
    }

    report.traces.sort();
    report.traces.dedup();
    report
}

/// Whether an object holds the text anywhere in its own value.
///
/// References are deliberately *not* followed. `verify` iterates every live
/// object, so a referenced object is searched on its own account; chasing
/// references here would re-search the same objects once per referrer and would
/// need its own cycle guard for nothing.
/// A short human description of an object, so a trace says what kind of thing
/// still holds the text.
///
/// "object 20" is not actionable, and object numbers are renumbered by the full
/// rewrite a redaction forces, so it does not even identify the same object the
/// caller started with. "an embedded font program" or "an annotation" tells a
/// caller what to do next.
fn describe(object: &Object) -> String {
    let dict = match object {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    };
    let Some(dict) = dict else { return "not a dictionary".into() };

    let name = |key: &str| {
        dict.get(key).and_then(Object::as_name).and_then(|n| n.as_str()).map(str::to_string)
    };
    if dict.get("Length1").is_some() || dict.get("FontFile2").is_some() {
        return "an embedded font program".into();
    }
    match (name("Type"), name("Subtype")) {
        (Some(t), Some(s)) => format!("/Type /{t} /Subtype /{s}"),
        (Some(t), None) => format!("/Type /{t}"),
        (None, Some(s)) => format!("/Subtype /{s}"),
        (None, None) if object.as_stream().is_some() => "an untyped stream".into(),
        (None, None) => "an untyped dictionary".into(),
    }
}

fn object_mentions(object: &Object, text: &str, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    match object {
        Object::String(s) => contains(s.as_bytes(), text.as_bytes()) || s.as_text().contains(text),
        Object::Name(n) => contains(n.as_bytes(), text.as_bytes()),
        Object::Array(items) => items.iter().any(|v| object_mentions(v, text, depth + 1)),
        Object::Dictionary(d) => d.iter().any(|(_, v)| object_mentions(v, text, depth + 1)),
        Object::Stream(s) => s.dict.iter().any(|(_, v)| object_mentions(v, text, depth + 1)),
        _ => false,
    }
}

impl PartialOrd for Trace {
    fn partial_cmp(&self, other: &Trace) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Trace {
    fn cmp(&self, other: &Trace) -> std::cmp::Ordering {
        (&self.string, &self.where_found).cmp(&(&other.string, &other.where_found))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page whose text, title and an annotation all quote the same secret.
    fn document() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Agent Kowalski reporting) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(
                6,
                "<< /Type /Annot /Subtype /Text /Rect [0 0 20 20] \
                 /Contents (a note about Kowalski) >>",
            )
            .object(7, "<< /Title (The Kowalski file) >>")
            .finish("/Root 1 0 R /Info 7 0 R")
    }

    fn analysed(bytes: Vec<u8>) -> (Document, EditablePage) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");
        (doc, page)
    }

    /// Plan, apply, mark and save -- the whole path a caller takes.
    fn redact(mut doc: Document, page: EditablePage, text: &str) -> (Vec<u8>, Vec<String>) {
        let plan = plan(&doc, &page, text).expect("plan");
        let strings = plan.strings.clone();
        let content = page.content;

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content("redact", &content, &plan.patches, plan.fidelity.clone())
            .expect("patch");
        if !plan.changes.is_empty() {
            session.set_objects("redact metadata", &plan.changes, plan.fidelity).expect("set");
        }
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;
        drop(saved);

        doc.mark_redacted();
        let out = rasura_cos::save(&doc, &SaveOptions::default()).expect("save").bytes;
        (out, strings)
    }

    #[test]
    fn the_redacted_text_is_gone_from_the_output() {
        let (doc, page) = analysed(document());
        let (out, strings) = redact(doc, page, "Kowalski");

        let report = verify(&out, &strings);
        assert!(report.is_clean(), "{:?}", report.traces);
        assert!(report.objects_checked > 0, "the verifier actually looked");
    }

    #[test]
    fn the_surrounding_text_survives() {
        // A redaction that removed the whole line would "pass" verification
        // while destroying the document.
        let (doc, page) = analysed(document());
        let (out, _) = redact(doc, page, "Kowalski");

        let after = Document::open(out).expect("reopen");
        let pages = rasura_content::page::pages(&after).expect("pages");
        let page = EditablePage::analyse(&after, &pages.pages[0]).expect("analyse");
        let text: String = page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect();

        assert!(text.contains("Agent"), "{text:?}");
        assert!(text.contains("reporting"), "{text:?}");
        assert!(!text.contains("Kowalski"), "{text:?}");
    }

    #[test]
    fn text_in_a_layer_that_is_turned_off_is_still_redacted() {
        // Spec 10.2's consequence for spec 10.6. "Hidden" is an instruction to
        // a viewer, not a property of the bytes: a layer that is off still
        // extracts, still shows up in `strings`, still gets copied by a reader
        // that ignores visibility. A redaction that skipped it would be exactly
        // the cosmetic failure this module exists to prevent -- and worse than
        // usual, because the page renders identically either way.
        let content = b"/OC /L1 BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (Agent Kowalski) Tj ET EMC\n";
        let bytes = ClassicBuilder::new()
            .object(
                1,
                "<< /Type /Catalog /Pages 2 0 R /OCProperties \
                 << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>",
            )
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /Properties << /L1 6 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(6, "<< /Type /OCG /Name (Reviewer notes) >>")
            .finish("/Root 1 0 R");

        let (doc, page) = analysed(bytes);
        // The layer is genuinely off, so the test is about hidden content.
        assert!(page.hidden_layer_at(page.runs[0].run.op_span.start).is_some());

        let (out, strings) = redact(doc, page, "Kowalski");
        let report = verify(&out, &strings);
        assert!(report.is_clean(), "{:?}", report.traces);
    }

    #[test]
    fn the_text_that_stays_does_not_move() {
        // The caller draws the black box over a rectangle computed from the
        // *original* layout. If the tail of the line closed up into the gap,
        // that box would cover words nobody asked to hide while the ones that
        // slid out from under it stayed legible.
        let (doc, page) = analysed(document());

        let before: Vec<(String, f64, f64)> = origins(&page);
        assert!(before.iter().any(|(t, _, _)| t == "A"), "{before:?}");

        let (out, _) = redact(doc, page, "Kowalski");
        let after_doc = Document::open(out).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after_page = EditablePage::analyse(&after_doc, &pages.pages[0]).expect("analyse");
        let after = origins(&after_page);

        // Every glyph that survived is where it was, to within the precision
        // the producer's own number formatting can express.
        for (text, x, y) in &after {
            let matched = before
                .iter()
                .any(|(t, bx, by)| t == text && (bx - x).abs() < 0.05 && (by - y).abs() < 0.05);
            assert!(matched, "{text:?} moved to ({x}, {y}); before: {before:?}");
        }
        assert!(after.len() < before.len(), "something was actually removed");
    }

    /// Every glyph's text and device-space origin, in order.
    fn origins(page: &EditablePage) -> Vec<(String, f64, f64)> {
        page.runs
            .iter()
            .flat_map(|r| {
                r.run
                    .glyphs
                    .iter()
                    .zip(&r.text)
                    .filter_map(|(g, t)| Some((t.clone()?, g.origin.x, g.origin.y)))
            })
            .collect()
    }

    #[test]
    fn redaction_forces_a_full_rewrite_even_when_incremental_is_asked_for() {
        // Spec 10.6 step 7, and the reason it is enforced in code: an
        // incremental append leaves the original bytes in the file, so the
        // redaction would be cosmetic.
        let (doc, page) = analysed(document());
        let plan = plan(&doc, &page, "Kowalski").expect("plan");
        let content = page.content;

        let mut doc = doc;
        {
            let mut session = EditSession::new(&mut doc);
            session
                .patch_content("redact", &content, &plan.patches, plan.fidelity.clone())
                .expect("patch");
            session.set_objects("meta", &plan.changes, plan.fidelity).expect("set");
            session.commit(&SaveOptions::default()).expect("commit");
        }
        doc.mark_redacted();

        // Asking for incremental explicitly must not defeat it.
        let out = rasura_cos::save(&doc, &SaveOptions::incremental()).expect("save");
        assert_eq!(out.mode, rasura_cos::SaveMode::FullRewrite);
        assert!(!out.bytes.starts_with(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>\nendobj\n4 0 obj\n<< /Length 61 >>\nstream\nBT /F1 12 Tf 1 0 0 1 72 700 Tm (Agent Kowalski reporting) Tj ET\n")
            || !contains(&out.bytes, b"Kowalski"));
    }

    #[test]
    fn an_incremental_save_would_have_left_the_text_behind() {
        // The failure the forced rewrite prevents, demonstrated. Without
        // `mark_redacted` the same edit saves incrementally and the original
        // bytes -- including the secret -- are still in the file.
        let (mut doc, page) = analysed(document());
        let plan = plan(&doc, &page, "Kowalski").expect("plan");
        let content = page.content;

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content("redact", &content, &plan.patches, plan.fidelity.clone())
            .expect("patch");
        let leaky = session.commit(&SaveOptions::default()).expect("commit").bytes;

        assert!(
            contains(&leaky, b"Kowalski"),
            "an incremental save keeps the original bytes -- this is what step 7 prevents"
        );
        let report = verify(&leaky, &["Kowalski".to_string()]);
        assert!(!report.is_clean(), "and the verifier catches it");
    }

    #[test]
    fn an_annotation_quoting_the_text_is_removed() {
        let (doc, page) = analysed(document());
        let (out, _) = redact(doc, page, "Kowalski");

        let after = Document::open(out).expect("reopen");
        let pages = rasura_content::page::pages(&after).expect("pages");
        let annots = after.get_entry(&pages.pages[0].dict, "Annots").ok().flatten();
        let count = annots.and_then(|a| a.as_array().map(<[Object]>::len)).unwrap_or(0);
        assert_eq!(count, 0, "the annotation went with it");
    }

    #[test]
    fn the_info_dictionary_is_purged() {
        // Invisible on the page and sitting in plain text near the front of the
        // file, which is where `strings` finds it first.
        let (doc, page) = analysed(document());
        let (out, _) = redact(doc, page, "Kowalski");
        assert!(!contains(&out, b"The Kowalski file"));
    }

    #[test]
    fn verify_finds_a_trace_the_object_walk_would_miss() {
        // A string in the file and in no object -- a comment, a stale table, a
        // gap between objects. The raw-bytes pass exists for this.
        let mut bytes = rasura_cos::testutil::minimal_classic();
        bytes.extend_from_slice(b"\n% leftover: Kowalski\n");
        let report = verify(&bytes, &["Kowalski".to_string()]);

        assert!(!report.is_clean());
        assert!(
            report.traces.iter().any(|t| t.where_found.contains("raw file")),
            "{:?}",
            report.traces
        );
    }

    #[test]
    fn verify_says_what_it_did_not_check() {
        // A clean report must not be read as more than it is. Font subsets and
        // image pixels are not searched, and silence would imply they were.
        let report = verify(&rasura_cos::testutil::minimal_classic(), &["absent".into()]);
        assert!(report.is_clean());
        assert_eq!(report.not_checked.len(), 2);
        assert!(report.not_checked.iter().any(|s| s.contains("font subset")));
    }

    #[test]
    fn the_font_subset_limitation_is_reported_on_every_plan() {
        // A subset's glyph inventory leaks the alphabet: removing `Kowalski`
        // from a page whose subset holds K, o, w, a, l, s, i still says those
        // letters were used.
        let (doc, page) = analysed(document());
        let plan = plan(&doc, &page, "Kowalski").expect("plan");
        match &plan.fidelity {
            Fidelity::Degraded(list) => {
                assert!(list.contains(&Compromise::FontSubsetRetained), "{list:?}")
            }
            other => panic!("a redaction is never Exact while step 6 is unbuilt: {other:?}"),
        }
    }

    #[test]
    fn text_inside_a_form_xobject_is_refused_not_mis_patched() {
        // The bug this guards was silent corruption. A run inside a form has
        // byte spans into the *form's* stream; the patch is applied against the
        // *page's*. When the page stream is the longer of the two the splice
        // succeeds and rewrites something else entirely, leaving the redacted
        // text exactly where it was.
        //
        // The page stream here is deliberately long, so an unguarded splice
        // would land rather than fail on a bounds check.
        let filler = "% ".to_string() + &"padding ".repeat(40);
        let page_content = format!("{filler}\nq 1 0 0 1 0 0 cm /Fm1 Do Q\n");
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                // The font is declared on the page as well as in the form: a
                // form-nested run''s font is looked up through the page''s
                // resources, so without this the glyphs map to nothing and the
                // fixture would exercise the guard on text nobody could find.
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Fm1 6 0 R >> /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", page_content.as_bytes())
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .stream(
                6,
                "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                 /Resources << /Font << /F1 5 0 R >> >>",
                b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Agent Kowalski) Tj ET\n",
            )
            .finish("/Root 1 0 R");

        let (doc, page) = analysed(bytes);
        // The text is found -- the walker descends into forms.
        let text: String = page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect();
        assert!(text.contains("Kowalski"), "the form's text is visible to the model: {text:?}");

        // And removing it is refused rather than attempted.
        let err = plan(&doc, &page, "Kowalski").expect_err("must refuse");
        assert!(matches!(err, RedactError::InsideForm { .. }), "{err:?}");
    }

    #[test]
    fn editing_text_inside_a_form_is_refused_too() {
        // The same trap reaches `replace_text`, where the consequence is a
        // corrupted page rather than a failed redaction.
        let filler = "% ".to_string() + &"padding ".repeat(40);
        let page_content = format!("{filler}\nq 1 0 0 1 0 0 cm /Fm1 Do Q\n");
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                // The font is declared on the page as well as in the form: a
                // form-nested run''s font is looked up through the page''s
                // resources, so without this the glyphs map to nothing and the
                // fixture would exercise the guard on text nobody could find.
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Fm1 6 0 R >> /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", page_content.as_bytes())
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .stream(
                6,
                "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                 /Resources << /Font << /F1 5 0 R >> >>",
                b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj ET\n",
            )
            .finish("/Root 1 0 R");

        let (doc, page) = analysed(bytes);
        let id = page.paragraphs[0].0;
        let err = crate::replace_text(&doc, &page, id, 0..5, "Howdy", crate::Policy::default())
            .expect_err("must refuse");
        assert!(matches!(err, crate::TextError::InsideForm { .. }), "{err:?}");
    }

    #[test]
    fn text_that_is_not_there_is_an_error_not_a_no_op() {
        // A caller that asked to remove something and got a clean result would
        // reasonably conclude it had been removed.
        let (doc, page) = analysed(document());
        let err = plan(&doc, &page, "Nakamura").expect_err("not present");
        assert!(matches!(err, RedactError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn every_occurrence_is_removed_not_just_the_first() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (x Kowalski y Kowalski z) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R");

        let (doc, page) = analysed(bytes);
        let (out, strings) = redact(doc, page, "Kowalski");
        assert!(verify(&out, &strings).is_clean());
    }
}
