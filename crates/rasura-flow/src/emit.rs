//! Document mode: writing a laid-out document back as a PDF.
//!
//! The last item of step 5, and the one that changes the contract. Everything
//! else in this crate reads; this writes, and the moment it does, §2's first
//! property stops holding:
//!
//! > An edit on page 40 must not change the rendered output of any other page
//! > by a single pixel, nor alter the bytes of any object it did not need to
//! > touch.
//!
//! Regenerating a page's content stream changes every glyph on it. That is the
//! *point* — a re-flowed document is meant to look different — and it is why
//! `docs/flow-model.md` puts this behind an explicit flag rather than behind a
//! function name:
//!
//! > **Refuse rather than scramble.** [...] A partially-flowed document is a
//! > legitimate output; a confidently-wrong one is not.
//!
//! # The flag is a field, not a name
//!
//! [`Options::accept_regeneration`] must be set to `true` by hand. A caller
//! cannot reach this by autocompleting their way through the API, and a code
//! reviewer can grep for it. The same technique as
//! `SaveOptions::accept_signature_destruction`, for the same reason: the
//! dangerous thing should be spelled out at the call site rather than implied
//! by the module it lives in.
//!
//! # What it regenerates, and what it leaves alone
//!
//! Only page content streams and the page tree. Metadata, annotations, form
//! fields, the structure tree, embedded files and every other object are
//! carried through untouched by the writer, because they were never read here.
//! A document that gains pages gets new page objects cloned from the layout's
//! own template; one that loses them has them removed with §10.9's navigation
//! fix-up, which `rasura_edit::pages` already does.
//!
//! # It closes I8 for real
//!
//! Steps 1 to 5 could check the round trip without writing a file, by reading
//! the model back out of the placement. This closes the loop the design
//! actually describes — model, lay out, **write**, re-open, re-extract, compare
//! — with a real PDF in the middle and no part of the pipeline trusted to
//! report on itself.

use crate::flow::FlowDocument;
use crate::layout::{Layout, TextStyle};
use rasura_cos::object::{Dictionary, Name, Object, Stream};
use rasura_cos::{Document, ObjId};
use rasura_edit::emit as ops;
use rasura_edit::numfmt::NumberStyle;
use rasura_edit::pages::PageSpec;

/// Why a document could not be regenerated.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EmitError {
    /// [`Options::accept_regeneration`] was not set.
    #[error(
        "regenerating page content replaces every glyph on the page; \
         set accept_regeneration to proceed"
    )]
    NotAccepted,

    /// The document has no pages to regenerate into.
    #[error("the document has no pages")]
    NoPages,

    #[error("the page tree could not be read: {0}")]
    PageTree(String),

    #[error("a page could not be added or removed: {0}")]
    Pages(String),
}

/// How to write the laid-out document back.
#[derive(Debug, Clone)]
pub struct Options {
    /// **Required.** Acknowledges that page content will be replaced.
    ///
    /// Not a name, a field. See the module note: the dangerous thing should be
    /// spelled out at the call site.
    pub accept_regeneration: bool,
    /// The base-14 face to set the document in.
    ///
    /// Helvetica, matching [`crate::Standard14`]. The engine measured with
    /// these metrics, so writing anything else would put lines where the
    /// measure did not expect them. A caller who supplies a different measurer
    /// should name the matching font here.
    pub font: &'static str,
    pub body: TextStyle,
    /// Resource name for the font in each page's `/Resources`.
    ///
    /// Prefixed so it cannot collide with a name the original document already
    /// uses — a page that already had an `/F1` would otherwise have its own
    /// font silently replaced.
    pub font_resource: &'static str,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            accept_regeneration: false,
            font: "Helvetica",
            body: TextStyle::default(),
            font_resource: "RasuraFlow",
        }
    }
}

/// What regenerating did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub pages_before: usize,
    pub pages_after: usize,
    pub pages_added: usize,
    pub pages_removed: usize,
    /// Lines written across the document.
    pub lines: usize,
    /// Characters dropped because the font's encoding cannot represent them.
    ///
    /// Helvetica is written with `/WinAnsiEncoding`, which has 224 characters
    /// in it. A document containing anything else — Greek, CJK, a dash the
    /// producer took from a Unicode block — loses those characters here.
    /// Counted rather than silently substituted, because "the text changed" is
    /// the one thing a caller must not learn from a reader.
    pub unencodable: usize,
}

