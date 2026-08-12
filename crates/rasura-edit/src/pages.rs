//! Deleting and reordering pages, with the navigation fix-up. Spec 10.9.
//!
//! > Any operation that changes page count or order must fix up: `/Outlines`
//! > and their `/Dest`, named destinations, `/Link` annotation destinations,
//! > `/OpenAction`, article threads and page labels. A dangling destination is
//! > a silent corruption.
//!
//! # Delete is hard; reorder is not
//!
//! The asymmetry is worth stating because it is not obvious and it decides most
//! of this module. **A destination names a page by object reference, not by
//! index.** Reordering `/Kids` therefore breaks nothing: every outline entry and
//! every link still points at the same page object, which is still in the
//! document, and viewers resolve it to its new position. The only thing a
//! reorder invalidates is `/PageLabels`, whose number tree *is* keyed by index.
//!
//! Deleting is the opposite. Every destination that named the removed page now
//! names an object that is not in the page tree — the file opens, renders and
//! extracts perfectly, and a click goes nowhere.
//!
//! # Retarget rather than remove
//!
//! A destination pointing at a deleted page could be dropped, or pointed
//! somewhere else. This retargets to the page that took its index — the one a
//! reader scrolling to that position now finds — falling back to the last
//! surviving page when the deleted one was last. Dropping would silently lose
//! an outline entry the user can see in the sidebar; retargeting keeps it and
//! is reported through [`Compromise`].
//!
//! What it will not do is leave the document dangling. If any destination
//! cannot be retargeted — a name whose value this walk could not attribute to a
//! rewritable site — the whole operation is **refused**, because a half-fixed
//! document is the silent corruption the spec is warning about.

use crate::session::{Compromise, Fidelity};
use rasura_content::dest::{self, Carrier, NameSite, Navigation};
use rasura_content::page::PageTree;
use rasura_cos::object::{Dictionary, Name, Object};
use rasura_cos::{Document, ObjId};
use std::collections::BTreeMap;

/// How far up a `/Parent` chain to walk before giving up.
const MAX_ANCESTRY: usize = 64;

/// Why a page operation could not be performed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PageError {
    #[error("page {0} does not exist")]
    NoSuchPage(usize),

    /// A document must have at least one page.
    #[error("a document cannot have no pages")]
    LastPage,

    /// The page tree does not say where this page is listed.
    ///
    /// Happens for a document recovered by scanning: the pages were found by
    /// their `/Type`, not by walking `/Kids`, so there is no array entry to
    /// remove. Refused rather than guessed — removing the wrong `/Kids` entry
    /// deletes a different page.
    #[error("page {0} has no recorded parent; the tree was recovered by scanning")]
    NoParent(usize),

    /// The `/Kids` array does not hold what the walk said it did.
    #[error("the parent of page {page} does not list it at slot {slot}")]
    SlotMismatch { page: usize, slot: usize },

    /// A destination could not be retargeted, so the edit would leave the
    /// document dangling.
    #[error("{count} destination(s) point at page {page} and cannot be retargeted")]
    Unfixable { page: usize, count: usize },

    #[error("{0}")]
    Cos(String),
}

/// The object changes an operation needs, ready for
/// [`EditSession::set_objects`](crate::EditSession::set_objects).
#[derive(Debug, Clone)]
pub struct PageEdit {
    pub changes: Vec<(ObjId, Option<Object>)>,
    pub fidelity: Fidelity,
    /// Destinations that were pointed somewhere else.
    pub retargeted: usize,
}

/// The object id of the catalog's root `/Pages` node.
///
/// Only needed for the empty-tree case: with pages present, the parent comes
/// from the anchor page's own recorded `/Parent`, which is the tree the walk
/// actually saw rather than the one the catalog claims.
fn page_tree_root(doc: &Document) -> Result<ObjId, PageError> {
    let catalog = doc.catalog().map_err(|e| PageError::Cos(e.to_string()))?;
    catalog
        .as_dict()
        .and_then(|d| d.get("Pages"))
        .and_then(Object::as_reference)
        .ok_or_else(|| PageError::Cos("the catalog has no /Pages reference".into()))
}

