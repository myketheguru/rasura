//! Compose a fixed document, render it, and compare against a stored image.
//!
//!   cargo run -p rasura-pixeldiff --bin golden -- corpus/golden
//!   cargo run -p rasura-pixeldiff --bin golden -- corpus/golden --bless
//!
//! pdfium confirms there is ink on the page and pdf.js confirms the text reads
//! back. Neither says the typesetting is *right*. Three keep-with-next bugs and
//! a float comparison that cost a line per column were all found by looking at
//! a rendered page while the suite was green, which is not a process that
//! scales past the person who happened to look.
//!
//! This does not know good typography from bad either. What it knows is when
//! the output **changed**, which is exactly what that sequence needed and did
//! not have: every one of those fixes altered the rendered page, and none of
//! them altered a test result.
//!
//! `--bless` writes the current render as the new reference. Deliberately a
//! flag rather than automatic: a golden that updates itself when it disagrees
//! is a golden that agrees with everything.

use pdfium_render::prelude::*;
use rasura_flow::compose::{Options, compose};
use rasura_flow::flow::{Block, FlowDocument, Inline};
use rasura_layout::frames::PageGeometry;
use std::process::ExitCode;

/// Anti-aliasing differs by a unit or two between pdfium builds, and a golden
/// that fails on a renderer upgrade teaches its reader to bless without looking.
const TOLERANCE: u8 = 12;

/// Above this many differing pixels the page really did change. A handful can
/// be hinting noise at a glyph edge; a line moving is thousands.
const ALLOWED: usize = 200;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "corpus/golden".into());
    let bless = std::env::args().any(|a| a == "--bless");

    let font = match std::fs::read("corpus/fonts/Roboto-Regular.ttf") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: corpus/fonts/Roboto-Regular.ttf: {e}");
            eprintln!("      run ./corpus/fetch-font.sh first");
            return ExitCode::FAILURE;
        }
    };

    let pdfium = match Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(
        "./target/pdfium/",
    ))
    .or_else(|_| Pdfium::bind_to_system_library())
    {
        Ok(b) => Pdfium::new(b),
        Err(e) => {
            eprintln!("SETUP: pdfium not found ({e}).");
            eprintln!("       Run harness/pixeldiff/fetch.sh first.");
            return ExitCode::FAILURE;
        }
    };

    let _ = std::fs::create_dir_all(&dir);
    let mut failed = false;

    for (name, opts) in cases() {
        match check(&pdfium, &dir, name, &font, &opts, bless) {
            Ok(message) => println!("{name:10} {message}"),
            Err(message) => {
                eprintln!("{name:10} FAIL: {message}");
                failed = true;
            }
        }
    }

    if failed {
        eprintln!("\nThe composed output changed. Look at the diff artefacts before blessing:");
        eprintln!("  cargo run -p rasura-pixeldiff --bin golden -- {dir} --bless");
        return ExitCode::FAILURE;
    }
    println!("\nthe composed pages render as they did.");
    ExitCode::SUCCESS
}

/// The documents held to a reference. Each exercises something the others do
/// not, because a golden that only covers one shape only protects one shape.
fn cases() -> Vec<(&'static str, Options)> {
    vec![
        // Headings, pagination and keep-with-next: the case whose bugs prompted
        // this whole harness.
        ("report", Options::default()),
        // The same text at half the measure. Column breaks land in different
        // places, which is where the keep-with-next faults appeared.
        (
            "columns",
            Options { geometry: PageGeometry::us_letter().with_columns(2), ..Options::default() },
        ),
    ]
}

