//! Frame inference: where text is *allowed* to go.
//!
//! Step 3 of `docs/flow-model.md`, and the component that document mode cannot
//! be built without. A PDF says where every glyph *is*; it never says where the
//! producer would have put the next one. A column box, a margin, a text frame —
//! none of it is in the file at any level, so it has to be reconstructed from
//! the only evidence there is, which is where content landed.
//!
//! # One page is one sample
//!
//! The design claim that makes this tractable:
//!
//! > **Frames are a document-level property, not a page-level one.** A single
//! > page gives one sample of where a column's text happened to fall; twenty
//! > pages of the same column give twenty, and their union converges on the
//! > frame the producer actually used. Inferring per page is the obvious
//! > approach and the wrong one.
//!
//! The consequence is concrete. On one page the last line of a paragraph is
//! short, so the right edge of the text is wherever that line stopped —
//! anywhere up to the measure. Twenty pages of the same column produce twenty
//! right edges, and the largest of them is the measure. So this works on an
//! occupancy histogram accumulated across pages rather than on any page's own
//! geometry.
//!
//! # The signals, in the order the design gives them
//!
//! 1. **`/StructTreeRoot`** — the producer named the containers. Used here to
//!    tell body text from furniture, not for geometry: a structure tree carries
//!    no coordinates, so it cannot say where a frame is, only what belongs in
//!    one.
//! 2. **Repeated block extents across pages, clustered.** The main mechanism.
//! 3. **`/MediaBox` and `/CropBox`** — an outer bound that is always true, and
//!    the fallback when a page group has no text to measure.
//! 4. **Ruling lines and table grids** — hard boundaries content did not cross.
//!    Not used yet; `rules::collect` produces them and they are a refinement
//!    rather than a foundation.
//!
//! # What this deliberately does not do
//!
//! It does not decide reading order between frames, and it does not place
//! anything. Both belong to the layout engine, which is step 5. A frame here is
//! a rectangle and a count of what fell inside it, and the report says how much
//! of the document that accounted for — because a frame set that contains 60%
//! of the text is not a frame set, and the only way to know is to measure.

use crate::model::{Block, DocumentModel};
use rasura_content::matrix::Rect;

/// Bin width for the occupancy histogram, in points.
///
/// One point. Finer buys nothing — no producer positions a column to better
/// than a point — and coarser starts merging a narrow gutter into its columns,
/// which is the one mistake that changes the answer rather than blurring it.
const BIN: f64 = 1.0;

/// How the frames of a page group were arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Accumulated over several pages, which is the case the method is designed
    /// for: the union across pages converges on the measure.
    Pages(usize),
    /// One page of evidence. The frame is where the text on that page happened
    /// to fall, which may be narrower than the measure the producer used —
    /// there is no second sample to widen it.
    SinglePage,
    /// No text to measure, so the page box stands in. Always true and never
    /// tight.
    PageBox,
    /// Not inferred at all: the caller said where the text goes.
    ///
    /// Composition rather than analysis. There is no document to measure — the
    /// frame is a decision, not a finding — and calling that `Pages(1)` would
    /// put a designed page and a one-page sample in the same bucket when they
    /// mean opposite things about how much to trust the number.
    Designed,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Evidence::Pages(_) => "repeated across pages",
            Evidence::SinglePage => "one page only",
            Evidence::PageBox => "the page box, for want of any text",
            Evidence::Designed => "specified by the caller",
        }
    }
}

/// A region text is allowed to occupy.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub rect: Rect,
    /// Position left to right within the page group, from zero.
    pub column: usize,
    /// How many blocks fell inside it, across every page of the group.
    pub blocks: usize,
    pub evidence: Evidence,
}

/// Pages that share a size, and the frames inferred for them.
///
/// Grouped by size because a document whose pages differ — a landscape plate in
/// a portrait report, a cover at a different trim — has different frames on
/// them, and averaging the two produces a frame that fits neither.
#[derive(Debug, Clone)]
pub struct PageGroup {
    pub pages: Vec<usize>,
    /// The group's page size in points, rounded to the nearest point.
    pub size: (f64, f64),
    /// Left to right.
    pub frames: Vec<Frame>,
}

/// Every frame in a document.
#[derive(Debug, Clone, Default)]
pub struct FrameSet {
    pub groups: Vec<PageGroup>,
}

