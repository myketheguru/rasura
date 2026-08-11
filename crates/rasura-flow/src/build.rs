//! Placed model to flow model.
//!
//! The conversion is mostly bookkeeping. The two real decisions are which
//! paragraphs are headings and which are list items, and both have the same
//! shape: the structure tree answers them outright where it exists, and where
//! it does not they are inferred from typography and the inference is recorded
//! as an inference.
//!
//! # Structure first, always
//!
//! `docs/flow-model.md` puts it as a design constraint rather than a
//! preference:
//!
//! > **Tagged documents first.** Where `/StructTreeRoot` exists the producer
//! > wrote the logical structure down and the guessing largely disappears.
//!
//! So `role_of` is consulted before any heuristic runs, and the heuristics are
//! written to be *conservative* rather than clever: a paragraph is promoted to
//! a heading only on positive evidence, and everything not promoted stays a
//! paragraph. The failure this avoids is the expensive one — an export whose
//! headings are confidently wrong reads worse than one with no headings at all,
//! because a reader cannot see the mistake.

use crate::flow::{
    Block, Cell, Drawing, DrawingKind, Emphasis, FlowDocument, Image, Inline, Item, List, Meta,
    Note, OpaqueReason, Provenance, Running, Source, Table,
};
use crate::report::{Guess, Report};
use rasura_layout::model::{Block as ModelBlock, BlockId, DocumentModel, PageModel};
use rasura_layout::paragraphs::{Paragraph, Style};
use rasura_layout::structure::{StructElement, StructTree};
use rasura_layout::{Line, PlacedGlyph};
use std::collections::HashMap;

/// How the conversion should behave where the evidence runs out.
#[derive(Debug, Clone)]
pub struct Options {
    /// Promote large paragraphs to headings when the document is untagged.
    ///
    /// On by default, because an export with no headings at all is not a flow
    /// document in any useful sense. Turn it off for a corpus measurement,
    /// where the question is what the *structure* says rather than what this
    /// crate can guess.
    pub infer_headings: bool,
    /// Recognise list items from their markers when the document is untagged.
    pub infer_lists: bool,
    /// A paragraph must be at least this many times the body size to become a
    /// heading. 1.15 is deliberately loose at the bottom: a 12pt body with 13pt
    /// bold headings is common, and the shortness test below carries most of
    /// the weight.
    pub heading_ratio: f64,
    /// A heading is short. Longer than this many characters and the size is
    /// taken to be a large body face rather than a title.
    pub heading_max_chars: usize,
    /// Keep glyphs drawn in an invisible render mode.
    ///
    /// On, because for a scanned page this is the only text there is: every OCR
    /// tool lays its output over the image in mode 3. Off for an export where
    /// the visible page is the whole product.
    pub keep_invisible: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            infer_headings: true,
            infer_lists: true,
            heading_ratio: 1.15,
            heading_max_chars: 120,
            keep_invisible: true,
        }
    }
}

