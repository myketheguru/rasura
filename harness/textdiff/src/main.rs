//! Differential text extraction. Spec 17, Phase 2's exit criterion:
//!
//! > Exit: text extraction with correct positions across the corpus.
//!
//! "Correct" needs an oracle, and pdf.js is the best one available: two decades
//! of work against the same corpus, and already vendored by `corpus/fetch.sh`.
//!
//! ```text
//! node harness/textdiff/extract-pdfjs.mjs corpus/external/pdfjs/test/pdfs corpus/pdfjs-text.jsonl
//! cargo run --release -p rasura-textdiff -- corpus/pdfjs-text.jsonl
//! ```
//!
//! # What is being compared, and what is not
//!
//! Phase 2 extracts *geometry*, and Unicode only through `/ToUnicode` -- §7.2
//! strategy 1 of seven. A page whose fonts have no `/ToUnicode` yields no text
//! here and readable text from pdf.js, which implements the whole chain. That is
//! a known Phase 3 gap, not a defect, so those pages are reported separately
//! rather than counted as failures. Q1 measured the size of that gap: 53% of
//! embedded fonts have a usable `/ToUnicode`.
//!
//! What *is* a Phase 2 defect: a page where both sides produce text and they
//! disagree, or where glyph positions disagree.

use rasura_content::{extract_page, page};
use rasura_cos::document::{Document, OpenOptions};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The two counts the comparison needs from the resolver.
struct SimpleReport {
    glyphs: usize,
    unmapped_glyphs: usize,
}

/// One page as pdf.js saw it.
#[derive(Debug, Default, Clone)]
struct Reference {
    file: String,
    page: usize,
    /// The view box, in PDF user space.
    x0: f64,
    y1: f64,
    text: String,
    items: Vec<(String, f64, f64)>,
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reference_path =
        args.first().cloned().unwrap_or_else(|| "corpus/pdfjs-text.jsonl".to_string());
    let corpus =
        args.get(1).cloned().unwrap_or_else(|| "corpus/external/pdfjs/test/pdfs".to_string());

    let Ok(raw) = std::fs::read_to_string(&reference_path) else {
        eprintln!(
            "no reference at {reference_path}.\n\
             Generate it with:\n  \
             node harness/textdiff/extract-pdfjs.mjs {corpus} {reference_path}"
        );
        return std::process::ExitCode::FAILURE;
    };

    let mut references: BTreeMap<(String, usize), Reference> = BTreeMap::new();
    for line in raw.lines() {
        if let Some(r) = parse_reference(line) {
            references.insert((r.file.clone(), r.page), r);
        }
    }
    if references.is_empty() {
        eprintln!("reference file has no usable records");
        return std::process::ExitCode::FAILURE;
    }

    let mut stats = Stats::default();
    let mut mismatches: Vec<String> = Vec::new();
    let mut current_file = String::new();
    let mut doc: Option<Document> = None;
    let mut pages = Vec::new();

    // §7.7's running elements and §7.8's model are the two things here with no
    // single-page answer, so pages are buffered per document and analysed when
    // the file changes.
    let mut buffered: Vec<BufferedPage> = Vec::new();

