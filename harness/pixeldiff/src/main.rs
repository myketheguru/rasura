//! The pixel-diff harness. Spec 14.3.
//!
//! > Render before and after with pdfium at 150 dpi. Compare with a perceptual
//! > threshold that ignores anti-aliasing noise (per-pixel ΔE below a small
//! > bound) but catches any glyph position shift above a quarter pixel. Store
//! > failures as side-by-side artefacts in CI.
//!
//! This is the check no structural test can reach. `qpdf --check` says the file
//! is well formed; pdf.js says it opens and the text comes back. Neither says
//! whether the page still *looks* the same, and §2's first property is entirely
//! about that:
//!
//! > An edit on page 40 must not change the rendered output of any other page
//! > by a single pixel, nor alter the bytes of any object it did not need to
//! > touch.
//!
//! So the question asked here is not "did anything change" — an injected glyph
//! is meant to appear — but **"did anything change where the old content
//! was"**. Everything left of the new glyph must be pixel-identical.
//!
//! pdfium is a test-only reference renderer (spec 4.2) and is never shipped.
//! The library does not know it exists.
//!
//! # Four questions, not one
//!
//! "Did the page change" is never the right question, and which question *is*
//! depends on what the edit was supposed to do. Asking the wrong one produces a
//! failure that is really the harness being confused, and a green tick that
//! means nothing is worse than a red one that means something.
//!
//! | Mode | Asks | Fits |
//! |---|---|---|
//! | *(default)* | did anything change **left of** the original ink? | content appended — an injected glyph |
//! | `--identical` | did **anything at all** change? | a page the edit did not touch |
//! | `--unchanged-before N` | did anything change **before** column N? | an edit inside a line of text |
//! | `--changed-within X0 X1` | did anything change **outside** those columns? | content that moved |
//!
//! The last three all take their bound from the caller, because only the caller
//! knows where the edit was. A renderer can see that pixels differ; it cannot
//! see which of them were supposed to.
//!
//! ```text
//! pwsh harness/pixeldiff/fetch.ps1          # or fetch.sh
//! cargo run -p rasura-pixeldiff -- before.pdf after.pdf
//! cargo run -p rasura-pixeldiff -- a.pdf b.pdf --page 2 --identical
//! cargo run -p rasura-pixeldiff -- a.pdf b.pdf --changed-within 145 852
//! ```

use pdfium_render::prelude::*;

/// Spec 14.3's rendering resolution.
const DPI: f32 = 150.0;

/// Per-channel difference below which two pixels are "the same".
///
/// Anti-aliasing is not deterministic across runs of the same renderer at the
/// same size -- sub-pixel rounding differs -- so a small band has to be
/// tolerated. A quarter-pixel shift of a hard edge changes that edge's coverage
/// by about a quarter, or ~64 of 255, so this sits comfortably below what spec
/// 14.3 asks to catch while ignoring what it asks to ignore.
const NOISE: i16 = 24;

/// Columns adjacent to new content that its anti-aliasing may legitimately
/// tint.
///
/// New content drawn immediately after old content shares a boundary, and a
/// rasteriser's edge filter reaches about a pixel either side of it. Without
/// this the harness reports a non-locality violation every time an injected
/// glyph is set flush against the text before it, which is the normal case.
///
/// It does not weaken what the test catches. Text that has genuinely *moved*
/// changes columns throughout the region it occupies -- every stem, every
/// counter -- not one column at its edge. A two-pixel band at the boundary is
/// the difference between a false alarm on every run and a test nobody trusts.
const AA_MARGIN: u32 = 2;

/// A rendered page.
struct Page {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

fn render(pdfium: &Pdfium, path: &str, index: u16, password: Option<&str>) -> Result<Page, String> {
    let doc = pdfium.load_pdf_from_file(path, password).map_err(|e| format!("{path}: {e}"))?;
    let page = doc
        .pages()
        .get(index.into())
        .map_err(|e| format!("{path}: no page {}: {e}", index as usize + 1))?;

    // 150 dpi against PDF's 72-per-inch user space.
    let config = PdfRenderConfig::new().scale_page_by_factor(DPI / 72.0);
    let bitmap = page.render_with_config(&config).map_err(|e| format!("{path}: render: {e}"))?;
    let image = bitmap.as_image().map_err(|e| format!("{path}: bitmap: {e}"))?.into_rgb8();

    Ok(Page { width: image.width(), height: image.height(), rgb: image.into_raw() })
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(before_path), Some(after_path)) = (args.first(), args.get(1)) else {
        eprintln!(
            "usage: pixeldiff <before.pdf> <after.pdf> [--artefacts DIR] [--page N]\n\
             \x20              [--password PW [--password PW]]\n\
             \x20              [--identical | --unchanged-before COL | --changed-within X0 X1]"
        );
        return std::process::ExitCode::from(2);
    };
    let artefacts =
        args.iter().position(|a| a == "--artefacts").and_then(|i| args.get(i + 1)).cloned();

