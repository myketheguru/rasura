//! Export a PDF to Markdown, or measure the conversion over a corpus.
//!
//!   cargo run -p rasura-flow --example export -- input.pdf
//!   cargo run -p rasura-flow --example export -- --survey corpus/files
//!
//! The single-file mode exists because `docs/flow-model.md` says export is
//! step 1 for a reason that is about judgement rather than features: "an export
//! is read by a human who will notice a scrambled paragraph immediately". This
//! is how a human reads one.
//!
//! The survey mode is the other half. A heuristic that fires on 3% of documents
//! and one that fires on 97% need different treatment, and neither is knowable
//! from a single file.

use rasura_flow::{Guess, markdown};
use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--survey") => {
            survey(Path::new(args.get(1).map(String::as_str).unwrap_or("corpus/files")))
        }
        Some("--i8") => i8(Path::new(args.get(1).map(String::as_str).unwrap_or("corpus/files"))),
        Some("--frames") => {
            frames(Path::new(args.get(1).map(String::as_str).unwrap_or("corpus/files")))
        }
        Some(path) => single(Path::new(path), args.iter().any(|a| a == "--running")),
        None => {
            eprintln!(
                "usage: export <file.pdf> [--running] | export --survey <dir> \
                 | export --frames <dir> | export --i8 <dir>"
            );
            std::process::exit(2);
        }
    }
}

fn single(path: &Path, include_running: bool) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let doc = match rasura_cos::Document::open(bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }
    };

    let (flow, report) = match rasura_flow::to_flow(&doc) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }
    };

    // The report goes to stderr so the Markdown on stdout can be redirected
    // into a file without the notes ending up in it.
    eprintln!(
        "{}: {} page(s), order from {}",
        path.display(),
        flow.meta.pages,
        flow.meta.order.as_str()
    );
    eprintln!("  {} block(s) in, {} out", report.blocks_in, report.blocks_out);
    for line in report.lines() {
        eprintln!("  note: {line}");
    }
    if report.is_exact() {
        eprintln!("  nothing was guessed");
    }

    let opts = markdown::Options { include_running, ..markdown::Options::default() };
    print!("{}", markdown::render(&flow, &opts));
}

#[derive(Default)]
struct Totals {
    files: usize,
    failed: usize,
    tagged: usize,
    structure_order: usize,
    exact: usize,
    blocks_in: usize,
    blocks_out: usize,
    empty_output: Vec<String>,
    empty_causes: BTreeMap<&'static str, usize>,
    unaccounted: Vec<String>,
    guesses: BTreeMap<&'static str, usize>,
    guess_files: BTreeMap<&'static str, usize>,
    rules_dropped: usize,
}