/// A page to be composed, rather than one to be measured.
///
/// The inference in this module answers "where does the text on this document's
/// pages actually sit". Composition asks the opposite question — "where should
/// text go on a page I am about to make" — and there is nothing to infer from,
/// so it is stated.
///
/// Points throughout, as everything in PDF is: 72 to the inch.
#[derive(Debug, Clone, PartialEq)]
pub struct PageGeometry {
    /// Width and height of the page box.
    pub size: (f64, f64),
    /// Top, right, bottom, left — clockwise from the top, as in CSS, because
    /// every other ordering has to be looked up every time.
    pub margins: (f64, f64, f64, f64),
    /// One or more, left to right.
    pub columns: usize,
    /// The space between columns. Ignored for a single column.
    pub gutter: f64,
}

impl Default for PageGeometry {
    fn default() -> PageGeometry {
        PageGeometry::us_letter()
    }
}

impl PageGeometry {
    /// 612 × 792 points, one inch of margin, one column.
    pub fn us_letter() -> PageGeometry {
        PageGeometry {
            size: (612.0, 792.0),
            margins: (72.0, 72.0, 72.0, 72.0),
            columns: 1,
            gutter: 18.0,
        }
    }

    /// 595 × 842 points — A4 — with the same inch of margin.
    pub fn a4() -> PageGeometry {
        PageGeometry { size: (595.0, 842.0), ..PageGeometry::us_letter() }
    }

    /// The same page in `columns` columns.
    pub fn with_columns(mut self, columns: usize) -> PageGeometry {
        self.columns = columns.max(1);
        self
    }

    /// The same page with every margin set to `points`.
    pub fn with_margin(mut self, points: f64) -> PageGeometry {
        self.margins = (points, points, points, points);
        self
    }

    /// The text frames this geometry describes, left to right.
    ///
    /// In the same downward-y space the layout engine works in: `y0` is the top
    /// of the frame and `y1` the bottom, which is *not* PDF page space. The
    /// emitter converts when it computes a baseline, and doing it here as well
    /// would flip the page twice.
    pub fn frames(&self) -> Vec<Frame> {
        let (top, right, bottom, left) = self.margins;
        let columns = self.columns.max(1);

        // A geometry whose margins exceed the page has no text area at all.
        // Clamped rather than refused: the caller gets an empty frame and a
        // layout with nothing placed, which is visible, rather than an error
        // from a function that only does arithmetic.
        let measure = (self.size.0 - left - right).max(0.0);
        let depth = (self.size.1 - top - bottom).max(0.0);
        let gutter = if columns > 1 { self.gutter } else { 0.0 };
        let column_width = ((measure - gutter * (columns - 1) as f64) / columns as f64).max(0.0);

        (0..columns)
            .map(|i| {
                let x0 = left + i as f64 * (column_width + gutter);
                Frame {
                    rect: Rect::new(x0, top, x0 + column_width, top + depth),
                    column: i,
                    blocks: 0,
                    evidence: Evidence::Designed,
                }
            })
            .collect()
    }
}

impl FrameSet {
    /// A frame set for pages that do not exist yet.
    ///
    /// One group, applying to every page: a composed document's page count is
    /// whatever the text turns out to need, so listing pages in advance would
    /// be inventing a number. [`FrameSet::frames_for`] reads an empty page list
    /// as "all of them" for exactly this case.
    pub fn designed(geometry: &PageGeometry) -> FrameSet {
        FrameSet {
            groups: vec![PageGroup {
                pages: Vec::new(),
                size: geometry.size,
                frames: geometry.frames(),
            }],
        }
    }

    /// The frames that apply to a page.
    ///
    /// A group with no pages listed applies to every page — that is what
    /// [`FrameSet::designed`] produces, and inference never produces it, since
    /// an inferred group exists because pages were measured into it.
    pub fn frames_for(&self, page: usize) -> &[Frame] {
        self.groups
            .iter()
            .find(|g| g.pages.contains(&page) || g.pages.is_empty())
            .map(|g| g.frames.as_slice())
            .unwrap_or(&[])
    }

    /// The frame a rectangle belongs to, by largest horizontal overlap.
    ///
    /// Horizontal rather than by area: a full-width heading overlaps every
    /// column vertically and belongs to the one it starts in, and a block that
    /// happens to be tall should not out-vote one that is in the right place.
    pub fn frame_for(&self, page: usize, rect: Rect) -> Option<&Frame> {
        self.frames_for(page)
            .iter()
            .map(|f| (f, overlap(f.rect.x0, f.rect.x1, rect.x0, rect.x1)))
            .filter(|(_, o)| *o > 0.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(f, _)| f)
    }

