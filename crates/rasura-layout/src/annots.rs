//! Reading a page's annotations. ISO 32000-1 §12.5.
//!
//! Annotations are the fourth of the non-text holes `docs/flow-model.md` lists
//! for step 2, and the one whose absence is easiest to miss:
//!
//! > Annotations are not read. Each of these is a hole a re-laid-out page would
//! > fall through.
//!
//! The reason it matters here rather than only in the edit layer is that an
//! annotation can carry **text a reader sees and the content stream does not**.
//! A filled form field's value lives in `/V` and is painted by an appearance
//! stream hanging off the widget; a sticky note's text lives in `/Contents` and
//! is painted by the viewer. A document model built only from page content
//! reports both pages as empty, which is exactly what the corpus survey found
//! for the `annotation-tx*.pdf` fixtures.
//!
//! # Reading, not editing
//!
//! Only reading is here. Creating an annotation means generating an appearance
//! stream, which needs a drawing surface and a font — `rasura-edit`'s business,
//! and it re-exports these types so there is one `Kind` in the workspace rather
//! than two that drift.

use rasura_content::matrix::Rect;
use rasura_content::page::Page;
use rasura_cos::object::{Dictionary, Object};
use rasura_cos::{Document, ObjId};

/// An annotation subtype this library models. ISO 32000-1 §12.5.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Link,
    FreeText,
    Line,
    Square,
    Circle,
    Polygon,
    PolyLine,
    Highlight,
    Underline,
    Squiggly,
    StrikeOut,
    Stamp,
    Ink,
    Popup,
    FileAttachment,
    Widget,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Link => "Link",
            Kind::FreeText => "FreeText",
            Kind::Line => "Line",
            Kind::Square => "Square",
            Kind::Circle => "Circle",
            Kind::Polygon => "Polygon",
            Kind::PolyLine => "PolyLine",
            Kind::Highlight => "Highlight",
            Kind::Underline => "Underline",
            Kind::Squiggly => "Squiggly",
            Kind::StrikeOut => "StrikeOut",
            Kind::Stamp => "Stamp",
            Kind::Ink => "Ink",
            Kind::Popup => "Popup",
            Kind::FileAttachment => "FileAttachment",
            Kind::Widget => "Widget",
        }
    }

    /// The inverse of [`Kind::as_str`].
    ///
    /// Deliberately not `FromStr`: that trait's `Err` would have to describe a
    /// `/Subtype` this module does not handle, and every caller treats that as
    /// "leave the annotation alone" rather than as a failure.
    pub fn from_name(name: &str) -> Option<Kind> {
        Some(match name {
            "Text" => Kind::Text,
            "Link" => Kind::Link,
            "FreeText" => Kind::FreeText,
            "Line" => Kind::Line,
            "Square" => Kind::Square,
            "Circle" => Kind::Circle,
            "Polygon" => Kind::Polygon,
            "PolyLine" => Kind::PolyLine,
            "Highlight" => Kind::Highlight,
            "Underline" => Kind::Underline,
            "Squiggly" => Kind::Squiggly,
            "StrikeOut" => Kind::StrikeOut,
            "Stamp" => Kind::Stamp,
            "Ink" => Kind::Ink,
            "Popup" => Kind::Popup,
            "FileAttachment" => Kind::FileAttachment,
            "Widget" => Kind::Widget,
            _ => return None,
        })
    }

    /// Whether the edit layer can draw this type's appearance from its own
    /// data.
    pub fn has_derivable_appearance(self) -> bool {
        matches!(
            self,
            Kind::Square
                | Kind::Circle
                | Kind::Line
                | Kind::Ink
                | Kind::Highlight
                | Kind::Underline
                | Kind::StrikeOut
                | Kind::Squiggly
        )
    }

    /// Whether the type carries text a reader is meant to read.
    ///
    /// `Link` and `Popup` do not: a link's `/Contents` is a description of where
    /// it goes, and a popup repeats its parent's text. Including either would
    /// duplicate content into an export rather than recover it.
    pub fn carries_text(self) -> bool {
        matches!(
            self,
            Kind::Text | Kind::FreeText | Kind::Widget | Kind::Stamp | Kind::FileAttachment
        )
    }
}