/// Convert a placed document model into flowing content.
pub fn flow(model: &DocumentModel, opts: &Options) -> (FlowDocument, Report) {
    let mut report = Report::default();
    let roles = Roles::new(model);

    let body_size = body_text_size(model);
    let mut out = FlowDocument {
        meta: Meta {
            pages: model.pages.len(),
            tagged: model.structure.is_some(),
            order: match model.order_source {
                rasura_layout::model::OrderSource::Structure => Provenance::Structure,
                rasura_layout::model::OrderSource::Geometry => Provenance::Geometry,
            },
            body_size,
        },
        ..FlowDocument::default()
    };

    if out.meta.order == Provenance::Geometry {
        // Named once, at the top, rather than on every block: the whole
        // document's order is a guess, and a caller who reads this should treat
        // the export's sequence as evidence rather than as fact.
        report.note(Guess::ReadingOrderInferred);
    }

    // The heading sizes, ranked, so level 1 is the largest actually present
    // rather than a fixed point on a scale no document agreed to.
    let ladder = if opts.infer_headings && model.structure.is_none() {
        heading_ladder(model, body_size, opts)
    } else {
        Vec::new()
    };

    let mut pending_list: Option<PendingList> = None;

    for (id, block) in model.in_reading_order() {
        let page = &model.pages[id.page];

        // A running header is furniture, not content. Collected once and
        // skipped here; see `FlowDocument::running`.
        if let ModelBlock::Running(r) = block {
            report.running_lifted += 1;
            if !out.running.iter().any(|seen| seen.template == r.template) {
                out.running.push(Running {
                    template: r.template.clone(),
                    top: matches!(r.placement, rasura_layout::running::Placement::Header),
                    pages: r.pages.clone(),
                    is_page_number: r.is_page_number,
                });
            }
            continue;
        }

        let produced = convert(block, id, page, &roles, &ladder, opts, &mut report);

        // List items arrive one paragraph at a time and have to be gathered:
        // the model has no notion of a list, so consecutive items are
        // consecutive paragraphs and nothing joins them but adjacency.
        match produced {
            Produced::ListItem { ordered, blocks } => match &mut pending_list {
                Some(p) if p.ordered == ordered => p.items.push(Item { blocks }),
                _ => {
                    flush_list(&mut pending_list, &mut out.blocks);
                    pending_list = Some(PendingList {
                        ordered,
                        items: vec![Item { blocks }],
                        source: Some(id),
                    });
                }
            },
            Produced::Blocks(blocks) => {
                flush_list(&mut pending_list, &mut out.blocks);
                out.blocks.extend(blocks);
            }
            Produced::Nothing => {}
        }
    }
    flush_list(&mut pending_list, &mut out.blocks);

    report.blocks_in = model.reading_order.len();
    report.blocks_out = out.blocks.len();

    // Annotations last, and outside the block accounting: they were never in
    // the reading order to begin with, because the cut tree only ever saw the
    // content stream. Counted separately so `blocks_out` stays comparable with
    // `blocks_in`.
    for (index, page) in model.pages.iter().enumerate() {
        for annotation in &page.annotations {
            let Some(text) = annotation.visible_text() else {
                if annotation.hidden {
                    report.hidden_annotations_skipped += 1;
                }
                continue;
            };
            report.notes_recovered += 1;
            out.blocks.push(Block::Note(Note {
                kind: annotation
                    .kind
                    .map(|k| k.as_str().to_string())
                    .unwrap_or_else(|| "Annot".to_string()),
                field: annotation.field_name.clone(),
                text: text.to_string(),
                page: index,
            }));
        }
    }

    (out, report)
}

struct PendingList {
    ordered: bool,
    items: Vec<Item>,
    source: Source,
}

fn flush_list(pending: &mut Option<PendingList>, into: &mut Vec<Block>) {
    if let Some(p) = pending.take() {
        into.push(Block::List(List { ordered: p.ordered, items: p.items, source: p.source }));
    }
}

enum Produced {
    Blocks(Vec<Block>),
    ListItem { ordered: bool, blocks: Vec<Block> },
    Nothing,
}

