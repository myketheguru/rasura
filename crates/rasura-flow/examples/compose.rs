//! Compose a document from a flow model: headings, prose, columns, a typeface.
//!
//!   ./corpus/fetch-font.sh
//!   cargo run -p rasura-flow --example compose -- target/composed
//!
//! `crates/rasura-edit/examples/compose.rs` is the same claim one layer down —
//! a page, a font, positioned text. This is the layer a person would actually
//! use: a document is described as blocks, and where the lines go is the
//! engine's problem.
//!
//! Three documents, because the interesting failures are in the differences:
//!
//!   report.pdf    one column, headings, enough prose to paginate
//!   columns.pdf   the same text in two columns, which must not change a word
//!   greek.pdf     text a simple font cannot encode, so a Type0 font is used

use rasura_flow::compose::{Options, compose};
use rasura_flow::flow::{Block, FlowDocument, Inline};
use rasura_layout::frames::PageGeometry;
use std::process::ExitCode;

fn main() -> ExitCode {
    let out = std::env::args().nth(1).unwrap_or_else(|| "target/composed".to_string());
    let dir = std::path::Path::new(&out);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("FAIL: {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }

    let font_path = "corpus/fonts/Roboto-Regular.ttf";
    let program = match std::fs::read(font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL: {font_path}: {e}\n      run ./corpus/fetch-font.sh first");
            return ExitCode::FAILURE;
        }
    };

    let jobs: Vec<(&str, FlowDocument, Options)> = vec![
        (
            "report",
            report(),
            Options { title: Some("A composed report".into()), ..Options::default() },
        ),
        (
            "columns",
            report(),
            Options {
                geometry: PageGeometry::us_letter().with_columns(2),
                title: Some("The same report, in two columns".into()),
                ..Options::default()
            },
        ),
        ("greek", greek(), Options::default()),
    ];

    let mut failed = false;
    for (name, flow, opts) in jobs {
        match compose(&flow, &program, &opts) {
            Ok((doc, report)) => {
                let saved = match rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("FAIL: {name}: {e}");
                        failed = true;
                        continue;
                    }
                };
                let path = dir.join(format!("{name}.pdf"));
                if let Err(e) = std::fs::write(&path, &saved.bytes) {
                    eprintln!("FAIL: {}: {e}", path.display());
                    failed = true;
                    continue;
                }
                println!(
                    "{name:8} {} page(s), {} line(s), {} -> {} ({} bytes)",
                    report.pages,
                    report.lines,
                    if report.composite { "Type0" } else { "TrueType" },
                    path.display(),
                    saved.bytes.len(),
                );
                if !report.missing.is_empty() {
                    println!("         missing: {:?}", report.missing);
                }
                failed |= !verify(&saved.bytes, &report);
            }
            Err(e) => {
                eprintln!("FAIL: {name}: {e}");
                failed = true;
            }
        }
    }

    if failed {
        return ExitCode::FAILURE;
    }
    println!("\ncomposed from a flow model, with no input document.");
    ExitCode::SUCCESS
}

/// The bytes reopen, every page has text, and the page count is what was
/// reported. A composition that reports four pages and writes three is the
/// failure worth catching here.
fn verify(bytes: &[u8], report: &rasura_flow::compose::Report) -> bool {
    let doc = match rasura_cos::Document::open(bytes.to_vec()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("         FAIL: does not reopen: {e}");
            return false;
        }
    };
    let mut ok = true;
    if !doc.leniencies().is_empty() {
        eprintln!("         FAIL: reopening needed leniencies: {:?}", doc.leniencies());
        ok = false;
    }
    let tree = match rasura_content::page::pages(&doc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("         FAIL: no page tree: {e}");
            return false;
        }
    };
    if tree.pages.len() != report.pages {
        eprintln!("         FAIL: reported {} page(s), wrote {}", report.pages, tree.pages.len());
        ok = false;
    }
    for (i, page) in tree.pages.iter().enumerate() {
        if rasura_layout::page_text(&doc, page).trim().is_empty() {
            eprintln!("         FAIL: page {} is blank", i + 1);
            ok = false;
        }
    }
    ok
}

fn para(text: &str) -> Block {
    Block::Paragraph { inlines: vec![Inline::text(text)], source: None }
}

fn heading(level: u8, text: &str) -> Block {
    Block::Heading { level, inlines: vec![Inline::text(text)], source: None }
}

fn report() -> FlowDocument {
    let body = [
        "A PDF is a page-description format. There are no paragraphs in one, no \
         words, frequently no spaces — only positioned glyph runs. Everything a \
         reader recovers about the structure of a page was reconstructed, and the \
         library that reconstructs it can also run the process backwards.",
        "This document was not edited from another. It began as a list of blocks: \
         two headings and some prose, with no page size, no line breaks and no \
         idea of where anything would go. The measure, the leading, the \
         pagination and the position of every line were decided by the layout \
         engine, and the glyphs come from a typeface subset into the file.",
        "The text is broken to the width it is drawn at. That sounds like a \
         tautology and is not: the widths used for line breaking are read from \
         the same table the font dictionary's own widths are written from, so a \
         line that was measured to fit does fit. Laying out with one font's \
         metrics and drawing with another's is the ordinary way for composed \
         text to overflow its column.",
        "Nothing here is guessed except one number. No table in a TrueType font \
         records the vertical stem width a PDF font descriptor asks for, so it is \
         estimated from the weight class and reported as an estimate, which is \
         the same rule this library follows everywhere else.",
    ];

    let mut blocks = vec![heading(1, "Composed from nothing")];
    blocks.push(para(body[0]));
    blocks.push(heading(2, "What this is"));
    blocks.push(para(body[1]));
    blocks.push(para(body[2]));
    blocks.push(heading(2, "What is estimated"));
    blocks.push(para(body[3]));
    // Enough repetition to force a second page, so pagination is exercised
    // rather than assumed.
    for i in 0..6 {
        blocks.push(heading(3, &format!("Section {}", i + 1)));
        blocks.push(para(body[i % body.len()]));
    }
    FlowDocument { blocks, ..FlowDocument::default() }
}

fn greek() -> FlowDocument {
    FlowDocument {
        blocks: vec![
            heading(1, "Ελληνικά"),
            para(
                "Δεν υπάρχει κωδικοποίηση WinAnsi για αυτά τα γράμματα, οπότε το \
                 έγγραφο χρησιμοποιεί γραμματοσειρά Type0 με Identity-H.",
            ),
            para(
                "The same document mixes English freely, because a composite font \
                 addresses glyphs by identifier and does not care which script \
                 they belong to.",
            ),
        ],
        ..FlowDocument::default()
    }
}
