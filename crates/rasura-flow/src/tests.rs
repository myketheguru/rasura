//! Documents are built here rather than committed, so the input to every
//! assertion is readable in the assertion that makes it.
//!
//! Every test goes through the whole stack — cos, content, layout, then this
//! crate — because the thing being checked is a *reconstruction*, and a fixture
//! that fed the flow builder a hand-made `DocumentModel` would be testing this
//! crate against my idea of what the layer below produces rather than against
//! what it produces.

use crate::flow::{Block, Emphasis, Inline};
use crate::{Guess, Options, markdown};
use rasura_cos::Document;
use rasura_cos::testutil::ClassicBuilder;

/// A page of text, from lines of `(text, size, font, y)`.
///
/// The font is a resource name declared by `fonts`, so a test can put bold and
/// roman on the same page without repeating the dictionary boilerplate.
struct PageSpec {
    lines: Vec<(String, f64, &'static str, f64)>,
}

impl PageSpec {
    fn new() -> Self {
        PageSpec { lines: Vec::new() }
    }

    fn line(mut self, text: &str, size: f64, font: &'static str, y: f64) -> Self {
        self.lines.push((text.to_string(), size, font, y));
        self
    }

    fn content(&self) -> Vec<u8> {
        let mut out = String::new();
        for (text, size, font, y) in &self.lines {
            out.push_str(&format!(
                "BT /{font} {size} Tf 1 0 0 1 72 {y} Tm ({}) Tj ET\n",
                text.replace('\\', r"\\").replace('(', r"\(").replace(')', r"\)")
            ));
        }
        out.into_bytes()
    }
}

/// Build a document from page specs, with three named fonts.
///
/// Object numbering: 1 catalog, 2 pages, 3..5 fonts, then a page and a content
/// stream per spec.
fn build(pages: Vec<PageSpec>) -> Vec<u8> {
    let n = pages.len() as u32;
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 6 + i * 2)).collect();

    let mut b = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(
            2,
            &format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
        )
        .object(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
        .object(4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>")
        .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique /Encoding /WinAnsiEncoding >>");

    for (i, spec) in pages.iter().enumerate() {
        let page = 6 + i as u32 * 2;
        b = b
            .object(
                page,
                &format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                     /Resources << /Font << /F1 3 0 R /FB 4 0 R /FI 5 0 R >> >> >>",
                    page + 1
                ),
            )
            .stream(page + 1, "", &spec.content());
    }
    b.finish("/Root 1 0 R")
}

/// How many blocks were folded into lists, which only the caller can see.
fn gathered(flow: &crate::FlowDocument) -> usize {
    flow.blocks
        .iter()
        .map(|b| match b {
            Block::List(l) => l.items.len().saturating_sub(1),
            _ => 0,
        })
        .sum()
}

fn flow_of(bytes: Vec<u8>) -> (crate::FlowDocument, crate::Report) {
    let doc = Document::open(bytes).expect("open");
    let model = rasura_layout::model::analyse(&doc).expect("analyse");
    crate::flow(&model, &Options::default())
}

fn markdown_of(bytes: Vec<u8>) -> String {
    let (flow, _) = flow_of(bytes);
    markdown::render(&flow, &markdown::Options::default())
}

/// Body text, one line every 14 points down the page.
fn prose(start_y: f64, lines: &[&str]) -> Vec<(String, f64, &'static str, f64)> {
    lines
        .iter()
        .enumerate()
        .map(|(i, t)| (t.to_string(), 10.0, "F1", start_y - i as f64 * 14.0))
        .collect()
}

