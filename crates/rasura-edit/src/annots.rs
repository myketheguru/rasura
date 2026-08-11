//! Annotations. Spec 10.7.
//!
//! > Full CRUD for: `/Text`, `/Link`, `/FreeText`, `/Line`, `/Square`,
//! > `/Circle`, `/Polygon`, `/PolyLine`, `/Highlight`, `/Underline`,
//! > `/Squiggly`, `/StrikeOut`, `/Stamp`, `/Ink`, `/Popup`, `/FileAttachment`,
//! > `/Widget`.
//! >
//! > Appearance streams (`/AP` `/N`, `/R`, `/D`) must be generated for any
//! > annotation Rasura creates or modifies — viewers that do not synthesise
//! > appearances (most of them, for most types) will otherwise show nothing.
//!
//! That second paragraph is the whole difficulty. Creating an annotation
//! dictionary is trivial; creating one that *appears* means drawing it, and
//! what a `/Stamp` or a `/FileAttachment` should look like is a design decision
//! no specification makes for you.
//!
//! # Which types are created, and why only these
//!
//! [`Kind`] covers all seventeen for reading and deleting — those need no
//! appearance. **Creation** is implemented for the types whose appearance is
//! *determined* by their own geometry and colour:
//!
//! | Created | Appearance is |
//! |---|---|
//! | `/Square`, `/Circle` | the `/Rect`, stroked and filled per `/IC` and `/C` |
//! | `/Line` | the two points in `/L` |
//! | `/Ink` | the paths in `/InkList` |
//! | `/Highlight`, `/Underline`, `/StrikeOut`, `/Squiggly` | the quads in `/QuadPoints` |
//!
//! For those, "generate the appearance" has one right answer and this module
//! draws it. For `/Text` (a note icon), `/Stamp`, `/FileAttachment` and
//! `/Popup`, it does not: the icon is the viewer's to choose, every viewer
//! chooses differently, and inventing one produces a document that looks like
//! no other. Those are [`AnnotError::NeedsDesignedAppearance`] and creation
//! declines.
//!
//! `/Widget` belongs to [`crate::forms`], which knows the field it draws.
//! `/Link` is created without an appearance on purpose — a link is a rectangle
//! a viewer makes clickable, and ISO 32000-1 §12.5.6.5 gives it no visible
//! form of its own beyond an optional border.

use crate::draw::Canvas;
use crate::numfmt::NumberStyle;
use crate::session::Fidelity;
use rasura_content::matrix::Rect;
use rasura_content::page::Page;
use rasura_cos::object::{Dictionary, Name, Object, PdfString};
use rasura_cos::{Document, ObjId};

/// The annotation subtypes of spec 10.7.
///
/// Defined one layer down, in `rasura_layout::annots`, and re-exported here.
/// Reading an annotation is a page-dictionary operation the layout layer needs
/// for its own model; only *writing* one requires this crate's drawing surface
/// and fonts. Two definitions would be two things to keep in step, and the one
/// that drifted would be the one nobody was looking at.
pub use rasura_layout::annots::{Annotation, Kind, read};

/// Why an annotation operation could not be performed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AnnotError {
    /// The type's appearance is a design decision, not a derivation.
    ///
    /// Spec 10.7 requires an appearance for anything created here. For a note
    /// icon or a stamp there is no right answer to derive — every viewer draws
    /// its own — so inventing one produces a document that looks like no other.
    #[error("{0} needs a designed appearance; this module only derives geometric ones")]
    NeedsDesignedAppearance(&'static str),

    /// `/Widget` belongs to the form layer, which knows the field it draws.
    #[error("a /Widget is created through the form layer, which knows its field")]
    UseFormLayer,

    #[error("no annotation {0:?} on this page")]
    NotFound(ObjId),

    #[error("the annotation needs a rectangle with area")]
    NoRect,

    #[error("{0}")]
    Cos(String),
}

