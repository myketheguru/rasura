//! Dump the embedded font programs of a PDF.
//!
//! Aggregate parse rates say whether something is wrong; they never say what.
//! This prints each font program's flavour, glyph count and — for Type 1 — the
//! opening bytes of a few charstrings, which is where a decryption or `lenIV`
//! mistake becomes visible.
//!
//! ```text
//! cargo run -p rasura-font --example fontdump -- file.pdf
//! ```

use rasura_cos::document::{Document, OpenOptions};
use rasura_cos::{ObjId, Object};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: fontdump <file.pdf>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("read");
    let doc = Document::open_with(bytes, &OpenOptions::default()).expect("open");

    let numbers: Vec<u32> = doc.xref().iter().map(|(n, _)| n).collect();
    for number in numbers {
        let id = ObjId::new(number, 0);
        let Ok(obj) = doc.get(id) else { continue };
        let Some(dict) = obj.as_dict() else { continue };
        // Matched on the font-program keys rather than on `/Type`, because a
        // descriptor that omits `/Type` is common and still a descriptor.
        if !["FontFile", "FontFile2", "FontFile3"].iter().any(|k| dict.get(k).is_some()) {
            continue;
        }
        let name = dict
            .get("FontName")
            .and_then(Object::as_name)
            .and_then(|n| n.as_str())
            .unwrap_or("(anonymous)")
            .to_string();

        let Some(program) = rasura_font::program::from_descriptor(&doc, dict) else {
            println!("{name}: no font program");
            continue;
        };
        print!(
            "{name}: {} from /{}, {} bytes",
            program.flavour.name(),
            program.slot.key(),
            program.bytes.len()
        );
        if program.is_mislabelled() {
            print!("  [declared {:?}]", program.declared.map(|d| d.name()));
        }
        println!();

        if program.flavour == rasura_font::Flavour::Type1 {
            match rasura_font::Type1::parse(&program.bytes) {
                Ok(font) => {
                    println!(
                        "    lenIV {}, {} glyph(s), {} subr(s), soundness {:.3}",
                        font.len_iv,
                        font.glyph_count(),
                        font.subrs.len(),
                        font.soundness()
                    );
                    for name in font.glyph_order.iter().take(6) {
                        let cs = font.charstring(name).unwrap_or_default();
                        let head: Vec<String> =
                            cs.iter().take(10).map(|b| format!("{b:3}")).collect();
                        println!(
                            "    {:<16} {:>4} bytes  {}  {}",
                            name,
                            cs.len(),
                            if rasura_font::type1::opens_correctly(cs) { "ok " } else { "BAD" },
                            head.join(" ")
                        );
                    }
                }
                Err(e) => println!("    parse failed: {e}"),
            }
        } else if program.flavour.is_sfnt() {
            // The four-byte tag, because it decides how everything after it is
            // read: `ttcf` means the bytes are a *collection* and the real
            // directory is elsewhere, which changes what every table offset is
            // relative to.
            let tag = program.bytes.get(..4).unwrap_or_default();
            println!(
                "    sfnt tag: {:?} ({})",
                String::from_utf8_lossy(tag),
                tag.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            );
            if let Ok(f) = rasura_font::Sfnt::parse(&program.bytes) {
                let tags: Vec<String> = f
                    .tables
                    .iter()
                    .map(|(t, _, _)| String::from_utf8_lossy(t).trim_end().to_string())
                    .collect();
                println!("    {} glyph(s), tables: {}", f.num_glyphs, tags.join(" "));
            }
        } else if let Ok(c) = rasura_font::Cff::parse(&program.bytes) {
            println!("    {} glyph(s), CID-keyed: {}", c.glyph_count(), c.is_cid);
        }
    }
}
