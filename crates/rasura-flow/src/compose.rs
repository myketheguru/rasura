//! A flow document and a typeface, to a finished PDF. Spec 9.2.
//!
//! [`crate::emit`] regenerates the pages of a document that already exists, and
//! is fenced behind `accept_regeneration` because doing so breaks §2's first
//! property — an edit stops being local when the whole page is rewritten. That
//! fence makes no sense here. There is no prior file whose bytes could be
//! preserved, so there is nothing to break: everything this writes is new.
//!
//! The pipeline is the one the flow model was built for, run backwards:
//!
//! ```text
//! flow ──layout──▶ placed pages ──▶ content streams ──▶ objects ──▶ bytes
//! ```
//!
//! # Measured with the font that ships
//!
//! Line breaking needs widths, and the widths used here come from the font
//! being embedded rather than from the standard-14 tables — the same `hmtx`
//! that `/Widths` is written from. So a line broken to a measure is the width
//! it draws at, which is what makes the right margin straight. Laying out with
//! one font's metrics and drawing with another's is the ordinary way for
//! composed text to end up overflowing its column.
//!
//! # What is and is not composed
//!
//! Lists are drawn with their markers and a hanging indent, and tables as a grid
//! of equal-width columns with their rules stroked — so neither is an
//! approximation any more. A figure is still placed as its alt text without the
//! image, which is a codec problem this crate does not have; it is counted in
//! [`Report::approximated`] rather than dropped. Equal-width columns are the one
//! simplification that remains in a table: sizing columns to their content is a
//! refinement the rules do not need in order to be right.

use crate::flow::{Block, FlowDocument, Inline};
use crate::layout::{self, Layout, Measurer, TextStyle};
use rasura_cos::Document;
use rasura_cos::object::{Dictionary, Name, ObjId, Object, PdfString};
use rasura_edit::pages::{PageSpec, insert_page};
use rasura_edit::{Canvas, EditSession, Fidelity};
use rasura_font::create::{Embedded, Options as FontOptions, embed_truetype};
use rasura_layout::frames::{FrameSet, PageGeometry};
use std::collections::{BTreeMap, BTreeSet};

/// How to compose.
#[derive(Debug, Clone)]
pub struct Options {
    pub geometry: PageGeometry,
    pub layout: layout::Options,
    /// The name the font gets in `/Resources`. Only visible inside the file.
    pub font_resource: String,
    /// `/Info /Title`, when the document should carry one.
    pub title: Option<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            geometry: PageGeometry::default(),
            layout: layout::Options::default(),
            font_resource: "F1".to_string(),
            title: None,
        }
    }
}

/// What composing did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub pages: usize,
    pub blocks: usize,
    pub lines: usize,
    /// Blocks placed as plain text because this does not draw their structure:
    /// tables without rules, lists without bullets, figures without images.
    pub approximated: usize,
    /// Characters in the flow document the typeface has no glyph for. They are
    /// not drawn — reported rather than substituted, as spec 2 requires.
    pub missing: Vec<char>,
    /// Characters dropped while encoding, which should equal what `missing`
    /// predicts and is counted separately so that it can be checked.
    pub dropped: usize,
    pub base_font: String,
    /// True when a Type0 font was needed.
    pub composite: bool,
    /// Whether the `/StemV` written into the descriptor was estimated.
    pub stem_v_guessed: bool,
    /// The base font of every face embedded, regular first. One entry unless the
    /// document used bold or italic and a family supplied those faces.
    pub faces: Vec<String>,
    /// Faces the document asked for that the family did not supply, so the text
    /// was drawn in the regular face instead. Named rather than faked: a bold run
    /// set in the regular face is a compromise the caller is told about, not a
    /// synthetic bold that would measure wrong.
    pub substituted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ComposeError {
    #[error("the font could not be embedded: {0}")]
    Font(String),
    #[error("the page could not be added: {0}")]
    Page(String),
    #[error("{0}")]
    Cos(String),
    /// A flow document with nothing in it produces a document with no pages,
    /// which is not a PDF anyone can open.
    #[error("there is nothing to compose: the flow document has no text")]
    Empty,
}