    pub fn total_frames(&self) -> usize {
        self.groups.iter().map(|g| g.frames.len()).sum()
    }

    /// The page group to lay new pages out into.
    ///
    /// The one with the most pages, because a document mode that re-paginates
    /// produces a different number of pages than it started with and has to
    /// choose a shape for the ones it makes. The dominant group is the
    /// document's own answer to "what does a page of this look like"; a cover
    /// or a landscape plate is not.
    pub fn template(&self) -> Option<&PageGroup> {
        self.groups.iter().max_by_key(|g| g.pages.len())
    }
}

/// How the inference should behave.
#[derive(Debug, Clone)]
pub struct Options {
    /// A gap narrower than this is paragraph spacing, not a gutter, in points.
    ///
    /// Six points is about half a line at body size. Below that, a column break
    /// would be invisible to a reader, which is a good reason not to claim one
    /// is there.
    pub min_gutter: f64,
    /// A column narrower than this is noise: a page number, a marginal rule, a
    /// stray glyph.
    pub min_column_width: f64,
    /// A bin must be occupied on this fraction of the group's pages to count as
    /// inside a frame.
    ///
    /// Zero would mean pure union, which is what the design describes and what
    /// a full-width heading breaks: a heading spanning two columns puts content
    /// in the gutter, and one such page would merge the columns for the whole
    /// document. A quarter tolerates that while still taking the union of the
    /// ragged right edges the method exists to resolve.
    pub occupancy_fraction: f64,
    /// Include tables when measuring. On: a table is text in a frame.
    pub include_tables: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_gutter: 6.0,
            min_column_width: 24.0,
            occupancy_fraction: 0.25,
            include_tables: true,
        }
    }
}

/// What the inference managed, and what it did not.
///
/// The measurement `docs/flow-model.md` asks for — "measured on tagged
/// documents where the answer is known" — comes down to two numbers a
/// reconstruction cannot fake. **Containment** is what fraction of the text
/// landed inside a frame; a frame set that misses a fifth of the document is
/// wrong however plausible its columns look. **Tightness** is frame area over
/// content area; a single frame the size of the page contains everything and
/// says nothing, and this is the number that catches it.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub blocks_considered: usize,
    /// Blocks whose rectangle lies inside some frame.
    pub blocks_contained: usize,
    /// Blocks that cover more than one frame.
    ///
    /// A full-width heading over a two-column body is the ordinary case, and it
    /// is *not* a failure: the producer meant it to span, and a layout engine
    /// has to be told so. Counted apart from `blocks_loose` because lumping the
    /// two together understates the method — the first run of this measurement
    /// reported 41 documents "below 90% containment" when most of what it was
    /// counting was headings behaving exactly as headings do.
    pub blocks_spanning: usize,
    /// Blocks that overlap one frame without fitting inside it, or no frame at
    /// all. The genuine misses.
    pub blocks_loose: usize,
    /// Frame area divided by the area of the blocks inside them. 1.0 is
    /// impossibly tight; a page-sized frame around a narrow column is 3 or 4.
    pub tightness: f64,
    pub groups: usize,
    pub frames: usize,
    /// Page groups that fell back to the page box.
    pub fallbacks: usize,
    /// Blocks lying partly or wholly outside the page box.
    ///
    /// The page box is supposed to be an outer bound. Eleven corpus files put
    /// text outside it, so this is counted rather than assumed away — content a
    /// reader cannot see is still content the model has to know about.
    pub blocks_outside_page_box: usize,
}

impl Report {
    pub fn containment(&self) -> f64 {
        if self.blocks_considered == 0 {
            return 1.0;
        }
        // Spanning blocks count as placed: they are where the producer put
        // them, across frames the method correctly separated.
        (self.blocks_contained + self.blocks_spanning) as f64 / self.blocks_considered as f64
    }

