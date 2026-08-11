//! Dump the reconstructed paragraph structure of a page.
//!
//! Aggregate corpus statistics say whether something is wrong; they never say
//! what. This prints one page's blocks, paragraphs, alignment, indents and
//! style runs so a suspicious number can be traced to the document that caused
//! it.
//!
//! ```text
//! cargo run -p rasura-layout --example paragraphs -- file.pdf [page]
//! ```

use rasura_content::page;
use rasura_cos::Document;
use rasura_layout::{detect, place, reconstruct, resolve_page, rules};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: paragraphs <file.pdf> [page-index]");
        std::process::exit(2);
    };
    let want: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let bytes = std::fs::read(&path).expect("read");
    let doc = Document::open(bytes).expect("open");
    let pages = page::pages(&doc).expect("pages");
    let Some(p) = pages.pages.get(want) else {
        eprintln!("page {want} out of range ({} pages)", pages.pages.len());
        std::process::exit(2);
    };

    let (runs, _) = resolve_page(&doc, p);
    let rules = rules::collect(&doc, p);
    let blocks = detect(place(&runs), &rules);
    println!("{} block(s), {} ruling line(s)\n", blocks.len(), rules.len());

    for (ti, t) in rasura_layout::detect_page(&blocks, &rules, &runs).iter().enumerate() {
        println!(
            "table {ti}  {}x{}  {:?}  fill {:.2}  x {:.0}..{:.0}  y {:.0}..{:.0}",
            t.rows,
            t.cols,
            t.origin,
            t.fill(),
            t.bbox.x0,
            t.bbox.x1,
            t.bbox.y0,
            t.bbox.y1
        );
        for r in 0..t.rows.min(8) {
            let row: Vec<String> = (0..t.cols.min(8))
                .map(|c| {
                    let text = t.cell(r, c).map(|c| c.text()).unwrap_or_default();
                    text.chars().take(18).collect()
                })
                .collect();
            println!("        | {}", row.join(" | "));
        }
        println!();
    }

    let notes = rasura_layout::footnotes(&blocks, &rules, p.media_box);
    let links = rasura_layout::running::link_markers(&blocks, &notes);
    for (n, link) in notes.iter().zip(links.iter()) {
        println!(
            "footnote  marker {:?}  {:.1}pt  {}  link {:?}",
            n.marker,
            n.size,
            if n.separated_by_rule { "ruled" } else { "unruled" },
            link.map(|m| (m.block, m.line))
        );
        let text: String = blocks[n.block].text().chars().take(90).collect();
        println!("        | {text}");
    }
    if !notes.is_empty() {
        println!();
    }

    for (bi, b) in blocks.iter().enumerate() {
        println!(
            "block {bi}  {:?}  x {:.0}..{:.0}  y {:.0}..{:.0}  {} line(s)",
            b.origin,
            b.bbox.x0,
            b.bbox.x1,
            b.bbox.y0,
            b.bbox.y1,
            b.lines.len()
        );
        for (pi, para) in reconstruct(b, &runs).iter().enumerate() {
            let styles: Vec<String> = {
                let mut seen: Vec<String> = Vec::new();
                for s in &para.styles {
                    let d = format!("{}@{:.1}", s.style.base_font, s.style.size);
                    if !seen.contains(&d) {
                        seen.push(d);
                    }
                }
                seen
            };
            println!(
                "  para {pi}  {:?} via {:?}  leading {:.1}  indent {:+.1}  \
                 margins {:.0}..{:.0}{}{}",
                para.alignment,
                para.reason,
                para.leading,
                para.first_line_indent,
                para.left_margin,
                para.right_margin,
                if para.hyphenation_was_present { "  [hyphenated]" } else { "" },
                match para.mcid {
                    Some(id) => format!("  [mcid {id}]"),
                    None => String::new(),
                }
            );
            println!("        styles: {}", styles.join(", "));
            for line in &b.lines[para.lines.clone()] {
                let text = rasura_layout::line_text(line);
                let text: String = text.chars().take(96).collect();
                println!("        | {text}");
            }
        }
        println!();
    }
}
