//! Rust values to JS values, and errors that keep their code.
//!
//! Plain objects rather than `#[wasm_bindgen]` structs throughout. A bound
//! struct gives JS a class whose every field is a getter, and every getter is a
//! call across the boundary — so reading a paragraph's seven fields costs seven
//! crossings, and a page of forty paragraphs costs two hundred and eighty.
//! Building the object once in Rust costs one.
//!
//! It also keeps the TypeScript declarations (§12.4) hand-written, which the
//! specification asks for: generated `.d.ts` from `wasm-bindgen` describes the
//! ABI rather than the API, and `any` appears in exactly the places a caller
//! most needs a type.

use js_sys::{Array, Error as JsError, Object, Reflect, Uint8Array};
use rasura::{Code, Error};
use wasm_bindgen::{JsCast, JsValue};

pub fn obj() -> Object {
    Object::new()
}

pub fn set(target: &Object, key: &str, value: &JsValue) {
    // The failure case is a frozen or proxied object, which cannot happen to
    // one this function just created. Ignored rather than unwrapped so a
    // conversion cannot panic across the boundary.
    let _ = Reflect::set(target, &JsValue::from_str(key), value);
}

pub fn set_str(target: &Object, key: &str, value: &str) {
    set(target, key, &JsValue::from_str(value));
}

pub fn set_num(target: &Object, key: &str, value: f64) {
    set(target, key, &JsValue::from_f64(value));
}

pub fn set_bool(target: &Object, key: &str, value: bool) {
    set(target, key, &JsValue::from_bool(value));
}

/// A JS `Error` carrying §11.5's code as a property.
///
/// A property rather than a subclass. `class PdfError extends Error` cannot be
/// constructed from Rust without shipping a JS shim, and the shim would have to
/// load before the wasm module — a load-order constraint for something a
/// property already provides. The TypeScript wrapper narrows this to a real
/// `PdfError` on the way out, where classes are cheap.
pub fn err(code: &str, message: &str) -> JsValue {
    let e = JsError::new(message);
    let value: JsValue = e.into();
    let _ = Reflect::set(&value, &JsValue::from_str("code"), &JsValue::from_str(code));
    value
}

pub fn from_error(e: Error) -> JsValue {
    let value = err(e.code().as_str(), e.message());
    if !e.detail().is_empty() {
        let _ = Reflect::set(&value, &JsValue::from_str("detail"), &JsValue::from_str(e.detail()));
    }
    value
}

/// The `code` property of a thrown value, for this crate's own tests.
///
/// Test-only: JS reads the property directly, so shipping this would be a
/// function nothing calls taking up space in a size-budgeted artefact.
#[cfg(test)]
pub fn code_of(value: &JsValue) -> Option<String> {
    Reflect::get(value, &JsValue::from_str("code")).ok()?.as_string()
}

pub fn strings(items: impl Iterator<Item = String>) -> JsValue {
    let array = Array::new();
    for s in items {
        array.push(&JsValue::from_str(&s));
    }
    array.into()
}

/// Copy bytes into a `Uint8Array`.
///
/// A copy, not a view. `Uint8Array::view` would hand JS a window onto linear
/// memory, which is faster and unsound the moment the allocator grows the heap:
/// the buffer detaches and the caller is left holding an empty array. Saved
/// bytes outlive the call that produced them, so they have to be copied.
pub fn bytes(data: &[u8]) -> JsValue {
    Uint8Array::from(data).into()
}

pub fn permissions(p: &rasura::Permissions) -> JsValue {
    let out = obj();
    set_bool(&out, "print", p.print);
    set_bool(&out, "modify", p.modify);
    set_bool(&out, "copy", p.copy);
    set_bool(&out, "annotate", p.annotate);
    set_bool(&out, "fillForms", p.fill_forms);
    set_bool(&out, "extractForAccessibility", p.extract_for_accessibility);
    set_bool(&out, "assemble", p.assemble);
    set_bool(&out, "printHighQuality", p.print_high_quality);
    out.into()
}

pub fn rect(r: &rasura::Rect) -> JsValue {
    let out = obj();
    set_num(&out, "x0", r.x0);
    set_num(&out, "y0", r.y0);
    set_num(&out, "x1", r.x1);
    set_num(&out, "y1", r.y1);
    out.into()
}