    /// Blocks inside exactly one frame, ignoring the ones that span.
    pub fn strict_containment(&self) -> f64 {
        if self.blocks_considered == 0 {
            return 1.0;
        }
        self.blocks_contained as f64 / self.blocks_considered as f64
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{} frame(s) in {} page group(s); {:.0}% of blocks contained, tightness {:.2}",
            self.frames,
            self.groups,
            self.containment() * 100.0,
            self.tightness
        )];
        if self.blocks_spanning > 0 {
            out.push(format!("{} block(s) span more than one frame", self.blocks_spanning));
        }
        if self.blocks_loose > 0 {
            out.push(format!("{} block(s) fit no frame", self.blocks_loose));
        }
        if self.fallbacks > 0 {
            out.push(format!(
                "{} page group(s) had no text and fell back to the page box",
                self.fallbacks
            ));
        }
        out
    }
}

/// Infer the frames of a document.
pub fn infer(model: &DocumentModel, opts: &Options) -> (FrameSet, Report) {
    let mut report = Report::default();
    let mut set = FrameSet::default();

    for (size, pages) in group_by_size(model) {
        let group = infer_group(model, &pages, size, opts, &mut report);
        set.groups.push(group);
    }

    report.groups = set.groups.len();
    report.frames = set.total_frames();
    score(model, &set, opts, &mut report);
    (set, report)
}

/// Pages that share a size, in page order.
///
/// Rounded to the point: a producer that writes 611.976 on one page and 612 on
/// another means the same size, and a group per rounding error would defeat the
/// whole method.
fn group_by_size(model: &DocumentModel) -> Vec<((f64, f64), Vec<usize>)> {
    let mut groups: Vec<((f64, f64), Vec<usize>)> = Vec::new();
    for (index, page) in model.pages.iter().enumerate() {
        let box_ = page_box(page);
        let size = (box_.width().abs().round(), box_.height().abs().round());
        match groups.iter_mut().find(|(s, _)| *s == size) {
            Some((_, pages)) => pages.push(index),
            None => groups.push((size, vec![index])),
        }
    }
    groups
}

/// The page's own bounds: the crop box where it has one, the media box
/// otherwise.
///
/// ISO 32000-1 §14.11.2: the crop box is what a viewer displays, so it is the
/// bound a reader actually sees. A media box larger than it includes printer
/// marks and bleed, which no text frame extends into.
fn page_box(page: &crate::model::PageModel) -> Rect {
    if page.crop_box.width().abs() > 0.0 && page.crop_box.height().abs() > 0.0 {
        page.crop_box
    } else {
        page.media_box
    }
}

