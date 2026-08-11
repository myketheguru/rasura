//! Resource resolution. Spec 6.4.
//!
//! A content stream refers to fonts, XObjects and colour spaces by name, and
//! those names are resolved against a `/Resources` dictionary. Which one is the
//! subtle part: a form XObject has its own `/Resources`, but ISO 32000-1 §8.10.1
//! says that if it is absent, the form inherits the resources of the stream that
//! invoked it -- and many producers omit it even when they should not, or
//! provide one that is missing entries the form actually uses.
//!
//! So this is a *stack*, not a dictionary. Lookup walks from the innermost
//! scope outwards, which handles both the well-formed case and the common
//! malformed one where a form's own dictionary is incomplete.

use rasura_cos::document::Document;
use rasura_cos::{Dictionary, Name, Object};
use std::sync::Arc;

/// The resource categories of ISO 32000-1 Table 34.
pub const CATEGORIES: [&str; 7] =
    ["Font", "XObject", "ExtGState", "ColorSpace", "Pattern", "Shading", "Properties"];

/// A stack of `/Resources` dictionaries, innermost last.
#[derive(Debug, Clone, Default)]
pub struct ResourceStack {
    scopes: Vec<Arc<Object>>,
}

impl ResourceStack {
    pub fn new() -> Self {
        ResourceStack::default()
    }

    /// Start from a page's inherited `/Resources`.
    pub fn from_page(resources: Option<Arc<Object>>) -> Self {
        let mut s = ResourceStack::default();
        if let Some(r) = resources {
            s.push(r);
        }
        s
    }

    pub fn push(&mut self, resources: Arc<Object>) {
        if resources.as_dict().is_some() {
            self.scopes.push(resources);
        }
    }

    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// A stack with one more scope, for descending into a form.
    pub fn with(&self, resources: Option<Arc<Object>>) -> ResourceStack {
        let mut out = self.clone();
        if let Some(r) = resources {
            out.push(r);
        }
        out
    }

    /// Look a name up in one category, innermost scope first.
    pub fn lookup(&self, doc: &Document, category: &str, name: &Name) -> Option<Arc<Object>> {
        for scope in self.scopes.iter().rev() {
            let dict = scope.as_dict()?;
            let Ok(Some(cat)) = doc.get_entry(dict, category) else { continue };
            let Some(cat_dict) = cat.as_dict() else { continue };
            let Some(value) = cat_dict.get_name(name) else { continue };
            if let Ok(resolved) = doc.resolve(value)
                && !resolved.is_null()
            {
                return Some(resolved);
            }
        }
        None
    }

    /// Look a name up without resolving what it points at.
    ///
    /// [`lookup`](Self::lookup) resolves, which is right for everything that
    /// wants the object's *contents* and wrong for anything that needs its
    /// identity. Optional content is the second kind: an `/OC` naming a group
    /// is a statement about which group, and resolving the reference throws
    /// away the only thing that answers it.
    pub fn lookup_raw(&self, doc: &Document, category: &str, name: &Name) -> Option<Object> {
        for scope in self.scopes.iter().rev() {
            let dict = scope.as_dict()?;
            let Ok(Some(cat)) = doc.get_entry(dict, category) else { continue };
            let Some(cat_dict) = cat.as_dict() else { continue };
            if let Some(value) = cat_dict.get_name(name) {
                return Some(value.clone());
            }
        }
        None
    }

    /// `/Font` lookup, which is the one that runs per text operator.
    pub fn font(&self, doc: &Document, name: &Name) -> Option<Arc<Object>> {
        self.lookup(doc, "Font", name)
    }

    /// `/XObject` lookup for `Do`.
    pub fn xobject(&self, doc: &Document, name: &Name) -> Option<Arc<Object>> {
        self.lookup(doc, "XObject", name)
    }

    /// The object id an `/XObject` name resolves to, needed for the cycle guard.
    pub fn xobject_id(&self, doc: &Document, name: &Name) -> Option<rasura_cos::ObjId> {
        self.lookup_id(doc, "XObject", name)
    }

    /// The object id a name resolves to in a category.
    ///
    /// `None` for a direct object, which is legal: a resource may be written
    /// inline rather than as a reference, and then it has no identity to
    /// return. Callers that need identity — a cycle guard, a caller wanting to
    /// go and read the object — have to handle its absence either way.
    pub fn lookup_id(
        &self,
        doc: &Document,
        category: &str,
        name: &Name,
    ) -> Option<rasura_cos::ObjId> {
        for scope in self.scopes.iter().rev() {
            let dict = scope.as_dict()?;
            let Ok(Some(cat)) = doc.get_entry(dict, category) else { continue };
            let Some(cat_dict) = cat.as_dict() else { continue };
            if let Some(Object::Reference(id)) = cat_dict.get_name(name) {
                return Some(*id);
            }
        }
        None
    }