fn survey(dir: &Path) {
    let mut totals = Totals::default();

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(read) => read.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    entries.sort();

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        totals.files += 1;

        let Ok(bytes) = std::fs::read(&path) else {
            totals.failed += 1;
            continue;
        };
        let Ok(doc) = rasura_cos::Document::open(bytes) else {
            // Encrypted or malformed beyond recovery. Counted, not reported:
            // whether a file opens is the object layer's business and is
            // measured by the invariant harness.
            totals.failed += 1;
            continue;
        };
        let Ok((flow, report)) = rasura_flow::to_flow(&doc) else {
            totals.failed += 1;
            continue;
        };

        if flow.meta.tagged {
            totals.tagged += 1;
        }
        if flow.meta.order == rasura_flow::Provenance::Structure {
            totals.structure_order += 1;
        }
        if report.is_exact() {
            totals.exact += 1;
        }
        totals.blocks_in += report.blocks_in;
        totals.blocks_out += report.blocks_out;
        totals.rules_dropped += report.rules_dropped;

        // The accounting invariant, over real documents rather than fixtures.
        // A block that leaves the model and appears in neither the output nor
        // the report has vanished, and the output still looks like a document.
        let gathered: usize = flow
            .blocks
            .iter()
            .map(|b| match b {
                rasura_flow::Block::List(l) => l.items.len().saturating_sub(1),
                _ => 0,
            })
            .sum();
        if !report.accounts_for_everything(gathered) {
            totals.unaccounted.push(name(&path));
        }

        // A document with blocks in and nothing out is the other failure this
        // survey is for. Attributed to a cause, because "produced nothing" is
        // a symptom and the causes want different fixes.
        if report.blocks_in > 0 && flow.blocks.is_empty() {
            let cause = if report.empty_paragraphs_dropped >= report.blocks_in {
                "every paragraph resolved to no text"
            } else if report.running_lifted >= report.blocks_in {
                "the whole document was running furniture"
            } else if report.rules_dropped >= report.blocks_in {
                "the page is vector art with nothing but rules on it"
            } else if report.empty_opaque_dropped >= report.blocks_in {
                "unclassified blocks with no recoverable text"
            } else {
                "mixed"
            };
            *totals.empty_causes.entry(cause).or_default() += 1;
            totals.empty_output.push(name(&path));
        }

        for guess in ALL_GUESSES {
            let n = report.made(*guess);
            if n > 0 {
                *totals.guesses.entry(guess.as_str()).or_default() += n;
                *totals.guess_files.entry(guess.as_str()).or_default() += 1;
            }
        }
    }

    let converted = totals.files - totals.failed;
    println!("{} file(s): {converted} converted, {} did not open", totals.files, totals.failed);
    if converted == 0 {
        return;
    }
    println!(
        "  tagged                {:>5} ({:.0}%)",
        totals.tagged,
        pct(totals.tagged, converted)
    );
    println!(
        "  order from structure  {:>5} ({:.0}%)",
        totals.structure_order,
        pct(totals.structure_order, converted)
    );
    println!(
        "  converted with no guesses {:>5} ({:.0}%)",
        totals.exact,
        pct(totals.exact, converted)
    );
    println!("  blocks {} in, {} out", totals.blocks_in, totals.blocks_out);

    println!("\n  guess                                        files   occurrences");
    for guess in ALL_GUESSES {
        let files = totals.guess_files.get(guess.as_str()).copied().unwrap_or(0);
        let n = totals.guesses.get(guess.as_str()).copied().unwrap_or(0);
        println!("  {:<42} {files:>5} {n:>13}", guess.as_str());
    }
    if totals.rules_dropped > 0 {
        println!("\n  {} rule(s) omitted as decoration across the corpus", totals.rules_dropped);
    }

    if !totals.empty_output.is_empty() {
        println!("\n  {} file(s) produced no output at all, by cause:", totals.empty_output.len());
        for (cause, n) in &totals.empty_causes {
            println!("    {n:>4}  {cause}");
        }
        println!(
            "    e.g. {}",
            totals.empty_output.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
        );
    }

    // The one result that is a defect rather than a measurement.
    if totals.unaccounted.is_empty() {
        println!("\n  every block was exported or accounted for, in all {converted} file(s)");
    } else {
        println!("\n  UNACCOUNTED: {} file(s) lost a block silently:", totals.unaccounted.len());
        for file in totals.unaccounted.iter().take(10) {
            println!("    {file}");
        }
    }
}

