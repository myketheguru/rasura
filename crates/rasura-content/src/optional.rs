//! Optional content — layers. Spec 10.2.
//!
//! > Preserve `/OCProperties`, `/OCGs`, `/OCMDs`. Expose layer visibility
//! > state. Content inside a `BDC /OC` block belongs to that layer; edits must
//! > stay inside it. **Do not flatten layers.**
//!
//! # What a layer actually is
//!
//! Not a container. ISO 32000-1 §8.11 has no structure that *holds* content: an
//! optional content group is a dictionary with a name, and content belongs to it
//! by being lexically between a `/OC /Name BDC` and its matching `EMC` in the
//! page's stream. The same group can claim spans on six pages, interleaved with
//! spans belonging to other groups, and nothing in the file lists them.
//!
//! That has one consequence worth stating before any code: **a hidden layer's
//! text is in the document.** It extracts, it is found by `strings`, it is
//! copied by a reader that ignores visibility. "Hidden" is an instruction to a
//! viewer, not a property of the bytes — which is why [`crate::dest`]'s
//! neighbours in the edit layer treat a hidden run as ordinary content to be
//! redacted, and why this module reports visibility rather than acting on it.
//!
//! # Do not flatten
//!
//! The spec's emphasis, and the reason is the same shape as the redaction one.
//! Flattening a layer means deciding which content survives — and the decision
//! depends on a configuration the *viewer* owns, that the user can change after
//! the file is saved. A CAD drawing with its dimensions layer off is not a
//! drawing without dimensions; it is a drawing whose dimensions someone will
//! turn back on. Baking the current state destroys information the format was
//! specifically designed to keep.
//!
//! So nothing here removes anything. Reading reports what is visible in the
//! default configuration, and the writer preserves `/OCProperties` unchanged
//! because it preserves every object it was not asked to touch.
//!
//! # What the corpus says
//!
//! Measured over the 992 corpus documents that open, 1,484 pages walked:
//!
//! | | Count |
//! |---|---|
//! | Documents with `/OCProperties` | 29 (2.9%) |
//! | Layers declared | 134 |
//! | **Layers off in the default configuration** | **89 (66%)** |
//! | Pages carrying optional content | 25 |
//! | Regions | 1,091 |
//! | **Regions hidden** | **1,050 (96%)** |
//! | Regions from an XObject's own `/OC` | 13 |
//!
//! Two of those numbers changed what this module does.
//!
//! **Most optional content is hidden.** Two thirds of declared layers are off,
//! and 96% of the regions on a page belong to one. So "a hidden layer's text is
//! in the document" is not a corner case to note in passing — it is the normal
//! state of every file that has layers at all, and it is why the edit layer is
//! told about visibility and the redaction path deliberately ignores it.
//!
//! **The XObject form is not negligible.** Thirteen regions come from an
//! XObject whose *own dictionary* carries `/OC`, with no `BDC` anywhere near
//! them. A walker looking only for marked content would count every one of
//! those as ordinary visible content — a hidden watermark reported as drawn.
//!
//! `/OCMD` is rare by comparison, and `/VE` rarer still. The expression form is
//! implemented anyway because it is the only one that can express "on when A
//! and not B", which the four policies cannot; it is depth-bounded because it
//! is a tree read from a file.

use crate::content::LogicalContent;
use crate::op::OpKind;
use crate::page::Page;
use crate::resources::ResourceStack;
use rasura_cos::object::{Dictionary, Name, Object};
use rasura_cos::{Document, ObjId};
use std::collections::BTreeMap;
use std::ops::Range;

/// How deep a `/VE` visibility expression may nest before the read gives up.
const MAX_EXPRESSION_DEPTH: usize = 32;

