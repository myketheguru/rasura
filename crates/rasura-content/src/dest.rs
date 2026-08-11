//! Where a document points. Spec 10.9.
//!
//! > Any operation that changes page count or order must fix up: `/Outlines`
//! > and their `/Dest`, named destinations (`/Dests`, `/Names`), `/Link`
//! > annotation destinations, `/OpenAction`, article threads (`/Threads`), and
//! > page labels (`/PageLabels`). **A dangling destination is a silent
//! > corruption; add an invariant check for it.**
//!
//! This module finds them. Rewriting them is the edit layer's job, and the
//! invariant suite uses the same walk to check that none dangles — which is why
//! it lives here rather than there: a check that used different code from the
//! fix-up would agree with it by coincidence.
//!
//! # What the corpus says to build
//!
//! Measured across the 960 pdf.js corpus documents that open:
//!
//! | | Documents | Destinations |
//! |---|---|---|
//! | `/Outlines` | 176 (18.3%) | 343 items, 306 with a destination |
//! | `/Link` annotations | 58 (6.0%) | 566 links, 180 with a destination |
//! | `/OpenAction` | 56 (5.8%) | 51 explicit |
//! | `/Names` → `/Dests` | 17 (1.8%) | 60 named |
//! | root `/Dests` | 5 (0.5%) | 34 named |
//! | `/PageLabels` | 22 (2.3%) | 33 ranges |
//! | `/Threads` | 1 (0.1%) | **0 beads** |
//!
//! Two findings shaped this code.
//!
//! **The `/A` action form dominates.** A destination can be written as `/Dest`
//! directly, or as an `/A` action of subtype `/GoTo` carrying `/D`. The spec's
//! sentence names `/Dest`; the corpus has `/A` `/D` outnumbering it **3.6 : 1**
//! on links and **4.5 : 1** on outline items. Handling only `/Dest` would find
//! a quarter of what is out there and report the rest as clean.
//!
//! **Named destinations cannot be skipped.** They are only 23% of all
//! destinations, but 107 of the 180 link destinations — the majority of the
//! most common carrier — so the name tree has to be walked even though only 17
//! documents have one.
//!
//! `/Threads` is deliberately not implemented. One corpus document has the key
//! and its value is an empty array; there is not one real article thread in 960
//! files. A traversal nothing can test is a traversal that will be wrong when
//! it finally matters, and saying so is better than shipping untested code that
//! looks like coverage.

use crate::page::PageTree;
use rasura_cos::object::{Dictionary, Object};
use rasura_cos::{Document, ObjId};
use std::collections::{BTreeMap, HashSet};

/// How deep a name tree or outline tree may nest before the walk gives up.
const MAX_DEPTH: usize = 64;

/// A cap on how many destinations are collected, so an adversarial file cannot
/// make this allocate without bound. Well above anything real: the largest
/// corpus document has 343.
const MAX_DESTINATIONS: usize = 200_000;

/// Where a destination was found, so a fix-up knows what to rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carrier {
    /// An outline item's own dictionary.
    Outline(ObjId),
    /// A `/Link` (or other) annotation on a page.
    Annotation { page: usize, annot: ObjId },
    /// The catalog's `/OpenAction`.
    OpenAction,
    /// A named destination, by its name.
    Named(String),
}

/// How the destination is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `/Dest` on the carrier.
    Direct,
    /// `/A << /S /GoTo /D ... >>`. The commoner of the two by 3.6 : 1.
    Action,
    /// The value of a name-tree or `/Dests` entry.
    NameTarget,
}