fn infer_group(
    model: &DocumentModel,
    pages: &[usize],
    size: (f64, f64),
    opts: &Options,
    report: &mut Report,
) -> PageGroup {
    // The page box in device space, which starts at the origin whatever the
    // crop box's own coordinates are: the page's base transform translates it
    // there.
    let page_bounds = Rect::new(0.0, 0.0, size.0, size.1);

    // The domain the histogram spans: the page box *and* whatever content falls
    // outside it.
    //
    // `docs/flow-model.md` lists the page box as "an outer bound that is always
    // true". On the corpus it is not. `endchar.pdf` has a 15×34pt crop box with
    // its text 260 points to the left of it, and clamping the domain to the box
    // put every block outside every frame — eleven files scored 0% containment
    // for this reason alone. Content outside the page box is content a reader
    // cannot see, and a model that omits it is still wrong about what is there.
    let mut domain = page_bounds;
    let mut outside = 0usize;
    for &index in pages {
        let Some(page) = model.pages.get(index) else { continue };
        for block in page.blocks.iter().filter(|b| is_body(b, opts)) {
            let b = block.bbox();
            if b.x0 < page_bounds.x0 || b.x1 > page_bounds.x1 {
                outside += 1;
            }
            domain = domain.union(&b);
        }
    }
    report.blocks_outside_page_box += outside;

    // Occupancy per bin, counted in *pages* rather than in blocks. A page with
    // forty paragraphs in one column should not outvote nineteen pages with
    // four, and the question being asked is "does the producer put text here",
    // which each page answers once.
    let bin_count = ((domain.width() / BIN).ceil() as usize).max(1);
    let mut occupancy = vec![0usize; bin_count];
    let mut measured_pages = 0usize;

    for &index in pages {
        let Some(page) = model.pages.get(index) else { continue };
        let mut seen = vec![false; bin_count];
        let mut any = false;
        for block in page.blocks.iter().filter(|b| is_body(b, opts)) {
            any = true;
            let (from, to) = bin_range(block.bbox(), domain.x0, bin_count);
            for bin in seen.iter_mut().take(to).skip(from) {
                *bin = true;
            }
        }
        if any {
            measured_pages += 1;
            for (bin, hit) in occupancy.iter_mut().zip(seen) {
                *bin += usize::from(hit);
            }
        }
    }

    if measured_pages == 0 {
        report.fallbacks += 1;
        return PageGroup {
            pages: pages.to_vec(),
            size,
            frames: vec![Frame {
                rect: page_bounds,
                column: 0,
                blocks: 0,
                evidence: Evidence::PageBox,
            }],
        };
    }

    let threshold = (measured_pages as f64 * opts.occupancy_fraction).ceil().max(1.0) as usize;
    let runs = columns(&occupancy, threshold, opts);

    let evidence =
        if measured_pages > 1 { Evidence::Pages(measured_pages) } else { Evidence::SinglePage };

    // Vertical extent per column, from the blocks that land in it. Taken across
    // every page of the group for the same reason as the horizontal extent: one
    // page's first paragraph may start below the top of the frame.
    let mut frames: Vec<Frame> = Vec::new();
    for (column, (from, to)) in runs.iter().enumerate() {
        let x0 = domain.x0 + *from as f64 * BIN;
        let x1 = domain.x0 + *to as f64 * BIN;
        let mut top = f64::MAX;
        let mut bottom = f64::MIN;
        let mut count = 0usize;

        for &index in pages {
            let Some(page) = model.pages.get(index) else { continue };
            for block in page.blocks.iter().filter(|b| is_body(b, opts)) {
                let b = block.bbox();
                // Assigned by majority overlap so a block belongs to one column,
                // and only its *vertical* extent is taken: the horizontal edges
                // come from the histogram, which knows about every page, and a
                // single wide block must not widen the column.
                if overlap(x0, x1, b.x0, b.x1) <= 0.0 {
                    continue;
                }
                if best_column(&runs, domain.x0, b) != Some(column) {
                    continue;
                }
                top = top.min(b.y0);
                bottom = bottom.max(b.y1);
                count += 1;
            }
        }

        if count == 0 || !top.is_finite() || !bottom.is_finite() {
            continue;
        }
        frames.push(Frame {
            rect: Rect::new(x0, top, x1, bottom),
            column,
            blocks: count,
            evidence,
        });
    }

    if frames.is_empty() {
        report.fallbacks += 1;
        frames.push(Frame { rect: page_bounds, column: 0, blocks: 0, evidence: Evidence::PageBox });
    }

    PageGroup { pages: pages.to_vec(), size, frames }
}

/// Whether a block is text that lives in a frame.
///
/// Running headers and footers are excluded: they are furniture the producer's
/// layout painted outside the text frame, and including them would stretch
/// every frame to the full height of the page. Images and vector art are
/// excluded because a figure may legitimately sit outside the measure — and
/// including them would let one full-bleed photograph define the frame for the
/// whole document.
fn is_body(block: &Block, opts: &Options) -> bool {
    match block {
        Block::Paragraph(_) => true,
        Block::Table(_) => opts.include_tables,
        // Spec 7.8's opaque content is still text on the page, and a layout
        // engine has to leave room for it.
        Block::Unknown(_) => true,
        Block::Running(_) | Block::Image(_) | Block::Vector(_) => false,
    }
}

fn bin_range(rect: Rect, origin: f64, bins: usize) -> (usize, usize) {
    let from = (((rect.x0 - origin).max(0.0)) / BIN).floor() as usize;
    let to = (((rect.x1 - origin).max(0.0)) / BIN).ceil() as usize;
    (from.min(bins), to.min(bins))
}