pub fn page(page: &rasura::Page) -> JsValue {
    let paragraphs = Array::new();
    for (i, p) in page.paragraphs().iter().enumerate() {
        let entry = obj();
        // The index is the identifier JS uses: `ParagraphId` is opaque, and a
        // caller has to name a paragraph somehow. Stable for the life of the
        // page object, which is the same guarantee the Rust id gives.
        set_num(&entry, "id", i as f64);
        set_str(&entry, "text", &p.text);
        set_str(&entry, "textConfidence", confidence(p.confidence));
        set(&entry, "box", &rect(&p.box_));
        set_str(&entry, "alignment", alignment(p.alignment));
        set_num(&entry, "leading", p.leading);
        set_num(&entry, "lineCount", p.line_count as f64);
        paragraphs.push(&entry.into());
    }

    let blocks = Array::new();
    for b in page.blocks() {
        let entry = obj();
        set_str(&entry, "kind", b.kind());
        set(&entry, "box", &rect(&b.box_()));
        blocks.push(&entry.into());
    }

    // Images and tables carry their own index as `id` for the same reason
    // paragraphs do: the Rust ids are opaque, and every operation that moves an
    // image or fills a cell has to name one.
    let images = Array::new();
    for (i, image) in page.images().iter().enumerate() {
        let entry = obj();
        set_num(&entry, "id", i as f64);
        set(&entry, "box", &rect(&image.box_));
        match image.pixels {
            Some((w, h)) => {
                let size = obj();
                set_num(&size, "width", w as f64);
                set_num(&size, "height", h as f64);
                set(&entry, "pixels", &size.into());
            }
            None => set(&entry, "pixels", &JsValue::NULL),
        }
        // Carried across so a UI can grey out a drag it would otherwise offer
        // and then have refused: an image inside a form XObject is shared with
        // every page that invokes the form.
        set_bool(&entry, "editable", image.editable);
        images.push(&entry.into());
    }

    let tables = Array::new();
    for (i, table) in page.tables().iter().enumerate() {
        let entry = obj();
        set_num(&entry, "id", i as f64);
        set(&entry, "box", &rect(&table.bbox));
        set_num(&entry, "rows", table.rows as f64);
        set_num(&entry, "columns", table.cols as f64);
        tables.push(&entry.into());
    }

    let out = obj();
    set_num(&out, "index", page.index() as f64);
    set(&out, "mediaBox", &rect(&page.media_box()));
    set_num(&out, "rotate", page.rotate() as f64);
    set_bool(&out, "scanned", page.is_scanned());
    set(&out, "paragraphs", &paragraphs.into());
    set(&out, "blocks", &blocks.into());
    set(&out, "images", &images.into());
    set(&out, "tables", &tables.into());
    out.into()
}

pub fn fonts(list: &[rasura::FontInfo]) -> JsValue {
    let array = Array::new();
    for f in list {
        let entry = obj();
        set_str(&entry, "pdfFont", &f.name);
        set_str(&entry, "family", &f.family);
        set_bool(&entry, "embedded", f.embedded);
        set_bool(&entry, "subset", f.subset);
        set_str(&entry, "coverage", f.latin_coverage.as_str());
        set_num(&entry, "writableLatin", f.writable as f64);
        set_bool(&entry, "needsSupplying", f.needs_supplying());
        array.push(&entry.into());
    }
    array.into()
}

pub fn metadata(m: &rasura::Metadata) -> JsValue {
    let field = |v: &Option<String>| match v {
        Some(s) => JsValue::from_str(s),
        None => JsValue::NULL,
    };
    let surface = |f: &rasura::metadata::Fields| {
        let out = obj();
        set(&out, "title", &field(&f.title));
        set(&out, "author", &field(&f.author));
        set(&out, "subject", &field(&f.subject));
        set(&out, "creator", &field(&f.creator));
        set(&out, "producer", &field(&f.producer));
        out
    };

    let clashes = Array::new();
    for d in m.disagreements() {
        let entry = obj();
        set_str(&entry, "field", d.field);
        set_str(&entry, "info", &d.info);
        set_str(&entry, "xmp", &d.xmp);
        clashes.push(&entry.into());
    }

    let out = obj();
    set(&out, "info", &surface(&m.info).into());
    set(&out, "xmp", &surface(&m.xmp_fields).into());
    set_bool(&out, "hasXmp", m.has_xmp());
    set(&out, "disagreements", &clashes.into());
    out.into()
}