#[test]
fn a_larger_short_line_becomes_a_heading_and_the_prose_does_not() {
    let mut spec = PageSpec::new().line("Quarterly Results", 20.0, "FB", 700.0);
    spec.lines.extend(prose(
        660.0,
        &[
            "Revenue rose by eleven per cent over the period, driven",
            "chiefly by the subscription business, which grew faster",
            "than the board had forecast at the start of the year.",
        ],
    ));

    let (flow, report) = flow_of(build(vec![spec]));

    let heading = flow.blocks.iter().find(|b| matches!(b, Block::Heading { .. }));
    let heading = heading.expect("the 20pt line should be promoted");
    assert!(heading.text().contains("Quarterly Results"), "{}", heading.text());
    assert!(matches!(heading, Block::Heading { level: 1, .. }));

    // And exactly one: the prose is 10pt and must stay prose.
    assert_eq!(flow.blocks.iter().filter(|b| matches!(b, Block::Heading { .. })).count(), 1);
    assert!(flow.blocks.iter().any(|b| matches!(b, Block::Paragraph { .. })));

    // The promotion is a guess and says so.
    assert_eq!(report.made(Guess::HeadingInferred), 1);
    assert!(!report.is_exact());
    assert_eq!(flow.meta.body_size, Some(10.0), "body size is the modal size, not the mean");
}

#[test]
fn a_long_line_in_a_large_face_is_not_a_heading() {
    // The check that stops "set in a big font" from meaning "is a title". Only
    // the size test would pass here; the shortness test is what refuses.
    let long = "This entire paragraph is set in a large face because the \
                designer wanted an airy opening spread, and it runs on for a \
                good long while without ever becoming a heading of any kind.";
    let spec = PageSpec::new().line(long, 20.0, "F1", 700.0).line(
        "Ordinary body text follows underneath at the usual size for the document.",
        10.0,
        "F1",
        660.0,
    );

    let (flow, report) = flow_of(build(vec![spec]));
    assert_eq!(
        flow.blocks.iter().filter(|b| matches!(b, Block::Heading { .. })).count(),
        0,
        "{:#?}",
        flow.blocks
    );
    assert_eq!(report.made(Guess::HeadingInferred), 0);
}

#[test]
fn heading_levels_rank_by_size_across_the_whole_document() {
    // The document-level claim from `docs/flow-model.md`: one page is one
    // sample. The 14pt heading is on page two and must still be level 2,
    // because level 1 is taken by an 18pt heading it never shares a page with.
    let mut first = PageSpec::new().line("Part One", 18.0, "FB", 700.0);
    first.lines.extend(prose(660.0, &["Body text for the first part of the document."]));

    let mut second = PageSpec::new().line("A Subsection", 14.0, "FB", 700.0);
    second.lines.extend(prose(660.0, &["Body text for the second part of the document."]));

    let (flow, _) = flow_of(build(vec![first, second]));

    let levels: Vec<(u8, String)> = flow
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Heading { level, .. } => Some((*level, b.text())),
            _ => None,
        })
        .collect();

    assert_eq!(levels.len(), 2, "{levels:?}");
    assert_eq!(levels[0].0, 1, "18pt is the largest, so level 1");
    assert_eq!(levels[1].0, 2, "14pt ranks below it, on a different page");
}

#[test]
fn emphasis_comes_from_the_font_name_and_is_reported_as_a_guess() {
    let spec = PageSpec::new()
        .line("Plain words here", 10.0, "F1", 700.0)
        .line("Bold words here", 10.0, "FB", 686.0)
        .line("Italic words here", 10.0, "FI", 672.0);

    let (flow, report) = flow_of(build(vec![spec]));

    let mut seen = Emphasis::default();
    for block in &flow.blocks {
        if let Block::Paragraph { inlines, .. } = block {
            for inline in inlines {
                if let Inline::Text { text, emphasis } = inline {
                    if text.contains("Bold") {
                        assert!(emphasis.bold, "{text:?} came from Helvetica-Bold");
                        seen.bold = true;
                    }
                    if text.contains("Italic") {
                        assert!(emphasis.italic, "{text:?} came from Helvetica-Oblique");
                        seen.italic = true;
                    }
                }
            }
        }
    }
    assert!(seen.bold && seen.italic, "{:#?}", flow.blocks);
    assert!(report.made(Guess::EmphasisFromFontName) > 0, "a name is not a measurement");
}