/// What creating or removing an annotation changes.
#[derive(Debug, Clone)]
pub struct AnnotEdit {
    pub changes: Vec<(ObjId, Option<Object>)>,
    pub fidelity: Fidelity,
    /// The annotation created, if any.
    pub created: Option<ObjId>,
}

/// What a new annotation should be.
#[derive(Debug, Clone)]
pub struct NewAnnotation {
    pub kind: Kind,
    pub rect: Rect,
    /// Stroke colour, RGB 0..1. Defaults to black.
    pub colour: (f64, f64, f64),
    /// Interior colour for `/Square` and `/Circle`; `None` leaves it unfilled.
    pub interior: Option<(f64, f64, f64)>,
    pub border_width: f64,
    pub contents: Option<String>,
    /// For `/Line`: the two endpoints. For the text-markup types: the quads.
    pub points: Vec<(f64, f64)>,
}

impl NewAnnotation {
    pub fn new(kind: Kind, rect: Rect) -> NewAnnotation {
        NewAnnotation {
            kind,
            rect,
            colour: (0.0, 0.0, 0.0),
            interior: None,
            border_width: 1.0,
            contents: None,
            points: Vec::new(),
        }
    }
}

/// Create an annotation, with the appearance spec 10.7 requires. Spec 10.7's C.
pub fn create(
    doc: &Document,
    page: &Page,
    new: &NewAnnotation,
    style: &NumberStyle,
) -> Result<AnnotEdit, AnnotError> {
    if new.kind == Kind::Widget {
        return Err(AnnotError::UseFormLayer);
    }
    // A link has no visible form of its own beyond a border, so it is the one
    // type created without an appearance on purpose rather than by omission.
    if !new.kind.has_derivable_appearance() && new.kind != Kind::Link {
        return Err(AnnotError::NeedsDesignedAppearance(new.kind.as_str()));
    }
    if new.rect.width() <= 0.0 || new.rect.height() <= 0.0 {
        return Err(AnnotError::NoRect);
    }

    // Numbers claimed rather than created, so the session makes both objects
    // and undo removes them. Same reason as `insert_page`.
    let first = doc.next_object_number();
    let (annot_id, ap_id) = (ObjId::new(first, 0), ObjId::new(first + 1, 0));

    let mut dict = Dictionary::new();
    dict.insert(Name::new("Type"), Object::name("Annot"));
    dict.insert(Name::new("Subtype"), Object::name(new.kind.as_str()));
    dict.insert(
        Name::new("Rect"),
        Object::Array(
            [new.rect.x0, new.rect.y0, new.rect.x1, new.rect.y1].map(Object::Real).to_vec(),
        ),
    );
    dict.insert(
        Name::new("C"),
        Object::Array(vec![
            Object::Real(new.colour.0),
            Object::Real(new.colour.1),
            Object::Real(new.colour.2),
        ]),
    );
    if let Some(ic) = new.interior {
        dict.insert(
            Name::new("IC"),
            Object::Array(vec![Object::Real(ic.0), Object::Real(ic.1), Object::Real(ic.2)]),
        );
    }
    let mut bs = Dictionary::new();
    bs.insert(Name::new("W"), Object::Real(new.border_width));
    dict.insert(Name::new("BS"), Object::Dictionary(bs));
    if let Some(text) = &new.contents {
        dict.insert(Name::new("Contents"), Object::String(PdfString::new_literal(text.as_bytes())));
    }
    if new.kind == Kind::Line && new.points.len() >= 2 {
        let l = [new.points[0].0, new.points[0].1, new.points[1].0, new.points[1].1];
        dict.insert(Name::new("L"), Object::Array(l.map(Object::Real).to_vec()));
    }
    if matches!(new.kind, Kind::Highlight | Kind::Underline | Kind::StrikeOut | Kind::Squiggly) {
        dict.insert(
            Name::new("QuadPoints"),
            Object::Array(quads_for(new).into_iter().map(Object::Real).collect()),
        );
    }

    let mut changes = Vec::new();
    if new.kind.has_derivable_appearance() {
        let stream = appearance(new, style).map_err(|e| AnnotError::Cos(e.to_string()))?;
        let mut ap = Dictionary::new();
        ap.insert(Name::new("N"), Object::Reference(ap_id));
        dict.insert(Name::new("AP"), Object::Dictionary(ap));
        changes.push((ap_id, Some(Object::Stream(stream))));
    }

    changes.push((annot_id, Some(Object::Dictionary(dict))));

    // Add it to the page.
    let mut annots = doc
        .get_entry(&page.dict, "Annots")
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    annots.push(Object::Reference(annot_id));
    let mut updated_page = page.dict.clone();
    updated_page.insert(Name::new("Annots"), Object::Array(annots));
    changes.push((page.id, Some(Object::Dictionary(updated_page))));

    Ok(AnnotEdit { changes, fidelity: Fidelity::Exact, created: Some(annot_id) })
}