/// One annotation as read from a page.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: ObjId,
    pub kind: Option<Kind>,
    pub rect: Option<Rect>,
    /// `/Contents`, the human-readable text.
    pub contents: Option<String>,
    pub has_appearance: bool,
    /// `/V` for a form field, as text where it is a string or a name.
    ///
    /// Inherited up the field tree, because a widget frequently carries no `/V`
    /// of its own and the value sits on its parent. Reading only the widget
    /// reports every such field as empty.
    pub value: Option<String>,
    /// The field's partial name, `/T`.
    pub field_name: Option<String>,
    /// `/F` bit 2 (Hidden) or bit 6 (NoView): the annotation is in the file and
    /// is not shown.
    ///
    /// Worth knowing before putting its text into an export — a hidden field's
    /// value is not something the reader of the page can see.
    pub hidden: bool,
}

impl Annotation {
    /// The text this annotation puts in front of a reader, if any.
    ///
    /// A widget shows its value; everything else shows its `/Contents`. Hidden
    /// annotations show nothing, whatever they contain.
    pub fn visible_text(&self) -> Option<&str> {
        if self.hidden {
            return None;
        }
        if !self.kind?.carries_text() {
            return None;
        }
        let text = match self.kind {
            Some(Kind::Widget) => self.value.as_deref(),
            _ => self.contents.as_deref(),
        }?;
        (!text.trim().is_empty()).then_some(text)
    }
}

/// Read a page's annotations. Spec 10.7's R.
pub fn read(doc: &Document, page: &Page) -> Vec<Annotation> {
    let Some(annots) = doc.get_entry(&page.dict, "Annots").ok().flatten() else {
        return Vec::new();
    };
    let Some(array) = annots.as_array() else { return Vec::new() };

    array
        .iter()
        .filter_map(|entry| {
            let id = entry.as_reference()?;
            let object = doc.resolve(entry).ok()?;
            let dict = object.as_dict()?;
            let flags = dict.get("F").and_then(Object::as_i64).unwrap_or(0);
            Some(Annotation {
                id,
                kind: dict
                    .get("Subtype")
                    .and_then(Object::as_name)
                    .and_then(|n| n.as_str())
                    .and_then(Kind::from_name),
                rect: rect_of(doc, dict),
                contents: dict.get("Contents").and_then(Object::as_string).map(|s| s.as_text()),
                has_appearance: dict.get("AP").is_some(),
                value: inherited_value(doc, dict, 0),
                field_name: dict.get("T").and_then(Object::as_string).map(|s| s.as_text()),
                // Bit positions are 1-based in the specification, so Hidden is
                // bit 2 and NoView is bit 6.
                hidden: flags & 0b10 != 0 || flags & 0b10_0000 != 0,
            })
        })
        .collect()
}

/// `/V`, walking up `/Parent` when the widget does not carry one.
///
/// A form field is frequently split: the field dictionary holds `/T` and `/V`,
/// and one or more widget annotations hold the geometry. Reading only the
/// widget reports every such field as having no value.
fn inherited_value(doc: &Document, dict: &Dictionary, depth: usize) -> Option<String> {
    // A malformed file can make `/Parent` a cycle. Ten is far beyond any real
    // field tree and terminates one that is not real.
    if depth > 10 {
        return None;
    }
    if let Some(v) = dict.get("V") {
        let resolved = doc.resolve(v).ok()?;
        if let Some(s) = resolved.as_string() {
            return Some(s.as_text());
        }
        if let Some(n) = resolved.as_name().and_then(|n| n.as_str()) {
            // A checkbox or radio group's value is a name: `/Off`, or the name
            // of the selected appearance state.
            return Some(n.to_string());
        }
        return None;
    }
    let parent = doc.resolve(dict.get("Parent")?).ok()?;
    inherited_value(doc, parent.as_dict()?, depth + 1)
}

