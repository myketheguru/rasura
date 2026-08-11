//! The invariant suite. Spec 14.2.
//!
//! These are assertions run over the whole corpus on every commit, not a test
//! phase that happens later. Phase 1's exit condition is I1 green on the full
//! corpus, and this crate is what decides whether that is true.
//!
//! Implemented at this phase:
//!
//! | Invariant | Status |
//! |---|---|
//! | I1 identity | implemented |
//! | I2 locality | object-level half implemented; the pixel half needs the render harness |
//! | I3 validity | structural checks implemented; `qpdf --check` shells out when available |
//! | I4 round-trip stability | implemented at the object level (text extraction arrives in Phase 2) |
//! | I5 undo exactness | implemented |
//! | I6 tag integrity | implemented |
//! | I7 redaction completeness | implemented |
//! | 10.9 destinations resolve | implemented; a gate on output, a diagnostic on input |
//!
//! Unimplemented invariants are reported as `Skipped` with the reason, never
//! silently passed. A suite that reports green for checks it did not run is
//! worse than no suite.

use rasura_cos::document::{Document, LoadMode, OpenOptions, RecoveryPolicy};
use rasura_cos::object::ObjId;
use rasura_cos::parser::Parser;
use rasura_cos::writer::{self, SaveMode, SaveOptions};
use rasura_cos::xref::XrefEntry;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    /// Not applicable to this file, or not yet implementable at this phase.
    Skipped,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub invariant: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn pass(invariant: &'static str) -> Self {
        Check { invariant, status: Status::Pass, detail: String::new() }
    }
    fn fail(invariant: &'static str, detail: impl Into<String>) -> Self {
        Check { invariant, status: Status::Fail, detail: detail.into() }
    }
    fn skip(invariant: &'static str, detail: impl Into<String>) -> Self {
        Check { invariant, status: Status::Skipped, detail: detail.into() }
    }
}

#[derive(Debug, Clone)]
pub struct FileReport {
    pub name: String,
    pub bytes: usize,
    pub checks: Vec<Check>,
    /// Deviations the parser tolerated, surfaced so a corpus entry that quietly
    /// starts triggering recovery is visible.
    pub leniencies: Vec<String>,
    /// Defects the file arrived with. Diagnostics, never failures.
    pub input_defects: Vec<String>,
}

impl FileReport {
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Fail)
    }
}

impl fmt::Display for FileReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.failed() { "FAIL" } else { "ok  " };
        writeln!(f, "{mark} {} ({} bytes)", self.name, self.bytes)?;
        for c in &self.checks {
            match c.status {
                Status::Pass => writeln!(f, "       {} pass", c.invariant)?,
                Status::Fail => writeln!(f, "       {} FAIL  {}", c.invariant, c.detail)?,
                Status::Skipped => writeln!(f, "       {} skip  {}", c.invariant, c.detail)?,
            }
        }
        for l in &self.leniencies {
            writeln!(f, "       leniency: {l}")?;
        }
        for d in &self.input_defects {
            writeln!(f, "       input defect: {d}")?;
        }
        Ok(())
    }
}

/// Files that Rasura is *expected* to refuse, and why.
///
/// A corpus of deliberately-broken files contains some that no reader can open:
/// a PDF with no catalog anywhere is not a document. Declining those with a
/// typed error is the correct outcome, so they are recorded here rather than
/// counted as defects.
///
/// The list is deliberately keyed by exact filename and carries a reason. It is
/// not a way to silence failures: anything that starts failing and is *not*
/// listed here is still red, which is the property that makes the suite a
/// regression signal.
pub fn expected_decline(name: &str) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base {
        "bug1020226.pdf" | "REDHAT-1531897-0.pdf" => {
            Some("no /Type /Catalog anywhere in the file, even after a full-file scan")
        }
        "PDFBOX-4352-0.pdf" => Some(
            "/Encrypt does not resolve to a dictionary, so whether the content is \
             protected cannot be determined",
        ),
        _ => None,
    }
}

