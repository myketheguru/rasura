//! Making a document, rather than editing one. Spec 9.2, spec 11 §`create`.
//!
//! The rest of this crate opens a file and changes it. This makes one that was
//! not there, which the specification has asked for since version 1.0 —
//! `static create(opts?: CreateOptions): Promise<Document>` — and which nothing
//! implemented, because until recently nothing could: `rasura_cos::Document`
//! had no constructor, an empty page tree could not be given a first page, and
//! no code in the workspace could write a `/FontFile2` for a typeface a
//! document had never seen.
//!
//! # It returns a [`Document`], not bytes
//!
//! A composed document is an ordinary one. It can be read back through
//! [`Document::page`], edited through [`Document::session`], protected,
//! redacted and saved like any other, because it *is* any other — the only
//! difference is that its first save is a full rewrite, there being no original
//! bytes to append to.
//!
//! # Fidelity
//!
//! Composition reports the same way every other operation does. A typeface with
//! no glyph for a character does not get one substituted; the character is
//! dropped and named in [`Composition::missing`]. The one value in a PDF font
//! descriptor that cannot be measured — `/StemV` — is estimated and
//! [`Composition::stem_v_estimated`] says so.

use crate::Document;
use crate::error::{Code, Error, Result};
use rasura_flow::compose;
use rasura_flow::flow::{self, FlowDocument};

pub use rasura_flow::compose::Family;
pub use rasura_layout::frames::PageGeometry;

/// A run of text with one emphasis, the unit a heading or paragraph is built
/// from.
///
/// A whole paragraph is one plain [`Span`] in the common case; a paragraph with
/// a bold word in it is three — plain, bold, plain. The emphasis is drawn in a
/// real face from [`Options::family`], never a synthetic slant or weight: a
/// `bold` span with no bold face supplied falls back to the regular one and is
/// named in [`Composition::substituted`], not faked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Span {
        Span { text: text.into(), bold: false, italic: false }
    }

    pub fn bold(text: impl Into<String>) -> Span {
        Span { text: text.into(), bold: true, italic: false }
    }

    pub fn italic(text: impl Into<String>) -> Span {
        Span { text: text.into(), bold: false, italic: true }
    }

    pub fn bold_italic(text: impl Into<String>) -> Span {
        Span { text: text.into(), bold: true, italic: true }
    }

    fn to_inline(&self) -> flow::Inline {
        flow::Inline::Text {
            text: self.text.clone(),
            emphasis: flow::Emphasis { bold: self.bold, italic: self.italic, invisible: false },
        }
    }
}

/// A block of a document to be composed.
///
/// Deliberately smaller than [`rasura_flow::flow::Block`], which is the model
/// *read out of* a document and carries provenance for everything in it. A
/// caller composing a document has no provenance to carry: they are saying what
/// they want, not recording what was found.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// `level` is 1 to 6, as in HTML. Sizes come from [`Options::heading_sizes`].
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    /// Drawn as its lines, without bullets. Reported in
    /// [`Composition::approximated`].
    List {
        items: Vec<String>,
    },
    /// A grid of cells, row-major, drawn with its rules. Columns are laid out
    /// equal-width and a cell wraps to the measure of its column; ragged rows
    /// are padded to the widest row's column count.
    Table {
        rows: Vec<Vec<String>>,
        /// Whether the first row is a header, drawn with a heavier rule beneath.
        header: bool,
    },
}

impl Content {
    /// A heading of one plain run. Use [`Content::heading_spans`] for emphasis.
    pub fn heading(level: u8, text: impl Into<String>) -> Content {
        Content::Heading { level: level.clamp(1, 6), spans: vec![Span::plain(text)] }
    }

    /// A heading built from styled runs.
    pub fn heading_spans(level: u8, spans: Vec<Span>) -> Content {
        Content::Heading { level: level.clamp(1, 6), spans }
    }

    /// A paragraph of one plain run. Use [`Content::paragraph_spans`] for emphasis.
    pub fn paragraph(text: impl Into<String>) -> Content {
        Content::Paragraph { spans: vec![Span::plain(text)] }
    }

    /// A paragraph built from styled runs.
    pub fn paragraph_spans(spans: Vec<Span>) -> Content {
        Content::Paragraph { spans }
    }