#[test]
fn a_running_header_is_lifted_out_of_the_flow_rather_than_repeated() {
    // Four pages so the repetition is unambiguous, with the header at the top
    // of each and different prose underneath.
    let pages: Vec<PageSpec> = (0..4)
        .map(|i| {
            let mut spec = PageSpec::new().line("Annual Report 2025", 8.0, "F1", 760.0);
            spec.lines.extend(prose(
                700.0,
                &[
                    "This page carries its own body text, which differs from",
                    "the text on every other page of the document.",
                ],
            ));
            spec.lines.push((format!("Section {}", i + 1), 10.0, "F1", 660.0));
            spec
        })
        .collect();

    let (flow, _) = flow_of(build(pages));

    assert!(
        flow.running.iter().any(|r| r.template.contains("Annual Report")),
        "the header should be collected: {:#?}",
        flow.running
    );
    let inline = flow.blocks.iter().filter(|b| b.text().contains("Annual Report")).count();
    assert_eq!(inline, 0, "and must not also appear between every two paragraphs");
}

/// A document whose text cannot be mapped to Unicode: `/Identity-H` with no
/// `/ToUnicode` and no embedded program. Spec 7.8's `Unknown`.
///
/// The widths matter. Without `/W` or `/DW` every glyph advances zero, the
/// region has no extent, and it is dropped two crates below this one — which
/// makes the fixture test nothing at all.
fn unmappable() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm <0102030405060708> Tj ET\n")
        .object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /Unknowable /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>",
        )
        .object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Unknowable \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor 7 0 R /DW 600 >>",
        )
        .object(7, "<< /Type /FontDescriptor /FontName /Unknowable /Flags 4 >>")
        .finish("/Root 1 0 R")
}

#[test]
fn unclassified_text_is_carried_or_counted_but_never_silently_dropped() {
    // The model classifies this page as one `Unknown` block. Whether any text
    // can be recovered from it is not this crate's business — but its fate is:
    // either it reaches the export marked as unclassified, or the report says
    // it had nothing recoverable. Vanishing without either is the failure.
    let doc = Document::open(unmappable()).expect("open");
    let model = rasura_layout::model::analyse(&doc).expect("analyse");
    assert_eq!(model.reading_order.len(), 1, "the fixture should give one block to convert");
    assert!(matches!(
        model.block(model.reading_order[0]),
        Some(rasura_layout::model::Block::Unknown(_))
    ));

    let (flow, report) = crate::flow(&model, &Options::default());
    let carried = flow.blocks.iter().any(|b| matches!(b, Block::Opaque { .. }));
    let counted = report.empty_opaque_dropped > 0;
    assert!(
        carried != counted,
        "exactly one of carried or counted: carried={carried} counted={counted}, {report:#?}"
    );
}

#[test]
fn every_block_the_model_offers_is_exported_or_accounted_for() {
    // The general form of the property above, over a document with something of
    // each kind in it. An export that quietly loses a block is the one bug a
    // reader cannot detect, because the output still looks like a document.
    let mut spec = PageSpec::new().line("A Heading", 20.0, "FB", 700.0);
    spec.lines.extend(prose(
        660.0,
        &["First paragraph of body text.", "Second paragraph, distinct from the first."],
    ));

    let doc = Document::open(build(vec![spec])).expect("open");
    let model = rasura_layout::model::analyse(&doc).expect("analyse");
    let (flow, report) = crate::flow(&model, &Options::default());

    // Blocks either become flow blocks, are gathered into a list, are lifted
    // out as running furniture, or are counted as dropped. Nothing else.
    // `Report::accounts_for_everything` is the same implementation the corpus
    // survey runs, so the fixture and the 958-file measurement cannot disagree
    // about what "accounted for" means.
    assert!(
        report.accounts_for_everything(gathered(&flow)),
        "{report:#?} against {} block(s) out",
        flow.blocks.len()
    );
    assert!(report.blocks_in > 0, "the fixture has to offer something to account for");
}

