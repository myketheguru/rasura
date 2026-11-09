//! Adding content: placed text, and new form fields. Spec 9.2.
//!
//! Everything else in this crate rewrites marks that already exist. These two
//! produce marks that never did — a value typed onto a flat government form with
//! no field to hold it, and a field created where the form has none. This is
//! what folding a "probe the layout, then fill it" tool into the library looks
//! like: the probing is [`crate::locate`]'s paragraphs and [`crate::annots`]'s
//! primitives; this is the placing.
//!
//! # Both are object edits, not stream splices
//!
//! Neither touches the bytes of a content stream that is already there. Placed
//! text is a *new* content stream appended to the page's `/Contents`, drawn with
//! a standard face added to the page's resources; a new field is a new widget
//! object linked into `/Annots` and the AcroForm. So both stage through
//! [`crate::EditSession::set_objects`] and undo by restoring the objects they
//! wrote — the same guarantee a page delete gives, for the same reason, and
//! without the hazard of splicing into a multi-stream `/Contents`.
//!
//! # The standard face, on purpose
//!
//! Placed text is drawn in Helvetica — a base-14 font, always present, never
//! embedded — not in the document's own typeface. For text that is *being
//! added* that is the right choice, not a compromise: a flat form was printed
//! in some face the file may not even embed, and matching it is neither possible
//! nor what a filled-in value should look like. A value typed into a form reads
//! as typed in, which is exactly what a clean standard face gives.
//!
//! # Where it refuses
//!
//! A page can inherit its `/Resources` from the page tree, or share them by
//! reference with other pages. Adding a font means either editing that shared
//! object — harmless, since an unused font changes no page's rendering — or, when
//! the page has no resources of its own to edit and the structure is one this
//! module does not model, refusing with a code rather than inventing a
//! `/Resources` that shadows the inherited one and drops every font the existing
//! content relied on.

use crate::draw::Canvas;
use crate::forms;
use crate::numfmt::NumberStyle;
use crate::session::{Compromise, Fidelity};
use rasura_content::matrix::Rect;
use rasura_content::page::Page;
use rasura_cos::object::{Dictionary, Name, Object, PdfString, Stream};
use rasura_cos::{Document, ObjId};
use rasura_font::create::win_ansi_code;

/// The resource name placed text is drawn with. Distinctive so it cannot
/// collide with a font the page already declares.
const PLACED_FONT: &str = "RasuraHelv";

/// Why placing content could not be done.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceError {
    /// Not one character of the text has a WinAnsi code.
    #[error("no character of {0:?} can be drawn by a standard WinAnsi font")]
    Unencodable(String),
    /// A `near_label` anchor named a label no paragraph on the page carries.
    #[error("no paragraph on this page reads {0:?}")]
    LabelNotFound(String),
    /// The target rectangle has zero or negative area.
    #[error("the rectangle has no area")]
    NoRect,
    /// The page's resources are inherited or shared in a shape this module does
    /// not edit, so a font cannot be added without risking another page.
    #[error("this page's resources are inherited or shared in an unsupported shape")]
    UnsupportedResources,
    #[error("{0}")]
    Cos(String),
}

/// What placing content changed.
#[derive(Debug, Clone)]
pub struct PlaceEdit {
    pub changes: Vec<(ObjId, Option<Object>)>,
    pub fidelity: Fidelity,
    /// Characters a standard WinAnsi face could not draw, dropped from the
    /// output. Reported, never silent.
    pub missing: Vec<char>,
    /// The field created, for `create_text_field`.
    pub created: Option<ObjId>,
}

/// A Helvetica font dictionary, inline — a base-14 font needs no descriptor and
/// no embedded program, so it lives directly in the resources it is named from.
fn helvetica() -> Object {
    let mut d = Dictionary::new();
    d.insert(Name::new("Type"), Object::name("Font"));
    d.insert(Name::new("Subtype"), Object::name("Type1"));
    d.insert(Name::new("BaseFont"), Object::name("Helvetica"));
    d.insert(Name::new("Encoding"), Object::name("WinAnsiEncoding"));
    Object::Dictionary(d)
}

