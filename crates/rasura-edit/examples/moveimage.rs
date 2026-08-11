//! Moving an image, judged by a renderer. Spec 9.2, Phase 6.
//!
//! The unit tests check that the bounding box lands where it was asked to, by
//! re-analysing the edited page — which is this library agreeing with itself.
//! pdfium has no such stake.
//!
//! The fixture draws the image **rotated**, because 1,129 of the 3,095 images
//! in the corpus (36%) are rotated or skewed, and that is the case a move
//! implemented by rewriting a bounding box would silently flatten.
//!
//! ```text
//! cargo run -p rasura-edit --example moveimage -- target/moveimage
//! cargo run -p rasura-pixeldiff -- \
//!     target/moveimage/before.pdf target/moveimage/after.pdf --page 2 --identical
//! ```

use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, SaveOptions};
use rasura_edit::locate::EditablePage;
use rasura_edit::{EditSession, move_image};

/// Two pages sharing one image XObject. Page one draws it rotated 30°; page two
/// draws it square and must not move.
fn document() -> Vec<u8> {
    // A 4x4 checkerboard, so a rotation is visible rather than inferred.
    let pixels: Vec<u8> =
        (0..16u8).map(|i| if (i / 4 + i % 4) % 2 == 0 { 0 } else { 255 }).collect();

    // cos(30°) and sin(30°) scaled to a 160-point square.
    let (c, s) = (160.0f64 * 0.8660254, 160.0f64 * 0.5);
    let page_one = format!("q {c:.4} {s:.4} {:.4} {c:.4} 150 480 cm /Im1 Do Q\n", -s);

    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /XObject << /Im1 5 0 R >> >> >>",
        )
        .stream(4, "", page_one.as_bytes())
        .stream(
            5,
            "/Type /XObject /Subtype /Image /Width 4 /Height 4 \
             /ColorSpace /DeviceGray /BitsPerComponent 8",
            &pixels,
        )
        .object(
            6,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R \
             /Resources << /XObject << /Im1 5 0 R >> >> >>",
        )
        .stream(7, "", b"q 160 0 0 160 150 480 cm /Im1 Do Q\n")
        .finish("/Root 1 0 R")
}

fn main() -> std::process::ExitCode {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| "target/moveimage".into());
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

    let Some(page) = EditablePage::analyse(&doc, &pages.pages[0]) else {
        eprintln!("FAIL: page one did not analyse");
        return std::process::ExitCode::FAILURE;
    };
    let graphics = rasura_layout::graphics::collect(&doc, &pages.pages[0]);
    let Some(image) = graphics.images.first() else {
        eprintln!("FAIL: no image found on page one");
        return std::process::ExitCode::FAILURE;
    };

    println!("image:   bbox {:?}", image.bbox);
    println!("         ctm  {:?}", image.ctm);
    println!("         rotated: {}", image.ctm.b.abs() > 1e-9 || image.ctm.c.abs() > 1e-9);

    // Which operation to exercise. All three go through the same wrap, so the
    // renderer is checking one mechanism from three directions.
    let args: Vec<String> = std::env::args().collect();
    let op = if args.iter().any(|a| a == "--scale") {
        "scale"
    } else if args.iter().any(|a| a == "--delete") {
        "delete"
    } else {
        "move"
    };

    let (dx, dy) = (120.0, -60.0);
    let result = match op {
        "scale" => rasura_edit::scale_image(&page, image, 1.5, 1.5),
        "delete" => rasura_edit::delete_image(&page, image),
        _ => move_image(&page, image, dx, dy),
    };
    let edit = match result {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: the {op} was refused: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("op:      {op}");
    println!("move:    {dx:+} {dy:+} in device space");
    println!("wrote:   {}", String::from_utf8_lossy(&edit.patches[0].bytes).replace('\n', " | "));

    let content = page.content;
    let mut session = EditSession::new(&mut doc);
    if let Err(e) = session.patch_content("move image", &content, &edit.patches, edit.fidelity) {
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

    // Re-analyse: the box must have moved by exactly what was asked, and the
    // linear part of the transform must be untouched.
    let after_doc = match Document::open(saved.bytes.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: reopen: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let after_pages = rasura_content::page::pages(&after_doc).expect("pages");
    let after = rasura_layout::graphics::collect(&after_doc, &after_pages.pages[0]);

    if op == "delete" {
        if !after.images.is_empty() {
            eprintln!("FAIL: the image is still on the page");
            return std::process::ExitCode::FAILURE;
        }
        println!("checked: the image is gone");
    }

    // The columns the image occupied before *or* after. Everything outside this
    // band must be untouched, and the pixel diff is given the range rather than
    // asked to infer it -- only the edit knows where the content went.
    let mut left = image.bbox.x0;
    let mut right = image.bbox.x1;

    if let Some(moved) = after.images.first() {
        println!("after:   bbox {:?}", moved.bbox);
        left = left.min(moved.bbox.x0);
        right = right.max(moved.bbox.x1);

        // The linear part must survive every operation that is not a scale.
        let expect_linear = op == "move";
        for (name, before, now) in [
            ("a", image.ctm.a, moved.ctm.a),
            ("b", image.ctm.b, moved.ctm.b),
            ("c", image.ctm.c, moved.ctm.c),
            ("d", image.ctm.d, moved.ctm.d),
        ] {
            if expect_linear && (before - now).abs() > 1e-6 {
                eprintln!("FAIL: the rotation changed: {name} {before} -> {now}");
                return std::process::ExitCode::FAILURE;
            }
        }

        match op {
            "move" => {
                let dx_actual = moved.bbox.x0 - image.bbox.x0;
                let dy_actual = moved.bbox.y0 - image.bbox.y0;
                if (dx_actual - dx).abs() > 1e-6 || (dy_actual - dy).abs() > 1e-6 {
                    eprintln!(
                        "FAIL: moved by {dx_actual:+.4} {dy_actual:+.4}, asked {dx:+} {dy:+}"
                    );
                    return std::process::ExitCode::FAILURE;
                }
                println!("checked: moved exactly {dx:+} {dy:+}, rotation preserved");
            }
            "scale" => {
                let grew = moved.bbox.width() / image.bbox.width();
                if (grew - 1.5).abs() > 1e-6 {
                    eprintln!("FAIL: scaled by {grew:.4}, asked for 1.5");
                    return std::process::ExitCode::FAILURE;
                }
                // A rotated image scaled evenly keeps its angle: the ratio of
                // the off-diagonal to the diagonal term is unchanged.
                let angle = |m: &rasura_content::matrix::Matrix| m.b.atan2(m.a);
                if (angle(&image.ctm) - angle(&moved.ctm)).abs() > 1e-9 {
                    eprintln!("FAIL: the angle changed under an even scale");
                    return std::process::ExitCode::FAILURE;
                }
                println!("checked: scaled by 1.5, angle preserved");
            }
            _ => {}
        }
    }

    // 150 dpi against PDF's 72 units per inch, matching the harness.
    let (col0, col1) = (
        (left.max(0.0) * 150.0 / 72.0).floor() as u32,
        (right.max(0.0) * 150.0 / 72.0).ceil() as u32,
    );
    println!("region:  columns {col0}..{col1}");

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

    let region = format!("{out_dir}/region.txt");
    if let Err(e) = std::fs::write(&region, format!("{col0} {col1}")) {
        eprintln!("FAIL: {region}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    println!("wrote:   {region}");

    std::process::ExitCode::SUCCESS
}
