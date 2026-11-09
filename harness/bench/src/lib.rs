//! Performance benchmarks with a regression gate. Spec 13.
//!
//! Spec 13 sets six budgets, measured "on a 2020-class laptop, Chrome,
//! single-threaded WASM", and asks for benchmarks in CI with a regression gate:
//! a 20% regression fails the build. The report calls the absence of these the
//! largest gap in the project's *verification*, and this closes it.
//!
//! # Native, and what that costs the comparison
//!
//! These run natively, not in WASM in a browser, so the numbers are not the ones
//! the spec's budgets are stated against — native is several times faster, and
//! the synthetic documents here are smaller than the 20 MB the spec names. So
//! the budget is printed for reference and **not** used as a pass/fail line: a
//! native run that beat the WASM budget would prove nothing, and one that missed
//! it might only mean the machine is busy.
//!
//! What *is* a pass/fail line is regression. A baseline is recorded once, on the
//! machine that will gate — a developer's, or CI's — and a later run that is more
//! than 20% slower than that baseline on any operation fails. That is the check
//! the spec actually specifies, and it is meaningful wherever it runs because it
//! compares a machine against itself.
//!
//! ```text
//! cargo run -p rasura-bench                      # measure, print, gate
//! cargo run -p rasura-bench -- --update-baseline # record this machine's baseline
//! ```

use rasura::{Document, SaveOptions};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

/// How much slower than the baseline an operation may run before it is a
/// regression. Spec 13: "A 20% regression fails the build."
pub const REGRESSION: f64 = 1.20;

/// One benchmarked operation's result.
pub struct Sample {
    pub name: &'static str,
    /// The document it ran against, for the printed table.
    pub document: &'static str,
    /// The median of the timed runs.
    pub median: Duration,
    /// Spec 13's budget for this operation — WASM, and for reference only.
    pub budget: Duration,
}

/// The median of `iters` runs of a region the closure times itself.
///
/// The closure returns the duration of the part that matters, so set-up — a
/// document opened, a page analysed — can happen outside the measured span. One
/// warm-up run is discarded first, and the median rather than the mean is taken
/// because a single scheduler hiccup should not move the number a gate reads.
pub fn measure_region(iters: usize, mut f: impl FnMut() -> Duration) -> Duration {
    let _ = f(); // warm-up, discarded
    let mut times: Vec<Duration> = (0..iters.max(1)).map(|_| f()).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

/// Run the whole suite.
pub fn run() -> Vec<Sample> {
    // Built once and cloned into each timed run. The clone is outside every
    // measured span, so copying the input bytes is not counted as open cost.
    let big = text_document(500, 15);
    let dense = text_document(1, 60);
    let small = text_document(1, 6);

    let open = measure_region(20, || {
        let bytes = big.clone();
        let t = Instant::now();
        let doc = Document::open(bytes).expect("open");
        let count = doc.page_count();
        let first = doc.page(0).expect("first page");
        let dt = t.elapsed();
        black_box((count, first.media_box()));
        dt
    });

    let paragraphs = measure_region(20, || {
        let bytes = dense.clone();
        let t = Instant::now();
        let doc = Document::open(bytes).expect("open");
        let page = doc.page(0).expect("page");
        let n = page.paragraphs().len();
        let dt = t.elapsed();
        black_box(n);
        dt
    });

    let replace = measure_region(50, || {
        // Set-up outside the timed span: the op is the edit, not the analysis.
        let mut doc = Document::open(small.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        let t = Instant::now();
        let outcome = session.replace_text(&page, id, 0..1, "X").expect("replace");
        let dt = t.elapsed();
        black_box(outcome.fidelity);
        dt
    });

    let save = measure_region(10, || {
        let mut doc = Document::open(big.clone()).expect("open");
        let page = doc.page(0).expect("page");
        let id = page.paragraphs()[0].id;
        let mut session = doc.edit();
        session.replace_text(&page, id, 0..1, "X").expect("replace");
        let t = Instant::now();
        let saved = session.commit(&SaveOptions::default()).expect("commit");
        let dt = t.elapsed();
        black_box(saved.bytes.len());
        dt
    });

    let extract = measure_region(5, || {
        let doc = Document::open(big.clone()).expect("open");
        let t = Instant::now();
        let mut total = 0usize;
        for i in 0..doc.page_count() {
            total += doc.page(i).expect("page").text().len();
        }
        let dt = t.elapsed();
        black_box(total);
        dt
    });

    vec![
        Sample {
            name: "open + first page metadata",
            document: "500 pages",
            median: open,
            budget: Duration::from_millis(120),
        },
        Sample {
            name: "page(0).paragraphs() cold",
            document: "dense text page",
            median: paragraphs,
            budget: Duration::from_millis(80),
        },
        Sample {
            name: "replaceText single paragraph",
            document: "one paragraph",
            median: replace,
            budget: Duration::from_millis(16),
        },
        Sample {
            name: "save() incremental, one edit",
            document: "500 pages",
            median: save,
            budget: Duration::from_millis(200),
        },
        Sample {
            name: "full text extraction",
            document: "500 pages",
            median: extract,
            budget: Duration::from_secs(4),
        },
    ]
}

/// Compare samples against a baseline, returning the operations that regressed.
///
/// An operation the baseline does not mention is not a regression — it is a new
/// benchmark, and the baseline simply has not caught up. Being faster than the
/// baseline is never a failure.
pub fn regressions<'a>(
    samples: &'a [Sample],
    baseline: &BTreeMap<String, u128>,
) -> Vec<Regression<'a>> {
    let mut out = Vec::new();
    for s in samples {
        if let Some(&was) = baseline.get(s.name) {
            let now = s.median.as_nanos();
            if now as f64 > was as f64 * REGRESSION {
                out.push(Regression { name: s.name, was_ns: was, now_ns: now });
            }
        }
    }
    out
}