/// A family of up to four faces to compose a document in.
///
/// Only `regular` is required. A document that uses bold or italic and whose
/// family supplies those faces is drawn in them; one whose family does not is
/// drawn in the regular face and told so, in [`Report::substituted`] — because
/// faking a bold by widening a regular would make the text overflow its measure
/// rather than admit the face is missing. A document that needs a face embeds
/// it; one that does not pays for nothing it did not use.
#[derive(Debug, Clone, Default)]
pub struct Family {
    pub regular: Vec<u8>,
    pub bold: Option<Vec<u8>>,
    pub italic: Option<Vec<u8>>,
    pub bold_italic: Option<Vec<u8>>,
}

impl Family {
    /// A family of one face, which composes everything in the regular program —
    /// what [`compose`] builds from its single font.
    pub fn regular(program: &[u8]) -> Family {
        Family { regular: program.to_vec(), ..Family::default() }
    }

    /// The program for a `(bold, italic)` request, or `None` when the family does
    /// not supply that face and the caller should fall back to the regular.
    fn program(&self, bold: bool, italic: bool) -> Option<&[u8]> {
        match (bold, italic) {
            (false, false) => Some(&self.regular),
            (true, false) => self.bold.as_deref(),
            (false, true) => self.italic.as_deref(),
            (true, true) => self.bold_italic.as_deref(),
        }
    }
}

/// Compose `flow` into a new document, set in one face, `font_program`.
///
/// The single-face case: every run is drawn in the one program whatever its
/// emphasis. [`compose_family`] draws bold and italic in their own faces.
pub fn compose(
    flow: &FlowDocument,
    font_program: &[u8],
    opts: &Options,
) -> Result<(Document, Report), ComposeError> {
    compose_family(flow, &Family::regular(font_program), opts)
}

/// One embedded face: the program embedded once, and the resource name the
/// content streams reach it by.
struct FaceEmbed {
    resource: String,
    embedded: Embedded,
}

/// A measurer backed by a whole family: each run measured in its own face.
///
/// The reason the layout engine takes a [`Measurer`] rather than one font. A
/// bold run is wider than the same words set regular, and a line of it has to
/// break where bold breaks — laying it out with the regular metrics and drawing
/// it bold is the ordinary way composed text overflows its column.
struct FamilyMeasurer<'a> {
    embeds: &'a [FaceEmbed],
    /// Which embedded face draws a `(bold, italic)` request — a fallback face
    /// points at the regular's index.
    faces: &'a BTreeMap<(bool, bool), usize>,
}

impl FamilyMeasurer<'_> {
    fn face_of(&self, style: TextStyle) -> usize {
        self.faces.get(&(style.bold, style.italic)).copied().unwrap_or(0)
    }
}

impl Measurer for FamilyMeasurer<'_> {
    fn width(&self, text: &str, style: TextStyle) -> f64 {
        self.embeds[self.face_of(style)].embedded.width(text, style.size)
    }
}

/// The `(bold, italic)` faces the document's runs call for.
///
/// Read from the flow, not the placement, so it can be known before anything is
/// embedded — which is what breaks the circle of "measure to lay out, lay out to
/// know the faces, embed the faces to measure". A heading is bold by the layout's
/// own rule, so its runs count as bold here too. Regular is always present: list
/// markers, table cells and the fallback all need it.
fn used_faces(flow: &FlowDocument) -> BTreeSet<(bool, bool)> {
    let mut set = BTreeSet::new();
    set.insert((false, false));
    for block in &flow.blocks {
        let (base_bold, inlines) = match block {
            Block::Heading { inlines, .. } => (true, inlines),
            Block::Paragraph { inlines, .. } => (false, inlines),
            _ => continue,
        };
        for inline in inlines {
            if let Inline::Text { emphasis, .. } = inline {
                set.insert((base_bold || emphasis.bold, emphasis.italic));
            }
        }
    }
    set
}