fn convert(
    block: &ModelBlock,
    id: BlockId,
    page: &PageModel,
    roles: &Roles,
    ladder: &[f64],
    opts: &Options,
    report: &mut Report,
) -> Produced {
    match block {
        ModelBlock::Paragraph(p) => {
            let lines = page.lines.get(id.index).map(Vec::as_slice).unwrap_or(&[]);
            let inlines = inlines_of(p, lines, opts, report);
            if crate::flow::text_of(&inlines).trim().is_empty() {
                // Accepted as a paragraph by the reconstruction and empty by the
                // time it got here. Counted rather than skipped: on the corpus
                // this is the single largest reason a document exports to
                // nothing, and a silent `continue` would make that invisible.
                report.empty_paragraphs_dropped += 1;
                return Produced::Nothing;
            }

            // The structure tree, if it named this paragraph.
            if let Some(role) = roles.role_of(id.page, p.mcid) {
                return from_role(role, inlines, id, report);
            }

            if opts.infer_lists {
                if let Some((ordered, rest)) = list_marker(&inlines, p) {
                    report.note(Guess::ListInferred);
                    return Produced::ListItem {
                        ordered,
                        blocks: vec![Block::Paragraph { inlines: rest, source: Some(id) }],
                    };
                }
            }

            if let Some(level) = heading_level(p, lines, ladder, opts) {
                report.note(Guess::HeadingInferred);
                return Produced::Blocks(vec![Block::Heading { level, inlines, source: Some(id) }]);
            }

            Produced::Blocks(vec![Block::Paragraph { inlines, source: Some(id) }])
        }

        ModelBlock::Table(t) => {
            let header = roles.table_has_header(id.page, t);
            let mut rows: Vec<Vec<Cell>> = vec![Vec::new(); t.rows];
            for cell in &t.cells {
                if cell.row >= t.rows {
                    continue;
                }
                let row = &mut rows[cell.row];
                while row.len() <= cell.column {
                    row.push(Cell::default());
                }
                let text = cell.lines.iter().map(rasura_layout::line_text).collect::<Vec<_>>();
                let joined = text.join(" ").trim().to_string();
                if !joined.is_empty() {
                    row[cell.column] = Cell {
                        blocks: vec![Block::Paragraph {
                            inlines: vec![Inline::text(joined)],
                            source: Some(id),
                        }],
                    };
                }
            }
            // A cell's inline styling is dropped here. Cells carry paragraphs
            // with their own style runs, and threading those through doubles
            // the size of this function to recover emphasis inside table cells
            // — worth doing, not worth doing first.
            report.note(Guess::TableCellStyleDropped);
            Produced::Blocks(vec![Block::Table(Table {
                rows,
                has_header: header,
                source: Some(id),
            })])
        }

        ModelBlock::Image(i) => Produced::Blocks(vec![Block::Figure {
            alt: roles.alt_for_figure(id.page),
            image: Image { object: i.id, pixels: i.pixels, page: id.page },
            source: Some(id),
        }]),

        // Vector artwork. This used to be dropped outright, because a
        // `VectorBlock` carried a bounding box and a path count and nothing
        // else; on the corpus that was 77 documents exporting to nothing at
        // all. It now carries its paths, so the drawing can at least be
        // reported — and the distinction that makes the report readable is
        // between a picture and a rule.
        ModelBlock::Vector(v) => {
            let kind = classify_drawing(v);
            if kind == DrawingKind::Rule {
                // Underlines, table borders and the line under a running head.
                // Marking each of them would bury the text they decorate.
                report.rules_dropped += 1;
                return Produced::Nothing;
            }
            Produced::Blocks(vec![Block::Drawing(Drawing {
                paths: v.count,
                kind,
                size: (v.bbox.width().abs(), v.bbox.height().abs()),
                source: Some(id),
            })])
        }

        ModelBlock::Unknown(raw) => {
            let text = raw.text().trim().to_string();
            if text.is_empty() {
                report.empty_opaque_dropped += 1;
                return Produced::Nothing;
            }
            Produced::Blocks(vec![Block::Opaque {
                text,
                reason: match raw.reason {
                    rasura_layout::model::DeclineReason::Unmapped => OpaqueReason::Unmapped,
                    rasura_layout::model::DeclineReason::NonHorizontal => {
                        OpaqueReason::NonHorizontal
                    }
                },
                source: Some(id),
            }])
        }

        // Handled by the caller, which needs to collect rather than convert.
        ModelBlock::Running(_) => Produced::Nothing,
    }
}

