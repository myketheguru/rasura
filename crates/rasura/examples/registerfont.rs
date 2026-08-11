//! Supplying a font, end to end, for a renderer to judge. Spec 11.3, 8.4.
//!
//! > `fontRequirements()` run immediately after `open()` lets a consuming
//! > application fetch exactly the fonts it needs before the user starts
//! > typing. It turns the worst constraint of the platform into a solvable,
//! > visible task.
//!
//! This is that task carried out. A document embeds a subset of Roboto holding
//! the two letters it uses; the user types `É`, which the subset threw away;
//! the caller has registered the full typeface, so the outline is taken out of
//! it and put *into* the document's own font.
//!
//! The distinction that matters: the page still uses one typeface. A substituted
//! `É` would be visibly different from the letters around it and identical in
//! every diff. An injected one is the same font, with one more glyph in it.
//!
//! ```text
//! ./corpus/fetch-font.sh
//! cargo run -p rasura --example registerfont -- target/registerfont
//! cargo run --release -p rasura-pixeldiff -- \
//!     target/registerfont/before.pdf target/registerfont/after.pdf \
//!     --unchanged-before 150
//! ```
//!
//! The pixel check is `--unchanged-before`, not `--identical`: the first letter
//! is deliberately different, so what has to hold is that nothing *before* it
//! moved. `É` is wider than `H`, so everything after it shifts — which is what
//! replacing a character with a wider one does, and not a defect.
//!
//! It also writes `js/test/subset.pdf`, which is the fixture the npm package's
//! own test suite uses. Generated rather than committed, because it is derived
//! from a font the repository does not carry.

use rasura::{Document, RegisterOptions, SaveOptions};