/// Compose `flow` into a new document set in `family`, drawing bold and italic
/// in their own faces where the family supplies them. Spec 9.2.
pub fn compose_family(
    flow: &FlowDocument,
    family: &Family,
    opts: &Options,
) -> Result<(Document, Report), ComposeError> {
    // Every character the document will draw, so each face's subset holds exactly
    // that. Whitespace is included deliberately: a font with no space glyph still
    // needs the code to advance, and `for_text` keeps it. Each face embeds the
    // whole set rather than only the characters that happen to appear in its runs
    // — a face short one glyph would drop it silently, and the saving is not worth
    // that risk.
    let mut characters: BTreeSet<char> = flow
        .blocks
        .iter()
        .flat_map(|b| b.text().chars().collect::<Vec<_>>())
        .filter(|c| !c.is_control())
        .collect();
    if characters.is_empty() {
        return Err(ComposeError::Empty);
    }
    characters.extend(marker_chars(flow));

    let mut doc = Document::new();

    // Embed the regular face first, then each other used face the family
    // supplies. A used face it does not supply falls back to the regular, which
    // is already embedded, so nothing is embedded twice.
    let mut embeds: Vec<FaceEmbed> = Vec::new();
    let mut faces: BTreeMap<(bool, bool), usize> = BTreeMap::new();
    let mut substituted: Vec<String> = Vec::new();

    let embed = |doc: &mut Document, program: &[u8]| -> Result<Embedded, ComposeError> {
        let next = || doc.reserve(1)[0];
        embed_truetype(program, &FontOptions { characters: characters.clone(), ..FontOptions::default() }, next)
            .map_err(|e| ComposeError::Font(e.to_string()))
    };

    let regular = embed(&mut doc, &family.regular)?;
    embeds.push(FaceEmbed { resource: opts.font_resource.clone(), embedded: regular });
    faces.insert((false, false), 0);

    for &(bold, italic) in used_faces(flow).iter() {
        if (bold, italic) == (false, false) {
            continue;
        }
        match family.program(bold, italic) {
            Some(program) => {
                let embedded = embed(&mut doc, program)?;
                let idx = embeds.len();
                // Derived from the regular's name, so the names cannot collide:
                // F1 -> F1B, F1I, F1BI.
                let suffix = match (bold, italic) {
                    (true, true) => "BI",
                    (true, false) => "B",
                    (false, true) => "I",
                    (false, false) => "",
                };
                embeds.push(FaceEmbed {
                    resource: format!("{}{suffix}", opts.font_resource),
                    embedded,
                });
                faces.insert((bold, italic), idx);
            }
            None => {
                // The family has no such face: draw it regular and say so.
                faces.insert((bold, italic), 0);
                substituted.push(face_name(bold, italic));
            }
        }
    }

    // Laid out with the family that will draw it, so each run breaks in its face.
    let frames = FrameSet::designed(&opts.geometry);
    let measurer = FamilyMeasurer { embeds: &embeds, faces: &faces };
    let (placed, _) = layout::layout(flow, &frames, &opts.layout, &measurer);
    if placed.blocks.is_empty() {
        return Err(ComposeError::Empty);
    }

    let mut missing: BTreeSet<char> = BTreeSet::new();
    for f in &embeds {
        missing.extend(f.embedded.missing.iter().copied());
    }

    let mut report = Report {
        pages: placed.pages,
        blocks: placed.blocks.len(),
        // Lists have their markers and tables their rules now, so neither is
        // approximated; a figure, drawn without its image, still is.
        approximated: placed
            .blocks
            .iter()
            .filter(|b| {
                flow.blocks.get(b.source).is_some_and(|s| matches!(s, Block::Figure { .. }))
            })
            .count(),
        missing: missing.into_iter().collect(),
        base_font: embeds[0].embedded.base_font.clone(),
        composite: embeds.iter().any(|f| f.embedded.composite),
        stem_v_guessed: embeds.iter().any(|f| f.embedded.description.stem_v_guessed),
        faces: embeds.iter().map(|f| f.embedded.base_font.clone()).collect(),
        substituted,
        ..Report::default()
    };

    // Every face must be reachable from every page by the name its runs use. A
    // content stream naming a resource the page does not define draws nothing,
    // and does it silently.
    let mut fonts = Dictionary::new();
    for f in &embeds {
        fonts.insert(f.resource.as_str(), Object::Reference(f.embedded.font));
    }
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));

    let height = opts.geometry.size.1;
    let media_box = [0.0, 0.0, opts.geometry.size.0, height];
    let changes: Vec<(ObjId, Option<Object>)> = embeds
        .iter()
        .flat_map(|f| f.embedded.objects.iter().map(|(id, o)| (*id, Some(o.clone()))))
        .collect();

    for page in 0..placed.pages {
        let (content, lines, dropped) = draw_page(&placed, page, height, &embeds, &faces);
        report.lines += lines;
        report.dropped += dropped;

        let spec = PageSpec { media_box, content, resources: Some(resources.clone()) };
        // Appended one after another: `insert_page` past the end appends, and
        // the tree is empty for the first, which is the case `Document::new`
        // exists to make reachable.
        let tree =
            rasura_content::page::pages(&doc).map_err(|e| ComposeError::Cos(e.to_string()))?;
        let edit = insert_page(&mut doc, &tree, page, &spec)
            .map_err(|e| ComposeError::Page(e.to_string()))?;

        // Applied immediately rather than accumulated: the next iteration reads
        // the page tree back to find where to append, so it has to see this one.
        let mut session = EditSession::new(&mut doc);
        session
            .set_objects("add page", &edit.changes, edit.fidelity.clone())
            .map_err(|e| ComposeError::Cos(e.to_string()))?;
    }

    {
        let mut session = EditSession::new(&mut doc);
        session
            .set_objects("embed fonts", &changes, Fidelity::Exact)
            .map_err(|e| ComposeError::Cos(e.to_string()))?;
    }

    if let Some(title) = &opts.title {
        let mut info = Dictionary::new();
        info.insert("Title", Object::String(PdfString::new_literal(title.as_bytes())));
        let id = doc.add(Object::Dictionary(info));
        doc.set_info(id);
    }

    Ok((doc, report))
}