/// One optional content group. ISO 32000-1 §8.11.2.
#[derive(Debug, Clone)]
pub struct Layer {
    pub id: ObjId,
    /// `/Name`. Required by the specification and absent in real files, where
    /// it becomes the empty string rather than a reason to drop the layer.
    pub name: String,
    /// Whether the default configuration `/D` shows it.
    pub visible: bool,
    /// `/Intent`. `/View` is the usual value; `/Design` marks a group that
    /// affects authoring rather than display, and a viewer applying visibility
    /// only considers `/View` groups.
    pub intents: Vec<String>,
    /// Listed in `/D` `/Locked`: the user may not toggle it.
    pub locked: bool,
    /// `/Usage`, which records *why* a group is on or off — for printing, for
    /// zoom range, for a language. Reported, never acted on: applying a usage
    /// rule means knowing the viewing context, which a library does not.
    pub usage: Option<Dictionary>,
}

/// How an `/OCMD` combines several groups. ISO 32000-1 Table 101.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Visible when any group is on. The default.
    #[default]
    AnyOn,
    AllOn,
    AnyOff,
    AllOff,
}

/// A `/VE` visibility expression: the general form, as a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expression {
    Group(ObjId),
    Not(Box<Expression>),
    And(Vec<Expression>),
    Or(Vec<Expression>),
}

/// An `/OCMD`, or a bare group used directly as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub groups: Vec<ObjId>,
    pub policy: Policy,
    /// `/VE`. When present it **overrides** `/OCGs` and `/P` entirely — Table
    /// 101 says so, and a reader that honoured both would disagree with itself
    /// on any file carrying a `/VE` that contradicts its policy.
    pub expression: Option<Expression>,
}

/// A document's optional content, as the default configuration leaves it.
#[derive(Debug, Clone, Default)]
pub struct OptionalContent {
    pub layers: Vec<Layer>,
    /// `/D` `/BaseState`: what a group not named in `/ON` or `/OFF` defaults to.
    pub base_state_on: bool,
    by_id: BTreeMap<ObjId, usize>,
}

impl OptionalContent {
    pub fn layer(&self, id: ObjId) -> Option<&Layer> {
        self.by_id.get(&id).and_then(|i| self.layers.get(*i))
    }

    /// Whether a single group is on in the default configuration.
    ///
    /// A group this document does not declare is treated as **visible**. That
    /// is the reader-friendly reading of §8.11.2.3 — content whose group cannot
    /// be found is content nothing has turned off — and the alternative hides
    /// text on the strength of a dangling reference.
    pub fn group_visible(&self, id: ObjId) -> bool {
        self.layer(id).is_none_or(|l| l.visible)
    }

    /// Whether content governed by `membership` is visible.
    pub fn visible(&self, membership: &Membership) -> bool {
        if let Some(expression) = &membership.expression {
            return self.evaluate(expression);
        }
        let states = || membership.groups.iter().map(|id| self.group_visible(*id));
        match membership.policy {
            // An /OCMD naming no groups is visible: there is no condition to
            // fail. `any()` on an empty iterator is false, which would hide it.
            Policy::AnyOn => membership.groups.is_empty() || states().any(|on| on),
            Policy::AllOn => states().all(|on| on),
            Policy::AnyOff => membership.groups.is_empty() || states().any(|on| !on),
            Policy::AllOff => states().all(|on| !on),
        }
    }

