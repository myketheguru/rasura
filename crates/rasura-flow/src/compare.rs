//! I8, the model round trip. Step 4 of `docs/flow-model.md`.
//!
//! > **I8 — Model stability.** Build the model, lay it out, extract the model
//! > from the result, and the two models agree.
//!
//! Document mode needs an invariant of its own because none of I1–I5 apply:
//! every one of them assumes the bytes are meant to survive, and a re-laid-out
//! page's bytes are meant to be replaced. What has to survive instead is the
//! *content* — every paragraph, in order, with its structure intact.
//!
//! # Why this compares flow documents and not placed ones
//!
//! The obvious comparison is between two [`rasura_layout::model::DocumentModel`]
//! values, and it cannot work. A placed model is coordinates: laying a document
//! out again moves every block by design, so a diff of placed models reports
//! every block as changed on a round trip that lost nothing at all.
//!
//! A [`FlowDocument`] has no coordinates — that is the whole point of it — so
//! two of them can be compared for the thing I8 is actually about. This is the
//! reason `docs/flow-model.md` puts the flow model at step 1 and I8 at step 4,
//! and the reason I8 comes *before* the layout engine rather than after: the
//! engine is not tractable to develop without it.
//!
//! # What can be round-tripped today
//!
//! There is no layout engine yet, so the loop I8 will eventually close —
//! model, lay out, re-extract — cannot run. Three round trips that *do* exist
//! exercise the same comparison and are worth having on their own:
//!
//! 1. **Analysis is deterministic.** The same bytes twice give the same flow
//!    document. A reconstruction that is not stable against itself cannot be
//!    stable against anything else.
//! 2. **A save preserves the model.** Write the document out unchanged, read it
//!    back, and the flow document is identical. This catches the writer
//!    perturbing content it was only meant to copy.
//! 3. **A surgical edit is local.** After replacing text in one paragraph,
//!    every *other* block is unchanged. §2's first property says an edit must
//!    not change any object it did not need to touch; this says the same thing
//!    one level up, where a caller can see it.
//!
//! When the layout engine lands it becomes a fourth caller of the same
//! function, which is the point of building this first.

use crate::flow::{Block, FlowDocument, Inline};

/// One way in which two flow documents differ.
///
/// Ordered roughly by severity: a lost block is worse than a changed emphasis,
/// and a caller triaging a corpus run wants the worst thing first.
#[derive(Debug, Clone, PartialEq)]
pub enum Difference {
    /// The documents have different numbers of blocks. Reported once, with the
    /// counts, rather than as a difference per block.
    BlockCount { before: usize, after: usize },
    /// A block changed kind — a heading became a paragraph, a list became prose.
    Kind { at: usize, before: &'static str, after: &'static str },
    /// The text of a block changed.
    Text { at: usize, before: String, after: String },
    /// A heading changed level. Not a text loss, and still a structural one:
    /// an `H2` that becomes an `H3` moves a whole section in a table of
    /// contents.
    HeadingLevel { at: usize, before: u8, after: u8 },
    /// Bold or italic was gained or lost.
    Emphasis { at: usize, text: String },
    /// A list gained or lost items.
    ListLength { at: usize, before: usize, after: usize },
    /// A table changed shape. The failure `docs/flow-model.md` names as one I8
    /// must catch: "a table flattened".
    TableShape { at: usize, before: (usize, usize), after: (usize, usize) },
    /// The same blocks came back in a different order. The other failure named:
    /// "reading order permuted".
    OrderChanged { moved: usize },
    /// A running header or footer was lost or gained.
    RunningChanged { before: usize, after: usize },
    /// Annotation text was lost or gained.
    NotesChanged { before: usize, after: usize },
    /// Pages went in and a different number came out. The weaker check the
    /// design asks for alongside I8: "if a re-laid-out document has 12% more
    /// pages than the original, something is wrong even when the model
    /// round-trips."
    PageCount { before: usize, after: usize },
}