/// A human name for a `(bold, italic)` face, for the substitution report.
fn face_name(bold: bool, italic: bool) -> String {
    match (bold, italic) {
        (true, true) => "bold italic".to_string(),
        (true, false) => "bold".to_string(),
        (false, true) => "italic".to_string(),
        (false, false) => "regular".to_string(),
    }
}

/// One page's content stream, drawing each run of each line in its own face.
fn draw_page(
    placed: &Layout,
    page: usize,
    height: f64,
    embeds: &[FaceEmbed],
    faces: &BTreeMap<(bool, bool), usize>,
) -> (Vec<u8>, usize, usize) {
    let mut canvas = Canvas::new(rasura_edit::numfmt::NumberStyle::default());
    let mut lines = 0;
    let mut dropped = 0;

    // The embedded face a run's style is drawn in — a fallback points at the
    // regular, which is index 0.
    let face_of = |style: &TextStyle| -> usize {
        faces.get(&(style.bold, style.italic)).copied().unwrap_or(0)
    };

    canvas.fill_gray(0.0);
    for block in placed.blocks.iter().filter(|b| b.page == page) {
        for line in &block.lines {
            // The layout engine measures downward from the top of the page and
            // PDF space runs upward, so the baseline is the page height less
            // the line's top and one em -- which is where a 1.2 leading puts
            // it.
            let baseline = height - (line.rect.y0 + line.style.size);

            // A list item's marker, drawn in the gutter to the left of the text,
            // in the regular face. It sits on the same baseline but at its own x,
            // and is drawn even when the item's text is empty — the marker is the
            // content then.
            if let Some(marker) = &line.marker {
                let face = &embeds[0];
                let (codes, lost) = face.embedded.encode(&marker.text);
                dropped += lost;
                if !codes.is_empty() {
                    let name = Name::new(&face.resource);
                    canvas.text_line(&name, line.style.size, marker.x, baseline, &codes);
                }
            }

            // Each run drawn in its face, advancing x by the width it draws at so
            // the next run begins exactly where this one ends.
            let mut x = line.rect.x0;
            let mut drew = false;
            for run in &line.runs {
                let face = &embeds[face_of(&run.style)];
                let advance = face.embedded.width(&run.text, run.style.size);
                let (codes, lost) = face.embedded.encode(&run.text);
                dropped += lost;
                if !codes.is_empty() {
                    let name = Name::new(&face.resource);
                    canvas.text_line(&name, run.style.size, x, baseline, &codes);
                    drew = true;
                }
                x += advance;
            }
            if drew {
                lines += 1;
            }
        }

        // A table's rules, stroked after its cell text. The grid is in the
        // layout's downward space, so each y flips into PDF's upward space.
        if let Some(grid) = &block.grid {
            draw_grid(&mut canvas, grid, height);
        }
    }
    (canvas.finish().unwrap_or_default(), lines, dropped)
}

