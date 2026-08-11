//! Write the document the demo editor opens.
//!
//!   cargo run -p rasura-flow --example sample
//!
//! Committed to the repository rather than fetched, and authored here rather
//! than borrowed, for one reason: **everything under `corpus/external` is other
//! people's files under other people's licences**, which `.gitignore` keeps out
//! of the tree and which a public repository must not carry. A demo whose
//! sample document cannot be published is a demo that cannot be published.
//!
//! It is deliberately shaped to exercise the things the demo shows: two pages
//! so navigation means something, a size hierarchy so heading inference has
//! something to infer, ragged paragraphs so line breaking is visible, and a
//! rule so the vector path is not empty.

use rasura_cos::testutil::ClassicBuilder;

/// Helvetica at 10pt: `n` is 5.56 and a space 2.78, so a line of the measure
/// below is about this many characters. Used to wrap the body text at a width
/// the page can actually hold, because a demo document whose text runs off the
/// page would be demonstrating the wrong thing.
const MEASURE: f64 = 468.0;

fn wrap(text: &str, size: f64) -> Vec<String> {
    let per_char = size * 0.5;
    let max = (MEASURE / per_char) as usize;
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > max {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// `(text, size, font, gap-before)` laid out from the top of the text frame.
fn page(blocks: &[(&str, f64, &str, f64)]) -> Vec<u8> {
    let mut out = String::new();
    let mut y = 720.0;

    for (text, size, font, gap) in blocks {
        y -= gap;
        for line in wrap(text, *size) {
            let escaped = line.replace('\\', r"\\").replace('(', r"\(").replace(')', r"\)");
            out.push_str(&format!("BT /{font} {size} Tf 1 0 0 1 72 {y:.1} Tm ({escaped}) Tj ET\n"));
            y -= size * 1.35;
        }
    }
    out.into_bytes()
}

fn main() {
    let one = page(&[
        ("Quarterly Report", 18.0, "FB", 0.0),
        (
            "Prepared for the board, and for anyone curious about what a PDF editor can see.",
            9.0,
            "FI",
            8.0,
        ),
        ("Summary", 13.0, "FB", 22.0),
        (
            "Revenue rose by eleven per cent over the period, driven chiefly by the \
             subscription business, which grew faster than the board had forecast at the \
             start of the year. The hardware line was flat. Costs rose in line with \
             headcount, and the margin held.",
            10.0,
            "F1",
            8.0,
        ),
        (
            "This paragraph exists so that line breaking is visible. Its last line is \
             short, which is exactly why a single page cannot tell you where the measure \
             is — twenty pages of the same column can.",
            10.0,
            "F1",
            10.0,
        ),
        ("Method", 13.0, "FB", 20.0),
        (
            "Figures are unaudited. Where a number is stated to two places it was \
             computed to more and rounded once, at the end, rather than at every step.",
            10.0,
            "F1",
            8.0,
        ),
    ]);

    let mut two_content = String::from_utf8(page(&[
        ("Notes and assumptions", 15.0, "FB", 0.0),
        (
            "Every glyph on this page carries an absolute position. Nothing in the file \
             says that this paragraph follows that one, or that either belongs to the \
             section above them. That structure was consumed when the document was \
             written, and everything the editor shows you about it was reconstructed.",
            10.0,
            "F1",
            12.0,
        ),
        (
            "The editor draws from that reconstruction rather than from a rendering \
             engine, which is why the page looks approximately rather than exactly like \
             a viewer would show it. What you are looking at is what the library knows.",
            10.0,
            "F1",
            10.0,
        ),
        ("Contact", 13.0, "FB", 20.0),
        ("Questions to the desk of A. Ozdamar, who does not exist.", 10.0, "F1", 8.0),
    ]))
    .expect("ascii");

    // A rule, so the page has vector art and the demo's block overlay has
    // something other than text to show.
    two_content.push_str("0.5 w 72 640 m 540 640 l S\n");

    let bytes = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [6 0 R 8 0 R] /Count 2 >>")
        .object(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
        .object(4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>")
        .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique /Encoding /WinAnsiEncoding >>")
        .object(
            6,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R \
             /Resources << /Font << /F1 3 0 R /FB 4 0 R /FI 5 0 R >> >> >>",
        )
        .stream(7, "", &one)
        .object(
            8,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R \
             /Resources << /Font << /F1 3 0 R /FB 4 0 R /FI 5 0 R >> >> >>",
        )
        .stream(9, "", two_content.as_bytes())
        .finish("/Root 1 0 R /Info << /Title (Quarterly Report) /Author (Rasura Studio) >>");

    let path = std::path::Path::new("demo/sample.pdf");
    std::fs::write(path, &bytes).expect("write demo/sample.pdf");
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}