    for ((file, index), reference) in &references {
        if *file != current_file {
            flush_document(doc.as_ref(), &current_file, &mut buffered, &mut stats, &mut mismatches);
            current_file = file.clone();
            doc = std::fs::read(Path::new(&corpus).join(file))
                .ok()
                .and_then(|b| Document::open_with(b, &OpenOptions::default()).ok());
            pages =
                doc.as_ref().and_then(|d| page::pages(d).ok()).map(|t| t.pages).unwrap_or_default();
        }
        let (Some(doc), Some(p)) = (doc.as_ref(), pages.get(*index)) else {
            stats.unopened += 1;
            continue;
        };

        // Extract with the content layer, then run the full §7.2 derivation
        // chain over it. Before Phase 3 this was `/ToUnicode` alone.
        // Extracted twice on purpose: once bare, once with the standard-14
        // metrics. The difference is the only honest measure of whether the
        // §8.2 hook is doing anything, and "we wired it up" is not a result.
        let (bare_runs, bare_report, _) = extract_page(doc, p);
        stats.glyphs_without_widths_bare += bare_report.glyphs_without_widths;
        drop(bare_runs);

        let (raw_runs, text_report_supplied, _) =
            rasura_content::text::extract_page_with(doc, p, &rasura_layout::Standard14Widths);
        stats.glyphs_without_widths_supplied += text_report_supplied.glyphs_without_widths;
        let (resolved, resolve_report) = rasura_layout::resolve_runs(doc, p, raw_runs);

        // Assemble lines, detect blocks, segment words, and compare *that*.
        // This is what a caller actually gets, and it exercises §7.3 to §7.5
        // over the whole corpus rather than over fixtures.
        let placed = rasura_layout::place(&resolved);
        let placed_total = placed.len();
        let rules = rasura_layout::rules::collect(doc, p);
        let blocks = rasura_layout::detect(placed, &rules);

        // Cutting must partition: every glyph that went in comes out in exactly
        // one block. A silent loss here would shorten extracted text in a way
        // the similarity score alone would blame on the mapping chain.
        let in_blocks: usize =
            blocks.iter().flat_map(|b| b.lines.iter()).map(|l| l.glyphs.len()).sum();
        if in_blocks != placed_total {
            stats.glyphs_lost += placed_total.abs_diff(in_blocks);
            if mismatches.len() < 60 {
                mismatches.push(format!(
                    "{file} p{index}: block detection lost glyphs, {placed_total} in, \
                     {in_blocks} out"
                ));
            }
        }

        // §7.6 over the whole corpus. Paragraphs must partition their block's
        // lines contiguously -- a gap would silently drop a line from every
        // consumer downstream, and reflow in Phase 5 works paragraph by
        // paragraph, so a dropped line is text that never moves.
        for b in &blocks {
            let paras = rasura_layout::reconstruct(b, &resolved);
            stats.paragraphs += paras.len();
            let mut next = 0usize;
            let mut ok = true;
            for p in &paras {
                ok &= p.lines.start == next;
                next = p.lines.end;
                // A one-line paragraph has no alignment to infer, so counting
                // it as "unknown" alongside genuine ambiguity would hide how
                // often the inference actually fails.
                if p.lines.len() < 2 {
                    stats.single_line += 1;
                } else {
                    *stats.alignments.entry(alignment_name(p.alignment)).or_default() += 1;
                }
                *stats.splits.entry(split_name(p.reason)).or_default() += 1;
                stats.style_runs += p.styles.len();
                if p.hyphenation_was_present {
                    stats.hyphenated += 1;
                }
                if p.mcid.is_some() {
                    stats.tagged_paragraphs += 1;
                }
            }
            if !ok || next != b.lines.len() {
                stats.lines_lost += 1;
                if mismatches.len() < 60 {
                    mismatches.push(format!(
                        "{file} p{index}: paragraphs do not partition their block, \
                         {} line(s) in, covered to {next}",
                        b.lines.len()
                    ));
                }
            }
        }

        // §7.7. Tables are the one structure here that can be badly wrong in a
        // way no aggregate catches, so the ruled and inferred routes are
        // counted separately: a ruled grid was drawn by the producer, an
        // aligned one is this library's guess.
        let tables = rasura_layout::detect_page(&blocks, &rules, &resolved);
        for t in &tables {
            if t.is_ruled() {
                stats.tables_ruled += 1;
                stats.cells_ruled += t.cells.len();
            } else {
                stats.tables_aligned += 1;
                stats.cells_aligned += t.cells.len();
            }
            if t.cells.len() > stats.biggest_table {
                stats.biggest_table = t.cells.len();
                stats.biggest_table_where =
                    format!("{file} p{index} ({}x{}, fill {:.3})", t.rows, t.cols, t.fill());
            }

            // Cells must partition the glyphs the table was built from. Stated
            // against the table's own input rather than a geometric re-query,
            // because two tables' bounding boxes can overlap and a re-query
            // would then count the same glyph twice and blame the library.
            if t.source_glyphs() != t.cell_glyphs() {
                stats.cell_glyphs_lost += t.source_glyphs().abs_diff(t.cell_glyphs());
                if mismatches.len() < 60 {
                    mismatches.push(format!(
                        "{file} p{index}: table cells lost glyphs, {} in, {} out",
                        t.source_glyphs(),
                        t.cell_glyphs()
                    ));
                }
            }
        }

        let notes = rasura_layout::footnotes(&blocks, &rules, p.media_box);
        stats.footnotes += notes.len();
        stats.footnotes_ruled += notes.iter().filter(|n| n.separated_by_rule).count();
        stats.footnote_links += notes.iter().filter(|n| n.marker_site.is_some()).count();
        for n in &notes {
            if n.marker_site.is_none() && stats.footnote_samples.len() < 12 {
                let text: String = blocks[n.block].text().chars().take(60).collect();
                stats.footnote_samples.push(format!(
                    "{file} p{index}: marker {:?}, {} -- {text}",
                    n.marker,
                    if n.separated_by_rule { "ruled" } else { "unruled" }
                ));
            }
        }

        buffered.push(BufferedPage {
            regions: blocks.clone(),
            rules: rules.clone(),
            runs: resolved.clone(),
            graphics: rasura_layout::graphics::collect(doc, p),
            media_box: p.media_box,
            crop_box: p.crop_box,
            rotate: p.rotate,
        });

        let ours: String = blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join("\n\n");
        stats.blocks += blocks.len();
        stats.rules += rules.len();
        stats.lines += blocks.iter().map(|b| b.lines.len()).sum::<usize>();
        stats.words += blocks
            .iter()
            .flat_map(|b| b.lines.iter())
            .map(|l| rasura_layout::segment(l).len())
            .sum::<usize>();

        let text_report = SimpleReport {
            glyphs: resolve_report.glyphs,
            unmapped_glyphs: resolve_report.glyphs - resolve_report.mapped,
        };
        stats.pages += 1;
        stats.our_glyphs += text_report.glyphs;
        stats.unmapped += text_report.unmapped_glyphs;
        for (strategy, n) in &resolve_report.by_strategy {
            *stats.strategies.entry(strategy.as_str()).or_default() += n;
        }

        let theirs = &reference.text;
        let ours_n = normalise(&ours);
        let theirs_n = normalise(theirs);

        if theirs_n.is_empty() && ours_n.is_empty() {
            stats.both_empty += 1;
            continue;
        }
        if ours_n.is_empty() {
            // Almost always a missing /ToUnicode: a Phase 3 gap, not a defect.
            if text_report.unmapped_glyphs > 0 {
                stats.tounicode_gap += 1;
            } else {
                stats.we_found_nothing += 1;
                if mismatches.len() < 40 {
                    mismatches.push(format!(
                        "{file} p{index}: pdf.js found {} chars, we found none and had no \
                         unmapped glyphs either",
                        theirs_n.chars().count()
                    ));
                }
            }
            continue;
        }
        if theirs_n.is_empty() {
            stats.they_found_nothing += 1;
            continue;
        }

        if reference_is_degenerate(&theirs_n) && !reference_is_degenerate(&ours_n) {
            stats.reference_degenerate += 1;
            continue;
        }

        // Both produced text. Compare as multisets of characters: content order
        // is not reading order until Phase 3, so sequence differences are
        // expected and character content is the part that must already agree.
        let similarity = char_similarity(&ours_n, &theirs_n);
        stats.compared += 1;
        stats.similarity_total += similarity;
        if similarity >= 0.98 {
            stats.near_exact += 1;
        } else if similarity >= 0.90 {
            stats.close += 1;
        } else {
            stats.diverged += 1;
            // Which way does the disagreement run? Containment says whether we
            // found *less* than pdf.js (a gap in this library) or *more* (pdf.js
            // failing on a font), and the two need entirely different responses.
            let ours_in_theirs = containment(&ours_n, &theirs_n);
            let theirs_in_ours = containment(&theirs_n, &ours_n);
            let verdict = if theirs_in_ours >= 0.95 {
                stats.we_found_more += 1;
                "we found everything pdf.js did, and more"
            } else if ours_in_theirs >= 0.95 {
                stats.we_found_less += 1;
                // The distinction that decides whether this is a Phase 3 gap or
                // a Phase 2 bug: if some glyphs on the page had no /ToUnicode,
                // the missing text is the mapping chain, which is Phase 3. If
                // every glyph mapped and text is still missing, this layer lost
                // it, and that is a defect now.
                if text_report.unmapped_glyphs > 0 {
                    stats.we_found_less_unmapped += 1;
                    "pdf.js found more; this page had unmapped glyphs (Phase 3 gap)"
                } else {
                    stats.we_found_less_all_mapped += 1;
                    "pdf.js found more, and every glyph we saw mapped (Phase 2 defect)"
                }
            } else {
                stats.different_content += 1;
                "different content on both sides"
            };
            if mismatches.len() < 60 {
                mismatches.push(format!(
                    "{file} p{index}: similarity {similarity:.2} -- {verdict}\n    \
                     ours   : {}\n    pdf.js : {}",
                    preview(&ours_n),
                    preview(&theirs_n)
                ));
            }
        }

        // Positions, for pages whose text already agrees. pdf.js reports the
        // origin in PDF user space with y up; ours is device space with y down,
        // so the reference y is flipped through the page height before either
        // is believed.
        // Only on pages where every glyph mapped. Our bounding box is built
        // from glyphs that have Unicode; pdf.js's from all its items. On a page
        // with unmapped glyphs those are different subsets, and the difference
        // would be measuring the mapping gap rather than the geometry.
        if similarity >= 0.98 && text_report.unmapped_glyphs == 0 && !reference.items.is_empty() {
            if let Some(delta) = position_delta(&resolved, reference) {
                stats.position_pages += 1;
                stats.position_total += delta;
                if delta > 1.0 {
                    stats.position_off += 1;
                    if mismatches.len() < 40 {
                        mismatches
                            .push(format!("{file} p{index}: text origin differs by {delta:.2} pt"));
                    }
                }
            }
        }
    }