impl Difference {
    /// Whether this difference means content was lost or moved, as opposed to
    /// presented differently.
    ///
    /// The distinction a corpus sweep needs: an emphasis that changed is worth
    /// a line in a report, and a dropped paragraph is worth failing over.
    pub fn is_content_loss(&self) -> bool {
        matches!(
            self,
            Difference::BlockCount { .. }
                | Difference::Text { .. }
                | Difference::ListLength { .. }
                | Difference::TableShape { .. }
                | Difference::OrderChanged { .. }
                | Difference::NotesChanged { .. }
        )
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Difference::BlockCount { .. } => "block count",
            Difference::Kind { .. } => "block kind",
            Difference::Text { .. } => "text",
            Difference::HeadingLevel { .. } => "heading level",
            Difference::Emphasis { .. } => "emphasis",
            Difference::ListLength { .. } => "list length",
            Difference::TableShape { .. } => "table shape",
            Difference::OrderChanged { .. } => "reading order",
            Difference::RunningChanged { .. } => "running heads",
            Difference::NotesChanged { .. } => "annotation text",
            Difference::PageCount { .. } => "page count",
        }
    }
}

/// What to hold the round trip to.
#[derive(Debug, Clone)]
pub struct Options {
    /// Compare bold and italic.
    ///
    /// On for a round trip through a save, which must preserve everything. A
    /// caller comparing an export against its source may want it off, since
    /// Markdown cannot represent every distinction the model can.
    pub compare_emphasis: bool,
    /// Compare the page count.
    ///
    /// Off when the round trip deliberately re-paginates, which a layout engine
    /// does: the check then belongs to the drift metric rather than to I8.
    pub compare_pages: bool,
    /// Collapse runs of whitespace before comparing text.
    ///
    /// On. Text is reconstructed from glyph positions, and a difference of one
    /// space between two runs of the same paragraph is a difference in
    /// segmentation rather than in content — worth chasing, and not worth
    /// failing an invariant over.
    pub normalise_whitespace: bool,
    /// Stop after this many differences. A document that has gone completely
    /// wrong produces one difference per block, and the first dozen say
    /// everything the thousandth would.
    pub limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            compare_emphasis: true,
            compare_pages: true,
            normalise_whitespace: true,
            limit: 32,
        }
    }
}

/// Compare two flow documents. Empty means they agree.
pub fn compare(before: &FlowDocument, after: &FlowDocument, opts: &Options) -> Vec<Difference> {
    let mut out = Vec::new();

    if opts.compare_pages && before.meta.pages != after.meta.pages {
        out.push(Difference::PageCount { before: before.meta.pages, after: after.meta.pages });
    }

    if before.running.len() != after.running.len() {
        out.push(Difference::RunningChanged {
            before: before.running.len(),
            after: after.running.len(),
        });
    }

    let notes_before = count_notes(before);
    let notes_after = count_notes(after);
    if notes_before != notes_after {
        out.push(Difference::NotesChanged { before: notes_before, after: notes_after });
    }

    if before.blocks.len() != after.blocks.len() {
        out.push(Difference::BlockCount { before: before.blocks.len(), after: after.blocks.len() });

        // A different length makes a positional comparison meaningless: every
        // block after the first insertion would be reported as changed. The
        // order check below still says something useful, so it runs; the
        // block-by-block one does not.
        order_difference(before, after, opts, &mut out);
        out.truncate(opts.limit);
        return out;
    }

    // Permutation is tested *before* the block-by-block comparison, not after.
    // A reordered document differs at almost every position, so comparing
    // positionally first buries the one fact worth reporting — that nothing was
    // lost and the order moved — under a wall of text differences. Asking the
    // cheaper, more specific question first is what makes the answer readable.
    let before_order = out.len();
    order_difference(before, after, opts, &mut out);
    if out.len() > before_order {
        return out;
    }

    for (at, (a, b)) in before.blocks.iter().zip(&after.blocks).enumerate() {
        if out.len() >= opts.limit {
            break;
        }
        compare_block(at, a, b, opts, &mut out);
    }

    out.truncate(opts.limit);
    out
}

