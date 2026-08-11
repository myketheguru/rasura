//! Dump a document's running headers and footers.
//!
//! Separate from the `paragraphs` example because this is the one analysis in
//! the crate with no single-page answer: it needs the whole document.
//!
//! ```text
//! cargo run -p rasura-layout --example running -- file.pdf [max-pages]
//! ```

use rasura_content::page;
use rasura_cos::Document;
use rasura_layout::{PageRegions, detect, place, resolve_page, rules, running_elements};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: running <file.pdf> [max-pages]");
        std::process::exit(2);
    };
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    let bytes = std::fs::read(&path).expect("read");
    let doc = Document::open(bytes).expect("open");
    let tree = page::pages(&doc).expect("pages");

    let mut per_page = Vec::new();
    let mut boxes = Vec::new();
    for p in tree.pages.iter().take(limit) {
        let (runs, _) = resolve_page(&doc, p);
        let rules = rules::collect(&doc, p);
        per_page.push(detect(place(&runs), &rules));
        boxes.push(p.media_box);
    }
    println!("{} page(s) analysed\n", per_page.len());

    let refs: Vec<PageRegions<'_>> = per_page
        .iter()
        .zip(boxes.iter())
        .map(|(b, m)| PageRegions { regions: b, media_box: *m })
        .collect();

    let found = running_elements(&refs);
    if found.is_empty() {
        println!("no running elements");
        return;
    }
    for (i, r) in found.iter().enumerate() {
        println!(
            "{i}  {:?}  on {} page(s){}{}",
            r.placement,
            r.pages.len(),
            if r.is_page_number { "  [page numbering]" } else { "" },
            if r.is_constant() { "  [constant]" } else { "" }
        );
        println!("     template: {:?}", r.template);
        let sample: Vec<&str> = r.instances.iter().take(6).map(|s| s.as_str()).collect();
        println!("     pages {:?}", &r.pages[..r.pages.len().min(6)]);
        println!("     e.g. {sample:?}");
    }
}