/// One operation that ran more than [`REGRESSION`] slower than its baseline.
pub struct Regression<'a> {
    pub name: &'a str,
    pub was_ns: u128,
    pub now_ns: u128,
}

/// Parse a baseline file: one `name<TAB>nanoseconds` line per operation.
///
/// A line that does not parse is skipped rather than fatal: a baseline is a
/// convenience a gate reads, not a document whose every byte is load-bearing,
/// and a single malformed line should not stop the suite from running.
pub fn parse_baseline(text: &str) -> BTreeMap<String, u128> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, ns)) = line.rsplit_once('\t') {
            if let Ok(ns) = ns.trim().parse::<u128>() {
                out.insert(name.trim().to_string(), ns);
            }
        }
    }
    out
}

/// Render samples as a baseline file.
pub fn format_baseline(samples: &[Sample]) -> String {
    let mut out = String::from("# rasura-bench baseline: operation<TAB>median nanoseconds\n");
    out.push_str("# recorded on the machine that gates; regenerate with --update-baseline\n");
    for s in samples {
        out.push_str(&format!("{}\t{}\n", s.name, s.median.as_nanos()));
    }
    out
}

/// A synthetic text PDF: `pages` pages of `lines_per_page` lines, set in base-14
/// Helvetica with one shared font.
///
/// Built by hand rather than taken from the corpus so the benchmark depends on
/// nothing that might be absent and measures the same document every run. The
/// text is real words at descending baselines, which is what the paragraph
/// detector needs to find anything — a page of one repeated glyph would measure
/// a path no document takes.
pub fn text_document(pages: usize, lines_per_page: usize) -> Vec<u8> {
    let mut b = Pdf::new();

    // 1 catalog, 2 pages, 3 font; the page objects and their content follow.
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(
        3,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );

    let mut kids = String::new();
    for p in 0..pages {
        let page_obj = 4 + 2 * p;
        let content_obj = page_obj + 1;
        kids.push_str(&format!("{page_obj} 0 R "));

        let mut content = String::new();
        for l in 0..lines_per_page {
            let y = 740 - l as i32 * 14;
            let text = format!(
                "Page {p} line {l}: the quick brown fox jumps over the lazy dog, \
                 composition fidelity and analysis"
            );
            content.push_str(&format!("BT /F1 11 Tf 1 0 0 1 72 {y} Tm ({text}) Tj ET\n"));
        }

        b.object(
            page_obj,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents {content_obj} 0 R /Resources << /Font << /F1 3 0 R >> >> >>"
            ),
        );
        b.stream(content_obj, "", content.as_bytes());
    }

    b.object(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"));
    b.finish("/Root 1 0 R")
}