fn count_notes(doc: &FlowDocument) -> usize {
    doc.blocks.iter().filter(|b| matches!(b, Block::Note(_))).count()
}

fn compare_block(at: usize, a: &Block, b: &Block, opts: &Options, out: &mut Vec<Difference>) {
    if a.kind() != b.kind() {
        out.push(Difference::Kind { at, before: a.kind(), after: b.kind() });
        return;
    }

    match (a, b) {
        (
            Block::Heading { level: la, inlines: ia, .. },
            Block::Heading { level: lb, inlines: ib, .. },
        ) => {
            if la != lb {
                out.push(Difference::HeadingLevel { at, before: *la, after: *lb });
            }
            compare_inlines(at, ia, ib, opts, out);
        }

        (Block::Paragraph { inlines: ia, .. }, Block::Paragraph { inlines: ib, .. }) => {
            compare_inlines(at, ia, ib, opts, out);
        }

        (Block::List(la), Block::List(lb)) => {
            if la.items.len() != lb.items.len() {
                out.push(Difference::ListLength {
                    at,
                    before: la.items.len(),
                    after: lb.items.len(),
                });
                return;
            }
            for (ia, ib) in la.items.iter().zip(&lb.items) {
                compare_text(at, &ia.text(), &ib.text(), opts, out);
            }
        }

        (Block::Table(ta), Block::Table(tb)) => {
            let shape = |t: &crate::flow::Table| {
                (t.rows.len(), t.rows.iter().map(Vec::len).max().unwrap_or(0))
            };
            if shape(ta) != shape(tb) {
                out.push(Difference::TableShape { at, before: shape(ta), after: shape(tb) });
                return;
            }
            for (ra, rb) in ta.rows.iter().zip(&tb.rows) {
                for (ca, cb) in ra.iter().zip(rb) {
                    compare_text(at, &ca.text(), &cb.text(), opts, out);
                }
            }
        }

        // A figure's identity is its object, and a drawing's is its path count.
        // Neither carries text, so the kind check above is most of the
        // comparison; what remains is whether it is still the same thing.
        (Block::Figure { image: fa, .. }, Block::Figure { image: fb, .. }) => {
            if fa.object != fb.object {
                out.push(Difference::Text {
                    at,
                    before: format!("{:?}", fa.object),
                    after: format!("{:?}", fb.object),
                });
            }
        }

        (Block::Drawing(da), Block::Drawing(db)) => {
            if da.paths != db.paths {
                out.push(Difference::Text {
                    at,
                    before: format!("{} path(s)", da.paths),
                    after: format!("{} path(s)", db.paths),
                });
            }
        }

        _ => compare_text(at, &a.text(), &b.text(), opts, out),
    }
}

fn compare_inlines(
    at: usize,
    a: &[Inline],
    b: &[Inline],
    opts: &Options,
    out: &mut Vec<Difference>,
) {
    compare_text(at, &crate::flow::text_of(a), &crate::flow::text_of(b), opts, out);

    if !opts.compare_emphasis {
        return;
    }
    // Compared as a summary rather than run by run: two models can split the
    // same sentence into different runs and mean the same thing, and reporting
    // that as an emphasis change would drown the cases where the emphasis
    // genuinely moved.
    let summary = |runs: &[Inline]| {
        let mut bold = String::new();
        let mut italic = String::new();
        for run in runs {
            if let Inline::Text { text, emphasis } = run {
                if emphasis.bold {
                    bold.push_str(text);
                }
                if emphasis.italic {
                    italic.push_str(text);
                }
            }
        }
        (normalise(&bold, opts), normalise(&italic, opts))
    };
    if summary(a) != summary(b) {
        out.push(Difference::Emphasis { at, text: normalise(&crate::flow::text_of(a), opts) });
    }
}