/// A page from raw content-stream operators, for the cases the line helper
/// cannot express.
fn raw_page(content: &str) -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /FB 6 0 R >> >> >>",
        )
        .stream(4, "", content.as_bytes())
        .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
        .object(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>")
        .finish("/Root 1 0 R")
}

#[test]
fn words_separated_by_pen_movement_are_still_separate_words() {
    // The defect the first real export showed: a PDF often contains no space
    // characters at all, the gap between words being produced by moving the
    // pen. Concatenating glyph text gives "Theefficientoffice", and every
    // fixture written with literal spaces in the string passes anyway — which
    // is why this one has none.
    let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (The) Tj \
                   1 0 0 1 92 700 Tm (efficient) Tj \
                   1 0 0 1 140 700 Tm (office) Tj ET\n";

    let (flow, _) = flow_of(raw_page(content));
    let text = flow.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join(" ");
    assert!(text.contains("The efficient office"), "{text:?}");
}

#[test]
fn a_word_broken_across_two_lines_is_rejoined_without_its_hyphen() {
    // "compre-" / "hensive" is one word the producer's line breaking split.
    // Left alone it exports as "compre- hensive", which is the single most
    // recognisable artefact of a mechanical PDF-to-text conversion.
    let content = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (a compre-) Tj \
                   1 0 0 1 72 688 Tm (hensive account of it) Tj ET\n";

    let (flow, report) = flow_of(raw_page(content));
    let text = flow.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join(" ");

    if report.made(Guess::HyphenationJoined) > 0 {
        assert!(text.contains("comprehensive"), "{text:?}");
        assert!(!text.contains("compre-"), "the hyphen goes with the join: {text:?}");
    } else {
        // The layer below did not flag the paragraph, so this crate must not
        // remove anything: stripping a hyphen it was not told about would break
        // "well-known" wherever it happened to fall at a line end.
        assert!(text.contains("compre-"), "unflagged hyphens survive intact: {text:?}");
    }
}

#[test]
fn several_bold_words_are_one_run_rather_than_one_run_each() {
    // Words are pushed one at a time with a plain space between them, so three
    // bold words arrive as five runs and render as `**a** **b** **c**`.
    let content = "BT /FB 10 Tf 1 0 0 1 72 700 Tm (Total) Tj \
                   1 0 0 1 100 700 Tm (revenue) Tj \
                   1 0 0 1 145 700 Tm (rose) Tj ET\n";

    let (flow, _) = flow_of(raw_page(content));

    // Asserted per paragraph rather than over the page, because how the region
    // detector splits a line of widely-spaced words is its business and not
    // what this test is about. The invariant is that within any one paragraph
    // no two consecutive runs share their emphasis — which is exactly the
    // condition that renders as `**a** **b**`.
    let mut multi_word_bold = false;
    for block in &flow.blocks {
        let Block::Paragraph { inlines, .. } = block else { continue };
        for pair in inlines.windows(2) {
            if let [Inline::Text { emphasis: a, .. }, Inline::Text { emphasis: b, .. }] = pair {
                assert_ne!(a, b, "two consecutive runs share emphasis: {inlines:#?}");
            }
        }
        if inlines.iter().any(
            |i| matches!(i, Inline::Text { text, emphasis } if emphasis.bold && text.contains(' ')),
        ) {
            multi_word_bold = true;
        }
    }
    assert!(multi_word_bold, "at least one bold run should span a space: {:#?}", flow.blocks);

    let out = markdown::render(&flow, &markdown::Options::default());
    assert!(out.contains("**Total revenue**"), "{out:?}");
    assert!(!out.contains("** **"), "no empty emphasis between words: {out:?}");
}