/// A rule, or a picture.
///
/// The test is deliberately narrow, because the cost of the two mistakes is not
/// symmetric: calling a chart a rule deletes it from the export, and calling a
/// rule a chart adds one line of noise. So a drawing is a rule only when it is a
/// small number of paths, every one of them an axis-aligned rectangle or a
/// straight line, and the whole thing is thinner than a line of text.
///
/// `paths` is empty on a page with more than `MAX_PATHS` of them, which cannot
/// be a rule by the count test anyway.
fn classify_drawing(block: &rasura_layout::VectorBlock) -> DrawingKind {
    const RULE_MAX_PATHS: usize = 4;
    const RULE_MAX_THICKNESS: f64 = 6.0;

    if block.count > RULE_MAX_PATHS || block.paths.is_empty() {
        return DrawingKind::Figure;
    }
    let thin = block.bbox.height().abs() <= RULE_MAX_THICKNESS
        || block.bbox.width().abs() <= RULE_MAX_THICKNESS;
    if !thin {
        return DrawingKind::Figure;
    }
    let simple = block.paths.iter().all(|p| {
        p.is_rectangle()
            || p.subpaths.iter().all(|s| {
                s.segments.len() <= 1
                    && s.segments
                        .iter()
                        .all(|seg| matches!(seg, rasura_layout::graphics::Segment::Line(_)))
            })
    });
    if simple { DrawingKind::Rule } else { DrawingKind::Figure }
}

/// Turn a structure-tree role into a block. The producer's own word wins.
fn from_role(role: &str, inlines: Vec<Inline>, id: BlockId, report: &mut Report) -> Produced {
    let source = Some(id);
    match role {
        "H1" | "H2" | "H3" | "H4" | "H5" | "H6" => {
            let level = role.as_bytes()[1] - b'0';
            Produced::Blocks(vec![Block::Heading { level, inlines, source }])
        }
        // A bare `H` is legal and says only "a heading". Level 1 is the least
        // wrong answer: nesting depth would be a guess, and the structure tree
        // exists precisely so this crate does not guess.
        "H" => Produced::Blocks(vec![Block::Heading { level: 1, inlines, source }]),
        "LI" | "LBody" => Produced::ListItem {
            // `/L` carries the numbering in `/ListNumbering`, which this layer
            // does not read yet, so ordered-ness is unknown and unordered is
            // the safer default: a bulleted list rendered as numbered invents
            // an order the document never claimed.
            ordered: false,
            blocks: vec![Block::Paragraph { inlines, source }],
        },
        "Lbl" => {
            // The list marker itself, which the flow model draws rather than
            // stores. Dropping it is right; dropping it silently is not.
            report.note(Guess::ListLabelDropped);
            Produced::Nothing
        }
        _ => Produced::Blocks(vec![Block::Paragraph { inlines, source }]),
    }
}

/// The structure tree, indexed for the questions this module asks of it.
struct Roles {
    /// `(page, mcid)` to the role of the element that owns it.
    by_mcid: HashMap<(usize, u32), String>,
    /// `/Alt` on a `Figure`, by page. Approximate — a page with two figures
    /// cannot tell them apart this way — and better than no alt text at all.
    figure_alt: HashMap<usize, String>,
    header_rows: bool,
}

impl Roles {
    fn new(model: &DocumentModel) -> Roles {
        let mut by_mcid = HashMap::new();
        let mut figure_alt = HashMap::new();
        let mut header_rows = false;

        if let Some(tree) = &model.structure {
            for element in &tree.elements {
                let role = effective_role(tree, element);
                if role == "TH" {
                    header_rows = true;
                }
                for (page, mcid) in &element.mcids {
                    by_mcid.insert((*page, *mcid), role.clone());
                }
                if role == "Figure" {
                    if let Some(alt) = element.alt.clone().or_else(|| element.actual_text.clone()) {
                        for (page, _) in &element.mcids {
                            figure_alt.entry(*page).or_insert(alt.clone());
                        }
                        // A figure with no marked content of its own still
                        // describes something; without an mcid there is no page
                        // to file it under, so it is left for the geometry.
                    }
                }
            }
        }
        Roles { by_mcid, figure_alt, header_rows }
    }

    fn role_of(&self, page: usize, mcid: Option<u32>) -> Option<&str> {
        self.by_mcid.get(&(page, mcid?)).map(String::as_str)
    }

    fn alt_for_figure(&self, page: usize) -> Option<String> {
        self.figure_alt.get(&page).cloned()
    }

