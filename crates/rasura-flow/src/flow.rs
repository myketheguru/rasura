//! The flow model itself: content that follows content.
//!
//! Everything below this crate is *placed*. A glyph knows its device-space
//! origin, a paragraph knows its bounding box, a block knows which page it is
//! on. Nothing knows what it comes after, because the file does not say — the
//! producer's flow was consumed to compute those positions and then discarded.
//!
//! This is the reconstruction of it, and the type is deliberately shaped like a
//! word-processor document rather than like a PDF: a linear sequence of blocks,
//! each carrying inline runs, with no coordinates anywhere. Everything that
//! *was* geometry has either become structure or been dropped, and the dropping
//! is recorded in [`crate::Report`] rather than done quietly.
//!
//! # Why no positions survive
//!
//! It is tempting to keep the bounding boxes "in case they are useful". They
//! are worse than useless here: a consumer that can see a box will eventually
//! believe it, and the whole contract of this model is that the boxes no longer
//! apply — the point of flowing content is that it moves. What survives is
//! [`Source`], which names where a block came from so a caller can get back to
//! the placed model, and cannot be mistaken for a position.

use rasura_layout::model::BlockId;

/// Where a flow block came from in the placed model.
///
/// Kept so a caller can trace an exported heading back to the paragraph it was
/// promoted from — which is the first thing anyone wants when the export looks
/// wrong. Several flow blocks may share a source (a table's rows) and a block
/// may have none (a synthesised list wrapper).
pub type Source = Option<BlockId>;

/// A reconstructed document as flowing content.
#[derive(Debug, Clone, Default)]
pub struct FlowDocument {
    pub blocks: Vec<Block>,
    /// Headers and footers, once each rather than once per page.
    ///
    /// Lifted out of the flow deliberately. A running header is not part of the
    /// text — it is furniture the producer's layout engine painted on every
    /// page — and leaving it inline puts "Annual Report 2025" between every two
    /// paragraphs of the export.
    pub running: Vec<Running>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub pages: usize,
    /// Whether `/StructTreeRoot` was present and used.
    pub tagged: bool,
    /// Where the block order came from.
    pub order: Provenance,
    /// The body text size the heading inference measured against, when it ran.
    pub body_size: Option<f64>,
}

/// How much of this document's shape was read rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provenance {
    /// `/StructTreeRoot` — the producer wrote the structure down.
    Structure,
    /// §7.5's cut-tree traversal over the page geometry.
    #[default]
    Geometry,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Structure => "structure",
            Provenance::Geometry => "geometry",
        }
    }
}

/// A header or footer, with the pages it appeared on.
#[derive(Debug, Clone)]
pub struct Running {
    /// The text with any varying numeric field replaced by `{}`.
    pub template: String,
    pub top: bool,
    pub pages: Vec<usize>,
    /// Whether the varying field looks like a page number, which is the one
    /// case where the variation is not worth reporting as a difference.
    pub is_page_number: bool,
}

/// One block of flowing content.
#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        source: Source,
    },
    Paragraph {
        inlines: Vec<Inline>,
        source: Source,
    },
    List(List),
    Table(Table),
    /// An image, with whatever description the document supplied.
    ///
    /// No pixels. Extracting and re-encoding them is a codec problem this crate
    /// does not have, so a figure carries its identity and its alt text and a
    /// consumer fetches the image itself if it wants one.
    Figure {
        alt: Option<String>,
        image: Image,
        source: Source,
    },
    /// Vector artwork: a chart, a logo, a diagram.
    ///
    /// No path data crosses into the flow model. A flow document is content in
    /// reading order, and a hundred Bézier segments are not content in that
    /// sense — but *something was there*, and before the layout layer retained
    /// path geometry this block could not be produced at all, so the drawing
    /// simply disappeared from every export. [`Drawing::paths`] and the bounds
    /// are enough for a consumer to leave room for it and to go back to
    /// `rasura_layout::graphics` for the geometry.
    Drawing(Drawing),
    /// Text an annotation puts on the page.
    ///
    /// A filled form field's value and a sticky note's contents are drawn by
    /// the viewer, not by the content stream, so they reach no other part of
    /// this model. A document whose text is entirely in form fields — which is
    /// what a filled-in tax return is — exports as an empty page without this.
    ///
    /// Emitted after the page's own content rather than interleaved with it,
    /// because an annotation has no position in the reading order: the cut tree
    /// never saw it.
    Note(Note),
    /// Content the reconstruction declined to classify. Spec 7.8's `Unknown`,
    /// carried through rather than dropped: text that could not be trusted is
    /// still text somebody may need, and silently omitting it from an export
    /// would be the worst of the available options.
    Opaque {
        text: String,
        reason: OpaqueReason,
        source: Source,
    },
}