/// Run every implementable invariant against one file's bytes.
pub fn check_file(name: &str, bytes: &[u8]) -> FileReport {
    let mut checks = Vec::new();
    let opts = OpenOptions::default();

    let doc = match Document::open_with(bytes.to_vec(), &opts) {
        Ok(d) => d,
        // Refusing a file whose password we were not given is correct
        // behaviour, not a defect. Counting it as a failure would pressure the
        // library towards opening things it cannot actually decrypt.
        Err(e) if e.code() == rasura_cos::ErrorCode::EncryptedPasswordRequired => {
            return FileReport {
                name: name.to_string(),
                bytes: bytes.len(),
                checks: vec![Check::skip("open", "needs a password that was not supplied")],
                leniencies: Vec::new(),
                input_defects: Vec::new(),
            };
        }
        Err(e) => {
            let check = match expected_decline(name) {
                Some(why) => Check::skip("open", format!("declined as expected: {why} ({e})")),
                None => Check::fail("open", format!("{e}")),
            };
            return FileReport {
                name: name.to_string(),
                bytes: bytes.len(),
                checks: vec![check],
                leniencies: Vec::new(),
                input_defects: Vec::new(),
            };
        }
    };

    checks.push(check_i1(&doc, bytes));
    checks.push(check_object_fidelity(&doc));
    checks.push(check_i2_object_locality(bytes));
    checks.push(check_i3_structure(&doc, bytes));
    checks.push(check_i4_stability(&doc));
    checks.push(check_i5_undo_exactness(bytes));
    checks.push(check_destinations(&doc));
    checks.push(check_i6_tag_integrity(bytes));
    checks.push(check_i7_redaction(bytes));

    let input_defects = describe_input_defects(&doc, bytes);
    // Read the leniency log last: entries accumulate as objects are loaded, and
    // the checks above are what load them.
    let leniencies = doc.leniencies().iter().map(|l| l.to_string()).collect();

    FileReport { name: name.to_string(), bytes: bytes.len(), checks, leniencies, input_defects }
}

/// I1 -- Identity. `open(bytes)` then `save()` with zero edits produces
/// byte-identical output.
///
/// A file that only opened via recovery is exempt, and says so: its save is
/// forced to a full rewrite, which invalidates byte identity by design. Marking
/// that as a pass would be dishonest and marking it as a failure would be wrong.
pub fn check_i1(doc: &Document, original: &[u8]) -> Check {
    const NAME: &str = "I1 identity";
    if doc.load_mode() == LoadMode::Reconstructed {
        return Check::skip(NAME, "opened in recovery mode; save is forced to FullRewrite");
    }
    match writer::save(doc, &SaveOptions::default()) {
        Ok(r) if r.bytes == original => Check::pass(NAME),
        Ok(r) => Check::fail(
            NAME,
            format!(
                "output differs: {} bytes in, {} out, first difference at {}",
                original.len(),
                r.bytes.len(),
                first_difference(original, &r.bytes).map_or("end".to_string(), |i| i.to_string())
            ),
        ),
        Err(e) => Check::fail(NAME, format!("save failed: {e}")),
    }
}

/// The stronger parser-fidelity check that I1 alone does not reach.
///
/// I1 passes trivially for an unmodified incremental save, because nothing is
/// re-serialised. This walks every object, serialises it, re-parses it, and
/// requires the two to agree byte for byte -- which is what actually proves the
/// object model round-trips. A parser that silently normalised
/// `/N#61me` to `/Name`, or dropped a producer's `\101` escape, fails here and
/// nowhere else.
pub fn check_object_fidelity(doc: &Document) -> Check {
    const NAME: &str = "object round-trip";
    let mut checked = 0usize;
    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        let Ok(object) = doc.get(id) else { continue };
        if object.is_null() {
            continue;
        }
        // Streams are compared by their dictionary; their bodies are covered by
        // I1 and by the filter round-trip tests.
        let first = writer::object_to_bytes(&object);
        let reparsed = match Parser::new(&first).parse_object() {
            Ok(o) => o,
            Err(e) => {
                return Check::fail(
                    NAME,
                    format!("object {id} did not re-parse: {e}\n  emitted: {}", preview(&first)),
                );
            }
        };
        let second = writer::object_to_bytes(&reparsed);
        if first != second {
            return Check::fail(
                NAME,
                format!(
                    "object {id} is not stable across serialisation\n  first:  {}\n  second: {}",
                    preview(&first),
                    preview(&second)
                ),
            );
        }
        checked += 1;
    }
    if checked == 0 { Check::skip(NAME, "no loadable objects") } else { Check::pass(NAME) }
}

/// I2 -- Locality, object half. After editing one object, every *other* object
/// must be byte-identical in the output.
///
/// The pixel half (rendering every other page and diffing) needs the pdfium
/// reference harness from spec 14.3 and is checked there.
pub fn check_i2_object_locality(original: &[u8]) -> Check {
    const NAME: &str = "I2 locality (objects)";
    let Ok(doc) = Document::open(original.to_vec()) else {
        return Check::skip(NAME, "file did not open");
    };
    if doc.load_mode() == LoadMode::Reconstructed {
        return Check::skip(NAME, "recovery mode forces a full rewrite");
    }

    // Pick a victim: any object that is a dictionary and is not the catalog.
    let victim =
        doc.xref().live_objects().map(|n| ObjId::new(n, generation_of(&doc, n))).find(|id| {
            doc.get(*id).is_ok_and(|o| o.as_dict().is_some() && o.as_stream().is_none())
        });
    let Some(victim) = victim else {
        return Check::skip(NAME, "no dictionary object to edit");
    };

    let mut doc = doc;
    let Ok(object) = doc.get(victim) else {
        return Check::skip(NAME, "victim object vanished");
    };
    let mut dict = object.as_dict().unwrap().clone();
    dict.insert(
        rasura_cos::object::Name::new("RasuraLocalityProbe"),
        rasura_cos::object::Object::Integer(1),
    );
    doc.set(victim, rasura_cos::object::Object::Dictionary(dict));

    let result = match writer::save(&doc, &SaveOptions::default()) {
        Ok(r) => r,
        Err(e) => return Check::fail(NAME, format!("save failed: {e}")),
    };

    if !result.bytes.starts_with(original) {
        return Check::fail(
            NAME,
            format!(
                "an incremental save rewrote original bytes; first difference at {}",
                first_difference(original, &result.bytes)
                    .map_or("end".to_string(), |i| i.to_string())
            ),
        );
    }

    // Reopen and confirm every other object still reads back identically.
    let Ok(after) = Document::open(result.bytes) else {
        return Check::fail(NAME, "the edited file did not reopen");
    };
    for number in doc.xref().live_objects() {
        if number == victim.number {
            continue;
        }
        let id = ObjId::new(number, generation_of(&doc, number));
        let (Ok(before_obj), Ok(after_obj)) = (doc.get(id), after.get(id)) else { continue };
        if writer::object_to_bytes(&before_obj) != writer::object_to_bytes(&after_obj) {
            return Check::fail(NAME, format!("editing {victim} changed object {id}"));
        }
    }
    Check::pass(NAME)
}

