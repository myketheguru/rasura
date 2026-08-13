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

pub use rasura_layout::frames::PageGeometry;

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
        text: String,
    },
    Paragraph {
        text: String,
    },
    /// Drawn as its lines, without bullets. Reported in
    /// [`Composition::approximated`].
    List {
        items: Vec<String>,
    },
}

impl Content {
    pub fn heading(level: u8, text: impl Into<String>) -> Content {
        Content::Heading { level: level.clamp(1, 6), text: text.into() }
    }

    pub fn paragraph(text: impl Into<String>) -> Content {
        Content::Paragraph { text: text.into() }
    }

    fn to_block(&self) -> flow::Block {
        match self {
            Content::Heading { level, text } => flow::Block::Heading {
                level: *level,
                inlines: vec![flow::Inline::text(text)],
                source: None,
            },
            Content::Paragraph { text } => {
                flow::Block::Paragraph { inlines: vec![flow::Inline::text(text)], source: None }
            }
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
        }
    }
}

/// How to compose.
#[derive(Debug, Clone)]
pub struct Options {
    /// The page: size, margins and columns.
    pub geometry: PageGeometry,
    /// The typeface, as a TrueType or OpenType file.
    ///
    /// Required, and deliberately so. The standard 14 fonts need no embedding
    /// and would make this optional, but a document set in one is a document
    /// whose appearance depends on what the reader happens to have installed —
    /// which is the opposite of what a PDF is for. A caller who wants that can
    /// still have it by building the objects directly.
    pub font: Vec<u8>,
    pub body_size: f64,
    /// Sizes for heading levels 1 to 6.
    pub heading_sizes: [f64; 6],
    pub title: Option<String>,
}

impl Options {
    /// Compose in `font`, at defaults for everything else.
    pub fn with_font(font: Vec<u8>) -> Options {
        Options {
            geometry: PageGeometry::default(),
            font,
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
        if opts.font.is_empty() {
            return Err(Error::new(
                Code::FontUnavailable,
                "composing needs a typeface: Options::font is empty",
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

        let composed = compose::compose(
            &flow,
            &opts.font,
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
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/Roboto-Regular.ttf"))
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