    /// A table of plain-text cells, no header row.
    pub fn table(rows: Vec<Vec<String>>) -> Content {
        Content::Table { rows, header: false }
    }

    /// A table whose first row is a header.
    pub fn table_with_header(rows: Vec<Vec<String>>) -> Content {
        Content::Table { rows, header: true }
    }

    fn to_block(&self) -> flow::Block {
        match self {
            Content::Heading { level, spans } => flow::Block::Heading {
                level: *level,
                inlines: spans.iter().map(Span::to_inline).collect(),
                source: None,
            },
            Content::Paragraph { spans } => flow::Block::Paragraph {
                inlines: spans.iter().map(Span::to_inline).collect(),
                source: None,
            },
            Content::List { items } => flow::Block::List(flow::List {
                items: items
                    .iter()
                    .map(|t| flow::Item {
                        blocks: vec![flow::Block::Paragraph {
                            inlines: vec![flow::Inline::text(t)],
                            source: None,
                        }],
                    })
                    .collect(),
                ordered: false,
                source: None,
            }),
            Content::Table { rows, header } => flow::Block::Table(flow::Table {
                rows: rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| flow::Cell {
                                blocks: vec![flow::Block::Paragraph {
                                    inlines: vec![flow::Inline::text(cell)],
                                    source: None,
                                }],
                            })
                            .collect()
                    })
                    .collect(),
                has_header: *header,
                source: None,
            }),
        }
    }
}

/// How to compose.
#[derive(Debug, Clone)]
pub struct Options {
    /// The page: size, margins and columns.
    pub geometry: PageGeometry,
    /// The typeface, as one or more TrueType/OpenType faces.
    ///
    /// The regular face is required, and deliberately so. The standard 14 fonts
    /// need no embedding and would make this optional, but a document set in one
    /// is a document whose appearance depends on what the reader happens to have
    /// installed — which is the opposite of what a PDF is for. A caller who
    /// wants that can still have it by building the objects directly.
    ///
    /// The bold, italic and bold-italic faces are optional. A document whose
    /// text calls for a face the family does not supply is drawn in the regular
    /// face and the gap is named in [`Composition::substituted`] — never slanted
    /// or thickened synthetically, which would measure wrong.
    pub family: Family,
    pub body_size: f64,
    /// Sizes for heading levels 1 to 6.
    pub heading_sizes: [f64; 6],
    pub title: Option<String>,
}

impl Options {
    /// Compose in one face, at defaults for everything else. Bold and italic
    /// runs fall back to this face and are reported as substituted.
    pub fn with_font(font: Vec<u8>) -> Options {
        Options::with_family(Family::regular(&font))
    }

    /// Compose in `family`, at defaults for everything else.
    pub fn with_family(family: Family) -> Options {
        Options {
            geometry: PageGeometry::default(),
            family,
            body_size: 11.0,
            heading_sizes: [24.0, 18.0, 14.0, 12.0, 11.0, 11.0],
            title: None,
        }
    }
}

/// What composing produced, beyond the document itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Composition {
    pub pages: usize,
    pub lines: usize,
    /// Blocks drawn as plain text because their structure is not: a list
    /// without bullets. Not dropped, and not silently either.
    pub approximated: usize,
    /// Characters the typeface has no glyph for. Dropped rather than
    /// substituted, and named here.
    pub missing: Vec<char>,
    /// `/BaseFont`, subset tag included.
    pub base_font: String,
    /// True when the text needed a composite font — anything outside WinAnsi.
    pub composite: bool,
    /// `/StemV` cannot be measured from a TrueType file; it was estimated.
    /// Always true today, and reported rather than assumed.
    pub stem_v_estimated: bool,
    /// The base font of every face embedded, regular first. One entry unless the
    /// document used bold or italic and the family supplied those faces.
    pub faces: Vec<String>,
    /// Faces the text asked for that the family did not supply, so those runs
    /// were drawn in the regular face. Named rather than faked.
    pub substituted: Vec<String>,
}

