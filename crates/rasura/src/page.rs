//! A page, as paragraphs and blocks. Spec 11.2.
//!
//! The layout crate produces a rich model — style runs carrying colours and
//! render modes, lines carrying baselines and tangents, regions carrying
//! detection provenance. Most of that is machinery for the layers above it, not
//! answers a caller asked for, and passing it through unchanged would put PDF
//! back into an API whose brief is to keep it out.
//!
//! So this is a narrowing. What survives is what §11.2 lists, plus the two
//! things it does not: [`Paragraph::confidence`], because text a document could
//! not resolve is text a caller must not trust, and [`Paragraph::id`], because
//! every edit addresses a paragraph and an index into a vector is not a stable
//! name.

use crate::error::{Code, Error, Result};
use crate::kind::PageKind;
use rasura_edit::locate::{EditablePage, ParagraphId as EditParagraphId};

pub use rasura_content::matrix::Rect;
pub use rasura_layout::paragraphs::Alignment;

/// A paragraph's address, stable for the life of a [`Page`].
///
/// Opaque on purpose. It is a region index and a paragraph index underneath,
/// and a caller who learned that would start constructing them — which would
/// break the moment paragraph detection improved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParagraphId(EditParagraphId);

impl ParagraphId {
    pub(crate) fn inner(self) -> EditParagraphId {
        self.0
    }
}

/// An image's address, stable for the life of a [`Page`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(pub(crate) usize);

/// One image on a page. Spec 11.2's `images()`.
#[derive(Debug, Clone)]
pub struct Image {
    pub id: ImageId,
    /// Where it sits on the page, after every transform.
    pub box_: Rect,
    /// Pixel dimensions, when the image declares them.
    pub pixels: Option<(u32, u32)>,
    /// Whether this layer can move, scale or delete it.
    ///
    /// False for an image drawn inside a form XObject: its byte spans address
    /// the form's stream rather than the page's, and a form may be invoked from
    /// several pages, so editing it changes all of them. Reported here so a UI
    /// can grey the handle out rather than offering a drag that will be
    /// refused.
    pub editable: bool,
}

/// How far to trust a paragraph's text. Spec 11.2's `textConfidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Every glyph resolved through `/ToUnicode` or an equally authoritative
    /// source.
    Exact,
    /// Some glyphs resolved by heuristic, or some not at all. The text is
    /// readable and may not be right.
    Partial,
    /// Nothing resolved. There are glyphs on the page and no way to say what
    /// they are.
    None,
}

/// One paragraph. Spec 11.2.
#[derive(Debug, Clone)]
pub struct Paragraph {
    pub id: ParagraphId,
    pub text: String,
    pub confidence: Confidence,
    pub box_: Rect,
    pub alignment: Alignment,
    pub leading: f64,
    pub line_count: usize,
}

/// Anything on a page that is not a paragraph. Spec 11.2's `blocks()`.
///
/// Deliberately coarse. A caller asking "what is on this page" wants the
/// inventory; a caller who needs a vector path's provenance is past the point
/// where this API helps and should be using [`crate::Document::raw`].
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph {
        id: ParagraphId,
        box_: Rect,
    },
    Table {
        box_: Rect,
        rows: usize,
        columns: usize,
    },
    Image {
        box_: Rect,
    },
    Vector {
        box_: Rect,
    },
    /// A running header or footer, which repeats across pages and is usually
    /// not what a caller means to edit.
    Running {
        box_: Rect,
    },
    /// Content the layout layer would not classify. Preserved and never
    /// reflowed: guessing is worse than declining.
    Unknown {
        box_: Rect,
    },
}

impl Block {
    pub fn box_(&self) -> Rect {
        match self {
            Block::Paragraph { box_, .. }
            | Block::Table { box_, .. }
            | Block::Image { box_ }
            | Block::Vector { box_ }
            | Block::Running { box_ }
            | Block::Unknown { box_ } => *box_,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Block::Paragraph { .. } => "paragraph",
            Block::Table { .. } => "table",
            Block::Image { .. } => "image",
            Block::Vector { .. } => "vector",
            Block::Running { .. } => "running",
            Block::Unknown { .. } => "unknown",
        }
    }
}

/// One analysed page.
pub struct Page {
    pub(crate) index: usize,
    pub(crate) editable: EditablePage,
    media_box: Rect,
    rotate: i32,
    kind: Option<PageKind>,
    paragraphs: Vec<Paragraph>,
    tables: Vec<rasura_layout::tables::Table>,
    /// Kept whole rather than reduced to rectangles: the edit operations
    /// address an image by its byte span and transform, not by where it
    /// happens to land.
    pub(crate) blocks: Vec<rasura_layout::graphics::ImageBlock>,
    images: Vec<Image>,
    vectors: Vec<Rect>,
}

