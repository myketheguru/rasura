//! The logical structure tree, from `/StructTreeRoot`. Spec 7.8.
//!
//! Everything else in this crate infers structure from geometry. This module
//! reads it from the file, because a tagged PDF already contains the answer:
//! the producer wrote down the reading order, the element types and which
//! marked-content sequences belong to which paragraph.
//!
//! That makes it the only **oracle** available for reading order. Geometry can
//! be checked against geometry forever without either side being right; a
//! structure tree is the author's own statement. Where one is present it wins,
//! and §7.6 already defers to `/MCID` for the same reason.
//!
//! Not every tagged file is trustworthy — plenty of producers emit a tree that
//! disagrees with the page — so the tree is exposed alongside the geometric
//! order rather than replacing it silently, and `DocumentModel` records which
//! one it used.

use rasura_cos::{Dictionary, Document, Object};
use std::collections::HashMap;

/// How deep the tree walk will go before concluding the file is malicious.
const MAX_DEPTH: usize = 64;

/// How many nodes will be visited in total. A structure tree is metadata; one
/// with a hundred thousand nodes is an attack, not a document.
const MAX_NODES: usize = 200_000;

/// One element of the logical structure hierarchy.
#[derive(Debug, Clone)]
pub struct StructElement {
    /// The structure type, `/S`. Standard names are `P`, `H1`..`H6`, `L`, `LI`,
    /// `Table`, `TR`, `TD`, `TH`, `Figure`, `Span`, and so on.
    pub kind: String,
    /// The structure type after resolving `/RoleMap`, when the document remaps
    /// a private type onto a standard one.
    pub role: String,
    /// Children, in the producer's own order. This *is* the reading order.
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    /// Marked-content ids owned directly by this element, with the page each
    /// belongs to.
    pub mcids: Vec<(usize, u32)>,
    /// `/Alt`, the alternative description used by assistive technology.
    pub alt: Option<String>,
    /// `/ActualText`, which overrides the glyphs for text extraction.
    pub actual_text: Option<String>,
    /// `/Lang`, when this subtree declares its own language.
    pub lang: Option<String>,
}

impl StructElement {
    /// Whether this is a block-level structure per the PDF standard roles.
    pub fn is_block(&self) -> bool {
        matches!(
            self.role.as_str(),
            "P" | "H"
                | "H1"
                | "H2"
                | "H3"
                | "H4"
                | "H5"
                | "H6"
                | "L"
                | "LI"
                | "LBody"
                | "Table"
                | "Caption"
                | "Figure"
                | "Formula"
                | "BlockQuote"
                | "TOC"
                | "TOCI"
                | "Index"
        )
    }

    pub fn is_heading(&self) -> bool {
        self.role == "H" || (self.role.len() == 2 && self.role.starts_with('H'))
    }
}

/// A document's logical structure.
#[derive(Debug, Clone, Default)]
pub struct StructTree {
    pub elements: Vec<StructElement>,
    /// Indices of the roots, in order.
    pub roots: Vec<usize>,
    /// Whether the walk hit a limit and the tree is therefore incomplete.
    /// Reported rather than hidden: a truncated tree must not be treated as
    /// authoritative.
    pub truncated: bool,
}

impl StructTree {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Every element in document reading order: depth-first, children in the
    /// order the producer wrote them.
    pub fn in_reading_order(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.elements.len());
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        let mut guard = 0usize;
        while let Some(i) = stack.pop() {
            guard += 1;
            if guard > MAX_NODES {
                break;
            }
            out.push(i);
            if let Some(e) = self.elements.get(i) {
                stack.extend(e.children.iter().rev().copied());
            }
        }
        out
    }

    /// The marked-content ids of one page, in the tree's reading order.
    ///
    /// This is the answer §7.5's XY-cut can only guess at, and the only
    /// independent check on it: two geometric heuristics agreeing proves
    /// nothing, whereas the producer's own ordering is evidence.
    pub fn mcid_order(&self, page: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for i in self.in_reading_order() {
            let Some(e) = self.elements.get(i) else { continue };
            for &(p, mcid) in &e.mcids {
                if p == page {
                    out.push(mcid);
                }
            }
        }
        out
    }

    /// Which element owns a given marked-content id on a given page.
    pub fn owner_of(&self, page: usize, mcid: u32) -> Option<usize> {
        self.elements.iter().position(|e| e.mcids.contains(&(page, mcid)))
    }
}