/// Remove a page, fixing up everything that pointed at it. Spec 10.9.
pub fn delete_page(doc: &Document, pages: &PageTree, index: usize) -> Result<PageEdit, PageError> {
    let page = pages.pages.get(index).ok_or(PageError::NoSuchPage(index))?;
    if pages.pages.len() <= 1 {
        return Err(PageError::LastPage);
    }
    let (parent_id, slot) = page.parent.ok_or(PageError::NoParent(index))?;

    let mut changes: BTreeMap<ObjId, Object> = BTreeMap::new();

    // 1. Remove the page from its parent's /Kids.
    let parent = dict_of(doc, parent_id)?;
    let mut kids = parent
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .ok_or(PageError::SlotMismatch { page: index, slot })?;
    if kids.get(slot).and_then(Object::as_reference) != Some(page.id) {
        return Err(PageError::SlotMismatch { page: index, slot });
    }
    kids.remove(slot);

    let mut updated = parent.clone();
    updated.insert(Name::new("Kids"), Object::Array(kids));
    changes.insert(parent_id, Object::Dictionary(updated));

    // 2. Decrement /Count on the parent and every node above it. A viewer that
    //    trusts /Count over the actual kid count will show a blank page
    //    otherwise, which is a rendering defect no structural check catches.
    adjust_counts(doc, parent_id, -1, &mut changes)?;

    // 3. Point everything that named this page somewhere else.
    let nav = dest::collect(doc, pages);
    let replacement = surviving_page(pages, index);
    let retargeted = retarget(doc, &nav, page.id, replacement, &mut changes)?;

    let mut compromises = Vec::new();
    if retargeted > 0 {
        compromises.push(Compromise::DestinationsRetargeted { count: retargeted });
    }
    if nav.has_page_labels {
        compromises.push(Compromise::PageLabelsStale);
    }

    Ok(PageEdit {
        changes: changes.into_iter().map(|(id, o)| (id, Some(o))).collect(),
        fidelity: if compromises.is_empty() {
            Fidelity::Exact
        } else {
            Fidelity::Degraded(compromises)
        },
        retargeted,
    })
}

/// Move a page to another position. Spec 9.2.
///
/// Destinations are untouched by design: they name pages by object reference,
/// so a reorder cannot break one. Only `/PageLabels` is index-keyed, and its
/// staleness is reported.
pub fn move_page(
    doc: &Document,
    pages: &PageTree,
    from: usize,
    to: usize,
) -> Result<PageEdit, PageError> {
    let page = pages.pages.get(from).ok_or(PageError::NoSuchPage(from))?;
    if to >= pages.pages.len() {
        return Err(PageError::NoSuchPage(to));
    }
    let (parent_id, slot) = page.parent.ok_or(PageError::NoParent(from))?;
    let target = pages.pages.get(to).ok_or(PageError::NoSuchPage(to))?;

    // Both ends must hang off the same node. A cross-branch move changes two
    // `/Count` subtrees and is refused rather than half-done: getting it wrong
    // produces a tree whose counts disagree with its kids, which viewers
    // resolve inconsistently.
    let (target_parent, target_slot) = target.parent.ok_or(PageError::NoParent(to))?;
    if target_parent != parent_id {
        return Err(PageError::SlotMismatch { page: to, slot: target_slot });
    }

    let parent = dict_of(doc, parent_id)?;
    let mut kids = parent
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .ok_or(PageError::SlotMismatch { page: from, slot })?;
    if kids.get(slot).and_then(Object::as_reference) != Some(page.id) {
        return Err(PageError::SlotMismatch { page: from, slot });
    }

    let entry = kids.remove(slot);
    kids.insert(target_slot.min(kids.len()), entry);

    let mut updated = parent.clone();
    updated.insert(Name::new("Kids"), Object::Array(kids));

    let nav = dest::collect(doc, pages);
    let mut compromises = Vec::new();
    if nav.has_page_labels {
        compromises.push(Compromise::PageLabelsStale);
    }

    Ok(PageEdit {
        changes: vec![(parent_id, Some(Object::Dictionary(updated)))],
        fidelity: if compromises.is_empty() {
            Fidelity::Exact
        } else {
            Fidelity::Degraded(compromises)
        },
        retargeted: 0,
    })
}

