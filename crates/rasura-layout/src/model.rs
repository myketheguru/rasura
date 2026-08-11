//! The document model. Spec 7.8.
//!
//! Everything before this produces evidence; this decides. A page's regions,
//! tables, running elements, images and vector art all describe the same
//! surface, and something has to say which classification wins where they
//! overlap.
//!
//! The ordering is by *how much the classifier knows*, strongest first:
//! a table claims its region because a drawn grid or aligned columns is
//! positive evidence; a running element claims its region because it repeats
//! across pages, which no single page can fake; a paragraph claims what is
//! left. Anything that survives all three is `Unknown`.
//!
//! Spec 7.8 is explicit about that last case: "Anything the reconstruction
//! cannot confidently classify is preserved as opaque content, rendered and
//! moved but never reflowed. Guessing is worse than declining." The whole point
//! of the enum is that declining is representable.

use crate::graphics::{ImageBlock, VectorBlock};
use crate::lines::Line;
use crate::paragraphs::Paragraph;
use crate::running::RunningElement;
use crate::structure::StructTree;
use crate::tables::Table;
use crate::{Region, ResolvedRun, Rule};
use rasura_content::matrix::Rect;
use rasura_cos::Document;

/// A block whose text is less than this proportion mapped to Unicode is not
/// classified as a paragraph. It can be shown and moved; it cannot be reflowed,
/// because reflow needs to know what the characters are.
const MIN_MAPPED: f64 = 0.5;

/// Where a block sits in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockId {
    pub page: usize,
    pub index: usize,
}

/// Content that could not be confidently classified.
///
/// Spec 7.8: "preserved verbatim, never reflowed". It keeps its lines, so it
/// can be drawn and repositioned, and it records *why* it was declined so a
/// caller can tell a scanned page from a corrupt font.
#[derive(Debug, Clone)]
pub struct RawBlock {
    pub lines: Vec<Line>,
    pub bbox: Rect,
    pub reason: DeclineReason,
}

impl RawBlock {
    /// Whatever text could be recovered. May be empty, and may be wrong; it is
    /// offered for search and display, never for reflow.
    pub fn text(&self) -> String {
        self.lines.iter().map(crate::line_text).collect::<Vec<_>>().join(" ")
    }
}

/// Why a region was not classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineReason {
    /// Too few glyphs mapped to Unicode for the text to be trusted.
    Unmapped,
    /// The text is not horizontal, so line and paragraph geometry does not
    /// apply. Reported rather than rotated into place, because the rotation
    /// would have to be guessed.
    NonHorizontal,
}

/// A classified block. Spec 7.8.
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image(ImageBlock),
    Vector(VectorBlock),
    Running(RunningElement),
    /// Preserved verbatim, never reflowed.
    Unknown(RawBlock),
}

impl Block {
    pub fn bbox(&self) -> Rect {
        match self {
            Block::Paragraph(p) => p.bbox,
            Block::Table(t) => t.bbox,
            Block::Image(i) => i.bbox,
            Block::Vector(v) => v.bbox,
            Block::Running(r) => r.bbox,
            Block::Unknown(u) => u.bbox,
        }
    }

    /// Whether this block's content may be reflowed by an edit. Spec 7.8 makes
    /// `Unknown` opaque; images and vector art are placed, not flowed.
    pub fn is_reflowable(&self) -> bool {
        matches!(self, Block::Paragraph(_) | Block::Table(_))
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Block::Paragraph(_) => "paragraph",
            Block::Table(_) => "table",
            Block::Image(_) => "image",
            Block::Vector(_) => "vector",
            Block::Running(_) => "running",
            Block::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PageModel {
    pub blocks: Vec<Block>,
    /// The page's annotations.
    ///
    /// Not blocks. An annotation is drawn over the page by the viewer rather
    /// than by the content stream, so it has no place in the reading order the
    /// cut tree produced — but it can carry text a reader sees and the content
    /// stream does not, which is why the model has to know about them at all.
    pub annotations: Vec<crate::annots::Annotation>,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotate: i32,
    /// The lines each block came from, parallel to `blocks`, for the blocks
    /// that came from text. Empty for images and vector art.
    pub lines: Vec<Vec<Line>>,
}

/// Where the reading order came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSource {
    /// `/StructTreeRoot` — the producer's own statement.
    Structure,
    /// §7.5's cut-tree traversal.
    Geometry,
}

#[derive(Debug, Clone)]
pub struct DocumentModel {
    pub pages: Vec<PageModel>,
    /// From `/StructTreeRoot` when the document is tagged.
    pub structure: Option<StructTree>,
    pub reading_order: Vec<BlockId>,
    /// Which source produced `reading_order`. Recorded rather than implied: a
    /// caller trusting the order should know whether it was read or inferred.
    pub order_source: OrderSource,
}

impl DocumentModel {
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.pages.get(id.page)?.blocks.get(id.index)
    }