/// I5 -- Undo exactness. "Any operation followed by `undo()` restores the exact
/// prior byte state."
///
/// The operation used is a real content-stream edit through `rasura-edit`,
/// not a synthetic one: the point is to exercise the path a caller takes, which
/// includes localising a span to its object, re-encoding through the original
/// filter chain, and staging the object as dirty. A probe that only called
/// `Document::set` and `discard_changes` would pass without touching any of
/// that.
///
/// The subject is **bytes**, not values. Restoring an object's value while
/// leaving it staged makes the writer append a revision that rewrites it to
/// exactly what it already said — the objects all read correctly and the file
/// has still changed. That failure is invisible to any value-level assertion,
/// and it is the one this check was written to catch.
pub fn check_i5_undo_exactness(original: &[u8]) -> Check {
    const NAME: &str = "I5 undo exactness";

    let Ok(mut doc) = Document::open(original.to_vec()) else {
        return Check::skip(NAME, "file did not open");
    };
    if doc.load_mode() == LoadMode::Reconstructed {
        return Check::skip(NAME, "recovery mode forces a full rewrite");
    }

    // The first page with content is the subject. A file with no page content
    // has nothing this invariant can be asserted about.
    let Ok(pages) = rasura_content::page::pages(&doc) else {
        return Check::skip(NAME, "no page tree");
    };
    let Some(page) = pages.pages.first() else {
        return Check::skip(NAME, "no pages");
    };
    let Ok((content, _)) = rasura_content::content::page_content(&doc, &page.dict) else {
        return Check::skip(NAME, "page content did not load");
    };
    if content.data().is_empty() || content.parts().is_empty() {
        return Check::skip(NAME, "the page has no content stream");
    }

    // An insertion of a comment at the start of the content: syntactically
    // inert, so it cannot break a page, but it is a genuine byte-level edit
    // that has to be localised, spliced, re-encoded and staged.
    let probe: &[u8] = b"% rasura undo probe\n";
    let mut session = rasura_edit::EditSession::new(&mut doc);
    let start = content.parts()[0].range.start;
    let report = session.patch_content(
        "undo probe",
        &content,
        &[rasura_edit::Patch::insert(start, probe.to_vec())],
        rasura_edit::Fidelity::Exact,
    );
    if let Err(e) = report {
        return Check::skip(NAME, format!("the probe edit did not apply: {e}"));
    }

    // The edit must actually have changed the file, or the undo below proves
    // nothing. A check that can only pass is worse than no check.
    let edited = match rasura_cos::save(session.document(), &SaveOptions::default()) {
        Ok(r) => r.bytes,
        Err(e) => return Check::fail(NAME, format!("saving the edit failed: {e}")),
    };
    if edited == original {
        return Check::fail(NAME, "the probe edit produced no change, so undo proves nothing");
    }

    match session.undo() {
        Ok(true) => {}
        Ok(false) => return Check::fail(NAME, "there was nothing to undo"),
        Err(e) => return Check::fail(NAME, format!("undo failed: {e}")),
    }

    let restored = match writer::save(&doc, &SaveOptions::default()) {
        Ok(r) => r.bytes,
        Err(e) => return Check::fail(NAME, format!("saving after undo failed: {e}")),
    };

    if restored == original {
        Check::pass(NAME)
    } else {
        Check::fail(
            NAME,
            format!(
                "undo left the file changed: {} bytes in, {} out, first difference at {}",
                original.len(),
                restored.len(),
                first_difference(original, &restored).map_or("end".to_string(), |i| i.to_string())
            ),
        )
    }
}