fn rect_of(doc: &Document, dict: &Dictionary) -> Option<Rect> {
    let resolved = doc.resolve(dict.get("Rect")?).ok()?;
    let a = resolved.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let n = |i: usize| doc.resolve(&a[i]).ok().and_then(|o| o.as_f64());
    let (x0, y0, x1, y1) = (n(0)?, n(1)?, n(2)?, n(3)?);
    Some(Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn with_annots(entries: &str, extra: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                &format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                     /Annots [{entries}] >>"
                ),
            )
            .stream(4, "", b"\n")
            .object(9, extra)
            .finish("/Root 1 0 R")
    }

    fn read_one(bytes: Vec<u8>) -> Annotation {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let mut list = read(&doc, &pages.pages[0]);
        assert_eq!(list.len(), 1, "{list:?}");
        list.remove(0)
    }

    #[test]
    fn a_sticky_note_offers_its_contents() {
        let a = read_one(with_annots(
            "9 0 R",
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (check this figure) >>",
        ));
        assert_eq!(a.kind, Some(Kind::Text));
        assert_eq!(a.visible_text(), Some("check this figure"));
    }

    #[test]
    fn a_widget_offers_its_value_rather_than_its_contents() {
        let a = read_one(with_annots(
            "9 0 R",
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (signatory) /Rect [10 10 30 30] \
             /V (A. Ozdamar) /Contents (tooltip nobody reads) >>",
        ));
        assert_eq!(a.field_name.as_deref(), Some("signatory"));
        assert_eq!(a.visible_text(), Some("A. Ozdamar"), "the value is what is painted");
    }

    #[test]
    fn a_widget_inherits_its_value_from_the_field_it_belongs_to() {
        // The split that makes a naive reader report every such field as empty:
        // `/V` on the field, geometry on the widget.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Annots [9 0 R] >>",
            )
            .stream(4, "", b"\n")
            .object(9, "<< /Type /Annot /Subtype /Widget /Rect [10 10 30 30] /Parent 10 0 R >>")
            .object(10, "<< /FT /Tx /T (signatory) /V (from the parent) >>")
            .finish("/Root 1 0 R");

        let a = read_one(bytes);
        assert_eq!(a.visible_text(), Some("from the parent"));
    }

    #[test]
    fn a_hidden_annotation_shows_nothing() {
        // `/F 2` is the Hidden flag. Its text is in the file and not on the
        // page, and an export that included it would show the reader something
        // the document does not.
        let a = read_one(with_annots(
            "9 0 R",
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /F 2 /Contents (secret) >>",
        ));
        assert!(a.hidden);
        assert_eq!(a.visible_text(), None);
        assert_eq!(a.contents.as_deref(), Some("secret"), "still read, just not shown");
    }

    #[test]
    fn a_link_carries_no_reader_text() {
        // A link's `/Contents` describes where it goes. Putting it in an export
        // would invent text the page never showed.
        let a = read_one(with_annots(
            "9 0 R",
            "<< /Type /Annot /Subtype /Link /Rect [10 10 30 30] /Contents (go to page 4) >>",
        ));
        assert_eq!(a.kind, Some(Kind::Link));
        assert_eq!(a.visible_text(), None);
    }

    #[test]
    fn a_cyclic_parent_chain_terminates() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Annots [9 0 R] >>",
            )
            .stream(4, "", b"\n")
            .object(9, "<< /Type /Annot /Subtype /Widget /Rect [0 0 1 1] /Parent 10 0 R >>")
            .object(10, "<< /Parent 9 0 R >>")
            .finish("/Root 1 0 R");

        let a = read_one(bytes);
        assert_eq!(a.value, None);
    }
}