/// Runs of occupied bins, merged across narrow gaps and stripped of noise.
fn columns(occupancy: &[usize], threshold: usize, opts: &Options) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;

    for (i, count) in occupancy.iter().enumerate() {
        if *count >= threshold {
            start.get_or_insert(i);
        } else if let Some(from) = start.take() {
            runs.push((from, i));
        }
    }
    if let Some(from) = start {
        runs.push((from, occupancy.len()));
    }

    // A gap narrower than a gutter is the space between two paragraphs that
    // happen not to reach the same width, not a column boundary.
    let min_gutter_bins = (opts.min_gutter / BIN).round() as usize;
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for run in runs {
        match merged.last_mut() {
            Some(last) if run.0.saturating_sub(last.1) < min_gutter_bins => last.1 = run.1,
            _ => merged.push(run),
        }
    }

    // A run narrower than a column is a fragment, not noise: something was
    // painted there. Dropping it outright left those blocks in no frame at all
    // — three of issue925.pdf's four blocks, and a long tail across the
    // corpus. So a narrow run is folded into whichever neighbour is closer, and
    // only a narrow run with no neighbour survives on its own.
    let min_width_bins = (opts.min_column_width / BIN).round() as usize;
    let mut out = merged;
    loop {
        if out.len() < 2 {
            break;
        }
        let Some(i) = out.iter().position(|(a, b)| b.saturating_sub(*a) < min_width_bins) else {
            break;
        };
        // Folded into whichever neighbour is *closer*, not simply the previous
        // one: a fragment to the left of the first column has no previous run,
        // and looking only backwards left it as a column of its own — which is
        // what this rule exists to prevent.
        let left_gap = if i > 0 { out[i].0.saturating_sub(out[i - 1].1) } else { usize::MAX };
        let right_gap =
            if i + 1 < out.len() { out[i + 1].0.saturating_sub(out[i].1) } else { usize::MAX };

        if left_gap <= right_gap {
            out[i - 1].1 = out[i].1;
        } else {
            out[i + 1].0 = out[i].0;
        }
        out.remove(i);
    }
    out
}