/// What a new page should look like. Spec 9.2's `insert_page(index, PageSpec)`.
#[derive(Debug, Clone)]
pub struct PageSpec {
    /// The page box, in PDF units. Defaults to US Letter.
    pub media_box: [f64; 4],
    /// Content stream bytes, usually from [`Canvas`](crate::Canvas).
    pub content: Vec<u8>,
    /// `/Resources`, which must name anything `content` draws with. Left empty
    /// for a blank page.
    pub resources: Option<Dictionary>,
}

impl Default for PageSpec {
    fn default() -> PageSpec {
        PageSpec { media_box: [0.0, 0.0, 612.0, 792.0], content: Vec::new(), resources: None }
    }
}

/// Add a page at `index`, pushing later pages down. Spec 9.2.
///
/// Nothing needs retargeting: an insertion changes no existing page's object,
/// and destinations name pages by object. It does invalidate `/PageLabels`,
/// which is reported.
///
/// The new page is appended to the same `/Pages` node as the page currently at
/// `index` — or to the last page's node when appending past the end. That keeps
/// the tree's shape and its `/Count` arithmetic local; building a balanced tree
/// is a producer's concern, not an editor's.
pub fn insert_page(
    doc: &mut Document,
    pages: &PageTree,
    index: usize,
    spec: &PageSpec,
) -> Result<PageEdit, PageError> {
    // Where it goes: before the page currently at `index`, or after the last.
    //
    // With no pages at all there is nothing to anchor against, and this used to
    // stop here — which is why a document could be created and never given a
    // first page. The root `/Pages` node is the parent in that case, and it
    // exists in every document this library will hand you: `Document::new`
    // builds one, and `open` refuses a file whose catalog does not resolve.
    let (parent_id, slot) = match pages.pages.get(index).or_else(|| pages.pages.last()) {
        Some(anchor) => {
            let (parent_id, anchor_slot) = anchor.parent.ok_or(PageError::NoParent(index))?;
            (parent_id, if index >= pages.pages.len() { anchor_slot + 1 } else { anchor_slot })
        }
        None => (page_tree_root(doc)?, 0),
    };

    // Two new objects. Their *numbers* are claimed here; the objects themselves
    // are created by the session along with every other change, so that undo
    // knows they did not previously exist and removes them rather than
    // restoring them. Using `add` would write them immediately, outside the
    // transaction, and an undo would leave the content stream behind.
    //
    // The content goes in unfiltered: a new stream has no producer whose
    // compression choice to match, and `/Length` is the writer's to compute.
    let reserved = doc.reserve(2);
    let (content_id, page_id) = (reserved[0], reserved[1]);

    let mut stream = rasura_cos::object::Stream::new(Dictionary::new(), Vec::new());
    stream.set_decoded(spec.content.clone());

    let mut page = Dictionary::new();
    page.insert(Name::new("Type"), Object::name("Page"));
    page.insert(Name::new("Parent"), Object::Reference(parent_id));
    page.insert(
        Name::new("MediaBox"),
        Object::Array(spec.media_box.iter().copied().map(Object::Real).collect()),
    );
    page.insert(Name::new("Contents"), Object::Reference(content_id));
    if let Some(resources) = &spec.resources {
        page.insert(Name::new("Resources"), Object::Dictionary(resources.clone()));
    }

    let mut changes: BTreeMap<ObjId, Object> = BTreeMap::new();
    changes.insert(content_id, Object::Stream(stream));
    changes.insert(page_id, Object::Dictionary(page));

    // Link it into the tree.
    let parent = dict_of(doc, parent_id)?;
    let mut kids = parent
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .ok_or(PageError::SlotMismatch { page: index, slot })?;
    kids.insert(slot.min(kids.len()), Object::Reference(page_id));

    let mut updated = parent.clone();
    updated.insert(Name::new("Kids"), Object::Array(kids));
    changes.insert(parent_id, Object::Dictionary(updated));

    adjust_counts(doc, parent_id, 1, &mut changes)?;

    let nav = dest::collect(doc, pages);
    let mut compromises = Vec::new();
    if nav.has_page_labels {
        compromises.push(Compromise::PageLabelsStale);
    }

    Ok(PageEdit {
        changes: changes.into_iter().map(|(id, o)| (id, Some(o))).collect(),
        fidelity: if compromises.is_empty() {
            Fidelity::Exact
        } else {
            Fidelity::Degraded(compromises)
        },
        retargeted: 0,
    })
}

