//! Turning annotations into page content. Spec 10.8.
//!
//! > Field flattening: convert widget appearances into page content and remove
//! > the fields. Common request; implement it.
//!
//! A filled form is two documents in one. The values live in the field tree as
//! `/V` entries, and what a reader *sees* is an appearance stream the viewer
//! draws on top of the page. Flattening collapses them: the appearance becomes
//! ordinary page content and the interactive part goes away.
//!
//! # Why it draws the appearance rather than the value
//!
//! The obvious implementation reads `/V`, picks the font from `/DA`, and lays
//! the text out again. That reproduces the *data* and not the *appearance* —
//! and the appearance is what the person filling the form saw and approved.
//! Alignment, scaling, comb spacing, a `/MK` border, a chosen radio button's
//! glyph, an ink signature's path: none of that is in `/V`, and a re-render
//! silently produces a document that differs from what was signed off.
//!
//! So the existing `/AP` `/N` stream is invoked as a form XObject at the
//! annotation's own rectangle. The bytes a viewer would have drawn are the
//! bytes that get drawn, which is the same principle as
//! [`crate::blocks`]'s wrap: carry the original through rather than
//! regenerate it.
//!
//! An annotation with no appearance stream is **not** flattened, and is
//! reported. Spec 10.7 says appearances must be generated for anything this
//! library creates or modifies; generating one here — inventing what a viewer
//! *might* have shown — is a different and much less safe operation than
//! preserving one that exists.

use crate::draw::Canvas;
use crate::numfmt::NumberStyle;
use crate::patch::Patch;
use crate::session::Fidelity;
use rasura_content::matrix::{Matrix, Rect};
use rasura_content::page::Page;
use rasura_cos::object::{Dictionary, Name, Object};
use rasura_cos::{Document, ObjId};

/// Why flattening could not proceed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FlattenError {
    #[error("the page has no annotations to flatten")]
    NothingToFlatten,

    #[error("{0}")]
    Cos(String),
}

/// An annotation that could not be flattened, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub annotation: ObjId,
    pub reason: &'static str,
}

/// The result of flattening one page.
#[derive(Debug, Clone)]
pub struct Flattened {
    /// Content appended to the page, drawing the appearances.
    pub patches: Vec<Patch>,
    /// The page's rewritten `/Annots` and `/Resources`.
    pub changes: Vec<(ObjId, Option<Object>)>,
    /// How many annotations became page content.
    pub drawn: usize,
    /// Annotations left interactive, with the reason.
    pub skipped: Vec<Skipped>,
    pub fidelity: Fidelity,
}