    fn evaluate(&self, expression: &Expression) -> bool {
        match expression {
            Expression::Group(id) => self.group_visible(*id),
            Expression::Not(inner) => !self.evaluate(inner),
            Expression::And(items) => items.iter().all(|e| self.evaluate(e)),
            Expression::Or(items) => items.iter().any(|e| self.evaluate(e)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// Read `/OCProperties`. Spec 10.2.
///
/// `None` when the document declares no optional content, which is 97% of them.
pub fn read(doc: &Document) -> Option<OptionalContent> {
    let catalog = doc.catalog().ok()?;
    let properties = doc.get_entry(catalog.as_dict()?, "OCProperties").ok()??;
    let properties = properties.as_dict()?;

    // The default configuration. Alternates in `/Configs` exist and are not
    // read: a viewer picks one, and picking a different one here would report a
    // visibility no reader will show.
    let default = doc
        .get_entry(properties, "D")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned())
        .unwrap_or_default();

    let base_state_on = default
        .get("BaseState")
        .and_then(Object::as_name)
        .and_then(|n| n.as_str())
        .is_none_or(|s| s == "ON");

    let on = id_set(doc, &default, "ON");
    let off = id_set(doc, &default, "OFF");
    let locked = id_set(doc, &default, "Locked");

    let declared = doc.get_entry(properties, "OCGs").ok().flatten();
    let declared =
        declared.as_deref().and_then(Object::as_array).map(<[Object]>::to_vec).unwrap_or_default();

    let mut layers = Vec::new();
    let mut by_id = BTreeMap::new();
    for entry in &declared {
        let Some(id) = entry.as_reference() else { continue };
        if by_id.contains_key(&id) {
            continue;
        }
        let Ok(object) = doc.get(id) else { continue };
        let Some(dict) = object.as_dict() else { continue };

        // /OFF wins over /ON. A group in both is a producer bug, and hiding it
        // is the conservative reading: showing content someone marked off is
        // the more surprising of the two failures.
        let visible = if off.contains(&id) {
            false
        } else if on.contains(&id) {
            true
        } else {
            base_state_on
        };

        by_id.insert(id, layers.len());
        layers.push(Layer {
            id,
            name: dict
                .get("Name")
                .and_then(Object::as_string)
                .map(|s| s.as_text())
                .unwrap_or_default(),
            visible,
            intents: intents_of(doc, dict),
            locked: locked.contains(&id),
            usage: doc.get_entry(dict, "Usage").ok().flatten().and_then(|u| u.as_dict().cloned()),
        });
    }

    Some(OptionalContent { layers, base_state_on, by_id })
}

/// `/Intent`, which is a name or an array of them.
fn intents_of(doc: &Document, dict: &Dictionary) -> Vec<String> {
    let Ok(Some(value)) = doc.get_entry(dict, "Intent") else {
        // Table 98: the default is /View.
        return vec!["View".to_string()];
    };
    let one = |o: &Object| o.as_name().and_then(|n| n.as_str()).map(str::to_string);
    match value.as_ref() {
        Object::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

fn id_set(doc: &Document, dict: &Dictionary, key: &str) -> Vec<ObjId> {
    doc.get_entry(dict, key)
        .ok()
        .flatten()
        .and_then(|v| v.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default()
        .iter()
        .filter_map(Object::as_reference)
        .collect()
}

/// Read an `/OC` value, which may be a group or a membership dictionary.
pub fn membership(doc: &Document, value: &Object) -> Option<Membership> {
    let id = value.as_reference();
    let object = doc.resolve(value).ok()?;
    let dict = object.as_dict()?;

    match dict.type_name().and_then(|n| n.as_str()) {
        Some("OCMD") => {
            let groups = match doc.get_entry(dict, "OCGs").ok().flatten().as_deref() {
                // A single group, written directly rather than in an array.
                Some(Object::Dictionary(_)) => {
                    dict.get("OCGs").and_then(Object::as_reference).into_iter().collect()
                }
                Some(Object::Array(items)) => {
                    items.iter().filter_map(Object::as_reference).collect()
                }
                _ => dict.get("OCGs").and_then(Object::as_reference).into_iter().collect(),
            };
            let policy = match dict.get("P").and_then(Object::as_name).and_then(|n| n.as_str()) {
                Some("AllOn") => Policy::AllOn,
                Some("AnyOff") => Policy::AnyOff,
                Some("AllOff") => Policy::AllOff,
                _ => Policy::AnyOn,
            };
            let expression = dict.get("VE").and_then(|ve| expression(doc, ve, 0));
            Some(Membership { groups, policy, expression })
        }
        // A bare /OCG used as an /OC value: the common case by a wide margin.
        _ => Some(Membership {
            groups: id.into_iter().collect(),
            policy: Policy::AnyOn,
            expression: None,
        }),
    }
}

/// A `/VE` array: `[/Not e]`, `[/And e...]`, `[/Or e...]`, or a group reference.
fn expression(doc: &Document, value: &Object, depth: usize) -> Option<Expression> {
    if depth > MAX_EXPRESSION_DEPTH {
        return None;
    }
    if let Some(id) = value.as_reference() {
        // A reference here is either a nested expression array or a group. The
        // array case has to be resolved; the group case must not be, because
        // resolving it loses the object number the expression is *about*.
        if let Ok(object) = doc.resolve(value)
            && let Some(items) = object.as_array()
        {
            return expression_from(doc, items, depth);
        }
        return Some(Expression::Group(id));
    }
    let items = value.as_array()?;
    expression_from(doc, items, depth)
}

fn expression_from(doc: &Document, items: &[Object], depth: usize) -> Option<Expression> {
    let operator = items.first()?.as_name()?.as_str()?;
    let operands: Vec<Expression> =
        items[1..].iter().filter_map(|o| expression(doc, o, depth + 1)).collect();
    match operator {
        "Not" => operands.into_iter().next().map(|e| Expression::Not(Box::new(e))),
        "And" => Some(Expression::And(operands)),
        "Or" => Some(Expression::Or(operands)),
        _ => None,
    }
}

/// A span of a page's content that belongs to optional content.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Byte range in the logical content buffer, from the `BDC` through the
    /// matching `EMC` — or the single `Do` operator, for an XObject whose own
    /// dictionary carries `/OC`.
    pub span: Range<usize>,
    /// Whether the default configuration shows it.
    pub visible: bool,
    /// The layer names involved, for a report a person reads.
    pub layers: Vec<String>,
    /// Whether this came from a `BDC` block or from an XObject's `/OC`.
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `/OC /Name BDC` … `EMC` in the content stream.
    MarkedContent,
    /// A form or image XObject whose dictionary has `/OC`, made conditional at
    /// the `Do` that draws it.
    XObject,
}

/// Every optional-content region on a page, in content order. Spec 10.2.
///
/// Nested regions each produce their own entry, inner ones included: content
/// inside two `BDC /OC` blocks is hidden if *either* is off, and a caller
/// asking "is this byte visible" needs both to answer.
///
/// Annotations are not covered here. Their `/OC` sits on the annotation
/// dictionary rather than in the page's stream, and they are not part of a
/// content span — [`membership`] reads one directly for a caller that wants it.
pub fn regions(
    doc: &Document,
    page: &Page,
    content: &LogicalContent,
    oc: &OptionalContent,
) -> Vec<Region> {
    let (ops, _) = crate::tokenize(content.data());
    let resources = ResourceStack::from_page(doc.get_entry(&page.dict, "Resources").ok().flatten());

    let mut out = Vec::new();
    // What is open. `None` for a `BMC`/`BDC` that is not optional content,
    // which still has to be tracked so `EMC` closes the right one.
    let mut open: Vec<Option<(usize, Membership)>> = Vec::new();

    for op in &ops {
        match op.kind {
            OpKind::BeginMarked => open.push(None),
            OpKind::BeginMarkedProps => {
                let is_oc = op
                    .operands
                    .first()
                    .and_then(Object::as_name)
                    .is_some_and(|n| n.as_bytes() == b"OC");
                let membership = if is_oc {
                    op.operands.get(1).and_then(|value| resolve_oc(doc, &resources, value))
                } else {
                    None
                };
                open.push(membership.map(|m| (op.span.start, m)));
            }
            OpKind::EndMarked => {
                if let Some(Some((start, membership))) = open.pop() {
                    out.push(region_of(oc, start..op.span.end, &membership, Source::MarkedContent));
                }
            }
            OpKind::InvokeXObject => {
                // An XObject's own /OC governs the `Do` that draws it, whether
                // or not any BDC is open. Missed, and a hidden logo counts as
                // visible content.
                let Some(name) = op.operands.first().and_then(Object::as_name) else { continue };
                let Some(dict) = xobject_dict(doc, &resources, name) else { continue };
                let Some(value) = dict.get("OC") else { continue };
                let Some(membership) = membership(doc, value) else { continue };
                out.push(region_of(oc, op.span.clone(), &membership, Source::XObject));
            }
            _ => {}
        }
    }
    out
}

fn region_of(
    oc: &OptionalContent,
    span: Range<usize>,
    membership: &Membership,
    source: Source,
) -> Region {
    Region {
        span,
        visible: oc.visible(membership),
        layers: membership
            .groups
            .iter()
            .map(|id| match oc.layer(*id) {
                Some(l) if !l.name.is_empty() => l.name.clone(),
                _ => format!("{id}"),
            })
            .collect(),
        source,
    }
}

/// `/OC /Name BDC`, where the name indexes the page's `/Properties`.
///
/// The inline-dictionary form is also accepted: `BDC`'s second operand may be a
/// dictionary written in the stream, and while no producer does that for `/OC`,
/// nothing forbids it.
fn resolve_oc(doc: &Document, resources: &ResourceStack, value: &Object) -> Option<Membership> {
    match value {
        // Looked up *without* resolving: an `/OC` naming a group is a statement
        // about which group, and resolving the reference discards the object
        // number that is the whole answer.
        Object::Name(name) => {
            let entry = resources.lookup_raw(doc, "Properties", name)?;
            membership(doc, &entry)
        }
        other => membership(doc, other),
    }
}

fn xobject_dict(doc: &Document, resources: &ResourceStack, name: &Name) -> Option<Dictionary> {
    let entry = resources.lookup(doc, "XObject", name)?;
    let object = doc.resolve(&entry).ok()?;
    match object.as_ref() {
        Object::Stream(s) => Some(s.dict.clone()),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// Whether a byte offset falls inside any region that is turned **off**.
///
/// The question the edit layer asks: content here is in the file and not on the
/// page, so an edit to it will change nothing a reader sees.
pub fn hidden_at(regions: &[Region], at: usize) -> Option<&Region> {
    regions.iter().find(|r| !r.visible && r.span.contains(&at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page with two layers: one on, one off, each wrapping a word.
    fn layered(config: &str) -> Vec<u8> {
        let content = b"/OC /L1 BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (shown) Tj ET EMC\n\
                        /OC /L2 BDC BT /F1 12 Tf 1 0 0 1 72 680 Tm (hidden) Tj ET EMC\n\
                        BT /F1 12 Tf 1 0 0 1 72 660 Tm (always) Tj ET\n";

        ClassicBuilder::new()
            .object(1, &format!("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 7 0 R] /D << {config} >> >> >>"))
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> \
                 /Properties << /L1 6 0 R /L2 7 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(6, "<< /Type /OCG /Name (Visible layer) >>")
            .object(7, "<< /Type /OCG /Name (Hidden layer) >>")
            .finish("/Root 1 0 R")
    }

    fn analysed(bytes: Vec<u8>) -> (Document, OptionalContent, Vec<Region>) {
        let doc = Document::open(bytes).expect("open");
        let oc = read(&doc).expect("optional content");
        let pages = crate::page::pages(&doc).expect("pages");
        let (content, _) = crate::page_content(&doc, &pages.pages[0].dict).expect("content");
        let regions = regions(&doc, &pages.pages[0], &content, &oc);
        (doc, oc, regions)
    }

    #[test]
    fn layers_are_read_with_their_names_and_visibility() {
        let (_doc, oc, _) = analysed(layered("/OFF [7 0 R]"));
        assert_eq!(oc.layers.len(), 2);
        assert_eq!(oc.layers[0].name, "Visible layer");
        assert!(oc.layers[0].visible);
        assert_eq!(oc.layers[1].name, "Hidden layer");
        assert!(!oc.layers[1].visible);
    }

    #[test]
    fn a_bdc_block_becomes_a_region_covering_its_content() {
        let (_doc, _oc, regions) = analysed(layered("/OFF [7 0 R]"));
        assert_eq!(regions.len(), 2, "{regions:#?}");
        assert!(regions[0].visible);
        assert!(!regions[1].visible);
        assert_eq!(regions[1].layers, vec!["Hidden layer"]);
        assert_eq!(regions[1].source, Source::MarkedContent);
    }

    #[test]
    fn the_hidden_span_covers_the_text_and_not_the_text_after_it() {
        // The property an edit depends on: a byte inside the hidden block is
        // reported hidden, and the unmarked line after it is not.
        let bytes = layered("/OFF [7 0 R]");
        let (_doc, _oc, regions) = analysed(bytes.clone());
        let text = String::from_utf8_lossy(&bytes).to_string();

        let content_start = text.find("/OC /L1").expect("content in the fixture");
        let hidden_at_offset = |needle: &str| {
            let at = text.find(needle).expect(needle) - content_start;
            hidden_at(&regions, at).is_some()
        };
        assert!(hidden_at_offset("(hidden)"));
        assert!(!hidden_at_offset("(shown)"));
        assert!(!hidden_at_offset("(always)"));
    }

    #[test]
    fn base_state_off_hides_everything_not_explicitly_on() {
        let (_doc, oc, _) = analysed(layered("/BaseState /OFF /ON [6 0 R]"));
        assert!(oc.layers[0].visible, "named in /ON");
        assert!(!oc.layers[1].visible, "not named, and the base state is off");
    }

    #[test]
    fn off_wins_over_on_for_a_group_named_in_both() {
        // A producer bug. Hiding is the conservative reading: showing content
        // somebody marked off is the more surprising of the two failures.
        let (_doc, oc, _) = analysed(layered("/ON [7 0 R] /OFF [7 0 R]"));
        assert!(!oc.layers[1].visible);
    }

    #[test]
    fn a_locked_layer_is_reported_as_locked() {
        let (_doc, oc, _) = analysed(layered("/Locked [6 0 R]"));
        assert!(oc.layers[0].locked);
        assert!(!oc.layers[1].locked);
    }

    #[test]
    fn a_document_with_no_optional_content_reads_as_none() {
        let doc = Document::open(rasura_cos::testutil::minimal_classic()).expect("open");
        assert!(read(&doc).is_none());
    }

    /// A page whose content is governed by an `/OCMD` with a policy.
    fn with_ocmd(ocmd: &str, config: &str) -> Vec<u8> {
        let content = b"/OC /M BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (conditional) Tj ET EMC\n";
        ClassicBuilder::new()
            .object(1, &format!("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 7 0 R] /D << {config} >> >> >>"))
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /Properties << /M 8 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            )
            .object(6, "<< /Type /OCG /Name (A) >>")
            .object(7, "<< /Type /OCG /Name (B) >>")
            .object(8, &format!("<< /Type /OCMD {ocmd} >>"))
            .finish("/Root 1 0 R")
    }

    fn ocmd_visible(ocmd: &str, config: &str) -> bool {
        let doc = Document::open(with_ocmd(ocmd, config)).expect("open");
        let oc = read(&doc).expect("optional content");
        let pages = crate::page::pages(&doc).expect("pages");
        let (content, _) = crate::page_content(&doc, &pages.pages[0].dict).expect("content");
        let regions = regions(&doc, &pages.pages[0], &content, &oc);
        assert_eq!(regions.len(), 1, "{regions:#?}");
        regions[0].visible
    }

    #[test]
    fn the_four_membership_policies_each_do_what_they_say() {
        // B is off, A is on, throughout.
        let off_b = "/OFF [7 0 R]";
        let both = "/OCGs [6 0 R 7 0 R]";
        assert!(ocmd_visible(&format!("{both} /P /AnyOn"), off_b), "A is on");
        assert!(!ocmd_visible(&format!("{both} /P /AllOn"), off_b), "B is off");
        assert!(ocmd_visible(&format!("{both} /P /AnyOff"), off_b), "B is off");
        assert!(!ocmd_visible(&format!("{both} /P /AllOff"), off_b), "A is on");
    }

    #[test]
    fn a_visibility_expression_overrides_the_policy() {
        // Table 101 says /VE wins. A reader honouring both would disagree with
        // itself on any file where they conflict -- and this fixture is one:
        // the policy says visible, the expression says not.
        let visible = ocmd_visible(
            "/OCGs [6 0 R 7 0 R] /P /AnyOn /VE [/Not [/Or 6 0 R 7 0 R]]",
            "/OFF [7 0 R]",
        );
        assert!(!visible, "/VE must win over /P");
    }

    #[test]
    fn an_and_expression_needs_every_group() {
        assert!(ocmd_visible("/VE [/And 6 0 R 7 0 R]", ""), "both on by default");
        assert!(!ocmd_visible("/VE [/And 6 0 R 7 0 R]", "/OFF [7 0 R]"), "B is off");
    }

    #[test]
    fn an_ocmd_naming_no_groups_is_visible() {
        // There is no condition to fail. `any()` over an empty list is false,
        // which would hide content nothing asked to hide.
        assert!(ocmd_visible("/P /AnyOn", ""));
    }

    #[test]
    fn an_unknown_group_is_treated_as_visible() {
        // Content whose group cannot be found is content nothing turned off.
        // The alternative hides text on the strength of a dangling reference.
        let oc = OptionalContent::default();
        assert!(oc.group_visible(ObjId::new(99, 0)));
    }

    #[test]
    fn an_xobject_with_its_own_oc_is_a_region() {
        // The /OC is on the XObject rather than in the stream, so a walker
        // looking only for BDC counts a hidden logo as visible content.
        let content = b"q 1 0 0 1 100 100 cm /X1 Do Q\n";
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /X1 7 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(6, "<< /Type /OCG /Name (Watermark) >>")
            .stream(
                7,
                " /Type /XObject /Subtype /Form /BBox [0 0 10 10] /OC 6 0 R",
                b"0 0 10 10 re f\n",
            )
            .finish("/Root 1 0 R")
        ;
        let (_doc, _oc, regions) = analysed(bytes);
        assert_eq!(regions.len(), 1, "{regions:#?}");
        assert_eq!(regions[0].source, Source::XObject);
        assert!(!regions[0].visible);
        assert_eq!(regions[0].layers, vec!["Watermark"]);
    }

    #[test]
    fn nested_blocks_each_produce_a_region() {
        // Content inside two /OC blocks is hidden if either is off, so a caller
        // asking about a byte needs both.
        let content = b"/OC /L1 BDC /OC /L2 BDC BT /F1 12 Tf (deep) Tj ET EMC EMC\n";
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 7 0 R] /D << /OFF [7 0 R] >> >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /Properties << /L1 6 0 R /L2 7 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /OCG /Name (Outer) >>")
            .object(7, "<< /Type /OCG /Name (Inner) >>")
            .finish("/Root 1 0 R");

        let (_doc, _oc, regions) = analysed(bytes);
        assert_eq!(regions.len(), 2, "{regions:#?}");
        // The inner block closes first.
        assert_eq!(regions[0].layers, vec!["Inner"]);
        assert!(!regions[0].visible);
        assert_eq!(regions[1].layers, vec!["Outer"]);
        assert!(regions[1].visible);
        // ...and the outer span contains the inner one.
        assert!(regions[1].span.start <= regions[0].span.start);
        assert!(regions[1].span.end >= regions[0].span.end);
    }

    #[test]
    fn a_non_oc_marked_block_does_not_confuse_the_nesting() {
        // `/P << /MCID 0 >> BDC` is not optional content, and its EMC must not
        // close an /OC block that is still open.
        let content = b"/OC /L1 BDC /P << /MCID 0 >> BDC BT /F1 12 Tf (x) Tj ET EMC EMC\n";
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /Properties << /L1 6 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(6, "<< /Type /OCG /Name (Only) >>")
            .finish("/Root 1 0 R");

        let (_doc, _oc, regions) = analysed(bytes);
        assert_eq!(regions.len(), 1, "{regions:#?}");
        // The region runs to the *second* EMC, covering the whole nested block.
        let content_len = content.len();
        assert!(
            regions[0].span.end >= content_len - 1,
            "the /OC block was closed by the inner EMC: {:?}",
            regions[0].span
        );
    }
}
