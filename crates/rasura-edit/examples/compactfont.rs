//! Compaction subsetting on a real font, for an outside judge. Spec 8.6.
//!
//! Compaction is the operation where a mistake is least visible from inside.
//! Renumber a font's glyphs and lose track of which is which and you get a
//! document that opens, validates, extracts the right text through
//! `/ToUnicode`, and **draws the wrong letters**. Nothing but a renderer
//! notices.
//!
//! So this embeds the whole of Roboto — 515 KB, 3,387 glyphs — into a document
//! that draws twelve of them, prunes it to 13, and asks pdfium whether the page
//! changed. It does not: 515,100 bytes of font become 12,856, and the render is
//! pixel-identical.
//!
//! ```text
//! ./corpus/fetch-font.sh                      # Roboto, Apache-2.0
//! cargo run -p rasura-edit --example compactfont -- target/compactfont
//! cargo run --release -p rasura-pixeldiff -- \
//!     target/compactfont/before.pdf target/compactfont/after.pdf --identical
//! ```
//!
//! `--identical` is the right mode and a strong claim: compaction must change
//! the file's size and **not one pixel** of its rendering.
//!
//! The other half is that the content stream is untouched. Spec 8.6 warns that
//! renumbering "would require rewriting every content stream that references
//! the font"; putting the renumbering into `/CIDToGIDMap` means it requires
//! rewriting none, and this checks the bytes rather than asserting it.

use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, ObjId, Object, SaveMode, SaveOptions};
use rasura_edit::EditSession;
use rasura_edit::compact;

