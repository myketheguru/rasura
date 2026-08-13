//! Build a PDF from nothing: no input file, a real embedded typeface, text.
//!
//!   ./corpus/fetch-font.sh
//!   cargo run -p rasura-edit --example compose -- target/compose
//!
//! The check that no unit test can make. Everything under `crates/` can assert
//! that the right dictionary keys were written; none of it can say whether a
//! reader accepts the file or whether the glyphs draw. This writes one and then
//! reads it back through the library's own parser, and CI puts qpdf, pdf.js and
//! pdfium on the result — because the characteristic failure of a font-
//! embedding path is a document that passes every structural check and renders
//! as blank boxes.
//!
//! Two documents, because they exercise different halves:
//!
//!   simple.pdf     Latin text, a /TrueType font, WinAnsi codes
//!   composite.pdf  Greek and Latin, a /Type0 font, /Identity-H, two-byte codes
//!
//! Both are subsets of Roboto, which the repository does not carry -- see
//! `corpus/fetch-font.sh` for why it is fetched rather than vendored.

use rasura_cos::{Document, ObjId, SaveOptions};
use rasura_edit::pages::{PageSpec, insert_page};
use rasura_edit::{Canvas, EditSession};
use rasura_font::create::{Embedded, Options, embed_truetype};
use std::process::ExitCode;

/// Where the text sits on the page, and how big.
const MARGIN: f64 = 72.0;
const TOP: f64 = 720.0;
const SIZE: f64 = 18.0;
const LEADING: f64 = 26.0;