/// One destination found in the document.
#[derive(Debug, Clone)]
pub struct Destination {
    pub carrier: Carrier,
    pub form: Form,
    /// The page it names, when it is an explicit array whose first element is a
    /// page reference that the page tree knows.
    ///
    /// `None` for a destination this walk could not resolve: a name with no
    /// entry, a page reference to an object that is not a page, or a form this
    /// module does not model. Whether that is a defect is the caller's
    /// judgement — a `/GoToR` pointing into another file is not dangling.
    pub page: Option<usize>,
    /// The page object it referenced, whether or not it resolved to an index.
    pub target: Option<ObjId>,
    /// The name it went through, for a destination written as a name.
    pub name: Option<String>,
    /// Whether the destination is a name that no name tree resolves.
    ///
    /// This is the dangling case §10.9 warns about, and it is kept separate
    /// from `page: None` because the two mean different things: an unresolved
    /// *name* is a broken link, while an unresolved *action* may simply be a
    /// URI.
    pub unresolved_name: bool,
}

/// Where a named destination's value is stored, so it can be rewritten.
///
/// Finding a name is not enough to fix it up: the value lives in one of two
/// quite different containers, and a caller that knows only the name would have
/// to walk the tree again to find out which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSite {
    /// A name-tree leaf's `/Names` array, at this position in it.
    ///
    /// The array alternates name, value, name, value; the index is the
    /// *value's* position.
    TreeLeaf { node: ObjId, value_at: usize },
    /// The pre-1.2 root `/Dests` dictionary, under this key.
    ///
    /// `container` is the dictionary's own object when it is indirect, and the
    /// catalog when the dictionary was written inline.
    RootDests { container: ObjId, key: String },
}

/// A named destination and where its value is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// The page object the name resolves to.
    pub target: Option<ObjId>,
    /// Where the value lives. `None` when the walk could not attribute it,
    /// which makes the name readable but not rewritable.
    pub site: Option<NameSite>,
}

/// Everything a page-count or page-order change has to fix up.
#[derive(Debug, Clone, Default)]
pub struct Navigation {
    pub destinations: Vec<Destination>,
    /// Named destinations, resolved to the page object each names.
    pub names: BTreeMap<String, Named>,
    /// Whether the document has `/PageLabels`, which a reorder must renumber.
    pub has_page_labels: bool,
    /// Whether the document has a non-empty `/Threads`.
    ///
    /// Reported rather than handled: see the module note.
    pub has_threads: bool,
    /// The walk stopped early — depth, cycle or cap.
    pub truncated: bool,
}

impl Navigation {
    /// Destinations that point at a page the document does not have.
    pub fn dangling(&self) -> impl Iterator<Item = &Destination> {
        self.destinations
            .iter()
            .filter(|d| d.unresolved_name || (d.target.is_some() && d.page.is_none()))
    }
}

/// Find every destination in a document.
pub fn collect(doc: &Document, pages: &PageTree) -> Navigation {
    let mut nav = Navigation::default();

    // Page object to index, so an explicit destination's first element can be
    // turned into something a reorder can remap.
    let index_of: BTreeMap<ObjId, usize> = pages.pages.iter().map(|p| (p.id, p.index)).collect();

    let Ok(catalog) = doc.catalog() else { return nav };
    let Some(catalog) = catalog.as_dict() else { return nav };

    // Names first: everything else may refer to them.
    collect_names(doc, catalog, &mut nav);
    for (name, entry) in nav.names.clone() {
        let page = entry.target.and_then(|id| index_of.get(&id).copied());
        nav.destinations.push(Destination {
            carrier: Carrier::Named(name.clone()),
            form: Form::NameTarget,
            page,
            target: entry.target,
            name: Some(name),
            unresolved_name: false,
        });
    }

    if let Some(open) = doc.get_entry(catalog, "OpenAction").ok().flatten() {
        if let Some(d) = read_any(doc, &open, &nav.names, &index_of, Carrier::OpenAction) {
            nav.destinations.push(d);
        }
    }

    collect_outlines(doc, catalog, &nav.names.clone(), &index_of, &mut nav);
    collect_annotations(doc, pages, &nav.names.clone(), &index_of, &mut nav);

    nav.has_page_labels = catalog.get("PageLabels").is_some();
    nav.has_threads = doc
        .get_entry(catalog, "Threads")
        .ok()
        .flatten()
        .and_then(|t| t.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false);

    nav
}

