//! A page that did not exist, drawn from scratch. Spec 17, Phase 6.
//!
//! Every other fixture in this crate edits content a producer wrote. This one
//! creates a page, fills it with operators from [`Canvas`], links it into the
//! page tree and commits — so the renderer is judging the draw-command emitter
//! rather than the patcher.
//!
//! The check that matters is on the *other* pages: inserting must not disturb
//! them. Spec 14.2's I2 is about exactly that, and an insertion has more ways
//! to go wrong than an edit does — a `/Count` left stale, a `/Kids` entry in the
//! wrong slot, an object number colliding with one already in the file.
//!
//! ```text
//! cargo run -p rasura-edit --example newpage -- target/newpage
//! cargo run -p rasura-pixeldiff -- \
//!     target/newpage/before.pdf target/newpage/after.pdf --page 1 --identical
//! ```

use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, SaveOptions};
use rasura_edit::{Canvas, EditSession, NumberStyle, PageSpec, insert_page};

/// Two pages with text, so an inserted third has neighbours to disturb.
fn document() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", b"BT /F1 24 Tf 1 0 0 1 72 700 Tm (The first page) Tj ET\n")
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
        .stream(7, "", b"BT /F1 24 Tf 1 0 0 1 72 700 Tm (The last page) Tj ET\n")
        .finish("/Root 1 0 R")
}

fn main() -> std::process::ExitCode {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| "target/newpage".into());
    let original = document();

    let mut doc = match Document::open(original.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: {e}");
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
    println!("before:  {} page(s)", pages.pages.len());

    // Draw the new page: a filled bar, an outlined box, and a line of text in
    // the font the neighbouring pages already carry.
    let mut canvas = Canvas::new(NumberStyle::default());
    canvas.save().fill_rgb(0.85, 0.1, 0.1).rect(72.0, 640.0, 468.0, 24.0).fill().restore();
    canvas
        .save()
        .stroke_rgb(0.0, 0.0, 0.0)
        .line_width(2.0)
        .rect(72.0, 400.0, 200.0, 200.0)
        .stroke()
        .restore();
    canvas.text_line(&rasura_cos::Name::new("F1"), 24.0, 72.0, 700.0, b"An inserted page");

    let content = match canvas.finish() {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("FAIL: the drawing is unbalanced: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("drew:    {} byte(s) of operators", content.len());

    // The page needs the font in its own /Resources: a page inherits only what
    // its ancestors declare, and this tree declares nothing.
    let mut fonts = rasura_cos::Dictionary::new();
    fonts.insert(
        rasura_cos::Name::new("F1"),
        rasura_cos::Object::Reference(rasura_cos::ObjId::new(5, 0)),
    );
    let mut resources = rasura_cos::Dictionary::new();
    resources.insert(rasura_cos::Name::new("Font"), rasura_cos::Object::Dictionary(fonts));

    let spec = PageSpec { content, resources: Some(resources), ..PageSpec::default() };
    let edit = match insert_page(&mut doc, &pages, 1, &spec) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: the insert was refused: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("insert:  {} object(s) changed", edit.changes.len());

    let mut session = EditSession::new(&mut doc);
    if let Err(e) = session.set_objects("insert page", &edit.changes, edit.fidelity) {
        eprintln!("FAIL: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let saved = match session.commit(&SaveOptions::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: commit: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if !saved.bytes.starts_with(&original) {
        eprintln!("FAIL: the incremental save rewrote original bytes.");
        return std::process::ExitCode::FAILURE;
    }
    println!("saved:   {} byte(s) appended", saved.bytes_appended);

    let after = match Document::open(saved.bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: reopen: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let tree = match rasura_content::page::pages(&after) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FAIL: the edited page tree: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if tree.pages.len() != 3 {
        eprintln!("FAIL: {} page(s) after inserting one into two", tree.pages.len());
        return std::process::ExitCode::FAILURE;
    }
    println!("after:   {} page(s)", tree.pages.len());

    // The new page must extract the text it was told to draw, through the same
    // chain any reader uses.
    let Some(page) = rasura_edit::EditablePage::analyse(&after, &tree.pages[1]) else {
        eprintln!("FAIL: the inserted page did not analyse");
        return std::process::ExitCode::FAILURE;
    };
    let text: String = page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect();
    if !text.contains("An inserted page") {
        eprintln!("FAIL: the inserted page reads {text:?}");
        return std::process::ExitCode::FAILURE;
    }
    println!("checked: the new page reads {text:?}");

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
    std::process::ExitCode::SUCCESS
}