/// Draw `text` at `(x, y)` on a page, in a standard face. Spec 9.2.
///
/// `(x, y)` is the text origin in page space — the left end of the baseline.
/// `background`, when given, fills a rectangle sized to the text behind it in
/// that RGB colour, for covering what was there before.
pub fn place_text(
    doc: &Document,
    page: &Page,
    x: f64,
    y: f64,
    size: f64,
    text: &str,
    background: Option<(f64, f64, f64)>,
    style: &NumberStyle,
) -> Result<PlaceEdit, PlaceError> {
    let mut codes = Vec::new();
    let mut missing = Vec::new();
    for c in text.chars() {
        match win_ansi_code(c) {
            Some(b) => codes.push(b),
            None => missing.push(c),
        }
    }
    if codes.is_empty() {
        return Err(PlaceError::Unencodable(text.to_string()));
    }

    let mut canvas = Canvas::new(*style);
    if let Some((r, g, b)) = background {
        // The width is estimated from the character count: the exact advance
        // needs the font's metrics, and a cover rectangle a little wide is the
        // safe error. Reported as an approximation only in that a caller who
        // needs an exact box passes their own `background` sizing by placing a
        // rectangle themselves.
        let width = codes.len() as f64 * size * 0.5;
        let pad = size * 0.25;
        canvas
            .save()
            .fill_rgb(r, g, b)
            .rect(x - pad, y - size * 0.3, width + 2.0 * pad, size * 1.3)
            .fill()
            .restore();
    }
    canvas.text_line(&Name::new(PLACED_FONT), size, x, y, &codes);
    let ops = canvas.finish().map_err(|e| PlaceError::Cos(e.to_string()))?;

    let stream_id = ObjId::new(doc.next_object_number(), 0);
    let mut stream = Stream::new(Dictionary::new(), Vec::new());
    stream.set_decoded(ops);

    let mut page_dict = page.dict.clone();
    let mut changes: Vec<(ObjId, Option<Object>)> = Vec::new();

    let contents_changed =
        append_to_array_entry(doc, &mut page_dict, "Contents", stream_id, &mut changes)
            .map_err(widen)?;
    let resources_changed = ensure_placed_font(doc, &mut page_dict, &mut changes)?;

    changes.push((stream_id, Some(Object::Stream(stream))));
    if contents_changed || resources_changed {
        changes.push((page.id, Some(Object::Dictionary(page_dict))));
    }

    let fidelity = if missing.is_empty() {
        Fidelity::Exact
    } else {
        Fidelity::Degraded(
            missing.iter().map(|c| Compromise::GlyphUnavailable { text: c.to_string() }).collect(),
        )
    };
    Ok(PlaceEdit { changes, fidelity, missing, created: None })
}

/// Create a text field over `rect`, named `name`, optionally pre-filled with
/// `value`. Spec 10.4. The field is a merged widget: one object that is both the
/// AcroForm field and the page annotation.
pub fn create_text_field(
    doc: &Document,
    page: &Page,
    rect: Rect,
    name: &str,
    value: &str,
    size: f64,
) -> Result<PlaceEdit, PlaceError> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(PlaceError::NoRect);
    }
    let widget_id = ObjId::new(doc.next_object_number(), 0);

    let mut widget = Dictionary::new();
    widget.insert(Name::new("Type"), Object::name("Annot"));
    widget.insert(Name::new("Subtype"), Object::name("Widget"));
    widget.insert(Name::new("FT"), Object::name("Tx"));
    widget.insert(Name::new("T"), Object::String(PdfString::new_literal(name.as_bytes())));
    widget.insert(
        Name::new("Rect"),
        Object::Array([rect.x0, rect.y0, rect.x1, rect.y1].map(Object::Real).to_vec()),
    );
    // Bit 3, Print: a field that does not print is invisible on paper, which is
    // the one place a form most needs to be filled.
    widget.insert(Name::new("F"), Object::Integer(4));
    widget.insert(
        Name::new("DA"),
        Object::String(PdfString::new_literal(format!("/Helv {size} Tf 0 g").as_bytes())),
    );
    widget.insert(Name::new("P"), Object::Reference(page.id));
    if !value.is_empty() {
        widget.insert(Name::new("V"), Object::String(PdfString::new_literal(value.as_bytes())));
    }

    let mut page_dict = page.dict.clone();
    let mut changes: Vec<(ObjId, Option<Object>)> = Vec::new();
    let annots_changed =
        append_to_array_entry(doc, &mut page_dict, "Annots", widget_id, &mut changes)
            .map_err(widen)?;

    changes.push((widget_id, Some(Object::Dictionary(widget))));
    if annots_changed {
        changes.push((page.id, Some(Object::Dictionary(page_dict))));
    }

    append_to_acroform(doc, widget_id, &mut changes)?;

    Ok(PlaceEdit { changes, fidelity: Fidelity::Exact, missing: Vec::new(), created: Some(widget_id) })
}

/// Append `new_ref` to a dictionary entry that is an array of references,
/// resolving the entry whether it is a direct array, a single reference, or an
/// indirect array. Returns whether `dict` itself was modified; an indirect array
/// is edited through `extra` and leaves `dict` pointing at it unchanged.
fn append_to_array_entry(
    doc: &Document,
    dict: &mut Dictionary,
    key: &str,
    new_ref: ObjId,
    extra: &mut Vec<(ObjId, Option<Object>)>,
) -> Result<bool, String> {
    match dict.get(key) {
        None => {
            dict.insert(Name::new(key), Object::Array(vec![Object::Reference(new_ref)]));
            Ok(true)
        }
        Some(Object::Array(a)) => {
            let mut a = a.clone();
            a.push(Object::Reference(new_ref));
            dict.insert(Name::new(key), Object::Array(a));
            Ok(true)
        }
        Some(Object::Reference(r)) => {
            let r = *r;
            let obj = doc.get(r).map_err(|e| e.to_string())?;
            match &*obj {
                // An indirect array of entries: append there and leave the page
                // pointing at it, so no page object is rewritten needlessly.
                Object::Array(a) => {
                    let mut a = a.clone();
                    a.push(Object::Reference(new_ref));
                    extra.push((r, Some(Object::Array(a))));
                    Ok(false)
                }
                // A single referenced stream (the common `/Contents`) or a lone
                // referenced annotation: the entry becomes a two-element array.
                _ => {
                    dict.insert(
                        Name::new(key),
                        Object::Array(vec![Object::Reference(r), Object::Reference(new_ref)]),
                    );
                    Ok(true)
                }
            }
        }
        Some(_) => {
            dict.insert(Name::new(key), Object::Array(vec![Object::Reference(new_ref)]));
            Ok(true)
        }
    }
}

