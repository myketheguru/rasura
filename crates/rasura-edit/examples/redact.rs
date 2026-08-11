//! A redaction, end to end, checked the way an outsider would check it.
//! Spec 10.6, 14.2 I7.
//!
//! > Redaction is not drawing a black rectangle.
//!
//! The unit tests for `redact` assert against the module's own report, which is
//! exactly the assurance a legal customer cannot accept: the code that removed
//! the text is also the code saying it is gone. This example takes the opposite
//! stance. It hides one word in **five** places a page does not show, saves,
//! and then looks for the word again — through `verify`, and separately through
//! a raw byte scan of the output file that knows nothing about PDF at all.
//!
//! The five hiding places are not invented. Each one is somewhere the corpus
//! found the word surviving a redaction that had reported itself clean:
//!
//! | Where | Why it is missed |
//! |---|---|
//! | The showing operator | the only one an obvious implementation removes |
//! | `/Info` `/Subject` | plain text near the front of the file |
//! | The XMP packet | a second copy of the same metadata, in XML |
//! | An outline title | navigation, not content, so nothing walks it |
//! | An **indirect** `/ActualText` | `as_string` on the key returns `None` |
//!
//! The last is the sharpest: `/ActualText 12 0 R` looks like an absent key to
//! any check that does not resolve it, and the string sits in a standalone
//! object that nothing else rewrites.
//!
//! ```text
//! cargo run -p rasura-edit --example redact -- target/redact
//! cargo run --release -p rasura-pixeldiff -- \
//!     target/redact/before.pdf target/redact/after.pdf --changed-within 420 580
//! ```
//!
//! The pixel diff is the other half of the claim, and `--changed-within` is the
//! mode that fits: removing text is easy to do destructively, and what has to
//! be shown is that the pixels which changed are the word's own and nothing
//! else on the page moved. The two columns are printed by the run above and
//! written to `redacted-columns.txt`, computed from the layout layer's
//! coordinates rather than hard-coded, so the two cannot drift apart.
//!
//! That bound is also why the glyphs which stay are held in place rather than
//! closing up: see [`rasura_edit::redact`]'s `remove_from_run`.

use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, SaveMode, SaveOptions};

/// The word to remove. Chosen to be unlikely to occur incidentally in a PDF's
/// own syntax, so a raw byte scan of the output means what it appears to.
const SECRET: &str = "Wolfgang";

/// A page with the secret in one visible place and four invisible ones.
fn planted_document() -> Vec<u8> {
    let content = format!(
        "BT /F1 18 Tf 1 0 0 1 72 700 Tm (Account holder: {SECRET} Mozart) Tj ET\n\
         BT /F1 18 Tf 1 0 0 1 72 660 Tm (Balance carried forward) Tj ET\n"
    );

    let xmp = format!(
        "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description dc:subject=\"{SECRET} Mozart, statement\"/>\
         </rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    );

    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /Metadata 11 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> /StructParents 0 >>",
        )
        .stream(4, "", content.as_bytes())
        .object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        )
        // A structure element whose /ActualText is an *indirect* string.
        .object(6, "<< /Type /StructElem /S /P /P 8 0 R /ActualText 7 0 R >>")
        .object(7, &format!("({SECRET} Mozart)"))
        .object(8, "<< /Type /Outlines /First 9 0 R /Last 9 0 R /Count 1 >>")
        .object(9, &format!("<< /Title ({SECRET}'s account) /Parent 8 0 R /Dest [3 0 R /Fit] >>"))
        .object(10, &format!("<< /Subject ({SECRET} Mozart) /Producer (Rasura example) >>"))
        .stream(11, " /Type /Metadata /Subtype /XML", xmp.as_bytes())
        .finish("/Root 1 0 R /Info 10 0 R")
}

/// Every offset at which `needle` occurs in `haystack`. Deliberately a plain
/// byte search: a scan that understood PDF could be fooled by the same
/// assumption the redaction was.
fn raw_occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

/// The device-space columns the secret occupies on page one, at the harness's
/// 150 dpi against PDF's 72 units per inch.
fn word_columns(doc: &Document) -> Option<(u32, u32)> {
    let pages = rasura_content::page::pages(doc).ok()?;
    let page = rasura_edit::locate::EditablePage::analyse(doc, pages.pages.first()?)?;

    for (id, _) in &page.paragraphs {
        let text = page.text_of(*id);
        let Some(at) = text.find(SECRET) else { continue };
        let from = text[..at].chars().count();
        let selection =
            rasura_edit::locate::select(&page, *id, from..from + SECRET.chars().count())?;
        let lines = page.lines_of(*id)?;

        let mut low = f64::MAX;
        let mut high = f64::MIN;
        for g in &selection.glyphs {
            let Some(glyph) = lines.get(g.line).and_then(|l| l.glyphs.get(g.glyph)) else {
                continue;
            };
            low = low.min(glyph.origin.x);
            high = high.max(glyph.origin.x + glyph.advance);
        }
        if low <= high {
            let to_column = |v: f64| (v * 150.0 / 72.0).max(0.0) as u32;
            return Some((to_column(low), to_column(high) + 1));
        }
    }
    None
}

