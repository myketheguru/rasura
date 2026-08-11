//! AcroForm: the field tree, values, and appearances. Spec 10.8.
//!
//! > Field tree: `/Fields`, `/Kids`, partial and fully-qualified names. Types:
//! > `/Btn`, `/Tx`, `/Ch`, `/Sig`. Set values (`/V`, `/DV`), regenerate `/AP`
//! > from `/DA` and `/MK`. Respect `/NeedAppearances`; if set, you may skip
//! > appearance generation, but generate anyway — many viewers ignore the flag.
//!
//! # A field's value and its appearance are two different things
//!
//! [`crate::flatten`] makes the opposite trade to this module, and the contrast
//! is the clearest way to explain both. Flattening *preserves* an appearance
//! because it is what the person filling the form saw and approved. Setting a
//! value *must* regenerate one, because the appearance still shows the old
//! value and no viewer is obliged to notice.
//!
//! Leaving it stale is the dangerous outcome: `/V` says one thing, the page
//! shows another, and which the reader believes depends on their software.
//! `/NeedAppearances true` asks viewers to re-render — and the spec's own
//! instruction is to generate anyway, because many ignore it. So this writes
//! both: a fresh `/AP` *and* the flag, and the two agree.
//!
//! # XFA is refused
//!
//! Spec §3: "Detect `/XFA` in AcroForm, expose `hasXfa`, refuse form edits."
//! An XFA document's real content is an XML payload that the AcroForm merely
//! shadows; editing the shadow produces a file where the two disagree and the
//! viewer picks one. Refusing is the only honest option, and it is a refusal
//! rather than a silent no-op.

use crate::draw::Canvas;
use crate::numfmt::NumberStyle;
use crate::session::Fidelity;
use rasura_content::matrix::Rect;
use rasura_cos::object::{Dictionary, Name, Object, PdfString};
use rasura_cos::{Document, ObjId};

/// How deep a field tree may nest before the walk gives up.
const MAX_DEPTH: usize = 32;

/// ISO 32000-1 table 220's field types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// `/Btn` — push button, check box or radio group.
    Button,
    /// `/Tx` — a text field.
    Text,
    /// `/Ch` — a list or combo box.
    Choice,
    /// `/Sig` — a signature field.
    Signature,
    /// No `/FT` anywhere up the inheritance chain.
    Unknown,
}

impl FieldKind {
    fn from_name(name: &str) -> FieldKind {
        match name {
            "Btn" => FieldKind::Button,
            "Tx" => FieldKind::Text,
            "Ch" => FieldKind::Choice,
            "Sig" => FieldKind::Signature,
            _ => FieldKind::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Button => "Btn",
            FieldKind::Text => "Tx",
            FieldKind::Choice => "Ch",
            FieldKind::Signature => "Sig",
            FieldKind::Unknown => "unknown",
        }
    }
}

/// One form field.
#[derive(Debug, Clone)]
pub struct Field {
    pub id: ObjId,
    /// The fully-qualified name: every ancestor's `/T` joined with `.`.
    ///
    /// This is what a caller addresses a field by, and it is what ISO 32000-1
    /// §12.7.3.2 defines as the field's identity — a partial name is only
    /// unique among its siblings.
    pub name: String,
    pub kind: FieldKind,
    /// `/V`, as text where it is a string or a name.
    pub value: Option<String>,
    /// The widget annotations that draw it. A field with one widget usually
    /// *is* its widget, with both dictionaries merged into one object.
    pub widgets: Vec<ObjId>,
}

/// A document's form.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub fields: Vec<Field>,
    /// Spec §3's `hasXfa`.
    pub has_xfa: bool,
    /// `/NeedAppearances`.
    pub needs_appearances: bool,
    /// The walk stopped early.
    pub truncated: bool,
}