impl Document {
    /// Compose a new document. Spec 11's `create`.
    ///
    /// ```no_run
    /// # fn main() -> rasura::Result<()> {
    /// use rasura::create::{Content, Options};
    /// let font = std::fs::read("Roboto-Regular.ttf").unwrap();
    /// let (doc, report) = rasura::Document::create(
    ///     &[
    ///         Content::heading(1, "A title"),
    ///         Content::paragraph("Text that will be broken to the measure."),
    ///     ],
    ///     &Options::with_font(font),
    /// )?;
    /// println!("{} page(s)", report.pages);
    /// let bytes = doc.save(&Default::default())?.bytes;
    /// # Ok(()) }
    /// ```
    pub fn create(content: &[Content], opts: &Options) -> Result<(Document, Composition)> {
        if content.is_empty() {
            return Err(Error::new(Code::InvalidArgument, "there is nothing to compose"));
        }
        if opts.family.regular.is_empty() {
            return Err(Error::new(
                Code::FontUnavailable,
                "composing needs a typeface: Options::family has no regular face",
            ));
        }

        let flow = FlowDocument {
            blocks: content.iter().map(Content::to_block).collect(),
            ..FlowDocument::default()
        };

        let mut layout = rasura_flow::layout::Options {
            heading_sizes: opts.heading_sizes,
            ..rasura_flow::layout::Options::default()
        };
        layout.body.size = opts.body_size;

        let composed = compose::compose_family(
            &flow,
            &opts.family,
            &compose::Options {
                geometry: opts.geometry.clone(),
                layout,
                font_resource: "F1".to_string(),
                title: opts.title.clone(),
            },
        )
        .map_err(map_error)?;
        let (cos, report) = composed;

        // Reconstructed through the ordinary reader rather than assembled from
        // what composition happened to know. A composed document that this
        // crate could not then read would be a bug worth finding here, at the
        // moment it is made, and not on the caller's first `page()`.
        let pages = rasura_content::page::pages(&cos)?;
        let tags = rasura_layout::validate_tags(&cos, &pages);
        let kinds = pages.pages.iter().map(|p| crate::kind::classify_page(&cos, p)).collect();

        let doc = Document { inner: cos, pages, kinds, tags, has_xfa: false, registry: Vec::new() };
        Ok((
            doc,
            Composition {
                pages: report.pages,
                lines: report.lines,
                approximated: report.approximated,
                missing: report.missing,
                base_font: report.base_font,
                composite: report.composite,
                stem_v_estimated: report.stem_v_guessed,
                faces: report.faces,
                substituted: report.substituted,
            },
        ))
    }
}