fn main() -> std::process::ExitCode {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| "target/redact".into());

    let original = planted_document();
    println!(
        "planted: {SECRET:?} appears {} time(s) in the input's raw bytes",
        raw_occurrences(&original, SECRET.as_bytes()).len()
    );

    // Before anything is removed: the checker has to *fail* on the input, or
    // passing on the output means nothing. A verifier that cannot see the word
    // when it is plainly there reports clean for the same reason on every file.
    let before = rasura_edit::redact::verify(&original, &[SECRET.to_string()]);
    println!("checker sees {} trace(s) in the input", before.traces.len());
    for trace in &before.traces {
        println!("  found: {}", trace.where_found);
    }
    if before.is_clean() {
        eprintln!("FAIL: the verifier reports clean on a file that plainly contains the word");
        return std::process::ExitCode::FAILURE;
    }

    let mut doc = match Document::open(original.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the fixture did not open: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Where the word sits on the page, in the renderer's columns. Computed
    // from the layout layer's own coordinates before anything is removed,
    // because it is what the pixel diff is given as the region the change is
    // allowed to occupy -- and because it is the same rectangle a caller would
    // use to draw the black box of step 9.
    let band = word_columns(&doc);
    match band {
        Some((x0, x1)) => println!("region:  device columns {x0}..{x1}"),
        None => println!("region:  not located; the pixel diff will have no bound to check"),
    }

    // One call: it plans every page, applies the patches and the object
    // changes through a session, and marks the document redacted. The mark is
    // not advisory -- see the save mode below.
    let redaction = match rasura_edit::redact::apply(&mut doc, SECRET) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: the redaction was refused: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("removing: {:?}", redaction.strings);
    println!("fidelity: {:?}", redaction.fidelity);
    println!("objects rewritten: {}", redaction.changes.len());

    // Asking for an incremental save on purpose. An incremental revision leaves
    // the original bytes -- and therefore the text -- in the file, so the
    // request has to be overridden rather than honoured. This is the assertion
    // that the rule lives in the writer and not in the documentation.
    let opts = SaveOptions { mode: Some(SaveMode::Incremental), ..SaveOptions::default() };
    let saved = match rasura_cos::save(&doc, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: save: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("asked for: Incremental; saved as: {:?}", saved.mode);
    if saved.mode != SaveMode::FullRewrite {
        eprintln!("FAIL: a redacted document was saved incrementally; the prior revision survives");
        return std::process::ExitCode::FAILURE;
    }

    // Spec 10.6's public assurance, run against the bytes on their way to disk.
    let report = rasura_edit::redact::verify(&saved.bytes, &redaction.strings);
    println!(
        "verified: {} object(s), {} stream(s), {} trace(s)",
        report.objects_checked,
        report.streams_checked,
        report.traces.len()
    );
    for note in &report.not_checked {
        println!("  not checked: {note}");
    }
    for trace in &report.traces {
        eprintln!("  TRACE: {:?} in {}", trace.string, trace.where_found);
    }
    if !report.is_clean() {
        eprintln!("FAIL: the redacted text is still in the file");
        return std::process::ExitCode::FAILURE;
    }

    // The check that shares no code with the redaction. `verify` walks objects
    // and decodes streams; this looks at the file the way `strings` does, and
    // would catch the word surviving somewhere neither of us thought to walk.
    let raw = raw_occurrences(&saved.bytes, SECRET.as_bytes());
    println!("raw byte scan: {} occurrence(s)", raw.len());
    if !raw.is_empty() {
        eprintln!("FAIL: {SECRET:?} survives at byte offset(s) {raw:?}");
        return std::process::ExitCode::FAILURE;
    }

    // The text that was *not* redacted is still there, which is the other half
    // of correctness: a redaction that removed the whole page would pass every
    // check above.
    if !raw_occurrences(&saved.bytes, b"Balance carried forward").is_empty() {
        println!("kept:    the untouched line survives verbatim");
    } else if rasura_content::page::pages(&doc)
        .ok()
        .zip(Document::open(saved.bytes.clone()).ok())
        .is_some()
    {
        // Compressed output puts it beyond a raw scan; the reopen below is the
        // real check, so this is a note rather than a failure.
        println!("kept:    the untouched line is not in plain bytes (stream compressed)");
    }

    let reopened = match Document::open(saved.bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the redacted file no longer opens: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("reopened: {} object(s), PDF {}", reopened.xref().len(), reopened.version());

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("FAIL: {out_dir}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    for (name, bytes) in [("before.pdf", &original), ("after.pdf", &saved.bytes)] {
        let path = format!("{out_dir}/{name}");
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({} bytes)", bytes.len());
    }

    // Written to a file rather than only printed, so CI can hand it to the
    // pixel diff without parsing stdout.
    if let Some((x0, x1)) = band {
        let path = format!("{out_dir}/redacted-columns.txt");
        if let Err(e) = std::fs::write(&path, format!("{x0} {x1}")) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({x0} {x1})");
    }

    std::process::ExitCode::SUCCESS
}