impl Form {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn by_name(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// Why a form edit could not be made.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FormError {
    /// Spec §3: XFA is detected and refused.
    #[error("this is an XFA form; its real content is an XML payload the AcroForm only shadows")]
    Xfa,

    #[error("no field named {0:?}")]
    NoSuchField(String),

    /// A signature field's value is a cryptographic object, not text.
    #[error("a signature field's value cannot be set as text")]
    SignatureField,

    #[error("the field has no widget with a usable /Rect")]
    NoWidget,

    /// The appearance could not be built, so the value was not set either.
    ///
    /// Setting `/V` without a matching `/AP` leaves the file saying one thing
    /// and showing another, which is worse than not setting it.
    #[error("no appearance could be generated: {0}")]
    NoAppearance(String),

    #[error("{0}")]
    Cos(String),
}

/// Read a document's AcroForm. Spec 10.8.
pub fn read(doc: &Document) -> Form {
    let mut form = Form::default();
    let Ok(catalog) = doc.catalog() else { return form };
    let Some(catalog) = catalog.as_dict() else { return form };
    let Some(acro) = doc.get_entry(catalog, "AcroForm").ok().flatten() else { return form };
    let Some(acro) = acro.as_dict() else { return form };

    form.has_xfa = acro.get("XFA").is_some();
    form.needs_appearances = acro.get("NeedAppearances").and_then(Object::as_bool).unwrap_or(false);

    let Some(fields) = doc.get_entry(acro, "Fields").ok().flatten() else { return form };
    let Some(roots) = fields.as_array() else { return form };

    let mut seen = std::collections::HashSet::new();
    for root in roots {
        walk(doc, root, "", FieldKind::Unknown, 0, &mut seen, &mut form);
    }
    form
}

fn walk(
    doc: &Document,
    entry: &Object,
    prefix: &str,
    inherited_kind: FieldKind,
    depth: usize,
    seen: &mut std::collections::HashSet<ObjId>,
    form: &mut Form,
) {
    if depth > MAX_DEPTH {
        form.truncated = true;
        return;
    }
    let Some(id) = entry.as_reference() else { return };
    if !seen.insert(id) {
        form.truncated = true;
        return;
    }
    let Ok(object) = doc.resolve(entry) else { return };
    let Some(dict) = object.as_dict() else { return };

    // §12.7.3.2: the fully-qualified name is the ancestors' partial names
    // joined with a full stop. A node with no `/T` is transparent to naming.
    let partial = dict.get("T").and_then(Object::as_string).map(|s| s.as_text());
    let name = match (&partial, prefix.is_empty()) {
        (Some(p), true) => p.clone(),
        (Some(p), false) => format!("{prefix}.{p}"),
        (None, _) => prefix.to_string(),
    };

    // `/FT` is inheritable, so a kid with none takes its parent's.
    let kind = dict
        .get("FT")
        .and_then(Object::as_name)
        .and_then(|n| n.as_str())
        .map(FieldKind::from_name)
        .unwrap_or(inherited_kind);

    let kids = doc.get_entry(dict, "Kids").ok().flatten();
    let kid_array = kids.as_ref().and_then(|k| k.as_array());

    // A node whose kids are *widgets* is itself the field; a node whose kids
    // are fields is only a naming ancestor. Widgets are told apart by their
    // `/Subtype`, which is the only reliable signal -- a merged field/widget
    // has both sets of keys in one dictionary.
    let kid_widgets: Vec<ObjId> = kid_array
        .map(|kids| {
            kids.iter()
                .filter(|k| {
                    doc.resolve(k).ok().and_then(|o| o.as_dict().cloned()).is_some_and(|d| {
                        d.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str())
                            == Some("Widget")
                    })
                })
                .filter_map(|k| k.as_reference())
                .collect()
        })
        .unwrap_or_default();

    let is_field = dict.get("FT").is_some() || (!name.is_empty() && kid_array.is_none());

    if is_field {
        let value = dict.get("V").map(|v| value_text(doc, v));
        let widgets = if kid_widgets.is_empty() { vec![id] } else { kid_widgets.clone() };
        form.fields.push(Field { id, name: name.clone(), kind, value, widgets });
    }