/// I8, the model round trip, across a corpus. `docs/flow-model.md` step 4.
///
/// The loop I8 will eventually close — model, lay out, re-extract — needs a
/// layout engine that does not exist yet. These are the round trips that do:
/// analysis against itself, and analysis across a save. Both use the same
/// comparison the layout engine will, which is the reason for building it now.
fn i8(dir: &Path) {
    use rasura_flow::compare::{Options, compare, summarise};

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(read) => read.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    entries.sort();

    let (mut files, mut skipped) = (0usize, 0usize);
    let (mut stable, mut unstable) = (0usize, 0usize);
    let (mut saved_ok, mut saved_differs) = (0usize, 0usize);
    let (mut laid_out_ok, mut laid_out_differs) = (0usize, 0usize);
    let (mut emitted_ok, mut emitted_differs, mut emitted_unreadable) = (0usize, 0usize, 0usize);
    let (mut emitted_differs_encoding, mut emitted_differs_other) = (0usize, 0usize);
    let mut unencodable = 0usize;
    let mut page_ratios: Vec<f64> = Vec::new();
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut worst: Vec<(String, String)> = Vec::new();
    let mut drifted: Vec<(String, f64)> = Vec::new();

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(doc) = rasura_cos::Document::open(bytes.clone()) else {
            skipped += 1;
            continue;
        };
        let Ok((first, _)) = rasura_flow::to_flow(&doc) else {
            skipped += 1;
            continue;
        };
        if first.blocks.is_empty() {
            skipped += 1;
            continue;
        }
        files += 1;

        // 1. Analysis against itself. A reconstruction that is not stable
        //    against the same bytes cannot be stable against anything.
        let Ok(again_doc) = rasura_cos::Document::open(bytes.clone()) else { continue };
        let Ok((again, _)) = rasura_flow::to_flow(&again_doc) else { continue };
        let self_diff = compare(&first, &again, &Options::default());
        if self_diff.is_empty() {
            stable += 1;
        } else {
            unstable += 1;
            worst.push((format!("{} (determinism)", name(&path)), summarise(&self_diff)));
            for d in &self_diff {
                *by_kind.entry(d.kind()).or_default() += 1;
            }
        }

        // 2. Through the layout engine. The loop `docs/flow-model.md` describes,
        //    closed: lay the model out into the document's own frames, read the
        //    model back out of the placement, and compare.
        let model = rasura_layout::model::analyse(&doc).ok();
        if let Some(model) = &model {
            let (set, _) = rasura_layout::frames::infer(model, &Default::default());
            let (placed, diff) = rasura_flow::layout::round_trip(
                &first,
                &set,
                &rasura_flow::layout::Options::default(),
                &rasura_flow::Standard14,
            );
            if diff.is_empty() {
                laid_out_ok += 1;
            } else {
                laid_out_differs += 1;
                worst.push((format!("{} (layout)", name(&path)), summarise(&diff)));
                for d in &diff {
                    *by_kind.entry(d.kind()).or_default() += 1;
                }
            }
            // Pagination is *expected* to move: the engine re-breaks every line
            // to its own metrics rather than the document's own font. Recorded
            // as drift rather than as a failure.
            if placed.pages > 0 && first.meta.pages > 0 {
                let ratio = placed.pages as f64 / first.meta.pages as f64;
                page_ratios.push(ratio);
            }
        }

        // 3. Through document mode: laid out, written as a PDF, re-opened and
        //    re-extracted. The loop with a real file in the middle, so no part
        //    of the pipeline is trusted to report on itself.
        //
        //    Compared on reading rather than on block structure: through a real
        //    PDF a paragraph split by pagination is two paragraphs to a reader,
        //    and the format has no mark that says otherwise.
        if let Ok(mut fresh) = rasura_cos::Document::open(bytes.clone()) {
            let emitted = rasura_flow::emit::regenerate_document(
                &mut fresh,
                &first,
                &rasura_flow::layout::Options::default(),
                &rasura_flow::emit::Options {
                    accept_regeneration: true,
                    ..rasura_flow::emit::Options::default()
                },
            );
            if let Ok(emit_report) = emitted {
                unencodable += emit_report.unencodable;
                match rasura_cos::save(&fresh, &rasura_cos::SaveOptions::default())
                    .ok()
                    .and_then(|s| rasura_cos::Document::open(s.bytes).ok())
                    .and_then(|d| rasura_flow::to_flow(&d).ok())
                {
                    Some((round, _)) => {
                        let diff = rasura_flow::compare::compare_reading(&first, &round);
                        if diff.is_empty() {
                            emitted_ok += 1;
                        } else {
                            emitted_differs += 1;
                            // Attributed, not just counted. A document whose
                            // text left WinAnsi behind was always going to
                            // differ, and lumping it with a real defect would
                            // hide the defect.
                            if emit_report.unencodable > 0 {
                                emitted_differs_encoding += 1;
                            } else {
                                emitted_differs_other += 1;
                                worst.push((format!("{} (emit)", name(&path)), summarise(&diff)));
                            }
                        }
                    }
                    None => emitted_unreadable += 1,
                }
            }
        }

        // 4. Across a save. The writer is meant to copy what it did not touch;
        //    this says so at the level a reader would notice.
        let Ok(written) = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()) else {
            continue;
        };
        let Ok(reopened) = rasura_cos::Document::open(written.bytes) else { continue };
        let Ok((after, _)) = rasura_flow::to_flow(&reopened) else { continue };

        let diff = compare(&first, &after, &Options::default());
        let drift = rasura_flow::Drift::measure(&first, &after);
        if diff.is_empty() {
            saved_ok += 1;
        } else {
            saved_differs += 1;
            worst.push((name(&path), summarise(&diff)));
            for d in &diff {
                *by_kind.entry(d.kind()).or_default() += 1;
            }
        }
        if drift.char_drift().abs() > 0.001 {
            drifted.push((name(&path), drift.char_drift()));
        }
    }

    println!("{files} file(s) with content to compare, {skipped} skipped");
    println!(
        "  analysis is deterministic   {stable} stable, {unstable} not ({:.1}%)",
        pct(stable, files)
    );
    println!(
        "  model survives a save       {saved_ok} identical, {saved_differs} differ ({:.1}%)",
        pct(saved_ok, files)
    );
    println!(
        "  model survives layout       {laid_out_ok} identical, {laid_out_differs} differ ({:.1}%)",
        pct(laid_out_ok, laid_out_ok + laid_out_differs)
    );
    if !page_ratios.is_empty() {
        page_ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "  pagination after layout     median {:.2}x the original page count",
            page_ratios[page_ratios.len() / 2]
        );
    }

    println!(
        "  survives document mode      {emitted_ok} identical, {emitted_differs} differ, {emitted_unreadable} unreadable ({:.1}%)",
        pct(emitted_ok, emitted_ok + emitted_differs + emitted_unreadable)
    );
    if emitted_differs > 0 {
        println!(
            "    of which {emitted_differs_encoding} lost characters WinAnsi cannot hold, \
             {emitted_differs_other} for another reason"
        );
    }
    if unencodable > 0 {
        println!("  {unencodable} character(s) lost to WinAnsi when written");
    }

    if !by_kind.is_empty() {
        println!("\n  differences by kind:");
        for (kind, count) in &by_kind {
            println!("    {kind:<16} {count:>5}");
        }
    }
    if !drifted.is_empty() {
        println!("\n  {} file(s) drifted in character count:", drifted.len());
        for (file, d) in drifted.iter().take(8) {
            println!("    {:+.1}%  {file}", d * 100.0);
        }
    }
    if worst.is_empty() {
        println!("\n  I8 holds on every file compared");
    } else {
        println!("\n  {} failing round trip(s):", worst.len());
        for (file, why) in worst.iter().take(12) {
            println!("    {file}: {why}");
        }
    }
}