    flush_document(doc.as_ref(), &current_file, &mut buffered, &mut stats, &mut mismatches);
    stats.report(&mismatches)
}

#[derive(Default)]
struct Stats {
    pages: usize,
    unopened: usize,
    our_glyphs: usize,
    unmapped: usize,
    both_empty: usize,
    tounicode_gap: usize,
    we_found_nothing: usize,
    they_found_nothing: usize,
    reference_degenerate: usize,
    compared: usize,
    similarity_total: f64,
    near_exact: usize,
    close: usize,
    diverged: usize,
    we_found_more: usize,
    we_found_less: usize,
    we_found_less_unmapped: usize,
    we_found_less_all_mapped: usize,
    different_content: usize,
    position_pages: usize,
    position_total: f64,
    position_off: usize,
    /// How many glyphs each §7.2 strategy accounted for.
    strategies: BTreeMap<&'static str, usize>,
    lines: usize,
    words: usize,
    blocks: usize,
    rules: usize,
    glyphs_lost: usize,
    paragraphs: usize,
    style_runs: usize,
    hyphenated: usize,
    tagged_paragraphs: usize,
    lines_lost: usize,
    single_line: usize,
    tables_ruled: usize,
    tables_aligned: usize,
    cells_ruled: usize,
    cells_aligned: usize,
    biggest_table: usize,
    biggest_table_where: String,
    cell_glyphs_lost: usize,
    footnotes: usize,
    footnotes_ruled: usize,
    footnote_links: usize,
    footnote_samples: Vec<String>,
    running: usize,
    running_page_numbers: usize,
    running_constant: usize,
    headers: usize,
    docs_with_running: usize,
    model_pages: usize,
    tagged_docs: usize,
    order_from_structure: usize,
    order_from_geometry: usize,
    order_defects: usize,
    model_glyphs_lost: usize,
    block_kinds: BTreeMap<&'static str, usize>,