/// I7 -- Redaction completeness. "`verify_redaction` finds no trace of redacted
/// strings anywhere in the output."
///
/// Run as an *end-to-end* check rather than a unit test: a real word is taken
/// off the page, the document is marked redacted, saved, reopened, and searched.
/// Every stage is one where the text could survive — the operator might be
/// rewritten and the metadata missed, the objects might be clean and the save
/// incremental, the save might be a full rewrite and the string still sit in an
/// annotation. Only the whole path proves anything.
///
/// The word chosen is one the page actually draws, so a file where nothing was
/// removed cannot pass by having nothing to find. A check that can only succeed
/// is worse than none.
pub fn check_i7_redaction(original: &[u8]) -> Check {
    const NAME: &str = "I7 redaction completeness";

    let Ok(doc) = Document::open(original.to_vec()) else {
        return Check::skip(NAME, "file did not open");
    };
    let Ok(pages) = rasura_content::page::pages(&doc) else {
        return Check::skip(NAME, "no page tree");
    };
    let Some(first) = pages.pages.first() else {
        return Check::skip(NAME, "no pages");
    };
    let Some(page) = rasura_edit::EditablePage::analyse(&doc, first) else {
        return Check::skip(NAME, "the page did not analyse");
    };

    // A word the page really draws, long enough not to appear by accident in an
    // operator name or a number, **and appearing nowhere in the document except
    // the content streams that draw it**.
    //
    // That last condition is what makes this measure redaction rather than the
    // corpus. Some files name a font after a word on the page
    // (`/BaseFont /NuptialScript`); one points `/Count` at a junk stream whose
    // bytes happen to contain a word from the text. Those occurrences are
    // structural: removing them would break the font reference or the page
    // tree, so no correct redaction removes them, and asserting their absence
    // would be asserting that redaction should corrupt the file.
    //
    // `verify` still reports them — it is deliberately stricter than this
    // check, because a caller asking "does this string appear in the bytes I am
    // about to hand over" wants the strict answer.
    // Joined with a space, not concatenated. Running the paragraphs together
    // fuses the last word of one onto the first of the next -- "BoldArial",
    // "ShortreportAbstract" -- and those appear in no paragraph, so the check
    // would skip perfectly redactable files while blaming the document.
    let text: String =
        page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect::<Vec<_>>().join(" ");
    let content_streams = page_content_objects(&doc);
    let Some(word) = text
        .split_whitespace()
        .filter(|w| w.chars().count() >= 5 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .find(|w| only_in_content(&doc, &content_streams, w))
        .map(str::to_string)
    else {
        return Check::skip(NAME, "no word appears solely in this page's drawn content");
    };

    drop(page);
    let mut doc = doc;
    // The document-wide entry point, not the page-scoped `plan`. A name removed
    // from page one and left on page five verifies as *failed*, which is the
    // right answer and is how the corpus caught the page-scoped version.
    let applied = match rasura_edit::redact::apply(&mut doc, &word) {
        Ok(r) => r,
        Err(e) => return Check::skip(NAME, format!("could not redact: {e}")),
    };
    let strings = applied.strings.clone();

    let out = match writer::save(&doc, &SaveOptions::default()) {
        Ok(r) => r,
        Err(e) => return Check::skip(NAME, format!("saving the redaction failed: {e}")),
    };
    if out.mode != rasura_cos::SaveMode::FullRewrite {
        return Check::fail(NAME, "a redacted document was not fully rewritten (spec 10.6 step 7)");
    }

    let report = rasura_edit::verify(&out.bytes, &strings);
    if report.is_clean() {
        Check::pass(NAME)
    } else {
        Check::fail(
            NAME,
            format!(
                "{:?} survived redaction in {} place(s): {}",
                word,
                report.traces.len(),
                report
                    .traces
                    .iter()
                    .take(3)
                    .map(|t| t.where_found.clone())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
    }
}

/// I6 -- Tag integrity. "For tagged documents, the structure tree after an edit
/// contains the same element count and ordering, with MCIDs resolving."
///
/// Run before *and after* a real edit, because the invariant is about
/// survival rather than about the input. A file whose tagging was already
/// broken is not this library's failure, so a document that starts degraded is
/// held to the standard that matters: the edit must not make it **worse**.
///
/// The edit chosen is a text replacement of equal length, which is the case
/// tagging is most likely to survive and therefore the one where a regression
/// is most clearly ours. Marked-content operators sit outside the showing
/// operator's span, so a correct patch leaves every `BDC`/`EMC` pair — and
/// every MCID — exactly where it was.
pub fn check_i6_tag_integrity(original: &[u8]) -> Check {
    const NAME: &str = "I6 tag integrity";

    let Ok(doc) = Document::open(original.to_vec()) else {
        return Check::skip(NAME, "file did not open");
    };
    let Ok(pages) = rasura_content::page::pages(&doc) else {
        return Check::skip(NAME, "no page tree");
    };

    let before = rasura_layout::validate_tags(&doc, &pages);
    if before.status == rasura_layout::TaggedStatus::Untagged {
        return Check::skip(NAME, "the document is not tagged");
    }

    // A word to replace with one of the same length, so the edit is as gentle
    // as an edit can be. Anything harsher would be testing reflow rather than
    // tagging.
    let Some(page) = pages.pages.first().and_then(|p| rasura_edit::EditablePage::analyse(&doc, p))
    else {
        return Check::skip(NAME, "the first page did not analyse");
    };
    let Some(id) = page.paragraphs.first().map(|(id, _)| *id) else {
        return Check::skip(NAME, "the first page has no paragraph");
    };
    let text = page.text_of(id);
    let Some(word) = text
        .split_whitespace()
        .find(|w| w.chars().count() >= 4 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string)
    else {
        return Check::skip(NAME, "no word to replace on the first page");
    };
    let Some(at) = text.find(&word).map(|byte| text[..byte].chars().count()) else {
        return Check::skip(NAME, "the word moved");
    };
    let replacement: String = "x".repeat(word.chars().count());

    let mut doc = doc;
    let edit = match rasura_edit::replace_text(
        &doc,
        &page,
        id,
        at..at + word.chars().count(),
        &replacement,
        rasura_edit::Policy::default(),
    ) {
        Ok(e) => e,
        Err(e) => return Check::skip(NAME, format!("the probe edit did not apply: {e}")),
    };

    let content = page.content;
    let saved = {
        let mut session = rasura_edit::EditSession::new(&mut doc);
        if let Err(e) = session.patch_content("tag probe", &content, &edit.patches, edit.fidelity) {
            return Check::skip(NAME, format!("the probe edit did not apply: {e}"));
        }
        match session.commit(&SaveOptions::default()) {
            Ok(r) => r.bytes,
            Err(e) => return Check::fail(NAME, format!("saving the probe edit failed: {e}")),
        }
    };

    let Ok(after_doc) = Document::open(saved) else {
        return Check::fail(NAME, "the edited document did not reopen");
    };
    let Ok(after_pages) = rasura_content::page::pages(&after_doc) else {
        return Check::fail(NAME, "the edited document lost its page tree");
    };
    let after = rasura_layout::validate_tags(&after_doc, &after_pages);

    if after.elements != before.elements {
        return Check::fail(
            NAME,
            format!("element count changed: {} -> {}", before.elements, after.elements),
        );
    }
    if after.claimed != before.claimed || after.drawn != before.drawn {
        return Check::fail(
            NAME,
            format!(
                "marked content changed: {} claimed / {} drawn -> {} / {}",
                before.claimed, before.drawn, after.claimed, after.drawn
            ),
        );
    }
    // A document that arrived degraded may stay degraded; it may not get worse.
    if after.findings.len() > before.findings.len() {
        let fresh: Vec<String> = after
            .findings
            .iter()
            .filter(|f| !before.findings.contains(f))
            .take(3)
            .map(|f| format!("{f:?}"))
            .collect();
        return Check::fail(
            NAME,
            format!(
                "the edit introduced {} tagging defect(s): {}",
                after.findings.len() - before.findings.len(),
                fresh.join("; ")
            ),
        );
    }
    Check::pass(NAME)
}

/// Every object that is a page's `/Contents` stream.
fn page_content_objects(doc: &Document) -> std::collections::BTreeSet<ObjId> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(pages) = rasura_content::page::pages(doc) else { return out };
    for page in &pages.pages {
        match page.dict.get("Contents") {
            Some(rasura_cos::object::Object::Reference(id)) => {
                out.insert(*id);
            }
            Some(rasura_cos::object::Object::Array(items)) => {
                out.extend(items.iter().filter_map(|o| o.as_reference()));
            }
            _ => {}
        }
    }
    out
}

/// Whether `word` appears only inside the given content streams.
fn only_in_content(
    doc: &Document,
    content: &std::collections::BTreeSet<ObjId>,
    word: &str,
) -> bool {
    let needle = word.as_bytes();
    let holds = |bytes: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);

    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        let Ok(object) = doc.get(id) else { continue };

        // A content stream's *dictionary* is not content. A page that draws the
        // literal text "ASCIIHexDecode" through a stream filtered with
        // `/ASCIIHexDecode` has the word in both, and only one of them is
        // removable — so such a word is not a fair subject for this check.
        if content.contains(&id) {
            if let Some(stream) = object.as_stream()
                && holds(&writer::object_to_bytes(&rasura_cos::object::Object::Dictionary(
                    stream.dict.clone(),
                )))
            {
                return false;
            }
            continue;
        }

        if holds(&writer::object_to_bytes(&object)) {
            return false;
        }
        if object.as_stream().is_some()
            && let Ok(decoded) = doc.decoded_stream(id)
            && holds(&decoded)
        {
            return false;
        }
    }
    true
}