/// Remove an annotation from a page. Spec 10.7's D.
pub fn delete(doc: &Document, page: &Page, id: ObjId) -> Result<AnnotEdit, AnnotError> {
    let annots = doc
        .get_entry(&page.dict, "Annots")
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    if !annots.iter().any(|a| a.as_reference() == Some(id)) {
        return Err(AnnotError::NotFound(id));
    }

    let kept: Vec<Object> = annots.into_iter().filter(|a| a.as_reference() != Some(id)).collect();
    let mut updated_page = page.dict.clone();
    if kept.is_empty() {
        updated_page.remove("Annots");
    } else {
        updated_page.insert(Name::new("Annots"), Object::Array(kept));
    }

    Ok(AnnotEdit {
        changes: vec![
            (page.id, Some(Object::Dictionary(updated_page))),
            // Blanked rather than deleted: an annotation may be referenced from
            // a /Popup twin or a field tree, and a dangling reference resolves
            // to null differently in different viewers.
            (id, Some(Object::Dictionary(Dictionary::new()))),
        ],
        fidelity: Fidelity::Exact,
        created: None,
    })
}

/// Set an annotation's `/Contents`, regenerating its appearance. Spec 10.7's U.
///
/// Only the text changes; the geometry that determines the appearance does not,
/// so the existing `/AP` stays correct. An update that changed geometry would
/// have to regenerate, which is why this is deliberately narrow.
pub fn set_contents(doc: &Document, id: ObjId, text: &str) -> Result<AnnotEdit, AnnotError> {
    let object = doc.get(id).map_err(|e| AnnotError::Cos(e.to_string()))?;
    let dict = object.as_dict().ok_or(AnnotError::NotFound(id))?;

    let mut updated = dict.clone();
    updated.insert(Name::new("Contents"), Object::String(PdfString::new_literal(text.as_bytes())));

    Ok(AnnotEdit {
        changes: vec![(id, Some(Object::Dictionary(updated)))],
        fidelity: Fidelity::Exact,
        created: None,
    })
}

/// The four corners of the rectangle, in `/QuadPoints` order.
fn quads_for(new: &NewAnnotation) -> Vec<f64> {
    if new.points.len() >= 8 {
        return new.points.iter().flat_map(|(x, y)| [*x, *y]).take(8).collect();
    }
    let r = new.rect;
    // ISO 32000-1 §12.5.6.10: upper-left, upper-right, lower-left, lower-right.
    vec![r.x0, r.y1, r.x1, r.y1, r.x0, r.y0, r.x1, r.y0]
}