/// Measure frame inference across a corpus. `docs/flow-model.md` step 3.
///
/// The two numbers that matter are containment and tightness, and they pull in
/// opposite directions on purpose: a single page-sized frame scores perfect
/// containment and useless tightness, and a frame drawn tightly round one
/// paragraph scores the reverse. A method is working when both are good at once.
fn frames(dir: &Path) {
    use rasura_layout::frames::{Evidence, Options};

    // One file prints its frames and what missed them. The aggregate says a
    // method is wrong; only this says why.
    if dir.is_file() {
        let Ok(bytes) = std::fs::read(dir) else {
            eprintln!("{}: unreadable", dir.display());
            std::process::exit(1);
        };
        let doc = rasura_cos::Document::open(bytes).expect("open");
        let model = rasura_layout::model::analyse(&doc).expect("analyse");
        let (set, report) = rasura_layout::frames::infer(&model, &Options::default());

        for group in &set.groups {
            println!("pages {:?} at {:?}", group.pages, group.size);
            for frame in &group.frames {
                println!(
                    "  column {} {:?} {} block(s), {}",
                    frame.column,
                    frame.rect,
                    frame.blocks,
                    frame.evidence.as_str()
                );
            }
        }
        println!("\n{report:#?}");

        for (index, page) in model.pages.iter().enumerate() {
            for block in &page.blocks {
                let b = block.bbox();
                if matches!(
                    block,
                    rasura_layout::model::Block::Running(_)
                        | rasura_layout::model::Block::Image(_)
                        | rasura_layout::model::Block::Vector(_)
                ) {
                    continue;
                }
                let fits = set.frame_for(index, b).is_some_and(|f| {
                    b.x0 >= f.rect.x0 - 1.0
                        && b.x1 <= f.rect.x1 + 1.0
                        && b.y0 >= f.rect.y0 - 1.0
                        && b.y1 <= f.rect.y1 + 1.0
                });
                if !fits {
                    println!("  loose: page {index} {} {:?}", block.kind(), b);
                }
            }
        }
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(read) => read.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    entries.sort();

    let (mut files, mut failed, mut multi_column, mut fallbacks) = (0, 0, 0usize, 0usize);
    let mut columns: BTreeMap<usize, usize> = BTreeMap::new();
    let mut containment_sum = 0.0;
    let mut strict_sum = 0.0;
    let mut spanning = 0usize;
    let mut tightness_sum = 0.0;
    // Collected rather than only summed: one document with a page-sized frame
    // around three glyphs pulls a mean of ratios anywhere it likes, and the
    // first run of this survey reported a mean tightness of 142 for exactly
    // that reason. The median says what a typical document looks like.
    let mut tightness_all: Vec<f64> = Vec::new();
    let mut outside_box = 0usize;
    let mut measured = 0usize;
    let mut single_page = 0usize;
    let mut worst: Vec<(String, f64)> = Vec::new();

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        files += 1;

        let Ok(bytes) = std::fs::read(&path) else {
            failed += 1;
            continue;
        };
        let Ok(doc) = rasura_cos::Document::open(bytes) else {
            failed += 1;
            continue;
        };
        let Ok(model) = rasura_layout::model::analyse(&doc) else {
            failed += 1;
            continue;
        };

        let (set, report) = rasura_layout::frames::infer(&model, &Options::default());
        if report.blocks_considered == 0 {
            continue;
        }
        measured += 1;
        containment_sum += report.containment();
        strict_sum += report.strict_containment();
        spanning += report.blocks_spanning;
        tightness_sum += report.tightness;
        tightness_all.push(report.tightness);
        outside_box += report.blocks_outside_page_box;
        fallbacks += report.fallbacks;

        let widest = set.groups.iter().map(|g| g.frames.len()).max().unwrap_or(0);
        *columns.entry(widest).or_default() += 1;
        if widest > 1 {
            multi_column += 1;
        }
        if set
            .groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .any(|f| f.evidence == Evidence::SinglePage)
        {
            single_page += 1;
        }
        if report.containment() < 0.9 {
            worst.push((name(&path), report.containment()));
        }
    }

    println!("{files} file(s): {measured} with text to measure, {failed} did not open");
    if measured == 0 {
        return;
    }
    println!(
        "  mean containment {:.1}% ({:.1}% counting a spanning block as a miss)",
        containment_sum / measured as f64 * 100.0,
        strict_sum / measured as f64 * 100.0
    );
    println!("  {spanning} block(s) legitimately span more than one frame");
    tightness_all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "  tightness        median {:.2}, mean {:.2}",
        tightness_all[tightness_all.len() / 2],
        tightness_sum / measured as f64
    );
    if outside_box > 0 {
        println!("  {outside_box} block(s) lie outside their page box");
    }
    println!(
        "  single-page evidence in {single_page} file(s) ({:.0}%)",
        pct(single_page, measured)
    );
    println!("  multi-column     {multi_column} file(s) ({:.0}%)", pct(multi_column, measured));
    if fallbacks > 0 {
        println!("  {fallbacks} page group(s) fell back to the page box");
    }

    println!("\n  frames per page group:");
    for (n, count) in &columns {
        println!("    {n:>2} frame(s)  {count:>4} file(s)");
    }

    // Named rather than summarised. A mean of 97% says nothing about the
    // documents where the method fails, and those are the ones worth reading.
    worst.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    if worst.is_empty() {
        println!("\n  every file kept at least 90% of its blocks inside a frame");
    } else {
        println!("\n  {} file(s) below 90% containment:", worst.len());
        for (file, score) in worst.iter().take(12) {
            println!("    {:.0}%  {file}", score * 100.0);
        }
    }
}

const ALL_GUESSES: &[Guess] = &[
    Guess::ReadingOrderInferred,
    Guess::HeadingInferred,
    Guess::ListInferred,
    Guess::ListLabelDropped,
    Guess::EmphasisFromFontName,
    Guess::TableCellStyleDropped,
    Guess::HyphenationJoined,
];

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 { 0.0 } else { n as f64 * 100.0 / of as f64 }
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