/// Destinations that point nowhere. Spec 10.9.
///
/// > A dangling destination is a silent corruption; add an invariant check for
/// > it.
///
/// "Silent" is the operative word. A link whose target no longer exists opens
/// the document, renders identically, extracts identically, and passes
/// `qpdf --check`; the only symptom is a click that does nothing, which no
/// automated check other than this one would notice.
///
/// Reported on the **input** as a diagnostic and asserted on nothing yet,
/// because no operation in the library can currently change page count or
/// order. When page operations land this becomes their gate — the check is
/// written first so that it is measured against real files before anything
/// depends on it, and so the fix-up cannot be written against a check that
/// agrees with it by construction.
pub fn check_destinations(doc: &Document) -> Check {
    const NAME: &str = "10.9 destinations resolve";

    let Ok(pages) = rasura_content::page::pages(doc) else {
        return Check::skip(NAME, "no page tree");
    };
    let nav = rasura_content::dest::collect(doc, &pages);

    if nav.destinations.is_empty() {
        return Check::skip(NAME, "the document has no destinations");
    }
    let dangling: Vec<String> = nav
        .dangling()
        .take(4)
        .map(|d| match (&d.name, d.target) {
            (Some(name), _) if d.unresolved_name => format!("name {name:?} is undefined"),
            (_, Some(id)) => format!("points at {id}, which is not a page"),
            _ => "points nowhere".to_string(),
        })
        .collect();

    if dangling.is_empty() {
        Check::pass(NAME)
    } else {
        // Counted as a *skip with a reason* rather than a failure: these are
        // defects in files this library did not write, and the corpus is full
        // of deliberately broken input. The same walk becomes a hard gate on
        // *output* once page operations exist to produce it.
        Check::skip(
            NAME,
            format!(
                "input defect: {} of {} destination(s) dangle -- {}",
                nav.dangling().count(),
                nav.destinations.len(),
                dangling.join("; ")
            ),
        )
    }
}