/// Draw every annotation's appearance into the page and remove it. Spec 10.8.
///
/// The page's `/Resources` `/XObject` gains an entry per flattened annotation,
/// under a generated name that cannot collide with an existing one.
pub fn flatten_annotations(
    doc: &Document,
    page: &Page,
    content_len: usize,
    style: &NumberStyle,
) -> Result<Flattened, FlattenError> {
    let annots = doc
        .get_entry(&page.dict, "Annots")
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    if annots.is_empty() {
        return Err(FlattenError::NothingToFlatten);
    }

    // Existing XObject names, so a generated one cannot shadow a real resource.
    let resources = page.resources.as_ref().and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut xobjects = doc
        .get_entry(&resources, "XObject")
        .ok()
        .flatten()
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();

    let mut canvas = Canvas::new(*style);
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut drawn = 0usize;

    for entry in &annots {
        let Some(id) = entry.as_reference() else {
            kept.push(entry.clone());
            continue;
        };
        let Ok(object) = doc.get(id) else {
            kept.push(entry.clone());
            continue;
        };
        let Some(dict) = object.as_dict() else {
            kept.push(entry.clone());
            continue;
        };

        // Hidden or NoView annotations are not drawn by a viewer, so flattening
        // them would *add* marks the reader never saw. Bit 2 is Hidden, bit 6
        // NoView, per ISO 32000-1 table 165.
        let flags = dict.get("F").and_then(Object::as_i64).unwrap_or(0);
        if flags & 0b10 != 0 || flags & 0b10_0000 != 0 {
            kept.push(entry.clone());
            skipped.push(Skipped { annotation: id, reason: "hidden or not viewable" });
            continue;
        }

        let Some(appearance) = normal_appearance(doc, dict) else {
            // Not flattened and left interactive: inventing an appearance is a
            // different and much less safe operation than preserving one.
            kept.push(entry.clone());
            skipped.push(Skipped { annotation: id, reason: "no /AP /N appearance stream" });
            continue;
        };
        let Some(rect) = rect_of(doc, dict) else {
            kept.push(entry.clone());
            skipped.push(Skipped { annotation: id, reason: "no usable /Rect" });
            continue;
        };

        let name = Name::new(format!("RasuraFlat{drawn}"));
        xobjects.insert(name.clone(), Object::Reference(appearance.id));

        // ISO 32000-1 §12.5.5: the form's /BBox is transformed by its /Matrix,
        // and the result is mapped to the annotation's /Rect. Skipping that
        // step draws the appearance at its own coordinates, which for a
        // rotated or offset BBox is the wrong place and the wrong size.
        canvas.save();
        canvas.concat(fit_to_rect(&appearance, rect));
        canvas.push_xobject(&name);
        canvas.restore();
        drawn += 1;
    }

    if drawn == 0 {
        return Err(FlattenError::NothingToFlatten);
    }

    let bytes = canvas.finish().map_err(|e| FlattenError::Cos(e.to_string()))?;

    let mut updated_resources = resources.clone();
    updated_resources.insert(Name::new("XObject"), Object::Dictionary(xobjects));

    let mut updated_page = page.dict.clone();
    updated_page.insert(Name::new("Resources"), Object::Dictionary(updated_resources));
    if kept.is_empty() {
        updated_page.remove("Annots");
    } else {
        updated_page.insert(Name::new("Annots"), Object::Array(kept));
    }

    let fidelity = if skipped.is_empty() {
        Fidelity::Exact
    } else {
        Fidelity::Degraded(vec![crate::session::Compromise::AnnotationsLeftInteractive {
            count: skipped.len(),
        }])
    };

    Ok(Flattened {
        // Appended, so the appearances paint over the page exactly as a viewer
        // draws them: annotations are painted after page content, and putting
        // them anywhere else would change what is on top.
        patches: vec![Patch::insert(content_len, bytes)],
        changes: vec![(page.id, Some(Object::Dictionary(updated_page)))],
        drawn,
        skipped,
        fidelity,
    })
}

/// A form XObject serving as an annotation's normal appearance.
struct Appearance {
    id: ObjId,
    bbox: Rect,
    matrix: Matrix,
}

fn normal_appearance(doc: &Document, annot: &Dictionary) -> Option<Appearance> {
    let ap = doc.get_entry(annot, "AP").ok()??;
    let ap = ap.as_dict()?;
    let normal = ap.get("N")?;

    // `/N` is either the stream itself or, for a check box or radio button, a
    // dictionary of states keyed by appearance name. The one to draw is the
    // state `/AS` names -- drawing any other would show a box ticked that is
    // not, which is the single most consequential thing to get wrong here.
    let chosen = match doc.resolve(normal).ok()?.as_dict() {
        Some(states) if doc.resolve(normal).ok()?.as_stream().is_none() => {
            let state = annot.get("AS").and_then(Object::as_name)?;
            states.get_name(state)?.clone()
        }
        _ => normal.clone(),
    };

    let id = chosen.as_reference()?;
    let resolved = doc.resolve(&chosen).ok()?;
    let stream = resolved.as_stream()?;

    let bbox = rect_from(doc, stream.dict.get("BBox")?)?;
    let matrix = stream
        .dict
        .get("Matrix")
        .and_then(Object::as_array)
        .filter(|a| a.len() == 6)
        .map(|a| {
            let n = |i: usize| a[i].as_f64().unwrap_or(0.0);
            Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5))
        })
        .unwrap_or(Matrix::IDENTITY);

    Some(Appearance { id, bbox, matrix })
}

fn rect_of(doc: &Document, annot: &Dictionary) -> Option<Rect> {
    let rect = rect_from(doc, annot.get("Rect")?)?;
    (rect.width().abs() > 0.0 && rect.height().abs() > 0.0).then_some(rect)
}