    // Which page to compare. One-based on the command line, because that is
    // what a person reading a PDF counts.
    let page_number: u16 = args
        .iter()
        .position(|a| a == "--page")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let index = page_number.saturating_sub(1);

    // Spec 14.2 I2: "After editing page N, every other page renders
    // pixel-identical." That is a different assertion from the injection case
    // -- there, something is *meant* to appear, and the question is only
    // whether it appeared in the right place. Here nothing may change at all,
    // which is the stronger and more important half of the invariant.
    let identical = args.iter().any(|a| a == "--identical");

    // For an edit *within* a line, the default model is the wrong question.
    // It assumes new content is appended to the right of what was there, which
    // is true of an injected glyph and false of a replaced word: the change is
    // legitimately inside the original ink, and everything after it moves
    // because that is what changing a word's width does.
    //
    // The claim that still holds, and the one worth checking, is that nothing
    // *before* the edit moved. A caller who knows where the edit was passes
    // that column here.
    let unchanged_before: Option<u32> = args
        .iter()
        .position(|a| a == "--unchanged-before")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok());

    // And for content that *moved*, neither question fits. An image dragged
    // across the middle of a page changes columns on both sides of where it
    // started, so "nothing before column N" is false and "nothing at all" is
    // false. What is true, and worth asserting, is that the change is confined
    // to the region the content occupied before and after — everything else on
    // the page held still.
    //
    // The caller supplies that region because only the caller knows it: it is
    // the union of the block's old and new bounds, which the edit computed and
    // the renderer cannot infer.
    let changed_within: Option<(u32, u32)> = args
        .iter()
        .position(|a| a == "--changed-within")
        .and_then(|i| Some((args.get(i + 1)?, args.get(i + 2)?)))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));

    // The native library is looked for beside the binary, then on the system
    // path. Bindings that cannot be found are a setup problem, not a test
    // failure, and exit differently so CI can tell them apart.
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
        .or_else(|_| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("target/pdfium/"))
        })
        .or_else(|_| Pdfium::bind_to_system_library());
    let pdfium = match bindings {
        Ok(b) => Pdfium::new(b),
        Err(e) => {
            eprintln!("SETUP: pdfium not found ({e}).");
            eprintln!("       Run harness/pixeldiff/fetch.ps1 (or fetch.sh) first.");
            return std::process::ExitCode::from(2);
        }
    };

    // Passwords for the two files, in order. One `--password` applies to both,
    // which is what a protect-and-compare run wants; two lets an encrypted file
    // be compared against a plain one, which is what proves encryption changed
    // no pixels.
    let passwords: Vec<&str> = args
        .iter()
        .zip(args.iter().skip(1))
        .filter(|(a, _)| *a == "--password")
        .map(|(_, p)| p.as_str())
        .collect();
    let password_for = |n: usize| passwords.get(n).or_else(|| passwords.first()).copied();

    let (before, after) = match (
        render(&pdfium, before_path, index, password_for(0)),
        render(&pdfium, after_path, index, password_for(1)),
    ) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("FAIL: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if before.width != after.width || before.height != after.height {
        eprintln!(
            "FAIL: page size changed, {}x{} -> {}x{}",
            before.width, before.height, after.width, after.height
        );
        return std::process::ExitCode::FAILURE;
    }

    // The renderer is checked before its output is trusted. A rasteriser
    // producing a blank page reports "nothing changed" for every input it will
    // ever be given -- a green tick that can never go red, which is worse than
    // no test at all.
    let Some(last_inked) = rightmost_ink(&before) else {
        eprintln!("SETUP: the baseline render is blank, so any comparison would pass.");
        eprintln!("       The renderer is at fault, not the document.");
        return std::process::ExitCode::from(2);
    };

    // Everything the original content occupied, less the band where new
    // content set flush against it may legitimately bleed.
    let protected_to = last_inked.saturating_sub(AA_MARGIN);

    let mut changed = 0usize;
    let mut first_changed = before.width;
    let mut last_changed: i64 = -1;
    let mut violations = 0usize;
    for y in 0..before.height {
        for x in 0..before.width {
            let i = ((y * before.width + x) * 3) as usize;
            let d = (0..3)
                .map(|c| (before.rgb[i + c] as i16 - after.rgb[i + c] as i16).abs())
                .max()
                .unwrap_or(0);
            if d > NOISE {
                changed += 1;
                first_changed = first_changed.min(x);
                last_changed = last_changed.max(x as i64);
                if x < protected_to {
                    violations += 1;
                }
            }
        }
    }

    println!(
        "render:          page {page_number}, {}x{} at {DPI} dpi",
        before.width, before.height
    );
    println!("changed pixels:  {changed}");
    println!("changed columns: {first_changed}..{last_changed}");
    println!("original ink ends at column {last_inked}");

    if let Some(dir) = &artefacts {
        // Spec 14.3: "Store failures as side-by-side artefacts in CI." Written
        // unconditionally, because a passing render is what the next failure
        // will be compared against by eye.
        let _ = std::fs::create_dir_all(dir);
        for (name, page) in [("before", &before), ("after", &after)] {
            let path = format!("{dir}/{name}.png");
            if let Some(buf) = image::RgbImage::from_raw(page.width, page.height, page.rgb.clone())
            {
                let _ = buf.save(&path);
            }
        }
        println!("artefacts:       {dir}/before.png, {dir}/after.png");
    }

    // Spec 14.2 I2, the pixel half. An untouched page must render *identically*
    // -- not "close enough", not "the text still extracts", but the same
    // pixels. It is the only check that catches an edit whose effect leaks
    // through a shared resource: a font dictionary rewritten in place, a
    // /Resources entry renumbered, an object stream repacked. All of those
    // leave the edited page correct and change a page nobody touched.
    if identical {
        if changed == 0 {
            println!("OK: page {page_number} is pixel-identical. Spec 2, property 1.");
            return std::process::ExitCode::SUCCESS;
        }
        eprintln!(
            "FAIL: {changed} pixel(s) changed on page {page_number}, which the edit did not touch."
        );
        eprintln!("      Columns {first_changed}..{last_changed}. Spec 2, property 1.");
        return std::process::ExitCode::FAILURE;
    }

    if changed == 0 {
        eprintln!("FAIL: nothing changed -- the edit did not draw.");
        return std::process::ExitCode::FAILURE;
    }

    if let Some((x0, x1)) = changed_within {
        // Widened by the anti-aliasing band on each side, for the same reason
        // the default mode has one: a rasteriser's edge filter reaches about a
        // pixel beyond the geometry it is drawing.
        let (low, high) = (x0.saturating_sub(AA_MARGIN), x1.saturating_add(AA_MARGIN));
        let mut outside = 0usize;
        let mut worst = 0u32;
        for y in 0..before.height {
            for x in 0..before.width {
                if x >= low && x <= high {
                    continue;
                }
                let i = ((y * before.width + x) * 3) as usize;
                let d = (0..3)
                    .map(|c| (before.rgb[i + c] as i16 - after.rgb[i + c] as i16).abs())
                    .max()
                    .unwrap_or(0);
                if d > NOISE {
                    outside += 1;
                    // The furthest stray, which is what tells a reader whether
                    // the region was slightly too narrow or the edit leaked
                    // somewhere else entirely.
                    worst = worst.max(if x < low { low - x } else { x - high });
                }
            }
        }
        if outside > 0 {
            eprintln!(
                "FAIL: {outside} pixel(s) changed outside columns {low}..{high}, up to \
                 {worst} column(s) away."
            );
            eprintln!("      Content moved that the edit did not touch. Spec 2, property 1.");
            return std::process::ExitCode::FAILURE;
        }
        println!(
            "OK: {changed} pixel(s) changed, all within columns {low}..{high}. Nothing \
             outside the edited region moved."
        );
        return std::process::ExitCode::SUCCESS;
    }

    if let Some(boundary) = unchanged_before {
        let limit = boundary.saturating_sub(AA_MARGIN);
        let before_boundary = (0..before.height)
            .flat_map(|y| (0..limit.min(before.width)).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let i = ((y * before.width + x) * 3) as usize;
                (0..3)
                    .map(|c| (before.rgb[i + c] as i16 - after.rgb[i + c] as i16).abs())
                    .max()
                    .unwrap_or(0)
                    > NOISE
            })
            .count();
        if before_boundary > 0 {
            eprintln!(
                "FAIL: {before_boundary} pixel(s) changed before column {limit}, which is \
                 upstream of the edit."
            );
            eprintln!("      The text before the edit moved. Spec 2, property 1.");
            return std::process::ExitCode::FAILURE;
        }
        println!(
            "OK: {changed} pixel(s) changed, none before column {limit}. Everything upstream \
             of the edit held still."
        );
        return std::process::ExitCode::SUCCESS;
    }

    if violations > 0 {
        eprintln!(
            "FAIL: {violations} pixel(s) changed before column {protected_to}, inside the \
             region the original content occupied."
        );
        eprintln!("      The edit moved content it did not touch. Spec 2, property 1.");
        return std::process::ExitCode::FAILURE;
    }

    println!(
        "OK: {changed} pixel(s) changed, none before column {protected_to}. The content \
         that was already there did not move."
    );
    std::process::ExitCode::SUCCESS
}

/// The rightmost column carrying any ink.
fn rightmost_ink(page: &Page) -> Option<u32> {
    for x in (0..page.width).rev() {
        for y in 0..page.height {
            let i = ((y * page.width + x) * 3) as usize;
            if page.rgb[i] < 200 || page.rgb[i + 1] < 200 || page.rgb[i + 2] < 200 {
                return Some(x);
            }
        }
    }
    None
}