/// Add the placed-text font to the page's resources. Returns whether the page
/// dict itself changed; an indirect `/Resources` is edited through `changes`.
fn ensure_placed_font(
    doc: &Document,
    page_dict: &mut Dictionary,
    changes: &mut Vec<(ObjId, Option<Object>)>,
) -> Result<bool, PlaceError> {
    match page_dict.get("Resources") {
        // Inherited: editing nothing here would leave the font undeclared, and
        // inventing a `/Resources` would shadow the inherited one. Refuse.
        None => Err(PlaceError::UnsupportedResources),
        Some(Object::Dictionary(res)) => {
            let mut res = res.clone();
            add_font(&mut res)?;
            page_dict.insert(Name::new("Resources"), Object::Dictionary(res));
            Ok(true)
        }
        Some(Object::Reference(r)) => {
            let r = *r;
            let obj = doc.get(r).map_err(|e| PlaceError::Cos(e.to_string()))?;
            let mut res =
                obj.as_dict().ok_or(PlaceError::UnsupportedResources)?.clone();
            add_font(&mut res)?;
            changes.push((r, Some(Object::Dictionary(res))));
            Ok(false)
        }
        Some(_) => Err(PlaceError::UnsupportedResources),
    }
}

/// Add `RasuraHelv` to a resources dictionary's `/Font`, refusing an indirect
/// `/Font` this module does not model.
fn add_font(res: &mut Dictionary) -> Result<(), PlaceError> {
    let mut font = match res.get("Font") {
        None => Dictionary::new(),
        Some(Object::Dictionary(f)) => f.clone(),
        Some(_) => return Err(PlaceError::UnsupportedResources),
    };
    font.insert(Name::new(PLACED_FONT), helvetica());
    res.insert(Name::new("Font"), Object::Dictionary(font));
    Ok(())
}

/// Link a new field into the AcroForm, creating the AcroForm if the document has
/// none. The field is added to `/Fields`, `NeedAppearances` is set so every
/// viewer renders the value, and `/DR` is seeded with Helvetica so the `/DA`
/// font resolves.
fn append_to_acroform(
    doc: &Document,
    field_id: ObjId,
    changes: &mut Vec<(ObjId, Option<Object>)>,
) -> Result<(), PlaceError> {
    let mut acro = forms::acroform(doc).unwrap_or_default();

    let mut fields = match acro.get("Fields") {
        Some(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    fields.push(Object::Reference(field_id));
    acro.insert(Name::new("Fields"), Object::Array(fields));
    acro.insert(Name::new("NeedAppearances"), Object::Bool(true));
    if acro.get("DA").is_none() {
        acro.insert(Name::new("DA"), Object::String(PdfString::new_literal(b"/Helv 0 Tf 0 g")));
    }
    ensure_dr_helvetica(&mut acro);

    // An existing indirect AcroForm is its own object; an inline or absent one
    // lives in the catalog, which is what gets rewritten.
    match forms::acroform_id(doc) {
        Some(id) => changes.push((id, Some(Object::Dictionary(acro)))),
        None => {
            let cat_id = doc.catalog_id().ok_or_else(|| PlaceError::Cos("no catalog".into()))?;
            let cat = doc.get(cat_id).map_err(|e| PlaceError::Cos(e.to_string()))?;
            let mut cat =
                cat.as_dict().ok_or_else(|| PlaceError::Cos("catalog is not a dict".into()))?.clone();
            cat.insert(Name::new("AcroForm"), Object::Dictionary(acro));
            changes.push((cat_id, Some(Object::Dictionary(cat))));
        }
    }
    Ok(())
}

/// Ensure the AcroForm's default resources carry a Helvetica the `/DA` can name.
fn ensure_dr_helvetica(acro: &mut Dictionary) {
    let mut dr = match acro.get("DR") {
        Some(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    let mut font = match dr.get("Font") {
        Some(Object::Dictionary(f)) => f.clone(),
        _ => Dictionary::new(),
    };
    if font.get("Helv").is_none() {
        font.insert(Name::new("Helv"), helvetica());
    }
    dr.insert(Name::new("Font"), Object::Dictionary(font));
    acro.insert(Name::new("DR"), Object::Dictionary(dr));
}

fn widen(e: String) -> PlaceError {
    PlaceError::Cos(e)
}