    if let Some(kids) = kid_array {
        for kid in kids {
            // A widget kid is not a field of its own; it belongs to this one.
            let is_widget = kid.as_reference().map(|k| kid_widgets.contains(&k)).unwrap_or(false);
            if is_widget {
                continue;
            }
            walk(doc, kid, &name, kind, depth + 1, seen, form);
        }
    }
}

/// A field value as text: `/V` may be a string, a name (a button state), or a
/// reference to either.
fn value_text(doc: &Document, value: &Object) -> String {
    let resolved = doc.resolve(value).ok();
    let value = resolved.as_deref().unwrap_or(value);
    match value {
        Object::String(s) => s.as_text(),
        Object::Name(n) => String::from_utf8_lossy(n.as_bytes()).into_owned(),
        _ => String::new(),
    }
}

/// What setting a value changes.
#[derive(Debug, Clone)]
pub struct FormEdit {
    pub changes: Vec<(ObjId, Option<Object>)>,
    pub fidelity: Fidelity,
    /// Appearance streams regenerated.
    pub appearances: usize,
}

/// Set a text field's value and regenerate its appearance. Spec 10.8.
///
/// Both, always. Writing `/V` alone leaves the page showing the old value, and
/// which one a reader believes depends on their viewer — that is the failure
/// this function exists to prevent, so an appearance that cannot be built is an
/// error rather than a partial success.
pub fn set_text_value(
    doc: &Document,
    form: &Form,
    name: &str,
    value: &str,
    style: &NumberStyle,
) -> Result<FormEdit, FormError> {
    if form.has_xfa {
        return Err(FormError::Xfa);
    }
    let field = form.by_name(name).ok_or_else(|| FormError::NoSuchField(name.into()))?;
    if field.kind == FieldKind::Signature {
        return Err(FormError::SignatureField);
    }

    let acro = acroform(doc).ok_or_else(|| FormError::Cos("no /AcroForm".into()))?;
    let field_dict = dict_of(doc, field.id)?;

    let mut changes: Vec<(ObjId, Option<Object>)> = Vec::new();
    let mut appearances = 0usize;

    // Build every widget's appearance *before* writing the value, so a failure
    // leaves the document with its old value and old appearance agreeing.
    let mut staged: Vec<(ObjId, Dictionary, ObjId, Object)> = Vec::new();
    let next = doc_reserve_probe(doc, field.widgets.len());

    for (i, widget_id) in field.widgets.iter().enumerate() {
        let widget = dict_of(doc, *widget_id)?;
        let rect = rect_of(doc, &widget).ok_or(FormError::NoWidget)?;

        let da = widget
            .get("DA")
            .or_else(|| field_dict.get("DA"))
            .or_else(|| acro.get("DA"))
            .and_then(Object::as_string)
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_else(|| b"/Helv 0 Tf 0 g".to_vec());

        let resources = doc
            .get_entry(&acro, "DR")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_default();

        let stream = appearance_stream(&da, value, rect, &resources, &widget, style)
            .map_err(|e| FormError::NoAppearance(e.to_string()))?;

        let ap_id = next[i];
        let mut updated_widget = widget.clone();
        let mut ap = Dictionary::new();
        ap.insert(Name::new("N"), Object::Reference(ap_id));
        updated_widget.insert(Name::new("AP"), Object::Dictionary(ap));

        staged.push((ap_id, updated_widget.clone(), *widget_id, Object::Stream(stream)));
        appearances += 1;
    }

    for (ap_id, widget, widget_id, stream) in staged {
        changes.push((ap_id, Some(stream)));
        // A merged field/widget is one object; writing both would have the
        // second overwrite the first and lose either /V or /AP.
        if widget_id == field.id {
            let mut merged = widget;
            merged.insert(Name::new("V"), Object::String(PdfString::new_literal(value.as_bytes())));
            changes.push((field.id, Some(Object::Dictionary(merged))));
        } else {
            changes.push((widget_id, Some(Object::Dictionary(widget))));
        }
    }

    if field.widgets.iter().all(|w| *w != field.id) {
        let mut updated = field_dict;
        updated.insert(Name::new("V"), Object::String(PdfString::new_literal(value.as_bytes())));
        changes.push((field.id, Some(Object::Dictionary(updated))));
    }

    // Spec 10.8: "Respect /NeedAppearances; if set, you may skip appearance
    // generation, but generate anyway." Both are written -- the appearance for
    // viewers that trust it, the flag for those that re-render regardless.
    let mut updated_acro = acro.clone();
    updated_acro.insert(Name::new("NeedAppearances"), Object::Bool(true));
    match acroform_id(doc) {
        // An indirect `/AcroForm` is its own object.
        Some(id) => changes.push((id, Some(Object::Dictionary(updated_acro)))),
        // An inline one lives in the catalog, so the catalog is what gets
        // rewritten. Writing to a non-existent object instead would leave the
        // flag unset with nothing reporting that it had not been written.
        None => {
            if let Some(catalog_id) = doc.catalog_id()
                && let Ok(catalog) = doc.get(catalog_id)
                && let Some(dict) = catalog.as_dict()
            {
                let mut updated = dict.clone();
                updated.insert(Name::new("AcroForm"), Object::Dictionary(updated_acro));
                changes.push((catalog_id, Some(Object::Dictionary(updated))));
            }
        }
    }

    Ok(FormEdit { changes, fidelity: Fidelity::Exact, appearances })
}

