//! Page tree traversal and attribute inheritance. Spec 6.4.
//!
//! `/Resources`, `/MediaBox`, `/CropBox` and `/Rotate` are inheritable: a page
//! that does not carry one takes it from the nearest ancestor that does. This is
//! the first place in the stack that knows what a page is -- `rasura-cos`
//! deliberately does not, because a page tree is structure rather than objects.
//!
//! The tree is walked defensively. A `/Kids` array that points back at an
//! ancestor, a `/Count` that disagrees with reality, a node with no `/Type`, and
//! a page tree a thousand levels deep are all things real files contain.

use crate::matrix::{Matrix, Rect};
use rasura_cos::document::Document;
use rasura_cos::error::{Leniency, LeniencyKind};
use rasura_cos::{CosError, Dictionary, ObjId, Object, Result};
use std::collections::HashSet;
use std::sync::Arc;

/// A page tree nested deeper than this is malformed, not deep.
const MAX_TREE_DEPTH: usize = 64;

/// Hard ceiling on pages, so a cyclic tree that evades the visited set still
/// terminates. Spec 14.1 wants a 10k-page document to work, so this is well
/// above anything real.
const MAX_PAGES: usize = 1_000_000;

/// The attributes a page inherits from its ancestors. ISO 32000-1 Table 30.
#[derive(Debug, Clone, Default)]
struct Inherited {
    resources: Option<Arc<Object>>,
    media_box: Option<Rect>,
    crop_box: Option<Rect>,
    rotate: Option<i32>,
}

impl Inherited {
    /// Overlay whatever this node defines onto what it inherited.
    fn refine(&self, doc: &Document, node: &Dictionary) -> Inherited {
        let mut out = self.clone();
        if let Ok(Some(r)) = doc.get_entry(node, "Resources")
            && r.as_dict().is_some()
        {
            out.resources = Some(r);
        }
        if let Some(r) = rect_entry(doc, node, "MediaBox") {
            out.media_box = Some(r);
        }
        if let Some(r) = rect_entry(doc, node, "CropBox") {
            out.crop_box = Some(r);
        }
        if let Ok(Some(v)) = doc.get_entry(node, "Rotate")
            && let Some(n) = v.as_i64()
        {
            out.rotate = Some(n as i32);
        }
        out
    }
}

fn rect_entry(doc: &Document, dict: &Dictionary, key: &str) -> Option<Rect> {
    let v = doc.get_entry(dict, key).ok()??;
    let items = v.as_array()?;
    // Entries may themselves be indirect, which is rare but legal.
    let resolved: Vec<Object> = items
        .iter()
        .map(|o| doc.resolve(o).map(|a| (*a).clone()).unwrap_or(Object::Null))
        .collect();
    Rect::from_array(&resolved).filter(|r| !r.is_empty())
}

/// One page, with its inheritance already resolved.
#[derive(Debug, Clone)]
pub struct Page {
    /// Zero-based index in reading order.
    pub index: usize,
    pub id: ObjId,
    pub dict: Dictionary,
    /// Always present: defaults to US Letter when the tree supplies none, which
    /// is what viewers do.
    pub media_box: Rect,
    /// Defaults to the media box, per ISO 32000-1 §14.11.2.
    pub crop_box: Rect,
    /// Normalised to 0, 90, 180 or 270.
    pub rotate: i32,
    /// The inherited `/Resources` dictionary, if any.
    pub resources: Option<Arc<Object>>,
    /// The `/Pages` node that lists this page, and its slot in that node's
    /// `/Kids`.
    ///
    /// Needed by anything that changes the page tree: removing a page means
    /// removing exactly one entry from exactly one `/Kids` array, and the tree
    /// walk is the only thing that knows which. `None` when the page was found
    /// by scanning rather than by walking — a recovered tree has no reliable
    /// parent to edit.
    pub parent: Option<(ObjId, usize)>,
}