    /// Blocks in reading order.
    pub fn in_reading_order(&self) -> impl Iterator<Item = (BlockId, &Block)> {
        self.reading_order.iter().filter_map(|id| self.block(*id).map(|b| (*id, b)))
    }

    /// Plain text of the whole document, in reading order.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (_, block) in self.in_reading_order() {
            let part = match block {
                Block::Paragraph(_) | Block::Running(_) | Block::Unknown(_) => {
                    self.block_text(block)
                }
                Block::Table(t) => t
                    .cells
                    .iter()
                    .map(|c| c.text())
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\t"),
                Block::Image(_) | Block::Vector(_) => String::new(),
            };
            if !part.trim().is_empty() {
                out.push_str(part.trim());
                out.push('\n');
            }
        }
        out
    }

    fn block_text(&self, block: &Block) -> String {
        // Paragraphs index into their page's lines, so text has to be taken
        // from the page rather than from the block alone.
        for page in &self.pages {
            for (i, b) in page.blocks.iter().enumerate() {
                if std::ptr::eq(b, block) {
                    return page
                        .lines
                        .get(i)
                        .map(|ls| ls.iter().map(crate::line_text).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                }
            }
        }
        String::new()
    }
}

/// What one page contributes to the model.
pub struct PageInput<'a> {
    pub regions: &'a [Region],
    /// The page's annotations, already read.
    ///
    /// Passed in rather than read here because uild_page has no document —
    /// and because a caller streaming pages may have read them already. Empty
    /// is a legitimate value and means the page has none.
    pub annotations: Vec<crate::annots::Annotation>,
    pub rules: &'a [Rule],
    pub runs: &'a [ResolvedRun],
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotate: i32,
    pub graphics: crate::graphics::Graphics,
}

/// Analyse a whole document, from bytes already opened.
///
/// [`build`] takes borrowed per-page evidence because the callers that stream
/// pages need to own it themselves. Everyone who does *not* — the tests here,
/// the text-diff harness, the flow builder — was assembling the same fifteen
/// lines, and three copies of a fifteen-line assembly is three chances to
/// collect the evidence in a slightly different order and wonder why the models
/// differ.
///
/// Eager: every page is resolved before anything is returned, because the
/// reading order and the running elements are document-level properties and
/// cannot be computed from a page at a time.
pub fn analyse(doc: &Document) -> Result<DocumentModel, rasura_cos::CosError> {
    let tree = rasura_content::page::pages(doc)?;

    // Collected first, then borrowed: `PageInput` holds references, so the
    // evidence has to outlive the inputs vector.
    let mut per_page = Vec::with_capacity(tree.pages.len());
    for p in &tree.pages {
        let (runs, _) = crate::resolve_page(doc, p);
        let rules = crate::rules::collect(doc, p);
        let regions = crate::detect(crate::place(&runs), &rules);
        let graphics = crate::graphics::collect(doc, p);
        let annotations = crate::annots::read(doc, p);
        per_page.push((
            regions,
            rules,
            runs,
            graphics,
            annotations,
            p.media_box,
            p.crop_box,
            p.rotate,
        ));
    }

    let inputs: Vec<PageInput<'_>> = per_page
        .iter()
        .map(|(regions, rules, runs, graphics, annotations, media_box, crop_box, rotate)| {
            PageInput {
                regions,
                annotations: annotations.clone(),
                rules,
                runs,
                media_box: *media_box,
                crop_box: *crop_box,
                rotate: *rotate,
                graphics: graphics.clone(),
            }
        })
        .collect();

    Ok(build(doc, inputs))
}