fn rect_from(doc: &Document, value: &Object) -> Option<Rect> {
    let resolved = doc.resolve(value).ok()?;
    let a = resolved.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let n = |i: usize| doc.resolve(&a[i]).ok().and_then(|o| o.as_f64());
    let (x0, y0, x1, y1) = (n(0)?, n(1)?, n(2)?, n(3)?);
    Some(Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
}

/// The transform mapping a form's transformed `/BBox` onto an annotation's
/// `/Rect`. ISO 32000-1 §12.5.5.
fn fit_to_rect(appearance: &Appearance, rect: Rect) -> Matrix {
    use rasura_content::matrix::Point;

    // The bounding box of the BBox's four corners after /Matrix.
    let corners = [
        Point { x: appearance.bbox.x0, y: appearance.bbox.y0 },
        Point { x: appearance.bbox.x1, y: appearance.bbox.y0 },
        Point { x: appearance.bbox.x1, y: appearance.bbox.y1 },
        Point { x: appearance.bbox.x0, y: appearance.bbox.y1 },
    ];
    let mapped: Vec<Point> = corners.iter().map(|p| appearance.matrix.apply(*p)).collect();
    let (mut lo_x, mut lo_y) = (f64::MAX, f64::MAX);
    let (mut hi_x, mut hi_y) = (f64::MIN, f64::MIN);
    for p in &mapped {
        lo_x = lo_x.min(p.x);
        lo_y = lo_y.min(p.y);
        hi_x = hi_x.max(p.x);
        hi_y = hi_y.max(p.y);
    }

    // A degenerate transformed box cannot be scaled onto anything; place it at
    // the rectangle's corner rather than dividing by zero.
    let sx = if hi_x > lo_x { rect.width() / (hi_x - lo_x) } else { 1.0 };
    let sy = if hi_y > lo_y { rect.height() / (hi_y - lo_y) } else { 1.0 };

    Matrix::new(sx, 0.0, 0.0, sy, rect.x0 - lo_x * sx, rect.y0 - lo_y * sy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page with one widget annotation carrying an appearance stream.
    fn form(annot_extra: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (page text) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(
                6,
                &format!(
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Kowalski) \
                     /Rect [100 500 300 530] {annot_extra} >>"
                ),
            )
            .stream(
                7,
                "/Type /XObject /Subtype /Form /BBox [0 0 200 30] \
                 /Resources << /Font << /F1 5 0 R >> >>",
                b"BT /F1 12 Tf 1 0 0 1 2 8 Tm (Kowalski) Tj ET\n",
            )
            .finish("/Root 1 0 R")
    }

    fn flatten(bytes: Vec<u8>) -> Result<(Document, Flattened), FlattenError> {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = pages.pages[0].clone();
        let (content, _) =
            rasura_content::content::page_content(&doc, &page.dict).expect("content");
        let out = flatten_annotations(&doc, &page, content.data().len(), &NumberStyle::default())?;
        Ok((doc, out))
    }

    #[test]
    fn a_widget_with_an_appearance_becomes_page_content() {
        let (mut doc, out) = flatten(form("/AP << /N 7 0 R >>")).expect("flatten");
        assert_eq!(out.drawn, 1);
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);

        let pages = rasura_content::page::pages(&doc).expect("pages");
        let (content, _) =
            rasura_content::content::page_content(&doc, &pages.pages[0].dict).expect("content");

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content("flatten", &content, &out.patches, out.fidelity.clone())
            .expect("patch");
        session.set_objects("flatten", &out.changes, out.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let after_pages = rasura_content::page::pages(&after).expect("pages");

        // The annotation is gone.
        assert!(
            after.get_entry(&after_pages.pages[0].dict, "Annots").ok().flatten().is_none(),
            "the page keeps no annotations"
        );

        // And its text is now drawn by the page, reachable through the ordinary
        // extraction chain rather than through an annotation.
        let page = crate::EditablePage::analyse(&after, &after_pages.pages[0]).expect("analyse");
        let text: String = page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect();
        assert!(text.contains("Kowalski"), "the appearance is now content: {text:?}");
        assert!(text.contains("page text"), "and the original content survives");
    }

    #[test]
    fn the_appearance_lands_in_the_annotations_rectangle() {
        // §12.5.5: the form's /BBox is mapped onto /Rect. Drawing it at its own
        // coordinates would put a 200x30 appearance at the page origin instead
        // of at (100, 500).
        let (mut doc, out) = flatten(form("/AP << /N 7 0 R >>")).expect("flatten");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let (content, _) =
            rasura_content::content::page_content(&doc, &pages.pages[0].dict).expect("content");

        let mut session = EditSession::new(&mut doc);
        session
            .patch_content("flatten", &content, &out.patches, out.fidelity.clone())
            .expect("patch");
        session.set_objects("flatten", &out.changes, out.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let after_pages = rasura_content::page::pages(&after).expect("pages");
        let page = crate::EditablePage::analyse(&after, &after_pages.pages[0]).expect("analyse");

        // The flattened glyphs sit inside the annotation rectangle, which in
        // device space is y = 792 - 530 .. 792 - 500.
        let flat =
            page.runs.iter().find(|r| r.text().contains("Kowalski")).expect("the flattened run");
        let origin = flat.run.glyphs[0].origin;
        assert!((100.0..300.0).contains(&origin.x), "{origin:?}");
        assert!((262.0..292.0).contains(&origin.y), "{origin:?}");
    }

    #[test]
    fn an_annotation_with_no_appearance_is_left_alone_and_reported() {
        // Inventing an appearance -- guessing what a viewer might have shown --
        // is a different and much less safe operation than preserving one.
        let err = flatten(form("")).expect_err("nothing flattenable");
        assert!(matches!(err, FlattenError::NothingToFlatten), "{err:?}");
    }

    #[test]
    fn a_hidden_annotation_is_not_drawn() {
        // A viewer does not draw it, so flattening it would *add* marks the
        // reader never saw. /F bit 2 is Hidden.
        let err = flatten(form("/AP << /N 7 0 R >> /F 2")).expect_err("hidden");
        assert!(matches!(err, FlattenError::NothingToFlatten), "{err:?}");
    }

    #[test]
    fn a_check_box_draws_the_state_that_is_selected() {
        // The single most consequential thing to get wrong: drawing /Off for a
        // box that is ticked, or the tick for one that is not.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
            )
            .stream(4, "", b"BT ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(
                6,
                "<< /Type /Annot /Subtype /Widget /FT /Btn /Rect [100 500 120 520] \
                 /AS /Yes /AP << /N << /Yes 7 0 R /Off 8 0 R >> >> >>",
            )
            .stream(
                7,
                "/Type /XObject /Subtype /Form /BBox [0 0 20 20] \
                 /Resources << /Font << /F1 5 0 R >> >>",
                b"BT /F1 12 Tf 1 0 0 1 4 4 Tm (TICKED) Tj ET\n",
            )
            .stream(
                8,
                "/Type /XObject /Subtype /Form /BBox [0 0 20 20] \
                 /Resources << /Font << /F1 5 0 R >> >>",
                b"BT /F1 12 Tf 1 0 0 1 4 4 Tm (EMPTY) Tj ET\n",
            )
            .finish("/Root 1 0 R");

        let (mut doc, out) = flatten(bytes).expect("flatten");
        assert_eq!(out.drawn, 1);

        let pages = rasura_content::page::pages(&doc).expect("pages");
        let (content, _) =
            rasura_content::content::page_content(&doc, &pages.pages[0].dict).expect("content");
        let mut session = EditSession::new(&mut doc);
        session
            .patch_content("flatten", &content, &out.patches, out.fidelity.clone())
            .expect("patch");
        session.set_objects("flatten", &out.changes, out.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let after_pages = rasura_content::page::pages(&after).expect("pages");
        let page = crate::EditablePage::analyse(&after, &after_pages.pages[0]).expect("analyse");
        let text: String = page.paragraphs.iter().map(|(id, _)| page.text_of(*id)).collect();

        assert!(text.contains("TICKED"), "the selected state was drawn: {text:?}");
        assert!(!text.contains("EMPTY"), "and the unselected one was not: {text:?}");
    }

    #[test]
    fn a_page_with_no_annotations_says_so() {
        let bytes = rasura_cos::testutil::classic_with_flate_content();
        let err = flatten(bytes).expect_err("nothing to flatten");
        assert!(matches!(err, FlattenError::NothingToFlatten), "{err:?}");
    }
}