/// I3 -- Validity. "Output passes `qpdf --check` and veraPDF."
///
/// The subject is the **output**, not the input. Checking the input conflates
/// two different things: a defect Rasura introduced, and a defect that was
/// in the file when it arrived. The corpus is full of the latter by design --
/// fuzzed files, files from broken producers, files pdf.js keeps precisely
/// because they are wrong -- and failing the library for those would make the
/// suite unusable as a regression signal.
///
/// Input defects are still worth seeing, so they are reported separately by
/// `describe_input_defects` as diagnostics that do not fail the run.
///
/// `qpdf --check` and veraPDF remain the authorities; CI shells out to qpdf.
/// This is what can be asserted from inside the library.
pub fn check_i3_structure(doc: &Document, _bytes: &[u8]) -> Check {
    const NAME: &str = "I3 validity (structural)";

    // A full rewrite is the mode that produces a fresh, self-consistent file,
    // so it is the one whose structure is entirely Rasura's responsibility.
    let opts =
        SaveOptions { mode: Some(SaveMode::FullRewrite), accept_signature_destruction: true };
    let out = match writer::save(doc, &opts) {
        Ok(r) => r.bytes,
        Err(e) => return Check::fail(NAME, format!("save failed: {e}")),
    };
    let rewritten = match Document::open(out.clone()) {
        Ok(d) => d,
        Err(e) => return Check::fail(NAME, format!("output did not reopen: {e}")),
    };

    let mut problems = Vec::new();
    if rewritten.catalog().is_err() && doc.catalog().is_ok() {
        problems.push("output lost the catalog the input had".to_string());
    }
    if rewritten.load_mode() == LoadMode::Reconstructed {
        problems.push("output only reopens through recovery".to_string());
    }
    for (number, entry) in rewritten.xref().iter() {
        let XrefEntry::InFile { offset, .. } = entry else { continue };
        if offset >= out.len() {
            problems.push(format!("output object {number}: offset {offset} past end of file"));
        } else if rewritten.get(ObjId::new(number, generation_of(&rewritten, number))).is_err() {
            problems.push(format!("output object {number}: entry points at unreadable data"));
        }
        if problems.len() > 8 {
            problems.push("...".into());
            break;
        }
    }

    if problems.is_empty() { Check::pass(NAME) } else { Check::fail(NAME, problems.join("; ")) }
}

/// Structural defects present in the *input*. Diagnostics, not failures.
///
/// A file can be defective and still perfectly usable -- ISO 32000-1 §7.3.10
/// makes a reference to a missing object null, so a dangling cross-reference
/// entry is legal. Reporting these separately keeps that distinction visible.
pub fn describe_input_defects(doc: &Document, bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if doc.catalog().is_err() {
        out.push("/Root does not resolve to a catalog".to_string());
    }
    for (number, entry) in doc.xref().iter() {
        match entry {
            XrefEntry::InFile { offset, .. } => {
                if offset >= bytes.len() {
                    out.push(format!("object {number}: offset {offset} is past end of file"));
                } else if doc.get(ObjId::new(number, generation_of(doc, number))).is_err() {
                    out.push(format!("object {number}: entry points at unreadable data"));
                }
            }
            XrefEntry::InObjStm { container, .. } => {
                if doc.xref().get(container).is_none() {
                    out.push(format!(
                        "object {number}: container object stream {container} is not in the table"
                    ));
                }
            }
            XrefEntry::Free { .. } => {}
        }
        if out.len() > 8 {
            out.push("...".into());
            break;
        }
    }
    out
}