/// Named destinations, from both the `/Names` name tree and the older root
/// `/Dests` dictionary.
///
/// Both forms are live in the corpus — 17 documents use the tree, 5 the
/// dictionary — and a file may carry either.
fn collect_names(doc: &Document, catalog: &Dictionary, nav: &mut Navigation) {
    if let Some(names) = doc.get_entry(catalog, "Names").ok().flatten()
        && let Some(names) = names.as_dict()
        && let Some(dests) = names.get("Dests")
    {
        let node_id = dests.as_reference();
        if let Ok(resolved) = doc.resolve(dests)
            && let Some(tree) = resolved.as_dict()
        {
            let mut seen = HashSet::new();
            walk_name_tree(doc, tree, node_id, 0, &mut seen, nav);
        }
    }

    // The pre-1.2 form: a flat dictionary in the catalog.
    if let Some(dests) = catalog.get("Dests") {
        // Rewriting an inline dictionary means rewriting the catalog that
        // holds it, so the container is whichever object actually carries the
        // bytes.
        let container = dests.as_reference().or_else(|| doc.catalog_id());
        if let Ok(resolved) = doc.resolve(dests)
            && let Some(dict) = resolved.as_dict()
        {
            for (key, value) in dict.iter() {
                let name = String::from_utf8_lossy(key.as_bytes()).into_owned();
                let site =
                    container.map(|c| NameSite::RootDests { container: c, key: name.clone() });
                nav.names
                    .entry(name)
                    .or_insert_with(|| Named { target: target_of(doc, value), site });
            }
        }
    }
}

/// A name tree: interior nodes carry `/Kids`, leaves carry `/Names`.
fn walk_name_tree(
    doc: &Document,
    node: &Dictionary,
    node_id: Option<ObjId>,
    depth: usize,
    seen: &mut HashSet<ObjId>,
    nav: &mut Navigation,
) {
    if depth > MAX_DEPTH || nav.names.len() > MAX_DESTINATIONS {
        nav.truncated = true;
        return;
    }

    // A leaf's /Names is a flat array of name, value, name, value.
    if let Some(entries) = doc.get_entry(node, "Names").ok().flatten()
        && let Some(array) = entries.as_array()
    {
        for (pair, chunk) in array.chunks(2).enumerate() {
            let [key, value] = chunk else { continue };
            let Some(key) = key.as_string() else { continue };
            let name = String::from_utf8_lossy(key.as_bytes()).into_owned();
            // The value's own index, which is what a rewrite addresses.
            let site = node_id.map(|node| NameSite::TreeLeaf { node, value_at: pair * 2 + 1 });
            nav.names.insert(name, Named { target: target_of(doc, value), site });
        }
    }

    if let Some(kids) = doc.get_entry(node, "Kids").ok().flatten()
        && let Some(array) = kids.as_array()
    {
        for kid in array {
            let kid_id = kid.as_reference();
            if let Some(id) = kid_id
                && !seen.insert(id)
            {
                nav.truncated = true;
                continue;
            }
            if let Ok(resolved) = doc.resolve(kid)
                && let Some(dict) = resolved.as_dict()
            {
                walk_name_tree(doc, dict, kid_id, depth + 1, seen, nav);
            }
        }
    }
}

/// The page object an explicit destination names.
///
/// A destination is an array whose first element is the page: `[page /XYZ left
/// top zoom]`. A name-tree value may also be a dictionary wrapping it in `/D`,
/// which is the form a `/GoTo` action's target takes when it was written out
/// as a named destination.
fn target_of(doc: &Document, value: &Object) -> Option<ObjId> {
    let resolved = doc.resolve(value).ok()?;
    if let Some(array) = resolved.as_array() {
        return array.first()?.as_reference();
    }
    if let Some(dict) = resolved.as_dict() {
        let inner = doc.get_entry(dict, "D").ok().flatten()?;
        return inner.as_array()?.first()?.as_reference();
    }
    None
}