const TEXT: &str = "Hamburgefons";

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().unwrap_or_else(|| "target/compactfont".into());
    let font_path = args.next().unwrap_or_else(|| "corpus/fonts/Roboto-Regular.ttf".into());

    let program = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL: {font_path}: {e}");
            eprintln!("      run ./corpus/fetch-font.sh first");
            return std::process::ExitCode::FAILURE;
        }
    };

    // The glyph ids Roboto uses for the text, read from its own `cmap`. The
    // document draws them as CIDs, which is what /Identity-H with an identity
    // /CIDToGIDMap means: CID = GID.
    let sfnt = match rasura_font::Sfnt::parse(&program) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: parsing {font_path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let Some(cmap) = rasura_font::Cmap::parse(&program, &sfnt) else {
        eprintln!("FAIL: {font_path} has no usable cmap");
        return std::process::ExitCode::FAILURE;
    };
    let Some(unicode) = cmap.best_unicode() else {
        eprintln!("FAIL: {font_path} has no Unicode cmap subtable");
        return std::process::ExitCode::FAILURE;
    };

    let mut gids = Vec::new();
    for c in TEXT.chars() {
        match unicode.lookup(&program, c as u32) {
            Some(gid) => gids.push(gid),
            None => {
                eprintln!("FAIL: {font_path} has no glyph for {c:?}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    println!("font:    {} glyph(s), {} bytes", sfnt.num_glyphs, program.len());
    println!("drawing: {TEXT:?} as {} glyph id(s) {gids:?}", gids.len());

    let before = document(&program, &gids, TEXT);
    let mut doc = match Document::open(before.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the built document did not open: {e}");
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
    let report = compact::plan(&doc, &pages);
    for (name, why) in &report.skipped {
        println!("skipped: {name} -- {why}");
    }
    let Some(done) = report.compacted.first() else {
        eprintln!("FAIL: nothing was compacted");
        return std::process::ExitCode::FAILURE;
    };
    println!(
        "pruned:  {} glyph(s) -> {}, {} bytes -> {} ({:.1}% of the original)",
        done.glyphs_before,
        done.glyphs_after,
        done.bytes_before,
        done.bytes_after,
        100.0 * done.bytes_after as f64 / done.bytes_before.max(1) as f64
    );
    println!("map:     {}", if done.added_map { "created" } else { "rewritten" });
    println!("fidelity: {:?}", report.fidelity);

    {
        let mut session = EditSession::new(&mut doc);
        let changes: Vec<(ObjId, Option<Object>)> =
            report.changes.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();
        if let Err(e) = session.set_objects("compact fonts", &changes, report.fidelity.clone()) {
            eprintln!("FAIL: staging: {e}");
            return std::process::ExitCode::FAILURE;
        }
        if let Err(e) = session.commit(&SaveOptions::default()) {
            eprintln!("FAIL: commit: {e}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // A full rewrite, or the old font program stays in the file and the
    // document is larger rather than smaller -- the one thing compaction is for.
    let opts = SaveOptions { mode: Some(SaveMode::FullRewrite), ..SaveOptions::default() };
    let saved = match rasura_cos::save(&doc, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: save: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!(
        "document: {} bytes -> {} ({:.1}%)",
        before.len(),
        saved.bytes.len(),
        100.0 * saved.bytes.len() as f64 / before.len() as f64
    );
    if saved.bytes.len() >= before.len() {
        eprintln!("FAIL: the compacted document is not smaller");
        return std::process::ExitCode::FAILURE;
    }

    // The claim spec 8.6 says cannot be made: the content stream is untouched.
    let after = match Document::open(saved.bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: the compacted document does not open: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let original_content = Document::open(before.clone())
        .ok()
        .and_then(|d| d.decoded_stream(ObjId::new(4, 0)).ok())
        .map(|b| b.to_vec());
    let new_content = after.decoded_stream(ObjId::new(4, 0)).ok().map(|b| b.to_vec());
    if original_content != new_content {
        eprintln!("FAIL: the content stream changed; the renumbering leaked out of the font");
        return std::process::ExitCode::FAILURE;
    }
    println!("content: unchanged, byte for byte");

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

    std::process::ExitCode::SUCCESS
}

/// A one-page document embedding `program` whole and drawing `gids`.
fn document(program: &[u8], gids: &[u16], text: &str) -> Vec<u8> {
    let codes: String = gids.iter().map(|g| format!("{g:04X}")).collect();
    let content = format!("BT /F1 36 Tf 1 0 0 1 72 700 Tm <{codes}> Tj ET\n").into_bytes();

    // /ToUnicode, so an external reader can check the text as well as the
    // pixels. Compaction does not touch it -- CIDs are unchanged -- which is
    // itself worth having a checker confirm.
    let to_unicode = rasura_font::embed::to_unicode_cmap_with(
        &gids.iter().zip(text.chars()).map(|(g, c)| (*g as u32, c.to_string())).collect::<Vec<_>>(),
        2,
    );

    // A /W array so the advances are the font's rather than /DW's, otherwise
    // every glyph is 500 wide and the page is visibly wrong before anything is
    // compacted.
    let sfnt = rasura_font::Sfnt::parse(program).expect("parsed above");
    let widths: String = gids
        .iter()
        .map(|g| {
            let advance = sfnt.advance(program, *g).unwrap_or(500);
            let scaled = advance as f64 * 1000.0 / sfnt.units_per_em.max(1) as f64;
            format!("{g} [{}]", scaled.round() as i64)
        })
        .collect::<Vec<_>>()
        .join(" ");

    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", &content)
        .object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /Roboto-Regular \
             /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 9 0 R >>",
        )
        .object(
            6,
            &format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Roboto-Regular \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor 7 0 R /DW 500 /W [{widths}] /CIDToGIDMap /Identity >>"
            ),
        )
        .object(
            7,
            "<< /Type /FontDescriptor /FontName /Roboto-Regular /Flags 4 \
             /FontBBox [-737 -271 1148 1056] /ItalicAngle 0 /Ascent 928 /Descent -244 \
             /CapHeight 711 /StemV 80 /FontFile2 8 0 R >>",
        )
        .stream(8, &format!(" /Length1 {}", program.len()), program)
        .stream(9, "", &to_unicode)
        .finish("/Root 1 0 R")
}