#[test]
fn a_filled_form_field_reaches_the_export() {
    // The document whose text is entirely in annotations. Before the model read
    // them this exported as a blank page, which is what the corpus survey found
    // for pdf.js's `annotation-tx*.pdf` fixtures.
    let bytes = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Annots [9 0 R 10 0 R] >>",
        )
        .stream(4, "", b"\n")
        .object(
            9,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (signatory) /Rect [72 400 372 424] \
             /V (A. Ozdamar) >>",
        )
        .object(
            10,
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (check this figure) >>",
        )
        .finish("/Root 1 0 R");

    let (flow, report) = flow_of(bytes);
    assert_eq!(report.notes_recovered, 2);

    let notes: Vec<&crate::flow::Note> = flow
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 2, "{:#?}", flow.blocks);
    assert_eq!(notes[0].field.as_deref(), Some("signatory"));
    assert_eq!(notes[0].text, "A. Ozdamar");
    assert_eq!(notes[1].text, "check this figure");

    let out = markdown::render(&flow, &markdown::Options::default());
    assert!(out.contains("A. Ozdamar"), "{out}");
    // Labelled, so a reader cannot mistake a field's value for body text.
    assert!(out.contains("Widget"), "{out}");

    let without = markdown::render(
        &flow,
        &markdown::Options { include_notes: false, ..markdown::Options::default() },
    );
    assert!(!without.contains("A. Ozdamar"), "and can be turned off: {without:?}");
}

#[test]
fn a_hidden_annotation_contributes_nothing() {
    let bytes = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Annots [9 0 R] >>",
        )
        .stream(4, "", b"\n")
        .object(
            9,
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /F 2 /Contents (not shown) >>",
        )
        .finish("/Root 1 0 R");

    let (flow, report) = flow_of(bytes);
    assert_eq!(report.notes_recovered, 0);
    assert_eq!(report.hidden_annotations_skipped, 1, "skipped, and said so");
    assert!(!markdown::render(&flow, &markdown::Options::default()).contains("not shown"));
}

#[test]
fn a_chart_is_reported_and_a_rule_is_not() {
    // Both are vector blocks. One is content and one is decoration, and an
    // export that marked every table border would be unreadable — but before
    // the layout layer retained path geometry, neither could be distinguished
    // and both were dropped.
    let rule = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
        .stream(4, "", b"72 700 400 1 re f\n")
        .finish("/Root 1 0 R");

    let (flow, report) = flow_of(rule);
    assert!(
        !flow.blocks.iter().any(|b| matches!(b, Block::Drawing(_))),
        "a 1pt rule is decoration: {:#?}",
        flow.blocks
    );
    assert_eq!(report.rules_dropped, 1);

    // A cluster of curves at a real size is a picture.
    let mut content = String::new();
    for i in 0..12 {
        let y = 400 + i * 4;
        content.push_str(&format!("100 {y} m 150 {} 250 {} 300 {y} c S\n", y + 40, y - 40));
    }
    let chart = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
        .stream(4, "", content.as_bytes())
        .finish("/Root 1 0 R");

    let (flow, _) = flow_of(chart);
    let drawing = flow
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Drawing(d) => Some(d),
            _ => None,
        })
        .expect("the chart is reported");
    assert_eq!(drawing.kind, crate::flow::DrawingKind::Figure);
    assert_eq!(drawing.paths, 12);

    let out = markdown::render(&flow, &markdown::Options::default());
    assert!(out.contains("drawing: 12 path(s)"), "{out}");
}

#[test]
fn markdown_renders_the_structure_it_was_given() {
    let mut spec = PageSpec::new().line("Quarterly Results", 20.0, "FB", 700.0);
    spec.lines.extend(prose(
        660.0,
        &[
            "Revenue rose by eleven per cent over the period, driven by",
            "the subscription business.",
        ],
    ));

    let out = markdown_of(build(vec![spec]));
    assert!(out.starts_with("# Quarterly Results\n\n"), "{out:?}");
    assert!(out.contains("Revenue rose"), "{out:?}");
    assert!(out.ends_with('\n') && !out.ends_with("\n\n"), "exactly one trailing newline: {out:?}");
}

