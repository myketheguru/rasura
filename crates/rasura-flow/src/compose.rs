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
//! # What is not composed
//!
//! Tables, lists and figures are placed as their text: the layout engine gives
//! them a rectangle and this draws the lines in it, without rules, bullets or
//! images. They are not silently dropped — [`Report::approximated`] counts
//! them — but a caller wanting a real table wants more than this does.

use crate::flow::{Block, FlowDocument};
use crate::layout::{self, Layout};
use rasura_cos::Document;
use rasura_cos::object::{Dictionary, Name, ObjId, Object, PdfString};
use rasura_edit::pages::{PageSpec, insert_page};
use rasura_edit::{Canvas, EditSession, Fidelity};
use rasura_font::create::{Embedded, Options as FontOptions, embed_truetype};
use rasura_layout::frames::{FrameSet, PageGeometry};
use std::collections::BTreeSet;

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

/// Compose `flow` into a new document, set in `font_program`.
pub fn compose(
    flow: &FlowDocument,
    font_program: &[u8],
    opts: &Options,
) -> Result<(Document, Report), ComposeError> {
    // Every character the document will draw, so the subset holds exactly that
    // and nothing else. Whitespace is included deliberately: a font with no
    // space glyph still needs the code to advance, and `for_text` keeps it.
    let characters: BTreeSet<char> = flow
        .blocks
        .iter()
        .flat_map(|b| b.text().chars().collect::<Vec<_>>())
        .filter(|c| !c.is_control())
        .collect();
    if characters.is_empty() {
        return Err(ComposeError::Empty);
    }

    let mut doc = Document::new();
    let embedded = {
        let next = || doc.reserve(1)[0];
        embed_truetype(font_program, &FontOptions { characters, ..FontOptions::default() }, next)
            .map_err(|e| ComposeError::Font(e.to_string()))?
    };

    // Laid out with the font that will draw it. This is the whole reason
    // `Embedded` implements `Measurer`.
    let frames = FrameSet::designed(&opts.geometry);
    let (placed, _) = layout::layout(flow, &frames, &opts.layout, &embedded);
    if placed.blocks.is_empty() {
        return Err(ComposeError::Empty);
    }

    let mut report = Report {
        pages: placed.pages,
        blocks: placed.blocks.len(),
        approximated: placed
            .blocks
            .iter()
            .filter(|b| {
                flow.blocks.get(b.source).is_some_and(|s| {
                    matches!(s, Block::Table(_) | Block::List(_) | Block::Figure { .. })
                })
            })
            .count(),
        missing: embedded.missing.clone(),
        base_font: embedded.base_font.clone(),
        composite: embedded.composite,
        stem_v_guessed: embedded.description.stem_v_guessed,
        ..Report::default()
    };

    // The font must be reachable from every page by the name the content
    // streams use. A content stream naming a resource the page does not define
    // draws nothing, and does it silently.
    let mut fonts = Dictionary::new();
    fonts.insert(opts.font_resource.as_str(), Object::Reference(embedded.font));
    let mut resources = Dictionary::new();
    resources.insert("Font", Object::Dictionary(fonts));

    let height = opts.geometry.size.1;
    let media_box = [0.0, 0.0, opts.geometry.size.0, height];
    let changes: Vec<(ObjId, Option<Object>)> =
        embedded.objects.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();

    for page in 0..placed.pages {
        let (content, lines, dropped) = draw_page(&placed, page, height, &embedded, opts);
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
            .set_objects("embed font", &changes, Fidelity::Exact)
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

/// One page's content stream.
fn draw_page(
    placed: &Layout,
    page: usize,
    height: f64,
    embedded: &Embedded,
    opts: &Options,
) -> (Vec<u8>, usize, usize) {
    let font = Name::new(&opts.font_resource);
    let mut canvas = Canvas::new(rasura_edit::numfmt::NumberStyle::default());
    let mut lines = 0;
    let mut dropped = 0;

    canvas.fill_gray(0.0);
    for block in placed.blocks.iter().filter(|b| b.page == page) {
        for line in &block.lines {
            let (codes, lost) = embedded.encode(&line.text);
            dropped += lost;
            if codes.is_empty() {
                continue;
            }
            // The layout engine measures downward from the top of the page and
            // PDF space runs upward, so the baseline is the page height less
            // the line's top and one em -- which is where a 1.2 leading puts
            // it.
            let baseline = height - (line.rect.y0 + line.style.size);
            canvas.text_line(&font, line.style.size, line.rect.x0, baseline, &codes);
            lines += 1;
        }
    }
    (canvas.finish().unwrap_or_default(), lines, dropped)
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

    #[test]
    fn nothing_to_compose_is_an_error_rather_than_a_pageless_pdf() {
        let Some(font) = roboto() else { return };
        let empty = FlowDocument::default();
        let refused = compose(&empty, &font, &Options::default());
        assert!(matches!(refused, Err(ComposeError::Empty)), "{:?}", refused.map(|(_, r)| r));
    }
}