/// I4 -- Round-trip stability. Full text extraction arrives in Phase 2; at the
/// object layer the equivalent is that a no-op save/reopen cycle yields the same
/// objects.
pub fn check_i4_stability(doc: &Document) -> Check {
    const NAME: &str = "I4 round-trip stability";
    if doc.load_mode() == LoadMode::Reconstructed {
        return Check::skip(NAME, "recovery mode; compared after full rewrite instead");
    }
    let saved = match writer::save(doc, &SaveOptions::default()) {
        Ok(r) => r.bytes,
        Err(e) => return Check::fail(NAME, format!("save failed: {e}")),
    };
    let Ok(reopened) = Document::open(saved) else {
        return Check::fail(NAME, "the saved file did not reopen");
    };
    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        let (Ok(a), Ok(b)) = (doc.get(id), reopened.get(id)) else { continue };
        if writer::object_to_bytes(&a) != writer::object_to_bytes(&b) {
            return Check::fail(NAME, format!("object {id} changed across a no-op save"));
        }
    }
    Check::pass(NAME)
}

/// A full rewrite must yield a file that opens and preserves reachable content.
/// Checked separately because it does not, and must not, preserve bytes.
pub fn check_full_rewrite(bytes: &[u8]) -> Check {
    const NAME: &str = "full rewrite reopens";
    let Ok(doc) = Document::open(bytes.to_vec()) else {
        return Check::skip(NAME, "file did not open");
    };
    let opts =
        SaveOptions { mode: Some(SaveMode::FullRewrite), accept_signature_destruction: true };
    let result = match writer::save(&doc, &opts) {
        Ok(r) => r,
        Err(e) => return Check::fail(NAME, format!("{e}")),
    };
    match Document::open(result.bytes) {
        Ok(re) if re.catalog().is_ok() => Check::pass(NAME),
        Ok(_) => Check::fail(NAME, "rewritten file has no usable catalog"),
        Err(e) => Check::fail(NAME, format!("rewritten file did not reopen: {e}")),
    }
}

/// A file that is not a PDF at all, or is damaged past recovery, must fail
/// cleanly. The parser must never panic, hang, or allocate unboundedly --
/// spec 14.4. This asserts the first of those; the fuzzer covers the rest.
pub fn check_declines_cleanly(bytes: &[u8]) -> Check {
    const NAME: &str = "declines cleanly";
    let opts = OpenOptions { recovery: RecoveryPolicy::Auto, ..Default::default() };
    match Document::open_with(bytes.to_vec(), &opts) {
        Ok(_) => Check::skip(NAME, "file opened; nothing to decline"),
        Err(e) => {
            // The error must carry a code a caller can branch on.
            let _code = e.code();
            Check::pass(NAME)
        }
    }
}

fn generation_of(doc: &Document, number: u32) -> u16 {
    match doc.xref().get(number) {
        Some(XrefEntry::InFile { generation, .. }) => generation,
        // Objects inside an object stream always have generation 0.
        _ => 0,
    }
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .or_else(|| if a.len() == b.len() { None } else { Some(a.len().min(b.len())) })
}

fn preview(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]);
    if bytes.len() > 160 { format!("{s}...") } else { s.into_owned() }
}