/// Read a destination from a carrier's `/Dest`, or from its `/A` action's `/D`.
fn read_any(
    doc: &Document,
    value: &Object,
    names: &BTreeMap<String, Named>,
    index_of: &BTreeMap<ObjId, usize>,
    carrier: Carrier,
) -> Option<Destination> {
    // An action dictionary; the destination is under /D.
    if let Ok(resolved) = doc.resolve(value)
        && let Some(dict) = resolved.as_dict()
        && let Some(d) = doc.get_entry(dict, "D").ok().flatten()
    {
        return Some(interpret(doc, &d, names, index_of, carrier, Form::Action));
    }
    Some(interpret(doc, value, names, index_of, carrier, Form::Direct))
}

/// Turn a destination value -- explicit array, name, or string -- into a
/// resolved page.
fn interpret(
    doc: &Document,
    value: &Object,
    names: &BTreeMap<String, Named>,
    index_of: &BTreeMap<ObjId, usize>,
    carrier: Carrier,
    form: Form,
) -> Destination {
    let mut dest =
        Destination { carrier, form, page: None, target: None, name: None, unresolved_name: false };

    let resolved = doc.resolve(value).ok();
    let value = resolved.as_deref().unwrap_or(value);

    match value {
        Object::Array(items) => {
            dest.target = items.first().and_then(Object::as_reference);
            dest.page = dest.target.and_then(|id| index_of.get(&id).copied());
        }
        Object::Name(n) => {
            let name = String::from_utf8_lossy(n.as_bytes()).into_owned();
            resolve_named(&name, names, index_of, &mut dest);
        }
        Object::String(s) => {
            let name = String::from_utf8_lossy(s.as_bytes()).into_owned();
            resolve_named(&name, names, index_of, &mut dest);
        }
        // A /URI or /GoToR action, or something this module does not model.
        // Not a destination into this document, and not a defect.
        _ => {}
    }
    dest
}

fn resolve_named(
    name: &str,
    names: &BTreeMap<String, Named>,
    index_of: &BTreeMap<ObjId, usize>,
    dest: &mut Destination,
) {
    dest.name = Some(name.to_string());
    match names.get(name) {
        Some(entry) => {
            dest.target = entry.target;
            dest.page = entry.target.and_then(|id| index_of.get(&id).copied());
        }
        // A name no name tree defines. This is the dangling case, and it is
        // distinguished from an unresolvable action because the two need
        // different answers from a caller.
        None => dest.unresolved_name = true,
    }
}

/// The outline tree. 176 corpus documents have one; 343 items between them.
fn collect_outlines(
    doc: &Document,
    catalog: &Dictionary,
    names: &BTreeMap<String, Named>,
    index_of: &BTreeMap<ObjId, usize>,
    nav: &mut Navigation,
) {
    let Some(outlines) = doc.get_entry(catalog, "Outlines").ok().flatten() else { return };
    let Some(root) = outlines.as_dict() else { return };
    let Some(first) = root.get("First") else { return };

    let mut seen = HashSet::new();
    let mut stack = vec![(first.clone(), 0usize)];

    while let Some((item, depth)) = stack.pop() {
        if depth > MAX_DEPTH || nav.destinations.len() > MAX_DESTINATIONS {
            nav.truncated = true;
            return;
        }
        let Some(id) = item.as_reference() else { continue };
        if !seen.insert(id) {
            // Outline chains are doubly linked and can be circular in a broken
            // file; each item is visited once.
            continue;
        }
        let Ok(resolved) = doc.resolve(&item) else { continue };
        let Some(dict) = resolved.as_dict() else { continue };

        // /Dest and /A are alternatives; /A wins where both appear, matching
        // what viewers do, and the corpus has none carrying both.
        let found = dict
            .get("A")
            .and_then(|a| read_any(doc, a, names, index_of, Carrier::Outline(id)))
            .filter(|d| d.target.is_some() || d.unresolved_name || d.name.is_some())
            .or_else(|| {
                dict.get("Dest")
                    .map(|v| interpret(doc, v, names, index_of, Carrier::Outline(id), Form::Direct))
            });
        if let Some(d) = found {
            nav.destinations.push(d);
        }

        if let Some(next) = dict.get("Next") {
            stack.push((next.clone(), depth));
        }
        if let Some(child) = dict.get("First") {
            stack.push((child.clone(), depth + 1));
        }
    }
}