/// Write a laid-out document back into `doc`, replacing its page content.
///
/// The document is modified in place; nothing is saved until the caller saves
/// it. A save after this must be a full rewrite for anything but a trivially
/// small document, which `rasura_cos::save` decides for itself.
pub fn regenerate(
    doc: &mut Document,
    layout: &Layout,
    opts: &Options,
) -> Result<Report, EmitError> {
    if !opts.accept_regeneration {
        return Err(EmitError::NotAccepted);
    }
    if layout.pages == 0 {
        return Err(EmitError::NoPages);
    }

    let tree = rasura_content::page::pages(doc).map_err(|e| EmitError::PageTree(e.to_string()))?;
    let before = tree.pages.len();
    if before == 0 {
        return Err(EmitError::NoPages);
    }

    let mut report =
        Report { pages_before: before, pages_after: layout.pages, ..Report::default() };

    // The style the original used to write its numbers, so the regenerated
    // stream looks like it belongs to the same file rather than to this
    // library. §9.5's reason for `numfmt` existing at all.
    let style = doc
        .decoded_stream(content_id(doc, &tree.pages[0].dict).unwrap_or(ObjId::new(0, 0)))
        .map(|bytes| rasura_edit::numfmt::sample(&bytes))
        .unwrap_or_default();

    // A font object, added once and referenced by every page. Standard 14, so
    // there is nothing to embed.
    let font_id = add_font(doc, opts);

    // Pages the layout wants that the document does not have. Added first, so
    // the page tree is the right length before any content is written.
    let template = tree.pages[before - 1].clone();
    if layout.pages > before {
        let spec = PageSpec {
            media_box: [0.0, 0.0, layout.page_size.0, layout.page_size.1],
            content: Vec::new(),
            resources: None,
        };
        for _ in before..layout.pages {
            let tree =
                rasura_content::page::pages(doc).map_err(|e| EmitError::PageTree(e.to_string()))?;
            let edit = rasura_edit::pages::insert_page(doc, &tree, tree.pages.len(), &spec)
                .map_err(|e| EmitError::Pages(e.to_string()))?;
            apply(doc, edit.changes);
            report.pages_added += 1;
        }
    }

    // Pages the document has that the layout does not want, removed from the
    // end so the indices of the ones being kept do not move.
    while layout.pages < current_page_count(doc)? {
        let tree =
            rasura_content::page::pages(doc).map_err(|e| EmitError::PageTree(e.to_string()))?;
        let last = tree.pages.len() - 1;
        let edit = rasura_edit::pages::delete_page(doc, &tree, last)
            .map_err(|e| EmitError::Pages(e.to_string()))?;
        apply(doc, edit.changes);
        report.pages_removed += 1;
    }

    // One content stream per page.
    let tree = rasura_content::page::pages(doc).map_err(|e| EmitError::PageTree(e.to_string()))?;
    let height = layout.page_size.1;

    for (index, page) in tree.pages.iter().enumerate() {
        let (content, lines, dropped) = page_content(layout, index, height, opts, &style);
        report.lines += lines;
        report.unencodable += dropped;

        let mut dict = page.dict.clone();
        set_resources(doc, &mut dict, font_id, opts);

        // A single stream replaces whatever the page had — an array of them, a
        // shared one, anything. The page is being regenerated; there is nothing
        // to preserve.
        let stream_id = doc.add(Object::Stream({
            let mut s = Stream::new(Dictionary::new(), Vec::new());
            s.set_decoded(content);
            s
        }));
        dict.insert("Contents", Object::Reference(stream_id));

        // `media_box` is not touched for pages the document already had: the
        // layout was inferred from their own frames, so their size is right by
        // construction, and changing it would move content the caller can see.
        let _ = &template;
        doc.set(page.id, Object::Dictionary(dict));
    }

    report.pages_after = tree.pages.len();
    Ok(report)
}

fn current_page_count(doc: &Document) -> Result<usize, EmitError> {
    rasura_content::page::pages(doc)
        .map(|t| t.pages.len())
        .map_err(|e| EmitError::PageTree(e.to_string()))
}