const PRESENT: &str = "Hi";
const TYPED: char = 'É';

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().unwrap_or_else(|| "target/registerfont".into());
    let font_path = args.next().unwrap_or_else(|| "corpus/fonts/Roboto-Regular.ttf".into());

    let roboto = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL: {font_path}: {e}");
            eprintln!("      run ./corpus/fetch-font.sh first");
            return std::process::ExitCode::FAILURE;
        }
    };

    let Some(before) = subset_document(&roboto) else {
        eprintln!("FAIL: could not build the subset fixture");
        return std::process::ExitCode::FAILURE;
    };
    println!("subset: a document embedding only {PRESENT:?}");

    let mut doc = match Document::open(before.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the fixture did not open: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Spec 11.3's opening move: ask what the document needs before typing.
    for font in doc.fonts() {
        println!(
            "needs:   {} embedded={} subset={} coverage={} supply={}",
            font.name,
            font.embedded,
            font.subset,
            font.latin_coverage.as_str(),
            font.needs_supplying()
        );
    }

    // Without a registered font the edit still happens and the missing glyph is
    // *reported*. That is the part that used to be silent: the page would draw
    // nothing where the letter belongs and no result said so.
    {
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        match session.replace_text(&page, id, 0..1, &TYPED.to_string()) {
            Ok(outcome) => {
                if outcome.missing_glyphs.is_empty() {
                    eprintln!("FAIL: the subset has no {TYPED} and nothing reported it");
                    return std::process::ExitCode::FAILURE;
                }
                println!("unregistered: missing {:?}", outcome.missing_glyphs);
            }
            Err(e) => {
                eprintln!("FAIL: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        let _ = session.rollback();
    }

    doc.register_font(roboto, &RegisterOptions { match_for: Some("Roboto-Regular".into()) });
    println!("registered: the full typeface");

    let page = doc.page(0).expect("page");
    let id = page.paragraphs()[0].id;
    let mut session = doc.edit();
    let outcome = match session.replace_text(&page, id, 0..1, &TYPED.to_string()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("FAIL: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("fidelity: {} {:?}", outcome.fidelity.as_str(), outcome.warnings);
    if outcome.fidelity != rasura::edit::Fidelity::Reembedded {
        eprintln!("FAIL: expected the reembedded rung, got {:?}", outcome.fidelity);
        return std::process::ExitCode::FAILURE;
    }
    if !outcome.missing_glyphs.is_empty() {
        eprintln!("FAIL: {TYPED} is still missing: {:?}", outcome.missing_glyphs);
        return std::process::ExitCode::FAILURE;
    }

    let saved = match session.commit(&SaveOptions::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: commit: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Read it back through the same API: the only proof the glyph is reachable
    // rather than merely present.
    let after_doc = match Document::open(saved.bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the edited document does not open: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let text = after_doc.page(0).expect("page").paragraphs()[0].text.clone();
    if !text.starts_with(TYPED) {
        eprintln!("FAIL: the injected character does not read back: {text:?}");
        return std::process::ExitCode::FAILURE;
    }
    println!("re-read:  {text:?}");

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("FAIL: {out_dir}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    for (name, bytes) in [("before.pdf", &before), ("after.pdf", &saved.bytes)] {
        let path = format!("{out_dir}/{name}");
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({} bytes)", bytes.len());
    }

    // The npm package's suite needs the same fixture, and cannot build one:
    // it has no font tooling. Written here rather than committed, because it is
    // derived from a typeface the repository does not carry.
    if std::path::Path::new("js/test").is_dir()
        && let Err(e) = std::fs::write("js/test/subset.pdf", &before)
    {
        eprintln!("note: could not write js/test/subset.pdf: {e}");
    } else if std::path::Path::new("js/test").is_dir() {
        println!("wrote:   js/test/subset.pdf");
    }

    std::process::ExitCode::SUCCESS
}

/// A document embedding a subset of `roboto` holding only [`PRESENT`].
fn subset_document(roboto: &[u8]) -> Option<Vec<u8>> {
    use rasura_cos::testutil::ClassicBuilder;

    let font = rasura_font::Sfnt::parse(roboto).ok()?;
    let cmap = rasura_font::Cmap::parse(roboto, &font)?;
    let table = cmap.best_unicode()?;

    let gids: Vec<u16> = PRESENT.chars().filter_map(|c| table.lookup(roboto, c as u32)).collect();
    let subset = rasura_font::compact_truetype(roboto, &gids).ok()?;
    let subset_font = rasura_font::Sfnt::parse(&subset.bytes).ok()?;
    let mapped: Vec<(u32, u16)> =
        PRESENT.chars().zip(&gids).map(|(c, g)| (c as u32, subset.mapping[g])).collect();
    let program = rasura_font::add_mappings(&subset.bytes, &subset_font, &mapped).ok()?;

    let per_em = font.units_per_em.max(1) as f64;
    let width = |c: char| {
        let gid = table.lookup(roboto, c as u32).unwrap_or(0);
        (font.advance(roboto, gid).unwrap_or(0) as f64 * 1000.0 / per_em).round() as i64
    };
    // /Widths spans the whole code range with zeros between, because it is
    // indexed by code from /FirstChar and not by order of appearance.
    let widths: Vec<String> = (72..=105)
        .map(|code| match char::from_u32(code) {
            Some(c) if PRESENT.contains(c) => width(c).to_string(),
            _ => "0".to_string(),
        })
        .collect();

    Some(
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 36 Tf 1 0 0 1 72 700 Tm (Hi) Tj ET\n")
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Roboto-Regular \
                     /FirstChar 72 /LastChar 105 /Widths [{}] \
                     /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
                    widths.join(" ")
                ),
            )
            .object(
                6,
                "<< /Type /FontDescriptor /FontName /ABCDEF+Roboto-Regular /Flags 32 \
                 /FontBBox [-737 -271 1148 1056] /ItalicAngle 0 /Ascent 928 /Descent -244 \
                 /CapHeight 711 /StemV 80 /FontFile2 7 0 R >>",
            )
            .stream(7, &format!(" /Length1 {}", program.len()), &program)
            .finish("/Root 1 0 R"),
    )
}