impl Page {
    pub(crate) fn analyse(
        doc: &rasura_cos::Document,
        raw: &rasura_content::page::Page,
        kind: Option<PageKind>,
    ) -> Result<Page> {
        let editable = EditablePage::analyse(doc, raw).ok_or_else(|| {
            Error::new(Code::Malformed, format!("page {} could not be read", raw.index))
        })?;

        let paragraphs = editable
            .paragraphs
            .iter()
            .map(|(id, para)| {
                let lines = editable.lines_of(*id).unwrap_or_default();
                Paragraph {
                    id: ParagraphId(*id),
                    text: editable.text_of(*id),
                    confidence: confidence_of(lines),
                    box_: para.bbox,
                    alignment: para.alignment,
                    leading: para.leading,
                    line_count: lines.len(),
                }
            })
            .collect();

        // Tables and graphics come from the same analysis the paragraphs did,
        // so they are computed once here rather than on every accessor: a UI
        // asking for blocks on each repaint should not re-run detection.
        let rules = rasura_layout::rules::collect(doc, raw);
        let tables = rasura_layout::tables::detect_page(&editable.regions, &rules, &editable.runs);
        let graphics = rasura_layout::graphics::collect(doc, raw);
        let images: Vec<Image> = graphics
            .images
            .iter()
            .enumerate()
            .map(|(i, image)| Image {
                id: ImageId(i),
                box_: image.bbox,
                pixels: image.pixels,
                editable: image.depth == 0,
            })
            .collect();
        let vectors = graphics.vectors.iter().map(|v| v.bbox).collect();
        let blocks = graphics.images;

        Ok(Page {
            index: raw.index,
            media_box: raw.media_box,
            rotate: raw.rotate,
            kind,
            editable,
            paragraphs,
            tables,
            blocks,
            images,
            vectors,
        })
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn media_box(&self) -> Rect {
        self.media_box
    }

    /// Clockwise degrees, always one of 0, 90, 180, 270.
    pub fn rotate(&self) -> i32 {
        self.rotate
    }

    /// True when this page is a picture of a page. Spec 3: no OCR, so the text
    /// of a scan is whatever an OCR tool already left in it.
    pub fn is_scanned(&self) -> bool {
        self.kind.is_some_and(|k| k.scanned)
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }

    pub fn paragraph(&self, id: ParagraphId) -> Option<&Paragraph> {
        self.paragraphs.iter().find(|p| p.id == id)
    }

    /// The paragraph containing a point, if any.
    ///
    /// The operation an editing UI performs on every click, which is why it is
    /// here rather than left to a caller looping over boxes: doing it wrong —
    /// by taking the first match rather than the smallest — puts the cursor in
    /// a column when the user clicked a footnote inside it.
    pub fn paragraph_at(&self, x: f64, y: f64) -> Option<&Paragraph> {
        self.paragraphs
            .iter()
            .filter(|p| x >= p.box_.x0 && x <= p.box_.x1 && y >= p.box_.y0 && y <= p.box_.y1)
            .min_by(|a, b| {
                area(&a.box_).partial_cmp(&area(&b.box_)).unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Everything on the page: paragraphs, tables, images and vectors.
    pub fn blocks(&self) -> Vec<Block> {
        let mut out: Vec<Block> =
            self.paragraphs.iter().map(|p| Block::Paragraph { id: p.id, box_: p.box_ }).collect();

        for table in &self.tables {
            out.push(Block::Table { box_: table.bbox, rows: table.rows, columns: table.cols });
        }
        for image in &self.images {
            out.push(Block::Image { box_: image.box_ });
        }
        for vector in &self.vectors {
            out.push(Block::Vector { box_: *vector });
        }
        out
    }

    /// Tables detected on this page. Spec 7.7.
    pub fn tables(&self) -> &[rasura_layout::tables::Table] {
        &self.tables
    }

    /// The images on this page. Spec 11.2.
    pub fn images(&self) -> &[Image] {
        &self.images
    }

    pub fn image(&self, id: ImageId) -> Option<&Image> {
        self.images.iter().find(|i| i.id == id)
    }

    /// The page's text, paragraphs joined by blank lines.
    pub fn text(&self) -> String {
        self.paragraphs.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n\n")
    }

    /// The layer below, for callers who need the byte spans.
    pub fn raw(&self) -> &EditablePage {
        &self.editable
    }
}

impl std::fmt::Debug for Page {
    /// The analysed page without the analysis: a `Debug` that printed every
    /// glyph run would be unreadable in the one place `Debug` is used, which is
    /// a failing assertion.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("index", &self.index)
            .field("paragraphs", &self.paragraphs.len())
            .field("tables", &self.tables.len())
            .field("images", &self.images.len())
            .field("scanned", &self.is_scanned())
            .finish()
    }
}

fn area(r: &Rect) -> f64 {
    ((r.x1 - r.x0) * (r.y1 - r.y0)).abs()
}

/// A paragraph is only as trustworthy as its least trustworthy glyph.
///
/// Taken as the *minimum* over the lines rather than a proportion. A paragraph
/// that resolved 95% of its glyphs is not 95% correct — it is a sentence with
/// an unknown word in it, and a caller deciding whether to show the text to a
/// user needs to know that one is there.
fn confidence_of(lines: &[rasura_layout::lines::Line]) -> Confidence {
    let mut total = 0usize;
    let mut mapped = 0usize;
    for line in lines {
        for glyph in &line.glyphs {
            total += 1;
            if glyph.is_mapped() {
                mapped += 1;
            }
        }
    }
    match (total, mapped) {
        (0, _) => Confidence::Exact,
        (t, m) if m == t => Confidence::Exact,
        (_, 0) => Confidence::None,
        _ => Confidence::Partial,
    }
}