// --- Renderer unit tests, over a hand-built flow document. ------------------
//
// These need no PDF: the question is what the renderer does with a given model,
// and building the model directly is the only way to ask about a table with no
// header, which the reconstruction cannot currently produce.

use crate::flow::{Cell, FlowDocument, Table};

fn para(text: &str) -> Block {
    Block::Paragraph { inlines: vec![Inline::text(text)], source: None }
}

fn cell(text: &str) -> Cell {
    Cell { blocks: vec![para(text)] }
}

#[test]
fn a_table_without_a_header_keeps_all_its_rows() {
    // GFM has no headerless table. Promoting the first row would delete a row
    // of data from the reader's view, so an empty header is emitted instead.
    let doc = FlowDocument {
        blocks: vec![Block::Table(Table {
            rows: vec![vec![cell("Q1"), cell("11.2")], vec![cell("Q2"), cell("12.9")]],
            has_header: false,
            source: None,
        })],
        ..FlowDocument::default()
    };

    let out = markdown::render(&doc, &markdown::Options::default());
    assert!(out.contains("| Q1 | 11.2 |"), "{out}");
    assert!(out.contains("| Q2 | 12.9 |"), "{out}");
    assert_eq!(out.matches("---").count(), 2, "one separator per column: {out}");
}

#[test]
fn a_pipe_in_a_cell_does_not_add_a_column() {
    let doc = FlowDocument {
        blocks: vec![Block::Table(Table {
            rows: vec![vec![cell("a|b"), cell("c\nd")]],
            has_header: false,
            source: None,
        })],
        ..FlowDocument::default()
    };

    let out = markdown::render(&doc, &markdown::Options::default());
    let row = out.lines().find(|l| l.contains("a")).expect("the row");
    assert!(row.contains(r"a\|b"), "the pipe is escaped: {row}");
    assert!(!out.contains("c\nd"), "the newline is flattened: {out:?}");

    // Two cells is three structural pipes. Counted as unescaped ones, because
    // an escaped pipe is content and the whole point of escaping it is that the
    // renderer stops seeing it as a column boundary.
    let structural = row
        .char_indices()
        .filter(|(i, c)| *c == '|' && (*i == 0 || row.as_bytes()[i - 1] != b'\\'))
        .count();
    assert_eq!(structural, 3, "two cells means three column boundaries: {row}");
}

#[test]
fn emphasis_markers_sit_against_the_text_not_against_its_spaces() {
    // `**bold ** ` is not bold in any Markdown implementation. A run that ends
    // with a space has to have the marker moved inside it.
    let doc = FlowDocument {
        blocks: vec![Block::Paragraph {
            inlines: vec![
                Inline::Text {
                    text: "bold ".to_string(),
                    emphasis: Emphasis { bold: true, ..Emphasis::default() },
                },
                Inline::text("and plain"),
            ],
            source: None,
        }],
        ..FlowDocument::default()
    };

    let out = markdown::render(&doc, &markdown::Options::default());
    assert!(out.starts_with("**bold** and plain"), "{out:?}");
}

#[test]
fn a_list_item_with_two_paragraphs_stays_one_item() {
    use crate::flow::{Item, List};

    let doc = FlowDocument {
        blocks: vec![Block::List(List {
            ordered: true,
            items: vec![
                Item { blocks: vec![para("First point."), para("Still the first point.")] },
                Item { blocks: vec![para("Second point.")] },
            ],
            source: None,
        })],
        ..FlowDocument::default()
    };

    let out = markdown::render(&doc, &markdown::Options::default());
    // The continuation is indented to the marker's width; if it were not, the
    // second paragraph would end the list and read as body text.
    assert!(out.contains("1. First point."), "{out}");
    assert!(out.contains("\n   Still the first point."), "{out:?}");
    assert!(out.contains("2. Second point."), "{out}");
}