pub fn outcome(o: &rasura::Outcome) -> JsValue {
    let out = obj();
    set_str(&out, "fidelity", o.fidelity.as_str());
    set(&out, "missingGlyphs", &strings(o.missing_glyphs.iter().cloned()));
    match o.reflowed_lines {
        Some(n) => set_num(&out, "reflowedLines", n as f64),
        None => set(&out, "reflowedLines", &JsValue::NULL),
    }
    set(&out, "warnings", &strings(o.warnings.iter().cloned()));
    out.into()
}

// ---------------------------------------------------------------------------
// Reading plain objects back out of JS.
//
// The options bags — an annotation to add, a protection policy — have six or
// seven fields each, most of them optional. Passing them as positional
// `wasm_bindgen` parameters would mean seven `Option<T>` arguments in a fixed
// order, which is unreadable at the call site and silently wrong the first time
// somebody transposes two of them. So they cross as objects and are read here.

fn get(source: &JsValue, key: &str) -> JsValue {
    Reflect::get(source, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn opt_str(source: &JsValue, key: &str) -> Option<String> {
    get(source, key).as_string()
}

fn opt_num(source: &JsValue, key: &str) -> Option<f64> {
    get(source, key).as_f64()
}

fn num_or(source: &JsValue, key: &str, fallback: f64) -> f64 {
    opt_num(source, key).unwrap_or(fallback)
}

fn bool_or(source: &JsValue, key: &str, fallback: bool) -> bool {
    get(source, key).as_bool().unwrap_or(fallback)
}

/// An RGB triple as `[r, g, b]`, each 0..1.
fn opt_rgb(source: &JsValue, key: &str) -> Result<Option<(f64, f64, f64)>, JsValue> {
    let value = get(source, key);
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let array: Array = value
        .dyn_into()
        .map_err(|_| err(Code::Internal.as_str(), &format!("{key} must be [r, g, b]")))?;
    if array.length() != 3 {
        return Err(err(Code::Internal.as_str(), &format!("{key} must have three components")));
    }
    let at = |i: u32| array.get(i).as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    Ok(Some((at(0), at(1), at(2))))
}

/// A flat `[x0, y0, x1, y1, ...]` list of points.
fn points(source: &JsValue, key: &str) -> Vec<(f64, f64)> {
    let Ok(array) = get(source, key).dyn_into::<Array>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < array.length() {
        out.push((array.get(i).as_f64().unwrap_or(0.0), array.get(i + 1).as_f64().unwrap_or(0.0)));
        i += 2;
    }
    out
}

pub fn to_rect(source: &JsValue) -> Result<rasura::Rect, JsValue> {
    if source.is_undefined() || source.is_null() {
        return Err(err(Code::Internal.as_str(), "a rect is required: { x0, y0, x1, y1 }"));
    }
    Ok(rasura::Rect {
        x0: num_or(source, "x0", 0.0),
        y0: num_or(source, "y0", 0.0),
        x1: num_or(source, "x1", 0.0),
        y1: num_or(source, "y1", 0.0),
    })
}

/// Build a [`rasura_edit::NewAnnotation`] from a plain object.
pub fn to_new_annotation(source: &JsValue) -> Result<rasura::annotations::NewAnnotation, JsValue> {
    use rasura::annotations::{Kind, NewAnnotation};

    let name = opt_str(source, "kind")
        .ok_or_else(|| err(Code::Internal.as_str(), "an annotation needs a kind"))?;
    let kind = Kind::from_name(&name).ok_or_else(|| {
        err(Code::Internal.as_str(), &format!("unknown annotation kind {name:?}"))
    })?;

    let mut new = NewAnnotation::new(kind, to_rect(&get(source, "rect"))?);
    if let Some(rgb) = opt_rgb(source, "colour")?.or(opt_rgb(source, "color")?) {
        new.colour = rgb;
    }
    new.interior = opt_rgb(source, "interior")?;
    if let Some(width) = opt_num(source, "borderWidth") {
        new.border_width = width;
    }
    new.contents = opt_str(source, "contents");
    new.points = points(source, "points");
    Ok(new)
}

/// Build a [`rasura::protection::Policy`] from a plain object.
pub fn to_policy(source: &JsValue) -> Result<rasura::protection::Policy, JsValue> {
    use rasura::protection::Strength as S;

    let strength = match opt_str(source, "strength").as_deref() {
        None | Some("aes-256") => S::Aes256,
        Some("aes-128") => S::Aes128,
        Some(other) => {
            return Err(err(
                Code::Internal.as_str(),
                &format!("unknown strength {other:?}; expected aes-256 or aes-128"),
            ));
        }
    };

    // Permissions default to *granted*, matching `Permissions::all()`. A field
    // a caller did not mention must not silently forbid printing.
    let bits = get(source, "permissions");
    let mut permissions = rasura::Permissions::all();
    if !bits.is_undefined() && !bits.is_null() {
        permissions.print = bool_or(&bits, "print", true);
        permissions.modify = bool_or(&bits, "modify", true);
        permissions.copy = bool_or(&bits, "copy", true);
        permissions.annotate = bool_or(&bits, "annotate", true);
        permissions.fill_forms = bool_or(&bits, "fillForms", true);
        permissions.extract_for_accessibility = bool_or(&bits, "extractForAccessibility", true);
        permissions.assemble = bool_or(&bits, "assemble", true);
        permissions.print_high_quality = bool_or(&bits, "printHighQuality", true);
    }

    Ok(rasura::protection::Policy {
        user_password: opt_str(source, "userPassword").unwrap_or_default(),
        owner_password: opt_str(source, "ownerPassword").unwrap_or_default(),
        permissions,
        encrypt_metadata: bool_or(source, "encryptMetadata", true),
        strength,
    })
}

// ---------------------------------------------------------------------------
// The other direction.

pub fn obj_id(id: rasura::ObjId) -> JsValue {
    let out = obj();
    set_num(&out, "number", id.number as f64);
    set_num(&out, "generation", id.generation as f64);
    out.into()
}

/// Read an object id back. Both fields are required: defaulting the generation
/// to 0 would quietly address a different object in a file that has reused a
/// slot, and those are exactly the files where deleting the wrong one matters.
pub fn to_obj_id(source: &JsValue) -> Result<rasura::ObjId, JsValue> {
    let number = opt_num(source, "number").ok_or_else(|| {
        err(Code::Internal.as_str(), "an annotation id needs { number, generation }")
    })?;
    let generation = opt_num(source, "generation").ok_or_else(|| {
        err(Code::Internal.as_str(), "an annotation id needs { number, generation }")
    })?;
    Ok(rasura::ObjId::new(number as u32, generation as u16))
}

pub fn annotations(list: &[rasura::annotations::Annotation]) -> JsValue {
    let array = Array::new();
    for a in list {
        let entry = obj();
        set(&entry, "id", &obj_id(a.id));
        match a.kind {
            Some(k) => set_str(&entry, "kind", k.as_str()),
            None => set(&entry, "kind", &JsValue::NULL),
        }
        match &a.rect {
            Some(r) => set(&entry, "rect", &rect(r)),
            None => set(&entry, "rect", &JsValue::NULL),
        }
        match &a.contents {
            Some(c) => set_str(&entry, "contents", c),
            None => set(&entry, "contents", &JsValue::NULL),
        }
        set_bool(&entry, "hasAppearance", a.has_appearance);
        array.push(&entry.into());
    }
    array.into()
}

pub fn fields(list: &[rasura::forms::Field]) -> JsValue {
    use rasura::forms::FieldKind as K;
    let array = Array::new();
    for f in list {
        let entry = obj();
        set(&entry, "id", &obj_id(f.id));
        set_str(&entry, "name", &f.name);
        set_str(
            &entry,
            "kind",
            match f.kind {
                K::Button => "button",
                K::Text => "text",
                K::Choice => "choice",
                K::Signature => "signature",
                K::Unknown => "unknown",
            },
        );
        match &f.value {
            Some(v) => set_str(&entry, "value", v),
            None => set(&entry, "value", &JsValue::NULL),
        }
        set_num(&entry, "widgets", f.widgets.len() as f64);
        array.push(&entry.into());
    }
    array.into()
}

/// §9.6's verification report.
///
/// `notChecked` is carried across deliberately. A caller who sees `clean: true`
/// and no list of exclusions will read it as "this document contains no trace
/// of the redacted text", which is a stronger claim than the check makes.
pub fn redaction_report(report: &rasura::redaction::Report) -> JsValue {
    let traces = Array::new();
    for t in &report.traces {
        let entry = obj();
        set_str(&entry, "string", &t.string);
        set_str(&entry, "whereFound", &t.where_found);
        traces.push(&entry.into());
    }

    let out = obj();
    set_bool(&out, "clean", report.traces.is_empty());
    set(&out, "traces", &traces.into());
    set_num(&out, "objectsChecked", report.objects_checked as f64);
    set_num(&out, "streamsChecked", report.streams_checked as f64);
    set(&out, "notChecked", &strings(report.not_checked.iter().map(|s| s.to_string())));
    out.into()
}

pub fn weaknesses(list: &[rasura::protection::Weakness]) -> JsValue {
    use rasura::protection::Weakness as W;
    strings(list.iter().map(|w| {
        match w {
            W::LegacyKeyDerivation => "legacy-key-derivation",
            W::EmptyUserPassword => "empty-user-password",
            W::OwnerPasswordEqualsUser => "owner-password-equals-user",
        }
        .to_string()
    }))
}

pub fn save_mode(mode: rasura::SaveMode) -> &'static str {
    match mode {
        rasura::SaveMode::Incremental => "incremental",
        rasura::SaveMode::FullRewrite => "full-rewrite",
    }
}

fn confidence(c: rasura::page::Confidence) -> &'static str {
    use rasura::page::Confidence as C;
    match c {
        C::Exact => "exact",
        C::Partial => "partial",
        C::None => "none",
    }
}