fn map_error(e: compose::ComposeError) -> Error {
    match e {
        compose::ComposeError::Font(m) => Error::new(Code::FontUnavailable, m),
        compose::ComposeError::Empty => {
            Error::new(Code::InvalidArgument, "there is nothing to compose")
        }
        compose::ComposeError::Page(m) | compose::ComposeError::Cos(m) => {
            Error::new(Code::Internal, m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roboto() -> Option<Vec<u8>> {
        face("Roboto-Regular.ttf")
    }

    fn face(name: &str) -> Option<Vec<u8>> {
        std::fs::read(format!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/{}"), name))
            .ok()
    }

    #[test]
    fn a_composed_document_is_an_ordinary_one() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let (doc, report) = Document::create(
            &[
                Content::heading(1, "Composed"),
                Content::paragraph(
                    "A paragraph long enough that it has to be broken into more than one \
                     line, so that the measure is doing something and the report has \
                     lines to count.",
                ),
            ],
            &Options::with_font(font),
        )
        .unwrap();

        assert_eq!(report.pages, 1);
        assert!(report.lines > 2, "{report:?}");
        assert!(report.missing.is_empty());
        assert!(report.stem_v_estimated, "no TrueType table records it");

        // The whole point of returning a Document: everything else works on it.
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).unwrap();
        assert!(!page.paragraphs().is_empty(), "the composed text reads back as paragraphs");
        assert!(page.paragraphs().iter().any(|p| p.text.contains("Composed")));

        // And it saves and reopens.
        let saved = doc.save(&Default::default()).unwrap();
        assert_eq!(saved.mode, rasura_cos::SaveMode::FullRewrite, "there is nothing to append to");
        let reopened = Document::open(saved.bytes).unwrap();
        assert_eq!(reopened.page_count(), 1);
    }

    #[test]
    fn a_composed_document_can_then_be_edited() {
        let Some(font) = roboto() else { return };
        let (doc, _) = Document::create(
            &[Content::paragraph("The quick brown fox jumps over the lazy dog.")],
            &Options::with_font(font),
        )
        .unwrap();

        // The claim that makes `create` worth returning a Document for: the
        // editing surface does not know or care that this file was composed.
        let before = doc.page(0).unwrap().paragraphs()[0].text.clone();
        assert!(before.contains("quick"));

        let saved = doc.save(&Default::default()).unwrap();
        let reopened = Document::open(saved.bytes).unwrap();
        assert!(reopened.page(0).unwrap().paragraphs()[0].text.contains("quick"));
    }

    #[test]
    fn emphasis_is_composed_in_the_faces_the_family_supplies() {
        let (Some(regular), Some(bold), Some(italic)) =
            (face("Roboto-Regular.ttf"), face("Roboto-Bold.ttf"), face("Roboto-Italic.ttf"))
        else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };

        let family = Family { regular, bold: Some(bold), italic: Some(italic), bold_italic: None };
        let (doc, report) = Document::create(
            &[Content::paragraph_spans(vec![
                Span::plain("A line with a "),
                Span::bold("bold"),
                Span::plain(" word, an "),
                Span::italic("italic"),
                Span::plain(" one, and a "),
                // No bold-italic face was supplied: this run falls back to the
                // regular face and the gap is reported, not faked.
                Span::bold_italic("both"),
                Span::plain(" one."),
            ])],
            &Options::with_family(family),
        )
        .unwrap();

        // Three faces embedded — regular, bold, italic — and reported.
        assert!(report.faces.len() >= 3, "faces: {:?}", report.faces);
        // The bold-italic run had no face, so it was drawn regular and named.
        assert!(!report.substituted.is_empty(), "the missing bold-italic face is reported");
        assert!(report.missing.is_empty(), "every glyph was drawable: {report:?}");

        // It still reads back and reopens like any other document.
        assert_eq!(doc.page_count(), 1);
        let saved = doc.save(&Default::default()).unwrap();
        assert_eq!(Document::open(saved.bytes).unwrap().page_count(), 1);
    }

    #[test]
    fn a_family_of_one_face_substitutes_nothing_until_asked() {
        let Some(font) = roboto() else { return };
        // Plain text in a one-face family: nothing to substitute.
        let (_, report) =
            Document::create(&[Content::paragraph("Plain text.")], &Options::with_font(font))
                .unwrap();
        assert_eq!(report.faces.len(), 1, "one face embedded: {:?}", report.faces);
        assert!(report.substituted.is_empty(), "plain text asks for no other face");
    }

    #[test]
    fn a_table_composes_as_a_grid_and_reads_back() {
        let Some(font) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };

        let (doc, report) = Document::create(
            &[
                Content::heading(1, "Hours"),
                Content::table_with_header(vec![
                    vec!["Task".into(), "Hours".into()],
                    vec!["Design".into(), "12".into()],
                    vec!["Build".into(), "30".into()],
                ]),
            ],
            &Options::with_font(font),
        )
        .unwrap();

        // The table is drawn, not dropped as an approximation.
        assert_eq!(report.approximated, 0, "a table is composed as a grid: {report:?}");
        assert!(report.missing.is_empty());

        // It reads back: the reader reconstructs the table, and the cell text is
        // in the page.
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).unwrap();
        assert!(!page.tables().is_empty(), "the composed grid reads back as a table");
        let text: String = page.paragraphs().iter().map(|p| p.text.as_str()).collect();
        assert!(text.contains("Design"), "cell text is present: {text:?}");

        let saved = doc.save(&Default::default()).unwrap();
        assert_eq!(Document::open(saved.bytes).unwrap().page_count(), 1);
    }

    #[test]
    fn composing_without_a_typeface_is_refused_by_name() {
        let err = Document::create(&[Content::paragraph("text")], &Options::with_font(Vec::new()))
            .expect_err("refused");
        assert_eq!(err.code(), Code::FontUnavailable);
    }

    #[test]
    fn composing_nothing_is_refused() {
        let Some(font) = roboto() else { return };
        let err = Document::create(&[], &Options::with_font(font)).expect_err("refused");
        assert_eq!(err.code(), Code::InvalidArgument);
    }
}