/// Stroke a table's rules: a border, a rule between every pair of rows and of
/// columns, and a heavier one beneath a header row.
fn draw_grid(canvas: &mut Canvas, grid: &crate::layout::Grid, height: f64) {
    let (Some(&left), Some(&right)) = (grid.columns.first(), grid.columns.last()) else {
        return;
    };
    let (Some(&first_row), Some(&last_row)) = (grid.rows.first(), grid.rows.last()) else {
        return;
    };

    canvas.stroke_rgb(0.0, 0.0, 0.0);

    // Horizontal rules, top border to bottom. The rule under a header row is
    // drawn heavier, which is the one piece of table styling worth the bytes.
    for (i, &y) in grid.rows.iter().enumerate() {
        canvas.line_width(if grid.header && i == 1 { 1.0 } else { 0.5 });
        canvas.move_to(left, height - y);
        canvas.line_to(right, height - y);
        canvas.stroke();
    }

    // Vertical rules, left border to right.
    canvas.line_width(0.5);
    for &x in &grid.columns {
        canvas.move_to(x, height - first_row);
        canvas.line_to(x, height - last_row);
        canvas.stroke();
    }
}

/// The characters list markers draw with, which no block's text contains.
///
/// `•` for any unordered list; the digits and `.` an ordered list numbers with.
/// Gathered so the embedded subset holds them — see where it is extended.
fn marker_chars(flow: &FlowDocument) -> BTreeSet<char> {
    let mut out = BTreeSet::new();
    for block in &flow.blocks {
        if let Block::List(list) = block {
            if list.ordered {
                out.extend('0'..='9');
                out.insert('.');
            } else {
                out.insert('•');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::Inline;

    fn roboto() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/Roboto-Regular.ttf"))
            .ok()
    }

    fn document(paragraphs: usize) -> FlowDocument {
        let mut blocks = vec![Block::Heading {
            level: 1,
            inlines: vec![Inline::text("Composed")],
            source: None,
        }];
        for i in 0..paragraphs {
            blocks.push(Block::Paragraph {
                inlines: vec![Inline::text(format!(
                    "Paragraph {i} of a document that did not exist until this ran. It is long \
                     enough to break across more than one line, because a composition that never \
                     wraps has not exercised the thing most likely to be wrong."
                ))],
                source: None,
            });
        }
        FlowDocument { blocks, ..FlowDocument::default() }
    }

    #[test]
    fn a_flow_document_becomes_a_pdf() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let (doc, report) = compose(&document(3), &font, &Options::default()).unwrap();

        assert!(report.lines > 3, "{report:?}");
        assert_eq!(report.dropped, 0, "plain Latin text drops nothing");
        assert!(report.missing.is_empty(), "{:?}", report.missing);
        assert!(!report.composite, "Latin fits a simple font");

        // Through the reader, and then through a reader on the saved bytes,
        // which is the only claim worth making.
        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).expect("what was composed reopens");
        assert_eq!(reopened.leniencies(), Vec::new());

        let tree = rasura_content::page::pages(&reopened).unwrap();
        assert_eq!(tree.pages.len(), report.pages);
        let text = rasura_layout::page_text(&reopened, &tree.pages[0]);
        assert!(text.contains("Composed"), "{text:?}");
    }

    #[test]
    fn text_longer_than_a_page_paginates() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        // Enough prose to need a second page. The count is not asserted
        // exactly -- it depends on the measure -- but more than one is the
        // whole point, and every page must be real.
        let (doc, report) = compose(&document(40), &font, &Options::default()).unwrap();
        assert!(report.pages > 1, "{} page(s)", report.pages);

        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).unwrap();
        let tree = rasura_content::page::pages(&reopened).unwrap();
        assert_eq!(tree.pages.len(), report.pages);
        for (i, page) in tree.pages.iter().enumerate() {
            let text = rasura_layout::page_text(&reopened, page);
            assert!(!text.trim().is_empty(), "page {i} came out blank");
        }
    }

    #[test]
    fn two_columns_are_narrower_than_one() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let wide = compose(&document(6), &font, &Options::default()).unwrap().1;
        let narrow = compose(
            &document(6),
            &font,
            &Options { geometry: PageGeometry::us_letter().with_columns(2), ..Options::default() },
        )
        .unwrap()
        .1;

        // Same text, half the measure: more lines. If the geometry were being
        // ignored the two would match exactly, which is the failure this
        // catches.
        assert!(narrow.lines > wide.lines, "{} vs {}", narrow.lines, wide.lines);
    }

    #[test]
    fn a_character_the_font_lacks_is_reported() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let flow = FlowDocument {
            blocks: vec![Block::Paragraph {
                inlines: vec![Inline::text("Roboto has no 中文 glyphs.")],
                source: None,
            }],
            ..FlowDocument::default()
        };
        let (_, report) = compose(&flow, &font, &Options::default()).unwrap();
        assert_eq!(report.missing, vec!['中', '文']);
        assert_eq!(report.dropped, 2, "and they are dropped, not substituted");
    }

    fn list(ordered: bool, items: &[&str]) -> Block {
        use crate::flow::{Item, List};
        Block::List(List {
            ordered,
            items: items
                .iter()
                .map(|t| Item {
                    blocks: vec![Block::Paragraph {
                        inlines: vec![Inline::text(*t)],
                        source: None,
                    }],
                })
                .collect(),
            source: None,
        })
    }

    #[test]
    fn a_list_is_composed_with_its_markers_and_is_not_approximated() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let flow = FlowDocument {
            blocks: vec![
                Block::Heading { level: 1, inlines: vec![Inline::text("Shopping")], source: None },
                list(false, &["apples", "pears", "fresh bread"]),
                list(true, &["wake", "work", "rest"]),
            ],
            ..FlowDocument::default()
        };
        let (doc, report) = compose(&flow, &font, &Options::default()).unwrap();

        // A list is drawn with its markers now, not flattened to text and counted
        // as an approximation.
        assert_eq!(report.approximated, 0, "both lists are composed, not approximated");
        // The marker glyphs are in the subset, so nothing is dropped.
        assert_eq!(report.dropped, 0, "{:?}", report.missing);
        assert!(report.missing.is_empty(), "{:?}", report.missing);

        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).expect("what was composed reopens");
        let tree = rasura_content::page::pages(&reopened).unwrap();
        let text = rasura_layout::page_text(&reopened, &tree.pages[0]);
        assert!(text.contains("apples") && text.contains("fresh bread"), "{text:?}");
        // The ordered list's numbers were drawn onto the page, in sequence.
        assert!(text.contains("1.") && text.contains("3."), "numbered markers drawn: {text:?}");
    }

    #[test]
    fn a_table_is_composed_with_rules_and_is_not_approximated() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        use crate::flow::{Cell, Table};
        let cell = |t: &str| Cell {
            blocks: vec![Block::Paragraph { inlines: vec![Inline::text(t)], source: None }],
        };
        let table = Block::Table(Table {
            rows: vec![
                vec![cell("Item"), cell("Price")],
                vec![cell("Apples"), cell("1.50")],
                vec![cell("Bread"), cell("2.00")],
            ],
            has_header: true,
            source: None,
        });
        let flow = FlowDocument { blocks: vec![table], ..FlowDocument::default() };
        let (doc, report) = compose(&flow, &font, &Options::default()).unwrap();

        assert_eq!(report.approximated, 0, "a table is drawn with rules, not approximated");
        assert_eq!(report.dropped, 0, "{:?}", report.missing);

        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).expect("what was composed reopens");
        // The stroked rules did not malform the content stream.
        assert_eq!(reopened.leniencies(), Vec::new());

        let tree = rasura_content::page::pages(&reopened).unwrap();
        let text = rasura_layout::page_text(&reopened, &tree.pages[0]);
        for word in ["Item", "Price", "Apples", "2.00"] {
            assert!(text.contains(word), "cell {word:?} missing from page: {text:?}");
        }
    }

    fn roboto_face(name: &str) -> Option<Vec<u8>> {
        std::fs::read(format!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/Roboto-{}.ttf"),
            name
        ))
        .ok()
    }

    fn emphasised_paragraph() -> FlowDocument {
        use crate::flow::{Emphasis, Inline};
        let bold = Emphasis { bold: true, ..Emphasis::default() };
        let italic = Emphasis { italic: true, ..Emphasis::default() };
        FlowDocument {
            blocks: vec![Block::Paragraph {
                inlines: vec![
                    Inline::text("A line with a "),
                    Inline::Text { text: "bold".into(), emphasis: bold },
                    Inline::text(" word and an "),
                    Inline::Text { text: "italic".into(), emphasis: italic },
                    Inline::text(" word."),
                ],
                source: None,
            }],
            ..FlowDocument::default()
        }
    }

    #[test]
    fn bold_and_italic_are_drawn_in_their_own_faces() {
        let (Some(regular), Some(bold), Some(italic)) =
            (roboto_face("Regular"), roboto_face("Bold"), roboto_face("Italic"))
        else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let family = Family { regular, bold: Some(bold), italic: Some(italic), bold_italic: None };
        let (doc, report) =
            compose_family(&emphasised_paragraph(), &family, &Options::default()).unwrap();

        assert_eq!(report.dropped, 0, "{:?}", report.missing);
        assert!(report.substituted.is_empty(), "the family supplies every face used: {:?}", report.substituted);
        assert_eq!(report.faces.len(), 3, "regular, bold and italic embedded: {:?}", report.faces);

        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).expect("what was composed reopens");
        assert_eq!(reopened.leniencies(), Vec::new(), "three faces, still well-formed");
        let tree = rasura_content::page::pages(&reopened).unwrap();
        let text = rasura_layout::page_text(&reopened, &tree.pages[0]);
        assert!(text.contains("bold") && text.contains("italic"), "{text:?}");
    }

    #[test]
    fn a_face_the_family_lacks_is_substituted_and_reported() {
        let Some(regular) = roboto_face("Regular") else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        // Regular only: the bold and italic runs have nowhere to go but the
        // regular face, and that compromise is named rather than hidden.
        let (_, report) =
            compose_family(&emphasised_paragraph(), &Family::regular(&regular), &Options::default())
                .unwrap();
        assert_eq!(report.faces.len(), 1, "only the regular face is embedded: {:?}", report.faces);
        assert!(report.substituted.contains(&"bold".to_string()), "{:?}", report.substituted);
        assert!(report.substituted.contains(&"italic".to_string()), "{:?}", report.substituted);
        // Substituted, not dropped: the words are still drawn.
        assert_eq!(report.dropped, 0, "{:?}", report.missing);
    }

    #[test]
    fn nothing_to_compose_is_an_error_rather_than_a_pageless_pdf() {
        let Some(font) = roboto() else { return };
        let empty = FlowDocument::default();
        let refused = compose(&empty, &font, &Options::default());
        assert!(matches!(refused, Err(ComposeError::Empty)), "{:?}", refused.map(|(_, r)| r));
    }
}