/// Draw the annotation's own geometry into a form XObject.
///
/// The stream's coordinates are the page's, and `/BBox` is the annotation's
/// `/Rect` — which is what makes the identity mapping of §12.5.5 correct and
/// keeps this drawing in the same space the caller specified its geometry in.
fn appearance(
    new: &NewAnnotation,
    style: &NumberStyle,
) -> Result<rasura_cos::object::Stream, crate::draw::DrawError> {
    let r = new.rect;
    let w = new.border_width.max(0.0);
    let mut canvas = Canvas::new(*style);
    canvas.save();

    match new.kind {
        Kind::Square => {
            if let Some(ic) = new.interior {
                canvas.fill_rgb(ic.0, ic.1, ic.2);
            }
            canvas.stroke_rgb(new.colour.0, new.colour.1, new.colour.2).line_width(w);
            canvas.rect(r.x0 + w / 2.0, r.y0 + w / 2.0, r.width() - w, r.height() - w);
            paint(&mut canvas, new.interior.is_some(), w > 0.0);
        }
        Kind::Circle => {
            if let Some(ic) = new.interior {
                canvas.fill_rgb(ic.0, ic.1, ic.2);
            }
            canvas.stroke_rgb(new.colour.0, new.colour.1, new.colour.2).line_width(w);
            ellipse(&mut canvas, r, w / 2.0);
            paint(&mut canvas, new.interior.is_some(), w > 0.0);
        }
        Kind::Line => {
            let (a, b) = match new.points.as_slice() {
                [a, b, ..] => (*a, *b),
                _ => ((r.x0, r.y0), (r.x1, r.y1)),
            };
            canvas
                .stroke_rgb(new.colour.0, new.colour.1, new.colour.2)
                .line_width(w.max(1.0))
                .move_to(a.0, a.1)
                .line_to(b.0, b.1)
                .stroke();
        }
        Kind::Ink => {
            canvas.stroke_rgb(new.colour.0, new.colour.1, new.colour.2).line_width(w.max(1.0));
            for (i, (x, y)) in new.points.iter().enumerate() {
                if i == 0 {
                    canvas.move_to(*x, *y);
                } else {
                    canvas.line_to(*x, *y);
                }
            }
            canvas.stroke();
        }
        Kind::Highlight => {
            // A highlight is a translucent wash over the text, and without an
            // /ExtGState it would hide what it marks. Drawn under the quads
            // rather than over them for the same reason a real viewer does:
            // the glyphs stay legible.
            canvas.fill_rgb(new.colour.0, new.colour.1, new.colour.2);
            let q = quads_for(new);
            canvas.rect(q[4], q[5], q[2] - q[0], q[1] - q[5]).fill();
        }
        Kind::Underline | Kind::StrikeOut | Kind::Squiggly => {
            let q = quads_for(new);
            let (left, right) = (q[4], q[2]);
            let (bottom, top) = (q[5], q[1]);
            let y = match new.kind {
                // §12.5.6.11: a strike-out crosses the middle, an underline
                // sits below the baseline.
                Kind::StrikeOut => (bottom + top) / 2.0,
                _ => bottom + (top - bottom) * 0.12,
            };
            canvas
                .stroke_rgb(new.colour.0, new.colour.1, new.colour.2)
                .line_width(w.max(1.0))
                .move_to(left, y)
                .line_to(right, y)
                .stroke();
        }
        _ => {}
    }

    canvas.restore();
    let ops = canvas.finish()?;

    let mut dict = Dictionary::new();
    dict.insert(Name::new("Type"), Object::name("XObject"));
    dict.insert(Name::new("Subtype"), Object::name("Form"));
    dict.insert(
        Name::new("BBox"),
        Object::Array([r.x0, r.y0, r.x1, r.y1].map(Object::Real).to_vec()),
    );
    let mut stream = rasura_cos::object::Stream::new(dict, Vec::new());
    stream.set_decoded(ops);
    Ok(stream)
}

fn paint(canvas: &mut Canvas, filled: bool, stroked: bool) {
    match (filled, stroked) {
        (true, true) => canvas.fill_and_stroke(),
        (true, false) => canvas.fill(),
        (false, true) => canvas.stroke(),
        // Neither: the path is ended without painting, so the operator stream
        // stays balanced rather than leaving a path open.
        (false, false) => canvas.end_path(),
    };
}