impl Block {
    pub fn kind(&self) -> &'static str {
        match self {
            Block::Heading { .. } => "heading",
            Block::Paragraph { .. } => "paragraph",
            Block::List(_) => "list",
            Block::Table(_) => "table",
            Block::Figure { .. } => "figure",
            Block::Drawing(_) => "drawing",
            Block::Note(_) => "note",
            Block::Opaque { .. } => "opaque",
        }
    }

    /// The block's text with all inline structure flattened away.
    pub fn text(&self) -> String {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => text_of(inlines),
            Block::List(l) => l.items.iter().map(|i| i.text()).collect::<Vec<_>>().join("\n"),
            Block::Table(t) => t
                .rows
                .iter()
                .map(|r| r.iter().map(|c| c.text()).collect::<Vec<_>>().join("\t"))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Figure { alt, .. } => alt.clone().unwrap_or_default(),
            Block::Drawing(_) => String::new(),
            Block::Note(n) => n.text.clone(),
            Block::Opaque { text, .. } => text.clone(),
        }
    }
}

/// Text carried by an annotation.
#[derive(Debug, Clone)]
pub struct Note {
    /// The annotation subtype, as ISO 32000-1 names it.
    pub kind: String,
    /// The field's name, for a form widget.
    pub field: Option<String>,
    pub text: String,
    pub page: usize,
}

/// Vector artwork, summarised.
#[derive(Debug, Clone)]
pub struct Drawing {
    /// How many painted paths went into it.
    pub paths: usize,
    pub kind: DrawingKind,
    /// Width and height in points, at the size it was drawn.
    pub size: (f64, f64),
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingKind {
    /// A single thin axis-aligned rectangle: a rule, an underline, a table
    /// border. Structural furniture rather than a picture, and by far the most
    /// common vector block in a text document — which is why it has its own
    /// name. An export that marked every one of these would be unreadable.
    Rule,
    /// Anything else.
    Figure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueReason {
    /// Too few glyphs mapped to Unicode for the text to be trusted.
    Unmapped,
    /// Not horizontal, so line and paragraph geometry does not apply.
    NonHorizontal,
}

impl OpaqueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            OpaqueReason::Unmapped => "text could not be mapped to Unicode",
            OpaqueReason::NonHorizontal => "text is not horizontal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct List {
    pub ordered: bool,
    pub items: Vec<Item>,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct Item {
    /// An item's content is blocks, not inlines: a list item may contain more
    /// than one paragraph, and a model that could not say so would silently
    /// join them.
    pub blocks: Vec<Block>,
}

impl Item {
    pub fn text(&self) -> String {
        self.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone)]
pub struct Table {
    /// Row-major. The first row is a header only when `has_header` says so.
    pub rows: Vec<Vec<Cell>>,
    /// Whether the first row was identified as a header — from the structure
    /// tree's `TH`, never guessed from formatting.
    pub has_header: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub blocks: Vec<Block>,
}

impl Cell {
    pub fn text(&self) -> String {
        self.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join(" ")
    }
}

/// An image's identity in the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The XObject, absent for an inline image.
    pub object: Option<rasura_cos::ObjId>,
    pub pixels: Option<(u32, u32)>,
    /// The page it was drawn on, which is the only way to find an inline image
    /// again.
    pub page: usize,
}

/// A run of text with one set of inline attributes.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text {
        text: String,
        emphasis: Emphasis,
    },
    /// A line break the author meant, as distinct from one the line-breaking
    /// produced. Only ever emitted where the evidence is positive — see
    /// `build::hard_break`.
    Break,
}

impl Inline {
    pub fn text(text: impl Into<String>) -> Inline {
        Inline::Text { text: text.into(), emphasis: Emphasis::default() }
    }
}

/// What a run of text looks like, as far as can be told without rendering it.
///
/// Derived from the PostScript font name, which is the only signal available at
/// this layer and is wrong sometimes: a face called `Helvetica-Black` is bold
/// and does not say so in a way any rule can catch, and a badly named subset
/// can claim anything. Reported as a guess in [`crate::Report`] rather than
/// presented as fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Emphasis {
    pub bold: bool,
    pub italic: bool,
    /// Rendering mode 7 and friends: the glyphs are in the file and paint
    /// nothing. Kept rather than dropped because this is how every OCR tool
    /// stores its output, and that text is the only text a scan has.
    pub invisible: bool,
}

impl Emphasis {
    pub fn is_plain(self) -> bool {
        self == Emphasis::default()
    }
}

pub(crate) fn text_of(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Break => out.push('\n'),
        }
    }
    out
}