    /// Whether a table's first row is a header.
    ///
    /// Only ever true when the structure tree says `TH` somewhere. Guessing it
    /// from bold text would be wrong often enough to matter: a table whose
    /// first row is emphasised for any other reason would acquire a header it
    /// does not have, and in Markdown a header row is not a style — it changes
    /// what the table means.
    fn table_has_header(&self, _page: usize, _table: &rasura_layout::Table) -> bool {
        self.header_rows
    }
}

/// `/RoleMap` is already applied by the structure reader, which leaves `role`
/// empty when the type mapped to nothing. Falling back to `kind` keeps a
/// private type visible rather than turning it into an unnamed paragraph.
fn effective_role(_tree: &StructTree, element: &StructElement) -> String {
    if element.role.is_empty() { element.kind.clone() } else { element.role.clone() }
}

/// Split a paragraph into styled inline runs.
fn inlines_of(
    paragraph: &Paragraph,
    lines: &[Line],
    opts: &Options,
    report: &mut Report,
) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();

    for (li, line) in lines.iter().enumerate() {
        if li > 0 {
            // Lines within a paragraph are joined with a space, not a break:
            // they were broken by the producer's measure, and preserving that
            // in a flow document would hard-wrap the text at a width that no
            // longer applies. `hard_break` handles the ones the author meant.
            //
            // Unless the previous line was hyphenated, in which case the two
            // halves are one word and both the hyphen and the space have to go.
            // §7.6 records `hyphenation_was_present` for exactly this, and
            // without it an export reads "ex- traction".
            if paragraph.hyphenation_was_present && ends_with_soft_hyphen(&mut out) {
                report.note(Guess::HyphenationJoined);
            } else {
                push_text(&mut out, " ", Emphasis::default());
            }
        }

        // Segmented into words rather than walked glyph by glyph.
        //
        // A PDF frequently contains no space characters at all: the gap between
        // two words is produced by moving the pen, and §7.3's segmentation is
        // what recovers it. Concatenating glyph text directly — which is what
        // this did first — produces `Theefficientoffice` from a document that
        // renders as "The efficient office", and it does so on every file whose
        // producer sets spaces positionally, which is most of them.
        for (wi, word) in rasura_layout::words::segment(line).iter().enumerate() {
            if wi > 0 {
                push_text(&mut out, " ", Emphasis::default());
            }
            for gi in word.glyphs.clone() {
                let Some(glyph) = line.glyphs.get(gi) else { continue };
                let Some(text) = glyph.text.as_deref() else {
                    continue;
                };
                // Per glyph rather than per word, because a style run can
                // change inside a word — `**bold**face` is one word and two
                // runs, and taking the word's first style would lose the
                // distinction.
                let style = style_at(paragraph, li, gi);
                let emphasis = style.map(|s| emphasis_of(s, report)).unwrap_or_default();
                if emphasis.invisible && !opts.keep_invisible {
                    continue;
                }
                push_text(&mut out, text, emphasis);
            }
        }
    }

    // Word segmentation happens per line in the layer below, which puts the
    // spaces between words inside each line's text; joining lines above adds
    // the ones between them. Collapsing here catches the overlap.
    collapse_spaces(&mut out);
    heal_split_emphasis(&mut out);
    out
}

/// Strip a trailing hyphen left by the producer's line breaking.
///
/// Returns whether one was found, so the caller knows not to add the space that
/// would otherwise join the two lines. Only the characters that are actually
/// used to break a word: a trailing em dash is punctuation the author wrote and
/// must survive.
fn ends_with_soft_hyphen(inlines: &mut Vec<Inline>) -> bool {
    let Some(Inline::Text { text, .. }) = inlines.last_mut() else {
        return false;
    };
    let trimmed = text.trim_end();
    if !trimmed.ends_with(['-', '\u{2010}', '\u{00ad}']) {
        return false;
    }
    // A single hyphen alone in a run is not a word being broken.
    if trimmed.chars().count() < 2 {
        return false;
    }
    let keep = trimmed.len() - trimmed.chars().next_back().map(char::len_utf8).unwrap_or(1);
    text.truncate(keep);
    if text.is_empty() {
        inlines.pop();
    }
    true
}