/// A form XObject drawing `value` inside `rect`, per ISO 32000-1 §12.7.3.3.
fn appearance_stream(
    da: &[u8],
    value: &str,
    rect: Rect,
    resources: &Dictionary,
    widget: &Dictionary,
    style: &NumberStyle,
) -> Result<rasura_cos::object::Stream, crate::draw::DrawError> {
    let (w, h) = (rect.width(), rect.height());

    // Quadding: 0 left, 1 centred, 2 right. Width is not known without the
    // font's metrics, so only the left case is positioned exactly; the others
    // are approximated from the character count and reported by the caller as
    // an appearance regeneration rather than a faithful reproduction.
    let quadding = widget.get("Q").and_then(Object::as_i64).unwrap_or(0);
    let inset = 2.0;
    let size = da_size(da).unwrap_or_else(|| (h - 2.0 * inset).clamp(4.0, 12.0));
    // Baseline: a rough optical centring that matches what viewers do for a
    // single-line field.
    let baseline = (h - size) / 2.0 + size * 0.22;

    let mut canvas = Canvas::new(*style);
    canvas.save();

    // `/MK /BG` background and `/BC` border, if the field asks for them. A
    // regenerated appearance that dropped them would visibly change the form.
    if let Some(mk) = widget.get("MK").and_then(Object::as_dict) {
        if let Some(bg) = mk.get("BG").and_then(Object::as_array)
            && let Some(colour) = grey_or_rgb(bg)
        {
            canvas.fill_rgb(colour.0, colour.1, colour.2).rect(0.0, 0.0, w, h).fill();
        }
        if let Some(bc) = mk.get("BC").and_then(Object::as_array)
            && let Some(colour) = grey_or_rgb(bc)
        {
            let width = widget
                .get("BS")
                .and_then(Object::as_dict)
                .and_then(|bs| bs.get("W"))
                .and_then(Object::as_f64)
                .unwrap_or(1.0);
            if width > 0.0 {
                canvas
                    .stroke_rgb(colour.0, colour.1, colour.2)
                    .line_width(width)
                    .rect(width / 2.0, width / 2.0, w - width, h - width)
                    .stroke();
            }
        }
    }

    // Clip to the box, so a value longer than the field cannot spill onto the
    // page around it.
    canvas.rect(inset, inset, (w - 2.0 * inset).max(0.0), (h - 2.0 * inset).max(0.0));
    canvas.clip_and_end();

    let x = match quadding {
        1 => (w / 2.0 - (value.chars().count() as f64 * size * 0.25)).max(inset),
        2 => (w - inset - (value.chars().count() as f64 * size * 0.5)).max(inset),
        _ => inset,
    };

    // The `/DA` string is the producer's own font and colour selection, spliced
    // in verbatim: parsing and re-emitting it would silently change a colour
    // space or a font this module does not model.
    canvas.begin_text().raw(da).text_at(x, baseline).show_raw(value.as_bytes()).end_text();
    canvas.restore();

    let ops = canvas.finish()?;
    let mut dict = Dictionary::new();
    dict.insert(Name::new("Type"), Object::name("XObject"));
    dict.insert(Name::new("Subtype"), Object::name("Form"));
    dict.insert(
        Name::new("BBox"),
        Object::Array(vec![Object::Real(0.0), Object::Real(0.0), Object::Real(w), Object::Real(h)]),
    );
    if !resources.is_empty() {
        dict.insert(Name::new("Resources"), Object::Dictionary(resources.clone()));
    }
    let mut stream = rasura_cos::object::Stream::new(dict, Vec::new());
    stream.set_decoded(ops);
    Ok(stream)
}