    // Non-text content. Counted *and asserted*: until now images and vector art
    // reached the model and nothing checked that they arrived, so one dropped
    // from every page in the corpus would have moved a number in an unasserted
    // histogram and failed nothing.
    images: usize,
    images_inline: usize,
    images_masked: usize,
    images_rotated: usize,
    images_without_pixels: usize,
    vectors: usize,
    vector_paths: usize,
    pages_with_images: usize,
    pages_with_vectors: usize,
    /// Images or vector blocks collected on a page that did not reach the model.
    graphics_lost: usize,
    order_pairs: usize,
    order_concordant: usize,
    order_pages_compared: usize,
    order_pages_exact: usize,
    glyphs_without_widths_bare: usize,
    glyphs_without_widths_supplied: usize,
    alignments: BTreeMap<&'static str, usize>,
    splits: BTreeMap<&'static str, usize>,
}

fn alignment_name(a: rasura_layout::Alignment) -> &'static str {
    use rasura_layout::Alignment::*;
    match a {
        Left => "left",
        Right => "right",
        Centre => "centre",
        Justified => "justified",
        Unknown => "unknown",
    }
}

fn split_name(r: rasura_layout::SplitReason) -> &'static str {
    use rasura_layout::SplitReason::*;
    match r {
        BlockStart => "block start",
        Mcid => "/MCID (authoritative)",
        StyleChange => "style change",
        FirstLineIndent => "first-line indent",
        LeadingGap => "leading gap",
    }
}