/// Rejoin runs that only a separator space is keeping apart.
///
/// Words are pushed one at a time with a plain space between them, so three
/// bold words become five runs — bold, space, bold, space, bold — and a
/// Markdown renderer faithfully emits `**1.1** **More** **text**`. The space
/// between two runs of the same emphasis belongs to that emphasis; the space
/// between two *different* ones does not, and is left alone.
fn heal_split_emphasis(inlines: &mut Vec<Inline>) {
    // Adjacent runs first. `push_text` merges as it goes, but only against the
    // run that was last at the time — a separator space that later turns out to
    // be removable leaves two like runs side by side, and the three-way rule
    // below would then skip straight past them.
    let mut i = 0;
    while i + 1 < inlines.len() {
        let same = matches!(
            (&inlines[i], &inlines[i + 1]),
            (Inline::Text { emphasis: a, .. }, Inline::Text { emphasis: b, .. }) if a == b
        );
        if same {
            let Inline::Text { text: tail, .. } = inlines.remove(i + 1) else { unreachable!() };
            if let Inline::Text { text, .. } = &mut inlines[i] {
                text.push_str(&tail);
            }
        } else {
            i += 1;
        }
    }

    let mut i = 0;
    while i + 2 < inlines.len() {
        let joinable = match (&inlines[i], &inlines[i + 1], &inlines[i + 2]) {
            (
                Inline::Text { emphasis: before, .. },
                Inline::Text { text: gap, emphasis: gap_emphasis },
                Inline::Text { emphasis: after, .. },
            ) => {
                gap.chars().all(|c| c == ' ')
                    && !gap.is_empty()
                    && gap_emphasis.is_plain()
                    && before == after
                    && !before.is_plain()
            }
            _ => false,
        };
        if !joinable {
            i += 1;
            continue;
        }
        let Inline::Text { text: gap, .. } = inlines.remove(i + 1) else { unreachable!() };
        let Inline::Text { text: tail, .. } = inlines.remove(i + 1) else { unreachable!() };
        if let Inline::Text { text, .. } = &mut inlines[i] {
            text.push_str(&gap);
            text.push_str(&tail);
        }
        // Deliberately not advancing: the run just grown may now be joinable
        // with the one after it, which is the four-bold-words case.
    }
}

/// The style run covering a glyph, if the paragraph recorded one.
fn style_at(paragraph: &Paragraph, line: usize, glyph: usize) -> Option<&Style> {
    paragraph
        .styles
        .iter()
        .find(|r| r.line == line && r.glyphs.start <= glyph && glyph < r.glyphs.end)
        .map(|r| &r.style)
}

/// Emphasis from the PostScript name, which is all this layer has.
///
/// The subset prefix goes first: `ABCDEF+Times-Bold` is a subset of a bold
/// face, and the six random uppercase letters in front of it are not evidence
/// of anything. After that it is substring matching, and it is wrong for any
/// face whose name does not describe its weight — which is why it is counted.
fn emphasis_of(style: &Style, report: &mut Report) -> Emphasis {
    let name = strip_subset_prefix(&style.base_font).to_ascii_lowercase();

    let bold = ["bold", "black", "heavy", "semibold", "demibold", "extrabold"]
        .iter()
        .any(|needle| name.contains(needle));
    let italic = ["italic", "oblique"].iter().any(|needle| name.contains(needle));

    if bold || italic {
        report.note(Guess::EmphasisFromFontName);
    }

    Emphasis {
        bold,
        italic,
        // Mode 3 is "neither fill nor stroke"; 7 is "add to clip and paint
        // nothing". Both put glyphs on the page that no reader sees.
        invisible: style.render_mode == 3 || style.render_mode == 7,
    }
}

pub(crate) fn strip_subset_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase()) {
        &name[7..]
    } else {
        name
    }
}

fn push_text(out: &mut Vec<Inline>, text: &str, emphasis: Emphasis) {
    if text.is_empty() {
        return;
    }
    // Merged into the previous run when the attributes match, so a paragraph of
    // forty glyphs is one inline rather than forty. Consumers iterate these,
    // and a Markdown renderer emitting `**a****b**` would be the visible
    // symptom of not doing it.
    if let Some(Inline::Text { text: prev, emphasis: prev_emphasis }) = out.last_mut() {
        if *prev_emphasis == emphasis {
            prev.push_str(text);
            return;
        }
    }
    out.push(Inline::Text { text: text.to_string(), emphasis });
}