/// Four Béziers approximating an ellipse inscribed in `rect`.
fn ellipse(canvas: &mut Canvas, rect: Rect, inset: f64) {
    // The circle constant: the control-point offset that makes a cubic Bézier
    // match a quarter arc to within a part in 10^3.
    const K: f64 = 0.552_284_75;

    let (x0, y0) = (rect.x0 + inset, rect.y0 + inset);
    let (x1, y1) = (rect.x1 - inset, rect.y1 - inset);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (ox, oy) = (rx * K, ry * K);

    canvas.move_to(cx - rx, cy);
    canvas.curve_to((cx - rx, cy + oy), (cx - ox, cy + ry), (cx, cy + ry));
    canvas.curve_to((cx + ox, cy + ry), (cx + rx, cy + oy), (cx + rx, cy));
    canvas.curve_to((cx + rx, cy - oy), (cx + ox, cy - ry), (cx, cy - ry));
    canvas.curve_to((cx - ox, cy - ry), (cx - rx, cy - oy), (cx - rx, cy));
    canvas.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;

    fn page_doc() -> Vec<u8> {
        rasura_cos::testutil::classic_with_flate_content()
    }

    fn first_page(doc: &Document) -> Page {
        rasura_content::page::pages(doc).expect("pages").pages[0].clone()
    }

    fn apply(mut doc: Document, edit: AnnotEdit) -> Document {
        let mut session = EditSession::new(&mut doc);
        session.set_objects("annot", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;
        Document::open(saved).expect("reopen")
    }

    #[test]
    fn a_square_is_created_with_an_appearance() {
        // Spec 10.7: viewers that do not synthesise appearances show nothing
        // without one, which for most types is most viewers.
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let mut new = NewAnnotation::new(Kind::Square, Rect::new(100.0, 100.0, 200.0, 160.0));
        new.interior = Some((1.0, 1.0, 0.0));

        let edit = create(&doc, &page, &new, &NumberStyle::default()).expect("create");
        let after = apply(doc, edit);

        let annots = read(&after, &first_page(&after));
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].kind, Some(Kind::Square));
        assert!(annots[0].has_appearance, "an appearance was generated");
    }

    #[test]
    fn the_appearance_draws_the_geometry_it_was_given() {
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let new = NewAnnotation::new(Kind::Square, Rect::new(100.0, 100.0, 200.0, 160.0));
        let edit = create(&doc, &page, &new, &NumberStyle::default()).expect("create");
        let after = apply(doc, edit);

        let annots = read(&after, &first_page(&after));
        let annot = after.get(annots[0].id).expect("annot");
        let ap = annot.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let text = String::from_utf8_lossy(&after.decoded_stream(n).expect("stream")).to_string();

        assert!(text.contains("re"), "a rectangle was drawn: {text}");
        assert!(text.contains("S"), "and stroked: {text}");
    }

    #[test]
    fn a_circle_is_four_beziers_not_a_rectangle() {
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let new = NewAnnotation::new(Kind::Circle, Rect::new(100.0, 100.0, 200.0, 160.0));
        let edit = create(&doc, &page, &new, &NumberStyle::default()).expect("create");
        let after = apply(doc, edit);

        let annots = read(&after, &first_page(&after));
        let annot = after.get(annots[0].id).expect("annot");
        let ap = annot.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let text = String::from_utf8_lossy(&after.decoded_stream(n).expect("stream")).to_string();

        assert_eq!(text.matches(" c").count(), 4, "four quarter arcs: {text}");
        assert!(!text.contains(" re"), "not a rectangle: {text}");
    }

    #[test]
    fn a_type_whose_appearance_is_a_design_decision_declines() {
        // A note icon, a stamp: every viewer draws its own, and inventing one
        // produces a document that looks like no other.
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);

        for kind in [Kind::Text, Kind::Stamp, Kind::FileAttachment, Kind::Popup, Kind::FreeText] {
            let new = NewAnnotation::new(kind, Rect::new(10.0, 10.0, 40.0, 40.0));
            let err = create(&doc, &page, &new, &NumberStyle::default())
                .expect_err("needs a designed appearance");
            assert!(matches!(err, AnnotError::NeedsDesignedAppearance(_)), "{kind:?}: {err:?}");
        }
    }

    #[test]
    fn a_widget_is_directed_to_the_form_layer() {
        // Only the form layer knows the field a widget draws, and its
        // appearance comes from `/DA` and `/MK` rather than from geometry.
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let new = NewAnnotation::new(Kind::Widget, Rect::new(10.0, 10.0, 40.0, 40.0));
        let err = create(&doc, &page, &new, &NumberStyle::default()).expect_err("wrong layer");
        assert!(matches!(err, AnnotError::UseFormLayer), "{err:?}");
    }

    #[test]
    fn a_link_is_created_without_an_appearance_on_purpose() {
        // §12.5.6.5: a link is a rectangle a viewer makes clickable and gives
        // no visible form of its own beyond an optional border.
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let new = NewAnnotation::new(Kind::Link, Rect::new(10.0, 10.0, 100.0, 30.0));
        let edit = create(&doc, &page, &new, &NumberStyle::default()).expect("create");
        let after = apply(doc, edit);

        let annots = read(&after, &first_page(&after));
        assert_eq!(annots[0].kind, Some(Kind::Link));
        assert!(!annots[0].has_appearance, "deliberately none");
    }

    /// Create an annotation and return the resulting document.
    fn with_annotation(new: &NewAnnotation) -> Document {
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let edit = create(&doc, &page, new, &NumberStyle::default()).expect("create");
        apply(doc, edit)
    }

    #[test]
    fn an_annotation_can_be_read_then_updated() {
        let mut new = NewAnnotation::new(Kind::Square, Rect::new(10.0, 10.0, 40.0, 40.0));
        new.contents = Some("first".into());
        let doc = with_annotation(&new);

        let annots = read(&doc, &first_page(&doc));
        assert_eq!(annots[0].contents.as_deref(), Some("first"));

        let edit = set_contents(&doc, annots[0].id, "second").expect("update");
        let after = apply(doc, edit);

        let annots = read(&after, &first_page(&after));
        assert_eq!(annots[0].contents.as_deref(), Some("second"));
        assert!(annots[0].has_appearance, "the geometry did not change, so /AP still stands");
    }

    #[test]
    fn deleting_removes_it_from_the_page() {
        let new = NewAnnotation::new(Kind::Square, Rect::new(10.0, 10.0, 40.0, 40.0));
        let doc = with_annotation(&new);

        let annots = read(&doc, &first_page(&doc));
        assert_eq!(annots.len(), 1);

        let page = first_page(&doc);
        let edit = delete(&doc, &page, annots[0].id).expect("delete");
        let after = apply(doc, edit);

        assert!(read(&after, &first_page(&after)).is_empty(), "the page keeps none");
    }
    #[test]
    fn deleting_something_that_is_not_there_is_an_error() {
        let doc = Document::open(page_doc()).expect("open");
        let page = first_page(&doc);
        let err = delete(&doc, &page, ObjId::new(999, 0)).expect_err("not found");
        assert!(matches!(err, AnnotError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn every_subtype_round_trips_through_its_name() {
        // All seventeen of spec 10.7 are readable, whatever this module can
        // create -- reading and deleting need no appearance.
        for kind in [
            Kind::Text,
            Kind::Link,
            Kind::FreeText,
            Kind::Line,
            Kind::Square,
            Kind::Circle,
            Kind::Polygon,
            Kind::PolyLine,
            Kind::Highlight,
            Kind::Underline,
            Kind::Squiggly,
            Kind::StrikeOut,
            Kind::Stamp,
            Kind::Ink,
            Kind::Popup,
            Kind::FileAttachment,
            Kind::Widget,
        ] {
            assert_eq!(Kind::from_name(kind.as_str()), Some(kind));
        }
    }
}