/// The page a reader lands on after `index` is removed.
fn surviving_page(pages: &PageTree, index: usize) -> ObjId {
    pages
        .pages
        .get(index + 1)
        .or_else(|| pages.pages.get(index.saturating_sub(1)))
        .map(|p| p.id)
        // Unreachable: `delete_page` refuses a one-page document.
        .unwrap_or(ObjId::new(0, 0))
}

fn dict_of(doc: &Document, id: ObjId) -> Result<Dictionary, PageError> {
    doc.get(id)
        .map_err(|e| PageError::Cos(e.to_string()))?
        .as_dict()
        .cloned()
        .ok_or_else(|| PageError::Cos(format!("{id} is not a dictionary")))
}

/// Walk up `/Parent`, adding `delta` to each `/Count`.
///
/// The whole ancestry, not just the immediate parent: a viewer that trusts
/// `/Count` over the actual kid list shows a blank page when they disagree,
/// which is a rendering defect `qpdf --check` does not catch.
fn adjust_counts(
    doc: &Document,
    from: ObjId,
    delta: i64,
    changes: &mut BTreeMap<ObjId, Object>,
) -> Result<(), PageError> {
    let mut at = Some(from);
    let mut seen = 0usize;

    while let Some(id) = at {
        seen += 1;
        if seen > MAX_ANCESTRY {
            break;
        }
        // Read through `changes` so a node edited earlier in this walk -- the
        // immediate parent, whose /Kids was just rewritten -- is not re-read
        // from the document and silently reverted.
        let current = match changes.get(&id) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => dict_of(doc, id)?,
        };

        let mut updated = current.clone();
        if let Some(count) = current.get("Count").and_then(Object::as_i64) {
            updated.insert(Name::new("Count"), Object::Integer((count + delta).max(0)));
            changes.insert(id, Object::Dictionary(updated.clone()));
        }
        at = current.get("Parent").and_then(Object::as_reference);
    }
    Ok(())
}

/// Point every destination naming `removed` at `replacement`.
///
/// Returns how many were changed. Errors if any could not be, rather than
/// leaving some of them dangling.
fn retarget(
    doc: &Document,
    nav: &Navigation,
    removed: ObjId,
    replacement: ObjId,
    changes: &mut BTreeMap<ObjId, Object>,
) -> Result<usize, PageError> {
    let mut fixed = 0usize;
    let mut unfixable = 0usize;

    for d in &nav.destinations {
        if d.target != Some(removed) {
            continue;
        }
        match &d.carrier {
            Carrier::Outline(id) | Carrier::Annotation { annot: id, .. } => {
                match rewrite_carrier(doc, *id, replacement, changes) {
                    true => fixed += 1,
                    false => unfixable += 1,
                }
            }
            Carrier::OpenAction => match rewrite_open_action(doc, replacement, changes) {
                true => fixed += 1,
                false => unfixable += 1,
            },
            Carrier::Named(name) => {
                let site = nav.names.get(name).and_then(|n| n.site.clone());
                match site.map(|s| rewrite_name(doc, &s, replacement, changes)) {
                    Some(true) => fixed += 1,
                    _ => unfixable += 1,
                }
            }
        }
    }

    if unfixable > 0 {
        return Err(PageError::Unfixable { page: 0, count: unfixable });
    }
    Ok(fixed)
}

/// Rewrite an outline item's or annotation's destination in place.
fn rewrite_carrier(
    doc: &Document,
    id: ObjId,
    replacement: ObjId,
    changes: &mut BTreeMap<ObjId, Object>,
) -> bool {
    let Ok(dict) = dict_of(doc, id) else { return false };
    let mut updated = dict.clone();

    // `/A` first: it is the form that wins where both appear, and the commoner
    // one by 3.6 to 1.
    if let Some(action) = dict.get("A")
        && let Ok(resolved) = doc.resolve(action)
        && let Some(action_dict) = resolved.as_dict()
        && let Some(d) = action_dict.get("D")
        && let Some(fixed) = repoint(doc, d, replacement)
    {
        // An indirect action is a separate object; rewrite it there.
        if let Some(action_id) = action.as_reference() {
            let mut a = action_dict.clone();
            a.insert(Name::new("D"), fixed);
            changes.insert(action_id, Object::Dictionary(a));
            return true;
        }
        let mut a = action_dict.clone();
        a.insert(Name::new("D"), fixed);
        updated.insert(Name::new("A"), Object::Dictionary(a));
        changes.insert(id, Object::Dictionary(updated));
        return true;
    }

    if let Some(dest) = dict.get("Dest")
        && let Some(fixed) = repoint(doc, dest, replacement)
    {
        updated.insert(Name::new("Dest"), fixed);
        changes.insert(id, Object::Dictionary(updated));
        return true;
    }
    false
}