/// Read `/StructTreeRoot`, if the document is tagged.
pub fn read(doc: &Document) -> Option<StructTree> {
    let catalog = doc.catalog().ok()?;
    let catalog = catalog.as_dict()?;
    let root = doc.resolve(catalog.get("StructTreeRoot")?).ok()?;
    let root = root.as_dict()?.clone();

    // Page object numbers, so a /Pg reference can be turned into a page index.
    let page_index = page_index(doc);
    let role_map = role_map(doc, &root);

    let mut tree = StructTree::default();
    let kids = root.get("K").cloned().unwrap_or(Object::Null);
    let mut budget = MAX_NODES;
    walk(doc, &kids, None, &page_index, &role_map, 0, &mut tree, &mut budget, None);
    tree.truncated = budget == 0;

    if tree.elements.is_empty() { None } else { Some(tree) }
}

/// Map page object numbers to page indices.
fn page_index(doc: &Document) -> HashMap<u32, usize> {
    let mut out = HashMap::new();
    if let Ok(tree) = rasura_content::page::pages(doc) {
        for (i, p) in tree.pages.iter().enumerate() {
            out.insert(p.id.number, i);
        }
    }
    out
}

/// `/RoleMap` maps a producer's private structure types onto standard ones.
fn role_map(doc: &Document, root: &Dictionary) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(map) = root.get("RoleMap") else { return out };
    let Ok(map) = doc.resolve(map) else { return out };
    let Some(map) = map.as_dict() else { return out };
    for (k, v) in map.iter() {
        if let Some(v) = v.as_name() {
            if let (Some(k), Some(v)) = (k.as_str(), v.as_str()) {
                out.insert(k.to_owned(), v.to_owned());
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(
    doc: &Document,
    node: &Object,
    parent: Option<usize>,
    pages: &HashMap<u32, usize>,
    roles: &HashMap<String, String>,
    depth: usize,
    tree: &mut StructTree,
    budget: &mut usize,
    inherited_page: Option<usize>,
) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    let Ok(node) = doc.resolve(node) else { return };

    match &*node {
        // An array of kids: each is walked in order, and that order is the
        // reading order.
        Object::Array(items) => {
            for item in items {
                walk(doc, item, parent, pages, roles, depth + 1, tree, budget, inherited_page);
            }
        }

        // A bare integer is an MCID belonging to the parent element.
        Object::Integer(n) => {
            if let (Some(p), Ok(mcid)) = (parent, u32::try_from(*n))
                && let Some(page) = inherited_page
                && let Some(e) = tree.elements.get_mut(p)
            {
                e.mcids.push((page, mcid));
            }
        }

        Object::Dictionary(dict) => {
            // A marked-content reference or an object reference, not an
            // element: `/Type /MCR` or `/OBJR`.
            let ty = dict
                .get("Type")
                .and_then(|o| o.as_name())
                .and_then(|n| n.as_str())
                .map(str::to_owned);
            let page = dict
                .get("Pg")
                .and_then(|o| o.as_reference())
                .and_then(|r| pages.get(&r.number).copied())
                .or(inherited_page);

            if ty.as_deref() == Some("MCR") {
                if let (Some(p), Some(mcid)) = (parent, dict.get("MCID"))
                    && let Some(mcid) = doc.resolve(mcid).ok().and_then(|o| o.as_i64())
                    && let (Ok(mcid), Some(page)) = (u32::try_from(mcid), page)
                    && let Some(e) = tree.elements.get_mut(p)
                {
                    e.mcids.push((page, mcid));
                }
                return;
            }
            if ty.as_deref() == Some("OBJR") {
                // An annotation, not page content. Nothing to attach.
                return;
            }

            *budget -= 1;
            let kind = dict
                .get("S")
                .and_then(|o| o.as_name())
                .and_then(|n| n.as_str())
                .map(str::to_owned)
                .unwrap_or_default();
            let role = roles.get(&kind).cloned().unwrap_or_else(|| kind.clone());

            let index = tree.elements.len();
            tree.elements.push(StructElement {
                kind,
                role,
                children: Vec::new(),
                parent,
                mcids: Vec::new(),
                alt: text_entry(doc, dict, "Alt"),
                actual_text: text_entry(doc, dict, "ActualText"),
                lang: text_entry(doc, dict, "Lang"),
            });
            match parent {
                Some(p) => tree.elements[p].children.push(index),
                None => tree.roots.push(index),
            }

            if let Some(kids) = dict.get("K") {
                walk(doc, kids, Some(index), pages, roles, depth + 1, tree, budget, page);
            }
        }

        _ => {}
    }
}

fn text_entry(doc: &Document, dict: &Dictionary, key: &str) -> Option<String> {
    let v = doc.resolve(dict.get(key)?).ok()?;
    match &*v {
        Object::String(s) => Some(s.as_text()),
        Object::Name(n) => n.as_str().map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// A tagged document: two paragraphs, each owning one MCID on page 0.
    fn tagged() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /Document /P 7 0 R /K [9 0 R 10 0 R] >>")
            .object(9, "<< /S /P /P 8 0 R /Pg 3 0 R /K 0 >>")
            .object(10, "<< /S /P /P 8 0 R /Pg 3 0 R /K [1 2] >>")
            .finish("/Root 1 0 R")
    }

    #[test]
    fn an_untagged_document_has_no_tree() {
        let doc = Document::open(crate::testutil::page_with("")).unwrap();
        assert!(read(&doc).is_none());
    }

    #[test]
    fn a_tagged_document_yields_its_hierarchy() {
        let doc = Document::open(tagged()).unwrap();
        let tree = read(&doc).expect("tagged");
        assert_eq!(tree.elements.len(), 3);
        assert_eq!(tree.roots, vec![0]);
        assert_eq!(tree.elements[0].role, "Document");
        assert_eq!(tree.elements[0].children, vec![1, 2]);
        assert_eq!(tree.elements[1].parent, Some(0));
    }

    #[test]
    fn mcids_attach_to_their_element_and_page() {
        let doc = Document::open(tagged()).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.elements[1].mcids, vec![(0, 0)]);
        assert_eq!(tree.elements[2].mcids, vec![(0, 1), (0, 2)]);
        assert_eq!(tree.owner_of(0, 2), Some(2));
        assert_eq!(tree.owner_of(0, 99), None);
    }

    #[test]
    fn reading_order_is_depth_first_in_the_producers_order() {
        let doc = Document::open(tagged()).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.in_reading_order(), vec![0, 1, 2]);
        assert_eq!(tree.mcid_order(0), vec![0, 1, 2]);
        assert!(tree.mcid_order(1).is_empty(), "another page owns nothing here");
    }

    #[test]
    fn a_role_map_resolves_private_types() {
        // A producer emitting /Para must still be understood as a paragraph.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] /RoleMap << /Para /P /Head /H1 >> >>")
            .object(8, "<< /S /Para /P 7 0 R /Pg 3 0 R /K 0 >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.elements[0].kind, "Para", "the original type is kept");
        assert_eq!(tree.elements[0].role, "P", "and the standard role resolved");
        assert!(tree.elements[0].is_block());
    }

    #[test]
    fn a_marked_content_reference_dictionary_is_understood() {
        // /MCR is the long form of the same thing, used when content lives on a
        // different page from the element.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /P /P 7 0 R /K [<< /Type /MCR /Pg 3 0 R /MCID 4 >>] >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.elements[0].mcids, vec![(0, 4)]);
    }

    #[test]
    fn an_object_reference_is_not_page_content() {
        // /OBJR points at an annotation. It must not be mistaken for an MCID.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /Link /P 7 0 R /Pg 3 0 R /K [<< /Type /OBJR /Obj 9 0 R >>] >>")
            .object(9, "<< /Type /Annot /Subtype /Link >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.elements.len(), 1);
        assert!(tree.elements[0].mcids.is_empty());
    }

    #[test]
    fn alt_and_actual_text_are_carried() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(
                8,
                "<< /S /Figure /P 7 0 R /Pg 3 0 R /Alt (A bar chart) \
                 /ActualText (Figure 1) /Lang (en-GB) /K 0 >>",
            )
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = read(&doc).unwrap();
        assert_eq!(tree.elements[0].alt.as_deref(), Some("A bar chart"));
        assert_eq!(tree.elements[0].actual_text.as_deref(), Some("Figure 1"));
        assert_eq!(tree.elements[0].lang.as_deref(), Some("en-GB"));
    }

    #[test]
    fn headings_are_recognised() {
        let doc = Document::open(tagged()).unwrap();
        let tree = read(&doc).unwrap();
        assert!(!tree.elements[1].is_heading());

        let e = StructElement {
            kind: "H2".into(),
            role: "H2".into(),
            children: vec![],
            parent: None,
            mcids: vec![],
            alt: None,
            actual_text: None,
            lang: None,
        };
        assert!(e.is_heading());
        assert!(e.is_block());
    }

    #[test]
    fn a_cyclic_tree_terminates() {
        // /K pointing back at an ancestor. A depth limit is the only defence,
        // since the same element legitimately appears under several parents in
        // some producers' output.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(7, "<< /Type /StructTreeRoot /K [8 0 R] >>")
            .object(8, "<< /S /Document /P 7 0 R /K [9 0 R] >>")
            .object(9, "<< /S /P /P 8 0 R /K [8 0 R] >>")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let tree = read(&doc).expect("still returns a tree");
        // Bounded by MAX_DEPTH rather than looping forever.
        assert!(tree.elements.len() <= MAX_DEPTH + 2, "{}", tree.elements.len());
        assert_eq!(tree.in_reading_order().len(), tree.elements.len());
    }
}