fn check(
    pdfium: &Pdfium,
    dir: &str,
    name: &str,
    font: &[u8],
    opts: &Options,
    bless: bool,
) -> Result<String, String> {
    let (doc, report) = compose(&document(), font, opts).map_err(|e| e.to_string())?;
    let bytes = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default())
        .map_err(|e| e.to_string())?
        .bytes;

    let pdf = format!("{dir}/{name}.pdf");
    std::fs::write(&pdf, &bytes).map_err(|e| e.to_string())?;

    let rendered = render(pdfium, &pdf)?;
    let golden = format!("{dir}/{name}.png");

    if bless || !std::path::Path::new(&golden).exists() {
        rendered.save(&golden).map_err(|e| e.to_string())?;
        return Ok(format!(
            "reference written, {} page(s), {} line(s)",
            report.pages, report.lines
        ));
    }

    let reference = image::open(&golden).map_err(|e| format!("{golden}: {e}"))?.to_rgb8();
    if reference.dimensions() != rendered.dimensions() {
        return Err(format!(
            "the page size changed: reference is {:?}, now {:?}",
            reference.dimensions(),
            rendered.dimensions()
        ));
    }

    let differing = reference
        .pixels()
        .zip(rendered.pixels())
        .filter(|(a, b)| a.0.iter().zip(b.0.iter()).any(|(x, y)| x.abs_diff(*y) > TOLERANCE))
        .count();

    if differing > ALLOWED {
        // The artefact is the point of the failure: a number of changed pixels
        // tells nobody what moved.
        let actual = format!("{dir}/{name}.actual.png");
        rendered.save(&actual).map_err(|e| e.to_string())?;
        return Err(format!(
            "{differing} pixels differ from the reference. Compare {golden} against {actual}"
        ));
    }

    Ok(format!("matches the reference ({differing} px within tolerance), {} page(s)", report.pages))
}

/// Every page, stacked into one image.
///
/// The first version rendered page one, and a deliberately reintroduced
/// keep-with-next bug passed it: the fault happens at page and column
/// boundaries, and page one of a short document has neither. A golden that
/// covers the first page of a document that does not paginate is a golden that
/// checks the easy part.
fn render(pdfium: &Pdfium, path: &str) -> Result<image::RgbImage, String> {
    let doc = pdfium.load_pdf_from_file(path, None).map_err(|e| format!("{path}: {e}"))?;
    // 100 dpi rather than 150. Enough that a line moving by a point shifts more
    // than a pixel, and it keeps a four-page reference under half a megabyte.
    let config = PdfRenderConfig::new().set_target_width(850);

    let mut pages = Vec::new();
    for page in doc.pages().iter() {
        let bitmap = page.render_with_config(&config).map_err(|e| format!("{path}: {e}"))?;
        pages.push(bitmap.as_image().map_err(|e| format!("{path}: {e}"))?.into_rgb8());
    }
    if pages.is_empty() {
        return Err(format!("{path} has no pages"));
    }

    let width = pages.iter().map(image::GenericImageView::width).max().unwrap_or(1);
    let height: u32 = pages.iter().map(image::GenericImageView::height).sum();
    let mut stacked = image::RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
    let mut y = 0;
    for page in &pages {
        image::imageops::replace(&mut stacked, page, 0, i64::from(y));
        y += page.height();
    }
    Ok(stacked)
}

/// The document under test. Fixed, and it has to stay fixed: changing it
/// invalidates every reference, which is a decision rather than an edit.
fn document() -> FlowDocument {
    let body = [
        "A PDF is a page-description format. There are no paragraphs in one, no \
         words, frequently no spaces, only positioned glyph runs. Everything a \
         reader recovers about the structure of a page was reconstructed.",
        "This document was not edited from another. It began as a list of blocks \
         with no page size, no line breaks and no idea of where anything would \
         go. The measure, the leading, the pagination and the position of every \
         line were decided by the layout engine.",
        "The text is broken to the width it is drawn at, because the widths used \
         for line breaking are read from the same table the font dictionary's \
         own widths are written from.",
    ];

    let para = |t: &str| Block::Paragraph { inlines: vec![Inline::text(t)], source: None };
    let heading =
        |level: u8, t: &str| Block::Heading { level, inlines: vec![Inline::text(t)], source: None };

    let mut blocks = vec![heading(1, "Composed from nothing"), para(body[0])];
    // Short sections, so headings land near column and page boundaries
    // repeatedly. That arrangement is what the keep-with-next bugs needed to
    // show, and a golden over one heading would not have caught any of them.
    for i in 0..26 {
        blocks.push(heading(if i % 3 == 0 { 2 } else { 3 }, &format!("Section {}", i + 1)));
        blocks.push(para(body[i % body.len()]));
    }
    FlowDocument { blocks, ..FlowDocument::default() }
}