/// `/Link` annotations. 58 corpus documents, 566 links, 180 with a destination.
fn collect_annotations(
    doc: &Document,
    pages: &PageTree,
    names: &BTreeMap<String, Named>,
    index_of: &BTreeMap<ObjId, usize>,
    nav: &mut Navigation,
) {
    for page in &pages.pages {
        let Some(annots) = doc.get_entry(&page.dict, "Annots").ok().flatten() else { continue };
        let Some(array) = annots.as_array() else { continue };

        for entry in array {
            if nav.destinations.len() > MAX_DESTINATIONS {
                nav.truncated = true;
                return;
            }
            let annot_id = entry.as_reference().unwrap_or(ObjId::new(0, 0));
            let Ok(resolved) = doc.resolve(entry) else { continue };
            let Some(dict) = resolved.as_dict() else { continue };

            let carrier = Carrier::Annotation { page: page.index, annot: annot_id };
            let found = dict
                .get("A")
                .and_then(|a| read_any(doc, a, names, index_of, carrier.clone()))
                .filter(|d| d.target.is_some() || d.unresolved_name)
                .or_else(|| {
                    dict.get("Dest")
                        .map(|v| interpret(doc, v, names, index_of, carrier, Form::Direct))
                });
            if let Some(d) = found {
                nav.destinations.push(d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// Two pages, an outline pointing at page two by an explicit array, and a
    /// link on page one pointing at page two through an `/A` action.
    fn navigated() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /OpenAction [3 0 R /Fit] >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Annots [9 0 R] >>",
            )
            .stream(4, "", b"BT ET\n")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>")
            .stream(7, "", b"BT ET\n")
            .object(8, "<< /Type /Outlines /First 10 0 R /Count 1 >>")
            .object(
                9,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
                 /A << /S /GoTo /D [6 0 R /Fit] >> >>",
            )
            .object(10, "<< /Title (Two) /Parent 8 0 R /Dest [6 0 R /Fit] >>")
            .finish("/Root 1 0 R")
    }

    fn nav_of(bytes: Vec<u8>) -> (Document, Navigation) {
        let doc = Document::open(bytes).expect("open");
        let pages = crate::page::pages(&doc).expect("pages");
        let nav = collect(&doc, &pages);
        (doc, nav)
    }

    #[test]
    fn the_page_tree_now_says_where_each_page_is_listed() {
        let doc = Document::open(navigated()).expect("open");
        let pages = crate::page::pages(&doc).expect("pages");

        assert_eq!(pages.pages[0].parent, Some((ObjId::new(2, 0), 0)));
        assert_eq!(pages.pages[1].parent, Some((ObjId::new(2, 0), 1)));
    }

    #[test]
    fn an_action_destination_is_found_not_just_a_direct_one() {
        // The corpus finding this module is shaped by: `/A` `/D` outnumbers
        // bare `/Dest` 3.6 to 1 on links. Finding only `/Dest` would report
        // most real destinations as absent.
        let (_doc, nav) = nav_of(navigated());
        let actions: Vec<_> = nav.destinations.iter().filter(|d| d.form == Form::Action).collect();
        assert_eq!(actions.len(), 1, "the link's /A /D was found: {:?}", nav.destinations);
        assert_eq!(actions[0].page, Some(1));
    }

    #[test]
    fn every_carrier_is_found() {
        let (_doc, nav) = nav_of(navigated());
        let has = |f: &dyn Fn(&Carrier) -> bool| nav.destinations.iter().any(|d| f(&d.carrier));

        assert!(has(&|c| matches!(c, Carrier::Outline(_))), "{:?}", nav.destinations);
        assert!(has(&|c| matches!(c, Carrier::Annotation { .. })), "{:?}", nav.destinations);
        assert!(has(&|c| matches!(c, Carrier::OpenAction)), "{:?}", nav.destinations);
    }

    #[test]
    fn explicit_destinations_resolve_to_a_page_index() {
        let (_doc, nav) = nav_of(navigated());
        for d in &nav.destinations {
            assert_eq!(d.page, Some(1).filter(|_| d.target == Some(ObjId::new(6, 0))).or(d.page));
        }
        assert!(nav.destinations.iter().all(|d| d.page.is_some()), "{:?}", nav.destinations);
        assert_eq!(nav.dangling().count(), 0);
    }

    #[test]
    fn a_named_destination_resolves_through_the_name_tree() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 8 0 R >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [9 0 R] >>")
            .stream(4, "", b"BT ET\n")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>")
            .stream(7, "", b"BT ET\n")
            .object(8, "<< /Names [(chapter2) [6 0 R /Fit]] >>")
            .object(9, "<< /Type /Annot /Subtype /Link /Dest (chapter2) >>")
            .finish("/Root 1 0 R");

        let (_doc, nav) = nav_of(bytes);
        assert_eq!(nav.names["chapter2"].target, Some(ObjId::new(6, 0)));
        assert!(matches!(nav.names["chapter2"].site, Some(NameSite::TreeLeaf { .. })));

        let link = nav
            .destinations
            .iter()
            .find(|d| matches!(d.carrier, Carrier::Annotation { .. }))
            .expect("the link");
        assert_eq!(link.name.as_deref(), Some("chapter2"));
        assert_eq!(link.page, Some(1), "resolved through the tree");
        assert_eq!(nav.dangling().count(), 0);
    }

    #[test]
    fn the_older_root_dests_dictionary_is_read_too() {
        // Five corpus documents use the pre-1.2 form. A fix-up that handled
        // only the name tree would leave them dangling.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /Dests << /intro [3 0 R /Fit] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
            .stream(4, "", b"BT ET\n")
            .finish("/Root 1 0 R");

        let (_doc, nav) = nav_of(bytes);
        assert_eq!(nav.names["intro"].target, Some(ObjId::new(3, 0)));
        assert!(matches!(nav.names["intro"].site, Some(NameSite::RootDests { .. })));
    }

    #[test]
    fn a_name_nothing_defines_is_reported_as_dangling() {
        // §10.9's "silent corruption". The link looks fine; it goes nowhere.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [5 0 R] >>")
            .stream(4, "", b"BT ET\n")
            .object(5, "<< /Type /Annot /Subtype /Link /Dest (nowhere) >>")
            .finish("/Root 1 0 R");

        let (_doc, nav) = nav_of(bytes);
        assert_eq!(nav.dangling().count(), 1);
        assert!(nav.dangling().next().expect("one").unresolved_name);
    }

    #[test]
    fn a_uri_action_is_not_a_dangling_destination() {
        // It points out of the document on purpose. Counting it as broken
        // would make the invariant fire on every file with a hyperlink.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [5 0 R] >>")
            .stream(4, "", b"BT ET\n")
            .object(
                5,
                "<< /Type /Annot /Subtype /Link \
                 /A << /S /URI /URI (https://example.invalid) >> >>",
            )
            .finish("/Root 1 0 R");

        let (_doc, nav) = nav_of(bytes);
        assert_eq!(nav.dangling().count(), 0, "{:?}", nav.destinations);
    }

    #[test]
    fn a_document_with_no_navigation_yields_none() {
        let bytes = rasura_cos::testutil::minimal_classic();
        let (_doc, nav) = nav_of(bytes);
        assert!(nav.destinations.is_empty());
        assert!(!nav.has_page_labels);
        assert!(!nav.has_threads);
    }

    #[test]
    fn an_empty_threads_array_is_not_reported_as_having_threads() {
        // The corpus's single /Threads document has exactly this, and counting
        // it would overstate what needs handling from zero to one.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /Threads 5 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
            .stream(4, "", b"BT ET\n")
            .object(5, "[]")
            .finish("/Root 1 0 R");

        let (_doc, nav) = nav_of(bytes);
        assert!(!nav.has_threads);
    }
}