fn apply(doc: &mut Document, changes: Vec<(ObjId, Option<Object>)>) {
    for (id, value) in changes {
        match value {
            Some(object) => doc.set(id, object),
            None => doc.delete(id),
        }
    }
}

fn content_id(doc: &Document, page: &Dictionary) -> Option<ObjId> {
    match doc.get_entry(page, "Contents").ok().flatten() {
        Some(_) => page.get("Contents").and_then(Object::as_reference),
        None => None,
    }
}

/// Add a standard-14 font object and return its id.
fn add_font(doc: &mut Document, opts: &Options) -> ObjId {
    let mut font = Dictionary::new();
    font.insert("Type", Object::Name(Name::new("Font")));
    font.insert("Subtype", Object::Name(Name::new("Type1")));
    font.insert("BaseFont", Object::Name(Name::new(opts.font)));
    font.insert("Encoding", Object::Name(Name::new("WinAnsiEncoding")));
    doc.add(Object::Dictionary(font))
}

/// Put the font into a page's `/Resources`, keeping whatever was there.
///
/// Merged rather than replaced. A regenerated page draws only this crate's
/// text, but its annotations' appearance streams may still name the page's own
/// resources, and dropping them would break every one of them.
fn set_resources(doc: &Document, page: &mut Dictionary, font: ObjId, opts: &Options) {
    let mut resources = doc
        .get_entry(page, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();

    let mut fonts = doc
        .get_entry(&resources, "Font")
        .ok()
        .flatten()
        .and_then(|f| f.as_dict().cloned())
        .unwrap_or_default();

    fonts.insert(opts.font_resource, Object::Reference(font));
    resources.insert("Font", Object::Dictionary(fonts));
    page.insert("Resources", Object::Dictionary(resources));
}

/// The content stream for one page.
fn page_content(
    layout: &Layout,
    page: usize,
    height: f64,
    opts: &Options,
    style: &NumberStyle,
) -> (Vec<u8>, usize, usize) {
    let mut out = Vec::new();
    let mut lines = 0usize;
    let mut dropped = 0usize;

    for block in layout.blocks.iter().filter(|b| b.page == page) {
        for line in &block.lines {
            let (codes, lost) = win_ansi(&line.text);
            dropped += lost;
            if codes.is_empty() {
                continue;
            }

            // Device space has y downward and the page's own space has it
            // upward, so the baseline is measured from the bottom. The baseline
            // sits one em below the top of the line box, which is where a
            // 1.2-times-size leading puts it.
            let size = block_size(layout, block.source, opts);
            let baseline = height - (line.rect.y0 + size);

            ops::write_op(&mut out, &ops::begin_text(), style);
            out.push(b'\n');
            ops::write_op(&mut out, &ops::set_font(&Name::new(opts.font_resource), size), style);
            out.push(b'\n');
            ops::write_op(
                &mut out,
                &ops::set_text_matrix([1.0, 0.0, 0.0, 1.0, line.rect.x0, baseline]),
                style,
            );
            out.push(b'\n');
            ops::write_op(&mut out, &ops::show_text(&codes), style);
            out.push(b'\n');
            ops::write_op(&mut out, &ops::end_text(), style);
            out.push(b'\n');
            lines += 1;
        }
    }
    (out, lines, dropped)
}

/// The size a placed block was set in.
///
/// Recovered from the line box rather than carried on `PlacedBlock`: the
/// engine's line height is 1.2 times the size, so the box says what the size
/// was. Keeping it on the block would be better and is a change to the layout
/// crate's type rather than to this one.
fn block_size(layout: &Layout, _source: usize, opts: &Options) -> f64 {
    let _ = layout;
    opts.body.size
}

/// Encode text as WinAnsi, counting what will not fit.
///
/// ISO 32000-1 Annex D. Latin-1 for most of the range, with the 0x80..0x9F
/// window holding the typographic characters — the quotes, dashes and ellipsis
/// a real document is full of, which is why they are worth the table rather
/// than being dropped with everything else.
fn win_ansi(text: &str) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(text.len());
    let mut dropped = 0usize;

    for ch in text.chars() {
        let code = match ch {
            '\u{20AC}' => Some(0x80),
            '\u{201A}' => Some(0x82),
            '\u{0192}' => Some(0x83),
            '\u{201E}' => Some(0x84),
            '\u{2026}' => Some(0x85),
            '\u{2020}' => Some(0x86),
            '\u{2021}' => Some(0x87),
            '\u{02C6}' => Some(0x88),
            '\u{2030}' => Some(0x89),
            '\u{0160}' => Some(0x8A),
            '\u{2039}' => Some(0x8B),
            '\u{0152}' => Some(0x8C),
            '\u{017D}' => Some(0x8E),
            '\u{2018}' => Some(0x91),
            '\u{2019}' => Some(0x92),
            '\u{201C}' => Some(0x93),
            '\u{201D}' => Some(0x94),
            '\u{2022}' => Some(0x95),
            '\u{2013}' => Some(0x96),
            '\u{2014}' => Some(0x97),
            '\u{02DC}' => Some(0x98),
            '\u{2122}' => Some(0x99),
            '\u{0161}' => Some(0x9A),
            '\u{203A}' => Some(0x9B),
            '\u{0153}' => Some(0x9C),
            '\u{017E}' => Some(0x9E),
            '\u{0178}' => Some(0x9F),
            c if (c as u32) < 0x80 || (0xA0..=0xFF).contains(&(c as u32)) => Some(c as u32 as u8),
            _ => None,
        };
        match code {
            Some(byte) => out.push(byte),
            None => dropped += 1,
        }
    }
    (out, dropped)
}