fn rewrite_open_action(
    doc: &Document,
    replacement: ObjId,
    changes: &mut BTreeMap<ObjId, Object>,
) -> bool {
    let Some(catalog_id) = doc.catalog_id() else { return false };
    let Ok(catalog) = dict_of(doc, catalog_id) else { return false };
    let Some(open) = catalog.get("OpenAction") else { return false };

    // Either a destination array directly, or a /GoTo action carrying /D.
    if let Some(fixed) = repoint(doc, open, replacement) {
        let mut updated = catalog.clone();
        updated.insert(Name::new("OpenAction"), fixed);
        changes.insert(catalog_id, Object::Dictionary(updated));
        return true;
    }
    if let Ok(resolved) = doc.resolve(open)
        && let Some(action) = resolved.as_dict()
        && let Some(d) = action.get("D")
        && let Some(fixed) = repoint(doc, d, replacement)
    {
        let mut a = action.clone();
        a.insert(Name::new("D"), fixed);
        if let Some(action_id) = open.as_reference() {
            changes.insert(action_id, Object::Dictionary(a));
        } else {
            let mut updated = catalog.clone();
            updated.insert(Name::new("OpenAction"), Object::Dictionary(a));
            changes.insert(catalog_id, Object::Dictionary(updated));
        }
        return true;
    }
    false
}

/// Rewrite a named destination's value where it is stored.
fn rewrite_name(
    doc: &Document,
    site: &NameSite,
    replacement: ObjId,
    changes: &mut BTreeMap<ObjId, Object>,
) -> bool {
    match site {
        NameSite::TreeLeaf { node, value_at } => {
            let Ok(leaf) = dict_of(doc, *node) else { return false };
            let Some(names) = leaf.get("Names").and_then(Object::as_array) else { return false };
            let mut names = names.to_vec();
            let Some(slot) = names.get_mut(*value_at) else { return false };
            let Some(fixed) = repoint(doc, slot, replacement) else { return false };
            *slot = fixed;

            let mut updated = leaf.clone();
            updated.insert(Name::new("Names"), Object::Array(names));
            changes.insert(*node, Object::Dictionary(updated));
            true
        }
        NameSite::RootDests { container, key } => {
            let Ok(dict) = dict_of(doc, *container) else { return false };
            // The container is either the /Dests dictionary itself or the
            // catalog holding it inline.
            let inline = dict.get("Dests").is_some();
            let target = if inline {
                let Some(d) = dict.get("Dests").and_then(Object::as_dict) else { return false };
                d.clone()
            } else {
                dict.clone()
            };
            let Some(value) = target.get(key.as_str()) else { return false };
            let Some(fixed) = repoint(doc, value, replacement) else { return false };

            let mut inner = target.clone();
            inner.insert(Name::new(key.as_bytes()), fixed);
            let mut updated = dict.clone();
            if inline {
                updated.insert(Name::new("Dests"), Object::Dictionary(inner));
            } else {
                updated = inner;
            }
            changes.insert(*container, Object::Dictionary(updated));
            true
        }
    }
}