fn alignment(a: rasura::Alignment) -> &'static str {
    use rasura::Alignment as A;
    match a {
        A::Left => "left",
        A::Right => "right",
        A::Centre => "centre",
        A::Justified => "justified",
        A::Unknown => "unknown",
    }
}

/// What a session has staged, for a caller deciding whether undo is available.
pub fn session_state(state: &rasura::SessionState) -> JsValue {
    let out = obj();
    set_num(&out, "staged", state.len() as f64);
    set_num(&out, "undone", state.redo_len() as f64);
    set_bool(&out, "canUndo", !state.is_empty());
    set_bool(&out, "canRedo", state.redo_len() > 0);
    set_bool(&out, "closed", state.is_closed());
    out.into()
}

/// Parse spec 9.3's line-breaking choice.
pub fn breaking(value: Option<&str>) -> Result<rasura::edit::Breaking, JsValue> {
    use rasura::edit::Breaking as B;
    match value {
        None | Some("greedy") => Ok(B::Greedy),
        Some("knuth-plass") => Ok(B::KnuthPlass),
        Some(other) => Err(err(
            Code::Internal.as_str(),
            &format!("unknown lineBreaking {other:?}; expected greedy or knuth-plass"),
        )),
    }
}

/// Parse spec 9.3's overflow policy.
pub fn overflow(value: Option<&str>) -> Result<rasura::edit::Overflow, JsValue> {
    use rasura::edit::Overflow as O;
    match value {
        None | Some("refuse") => Ok(O::Refuse),
        Some("allow") => Ok(O::Allow),
        Some("grow") => Ok(O::Grow),
        Some("shrink") => Ok(O::Shrink),
        Some(other) => Err(err(
            Code::Internal.as_str(),
            &format!("unknown overflow {other:?}; expected refuse, allow, grow or shrink"),
        )),
    }
}

/// Parse §11.4's `requireFidelity` argument.
///
/// An unknown string is an error rather than a silent fall back to the loosest
/// setting. A caller who typed `'exect'` asked for strictness and would
/// otherwise get none — the single worst way for this particular knob to fail.
pub fn fidelity_floor(value: Option<&str>) -> Result<rasura::edit::Fidelity, JsValue> {
    use rasura::edit::Fidelity as F;
    match value {
        None => Ok(F::Overlaid),
        Some("exact") => Ok(F::Exact),
        Some("reembedded") => Ok(F::Reembedded),
        Some("substituted") => Ok(F::Substituted),
        Some("overlaid") => Ok(F::Overlaid),
        Some(other) => Err(err(
            Code::Internal.as_str(),
            &format!(
                "unknown fidelity {other:?}; expected exact, reembedded, substituted or overlaid"
            ),
        )),
    }
}