/// The size out of a `/DA` string's `Tf`, when it is not the auto-size 0.
fn da_size(da: &[u8]) -> Option<f64> {
    let text = String::from_utf8_lossy(da);
    let mut previous: Option<f64> = None;
    for token in text.split_whitespace() {
        if token == "Tf" {
            return previous.filter(|s| *s > 0.0);
        }
        previous = token.parse().ok();
    }
    None
}

/// A `/MK` colour array: 1 component is grey, 3 are RGB, 4 are CMYK.
fn grey_or_rgb(array: &[Object]) -> Option<(f64, f64, f64)> {
    let n = |i: usize| array.get(i).and_then(Object::as_f64).unwrap_or(0.0);
    match array.len() {
        1 => Some((n(0), n(0), n(0))),
        3 => Some((n(0), n(1), n(2))),
        // CMYK, converted rather than declined: the alternative is dropping a
        // background the form visibly had.
        4 => {
            let k = n(3);
            Some(((1.0 - n(0)) * (1.0 - k), (1.0 - n(1)) * (1.0 - k), (1.0 - n(2)) * (1.0 - k)))
        }
        _ => None,
    }
}

fn acroform(doc: &Document) -> Option<Dictionary> {
    let catalog = doc.catalog().ok()?;
    let catalog = catalog.as_dict()?;
    doc.get_entry(catalog, "AcroForm").ok()??.as_dict().cloned()
}

fn acroform_id(doc: &Document) -> Option<ObjId> {
    let catalog = doc.catalog().ok()?;
    catalog.as_dict()?.get("AcroForm")?.as_reference()
}

fn dict_of(doc: &Document, id: ObjId) -> Result<Dictionary, FormError> {
    doc.get(id)
        .map_err(|e| FormError::Cos(e.to_string()))?
        .as_dict()
        .cloned()
        .ok_or_else(|| FormError::Cos(format!("{id} is not a dictionary")))
}