/// Assemble the document model.
pub fn build(doc: &Document, pages: Vec<PageInput<'_>>) -> DocumentModel {
    // Running elements first: they are the only classification that needs every
    // page, and a block claimed as a running header must not also be offered as
    // a paragraph.
    let page_regions: Vec<crate::PageRegions<'_>> = pages
        .iter()
        .map(|p| crate::PageRegions { regions: p.regions, media_box: p.media_box })
        .collect();
    let running = crate::running_elements(&page_regions);

    let mut models = Vec::with_capacity(pages.len());
    for (pi, input) in pages.iter().enumerate() {
        models.push(build_page(pi, input, &running));
    }

    let structure = crate::structure::read(doc);
    let (reading_order, order_source) = reading_order(&models, structure.as_ref());

    DocumentModel { pages: models, structure, reading_order, order_source }
}

fn build_page(page: usize, input: &PageInput<'_>, running: &[RunningElement]) -> PageModel {
    let mut blocks = Vec::new();
    let mut lines: Vec<Vec<Line>> = Vec::new();

    // 1. Tables. Strongest text evidence: a drawn grid, or columns that line up
    //    across rows. Claimed first so the regions inside a table are not also
    //    emitted as paragraphs.
    let tables = crate::detect_page(input.regions, input.rules, input.runs);

    // Claimed by *membership*, not by bounding box. Claiming geometrically
    // while filling by membership is not the same set: a glyph inside a
    // table's box that no cell took would be claimed and then never stored, and
    // on the corpus that silently dropped 2,216 glyphs. A glyph's (run, index)
    // is unique on a page, so the two sets can be made to agree exactly.
    let mut claimed: std::collections::HashSet<(usize, usize)> = Default::default();
    for t in &tables {
        for l in t.cells.iter().flat_map(|c| c.lines.iter()) {
            for g in &l.glyphs {
                claimed.insert((g.run, g.index));
            }
        }
    }
    // A table is emitted at the position of the first region it consumed, not
    // ahead of everything. §7.5's cut-tree traversal *is* the geometric reading
    // order, and hoisting tables to the front would discard it -- which would
    // also make the structure tree's order incomparable with it, and that
    // comparison is the only external check either one has.
    let mut anchored: Vec<(usize, usize)> = Vec::new();
    for (ti, t) in tables.iter().enumerate() {
        let first = input
            .regions
            .iter()
            .position(|r| {
                r.lines.iter().flat_map(|l| l.glyphs.iter()).any(|g| {
                    t.cells
                        .iter()
                        .flat_map(|c| c.lines.iter())
                        .flat_map(|l| l.glyphs.iter())
                        .any(|cg| cg.run == g.run && cg.index == g.index)
                })
            })
            .unwrap_or(usize::MAX);
        anchored.push((first, ti));
    }

    // 2. Running elements. Cross-page repetition is evidence no single page can
    //    manufacture, so it outranks the paragraph reading of the same region.
    let mut running_here: Vec<usize> = Vec::new();
    for r in running {
        for (&p, &b) in r.pages.iter().zip(r.blocks.iter()) {
            if p == page {
                running_here.push(b);
            }
        }
    }

    // 3. Everything else, in region order.
    let mut emitted_tables = vec![false; tables.len()];
    for (ri, region) in input.regions.iter().enumerate() {
        for (anchor, ti) in &anchored {
            if *anchor == ri && !emitted_tables[*ti] {
                emitted_tables[*ti] = true;
                blocks.push(Block::Table(tables[*ti].clone()));
                lines.push(Vec::new());
            }
        }
        if running_here.contains(&ri) {
            let r = running
                .iter()
                .find(|r| r.pages.iter().zip(r.blocks.iter()).any(|(&p, &b)| p == page && b == ri));
            if let Some(r) = r {
                blocks.push(Block::Running(r.clone()));
                lines.push(region.lines.clone());
            }
            continue;
        }
        // Whatever a table did not take. A region partly inside a table keeps
        // its remainder rather than being dropped whole.
        let left: Vec<crate::PlacedGlyph> = region
            .lines
            .iter()
            .flat_map(|l| l.glyphs.iter())
            .filter(|g| !claimed.contains(&(g.run, g.index)))
            .cloned()
            .collect();
        if left.is_empty() {
            continue;
        }
        let region = if left.len() == region.lines.iter().map(|l| l.glyphs.len()).sum::<usize>() {
            region.clone()
        } else {
            let relined = crate::assemble(left);
            Region { bbox: bounds(&relined), lines: relined, origin: region.origin, order: ri }
        };
        if region.lines.iter().all(|l| l.is_empty()) {
            continue;
        }

        match decline(&region) {
            Some(reason) => {
                blocks.push(Block::Unknown(RawBlock {
                    lines: region.lines.clone(),
                    bbox: region.bbox,
                    reason,
                }));
                lines.push(region.lines.clone());
            }
            None => {
                for p in crate::reconstruct(&region, input.runs) {
                    let own: Vec<Line> = region.lines[p.lines.clone()].to_vec();
                    blocks.push(Block::Paragraph(p));
                    lines.push(own);
                }
            }
        }
    }

    // A table whose glyphs came from no region -- possible when a ruled grid
    // encloses content the cut assigned elsewhere -- still belongs in the page.
    for (ti, done) in emitted_tables.iter().enumerate() {
        if !done {
            blocks.push(Block::Table(tables[ti].clone()));
            lines.push(Vec::new());
        }
    }

    // 4. Graphics, appended: they have no position in the text flow, and
    //    inserting them by geometry would assert an ordering the page does not
    //    state.
    for i in &input.graphics.images {
        blocks.push(Block::Image(i.clone()));
        lines.push(Vec::new());
    }
    for v in &input.graphics.vectors {
        blocks.push(Block::Vector(v.clone()));
        lines.push(Vec::new());
    }

    PageModel {
        annotations: input.annotations.clone(),
        blocks,
        lines,
        media_box: input.media_box,
        crop_box: input.crop_box,
        rotate: input.rotate,
    }
}