impl Page {
    /// The visible box: `/CropBox` intersected with `/MediaBox`.
    pub fn visible_box(&self) -> Rect {
        let c = self.crop_box;
        let m = self.media_box;
        let r = Rect::new(c.x0.max(m.x0), c.y0.max(m.y0), c.x1.min(m.x1), c.y1.min(m.y1));
        if r.is_empty() { m } else { r }
    }

    /// Size in points after rotation, which is what a viewer lays out.
    pub fn display_size(&self) -> (f64, f64) {
        let b = self.visible_box();
        if self.rotate % 180 == 0 { (b.width(), b.height()) } else { (b.height(), b.width()) }
    }

    /// Maps PDF user space to a y-down device space whose origin is the top
    /// left of the displayed page, with `/Rotate` applied.
    ///
    /// PDF user space has its origin at the bottom left with y increasing
    /// upwards; every raster target and every browser coordinate system is the
    /// other way up. Folding the flip into the base CTM means nothing above this
    /// layer has to remember to do it.
    pub fn base_ctm(&self) -> Matrix {
        let b = self.visible_box();
        // Move the crop box origin to (0, 0), then flip y.
        let normalise = Matrix::translate(-b.x0, -b.y0).then(&Matrix::new(
            1.0,
            0.0,
            0.0,
            -1.0,
            0.0,
            b.height(),
        ));

        let (w, h) = (b.width(), b.height());
        let rotation = match self.rotate.rem_euclid(360) {
            90 => Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
            180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
            270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
            _ => Matrix::IDENTITY,
        };
        normalise.then(&rotation)
    }
}

/// The result of walking the page tree.
pub struct PageTree {
    pub pages: Vec<Page>,
    pub leniencies: Vec<Leniency>,
}

/// Walk the catalog's page tree in reading order.
pub fn pages(doc: &Document) -> Result<PageTree> {
    let catalog = doc.catalog()?;
    let catalog_dict =
        catalog.as_dict().ok_or_else(|| CosError::malformed(0, "catalog is not a dictionary"))?;

    let mut out = PageTree { pages: Vec::new(), leniencies: Vec::new() };

    let root_ref = catalog_dict.get("Pages").cloned();
    let Some(root_ref) = root_ref else {
        // No /Pages at all. Some damaged files still have page objects lying
        // around; finding them is better than reporting a zero-page document.
        out.leniencies.push(Leniency::new(
            LeniencyKind::CatalogScanned,
            0,
            "catalog has no /Pages; scanning for /Type /Page objects",
        ));
        scan_for_pages(doc, &mut out);
        return Ok(out);
    };

    let mut path: HashSet<ObjId> = HashSet::new();
    let root_id = root_ref.as_reference();
    let root = doc.resolve(&root_ref)?;
    if let Some(id) = root_id {
        path.insert(id);
    }

    match root.as_dict() {
        Some(d) => {
            let inherited = Inherited::default().refine(doc, d);
            // The root is nobody's kid, so it has no slot.
            visit(doc, d, root_id, &inherited, Cursor::default(), &mut path, &mut out);
        }
        None => {
            out.leniencies.push(Leniency::new(
                LeniencyKind::CatalogScanned,
                0,
                "/Pages is not a dictionary; scanning for /Type /Page objects",
            ));
            scan_for_pages(doc, &mut out);
        }
    }

    if out.pages.is_empty() && root.as_dict().is_some() {
        out.leniencies.push(Leniency::new(
            LeniencyKind::CatalogScanned,
            0,
            "page tree yielded no pages; scanning for /Type /Page objects",
        ));
        scan_for_pages(doc, &mut out);
    }
    Ok(out)
}

/// Where the walk currently is in the page tree.
///
/// Grouped rather than passed loose: the depth and the parent slot always
/// travel together, and separating them is how a recursive call ends up
/// reporting a child at its parent''s position.
#[derive(Debug, Clone, Copy, Default)]
struct Cursor {
    depth: usize,
    /// The `/Pages` object listing this node, and its slot in that `/Kids`.
    /// Threaded down so a leaf knows the one array entry that would have to be
    /// removed to remove the page.
    slot: Option<(ObjId, usize)>,
}