fn compare_text(at: usize, a: &str, b: &str, opts: &Options, out: &mut Vec<Difference>) {
    let (a, b) = (normalise(a, opts), normalise(b, opts));
    if a != b {
        out.push(Difference::Text { at, before: a, after: b });
    }
}

fn normalise(text: &str, opts: &Options) -> String {
    if !opts.normalise_whitespace {
        return text.to_string();
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the same content came back in a different order.
///
/// Reported as a count of blocks not in their original position rather than as
/// a permutation: the number a reader wants is "how much moved", and a full
/// alignment would be a diff algorithm in service of a yes-or-no question.
fn order_difference(
    before: &FlowDocument,
    after: &FlowDocument,
    opts: &Options,
    out: &mut Vec<Difference>,
) {
    let texts = |doc: &FlowDocument| -> Vec<String> {
        doc.blocks.iter().map(|b| normalise(&b.text(), opts)).filter(|t| !t.is_empty()).collect()
    };
    let (a, b) = (texts(before), texts(after));
    if a.len() != b.len() {
        return;
    }

    let mut sorted_a = a.clone();
    let mut sorted_b = b.clone();
    sorted_a.sort();
    sorted_b.sort();
    if sorted_a != sorted_b {
        // Not a permutation: the content itself differs, which the block
        // comparison has already reported or will.
        return;
    }

    let moved = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    if moved > 0 {
        out.push(Difference::OrderChanged { moved });
    }
}

/// Compare only what a reader would read: the document's text, in order.
///
/// The comparison for a round trip that went through a **real PDF**. Block
/// boundaries cannot survive re-pagination and it is not a defect that they
/// don't: a paragraph the layout split across a page break is, to anyone
/// reading the result, two paragraphs — there is no mark in the file saying
/// otherwise, and the reconstruction is right to report two. Holding the
/// through-PDF round trip to block-for-block equality would be holding it to a
/// standard the format cannot express.
///
/// What must still hold is that every word is there and in the same order,
/// which is what this checks. [`compare`] remains the stricter test for the
/// in-memory round trip, where nothing has been re-paginated.
pub fn compare_reading(before: &FlowDocument, after: &FlowDocument) -> Vec<Difference> {
    let opts = Options::default();
    let text = |doc: &FlowDocument| {
        normalise(&doc.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join(" "), &opts)
    };
    let (a, b) = (text(before), text(after));
    if a == b {
        return Vec::new();
    }

    // Reported at the first divergence rather than as two whole documents: a
    // difference thousands of characters in is unreadable when the message
    // carries both sides in full.
    let at = a
        .chars()
        .zip(b.chars())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.chars().count().min(b.chars().count()));
    let window = |s: &str| s.chars().skip(at.saturating_sub(30)).take(90).collect::<String>();
    vec![Difference::Text { at, before: window(&a), after: window(&b) }]
}

/// A one-line summary of a round trip, for a corpus sweep.
pub fn summarise(differences: &[Difference]) -> String {
    if differences.is_empty() {
        return "identical".to_string();
    }
    let losses = differences.iter().filter(|d| d.is_content_loss()).count();
    format!(
        "{} difference(s), {losses} of them content loss: {}",
        differences.len(),
        differences.iter().take(3).map(Difference::kind).collect::<Vec<_>>().join(", ")
    )
}

/// Drift between two documents that is expected to be non-zero.
///
/// The weaker companion check `docs/flow-model.md` asks for. A re-laid-out
/// document legitimately has different pagination, and "different" has a limit:
/// twelve per cent more pages means something is wrong even when every block
/// round-tripped.
#[derive(Debug, Clone, Default)]
pub struct Drift {
    pub pages_before: usize,
    pub pages_after: usize,
    /// Characters of text before and after, which stands in for ink coverage.
    ///
    /// Ink proper needs a renderer. Character count is the part of it this
    /// layer can measure honestly, and it catches the failure that matters —
    /// text quietly disappearing — without pretending to measure area.
    pub chars_before: usize,
    pub chars_after: usize,
}

impl Drift {
    pub fn measure(before: &FlowDocument, after: &FlowDocument) -> Drift {
        let chars = |doc: &FlowDocument| -> usize {
            doc.blocks.iter().map(|b| b.text().chars().count()).sum()
        };
        Drift {
            pages_before: before.meta.pages,
            pages_after: after.meta.pages,
            chars_before: chars(before),
            chars_after: chars(after),
        }
    }

    /// Page growth as a fraction. Zero when the pagination is unchanged.
    pub fn page_drift(&self) -> f64 {
        if self.pages_before == 0 {
            return 0.0;
        }
        (self.pages_after as f64 - self.pages_before as f64) / self.pages_before as f64
    }

    pub fn char_drift(&self) -> f64 {
        if self.chars_before == 0 {
            return 0.0;
        }
        (self.chars_after as f64 - self.chars_before as f64) / self.chars_before as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Cell, Emphasis, Item, List, Table};

    fn para(text: &str) -> Block {
        Block::Paragraph { inlines: vec![Inline::text(text)], source: None }
    }

    fn doc(blocks: Vec<Block>) -> FlowDocument {
        FlowDocument { blocks, ..FlowDocument::default() }
    }

    #[test]
    fn identical_documents_have_no_differences() {
        let a = doc(vec![para("one"), para("two")]);
        assert!(compare(&a, &a.clone(), &Options::default()).is_empty());
        assert_eq!(summarise(&[]), "identical");
    }

    #[test]
    fn whitespace_alone_is_not_a_difference() {
        // Text is reconstructed from glyph positions, so one model may put a
        // space where another puts two. That is a segmentation difference, and
        // failing an invariant over it would make I8 useless on real files.
        let a = doc(vec![para("the quick brown fox")]);
        let b = doc(vec![para("the  quick   brown fox")]);
        assert!(compare(&a, &b, &Options::default()).is_empty());

        let strict = Options { normalise_whitespace: false, ..Options::default() };
        assert!(!compare(&a, &b, &strict).is_empty(), "and it is still visible on request");
    }

    #[test]
    fn a_dropped_block_is_content_loss() {
        let a = doc(vec![para("one"), para("two"), para("three")]);
        let b = doc(vec![para("one"), para("three")]);

        let diff = compare(&a, &b, &Options::default());
        assert!(matches!(diff[0], Difference::BlockCount { before: 3, after: 2 }));
        assert!(diff[0].is_content_loss());
    }

    #[test]
    fn a_permuted_reading_order_is_caught_even_though_nothing_was_lost() {
        // The failure `docs/flow-model.md` names first: every block survives and
        // the document reads in the wrong order. A comparison that only counted
        // blocks would pass this.
        let a = doc(vec![para("first"), para("second"), para("third")]);
        let b = doc(vec![para("second"), para("first"), para("third")]);

        let diff = compare(&a, &b, &Options::default());
        assert_eq!(diff.len(), 1, "{diff:#?}");
        assert!(matches!(diff[0], Difference::OrderChanged { moved: 2 }), "{diff:#?}");
        assert!(diff[0].is_content_loss());
    }

    #[test]
    fn a_flattened_table_is_caught() {
        // The other named failure. Three rows becoming one is a table that was
        // read as prose, and its text may well be identical.
        let cell = |t: &str| Cell { blocks: vec![para(t)] };
        let a = doc(vec![Block::Table(Table {
            rows: vec![vec![cell("a"), cell("b")], vec![cell("c"), cell("d")]],
            has_header: false,
            source: None,
        })]);
        let b = doc(vec![Block::Table(Table {
            rows: vec![vec![cell("a"), cell("b"), cell("c"), cell("d")]],
            has_header: false,
            source: None,
        })]);

        let diff = compare(&a, &b, &Options::default());
        assert!(
            matches!(diff[0], Difference::TableShape { before: (2, 2), after: (1, 4), .. }),
            "{diff:#?}"
        );
    }

    #[test]
    fn a_lost_style_is_reported_without_being_called_content_loss() {
        let plain = doc(vec![Block::Paragraph {
            inlines: vec![Inline::text("total revenue")],
            source: None,
        }]);
        let bold = doc(vec![Block::Paragraph {
            inlines: vec![Inline::Text {
                text: "total revenue".to_string(),
                emphasis: Emphasis { bold: true, ..Emphasis::default() },
            }],
            source: None,
        }]);

        let diff = compare(&plain, &bold, &Options::default());
        assert!(matches!(diff[0], Difference::Emphasis { .. }), "{diff:#?}");
        assert!(!diff[0].is_content_loss(), "a style is not content");

        let relaxed = Options { compare_emphasis: false, ..Options::default() };
        assert!(compare(&plain, &bold, &relaxed).is_empty());
    }

    #[test]
    fn splitting_one_run_into_two_is_not_an_emphasis_change() {
        // Two models can divide the same sentence differently and mean the same
        // thing. Comparing run by run would report that as a style change on
        // almost every document.
        let one = doc(vec![Block::Paragraph {
            inlines: vec![Inline::Text {
                text: "total revenue".to_string(),
                emphasis: Emphasis { bold: true, ..Emphasis::default() },
            }],
            source: None,
        }]);
        let two = doc(vec![Block::Paragraph {
            inlines: vec![
                Inline::Text {
                    text: "total ".to_string(),
                    emphasis: Emphasis { bold: true, ..Emphasis::default() },
                },
                Inline::Text {
                    text: "revenue".to_string(),
                    emphasis: Emphasis { bold: true, ..Emphasis::default() },
                },
            ],
            source: None,
        }]);

        assert!(compare(&one, &two, &Options::default()).is_empty());
    }

    #[test]
    fn a_heading_demoted_to_a_paragraph_is_a_kind_change() {
        let a = doc(vec![Block::Heading {
            level: 1,
            inlines: vec![Inline::text("Results")],
            source: None,
        }]);
        let b = doc(vec![para("Results")]);

        let diff = compare(&a, &b, &Options::default());
        assert!(
            matches!(diff[0], Difference::Kind { before: "heading", after: "paragraph", .. }),
            "{diff:#?}"
        );
    }

    #[test]
    fn a_list_that_loses_an_item_is_caught() {
        let items = |n: usize| List {
            ordered: true,
            items: (0..n).map(|i| Item { blocks: vec![para(&format!("item {i}"))] }).collect(),
            source: None,
        };
        let diff = compare(
            &doc(vec![Block::List(items(3))]),
            &doc(vec![Block::List(items(2))]),
            &Options::default(),
        );
        assert!(matches!(diff[0], Difference::ListLength { before: 3, after: 2, .. }));
    }

    #[test]
    fn the_difference_list_is_bounded() {
        // A document that has gone completely wrong produces one difference per
        // block, and the thousandth says nothing the twelfth did not.
        let a = doc((0..500).map(|i| para(&format!("before {i}"))).collect());
        let b = doc((0..500).map(|i| para(&format!("after {i}"))).collect());
        let diff = compare(&a, &b, &Options { limit: 10, ..Options::default() });
        assert_eq!(diff.len(), 10);
    }

    #[test]
    fn drift_measures_what_i8_deliberately_allows() {
        // Pagination is *expected* to change when a document is laid out again.
        // I8 says the content agreed; drift says by how much the shape moved.
        let mut before = doc(vec![para("one"), para("two")]);
        before.meta.pages = 10;
        let mut after = before.clone();
        after.meta.pages = 12;

        let drift = Drift::measure(&before, &after);
        assert!((drift.page_drift() - 0.2).abs() < 1e-9);
        assert_eq!(drift.char_drift(), 0.0, "the text did not change, only the pagination");
    }
}
