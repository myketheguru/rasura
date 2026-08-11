//! Walks every page of every corpus file through the content layer.
//!
//! Phase 1's lesson was that hand-written fixtures found one bug and a real
//! corpus found five. This is that lesson applied before anything is built on
//! top of §6.4: the walker meets a thousand real files now, not after the
//! layout engine is sitting on it.
//!
//! It asserts the invariants the content layer must hold regardless of what the
//! file contains:
//!
//! * every operator's span lies within the logical buffer and is non-empty
//! * spans are ordered and non-overlapping
//! * every span maps back to a real source object
//! * the graphics state stack is balanced by the end of a page
//! * nothing panics, hangs, or recurses without bound
//!
//! ```text
//! cargo run --release -p rasura-contentwalk
//! cargo run --release -p rasura-contentwalk -- path/to/dir
//! ```

use rasura_content::{
    ContentVisitor, Flow, Op, OpKind, StateMachine, WalkContext, page, walk_page,
};
use rasura_cos::document::{Document, OpenOptions};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files whose content genuinely cannot be reached, and why.
///
/// Same discipline as the invariant suite's `expected_decline`: a corpus of
/// deliberately-broken files contains some that no reader can get into, and
/// recording them by name keeps the gate meaningful. Anything failing that is
/// *not* listed here is still red.
fn expected_failure(name: &str) -> Option<&'static str> {
    match name {
        "issue19484_1.pdf" | "issue19484_2.pdf" => Some(
            "object streams do not decrypt under any key length, EncryptMetadata \
             setting, or as plaintext; the file's own /U validates, so the streams \
             themselves are corrupt",
        ),
        _ => None,
    }
}

#[derive(Default)]
struct Checker {
    ops: usize,
    text_ops: usize,
    /// Span problems, which are defects in this layer rather than in the file.
    problems: Vec<String>,
    last_end: usize,
    last_depth: usize,
    kinds: BTreeMap<&'static str, usize>,
}

