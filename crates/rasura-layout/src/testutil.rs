//! Shared fixtures for the reconstruction tests.
//!
//! Every module from §7.3 onwards needs the same thing: a one-page document
//! with predictable metrics, run through the whole chain. Two fonts are
//! provided so style changes can be tested, both fixed-width at 500/1000 units,
//! which makes every advance exactly half the font size and every expected
//! coordinate calculable by hand rather than by running the code and pasting
//! the answer back in.

use crate::{Region, ResolvedRun, Rule};
use rasura_content::page;
use rasura_cos::Document;
use rasura_cos::testutil::ClassicBuilder;

/// A single-page PDF with `/F1` Helvetica and `/F2` Times-Roman, both metrically
/// fixed at 500/1000, on a 600x800 media box.
pub fn page_with(content: &str) -> Vec<u8> {
    let widths = "500 ".repeat(95);
    let font = |base: &str| {
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding \
              /FirstChar 32 /LastChar 126 /Widths [{widths}] >>"
        )
    };
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>",
        )
        .stream(4, "", content.as_bytes())
        .object(5, &font("Helvetica"))
        .object(6, &font("Times-Roman"))
        .finish("/Root 1 0 R")
}

/// Run a content stream through the whole reconstruction chain.
pub fn build_page(content: &str) -> (Vec<Region>, Vec<Rule>, Vec<ResolvedRun>) {
    let doc = Document::open(page_with(content)).expect("open");
    let p = page::pages(&doc).expect("pages").pages.remove(0);
    let (runs, _) = crate::resolve_page(&doc, &p);
    let rules = crate::rules::collect(&doc, &p);
    let blocks = crate::detect(crate::place(&runs), &rules);
    (blocks, rules, runs)
}

/// One `Tj` per entry, at the given user-space position, in `/F1` at 10pt.
pub fn page_source(lines: &[(f64, f64, &str)]) -> String {
    lines
        .iter()
        .map(|(x, y, text)| format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm ({text}) Tj ET\n"))
        .collect()
}