fn best_column(runs: &[(usize, usize)], origin: f64, rect: Rect) -> Option<usize> {
    runs.iter()
        .enumerate()
        .map(|(i, (from, to))| {
            (i, overlap(origin + *from as f64 * BIN, origin + *to as f64 * BIN, rect.x0, rect.x1))
        })
        .filter(|(_, o)| *o > 0.0)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Containment and tightness, over every body block in the document.
fn score(model: &DocumentModel, set: &FrameSet, opts: &Options, report: &mut Report) {
    let mut content_area = 0.0;
    for (index, page) in model.pages.iter().enumerate() {
        for block in page.blocks.iter().filter(|b| is_body(b, opts)) {
            let b = block.bbox();
            report.blocks_considered += 1;
            content_area += (b.width() * b.height()).abs();

            // Spanning is decided before containment: a block covering two
            // frames is not loose, it is wide.
            let covered = set
                .frames_for(index)
                .iter()
                .filter(|f| overlap(f.rect.x0, f.rect.x1, b.x0, b.x1) > 1.0)
                .count();
            if covered > 1 {
                report.blocks_spanning += 1;
                continue;
            }

            match set.frame_for(index, b) {
                Some(frame) => {
                    // A small tolerance: a frame's edge comes from a bin
                    // boundary and a block's from a glyph's advance, and
                    // demanding exact containment would fail on rounding.
                    let r = frame.rect;
                    if b.x0 >= r.x0 - 1.0
                        && b.x1 <= r.x1 + 1.0
                        && b.y0 >= r.y0 - 1.0
                        && b.y1 <= r.y1 + 1.0
                    {
                        report.blocks_contained += 1;
                    } else {
                        report.blocks_loose += 1;
                    }
                }
                None => report.blocks_loose += 1,
            }
        }
    }

    let frame_area: f64 = set
        .groups
        .iter()
        .flat_map(|g| g.frames.iter().map(move |f| (f, g.pages.len())))
        .map(|(f, pages)| (f.rect.width() * f.rect.height()).abs() * pages as f64)
        .sum();

    report.tightness = if content_area > 0.0 { frame_area / content_area } else { 0.0 };
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::Document;
    use rasura_cos::testutil::ClassicBuilder;

    /// A document from per-page content streams, 612x792.
    fn document(pages: &[String]) -> DocumentModel {
        let n = pages.len() as u32;
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + i * 2)).collect();

        let mut b = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, &format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")))
            .object(
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            );

        for (i, content) in pages.iter().enumerate() {
            let page = 4 + i as u32 * 2;
            b = b
                .object(
                    page,
                    &format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                         /Resources << /Font << /F1 3 0 R >> >> >>",
                        page + 1
                    ),
                )
                .stream(page + 1, "", content.as_bytes());
        }

        let doc = Document::open(b.finish("/Root 1 0 R")).expect("open");
        crate::model::analyse(&doc).expect("analyse")
    }

    /// Lines of text at a given left edge and width, in user-space coordinates.
    ///
    /// The width is honoured rather than approximated: `n` and a space in
    /// 10pt Helvetica are 5.56 and 2.78 points, so a repetition is 8.34 and the
    /// count follows from the measure. Getting this wrong is not a small error —
    /// the first version of this helper produced text two-thirds wider than it
    /// claimed, which merged the two columns of the fixture below and made the
    /// test look like an algorithm failure.
    fn column(x: f64, width: f64, top: f64, lines: usize) -> String {
        const PER_REPETITION: f64 = 5.56 + 2.78;
        let full = ((width + 2.78) / PER_REPETITION).floor().max(2.0) as usize;

        let mut out = String::new();
        for i in 0..lines {
            let y = top - i as f64 * 14.0;
            // Every fifth line is short, which is the whole reason a single
            // page cannot be trusted to give the measure.
            let reps = if i % 5 == 4 { (full / 3).max(2) } else { full };
            out.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET\n",
                "n ".repeat(reps).trim_end()
            ));
        }
        out
    }

    #[test]
    fn a_single_column_document_yields_one_frame() {
        let pages: Vec<String> = (0..4).map(|_| column(72.0, 468.0, 700.0, 20)).collect();
        let (frames, report) = infer(&document(&pages), &Options::default());

        assert_eq!(frames.groups.len(), 1, "one page size");
        let frames = &frames.groups[0].frames;
        assert_eq!(frames.len(), 1, "{frames:#?}");
        assert!(frames[0].rect.x0 >= 70.0 && frames[0].rect.x0 <= 74.0, "{:?}", frames[0].rect);
        assert!(report.containment() > 0.99, "{report:?}");
    }

    #[test]
    fn two_columns_are_separated_by_their_gutter() {
        // The case the whole method exists for. A gutter is a vertical band no
        // page puts text in; a paragraph gap is not.
        let pages: Vec<String> = (0..6)
            .map(|_| {
                format!("{}{}", column(72.0, 220.0, 700.0, 24), column(320.0, 220.0, 700.0, 24))
            })
            .collect();

        let (set, report) = infer(&document(&pages), &Options::default());
        let frames = &set.groups[0].frames;
        assert_eq!(frames.len(), 2, "{frames:#?}");

        assert!(frames[0].rect.x1 < frames[1].rect.x0, "they do not overlap: {frames:#?}");
        let gutter = frames[1].rect.x0 - frames[0].rect.x1;
        assert!(gutter >= 6.0, "the gutter survives: {gutter}");
        assert!(report.containment() > 0.95, "{report:?}");
        assert_eq!(frames[0].column, 0);
        assert_eq!(frames[1].column, 1);
    }

    #[test]
    fn the_frame_is_the_union_across_pages_not_one_pages_extent() {
        // The claim from `docs/flow-model.md` stated as a test: page one's text
        // is short, page two's reaches the measure, and the frame is the
        // measure rather than the average or the first sample.
        let narrow = column(72.0, 200.0, 700.0, 6);
        let wide = column(72.0, 460.0, 700.0, 6);

        let (one_page, _) = infer(&document(std::slice::from_ref(&narrow)), &Options::default());
        let (both, _) = infer(&document(&[narrow, wide]), &Options::default());

        let short = one_page.groups[0].frames[0].rect.x1;
        let long = both.groups[0].frames[0].rect.x1;
        assert!(long > short + 100.0, "one page gave {short}, two gave {long}");
    }

    #[test]
    fn one_page_of_evidence_says_so() {
        let (set, _) = infer(&document(&[column(72.0, 400.0, 700.0, 8)]), &Options::default());
        assert_eq!(set.groups[0].frames[0].evidence, Evidence::SinglePage);

        let (set, _) = infer(
            &document(&[column(72.0, 400.0, 700.0, 8), column(72.0, 400.0, 700.0, 8)]),
            &Options::default(),
        );
        assert_eq!(set.groups[0].frames[0].evidence, Evidence::Pages(2));
    }

    #[test]
    fn a_full_width_heading_does_not_merge_two_columns() {
        // The failure mode pure union has: a heading crossing the gutter on one
        // page in eight would join the columns for the whole document.
        let mut pages: Vec<String> = (0..8)
            .map(|_| {
                format!("{}{}", column(72.0, 220.0, 700.0, 24), column(320.0, 220.0, 700.0, 24))
            })
            .collect();
        pages[0] = format!(
            "BT /F1 18 Tf 1 0 0 1 72 740 Tm ({}) Tj ET\n{}",
            "wide heading across the whole measure", pages[0]
        );

        let (set, _) = infer(&document(&pages), &Options::default());
        assert_eq!(set.groups[0].frames.len(), 2, "{:#?}", set.groups[0].frames);
    }

    #[test]
    fn pages_of_different_sizes_get_their_own_frames() {
        // Averaging a landscape plate with a portrait report gives a frame that
        // fits neither.
        let portrait = column(72.0, 400.0, 700.0, 10);
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>")
            .object(
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            )
            .object(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
            )
            .stream(5, "", portrait.as_bytes())
            .object(
                6,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Contents 7 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
            )
            .stream(7, "", column(100.0, 600.0, 500.0, 8).as_bytes())
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let model = crate::model::analyse(&doc).expect("analyse");
        let (set, report) = infer(&model, &Options::default());

        assert_eq!(set.groups.len(), 2, "{:#?}", set.groups);
        assert_eq!(report.groups, 2);
        assert_ne!(set.groups[0].size, set.groups[1].size);
    }

    #[test]
    fn a_page_with_no_text_falls_back_to_the_page_box_and_reports_it() {
        let (set, report) =
            infer(&document(&[String::from("0 0 10 10 re f\n")]), &Options::default());
        let frame = &set.groups[0].frames[0];
        assert_eq!(frame.evidence, Evidence::PageBox);
        assert_eq!(report.fallbacks, 1);
        assert!(frame.rect.width() > 600.0, "{:?}", frame.rect);
    }

    #[test]
    fn a_narrow_fragment_joins_a_frame_rather_than_being_discarded() {
        // A stray character in its own x-range is narrower than a column and is
        // still content. Discarding it as noise left three of `issue925.pdf`'s
        // four blocks in no frame at all; folding it into its neighbour took
        // corpus containment from 97.4% to 99.6% and *raised* multi-column
        // detection, so the filter it replaced was costing accuracy rather than
        // buying it.
        let page = format!("{}{}", column(200.0, 300.0, 700.0, 8), column(72.0, 3.0, 600.0, 1));
        let (set, report) = infer(&document(&[page]), &Options::default());

        assert!(report.containment() > 0.99, "the fragment is framed: {report:?}");
        let frames = &set.groups[0].frames;
        assert_eq!(frames.len(), 1, "and did not become a column of its own: {frames:#?}");
        assert!(frames[0].rect.x0 <= 73.0, "the frame reaches it: {:?}", frames[0].rect);
    }

    #[test]
    fn content_outside_the_page_box_is_still_framed() {
        // `docs/flow-model.md` lists the page box as "an outer bound that is
        // always true". Eleven corpus files disagree — `endchar.pdf` has a
        // 15x34pt crop box and its text 260 points to the left of it — and
        // clamping the histogram to the box put every block outside every
        // frame. Found by the corpus survey scoring those files at 0%.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            )
            .object(
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /CropBox [400 700 460 760] /Contents 5 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
            )
            // Well to the left of the crop box, in the media box's coordinates.
            .stream(5, "", column(72.0, 300.0, 730.0, 3).as_bytes())
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let model = crate::model::analyse(&doc).expect("analyse");
        let (set, report) = infer(&model, &Options::default());

        assert!(report.blocks_outside_page_box > 0, "the fixture puts text outside the box");
        assert!(report.containment() > 0.99, "and it is still framed: {report:?}");
        let frame = &set.groups[0].frames[0];
        assert!(frame.rect.width() > 100.0, "the frame covers the text, not the box: {frame:?}");
    }

    #[test]
    fn tightness_catches_a_frame_that_is_merely_the_page() {
        // A frame the size of the page contains everything and says nothing.
        // Containment cannot tell the difference; this is the number that can.
        let narrow: Vec<String> = (0..4).map(|_| column(72.0, 200.0, 700.0, 20)).collect();
        let (_, tight) = infer(&document(&narrow), &Options::default());

        let (_, loose) = infer(&document(&[String::from("0 0 1 1 re f\n")]), &Options::default());
        assert!(tight.tightness < 3.0, "a measured frame is close to its content: {tight:?}");
        assert!(loose.tightness == 0.0 || loose.tightness > 10.0, "{loose:?}");
    }
}