/// Assemble §7.8's document model for one file and check what it preserves.
///
/// The model is where a classification mistake finally becomes visible: a
/// region claimed by the wrong classifier, or by none, is content that has
/// silently left the document. So the assertion is about *coverage* — every
/// region's glyphs must survive into some block — rather than about whether
/// each classification was the nicest one.
fn check_model(
    doc: &Document,
    buffered: &[BufferedPage],
    stats: &mut Stats,
    file: &str,
    mismatches: &mut Vec<String>,
) {
    let inputs: Vec<rasura_layout::PageInput<'_>> = buffered
        .iter()
        .map(|p| rasura_layout::PageInput {
            regions: &p.regions,
            // Not read here: this harness measures text reconstruction against
            // pdf.js, and pdf.js's reference text does not include annotation
            // content either. Passing them in would make the two incomparable.
            annotations: Vec::new(),
            rules: &p.rules,
            runs: &p.runs,
            media_box: p.media_box,
            crop_box: p.crop_box,
            rotate: p.rotate,
            graphics: p.graphics.clone(),
        })
        .collect();

    let model = rasura_layout::model::build(doc, inputs);
    stats.model_pages += model.pages.len();
    if model.structure.is_some() {
        stats.tagged_docs += 1;
    }
    match model.order_source {
        rasura_layout::OrderSource::Structure => stats.order_from_structure += 1,
        rasura_layout::OrderSource::Geometry => stats.order_from_geometry += 1,
    }
    for page in &model.pages {
        for b in &page.blocks {
            *stats.block_kinds.entry(b.kind()).or_default() += 1;
        }
    }

    // The fourth partition, and the one that was missing: graphics collected on
    // a page must all reach the model. `model::build` pushes images and vectors
    // one for one, so any discrepancy is a defect rather than a judgement call.
    for (i, buffer) in buffered.iter().enumerate() {
        let Some(page) = model.pages.get(i) else { continue };
        let collected_images = buffer.graphics.images.len();
        let collected_vectors = buffer.graphics.vectors.len();

        let in_model_images =
            page.blocks.iter().filter(|b| matches!(b, rasura_layout::Block::Image(_))).count();
        let in_model_vectors =
            page.blocks.iter().filter(|b| matches!(b, rasura_layout::Block::Vector(_))).count();

        if in_model_images != collected_images || in_model_vectors != collected_vectors {
            stats.graphics_lost += collected_images.abs_diff(in_model_images)
                + collected_vectors.abs_diff(in_model_vectors);
            if mismatches.len() < 60 {
                mismatches.push(format!(
                    "{file} p{i}: {collected_images} image(s) and {collected_vectors} vector(s) \
                     collected, {in_model_images} and {in_model_vectors} in the model"
                ));
            }
        }

        stats.images += collected_images;
        stats.vectors += collected_vectors;
        if collected_images > 0 {
            stats.pages_with_images += 1;
        }
        if collected_vectors > 0 {
            stats.pages_with_vectors += 1;
        }
        for image in &buffer.graphics.images {
            if image.inline {
                stats.images_inline += 1;
            }
            if image.is_mask {
                stats.images_masked += 1;
            }
            // A transform whose off-diagonal terms are non-zero is rotated or
            // skewed. Worth counting because it is the case an edit that moves
            // an image has to preserve, and the case a bbox alone cannot
            // describe.
            if image.ctm.b.abs() > 1e-9 || image.ctm.c.abs() > 1e-9 {
                stats.images_rotated += 1;
            }
            if image.pixels.is_none() {
                stats.images_without_pixels += 1;
            }
        }
        for vector in &buffer.graphics.vectors {
            stats.vector_paths += vector.count;
        }
    }

    // Reading order must list every block exactly once. A duplicate would emit
    // the same text twice; an omission would lose it.
    let total: usize = model.pages.iter().map(|p| p.blocks.len()).sum();
    let mut seen = model.reading_order.clone();
    seen.sort();
    seen.dedup();
    if seen.len() != total || model.reading_order.len() != total {
        stats.order_defects += 1;
        if mismatches.len() < 60 {
            mismatches.push(format!(
                "{file}: reading order lists {} of {total} block(s), {} distinct",
                model.reading_order.len(),
                seen.len()
            ));
        }
    }

    // Phase 3's exit criterion asks for reading order to be *correct*, and
    // until now there was no oracle: pdf.js is content-ordered, so comparing
    // against it proves nothing about ordering, and two geometric heuristics
    // agreeing proves nothing at all. A tagged document is different -- the
    // producer wrote the reading order down. Where one exists, §7.5's cut-tree
    // order can finally be scored against it.
    if let Some(tree) = model.structure.as_ref().filter(|t| !t.truncated) {
        for (p, page) in model.pages.iter().enumerate() {
            // The geometric position of each block that the tree names, in the
            // order the tree names them.
            let mut positions: Vec<usize> = Vec::new();
            for mcid in tree.mcid_order(p) {
                if let Some(i) = page.blocks.iter().position(|b| match b {
                    rasura_layout::Block::Paragraph(para) => para.mcid == Some(mcid),
                    _ => false,
                }) {
                    if !positions.contains(&i) {
                        positions.push(i);
                    }
                }
            }
            if positions.len() < 2 {
                continue;
            }
            // Concordant pairs: for every pair the tree orders, does the
            // geometry put them the same way round? This is Kendall's tau
            // without the normalisation, and it degrades gracefully -- one
            // block out of place costs a little, a reversed page costs
            // everything.
            let mut concordant = 0usize;
            let mut total = 0usize;
            for i in 0..positions.len() {
                for j in i + 1..positions.len() {
                    total += 1;
                    if positions[i] < positions[j] {
                        concordant += 1;
                    }
                }
            }
            stats.order_pairs += total;
            stats.order_concordant += concordant;
            stats.order_pages_compared += 1;
            if concordant == total {
                stats.order_pages_exact += 1;
            } else if mismatches.len() < 60 && concordant * 2 < total {
                mismatches.push(format!(
                    "{file} p{p}: geometric reading order disagrees with the structure tree \
                     on {}/{total} pairs",
                    total - concordant
                ));
            }
        }
    }

    // Every glyph that went into a region must be reachable from some block.
    // Tables hold their own copies, so the check is per page and by count.
    for (buf, page) in buffered.iter().zip(model.pages.iter()) {
        let into: usize =
            buf.regions.iter().flat_map(|r| r.lines.iter()).map(|l| l.glyphs.len()).sum();
        let out: usize = page
            .blocks
            .iter()
            .map(|b| match b {
                rasura_layout::Block::Table(t) => t.cell_glyphs(),
                _ => 0,
            })
            .sum::<usize>()
            + page.lines.iter().flatten().map(|l| l.glyphs.len()).sum::<usize>();
        if out < into {
            stats.model_glyphs_lost += into - out;
            if mismatches.len() < 60 {
                mismatches.push(format!(
                    "{file}: the model dropped glyphs, {into} in regions, {out} in blocks"
                ));
            }
        }
    }
}

/// One page held until the whole document is available.
struct BufferedPage {
    regions: Vec<rasura_layout::Region>,
    rules: Vec<rasura_layout::Rule>,
    runs: Vec<rasura_layout::ResolvedRun>,
    graphics: rasura_layout::Graphics,
    media_box: rasura_content::matrix::Rect,
    crop_box: rasura_content::matrix::Rect,
    rotate: i32,
}

/// Analyse one document's buffered pages, then clear.
fn flush_document(
    doc: Option<&Document>,
    file: &str,
    buffered: &mut Vec<BufferedPage>,
    stats: &mut Stats,
    mismatches: &mut Vec<String>,
) {
    if buffered.len() >= 3 {
        let refs: Vec<rasura_layout::PageRegions<'_>> = buffered
            .iter()
            .map(|p| rasura_layout::PageRegions { regions: &p.regions, media_box: p.media_box })
            .collect();
        let found = rasura_layout::running_elements(&refs);
        if !found.is_empty() {
            stats.docs_with_running += 1;
        }
        stats.running += found.len();
        stats.running_page_numbers += found.iter().filter(|r| r.is_page_number).count();
        stats.running_constant += found.iter().filter(|r| r.is_constant()).count();
        stats.headers +=
            found.iter().filter(|r| r.placement == rasura_layout::Placement::Header).count();
    }
    if let Some(doc) = doc
        && !buffered.is_empty()
    {
        check_model(doc, buffered, stats, file, mismatches);
    }
    buffered.clear();
}