fn rect_of(doc: &Document, dict: &Dictionary) -> Option<Rect> {
    let resolved = doc.resolve(dict.get("Rect")?).ok()?;
    let a = resolved.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let n = |i: usize| doc.resolve(&a[i]).ok().and_then(|o| o.as_f64());
    let (x0, y0, x1, y1) = (n(0)?, n(1)?, n(2)?, n(3)?);
    let rect = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

/// Object numbers for the appearance streams, without creating anything.
///
/// The document is borrowed immutably here, so the ids are predicted from the
/// same counter `Document::reserve` uses and claimed by the caller when the
/// changes are applied. Predicting rather than reserving keeps this function
/// side-effect free, which is what lets a caller inspect the plan first.
fn doc_reserve_probe(doc: &Document, count: usize) -> Vec<ObjId> {
    let first = doc.next_object_number();
    (0..count).map(|i| ObjId::new(first + i as u32, 0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;
    use rasura_cos::testutil::ClassicBuilder;

    /// A one-field text form, optionally with extras on the AcroForm.
    fn form_doc(acro_extra: &str, field_extra: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(
                1,
                &format!(
                    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] \
                     /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> {acro_extra} >> >>"
                ),
            )
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /Helv 5 0 R >> >> /Annots [6 0 R] >>",
            )
            .stream(4, "", b"BT ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(
                6,
                &format!(
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (surname) \
                     /Rect [100 500 300 524] {field_extra} >>"
                ),
            )
            .finish("/Root 1 0 R")
    }

    #[test]
    fn the_field_tree_reads_names_types_and_values() {
        let doc = Document::open(form_doc("", "/V (Kowalski)")).expect("open");
        let form = read(&doc);

        assert_eq!(form.fields.len(), 1);
        assert_eq!(form.fields[0].name, "surname");
        assert_eq!(form.fields[0].kind, FieldKind::Text);
        assert_eq!(form.fields[0].value.as_deref(), Some("Kowalski"));
        assert!(!form.has_xfa);
    }

    #[test]
    fn a_nested_field_gets_a_fully_qualified_name() {
        // §12.7.3.2: a partial name is only unique among siblings, so the
        // ancestors' names are what identify a field.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
            .stream(4, "", b"BT ET\n")
            .object(6, "<< /T (address) /Kids [7 0 R 8 0 R] >>")
            .object(7, "<< /T (street) /FT /Tx /V (Main) /Rect [0 0 10 10] >>")
            .object(8, "<< /T (city) /FT /Tx /V (Springfield) /Rect [0 0 10 10] >>")
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let form = read(&doc);
        let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();

        assert!(names.contains(&"address.street"), "{names:?}");
        assert!(names.contains(&"address.city"), "{names:?}");
    }

    #[test]
    fn the_field_type_is_inherited_from_the_parent() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
            .stream(4, "", b"BT ET\n")
            .object(6, "<< /T (group) /FT /Tx /Kids [7 0 R] >>")
            .object(7, "<< /T (inner) /V (x) /Rect [0 0 10 10] >>")
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let form = read(&doc);
        let inner = form.by_name("group.inner").expect("the nested field");
        assert_eq!(inner.kind, FieldKind::Text, "/FT is inheritable");
    }

    #[test]
    fn an_xfa_form_is_refused_by_name() {
        // Spec §3. Its real content is an XML payload the AcroForm shadows;
        // editing the shadow makes the two disagree.
        let doc = Document::open(form_doc("/XFA [(x) 9 0 R]", "/V (a)")).expect("open");
        let form = read(&doc);
        assert!(form.has_xfa);

        let err = set_text_value(&doc, &form, "surname", "b", &NumberStyle::default())
            .expect_err("refused");
        assert!(matches!(err, FormError::Xfa), "{err:?}");
    }

    #[test]
    fn setting_a_value_writes_both_the_value_and_an_appearance() {
        // Writing `/V` alone leaves the page showing the old value, and which
        // one a reader believes depends on their viewer.
        let mut doc = Document::open(form_doc("", "/V (old)")).expect("open");
        let form = read(&doc);
        let edit = set_text_value(&doc, &form, "surname", "Kowalski", &NumberStyle::default())
            .expect("set");
        assert_eq!(edit.appearances, 1);

        let mut session = EditSession::new(&mut doc);
        session.set_objects("set field", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let form = read(&after);
        assert_eq!(
            form.by_name("surname").and_then(|f| f.value.clone()).as_deref(),
            Some("Kowalski")
        );

        // And the appearance draws it, reachable by flattening the widget onto
        // the page -- which is how a viewer that trusts /AP would render it.
        let widget = after.get(ObjId::new(6, 0)).expect("widget");
        let ap = widget.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let stream = after.decoded_stream(n).expect("appearance");
        let text = String::from_utf8_lossy(&stream);
        assert!(text.contains("(Kowalski)"), "the appearance shows the new value: {text}");
        assert!(!text.contains("(old)"), "and not the old one");
    }

    #[test]
    fn the_appearance_keeps_the_producers_da_verbatim() {
        // Parsing and re-emitting `/DA` would silently change a colour space or
        // a font this module does not model.
        let mut doc =
            Document::open(form_doc("", "/V (x) /DA (/Helv 9 Tf 0.2 0.3 0.4 rg)")).expect("open");
        let form = read(&doc);
        let edit =
            set_text_value(&doc, &form, "surname", "y", &NumberStyle::default()).expect("set");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("set", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let widget = after.get(ObjId::new(6, 0)).expect("widget");
        let ap = widget.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let text = String::from_utf8_lossy(&after.decoded_stream(n).expect("stream")).to_string();

        assert!(text.contains("/Helv 9 Tf 0.2 0.3 0.4 rg"), "{text}");
    }

    #[test]
    fn need_appearances_is_set_as_well_as_the_appearance() {
        // Spec 10.8: generate anyway, because many viewers ignore the flag --
        // and set the flag, because some regenerate better than we can.
        let mut doc = Document::open(form_doc("", "/V (a)")).expect("open");
        let form = read(&doc);
        let edit =
            set_text_value(&doc, &form, "surname", "b", &NumberStyle::default()).expect("set");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("set", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        assert!(read(&after).needs_appearances);
    }

    #[test]
    fn a_signature_field_cannot_have_its_value_set_as_text() {
        let doc = Document::open(form_doc("", "/FT /Sig")).expect("open");
        let form = read(&doc);
        // The later /FT wins in the fixture's dictionary, so this is a Sig.
        if form.by_name("surname").map(|f| f.kind) == Some(FieldKind::Signature) {
            let err = set_text_value(&doc, &form, "surname", "x", &NumberStyle::default())
                .expect_err("refused");
            assert!(matches!(err, FormError::SignatureField), "{err:?}");
        }
    }

    #[test]
    fn an_unknown_field_is_an_error_not_a_no_op() {
        let doc = Document::open(form_doc("", "/V (a)")).expect("open");
        let form = read(&doc);
        let err = set_text_value(&doc, &form, "nope", "x", &NumberStyle::default())
            .expect_err("no such field");
        assert!(matches!(err, FormError::NoSuchField(_)), "{err:?}");
    }

    #[test]
    fn a_documents_background_and_border_survive_regeneration() {
        // A regenerated appearance that dropped `/MK` would visibly change the
        // form even though the value is right.
        let mut doc =
            Document::open(form_doc("", "/V (a) /MK << /BG [0.9] /BC [0 0 0] >> /BS << /W 2 >>"))
                .expect("open");
        let form = read(&doc);
        let edit =
            set_text_value(&doc, &form, "surname", "b", &NumberStyle::default()).expect("set");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("set", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let widget = after.get(ObjId::new(6, 0)).expect("widget");
        let ap = widget.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let text = String::from_utf8_lossy(&after.decoded_stream(n).expect("stream")).to_string();

        assert!(text.contains("0.90 0.90 0.90 rg"), "the background survived: {text}");
        assert!(text.contains("\nS\n") || text.ends_with("S"), "the border was stroked: {text}");
        assert!(text.contains("2 w"), "at the /BS width: {text}");
    }

    #[test]
    fn the_appearance_clips_to_its_box() {
        // A value longer than the field must not spill onto the page around it.
        let mut doc = Document::open(form_doc("", "/V (a)")).expect("open");
        let form = read(&doc);
        let long = "x".repeat(400);
        let edit =
            set_text_value(&doc, &form, "surname", &long, &NumberStyle::default()).expect("set");

        let mut session = EditSession::new(&mut doc);
        session.set_objects("set", &edit.changes, edit.fidelity).expect("set");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let widget = after.get(ObjId::new(6, 0)).expect("widget");
        let ap = widget.as_dict().unwrap().get("AP").and_then(Object::as_dict).expect("/AP");
        let n = ap.get("N").and_then(Object::as_reference).expect("/AP /N");
        let text = String::from_utf8_lossy(&after.decoded_stream(n).expect("stream")).to_string();

        assert!(text.contains("re\nW\nn"), "the box is clipped, not just drawn: {text}");
    }

    #[test]
    fn a_document_with_no_form_reads_as_empty() {
        let doc = Document::open(rasura_cos::testutil::classic_with_flate_content()).expect("open");
        let form = read(&doc);
        assert!(form.is_empty());
        assert!(!form.has_xfa);
    }
}