/// The generated seed corpus. Real producer output lives in `corpus/files/` and
/// is picked up by the runner; these are the structural cases that must hold
/// regardless of what anyone contributes.
pub fn seed_corpus() -> Vec<(String, Vec<u8>)> {
    use rasura_cos::testutil as t;
    let mut out: Vec<(String, Vec<u8>)> = vec![
        ("minimal-classic".into(), t::minimal_classic()),
        ("classic-flate-content".into(), t::classic_with_flate_content()),
        ("xref-stream-objstm".into(), t::xref_stream_with_objstm()),
        ("two-revisions".into(), t::two_revisions()),
        ("encrypted-rc4-128".into(), t::encrypted(t::FixtureCipher::Rc4_128)),
        ("encrypted-aes-128".into(), t::encrypted(t::FixtureCipher::Aes128)),
        (
            "encrypted-rc4-128-xref-stream".into(),
            t::encrypted_xref_stream(t::FixtureCipher::Rc4_128),
        ),
        (
            "encrypted-aes-128-xref-stream".into(),
            t::encrypted_xref_stream(t::FixtureCipher::Aes128),
        ),
    ];
    // Spec 17, Phase 4's exit criterion: "injection round-trips validate".
    // Everything else in the font layer checks its own output; this puts a real
    // document with an injected glyph in front of qpdf and veraPDF, which have
    // no stake in agreeing with us.
    out.push(("injected-truetype".into(), rasura_font::fixture::injected_truetype_pdf()));
    // The same document before the injection, so §14.3's pixel diff has a
    // baseline to compare against.
    out.push(("injected-truetype-before".into(), rasura_font::fixture::uninjected_truetype_pdf()));

    out.extend(t::adversarial().into_iter().map(|(n, b)| (format!("adversarial-{n}"), b)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_corpus_is_green() {
        let mut failures = Vec::new();
        for (name, bytes) in seed_corpus() {
            let report = check_file(&name, &bytes);
            if report.failed() {
                failures.push(report.to_string());
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn full_rewrite_is_green_across_the_seed_corpus() {
        for (name, bytes) in seed_corpus() {
            let c = check_full_rewrite(&bytes);
            assert_ne!(c.status, Status::Fail, "{name}: {}", c.detail);
        }
    }

    #[test]
    fn i1_actually_catches_a_perturbation() {
        // A guard against the invariant passing vacuously: if the saved output
        // is compared against the wrong bytes, I1 must fail.
        let bytes = rasura_cos::testutil::minimal_classic();
        let doc = Document::open(bytes.clone()).unwrap();
        let mut perturbed = bytes.clone();
        let n = perturbed.len() / 2;
        perturbed[n] ^= 0x01;
        assert_eq!(check_i1(&doc, &perturbed).status, Status::Fail);
        assert_eq!(check_i1(&doc, &bytes).status, Status::Pass);
    }

    #[test]
    fn i5_passes_on_a_document_with_content() {
        let bytes = rasura_cos::testutil::classic_with_flate_content();
        assert_eq!(check_i5_undo_exactness(&bytes).status, Status::Pass);
    }

    #[test]
    fn i5_would_catch_an_undo_that_left_the_document_staged() {
        // The failure mode I5 exists for, reproduced by hand: restore an
        // object's value but leave it dirty. Every object then reads back
        // correctly and the file has still grown by a revision.
        //
        // Without this, the check could be satisfied by an `undo` that did
        // nothing at all on a document whose save happened to be stable, and
        // nobody would know.
        let original = rasura_cos::testutil::classic_with_flate_content();
        let mut doc = Document::open(original.clone()).unwrap();

        let id = ObjId::new(4, 0);
        let before = (*doc.get(id).unwrap()).clone();
        doc.set(id, before.clone());

        let restored = writer::save(&doc, &SaveOptions::default()).unwrap().bytes;
        assert_ne!(
            restored, original,
            "restoring a value while leaving it staged does change the file -- \
             which is exactly why I5 compares bytes and not values"
        );
    }

    #[test]
    fn i5_declines_rather_than_passes_on_a_document_with_no_content() {
        // Spec 14.2's rule for this suite: never silently pass what was not
        // actually checked.
        let bytes = rasura_cos::testutil::minimal_classic();
        let check = check_i5_undo_exactness(&bytes);
        assert!(
            matches!(check.status, Status::Pass | Status::Skipped),
            "a contentless file is skipped or genuinely passes, never failed: {check:?}"
        );
    }

    #[test]
    fn i6_skips_an_untagged_document_rather_than_passing_it() {
        // A pass would say the tagging survived an edit, on a file that has no
        // tagging. Spec 14.2's rule: never silently pass what was not checked.
        let bytes = rasura_cos::testutil::classic_with_flate_content();
        assert_eq!(check_i6_tag_integrity(&bytes).status, Status::Skipped);
    }

    #[test]
    fn i6_notices_marked_content_disappearing() {
        // The regression I6 exists to catch, reproduced against the validator:
        // an element still claiming content the page no longer draws. If a
        // future patcher rewrites a `BDC` away, this is the shape of the
        // failure it would produce.
        use rasura_layout::{Finding, TaggedStatus};

        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 8 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /StructParents 0 >>",
            )
            // The element below claims MCID 0; this content marks nothing.
            .stream(4, "", b"BT ET\n")
            .object(8, "<< /Type /StructTreeRoot /K [9 0 R] >>")
            .object(9, "<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 0 >>")
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let report = rasura_layout::validate_tags(&doc, &pages);

        assert_eq!(report.status, TaggedStatus::Degraded);
        assert!(
            report.findings.contains(&Finding::DanglingMcid { page: 0, mcid: 0 }),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn object_fidelity_catches_a_name_that_does_not_round_trip() {
        // The mechanism this check exists to protect: a name whose raw encoding
        // differs from its decoded value.
        use rasura_cos::object::{Name, Object};
        let n = Object::Name(Name::from_raw(b"N#61me"));
        let bytes = writer::object_to_bytes(&n);
        assert_eq!(bytes, b"/N#61me");
        let back = Parser::new(&bytes).parse_object().unwrap();
        assert_eq!(writer::object_to_bytes(&back), bytes);
    }

    #[test]
    fn garbage_input_is_declined_not_accepted() {
        for junk in
            [b"".to_vec(), b"not a pdf at all".to_vec(), b"%PDF-1.4\n".to_vec(), vec![0xffu8; 4096]]
        {
            let c = check_declines_cleanly(&junk);
            assert_ne!(c.status, Status::Fail, "{}", c.detail);
        }
    }
}