fn visit(
    doc: &Document,
    node: &Dictionary,
    node_id: Option<ObjId>,
    inherited: &Inherited,
    at: Cursor,
    path: &mut HashSet<ObjId>,
    out: &mut PageTree,
) {
    if at.depth > MAX_TREE_DEPTH || out.pages.len() >= MAX_PAGES {
        out.leniencies.push(Leniency::new(
            LeniencyKind::MalformedXrefSection,
            0,
            format!("page tree deeper than {MAX_TREE_DEPTH} levels; stopping"),
        ));
        return;
    }

    let kids = doc.get_entry(node, "Kids").ok().flatten();
    let node_type = node.type_name().map(|t| t.as_bytes().to_vec());

    // Classify by /Kids rather than by /Type: files exist whose page tree nodes
    // are typed /Page but carry /Kids, and whose leaves carry no /Type at all.
    let has_kids = kids.as_ref().and_then(|k| k.as_array()).is_some_and(|a| !a.is_empty());

    if !has_kids {
        if node_type.as_deref() == Some(b"Pages") {
            // An interior node with no children. Legal if /Count is 0.
            return;
        }
        emit_page(doc, node, node_id, inherited, out, at.slot);
        return;
    }

    let Some(kids) = kids.as_ref().and_then(|k| k.as_array()) else { return };
    for (index, kid) in kids.iter().enumerate() {
        let kid_id = kid.as_reference();
        let below = Cursor { depth: at.depth + 1, slot: node_id.map(|p| (p, index)) };
        if let Some(id) = kid_id
            && !path.insert(id)
        {
            out.leniencies.push(Leniency::new(
                LeniencyKind::MalformedXrefSection,
                0,
                format!("page tree cycle at object {id}; pruning"),
            ));
            continue;
        }
        if let Ok(resolved) = doc.resolve(kid)
            && let Some(d) = resolved.as_dict()
        {
            let child_inherited = inherited.refine(doc, d);
            visit(doc, d, kid_id, &child_inherited, below, path, out);
        }
        if let Some(id) = kid_id {
            // Remove on the way out: a node legitimately reachable twice by
            // different paths is not a cycle, only one on the current path is.
            path.remove(&id);
        }
    }
}

fn emit_page(
    doc: &Document,
    dict: &Dictionary,
    id: Option<ObjId>,
    inherited: &Inherited,
    out: &mut PageTree,
    parent: Option<(ObjId, usize)>,
) {
    let _ = doc;
    // US Letter, which is what viewers assume when a file supplies nothing.
    let media_box = inherited.media_box.unwrap_or(Rect::new(0.0, 0.0, 612.0, 792.0));
    let crop_box = inherited.crop_box.unwrap_or(media_box);
    // ISO 32000-1: /Rotate shall be a multiple of 90. Round rather than refuse.
    let raw = inherited.rotate.unwrap_or(0);
    let rotate = ((raw as f64 / 90.0).round() as i32 * 90).rem_euclid(360);

    out.pages.push(Page {
        index: out.pages.len(),
        id: id.unwrap_or(ObjId::new(0, 0)),
        dict: dict.clone(),
        media_box,
        crop_box,
        rotate,
        resources: inherited.resources.clone(),
        parent,
    });
}