/// A minimal classic-xref PDF writer, enough for the synthetic benchmark inputs.
///
/// The same shape as the reader's own fixtures: numbered objects, offsets
/// tracked as the bytes are appended, and a cross-reference table that agrees
/// with them — which is the one thing that, wrong, makes every measurement below
/// a measurement of a broken file.
struct Pdf {
    bytes: Vec<u8>,
    offsets: BTreeMap<usize, usize>,
}

impl Pdf {
    fn new() -> Pdf {
        let mut pdf = Pdf { bytes: Vec::new(), offsets: BTreeMap::new() };
        pdf.push(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        pdf
    }

    fn push(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
    }

    fn object(&mut self, n: usize, body: &str) {
        self.offsets.insert(n, self.bytes.len());
        self.push(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    fn stream(&mut self, n: usize, dict: &str, data: &[u8]) {
        self.offsets.insert(n, self.bytes.len());
        self.push(format!("{n} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes());
        self.push(data);
        self.push(b"\nendstream\nendobj\n");
    }

    fn finish(mut self, trailer: &str) -> Vec<u8> {
        let count = self.offsets.keys().max().copied().unwrap_or(0) + 1;
        let start = self.bytes.len();
        let mut table = format!("xref\n0 {count}\n0000000000 65535 f \n");
        for n in 1..count {
            let at = self.offsets.get(&n).copied().unwrap_or(0);
            table.push_str(&format!("{at:010} 00000 n \n"));
        }
        self.push(table.as_bytes());
        self.push(format!("trailer\n<< /Size {count} {trailer} >>\nstartxref\n{start}\n%%EOF\n").as_bytes());
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_document_opens_with_the_pages_and_text_it_claims() {
        let bytes = text_document(3, 5);
        let doc = Document::open(bytes).expect("the builder writes a readable PDF");
        assert_eq!(doc.page_count(), 3);
        let page = doc.page(0).expect("page");
        assert!(!page.paragraphs().is_empty(), "the detector finds text on the page");
        assert!(page.text().contains("quick brown fox"), "{:?}", page.text());
    }

    #[test]
    fn a_baseline_round_trips_through_its_text_form() {
        let samples = vec![Sample {
            name: "open + first page metadata",
            document: "500 pages",
            median: Duration::from_nanos(12_345),
            budget: Duration::from_millis(120),
        }];
        let parsed = parse_baseline(&format_baseline(&samples));
        assert_eq!(parsed.get("open + first page metadata"), Some(&12_345));
    }

    #[test]
    fn a_run_within_the_margin_is_not_a_regression_and_past_it_is() {
        let samples = vec![Sample {
            name: "op",
            document: "d",
            median: Duration::from_nanos(1_000),
            budget: Duration::from_millis(1),
        }];
        // 19% slower: inside the 20% margin.
        let ok: BTreeMap<String, u128> = [("op".to_string(), 840u128)].into_iter().collect();
        assert!(regressions(&samples, &ok).is_empty());
        // 25% slower: over it.
        let bad: BTreeMap<String, u128> = [("op".to_string(), 800u128)].into_iter().collect();
        assert_eq!(regressions(&samples, &bad).len(), 1);
        // An operation the baseline does not know is not a regression.
        assert!(regressions(&samples, &BTreeMap::new()).is_empty());
    }
}