/// Collapse runs of whitespace, across inline boundaries.
fn collapse_spaces(inlines: &mut Vec<Inline>) {
    let mut trailing_space = true; // leading whitespace is trailing whitespace
    for inline in inlines.iter_mut() {
        if let Inline::Text { text, .. } = inline {
            let mut cleaned = String::with_capacity(text.len());
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !trailing_space {
                        cleaned.push(' ');
                        trailing_space = true;
                    }
                } else {
                    cleaned.push(ch);
                    trailing_space = false;
                }
            }
            *text = cleaned;
        } else {
            trailing_space = false;
        }
    }
    // Drop the trailing space and any run left empty by the pass above.
    while let Some(Inline::Text { text, .. }) = inlines.last_mut() {
        while text.ends_with(' ') {
            text.pop();
        }
        if text.is_empty() {
            inlines.pop();
        } else {
            break;
        }
    }
    inlines.retain(|i| !matches!(i, Inline::Text { text, .. } if text.is_empty()));
}

/// The document's body text size: the size most glyphs are set in.
///
/// Modal rather than mean, and weighted by glyph count rather than by
/// paragraph, because a document with forty headings and four hundred lines of
/// prose should measure the prose. Rounded to half a point so that 11.999 and
/// 12.0 are the same size, which they are.
fn body_text_size(model: &DocumentModel) -> Option<f64> {
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for page in &model.pages {
        for (i, block) in page.blocks.iter().enumerate() {
            if !matches!(block, ModelBlock::Paragraph(_)) {
                continue;
            }
            for line in page.lines.get(i).map(Vec::as_slice).unwrap_or(&[]) {
                *counts.entry(quantise(line.size)).or_default() += line.glyphs.len();
            }
        }
    }
    modal(counts)
}

/// The most common size, with ties broken by preferring the smaller.
///
/// The tie-break is not cosmetic. `max_by_key` over a `HashMap` visits in an
/// order that varies between runs, so two sizes with equal glyph counts gave a
/// different body size each time — which changed the heading ladder, which
/// changed whether a paragraph was promoted. I8's determinism check found
/// exactly one corpus file where that happened (`issue11713.pdf`), reporting
/// the same bytes as producing two different models.
///
/// Smaller wins because body text is the smaller of any two sizes it is tied
/// with, and because the failure of guessing wrong in that direction is a
/// heading that was not promoted rather than a paragraph that was.
pub(crate) fn modal(counts: HashMap<u64, usize>) -> Option<f64> {
    counts
        .into_iter()
        .max_by_key(|(size, count)| (*count, std::cmp::Reverse(*size)))
        .map(|(size, _)| size as f64 / 2.0)
}

fn quantise(size: f64) -> u64 {
    (size * 2.0).round().max(0.0) as u64
}

/// The distinct heading sizes present, largest first.
///
/// Built across the whole document rather than per page for the reason
/// `docs/flow-model.md` gives about frames: one page is one sample. A document
/// whose only 18pt text is on page 9 still has an 18pt heading level, and
/// deciding that per page would give it a different level on every page it
/// appears.
fn heading_ladder(model: &DocumentModel, body: Option<f64>, opts: &Options) -> Vec<f64> {
    let Some(body) = body else {
        return Vec::new();
    };
    let mut sizes: Vec<u64> = Vec::new();
    for page in &model.pages {
        for (i, block) in page.blocks.iter().enumerate() {
            let ModelBlock::Paragraph(p) = block else { continue };
            let lines = page.lines.get(i).map(Vec::as_slice).unwrap_or(&[]);
            if !looks_like_heading(p, lines, body, opts) {
                continue;
            }
            let size = quantise(paragraph_size(lines));
            if !sizes.contains(&size) {
                sizes.push(size);
            }
        }
    }
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.into_iter().map(|s| s as f64 / 2.0).collect()
}