/// Replace the page reference at the head of a destination array.
///
/// Everything after the first element -- `/XYZ left top zoom`, `/Fit`, and the
/// rest of §12.3.2.2's forms -- is carried through untouched. Those describe
/// *where on the page* to land, and are as valid on the replacement page as on
/// the removed one.
fn repoint(doc: &Document, value: &Object, replacement: ObjId) -> Option<Object> {
    let resolved = doc.resolve(value).ok();
    let array = resolved.as_deref().unwrap_or(value).as_array()?;
    let mut out = array.to_vec();
    *out.first_mut()? = Object::Reference(replacement);
    Some(Object::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;
    use rasura_cos::testutil::ClassicBuilder;

    #[test]
    fn the_first_page_of_an_empty_document_can_be_created() {
        // The whole point of `Document::new`: before this, a created document
        // had a page tree that nothing could ever put a page into, because
        // `insert_page` needed an existing page to anchor against.
        let mut doc = Document::new();
        let tree = rasura_content::page::pages(&doc).unwrap();
        assert_eq!(tree.pages.len(), 0);

        let mut canvas = crate::Canvas::new(crate::numfmt::NumberStyle::default());
        canvas.fill_rgb(0.1, 0.2, 0.9);
        canvas.rect(72.0, 72.0, 200.0, 100.0);
        canvas.fill();
        let spec = PageSpec { content: canvas.finish().unwrap(), ..PageSpec::default() };

        let edit = insert_page(&mut doc, &tree, 0, &spec).expect("the first page");
        {
            let mut session = EditSession::new(&mut doc);
            session.set_objects("insert page", &edit.changes, edit.fidelity.clone()).unwrap();
        }

        // Through the reader, not the writer's own bookkeeping.
        let tree = rasura_content::page::pages(&doc).unwrap();
        assert_eq!(tree.pages.len(), 1);
        assert_eq!(tree.pages[0].media_box.x1, 612.0);
        assert_eq!(tree.pages[0].media_box.y1, 792.0);

        // And it survives a round trip as a file, which is the claim that
        // matters: parsed by the same reader used on everyone else's PDFs,
        // with nothing forgiven along the way.
        let saved = rasura_cos::writer::save(&doc, &SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).expect("a created document reopens");
        assert_eq!(reopened.leniencies(), Vec::new());
        let tree = rasura_content::page::pages(&reopened).unwrap();
        assert_eq!(tree.pages.len(), 1);
    }

    /// Three pages, an outline pointing at page two, a link on page one
    /// pointing at page two through an action, a named destination for page
    /// three, and an /OpenAction on page two.
    fn navigated() -> Vec<u8> {
        ClassicBuilder::new()
            .object(
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /OpenAction [6 0 R /Fit] \
                 /Names << /Dests 12 0 R >> >>",
            )
            .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R 10 0 R] /Count 3 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [9 0 R] >>")
            .stream(4, "", b"BT ET\n")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>")
            .stream(7, "", b"BT ET\n")
            .object(8, "<< /Type /Outlines /First 11 0 R /Count 1 >>")
            .object(
                9,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 9 9] \
                 /A << /S /GoTo /D [6 0 R /Fit] >> >>",
            )
            .object(10, "<< /Type /Page /Parent 2 0 R /Contents 13 0 R >>")
            .object(11, "<< /Title (Two) /Parent 8 0 R /Dest [6 0 R /Fit] >>")
            .object(12, "<< /Names [(three) [10 0 R /Fit]] >>")
            .stream(13, "", b"BT ET\n")
            .finish("/Root 1 0 R")
    }

    fn open(bytes: Vec<u8>) -> (Document, PageTree) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        (doc, pages)
    }

    /// Apply an edit and reopen, so every assertion is against a real file.
    fn commit(mut doc: Document, edit: PageEdit) -> Document {
        let mut session = EditSession::new(&mut doc);
        session.set_objects("page edit", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;
        Document::open(saved).expect("reopen")
    }

    #[test]
    fn deleting_a_page_removes_it_from_the_tree() {
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 1).expect("delete");
        let after = commit(doc, edit);

        let tree = rasura_content::page::pages(&after).expect("pages");
        assert_eq!(tree.pages.len(), 2);
        assert!(tree.pages.iter().all(|p| p.id != ObjId::new(6, 0)), "page two is gone");
    }

    #[test]
    fn deleting_a_page_fixes_the_count_chain() {
        // A viewer that trusts /Count over the kid list shows a blank page
        // otherwise -- a rendering defect no structural check catches.
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 1).expect("delete");
        let after = commit(doc, edit);

        let root = after.get(ObjId::new(2, 0)).expect("root").as_dict().expect("dict").clone();
        assert_eq!(root.get("Count").and_then(Object::as_i64), Some(2));
    }

    #[test]
    fn every_destination_that_pointed_at_the_deleted_page_is_retargeted() {
        // The outline item, the link's /A /D, and the /OpenAction all named
        // page two. None may be left dangling.
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 1).expect("delete");
        assert_eq!(edit.retargeted, 3, "outline, link and open action");

        let after = commit(doc, edit);
        let tree = rasura_content::page::pages(&after).expect("pages");
        let nav = dest::collect(&after, &tree);
        assert_eq!(nav.dangling().count(), 0, "{:?}", nav.destinations);
    }

    #[test]
    fn the_retargeted_destination_keeps_its_view() {
        // `/Fit` describes where on the page to land and is as valid on the
        // replacement as on the removed page. Dropping it would silently change
        // the zoom a link arrives at.
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 1).expect("delete");
        let after = commit(doc, edit);

        let item = after.get(ObjId::new(11, 0)).expect("outline").as_dict().expect("dict").clone();
        let dest = item.get("Dest").and_then(Object::as_array).expect("dest");
        assert_eq!(dest.len(), 2, "{dest:?}");
        assert_eq!(dest[0].as_reference(), Some(ObjId::new(10, 0)), "now page three");
        assert_eq!(dest[1].as_name().and_then(|n| n.as_str()), Some("Fit"));
    }

    #[test]
    fn a_named_destination_is_retargeted_in_its_name_tree_leaf() {
        // Deleting the *last* page: the name pointing at it must move back
        // rather than dangle.
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 2).expect("delete");
        let after = commit(doc, edit);

        let leaf = after.get(ObjId::new(12, 0)).expect("leaf").as_dict().expect("dict").clone();
        let names = leaf.get("Names").and_then(Object::as_array).expect("names");
        let value = names[1].as_array().expect("destination");
        assert_eq!(value[0].as_reference(), Some(ObjId::new(6, 0)), "moved back to page two");

        let tree = rasura_content::page::pages(&after).expect("pages");
        assert_eq!(dest::collect(&after, &tree).dangling().count(), 0);
    }

    #[test]
    fn deleting_reports_what_it_retargeted() {
        let (doc, pages) = open(navigated());
        let edit = delete_page(&doc, &pages, 1).expect("delete");
        match &edit.fidelity {
            Fidelity::Degraded(list) => {
                assert!(list.contains(&Compromise::DestinationsRetargeted { count: 3 }), "{list:?}")
            }
            other => panic!("a retarget is a compromise worth reporting, got {other:?}"),
        }
    }

    #[test]
    fn the_last_page_cannot_be_deleted() {
        let (doc, pages) = open(rasura_cos::testutil::classic_with_flate_content());
        assert!(matches!(delete_page(&doc, &pages, 0), Err(PageError::LastPage)));
    }

    #[test]
    fn a_page_that_does_not_exist_is_refused() {
        let (doc, pages) = open(navigated());
        assert!(matches!(delete_page(&doc, &pages, 9), Err(PageError::NoSuchPage(9))));
    }

    #[test]
    fn reordering_pages_changes_the_order() {
        let (doc, pages) = open(navigated());
        let before: Vec<ObjId> = pages.pages.iter().map(|p| p.id).collect();

        let edit = move_page(&doc, &pages, 0, 2).expect("move");
        let after = commit(doc, edit);
        let tree = rasura_content::page::pages(&after).expect("pages");
        let now: Vec<ObjId> = tree.pages.iter().map(|p| p.id).collect();

        assert_eq!(now.len(), before.len(), "no page was lost");
        assert_eq!(now[2], before[0], "the first page moved to the end");
        assert_eq!(now[0], before[1]);
    }

    #[test]
    fn reordering_breaks_no_destination() {
        // The asymmetry this module is built around: destinations name pages by
        // *object*, so a reorder cannot dangle one. If this ever fails, the
        // retarget logic is needed on move too.
        let (doc, pages) = open(navigated());
        let edit = move_page(&doc, &pages, 0, 2).expect("move");
        assert_eq!(edit.retargeted, 0);

        let after = commit(doc, edit);
        let tree = rasura_content::page::pages(&after).expect("pages");
        let nav = dest::collect(&after, &tree);
        assert_eq!(nav.dangling().count(), 0, "{:?}", nav.destinations);
    }

    #[test]
    fn a_page_operation_undoes_exactly() {
        // I5 through the object-level primitive.
        let original = navigated();
        let mut doc = Document::open(original.clone()).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let edit = delete_page(&doc, &pages, 1).expect("delete");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("delete", &edit.changes, edit.fidelity).expect("set");
        assert!(session.undo().expect("undo"));
        assert_eq!(session.commit(&SaveOptions::default()).expect("commit").bytes, original);
    }

    #[test]
    fn an_inserted_page_appears_where_asked_and_carries_its_content() {
        let (mut doc, pages) = open(navigated());
        let content = {
            let mut c = crate::Canvas::new(crate::NumberStyle::default());
            c.fill_rgb(0.0, 0.0, 0.0).rect(72.0, 700.0, 100.0, 20.0).fill();
            c.finish().expect("balanced")
        };
        let spec = PageSpec { content: content.clone(), ..PageSpec::default() };
        let edit = insert_page(&mut doc, &pages, 1, &spec).expect("insert");
        let after = commit(doc, edit);

        let tree = rasura_content::page::pages(&after).expect("pages");
        assert_eq!(tree.pages.len(), 4);

        // The new page is second, and it draws what it was given.
        let inserted = &tree.pages[1];
        let (logical, _) =
            rasura_content::content::page_content(&after, &inserted.dict).expect("content");
        assert_eq!(logical.data(), &content[..]);
    }

    #[test]
    fn inserting_fixes_the_count_chain() {
        let (mut doc, pages) = open(navigated());
        let edit = insert_page(&mut doc, &pages, 0, &PageSpec::default()).expect("insert");
        let after = commit(doc, edit);

        let root = after.get(ObjId::new(2, 0)).expect("root").as_dict().expect("dict").clone();
        assert_eq!(root.get("Count").and_then(Object::as_i64), Some(4));
    }

    #[test]
    fn inserting_past_the_end_appends() {
        let (mut doc, pages) = open(navigated());
        let before: Vec<ObjId> = pages.pages.iter().map(|p| p.id).collect();

        let edit = insert_page(&mut doc, &pages, 99, &PageSpec::default()).expect("insert");
        let after = commit(doc, edit);
        let tree = rasura_content::page::pages(&after).expect("pages");

        assert_eq!(tree.pages.len(), 4);
        assert_eq!(tree.pages[..3].iter().map(|p| p.id).collect::<Vec<_>>(), before);
    }

    #[test]
    fn inserting_breaks_no_destination() {
        // It changes no existing page's object, so nothing that pointed at one
        // can dangle. Asserted rather than assumed.
        let (mut doc, pages) = open(navigated());
        let edit = insert_page(&mut doc, &pages, 1, &PageSpec::default()).expect("insert");
        assert_eq!(edit.retargeted, 0);

        let after = commit(doc, edit);
        let tree = rasura_content::page::pages(&after).expect("pages");
        assert_eq!(dest::collect(&after, &tree).dangling().count(), 0);
    }

    #[test]
    fn an_inserted_page_undoes_exactly() {
        // The harder direction for undo: the operation *created* objects, so
        // undoing has to delete them rather than restore a prior value.
        let original = navigated();
        let mut doc = Document::open(original.clone()).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let edit = insert_page(&mut doc, &pages, 1, &PageSpec::default()).expect("insert");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("insert", &edit.changes, edit.fidelity).expect("set");
        assert!(session.undo().expect("undo"));
        assert_eq!(session.commit(&SaveOptions::default()).expect("commit").bytes, original);
    }

    #[test]
    fn a_document_with_page_labels_says_they_are_stale() {
        // Reordering does not break destinations and *does* break page labels,
        // whose number tree is keyed by index. Renumbering it is not attempted;
        // saying so is.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /P (i) >>] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
            .stream(4, "", b"BT ET\n")
            .object(5, "<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>")
            .stream(6, "", b"BT ET\n")
            .finish("/Root 1 0 R");

        let (doc, pages) = open(bytes);
        let edit = move_page(&doc, &pages, 0, 1).expect("move");
        match &edit.fidelity {
            Fidelity::Degraded(list) => assert!(list.contains(&Compromise::PageLabelsStale)),
            other => panic!("expected a stale-labels report, got {other:?}"),
        }
    }
}