#[test]
fn opaque_blocks_are_marked_rather_than_dropped() {
    use crate::flow::OpaqueReason;

    let doc = FlowDocument {
        blocks: vec![Block::Opaque {
            text: "recoverable but untrusted".to_string(),
            reason: OpaqueReason::Unmapped,
            source: None,
        }],
        ..FlowDocument::default()
    };

    let shown = markdown::render(&doc, &markdown::Options::default());
    assert!(shown.contains("unclassified"), "{shown}");
    assert!(shown.contains("recoverable but untrusted"), "{shown}");

    let hidden = markdown::render(
        &doc,
        &markdown::Options { include_opaque: false, ..markdown::Options::default() },
    );
    assert_eq!(hidden.trim(), "", "and can be turned off explicitly: {hidden:?}");
}

#[test]
fn markdown_syntax_in_the_source_text_is_escaped() {
    // A PDF containing "see [1] * note" must not render as a link and a bullet.
    let doc = FlowDocument { blocks: vec![para("see [1] * note_here")], ..FlowDocument::default() };
    let out = markdown::render(&doc, &markdown::Options::default());
    assert!(out.contains(r"\[1\]"), "{out:?}");
    assert!(out.contains(r"\*"), "{out:?}");
    assert!(out.contains(r"note\_here"), "{out:?}");
}

#[test]
fn the_modal_size_breaks_ties_the_same_way_every_time() {
    // `max_by_key` over a `HashMap` visits in an order that varies between
    // runs, so two sizes with equal glyph counts produced a different body size
    // each time — which moved the heading ladder, which changed whether a
    // paragraph was promoted. I8's determinism sweep found it on exactly one
    // corpus file, reporting the same bytes as giving two different models.
    //
    // Asserted on the rule rather than through a fixture: constructing a tie in
    // glyph counts through a real page is fiddly, and the tie-break is the
    // thing that was wrong.
    let counts = || {
        let mut m = std::collections::HashMap::new();
        m.insert(20u64, 100usize); // 10pt, quantised
        m.insert(36u64, 100usize); // 18pt, exactly as common
        m.insert(24u64, 40usize);
        m
    };

    for _ in 0..64 {
        assert_eq!(
            crate::build::modal(counts()),
            Some(10.0),
            "the smaller of two tied sizes wins, every time"
        );
    }

    let mut clear = std::collections::HashMap::new();
    clear.insert(36u64, 100usize);
    clear.insert(20u64, 40usize);
    assert_eq!(crate::build::modal(clear), Some(18.0), "a real majority still wins");
}

#[test]
fn analysing_the_same_bytes_twice_gives_the_same_flow_document() {
    // I8's first round trip, as a unit test. A reconstruction that is not
    // stable against itself cannot be stable against a layout engine.
    use crate::compare::{Options as CompareOptions, compare};

    let mut spec = PageSpec::new().line("Quarterly Results", 20.0, "FB", 700.0);
    spec.lines
        .extend(prose(660.0, &["Revenue rose by eleven per cent.", "The board was pleased."]));
    let bytes = build(vec![spec]);

    let (first, _) = flow_of(bytes.clone());
    for _ in 0..8 {
        let (again, _) = flow_of(bytes.clone());
        let diff = compare(&first, &again, &CompareOptions::default());
        assert!(diff.is_empty(), "{diff:#?}");
    }
}

#[test]
fn the_subset_prefix_is_not_part_of_the_face_name() {
    // `ABCDEF+Times-Bold` is a subset of a bold face. Six random uppercase
    // letters are not evidence of anything, and one of them being `B` must not
    // make a roman face bold.
    assert_eq!(crate::build::strip_subset_prefix("ABCDEF+Times-Bold"), "Times-Bold");
    assert_eq!(crate::build::strip_subset_prefix("Times-Bold"), "Times-Bold");
    assert_eq!(crate::build::strip_subset_prefix("ABC+Times"), "ABC+Times", "too short to be one");
    assert_eq!(crate::build::strip_subset_prefix("abcdef+Times"), "abcdef+Times", "must be caps");
}