fn paragraph_size(lines: &[Line]) -> f64 {
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for line in lines {
        *counts.entry(quantise(line.size)).or_default() += line.glyphs.len().max(1);
    }
    modal(counts).unwrap_or(0.0)
}

/// Positive evidence only: bigger than the body, and short.
///
/// Both halves matter. Size alone promotes the first paragraph of a document
/// set in a large face; shortness alone promotes every one-line paragraph in
/// the document, of which there are many.
fn looks_like_heading(p: &Paragraph, lines: &[Line], body: f64, opts: &Options) -> bool {
    if lines.is_empty() || body <= 0.0 {
        return false;
    }
    let size = paragraph_size(lines);
    if size < body * opts.heading_ratio {
        return false;
    }
    let chars: usize = lines
        .iter()
        .flat_map(|l| l.glyphs.iter())
        .filter_map(|g| g.text.as_deref())
        .map(str::len)
        .sum();
    if chars == 0 || chars > opts.heading_max_chars {
        return false;
    }
    // A justified block is prose. Nobody justifies a title, and a heading
    // detected inside a justified column is a column that was mis-split.
    !matches!(p.alignment, rasura_layout::Alignment::Justified)
}

fn heading_level(p: &Paragraph, lines: &[Line], ladder: &[f64], opts: &Options) -> Option<u8> {
    if ladder.is_empty() {
        return None;
    }
    let body = ladder.last().copied().unwrap_or(0.0);
    // `looks_like_heading` was already applied when the ladder was built, so
    // membership in the ladder is the test here — recomputing the predicate
    // would risk the two disagreeing.
    let _ = (body, opts);
    let size = quantise(paragraph_size(lines));
    let rank = ladder.iter().position(|s| quantise(*s) == size)?;
    if matches!(p.alignment, rasura_layout::Alignment::Justified) {
        return None;
    }
    Some((rank as u8 + 1).min(6))
}

/// Recognise a list marker at the start of a paragraph.
///
/// Returns the list's ordered-ness and the paragraph with the marker removed.
/// Deliberately narrow: a bullet character, or a number or letter followed by
/// `.` or `)`, and in the numbered case a hanging indent as corroboration. A
/// paragraph beginning "1948 was a difficult year" has a number and a full stop
/// nowhere near it, and must not become a list item.
fn list_marker(inlines: &[Inline], p: &Paragraph) -> Option<(bool, Vec<Inline>)> {
    let text = crate::flow::text_of(inlines);
    let trimmed = text.trim_start();

    const BULLETS: [char; 8] = ['•', '·', '‣', '▪', '◦', '–', '—', '*'];
    if let Some(first) = trimmed.chars().next() {
        if BULLETS.contains(&first) {
            let rest = trimmed[first.len_utf8()..].trim_start();
            if rest.is_empty() {
                return None;
            }
            return Some((false, vec![Inline::text(rest)]));
        }
    }

    // A hanging indent is what a numbered list has and a numbered sentence does
    // not: the marker sits left of the text it introduces.
    if p.first_line_indent >= -0.5 {
        return None;
    }
    let mut chars = trimmed.char_indices();
    let mut end = 0;
    let mut seen_digit = false;
    for (i, ch) in chars.by_ref() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = i + 1;
            continue;
        }
        if (ch == '.' || ch == ')') && seen_digit {
            end = i + 1;
        }
        break;
    }
    if !seen_digit || end == 0 || !trimmed[..end].ends_with(['.', ')']) {
        return None;
    }
    let rest = trimmed[end..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((true, vec![Inline::text(rest)]))
}

/// Whether a glyph run ended a line the author meant to end.
///
/// Not used yet, and left here as the named hole rather than as an accidental
/// omission: distinguishing an author's line break from the producer's requires
/// knowing the measure, which is frame inference — step 3 in
/// `docs/flow-model.md`, not step 1.
#[allow(dead_code)]
fn hard_break(_glyphs: &[PlacedGlyph]) -> bool {
    false
}