impl Stats {
    fn report(&self, mismatches: &[String]) -> std::process::ExitCode {
        println!("=== text extraction vs pdf.js ===\n");
        println!("{} page(s) compared, {} could not be opened", self.pages, self.unopened);
        println!("{} glyph(s) extracted, {} unmapped", self.our_glyphs, self.unmapped);
        println!(
            "{} glyph(s) had no width from the file; the standard-14 metrics (§8.2) \
             leave {} without one",
            self.glyphs_without_widths_bare, self.glyphs_without_widths_supplied
        );
        println!(
            "{} block(s), {} line(s), {} word(s), {} ruling line(s)",
            self.blocks, self.lines, self.words, self.rules
        );
        println!(
            "{} paragraph(s), {} style run(s), {} hyphenated, {} from tagged content",
            self.paragraphs, self.style_runs, self.hyphenated, self.tagged_paragraphs
        );
        println!(
            "{} image(s) on {} page(s): {} inline, {} stencil mask(s), {} rotated or skewed, \
             {} without /Width or /Height",
            self.images,
            self.pages_with_images,
            self.images_inline,
            self.images_masked,
            self.images_rotated,
            self.images_without_pixels
        );
        println!(
            "{} vector block(s) on {} page(s), {} painted path(s)\n",
            self.vectors, self.pages_with_vectors, self.vector_paths
        );
        println!(
            "{} table(s): {} ruled by the producer ({} cells), {} inferred from \
             alignment ({} cells); largest {} cells",
            self.tables_ruled + self.tables_aligned,
            self.tables_ruled,
            self.cells_ruled,
            self.tables_aligned,
            self.cells_aligned,
            self.biggest_table
        );
        if !self.biggest_table_where.is_empty() {
            println!("  largest: {}", self.biggest_table_where);
        }
        println!(
            "{} footnote(s), {} with a separating rule, {} linked to an in-text marker\n",
            self.footnotes, self.footnotes_ruled, self.footnote_links
        );
        println!(
            "{} running element(s) across {} document(s): {} header(s), {} footer(s); \
             {} constant, {} page numbering",
            self.running,
            self.docs_with_running,
            self.headers,
            self.running - self.headers,
            self.running_constant,
            self.running_page_numbers
        );
        println!(
            "\n§7.8 document model: {} page(s), {} tagged document(s); reading order from \
             structure {}, from geometry {}",
            self.model_pages, self.tagged_docs, self.order_from_structure, self.order_from_geometry
        );
        for (kind, n) in &self.block_kinds {
            println!("  {kind:<10} {n:>7}");
        }
        if self.order_pages_compared > 0 {
            println!(
                "\nReading order vs the structure tree, the only oracle available:\n  \
                 {}/{} ordered pairs concordant ({:.1}%), {}/{} page(s) exactly right",
                self.order_concordant,
                self.order_pairs,
                self.order_concordant as f64 * 100.0 / self.order_pairs.max(1) as f64,
                self.order_pages_exact,
                self.order_pages_compared
            );
        }
        println!();

        for s in &self.footnote_samples {
            println!("  unlinked: {s}");
        }
        if !self.footnote_samples.is_empty() {
            println!();
        }

        if self.paragraphs > 0 {
            let pct = |n: usize| n as f64 * 100.0 / self.paragraphs as f64;
            let multi: usize = self.alignments.values().sum();
            println!(
                "Paragraph alignment (§7.6), over the {multi} multi-line paragraph(s); \
                 {} are single-line and have no alignment to infer:",
                self.single_line
            );
            for (name, n) in &self.alignments {
                let share = if multi > 0 { *n as f64 * 100.0 / multi as f64 } else { 0.0 };
                println!("  {name:<10} {n:>7}  {share:>5.1}%");
            }
            println!("\nWhy each paragraph began:");
            for (name, n) in &self.splits {
                println!("  {name:<22} {n:>7}  {:>5.1}%", pct(*n));
            }
            println!();
        }

        if !self.strategies.is_empty() {
            println!("--- which §7.2 strategy resolved each glyph ---");
            let mut rows: Vec<_> = self.strategies.iter().collect();
            rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            let total: usize = self.strategies.values().sum();
            for (name, n) in rows {
                println!("  {name:<24} {n:>9}  {:>5.1}%", 100.0 * *n as f64 / total as f64);
            }
            println!();
        }

        println!("--- outcome ---");
        println!("  {:<34} {}", "both found no text", self.both_empty);
        println!("  {:<34} {}", "pdf.js only, we had unmapped glyphs", self.tounicode_gap);
        println!("  {:<34} {}", "pdf.js only, no unmapped glyphs", self.we_found_nothing);
        println!("  {:<34} {}", "we found text, pdf.js did not", self.they_found_nothing);
        println!("  {:<34} {}", "pdf.js output degenerate, ours not", self.reference_degenerate);
        println!("  {:<34} {}", "both found text", self.compared);

        if self.compared > 0 {
            println!("\n--- agreement on the {} comparable page(s) ---", self.compared);
            println!(
                "  {:<20} {:>6}  {:>5.1}%",
                "near-exact (>=0.98)",
                self.near_exact,
                100.0 * self.near_exact as f64 / self.compared as f64
            );
            println!(
                "  {:<20} {:>6}  {:>5.1}%",
                "close (>=0.90)",
                self.close,
                100.0 * self.close as f64 / self.compared as f64
            );
            println!(
                "  {:<20} {:>6}  {:>5.1}%",
                "diverged",
                self.diverged,
                100.0 * self.diverged as f64 / self.compared as f64
            );
            println!("  mean similarity: {:.3}", self.similarity_total / self.compared as f64);
            if self.diverged > 0 {
                println!("\n  of the {} diverged:", self.diverged);
                println!("    {:<40} {}", "we found more than pdf.js", self.we_found_more);
                println!("    {:<40} {}", "we found less than pdf.js", self.we_found_less);
                println!(
                    "      {:<38} {}",
                    "of which had unmapped glyphs (Phase 3)", self.we_found_less_unmapped
                );
                println!(
                    "      {:<38} {}",
                    "of which mapped everything (Phase 2)", self.we_found_less_all_mapped
                );
                println!(
                    "    {:<40} {}",
                    "different content (order, bidi)", self.different_content
                );
            }
        }

        if self.position_pages > 0 {
            println!("\n--- glyph positions on {} page(s) ---", self.position_pages);
            println!(
                "  mean delta at the text origin: {:.3} pt",
                self.position_total / self.position_pages as f64
            );
            println!("  pages off by more than 1 pt: {}", self.position_off);
        }

        if !mismatches.is_empty() {
            println!("\n--- sample disagreements ---");
            for m in mismatches.iter().take(20) {
                println!("  {m}");
            }
        }

        // The gate.
        //
        // Not "95% of pages must agree": that would fail this library for the
        // /ToUnicode chain it does not yet have, which is Phase 3 by design and
        // whose size Q1 already measured. What Phase 2 owes is that no text is
        // lost for any *other* reason, and that geometry is right.
        //
        // So: zero pages where every glyph mapped and text still went missing,
        // and positions agreeing on the pages where both sides saw the same
        // glyphs.
        let text_ok = self.we_found_less_all_mapped == 0;
        let pos_ok = self.position_pages == 0
            || self.position_off as f64 / self.position_pages as f64 <= 0.05;
        // Region detection partitions; losing a glyph is never acceptable. Nor
        // is a paragraph split that leaves a line in no paragraph at all.
        let blocks_ok = self.glyphs_lost == 0;
        let paras_ok = self.lines_lost == 0;
        let cells_ok = self.cell_glyphs_lost == 0;
        // §7.8: a block reachable from no reading-order entry, or a region
        // whose glyphs reach no block, is content that has left the document.
        let model_ok = self.model_glyphs_lost == 0 && self.order_defects == 0;
        // Non-text content has to arrive too. Text has had four partition
        // assertions since Phase 3; images and vector art had none, so they
        // could vanish between collection and the model without anything
        // going red.
        let graphics_ok = self.graphics_lost == 0;

        println!();
        if !graphics_ok {
            println!(
                "FAIL: {} image(s) or vector block(s) were collected and did not reach the model.",
                self.graphics_lost
            );
        }
        if !blocks_ok {
            println!("FAIL: block detection lost {} glyph(s).", self.glyphs_lost);
        }
        if !paras_ok {
            println!("FAIL: paragraphs failed to partition {} block(s).", self.lines_lost);
        }
        if !cells_ok {
            println!("FAIL: table cells lost {} glyph(s).", self.cell_glyphs_lost);
        }
        if !model_ok {
            println!(
                "FAIL: the document model dropped {} glyph(s) and has {} reading-order defect(s).",
                self.model_glyphs_lost, self.order_defects
            );
        }
        if text_ok && pos_ok && blocks_ok && paras_ok && cells_ok && model_ok && graphics_ok {
            println!("Phase 2 exit criterion met on this corpus.");
            println!(
                "  {} page(s) still short of pdf.js purely for want of /ToUnicode; \
                 that is Phase 3.",
                self.tounicode_gap + self.we_found_less_unmapped
            );
            std::process::ExitCode::SUCCESS
        } else {
            if !text_ok {
                println!(
                    "FAIL: {} page(s) lost text despite every glyph mapping.",
                    self.we_found_less_all_mapped
                );
            }
            if !pos_ok {
                println!(
                    "FAIL: {}/{} fully-mapped pages disagree on position by more than 1 pt.",
                    self.position_off, self.position_pages
                );
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// How far apart the two sides put the *text on the page*, compared as bounding
/// boxes of glyph origins.
///
/// Glyph-to-glyph would need the same segmentation on both sides, which is
/// Phase 3. Comparing the *first* of each is worse than useless: pdf.js groups
/// by text item and this library groups by showing operator, so "first" means
/// different things and the metric measures the grouping rather than the
/// geometry. A bounding box is invariant to both, and still catches what
/// matters now -- a systematic offset, a flipped axis, a missing rotation, a
/// mishandled crop box.
fn position_delta(runs: &[rasura_layout::ResolvedRun], reference: &Reference) -> Option<f64> {
    let mut ours = BBox::default();
    for run in runs {
        for (glyph, text) in run.run.glyphs.iter().zip(&run.text) {
            // Whitespace-only glyphs are excluded to match the reference
            // filter. pdf.js drops items that trim to nothing, so counting a
            // leading space here would drag our minimum left of theirs and
            // measure the filter rather than the geometry.
            if text.as_deref().is_some_and(|t| !t.trim().is_empty()) {
                ours.add(glyph.origin.x, glyph.origin.y);
            }
        }
    }
    let mut theirs = BBox::default();
    for (s, x, y) in &reference.items {
        if !s.trim().is_empty() {
            // pdf.js reports PDF user space: y up, origin at the media box.
            // Ours is device space: y down, origin at the crop box corner.
            theirs.add(x - reference.x0, reference.y1 - y);
        }
    }
    let (a, b) = (ours.finish()?, theirs.finish()?);
    // Only the top-left corner is comparable. pdf.js reports one origin per
    // *text item*, which is where that item starts; this library reports one
    // per glyph. So the two maxima measure different things -- theirs is the
    // last item's start, ours is the last glyph's start -- and differ by about
    // the width of a run. The minima are the same quantity on both sides: the
    // leftmost and topmost place text begins.
    Some(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt())
}

#[derive(Default)]
struct BBox {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    any: bool,
}

impl BBox {
    fn add(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if !self.any {
            self.min_x = x;
            self.max_x = x;
            self.min_y = y;
            self.max_y = y;
            self.any = true;
            return;
        }
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_y = self.min_y.min(y);
        self.max_y = self.max_y.max(y);
    }

    fn finish(&self) -> Option<(f64, f64, f64, f64)> {
        self.any.then_some((self.min_x, self.min_y, self.max_x, self.max_y))
    }
}

/// True when the reference is obviously wrong rather than merely different.
///
/// pdf.js emits a run of one repeated character when it cannot map a font --
/// `bbbbbbbb` for a page of English prose. Counting that as a disagreement would
/// penalise this library for being *more* correct, so those pages are reported
/// separately instead of failing the gate.
fn reference_is_degenerate(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 20 {
        return false;
    }
    // Not "one distinct character" -- pdf.js's fallback output usually has a
    // few. The signal is one character dominating a long stretch of what should
    // be prose.
    let mut counts: BTreeMap<char, usize> = BTreeMap::new();
    for c in &chars {
        *counts.entry(*c).or_default() += 1;
    }
    let most = counts.values().copied().max().unwrap_or(0);
    most as f64 / chars.len() as f64 > 0.7
}

/// Collapse whitespace and drop it entirely for comparison: pdf.js inserts
/// spaces its own way, and word segmentation is Phase 3.
fn normalise(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What fraction of `a`'s characters `b` also contains, as a multiset.
/// `containment(a, b) == 1.0` means `b` accounts for all of `a`.
fn containment(a: &str, b: &str) -> f64 {
    if a.is_empty() {
        return 1.0;
    }
    let mut counts: BTreeMap<char, i64> = BTreeMap::new();
    for c in b.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let mut shared = 0i64;
    for c in a.chars() {
        if let Some(n) = counts.get_mut(&c)
            && *n > 0
        {
            *n -= 1;
            shared += 1;
        }
    }
    shared as f64 / a.chars().count() as f64
}

/// Multiset similarity over characters: how much of the smaller string's
/// content the larger one accounts for.
fn char_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut counts: BTreeMap<char, i64> = BTreeMap::new();
    for c in a.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let mut shared = 0i64;
    for c in b.chars() {
        if let Some(n) = counts.get_mut(&c)
            && *n > 0
        {
            *n -= 1;
            shared += 1;
        }
    }
    let total = a.chars().count().max(b.chars().count()) as f64;
    if total == 0.0 { 1.0 } else { shared as f64 / total }
}

fn preview(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    if s.chars().count() > 60 { format!("{t}...") } else { t }
}

/// Minimal JSON reading, rather than adding a dependency to a harness.
fn parse_reference(line: &str) -> Option<Reference> {
    let mut r = Reference {
        file: json_string(line, "\"file\":")?,
        page: json_number(line, "\"page\":")? as usize,
        x0: json_number(line, "\"x0\":").unwrap_or(0.0),
        y1: json_number(line, "\"y1\":").unwrap_or(0.0),
        text: json_string(line, "\"text\":").unwrap_or_default(),
        items: Vec::new(),
    };
    // Items are only needed for the position check; parse the first few.
    let mut rest = line;
    while let Some(at) = rest.find("{\"str\":") {
        rest = &rest[at..];
        let str_val = json_string(rest, "\"str\":").unwrap_or_default();
        let x = json_number(rest, "\"x\":").unwrap_or(0.0);
        let y = json_number(rest, "\"y\":").unwrap_or(0.0);
        r.items.push((str_val, x, y));
        rest = &rest[1..];
        // Enough to bound the text on a dense page; the metric is a bounding
        // box, so a few hundred origins settle it.
        if r.items.len() >= 500 {
            break;
        }
    }
    Some(r)
}

fn json_string(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    let rest = line[at..].trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        // A lone surrogate cannot be a char; keep the
                        // replacement rather than dropping the character.
                        out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                    }
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
}

fn json_number(line: &str, key: &str) -> Option<f64> {
    let at = line.find(key)? + key.len();
    let rest = line[at..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[allow(dead_code)]
fn unused(_: &PathBuf) {}