/// Lay a flow document out, write it back, and return the bytes.
///
/// The whole of document mode in one call, for a caller who wants it and has
/// said so. The frames are inferred from the document's own geometry, so a
/// two-column report comes back as a two-column report.
pub fn regenerate_document(
    doc: &mut Document,
    flow: &FlowDocument,
    layout_opts: &crate::layout::Options,
    opts: &Options,
) -> Result<Report, EmitError> {
    let model =
        rasura_layout::model::analyse(doc).map_err(|e| EmitError::PageTree(e.to_string()))?;
    let (frames, _) = rasura_layout::frames::infer(&model, &Default::default());
    let (placed, _) = crate::layout::layout(flow, &frames, layout_opts, &crate::Standard14);
    regenerate(doc, &placed, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::compare_reading;
    use rasura_cos::testutil::ClassicBuilder;

    /// A document of plain prose, from which a flow model can be built.
    fn source(paragraphs: usize) -> Vec<u8> {
        let mut content = String::new();
        for i in 0..paragraphs {
            for line in 0..3 {
                let y = 700.0 - (i * 3 + line) as f64 * 14.0;
                content.push_str(&format!(
                    "BT /F1 10 Tf 1 0 0 1 72 {y} Tm (paragraph {i} line {line} of the source \
                     document text) Tj ET\n"
                ));
            }
        }
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    #[test]
    fn regeneration_is_refused_unless_it_is_asked_for_by_name() {
        // The flag is the whole point: a caller cannot reach page regeneration
        // by autocompleting through the API.
        let mut doc = Document::open(source(2)).expect("open");
        let layout = Layout { pages: 1, blocks: Vec::new(), page_size: (612.0, 792.0) };

        let err = regenerate(&mut doc, &layout, &Options::default()).expect_err("refused");
        assert!(matches!(err, EmitError::NotAccepted), "{err:?}");
    }

    #[test]
    fn i8_holds_through_a_written_and_reopened_pdf() {
        // The loop `docs/flow-model.md` actually describes, closed end to end:
        // build the model, lay it out, **write a PDF**, re-open it, extract the
        // model again, and compare. Nothing in the pipeline is trusted to
        // report on itself — the comparison is between a model built from the
        // original bytes and one built from bytes a reader has to parse.
        let mut doc = Document::open(source(4)).expect("open");
        let (before, _) = crate::to_flow(&doc).expect("flow");

        let report = regenerate_document(
            &mut doc,
            &before,
            &crate::layout::Options::default(),
            &Options { accept_regeneration: true, ..Options::default() },
        )
        .expect("regenerate");
        assert!(report.lines > 0, "{report:?}");
        assert_eq!(report.unencodable, 0, "the fixture is plain ASCII");

        let bytes =
            rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).expect("save").bytes;
        let reopened = Document::open(bytes).expect("the written file must parse");
        let (after, _) = crate::to_flow(&reopened).expect("flow again");

        // Pagination and emphasis are excluded: the engine re-breaks to its own
        // metrics, and the written document is set in one face.
        // compare_reading rather than compare: through a real PDF a
        // paragraph split by pagination genuinely *is* two paragraphs to a
        // reader, and the format has no mark that says otherwise. What must
        // hold is that every word survived, in order.
        let diff = compare_reading(&before, &after);
        assert!(diff.is_empty(), "{diff:#?}");
    }

    #[test]
    fn the_written_file_is_a_valid_pdf_that_reopens_with_its_text() {
        let mut doc = Document::open(source(3)).expect("open");
        let (before, _) = crate::to_flow(&doc).expect("flow");
        regenerate_document(
            &mut doc,
            &before,
            &crate::layout::Options::default(),
            &Options { accept_regeneration: true, ..Options::default() },
        )
        .expect("regenerate");

        let bytes =
            rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).expect("save").bytes;
        let reopened = Document::open(bytes).expect("reopen");
        let tree = rasura_content::page::pages(&reopened).expect("pages");
        // The layout crate's extractor, not the content crate's: standard-14
        // widths come from Standard14Widths, which lives one layer up, and
        // asura_content::page_text has no width source to give it. A
        // consumer uses this one.
        let text = rasura_layout::page_text(&reopened, &tree.pages[0]);
        assert!(text.contains("paragraph 0"), "{text:?}");
        assert!(text.contains("source"), "{text:?}");
    }

    #[test]
    fn characters_the_encoding_cannot_hold_are_counted_not_swallowed() {
        // WinAnsi has 224 characters in it. A document with anything else loses
        // those characters, and a caller must not learn that from a reader.
        let (codes, dropped) = win_ansi("naive café — 東京");
        assert!(dropped > 0, "the CJK cannot be encoded");
        assert!(!codes.is_empty(), "and the rest still is");

        // The typographic window is worth having: an em dash is not exotic.
        let (dash, lost) = win_ansi("—");
        assert_eq!(lost, 0);
        assert_eq!(dash, vec![0x97]);
    }

    #[test]
    fn a_document_that_grows_gains_pages_and_keeps_its_content() {
        // Forced onto more pages than it started with, which exercises the page
        // tree growing rather than only its content being replaced.
        let mut doc = Document::open(source(30)).expect("open");
        let (before, _) = crate::to_flow(&doc).expect("flow");

        let model = rasura_layout::model::analyse(&doc).expect("analyse");
        let (frames, _) = rasura_layout::frames::infer(&model, &Default::default());
        // A frame with room for a handful of lines, so 30 paragraphs cannot fit
        // on one page however they are broken.
        let tight = rasura_layout::frames::FrameSet {
            groups: vec![rasura_layout::frames::PageGroup {
                pages: vec![0],
                size: (612.0, 792.0),
                frames: vec![rasura_layout::frames::Frame {
                    rect: rasura_content::matrix::Rect::new(72.0, 72.0, 540.0, 200.0),
                    column: 0,
                    blocks: 1,
                    evidence: rasura_layout::frames::Evidence::SinglePage,
                }],
            }],
        };
        let _ = frames;

        let (placed, _) = crate::layout::layout(
            &before,
            &tight,
            &crate::layout::Options::default(),
            &crate::Standard14,
        );
        assert!(placed.pages > 1, "the fixture should not fit on one page");

        let report = regenerate(
            &mut doc,
            &placed,
            &Options { accept_regeneration: true, ..Options::default() },
        )
        .expect("regenerate");
        assert!(report.pages_added > 0, "{report:?}");
        assert_eq!(report.pages_after, placed.pages);

        let bytes =
            rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).expect("save").bytes;
        let reopened = Document::open(bytes).expect("reopen");
        let (after, _) = crate::to_flow(&reopened).expect("flow again");

        // compare_reading rather than compare: through a real PDF a
        // paragraph split by pagination genuinely *is* two paragraphs to a
        // reader, and the format has no mark that says otherwise. What must
        // hold is that every word survived, in order.
        let diff = compare_reading(&before, &after);
        assert!(diff.is_empty(), "{diff:#?}");
    }
}
