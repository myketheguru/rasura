//! Glyph injection into a typeface this library did not write.
//!
//! Every other injection test runs against `fixture::truetype`, a font
//! synthesised by the same crate that reads it. That is circular in a way worth
//! naming: it proves the writer and the parser agree, not that either matches
//! what the rest of the world produces. A real font brings composite glyphs,
//! hinting programs, a `post` table, `GSUB`, several `cmap` subtables and 2048
//! units per em — none of which the fixture has.
//!
//! It writes the before/after pair the §14.3 pixel diff needs, so the claim
//! ends at a renderer rather than at this crate's own parsers.
//!
//! ```text
//! pwsh corpus/fetch-font.ps1        # or ./corpus/fetch-font.sh
//! cargo run -p rasura-font --example realfont -- \
//!     corpus/fonts/Roboto-Regular.ttf target/realfont
//! cargo run -p rasura-pixeldiff -- \
//!     target/realfont/before.pdf target/realfont/after.pdf
//! ```

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(font_path), Some(out_dir)) = (args.next(), args.next()) else {
        eprintln!("usage: realfont <font.ttf> <output-dir>");
        return std::process::ExitCode::from(2);
    };

    let full = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("SETUP: cannot read {font_path}: {e}");
            eprintln!("       Run corpus/fetch-font.sh (or fetch-font.ps1) first.");
            return std::process::ExitCode::from(2);
        }
    };

    let font = match rasura_font::Sfnt::parse(&full) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: {font_path} does not parse as an sfnt: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("font:    {} glyphs, {} units per em", font.num_glyphs, font.units_per_em);

    // É rather than a plain letter, deliberately. In almost every Latin
    // typeface it is a *composite*: a reference to E plus a reference to the
    // acute, each with its own transform. Injecting it is only correct if the
    // components come too and their glyph ids are renumbered inside the
    // composite's own body — the failure mode is a glyph that draws the wrong
    // letter, or nothing.
    let pair = match rasura_font::fixture::inject_into_real_font(&full, "Hamburg", 'É') {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: injection: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("subset:  {} glyphs after the producer's subset", pair.glyphs_before);
    println!("inject:  {} glyphs after injection", pair.glyphs_after);
    println!("         {} component(s) pulled in with it", pair.components_pulled);

    if pair.components_pulled == 0 {
        // Not fatal — a font is free to draw É as a simple outline — but it
        // means this run did not exercise the composite path, and a run that
        // silently tests less than it claims is worth saying out loud.
        println!("note:    the glyph was simple, so the composite path went untested");
    }

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("FAIL: {out_dir}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    for (name, bytes) in [("before.pdf", &pair.before), ("after.pdf", &pair.after)] {
        let path = format!("{out_dir}/{name}");
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({} bytes)", bytes.len());
    }

    std::process::ExitCode::SUCCESS
}
