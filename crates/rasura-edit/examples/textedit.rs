//! A text edit, end to end, written out for external judgement. Spec 14.2 I2.
//!
//! Every test in this crate checks its own output: the splice engine's spans
//! are compared against the buffer it produced, the encoder's codes against the
//! decoder that made them. That catches a great deal and proves nothing to
//! anyone else.
//!
//! This builds a **two-page** document and edits a word on page one. Two pages
//! is the point — spec §14.2's second invariant is not about the page that
//! changed:
//!
//! > **I2 — Locality.** After editing page *N*, every other page renders
//! > pixel-identical and every object not on page *N* is byte-identical.
//!
//! The failure that catches is one no single-page fixture can express: an edit
//! whose effect leaks through something the pages share. Rewriting a font
//! dictionary in place, renumbering a `/Resources` entry, repacking an object
//! stream — each leaves the edited page correct and changes a page nobody
//! touched. Only rendering the other page says so.
//!
//! ```text
//! cargo run -p rasura-edit --example textedit -- target/textedit
//! cargo run -p rasura-pixeldiff -- \
//!     target/textedit/before.pdf target/textedit/after.pdf --page 2 --identical
//! ```

use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, SaveOptions};
use rasura_edit::locate::EditablePage;
use rasura_edit::reflow::{Breaking, Overflow, Policy};
use rasura_edit::{EditSession, replace_text};

/// Two pages sharing one font object, which is what makes the locality check
/// meaningful: a careless edit has something to leak through.
fn two_page_document() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", b"BT /F1 24 Tf 1 0 0 1 72 700 Tm (The first page says hello) Tj ET\n")
        .object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        )
        .object(
            6,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(7, "", b"BT /F1 24 Tf 1 0 0 1 72 700 Tm (The second page is untouched) Tj ET\n")
        .finish("/Root 1 0 R")
}

fn main() -> std::process::ExitCode {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| "target/textedit".into());

    let original = two_page_document();
    let mut doc = match Document::open(original.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the fixture did not open: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let pages = match rasura_content::page::pages(&doc) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: no page tree: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let Some(page) = EditablePage::analyse(&doc, &pages.pages[0]) else {
        eprintln!("FAIL: page one did not analyse");
        return std::process::ExitCode::FAILURE;
    };

    let Some((id, _)) = page.paragraphs.first() else {
        eprintln!("FAIL: page one has no paragraph");
        return std::process::ExitCode::FAILURE;
    };
    let text = page.text_of(*id);
    println!("before:  {text:?}");

    let Some(at) = text.find("hello") else {
        eprintln!("FAIL: the fixture text changed");
        return std::process::ExitCode::FAILURE;
    };

    // Where the edit begins on the page, so the pixel diff can assert that
    // nothing *upstream* of it moved. Computing it here rather than hard-coding
    // it in CI keeps the two from drifting apart when the fixture changes.
    let edit_column = rasura_edit::locate::select(&page, *id, at..at + 5)
        .and_then(|s| {
            let lines = page.lines_of(*id)?;
            s.glyphs
                .iter()
                .filter_map(|g| lines.get(g.line)?.glyphs.get(g.glyph))
                .map(|g| g.origin.x)
                .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.min(x))))
        })
        // 150 dpi against PDF's 72 units per inch, matching the harness.
        .map(|x| (x * 150.0 / 72.0).floor().max(0.0) as u32);
    println!("edit starts at device column {edit_column:?}");

    // A replacement of the same character count but a different width, so the
    // glyphs after it genuinely move and the diff has something to find.
    //
    // `--grow` replaces it with far more text instead, to show what a paragraph
    // that no longer fits actually does: it re-breaks into extra lines that
    // *overlap* whatever sits below, because PDF has no flow and the block
    // beneath is positioned absolutely.
    let grow = std::env::args().any(|a| a == "--grow");
    let replacement = if grow {
        "hello and a great deal more besides, far more than ever fitted on one line of \
         this page, so that the paragraph has to find somewhere to put the remainder"
    } else {
        "WORLD"
    };

    let policy = Policy { breaking: Breaking::Greedy, overflow: Overflow::Allow };
    let edit = match replace_text(&doc, &page, *id, at..at + 5, replacement, policy) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: the edit was refused: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("after:   {:?}", edit.text_after);
    println!("fidelity: {:?}", edit.fidelity);
    println!("patches: {}", edit.patches.len());

    let content = page.content;
    let mut session = EditSession::new(&mut doc);
    let report =
        match session.patch_content("replace hello", &content, &edit.patches, edit.fidelity) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("FAIL: the patch did not apply: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
    println!("touched: {:?}, {:+} content byte(s)", report.objects_touched, report.bytes_delta);

    let saved = match session.commit(&SaveOptions::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: commit: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("saved:   {:?}, {} byte(s) appended", saved.mode, saved.bytes_appended);

    // The object-level half of I2, asserted here so the renderer is confirming
    // rather than discovering it.
    if !saved.bytes.starts_with(&original) {
        eprintln!("FAIL: the incremental save rewrote original bytes.");
        return std::process::ExitCode::FAILURE;
    }

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

    // Written to a file rather than only printed, so CI can pass it to the
    // pixel diff without parsing stdout.
    if let Some(column) = edit_column {
        let path = format!("{out_dir}/edit-column.txt");
        if let Err(e) = std::fs::write(&path, column.to_string()) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({column})");
    }

    std::process::ExitCode::SUCCESS
}