/// Whether a region must be declined rather than read as paragraphs.
fn decline(region: &Region) -> Option<DeclineReason> {
    let glyphs: Vec<&crate::PlacedGlyph> =
        region.lines.iter().flat_map(|l| l.glyphs.iter()).collect();
    if glyphs.is_empty() {
        return None;
    }

    // Non-horizontal text: line assembly clusters on the baseline direction, so
    // the lines are right, but indent, alignment and margin all assume a
    // horizontal measure and would be nonsense.
    if region.lines.iter().any(|l| !l.is_horizontal()) {
        return Some(DeclineReason::NonHorizontal);
    }

    let mapped = glyphs.iter().filter(|g| g.is_mapped()).count();
    if (mapped as f64) < glyphs.len() as f64 * MIN_MAPPED {
        return Some(DeclineReason::Unmapped);
    }
    None
}

fn bounds(lines: &[Line]) -> Rect {
    let mut b = Rect { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
    for l in lines {
        b.x0 = b.x0.min(l.bbox.x0);
        b.y0 = b.y0.min(l.bbox.y0);
        b.x1 = b.x1.max(l.bbox.x1);
        b.y1 = b.y1.max(l.bbox.y1);
    }
    if lines.is_empty() { Rect::default() } else { b }
}

/// Reading order, preferring the structure tree.
fn reading_order(
    pages: &[PageModel],
    structure: Option<&StructTree>,
) -> (Vec<BlockId>, OrderSource) {
    let geometric: Vec<BlockId> = pages
        .iter()
        .enumerate()
        .flat_map(|(p, page)| (0..page.blocks.len()).map(move |i| BlockId { page: p, index: i }))
        .collect();

    // The structure tree is the producer's own statement of reading order and
    // the only oracle available -- but only where it actually covers the page.
    // A tree that is truncated, or that names marked content this page does not
    // have, is worse than the geometry it would replace.
    let Some(tree) = structure.filter(|t| !t.truncated && !t.is_empty()) else {
        return (geometric, OrderSource::Geometry);
    };

    let mut ordered: Vec<BlockId> = Vec::with_capacity(geometric.len());
    let mut used = vec![false; geometric.len()];
    let mut covered = 0usize;

    for (p, page) in pages.iter().enumerate() {
        let order = tree.mcid_order(p);
        if order.is_empty() {
            continue;
        }
        for mcid in order {
            for (i, block) in page.blocks.iter().enumerate() {
                let Block::Paragraph(para) = block else { continue };
                if para.mcid != Some(mcid) {
                    continue;
                }
                let flat = geometric.iter().position(|b| b.page == p && b.index == i);
                if let Some(flat) = flat
                    && !used[flat]
                {
                    used[flat] = true;
                    covered += 1;
                    ordered.push(BlockId { page: p, index: i });
                }
            }
        }
    }

    // Anything the tree did not mention keeps its geometric position, appended
    // in that order. A partial tree still improves on none.
    for (flat, id) in geometric.iter().enumerate() {
        if !used[flat] {
            ordered.push(*id);
        }
    }

    let source = if covered > 0 { OrderSource::Structure } else { OrderSource::Geometry };
    (ordered, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::page_with;
    use rasura_content::page;

    /// Build a model from one content stream.
    fn model_of(content: &str) -> DocumentModel {
        model_of_bytes(page_with(content))
    }

    fn model_of_bytes(bytes: Vec<u8>) -> DocumentModel {
        let doc = Document::open(bytes).expect("open");
        let tree = page::pages(&doc).expect("pages");

        // Collected per page, then borrowed: `PageInput` holds references.
        let mut per_page = Vec::new();
        for p in &tree.pages {
            let (runs, _) = crate::resolve_page(&doc, p);
            let rules = crate::rules::collect(&doc, p);
            let regions = crate::detect(crate::place(&runs), &rules);
            let graphics = crate::graphics::collect(&doc, p);
            per_page.push((regions, rules, runs, graphics, p.clone()));
        }
        let inputs: Vec<PageInput<'_>> = per_page
            .iter()
            .map(|(regions, rules, runs, graphics, p)| PageInput {
                regions,
                annotations: crate::annots::read(&doc, p),
                rules,
                runs,
                media_box: p.media_box,
                crop_box: p.crop_box,
                rotate: p.rotate,
                graphics: graphics.clone(),
            })
            .collect();
        build(&doc, inputs)
    }

    fn prose(lines: usize) -> String {
        (0..lines)
            .map(|i| {
                format!(
                    "BT /F1 10 Tf 1 0 0 1 72 {} Tm (ordinary body text line {i} here) Tj ET\n",
                    700 - i * 12
                )
            })
            .collect()
    }

    #[test]
    fn prose_becomes_paragraph_blocks() {
        let m = model_of(&prose(6));
        assert_eq!(m.pages.len(), 1);
        assert!(m.pages[0].blocks.iter().any(|b| matches!(b, Block::Paragraph(_))));
        assert_eq!(m.order_source, OrderSource::Geometry);
        assert!(m.structure.is_none());
    }

    #[test]
    fn every_block_appears_exactly_once_in_reading_order() {
        let m = model_of(&prose(6));
        let total: usize = m.pages.iter().map(|p| p.blocks.len()).sum();
        assert_eq!(m.reading_order.len(), total);
        let mut sorted = m.reading_order.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), total, "no block is listed twice or omitted");
    }

    #[test]
    fn an_image_becomes_an_image_block() {
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /XObject << /Im1 5 0 R >> >> >>",
            )
            .stream(4, "", b"q 200 0 0 100 100 600 cm /Im1 Do Q")
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 20 /Height 10 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8; 8],
            )
            .finish("/Root 1 0 R");
        let m = model_of_bytes(bytes);
        let img = m.pages[0].blocks.iter().find(|b| matches!(b, Block::Image(_)));
        assert!(img.is_some(), "the image is in the model");
        assert!(!img.unwrap().is_reflowable(), "an image is placed, not flowed");
    }

    #[test]
    fn vector_art_becomes_a_vector_block() {
        let mut c = prose(4);
        for i in 0..6 {
            c.push_str(&format!("400 {} m 440 {} l S\n", 300 + i * 4, 300 + i * 4));
        }
        let m = model_of(&c);
        assert!(m.pages[0].blocks.iter().any(|b| matches!(b, Block::Vector(_))));
    }

    #[test]
    fn a_table_claims_its_region_and_no_paragraph_duplicates_it() {
        let mut c = String::new();
        for x in [72.0, 172.0, 272.0, 372.0f64] {
            c.push_str(&format!("{x} 600 m {x} 700 l S\n"));
        }
        for y in [600.0, 633.0, 666.0, 700.0f64] {
            c.push_str(&format!("72 {y} m 372 {y} l S\n"));
        }
        for (r, y) in [680.0, 647.0, 614.0f64].iter().enumerate() {
            for (col, x) in [80.0, 180.0, 280.0f64].iter().enumerate() {
                c.push_str(&format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm (r{r}c{col}) Tj ET\n"));
            }
        }
        let m = model_of(&c);
        assert_eq!(m.pages[0].blocks.iter().filter(|b| matches!(b, Block::Table(_))).count(), 1);
        // The cell contents must not also appear as free paragraphs.
        let paras = m.pages[0].blocks.iter().filter(|b| matches!(b, Block::Paragraph(_))).count();
        assert_eq!(paras, 0, "the table's own text must not be duplicated as paragraphs");
    }

    #[test]
    fn unmapped_text_is_declined_rather_than_guessed() {
        // Spec 7.8: "Guessing is worse than declining." A font with no
        // /ToUnicode and no recognisable glyph names yields no characters, so
        // the region is preserved opaque instead of being read as a paragraph.
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(
                4,
                "",
                b"BT /F1 10 Tf 1 0 0 1 72 700 Tm <0001000200030004> Tj ET\n\
                  BT /F1 10 Tf 1 0 0 1 72 688 Tm <0005000600070008> Tj ET\n\
                  BT /F1 10 Tf 1 0 0 1 72 676 Tm <00090010001100120013> Tj ET\n",
            )
            // Identity-H over a private CID ordering, with no /ToUnicode. The
            // codes are CIDs into a subset font and mean nothing outside it, so
            // there is no strategy in §7.2 that can recover characters. A
            // /Differences encoding naming /g1 would *not* do -- §7.2 correctly
            // falls back to the character code through the base encoding, and
            // the text comes out readable.
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /AAAAAA+Custom \
                 /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
            )
            .object(
                6,
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /AAAAAA+Custom \
                 /CIDSystemInfo << /Registry (Private) /Ordering (Private) /Supplement 0 >> \
                 /FontDescriptor 7 0 R /DW 500 >>",
            )
            .object(
                7,
                "<< /Type /FontDescriptor /FontName /AAAAAA+Custom /Flags 4 /ItalicAngle 0 \
                 /Ascent 700 /Descent -200 /CapHeight 700 /StemV 80 \
                 /FontBBox [0 -200 1000 700] >>",
            )
            .finish("/Root 1 0 R");
        let m = model_of_bytes(bytes);
        let unknown: Vec<&Block> =
            m.pages[0].blocks.iter().filter(|b| matches!(b, Block::Unknown(_))).collect();
        assert_eq!(
            unknown.len(),
            1,
            "{:?}",
            m.pages[0].blocks.iter().map(|b| b.kind()).collect::<Vec<_>>()
        );
        let Block::Unknown(raw) = unknown[0] else { unreachable!() };
        assert_eq!(raw.reason, DeclineReason::Unmapped);
        assert!(!unknown[0].is_reflowable(), "opaque content is never reflowed");
        assert!(!raw.lines.is_empty(), "but it keeps its lines, so it can still be drawn");
    }

    #[test]
    fn rotated_text_is_declined_rather_than_measured_horizontally() {
        // Line assembly handles a rotated baseline correctly, but indent,
        // alignment and margin all assume a horizontal measure, so reading it
        // as a paragraph would report confident nonsense.
        let mut c = String::new();
        for i in 0..5 {
            c.push_str(&format!(
                "BT /F1 10 Tf 0 1 -1 0 {} 300 Tm (rotated text line) Tj ET\n",
                300 + i * 12
            ));
        }
        let m = model_of(&c);
        let unknown: Vec<&Block> =
            m.pages[0].blocks.iter().filter(|b| matches!(b, Block::Unknown(_))).collect();
        assert!(
            !unknown.is_empty(),
            "{:?}",
            m.pages[0].blocks.iter().map(|b| b.kind()).collect::<Vec<_>>()
        );
        let Block::Unknown(raw) = unknown[0] else { unreachable!() };
        assert_eq!(raw.reason, DeclineReason::NonHorizontal);
    }

    #[test]
    fn a_tagged_document_takes_its_reading_order_from_the_structure_tree() {
        // Two paragraphs whose /MCID order is the reverse of their geometric
        // order. The tree is authoritative, so the model must follow it.
        let content = "/P << /MCID 1 >> BDC BT /F1 10 Tf 1 0 0 1 72 700 Tm \
                       (this sits at the top of the page) Tj ET EMC\n\
                       /P << /MCID 0 >> BDC BT /F1 10 Tf 1 0 0 1 72 400 Tm \
                       (this sits much lower down again) Tj ET EMC\n";
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                     /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{}] >>",
                    "500 ".repeat(95)
                ),
            )
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /Document /P 7 0 R /K [9 0 R 10 0 R] >>")
            .object(9, "<< /S /P /P 8 0 R /Pg 3 0 R /K 0 >>")
            .object(10, "<< /S /P /P 8 0 R /Pg 3 0 R /K 1 >>")
            .finish("/Root 1 0 R");

        let m = model_of_bytes(bytes);
        assert!(m.structure.is_some(), "the document is tagged");
        assert_eq!(m.order_source, OrderSource::Structure);

        // MCID 0 is the lower block, and the tree lists it first.
        let first = m.in_reading_order().next().expect("a first block");
        let Block::Paragraph(p) = first.1 else { panic!("expected a paragraph first") };
        assert_eq!(p.mcid, Some(0), "the tree's order wins over the geometric one");
    }

    #[test]
    fn an_untagged_document_falls_back_to_geometry() {
        let m = model_of(&prose(6));
        assert_eq!(m.order_source, OrderSource::Geometry);
    }

    #[test]
    fn reading_order_covers_every_block_even_when_tagged() {
        // The tree names only some content; the rest must still appear.
        let content = "/P << /MCID 0 >> BDC BT /F1 10 Tf 1 0 0 1 72 700 Tm \
                       (tagged content on this line) Tj ET EMC\n\
                       BT /F1 10 Tf 1 0 0 1 72 400 Tm (untagged content down here) Tj ET\n";
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                &format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                     /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{}] >>",
                    "500 ".repeat(95)
                ),
            )
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /P /P 7 0 R /Pg 3 0 R /K 0 >>")
            .finish("/Root 1 0 R");

        let m = model_of_bytes(bytes);
        let total: usize = m.pages.iter().map(|p| p.blocks.len()).sum();
        assert_eq!(m.reading_order.len(), total, "untagged blocks are not dropped");
        let mut sorted = m.reading_order.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), total);
    }

    #[test]
    fn document_text_follows_reading_order() {
        let m = model_of(&prose(4));
        let text = m.text();
        assert!(text.contains("line 0"), "{text}");
        assert!(text.contains("line 3"), "{text}");
    }

    #[test]
    fn an_empty_page_yields_an_empty_model() {
        let m = model_of("");
        assert_eq!(m.pages.len(), 1);
        assert!(m.pages[0].blocks.is_empty());
        assert!(m.reading_order.is_empty());
        assert_eq!(m.text(), "");
    }

    #[test]
    fn page_geometry_is_carried_through() {
        let m = model_of(&prose(3));
        assert_eq!(m.pages[0].media_box, Rect::new(0.0, 0.0, 600.0, 800.0));
        assert_eq!(m.pages[0].rotate, 0);
    }
}