fn main() -> ExitCode {
    let out = std::env::args().nth(1).unwrap_or_else(|| "target/compose".to_string());
    let dir = std::path::Path::new(&out);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("FAIL: {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }

    let font_path = "corpus/fonts/Roboto-Regular.ttf";
    let program = match std::fs::read(font_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("FAIL: {font_path}: {e}");
            eprintln!("      run ./corpus/fetch-font.sh first");
            return ExitCode::FAILURE;
        }
    };
    println!("font:    {font_path}, {} bytes", program.len());

    let simple =
        ["Composed from nothing.", "No input file, one embedded", "typeface, real glyphs."];
    let composite = ["Ελληνικά and English", "in one Type0 font,", "addressed by glyph id."];

    let mut failed = false;
    for (name, lines) in [("simple", &simple), ("composite", &composite)] {
        match compose(&program, lines) {
            Ok(bytes) => {
                let path = dir.join(format!("{name}.pdf"));
                if let Err(e) = std::fs::write(&path, &bytes) {
                    eprintln!("FAIL: {}: {e}", path.display());
                    failed = true;
                    continue;
                }
                println!("\n{name}: {} bytes -> {}", bytes.len(), path.display());
                failed |= !verify(&bytes, lines);
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
    println!("\nboth documents were built from nothing and reopen through the library.");
    ExitCode::SUCCESS
}

/// Create, embed, draw, save.
fn compose(program: &[u8], lines: &[&str; 3]) -> Result<Vec<u8>, String> {
    let mut doc = Document::new();

    // Every character that will be drawn. The subset holds exactly this and
    // nothing else, which is what keeps a two-line document from carrying
    // 3,387 glyphs.
    let opts = Options::for_text(&lines.join(" "));
    let embedded = {
        // The ids come from the document's own allocator, so nothing collides
        // with the catalog and page tree `Document::new` already wrote.
        let next = || doc.reserve(1)[0];
        embed_truetype(program, &opts, next).map_err(|e| e.to_string())?
    };

    println!(
        "  {}: {} glyph(s), {}",
        embedded.base_font,
        opts.characters.len() - embedded.missing.len(),
        if embedded.composite { "Type0 / Identity-H" } else { "TrueType / WinAnsi" },
    );
    if !embedded.missing.is_empty() {
        println!("  missing: {:?}", embedded.missing);
    }
    if embedded.description.stem_v_guessed {
        println!(
            "  StemV {} is an estimate; no sfnt table records it",
            embedded.description.stem_v
        );
    }

    let content = draw(&embedded, lines);

    // The font has to be reachable from the page, by the same name the content
    // stream used. A content stream naming a resource the page does not define
    // is the other way to make text invisible.
    let mut fonts = rasura_cos::Dictionary::new();
    fonts.insert("F1", rasura_cos::Object::Reference(embedded.font));
    let mut resources = rasura_cos::Dictionary::new();
    resources.insert("Font", rasura_cos::Object::Dictionary(fonts));

    let spec = PageSpec { content, resources: Some(resources), ..PageSpec::default() };
    let tree = rasura_content::page::pages(&doc).map_err(|e| e.to_string())?;
    let page = insert_page(&mut doc, &tree, 0, &spec).map_err(|e| e.to_string())?;

    // Through a session, so the whole composition is one undoable operation
    // rather than a sequence of writes with no way back.
    {
        let mut session = EditSession::new(&mut doc);
        let font_objects: Vec<_> =
            embedded.objects.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();
        session
            .set_objects("embed font", &font_objects, rasura_edit::Fidelity::Exact)
            .map_err(|e| e.to_string())?;
        session
            .set_objects("add page", &page.changes, page.fidelity.clone())
            .map_err(|e| e.to_string())?;
    }

    Ok(rasura_cos::save(&doc, &SaveOptions::default()).map_err(|e| e.to_string())?.bytes)
}

/// The content stream: one `Tj` per line, positioned from the top.
fn draw(embedded: &Embedded, lines: &[&str; 3]) -> Vec<u8> {
    let font = rasura_cos::Name::new("F1");
    let mut canvas = Canvas::new(rasura_edit::numfmt::NumberStyle::default());
    canvas.fill_gray(0.1);
    for (i, line) in lines.iter().enumerate() {
        // Encoded by the font, not by this function: a simple font wants
        // WinAnsi bytes and a composite one wants two-byte glyph ids, and the
        // drawing code has no business knowing which.
        let (codes, dropped) = embedded.encode(line);
        if dropped > 0 {
            eprintln!("  warning: {dropped} character(s) of {line:?} have no glyph");
        }
        canvas.text_line(&font, SIZE, MARGIN, TOP - i as f64 * LEADING, &codes);
    }
    // A rule under the text, so the page is not text alone and the vector path
    // is exercised too.
    canvas.line_width(0.75);
    canvas.move_to(MARGIN, TOP - 3.0 * LEADING);
    canvas.line_to(540.0, TOP - 3.0 * LEADING);
    canvas.stroke();
    canvas.finish().unwrap_or_default()
}

/// Read the file back through the library's own parser.
///
/// Not a proof that it renders — that needs pdfium, and CI does it. It is a
/// proof that what was written is what a reader sees: the page is there, the
/// font resolved, and the text comes back out through `/ToUnicode`.
fn verify(bytes: &[u8], lines: &[&str; 3]) -> bool {
    let doc = match Document::open(bytes.to_vec()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  FAIL: the file this library wrote does not reopen: {e}");
            return false;
        }
    };
    let mut ok = true;

    let leniencies = doc.leniencies();
    if !leniencies.is_empty() {
        eprintln!("  FAIL: reopening needed {} leniency/ies: {leniencies:?}", leniencies.len());
        ok = false;
    }

    let tree = match rasura_content::page::pages(&doc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("  FAIL: no page tree: {e}");
            return false;
        }
    };
    if tree.pages.len() != 1 {
        eprintln!("  FAIL: {} page(s), expected 1", tree.pages.len());
        ok = false;
    }

    // The text, extracted the way any consumer would. This is what catches a
    // /ToUnicode that was written but is wrong -- the file would look perfect
    // and be uncopyable.
    let text = rasura_layout::page_text(&doc, &tree.pages[0]);
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    println!("  text:  {flat:?}");
    for line in lines {
        let wanted: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !flat.contains(&wanted) {
            eprintln!("  FAIL: {wanted:?} did not come back out");
            ok = false;
        }
    }

    // The font program is really in there, and is really a font.
    let embedded_programs = font_programs(&doc);
    if embedded_programs.is_empty() {
        eprintln!("  FAIL: no /FontFile2 in the saved document");
        ok = false;
    }
    for (id, program) in &embedded_programs {
        match rasura_font::sfnt::Sfnt::parse(program) {
            Ok(f) => {
                println!("  font:  {id} is a {} glyph sfnt, {} bytes", f.num_glyphs, program.len())
            }
            Err(e) => {
                eprintln!("  FAIL: {id} is not a readable font: {e}");
                ok = false;
            }
        }
    }
    ok
}

fn font_programs(doc: &Document) -> Vec<(ObjId, Vec<u8>)> {
    let mut out = Vec::new();
    for (number, _) in doc.xref().iter() {
        let id = ObjId::new(number, 0);
        let Ok(obj) = doc.get(id) else { continue };
        let Some(dict) = obj.as_dict() else { continue };
        if dict.get("Type").and_then(rasura_cos::Object::as_name)
            != Some(&rasura_cos::Name::new("FontDescriptor"))
        {
            continue;
        }
        if let Some(file) = dict.get("FontFile2").and_then(rasura_cos::Object::as_reference)
            && let Ok(data) = doc.decoded_stream(file)
        {
            out.push((file, data.to_vec()));
        }
    }
    out
}