/// Last resort for a document whose page tree is unusable: take every
/// `/Type /Page` object in object-number order.
///
/// Object number order is not reading order, but it correlates strongly in
/// practice, and some order beats no pages at all.
fn scan_for_pages(doc: &Document, out: &mut PageTree) {
    for number in doc.xref().live_objects() {
        if out.pages.len() >= MAX_PAGES {
            break;
        }
        let id = ObjId::new(number, 0);
        let Ok(obj) = doc.get(id) else { continue };
        let Some(dict) = obj.as_dict() else { continue };
        if dict.type_name().is_some_and(|t| t.as_bytes() == b"Page") {
            let inherited = Inherited::default().refine(doc, dict);
            // Recovered by scanning: there is no reliable parent to edit.
            emit_page(doc, dict, Some(id), &inherited, out, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Point;
    use rasura_cos::testutil::ClassicBuilder;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn finds_the_single_page_of_a_minimal_file() {
        let doc = Document::open(rasura_cos::testutil::minimal_classic()).unwrap();
        let tree = pages(&doc).unwrap();
        assert_eq!(tree.pages.len(), 1);
        assert!(tree.leniencies.is_empty(), "{:?}", tree.leniencies);
        assert_eq!(tree.pages[0].media_box, Rect::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(tree.pages[0].index, 0);
    }

    #[test]
    fn attributes_are_inherited_from_ancestors() {
        // /MediaBox, /Resources and /Rotate on the root; nothing on the page.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 400] \
                 /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .object(3, "<< /Type /Page /Parent 2 0 R >>")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = pages(&doc).unwrap();
        let p = &tree.pages[0];
        assert_eq!(p.media_box, Rect::new(0.0, 0.0, 200.0, 400.0));
        assert_eq!(p.rotate, 90);
        assert!(p.resources.is_some());
    }

    #[test]
    fn a_page_overrides_what_it_inherits() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 400] /Rotate 90 >>",
            )
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 60] /Rotate 180 >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = &pages(&doc).unwrap().pages[0];
        assert_eq!(p.media_box, Rect::new(0.0, 0.0, 50.0, 60.0));
        assert_eq!(p.rotate, 180);
    }

    #[test]
    fn nested_trees_produce_reading_order() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 4 >>")
            .object(3, "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>")
            .object(4, "<< /Type /Page /Parent 3 0 R /Note (a) >>")
            .object(5, "<< /Type /Page /Parent 3 0 R /Note (b) >>")
            .object(6, "<< /Type /Pages /Parent 2 0 R /Kids [7 0 R 8 0 R] /Count 2 >>")
            .object(7, "<< /Type /Page /Parent 6 0 R /Note (c) >>")
            .object(8, "<< /Type /Page /Parent 6 0 R /Note (d) >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = pages(&doc).unwrap();
        let notes: Vec<String> = tree
            .pages
            .iter()
            .map(|p| {
                String::from_utf8_lossy(p.dict.get("Note").unwrap().as_string().unwrap().as_bytes())
                    .into_owned()
            })
            .collect();
        assert_eq!(notes, vec!["a", "b", "c", "d"]);
        assert!(tree.leniencies.is_empty());
    }

    #[test]
    fn a_cyclic_page_tree_terminates_and_is_reported() {
        // The adversarial fixture: a node whose child is its own ancestor.
        let bytes = rasura_cos::testutil::adversarial()
            .into_iter()
            .find(|(n, _)| *n == "cyclic-page-tree")
            .unwrap()
            .1;
        let doc = Document::open(bytes).unwrap();
        let tree = pages(&doc).unwrap();
        assert!(
            tree.leniencies.iter().any(|l| l.detail.contains("cycle")),
            "{:?}",
            tree.leniencies
        );
    }

    #[test]
    fn a_page_tree_with_no_pages_falls_back_to_scanning() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [] /Count 0 >>")
            .object(3, "<< /Type /Page /MediaBox [0 0 10 10] >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = pages(&doc).unwrap();
        assert_eq!(tree.pages.len(), 1, "the orphaned page should be found");
        assert!(tree.leniencies.iter().any(|l| l.kind == LeniencyKind::CatalogScanned));
    }

    #[test]
    fn crop_box_defaults_to_media_box_and_is_clamped_to_it() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /MediaBox [0 0 100 100] /CropBox [-50 -50 500 500] >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = &pages(&doc).unwrap().pages[0];
        // A crop box larger than the media box is clamped, as viewers do.
        assert_eq!(p.visible_box(), Rect::new(0.0, 0.0, 100.0, 100.0));
    }

    #[test]
    fn rotate_is_normalised() {
        for (raw, want) in [(0, 0), (90, 90), (450, 90), (-90, 270), (360, 0), (45, 90)] {
            let bytes = ClassicBuilder::new()
                .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
                .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
                .object(3, &format!("<< /Type /Page /Rotate {raw} >>"))
                .finish("/Root 1 0 R");
            let doc = Document::open(bytes).unwrap();
            assert_eq!(pages(&doc).unwrap().pages[0].rotate, want, "raw {raw}");
        }
    }

    // --- base CTM ------------------------------------------------------------

    fn page_with(media: &str, rotate: i32) -> Page {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, &format!("<< /Type /Page /MediaBox {media} /Rotate {rotate} >>"))
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        pages(&doc).unwrap().pages.remove(0)
    }

    #[test]
    fn base_ctm_flips_y_so_the_origin_is_top_left() {
        let p = page_with("[0 0 612 792]", 0);
        let m = p.base_ctm();
        // Bottom-left in user space is top-left... no: bottom-left in PDF is
        // the *bottom* of the display, so y maps to the full height.
        let bottom_left = m.apply(Point::new(0.0, 0.0));
        assert!(close(bottom_left.x, 0.0) && close(bottom_left.y, 792.0), "{bottom_left:?}");
        let top_left = m.apply(Point::new(0.0, 792.0));
        assert!(close(top_left.x, 0.0) && close(top_left.y, 0.0), "{top_left:?}");
    }

    #[test]
    fn base_ctm_honours_a_translated_crop_box() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /MediaBox [0 0 612 792] /CropBox [100 100 300 400] >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let p = &pages(&doc).unwrap().pages[0];
        let m = p.base_ctm();
        // The crop box's own bottom-left becomes the display's bottom-left.
        let q = m.apply(Point::new(100.0, 100.0));
        assert!(close(q.x, 0.0) && close(q.y, 300.0), "{q:?}");
    }

    #[test]
    fn rotation_maps_the_page_into_the_rotated_display_box() {
        // A 90-degree rotated page is displayed landscape.
        let p = page_with("[0 0 200 400]", 90);
        assert_eq!(p.display_size(), (400.0, 200.0));
        let m = p.base_ctm();
        // Every corner must land inside the rotated display box.
        for (x, y) in [(0.0, 0.0), (200.0, 0.0), (0.0, 400.0), (200.0, 400.0)] {
            let q = m.apply(Point::new(x, y));
            assert!(
                q.x >= -1e-6 && q.x <= 400.0 + 1e-6 && q.y >= -1e-6 && q.y <= 200.0 + 1e-6,
                "corner ({x},{y}) -> {q:?} is outside the 400x200 display box"
            );
        }
    }

    #[test]
    fn every_rotation_keeps_content_inside_the_display_box() {
        for rotate in [0, 90, 180, 270] {
            let p = page_with("[0 0 200 400]", rotate);
            let (w, h) = p.display_size();
            let m = p.base_ctm();
            for (x, y) in [(0.0, 0.0), (200.0, 0.0), (0.0, 400.0), (200.0, 400.0)] {
                let q = m.apply(Point::new(x, y));
                assert!(
                    q.x >= -1e-6 && q.x <= w + 1e-6 && q.y >= -1e-6 && q.y <= h + 1e-6,
                    "rotate {rotate}: ({x},{y}) -> {q:?} outside {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn base_ctm_is_invertible_for_hit_testing() {
        // paragraphAt(point) in the public API needs to go the other way.
        for rotate in [0, 90, 180, 270] {
            let p = page_with("[0 0 612 792]", rotate);
            let m = p.base_ctm();
            let inv = m.invert().expect("base CTM must be invertible");
            let user = Point::new(72.0, 700.0);
            let back = inv.apply(m.apply(user));
            assert!(close(back.x, user.x) && close(back.y, user.y), "rotate {rotate}: {back:?}");
        }
    }
}