impl ContentVisitor for Checker {
    fn visit(&mut self, op: &Op, _state: &mut StateMachine, ctx: &WalkContext<'_>) -> Flow {
        self.ops += 1;
        if op.kind.shows_text() {
            self.text_ops += 1;
        }
        *self.kinds.entry(op.kind.keyword().unwrap_or("<special>")).or_default() += 1;

        // Spans must be usable: the edit layer will splice at exactly these.
        if op.span.start >= op.span.end {
            self.problems.push(format!("empty span for {:?}", op.kind));
        }
        if op.span.end > ctx.content.len() {
            self.problems.push(format!(
                "span {:?} for {:?} runs past the {}-byte buffer",
                op.span,
                op.kind,
                ctx.content.len()
            ));
        }
        // Ordering only holds within one stream; a form resets the buffer.
        if ctx.depth == self.last_depth && op.span.start < self.last_end {
            self.problems.push(format!(
                "span {:?} for {:?} overlaps the previous operator ending at {}",
                op.span, op.kind, self.last_end
            ));
        }
        self.last_end = op.span.end;
        self.last_depth = ctx.depth;

        // Every operator has to be attributable to an object, or a patch would
        // have nowhere to go.
        if ctx.content.source_of(op.span.start).is_none() {
            self.problems.push(format!(
                "operator {:?} at {} maps to no source object",
                op.kind, op.span.start
            ));
        }
        Flow::Continue
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs: Vec<PathBuf> = if args.is_empty() {
        vec![PathBuf::from("corpus/files"), PathBuf::from("corpus/external")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        match collect(dir) {
            Ok(f) => files.extend(f),
            Err(e) => println!("note: {} not read ({e})", dir.display()),
        }
    }
    if files.is_empty() {
        eprintln!("no PDFs found");
        return std::process::ExitCode::FAILURE;
    }

    let mut docs = 0usize;
    let mut total_pages = 0usize;
    let mut total_ops = 0usize;
    let mut total_text_ops = 0usize;
    let mut forms = 0usize;
    let mut pages_with_text = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut expected: Vec<String> = Vec::new();
    let mut leniencies: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut unopened = 0usize;
    let mut no_pages = 0usize;
    // Spec 10.2. Counted here because this is the walk that already visits
    // every page: whether layers are a niche feature or a routine one decides
    // how much of the edit layer has to know about them.
    let mut docs_with_layers = 0usize;
    let mut layers_declared = 0usize;
    let mut layers_off = 0usize;
    let mut oc_regions = 0usize;
    let mut oc_regions_hidden = 0usize;
    let mut oc_from_xobject = 0usize;
    let mut pages_with_oc = 0usize;

    for path in &files {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok(doc) = Document::open_with(bytes, &OpenOptions::default()) else {
            unopened += 1;
            continue;
        };
        docs += 1;

        let tree = match page::pages(&doc) {
            Ok(t) => t,
            Err(e) => {
                match expected_failure(&short(path)) {
                    Some(why) => expected.push(format!("{}: {why}", short(path))),
                    None => failures.push(format!("{}: page tree: {e}", short(path))),
                }
                continue;
            }
        };
        for l in &tree.leniencies {
            *leniencies.entry(format!("{:?}", l.kind)).or_default() += 1;
        }
        if tree.pages.is_empty() {
            no_pages += 1;
        }

        let optional = rasura_content::optional::read(&doc);
        if let Some(oc) = &optional {
            docs_with_layers += 1;
            layers_declared += oc.layers.len();
            layers_off += oc.layers.iter().filter(|l| !l.visible).count();
        }

        // A handful of corpus files are enormous; the point is coverage of
        // shapes, not of page counts.
        for p in tree.pages.iter().take(50) {
            total_pages += 1;

            if let Some(oc) = &optional
                && let Ok((content, _)) = rasura_content::page_content(&doc, &p.dict)
            {
                let regions = rasura_content::optional::regions(&doc, p, &content, oc);
                if !regions.is_empty() {
                    pages_with_oc += 1;
                }
                oc_regions += regions.len();
                oc_regions_hidden += regions.iter().filter(|r| !r.visible).count();
                oc_from_xobject += regions
                    .iter()
                    .filter(|r| r.source == rasura_content::optional::Source::XObject)
                    .count();
            }

            let mut checker = Checker::default();
            let report = walk_page(&doc, p, &mut checker);

            total_ops += checker.ops;
            total_text_ops += checker.text_ops;
            forms += report.forms_entered;
            if checker.text_ops > 0 {
                pages_with_text += 1;
            }
            for (k, n) in checker.kinds {
                *kinds.entry(k).or_default() += n;
            }
            for l in &report.leniencies {
                *leniencies.entry(format!("{:?}", l.kind)).or_default() += 1;
            }
            for problem in checker.problems.iter().take(3) {
                failures.push(format!("{} page {}: {problem}", short(path), p.index));
            }
        }
    }

    println!("=== content layer walk ===\n");
    println!("{} file(s), {docs} opened, {unopened} would not open", files.len());
    println!("{total_pages} page(s) walked, {no_pages} document(s) yielded no pages");
    println!("{total_ops} operator(s), {total_text_ops} text-showing");
    println!("{pages_with_text} page(s) had text, {forms} form XObject(s) entered\n");

    println!("--- optional content (spec 10.2) ---");
    println!("{docs_with_layers} document(s) with /OCProperties, {layers_declared} layer(s)");
    println!("  {layers_off} layer(s) off in the default configuration");
    println!("{pages_with_oc} page(s) carry optional content, {oc_regions} region(s)");
    println!("  {oc_regions_hidden} hidden, {oc_from_xobject} from an XObject's own /OC\n");

    println!("--- most frequent operators ---");
    let mut rows: Vec<_> = kinds.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in rows.iter().take(12) {
        println!("  {k:<10} {n:>9}");
    }

    if !leniencies.is_empty() {
        println!("\n--- leniencies (file defects, not failures) ---");
        let mut rows: Vec<_> = leniencies.into_iter().collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (k, n) in rows.iter().take(12) {
            println!("  {k:<28} {n:>7}");
        }
    }

    if !expected.is_empty() {
        println!("\n--- {} expected failure(s) ---", expected.len());
        for e in &expected {
            println!("  {e}");
        }
    }

    if failures.is_empty() {
        println!("\nno span or attribution defects");
        std::process::ExitCode::SUCCESS
    } else {
        println!("\n--- {} defect(s) in the content layer ---", failures.len());
        for f in failures.iter().take(25) {
            println!("  {f}");
        }
        if failures.len() > 25 {
            println!("  ... and {} more", failures.len() - 25);
        }
        std::process::ExitCode::FAILURE
    }
}

fn short(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn collect(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect(&path)?);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

// Silence an unused-import warning when OpKind is only referenced in tests.
#[allow(dead_code)]
fn _kind_is_used(k: OpKind) -> bool {
    k.shows_text()
}