    /// Every name defined in a category, across all scopes. Inner scopes shadow
    /// outer ones, so a name appears once.
    pub fn names_in(&self, doc: &Document, category: &str) -> Vec<Name> {
        let mut out: Vec<Name> = Vec::new();
        for scope in self.scopes.iter().rev() {
            let Some(dict) = scope.as_dict() else { continue };
            let Ok(Some(cat)) = doc.get_entry(dict, category) else { continue };
            let Some(cat_dict) = cat.as_dict() else { continue };
            for key in cat_dict.keys() {
                if !out.contains(key) {
                    out.push(key.clone());
                }
            }
        }
        out
    }
}

/// Read a form XObject's `/Resources`, if it has one.
pub fn form_resources(doc: &Document, form: &Dictionary) -> Option<Arc<Object>> {
    doc.get_entry(form, "Resources").ok().flatten().filter(|r| r.as_dict().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page;
    use rasura_cos::testutil::ClassicBuilder;

    fn doc_with_resources() -> Document {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 5 0 R /F2 6 0 R >> /XObject << /X1 7 0 R >> >> >>",
            )
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>")
            .object(7, "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] >>")
            .finish("/Root 1 0 R");
        Document::open(bytes).unwrap()
    }

    fn page_stack(doc: &Document) -> ResourceStack {
        let p = page::pages(doc).unwrap().pages.remove(0);
        ResourceStack::from_page(p.resources)
    }

    #[test]
    fn looks_up_a_font_by_name() {
        let doc = doc_with_resources();
        let stack = page_stack(&doc);
        let f = stack.font(&doc, &Name::new("F1")).unwrap();
        assert_eq!(
            f.as_dict().unwrap().get("BaseFont").unwrap().as_name().unwrap().as_bytes(),
            b"Helvetica"
        );
    }

    #[test]
    fn a_missing_name_is_none_not_an_error() {
        let doc = doc_with_resources();
        assert!(page_stack(&doc).font(&doc, &Name::new("Nope")).is_none());
    }

    #[test]
    fn inner_scopes_shadow_outer_ones() {
        let doc = doc_with_resources();
        let mut stack = page_stack(&doc);
        // A form that redefines F1 as the Times font.
        let mut inner_font = Dictionary::new();
        inner_font.insert(Name::new("F1"), Object::Reference(rasura_cos::ObjId::new(6, 0)));
        let mut inner = Dictionary::new();
        inner.insert(Name::new("Font"), Object::Dictionary(inner_font));
        stack.push(Arc::new(Object::Dictionary(inner)));

        let f = stack.font(&doc, &Name::new("F1")).unwrap();
        assert_eq!(
            f.as_dict().unwrap().get("BaseFont").unwrap().as_name().unwrap().as_bytes(),
            b"Times-Roman"
        );
    }

    #[test]
    fn an_incomplete_inner_scope_falls_back_to_the_outer_one() {
        // ISO 32000-1 8.10.1 plus the common malformed case: a form whose own
        // /Resources omits a font the form actually uses.
        let doc = doc_with_resources();
        let mut stack = page_stack(&doc);
        let mut inner = Dictionary::new();
        inner.insert(Name::new("Font"), Object::Dictionary(Dictionary::new()));
        stack.push(Arc::new(Object::Dictionary(inner)));

        let f = stack.font(&doc, &Name::new("F2"));
        assert!(f.is_some(), "must fall through to the page's resources");
    }

    #[test]
    fn a_form_with_no_resources_uses_the_invokers() {
        let doc = doc_with_resources();
        let stack = page_stack(&doc);
        let inherited = stack.with(None);
        assert!(inherited.font(&doc, &Name::new("F1")).is_some());
        assert_eq!(inherited.depth(), stack.depth());
    }

    #[test]
    fn xobject_ids_are_available_for_the_cycle_guard() {
        let doc = doc_with_resources();
        let stack = page_stack(&doc);
        assert_eq!(stack.xobject_id(&doc, &Name::new("X1")), Some(rasura_cos::ObjId::new(7, 0)));
        assert!(stack.xobject_id(&doc, &Name::new("Missing")).is_none());
    }

    #[test]
    fn names_in_a_category_are_deduplicated_across_scopes() {
        let doc = doc_with_resources();
        let mut stack = page_stack(&doc);
        let mut inner_font = Dictionary::new();
        inner_font.insert(Name::new("F1"), Object::Reference(rasura_cos::ObjId::new(6, 0)));
        inner_font.insert(Name::new("F9"), Object::Reference(rasura_cos::ObjId::new(6, 0)));
        let mut inner = Dictionary::new();
        inner.insert(Name::new("Font"), Object::Dictionary(inner_font));
        stack.push(Arc::new(Object::Dictionary(inner)));

        let mut names: Vec<String> =
            stack.names_in(&doc, "Font").iter().map(|n| n.as_str().unwrap().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["F1", "F2", "F9"]);
    }

    #[test]
    fn an_empty_stack_resolves_nothing_without_panicking() {
        let doc = doc_with_resources();
        let stack = ResourceStack::new();
        assert!(stack.is_empty());
        assert!(stack.font(&doc, &Name::new("F1")).is_none());
        assert!(stack.names_in(&doc, "Font").is_empty());
    }
}
