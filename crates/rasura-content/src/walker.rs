//! Content traversal with form XObject recursion. Spec 6.4.
//!
//! Walks a page's operators, driving the graphics state, and descends into form
//! XObjects with the CTM composed by their `/Matrix` and their own `/Resources`
//! pushed onto the stack.
//!
//! Tiling patterns and Type 3 glyph procedures are content streams too, and go
//! through the same machinery via [`walk_stream`].
//!
//! # Cycles
//!
//! A form XObject that invokes itself, directly or through a chain, is a real
//! file that exists. Guarding needs both a depth limit *and* a visited set of
//! the objects on the current path -- the depth limit alone permits an
//! exponential blowup where two forms invoke each other and each invocation
//! doubles the work.

use crate::content::LogicalContent;
use crate::matrix::Matrix;
use crate::op::{Op, OpKind};
use crate::page::Page;
use crate::resources::{ResourceStack, form_resources};
use crate::state::StateMachine;
use crate::tokenizer::tokenize;
use rasura_cos::document::Document;
use rasura_cos::error::{Leniency, LeniencyKind};
use rasura_cos::{Dictionary, ObjId, Object};
use std::collections::HashSet;

/// Recursion deeper than this is malformed. Real nesting rarely exceeds five.
const MAX_FORM_DEPTH: usize = 16;

/// Where the operators currently being visited came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The page's own `/Contents`.
    Page,
    /// A form XObject invoked by `Do`.
    Form(ObjId),
    /// A Type 3 glyph procedure.
    Type3Glyph(ObjId),
    /// A tiling pattern's content stream.
    Pattern(ObjId),
}

/// What the walker knows that the state machine does not.
pub struct WalkContext<'a> {
    /// The document, so a visitor can resolve the resources it is handed.
    /// Looking up a font is the reason this is here.
    pub doc: &'a Document,
    pub resources: &'a ResourceStack,
    pub content: &'a LogicalContent,
    pub source: Source,
    /// Form-XObject nesting depth. Zero on the page itself.
    pub depth: usize,
}

/// What a visitor wants to happen next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// For `Do`: do not descend into this XObject.
    SkipRecursion,
    /// Abandon the whole walk.
    Stop,
}

/// Receives each operator after the state machine has applied it.
///
/// The state is passed mutably because extraction needs to advance the text
/// matrix per glyph, which requires font metrics the walker does not have.
pub trait ContentVisitor {
    fn visit(&mut self, op: &Op, state: &mut StateMachine, ctx: &WalkContext<'_>) -> Flow;

    /// Called before descending into a form XObject, after its `/Matrix` has
    /// been applied. Default implementation permits the descent.
    fn enter_form(&mut self, _id: ObjId, _dict: &Dictionary, _state: &StateMachine) -> Flow {
        Flow::Continue
    }

    /// Called after leaving a form XObject.
    fn leave_form(&mut self, _id: ObjId) {}
}

/// A visitor built from a closure, for callers that need no state of their own.
pub struct FnVisitor<F>(pub F);

impl<F> ContentVisitor for FnVisitor<F>
where
    F: FnMut(&Op, &mut StateMachine, &WalkContext<'_>) -> Flow,
{
    fn visit(&mut self, op: &Op, state: &mut StateMachine, ctx: &WalkContext<'_>) -> Flow {
        (self.0)(op, state, ctx)
    }
}

/// The outcome of a walk.
#[derive(Debug, Default)]
pub struct WalkReport {
    pub ops_visited: usize,
    pub forms_entered: usize,
    pub leniencies: Vec<Leniency>,
    pub errors: Vec<rasura_cos::CosError>,
}

/// Walk a page's content.
pub fn walk_page(doc: &Document, page: &Page, visitor: &mut dyn ContentVisitor) -> WalkReport {
    let mut report = WalkReport::default();

    let (content, errors) = match crate::content::page_content(doc, &page.dict) {
        Ok(v) => v,
        Err(e) => {
            report.errors.push(e);
            return report;
        }
    };
    report.errors.extend(errors);
    if content.is_empty() {
        return report;
    }

    let resources = ResourceStack::from_page(page.resources.clone());
    let mut state = StateMachine::new(page.base_ctm());
    let mut path: HashSet<ObjId> = HashSet::new();

    walk_content(
        doc,
        &content,
        &resources,
        Source::Page,
        0,
        &mut state,
        &mut path,
        visitor,
        &mut report,
    );
    report.leniencies.extend(state.take_leniencies());
    report
}

/// Walk an arbitrary content stream -- a tiling pattern, a Type 3 glyph
/// procedure -- with a caller-supplied state and resources.
pub fn walk_stream(
    doc: &Document,
    id: ObjId,
    data: &[u8],
    resources: &ResourceStack,
    source: Source,
    state: &mut StateMachine,
    visitor: &mut dyn ContentVisitor,
) -> WalkReport {
    let mut report = WalkReport::default();
    let content = LogicalContent::single(id, data);
    let mut path: HashSet<ObjId> = HashSet::new();
    path.insert(id);
    walk_content(doc, &content, resources, source, 0, state, &mut path, visitor, &mut report);
    report
}

#[allow(clippy::too_many_arguments)]
fn walk_content(
    doc: &Document,
    content: &LogicalContent,
    resources: &ResourceStack,
    source: Source,
    depth: usize,
    state: &mut StateMachine,
    path: &mut HashSet<ObjId>,
    visitor: &mut dyn ContentVisitor,
    report: &mut WalkReport,
) -> Flow {
    let (ops, leniencies) = tokenize(content.data());
    report.leniencies.extend(leniencies);

    for op in &ops {
        // Apply first, then visit: the visitor sees the state as it is *at* the
        // operator, which for `'` and `"` means after their implicit T*.
        state.apply(op);
        report.ops_visited += 1;

        let ctx = WalkContext { doc, resources, content, source, depth };
        match visitor.visit(op, state, &ctx) {
            Flow::Stop => return Flow::Stop,
            Flow::SkipRecursion => continue,
            Flow::Continue => {}
        }

        if op.kind == OpKind::InvokeXObject
            && let Flow::Stop = do_xobject(doc, op, resources, depth, state, path, visitor, report)
        {
            return Flow::Stop;
        }
    }
    Flow::Continue
}

#[allow(clippy::too_many_arguments)]
fn do_xobject(
    doc: &Document,
    op: &Op,
    resources: &ResourceStack,
    depth: usize,
    state: &mut StateMachine,
    path: &mut HashSet<ObjId>,
    visitor: &mut dyn ContentVisitor,
    report: &mut WalkReport,
) -> Flow {
    let Some(name) = op.name(0) else { return Flow::Continue };
    let Some(xobject) = resources.xobject(doc, name) else {
        report.leniencies.push(Leniency::new(
            LeniencyKind::UnknownKeyword,
            op.span.start,
            format!("Do names /{} which /XObject does not define", display(name)),
        ));
        return Flow::Continue;
    };
    let Some(stream) = xobject.as_stream() else { return Flow::Continue };

    // Images are not content; the visitor already saw the `Do`.
    let subtype =
        stream.dict.get("Subtype").and_then(Object::as_name).map(|n| n.as_bytes().to_vec());
    if subtype.as_deref() != Some(b"Form") {
        return Flow::Continue;
    }

    let Some(id) = resources.xobject_id(doc, name) else {
        // A directly-embedded form has no object number, so it cannot recurse
        // into itself and needs no guard entry.
        return descend(
            doc,
            op,
            &xobject,
            ObjId::new(0, 0),
            resources,
            depth,
            state,
            path,
            visitor,
            report,
        );
    };

    if depth + 1 > MAX_FORM_DEPTH {
        report.leniencies.push(Leniency::new(
            LeniencyKind::UnknownKeyword,
            op.span.start,
            format!("form XObject nesting deeper than {MAX_FORM_DEPTH}; not descending"),
        ));
        return Flow::Continue;
    }
    if !path.insert(id) {
        report.leniencies.push(Leniency::new(
            LeniencyKind::UnknownKeyword,
            op.span.start,
            format!("form XObject {id} invokes itself; not descending"),
        ));
        return Flow::Continue;
    }

    let flow = descend(doc, op, &xobject, id, resources, depth, state, path, visitor, report);
    path.remove(&id);
    flow
}

#[allow(clippy::too_many_arguments)]
fn descend(
    doc: &Document,
    op: &Op,
    xobject: &Object,
    id: ObjId,
    resources: &ResourceStack,
    depth: usize,
    state: &mut StateMachine,
    path: &mut HashSet<ObjId>,
    visitor: &mut dyn ContentVisitor,
    report: &mut WalkReport,
) -> Flow {
    let Some(stream) = xobject.as_stream() else { return Flow::Continue };
    let dict = stream.dict.clone();

    let data = match doc.decoded_stream(id) {
        Ok(d) => d,
        Err(e) => {
            report.errors.push(e);
            return Flow::Continue;
        }
    };

    // ISO 32000-1 §8.10.1: invoking a form is equivalent to `q`, concatenating
    // /Matrix, clipping to /BBox, running the content, then `Q`.
    let saved = state.state().clone();
    if let Ok(Some(m)) = doc.get_entry(&dict, "Matrix")
        && let Some(items) = m.as_array()
        && let Some(matrix) = Matrix::from_array(items)
        && matrix.is_finite()
    {
        state.state_mut().ctm = matrix.then(&saved.ctm);
    }

    if visitor.enter_form(id, &dict, state) == Flow::Stop {
        *state.state_mut() = saved;
        return Flow::Stop;
    }
    report.forms_entered += 1;

    let inner = resources.with(form_resources(doc, &dict));
    let content = LogicalContent::single(id, &data);
    let flow = walk_content(
        doc,
        &content,
        &inner,
        Source::Form(id),
        depth + 1,
        state,
        path,
        visitor,
        report,
    );

    visitor.leave_form(id);
    // Restore, matching the implicit Q. Anything the form left on the stack is
    // discarded with it.
    *state.state_mut() = saved;
    let _ = op;
    flow
}

fn display(name: &rasura_cos::Name) -> String {
    String::from_utf8_lossy(name.as_bytes()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Point;
    use crate::page;
    use rasura_cos::testutil::ClassicBuilder;

    /// Records every text-showing operator with the CTM in force.
    #[derive(Default)]
    struct Recorder {
        shown: Vec<(String, Matrix, usize)>,
        forms: Vec<ObjId>,
    }

    impl ContentVisitor for Recorder {
        fn visit(&mut self, op: &Op, state: &mut StateMachine, ctx: &WalkContext<'_>) -> Flow {
            if op.kind == OpKind::ShowText
                && let Some(s) = op.operands.first().and_then(Object::as_string)
            {
                self.shown.push((
                    String::from_utf8_lossy(s.as_bytes()).into_owned(),
                    state.text_rendering_matrix(),
                    ctx.depth,
                ));
            }
            Flow::Continue
        }
        fn enter_form(&mut self, id: ObjId, _d: &Dictionary, _s: &StateMachine) -> Flow {
            self.forms.push(id);
            Flow::Continue
        }
    }

    fn walk(bytes: Vec<u8>) -> (Recorder, WalkReport) {
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let mut r = Recorder::default();
        let report = walk_page(&doc, &p, &mut r);
        (r, report)
    }

    #[test]
    fn walks_a_simple_page() {
        let (r, report) = walk(rasura_cos::testutil::classic_with_flate_content());
        assert_eq!(r.shown.len(), 1);
        assert_eq!(r.shown[0].0, "Hello, rasura");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.ops_visited >= 5);
    }

    fn form_page(form_content: &str, form_extra: &str, page_content: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Fm1 5 0 R >> /Font << /F1 6 0 R >> >> >>",
            )
            .stream(4, "", page_content.as_bytes())
            .stream(
                5,
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] {form_extra}"),
                form_content.as_bytes(),
            )
            .object(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .finish("/Root 1 0 R")
    }

    #[test]
    fn descends_into_a_form_xobject() {
        let (r, report) = walk(form_page(
            "BT /F1 12 Tf 0 0 Td (inside) Tj ET",
            "",
            "BT /F1 12 Tf 10 10 Td (outside) Tj ET /Fm1 Do",
        ));
        assert_eq!(report.forms_entered, 1);
        assert_eq!(r.forms, vec![ObjId::new(5, 0)]);
        let texts: Vec<&str> = r.shown.iter().map(|(s, _, _)| s.as_str()).collect();
        assert_eq!(texts, vec!["outside", "inside"]);
        assert_eq!(r.shown[0].2, 0, "page content is at depth 0");
        assert_eq!(r.shown[1].2, 1, "form content is at depth 1");
    }

    #[test]
    fn form_matrix_composes_into_the_ctm() {
        // The form draws at (0,0); its /Matrix translates by (200, 300).
        let (r, _) = walk(form_page(
            "BT /F1 12 Tf 0 0 Td (inside) Tj ET",
            "/Matrix [1 0 0 1 200 300]",
            "/Fm1 Do",
        ));
        let trm = r.shown[0].1;
        let origin = trm.apply(Point::new(0.0, 0.0));
        // Page is 792 tall and the base CTM flips y, so user (200,300) is
        // device (200, 492).
        assert!((origin.x - 200.0).abs() < 1e-6, "{origin:?}");
        assert!((origin.y - 492.0).abs() < 1e-6, "{origin:?}");
    }

    #[test]
    fn the_ctm_is_restored_after_a_form() {
        // `Do` is equivalent to q ... Q, so content after it is unaffected by
        // whatever the form did to the CTM.
        let (r, _) = walk(form_page(
            "5 0 0 5 50 50 cm",
            "/Matrix [1 0 0 1 200 300]",
            "/Fm1 Do BT /F1 12 Tf 10 20 Td (after) Tj ET",
        ));
        let trm = r.shown[0].1;
        let origin = trm.apply(Point::new(0.0, 0.0));
        assert!((origin.x - 10.0).abs() < 1e-6, "{origin:?}");
        assert!((origin.y - 772.0).abs() < 1e-6, "{origin:?}");
    }

    #[test]
    fn a_form_uses_its_own_resources_first() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Fm1 5 0 R >> /Font << /F1 6 0 R >> >> >>",
            )
            .stream(4, "", b"/Fm1 Do")
            .stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                 /Resources << /Font << /F1 7 0 R >> >>",
                b"BT /F1 12 Tf 0 0 Td (x) Tj ET",
            )
            .object(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .object(7, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>")
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);

        // Resolve /F1 as the form's own walk would.
        let mut seen: Vec<String> = Vec::new();
        let mut v = FnVisitor(|op: &Op, state: &mut StateMachine, ctx: &WalkContext<'_>| {
            if op.kind == OpKind::ShowText
                && let Some(name) = state.text().font.clone()
                && let Some(font) = ctx.resources.font(&doc, &name)
            {
                seen.push(
                    String::from_utf8_lossy(
                        font.as_dict()
                            .unwrap()
                            .get("BaseFont")
                            .unwrap()
                            .as_name()
                            .unwrap()
                            .as_bytes(),
                    )
                    .into_owned(),
                );
            }
            Flow::Continue
        });
        walk_page(&doc, &p, &mut v);
        assert_eq!(seen, vec!["Courier"], "the form's own /F1 must win");
    }

    #[test]
    fn a_self_invoking_form_is_pruned_and_reported() {
        let (_, report) = walk(form_page(
            "BT /F1 12 Tf 0 0 Td (loop) Tj ET /Fm1 Do",
            "/Resources << /XObject << /Fm1 5 0 R >> /Font << /F1 6 0 R >> >>",
            "/Fm1 Do",
        ));
        assert_eq!(report.forms_entered, 1, "must descend exactly once");
        assert!(
            report.leniencies.iter().any(|l| l.detail.contains("invokes itself")),
            "{:?}",
            report.leniencies
        );
    }

    #[test]
    fn mutually_recursive_forms_terminate() {
        // Two forms invoking each other: the depth limit alone would allow an
        // exponential blowup, so the visited set has to catch it.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /XObject << /A 5 0 R /B 6 0 R >> >> >>",
            )
            .stream(4, "", b"/A Do")
            .stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 9 9] \
                 /Resources << /XObject << /B 6 0 R >> >>",
                b"/B Do",
            )
            .stream(
                6,
                "/Type /XObject /Subtype /Form /BBox [0 0 9 9] \
                 /Resources << /XObject << /A 5 0 R >> >>",
                b"/A Do",
            )
            .finish("/Root 1 0 R");
        let (_, report) = walk(bytes);
        assert_eq!(report.forms_entered, 2);
        assert!(report.leniencies.iter().any(|l| l.detail.contains("invokes itself")));
    }

    #[test]
    fn an_image_xobject_is_not_descended_into() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /XObject << /Im1 5 0 R >> >> >>",
            )
            .stream(4, "", b"/Im1 Do")
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
                 /ColorSpace /DeviceGray",
                b"\x00",
            )
            .finish("/Root 1 0 R");
        let (_, report) = walk(bytes);
        assert_eq!(report.forms_entered, 0);
        assert!(report.leniencies.is_empty(), "{:?}", report.leniencies);
    }

    #[test]
    fn a_do_naming_a_missing_xobject_is_reported_not_fatal() {
        let (_, report) = walk(form_page("", "", "/Nope Do BT /F1 12 Tf (still here) Tj ET"));
        assert!(report.leniencies.iter().any(|l| l.detail.contains("does not define")));
        assert!(report.ops_visited >= 3, "the walk must continue past the bad Do");
    }

    #[test]
    fn a_visitor_can_stop_the_walk() {
        let doc = Document::open(rasura_cos::testutil::classic_with_flate_content()).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let mut count = 0usize;
        let mut v = FnVisitor(|_op: &Op, _s: &mut StateMachine, _c: &WalkContext<'_>| {
            count += 1;
            if count >= 2 { Flow::Stop } else { Flow::Continue }
        });
        let report = walk_page(&doc, &p, &mut v);
        assert_eq!(report.ops_visited, 2);
    }

    #[test]
    fn skip_recursion_prevents_descent_without_stopping() {
        let doc = Document::open(form_page(
            "BT /F1 12 Tf (inside) Tj ET",
            "",
            "/Fm1 Do BT /F1 12 Tf (after) Tj ET",
        ))
        .unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let mut shown: Vec<String> = Vec::new();
        let mut v = FnVisitor(|op: &Op, _s: &mut StateMachine, _c: &WalkContext<'_>| {
            if op.kind == OpKind::InvokeXObject {
                return Flow::SkipRecursion;
            }
            if op.kind == OpKind::ShowText
                && let Some(s) = op.operands.first().and_then(Object::as_string)
            {
                shown.push(String::from_utf8_lossy(s.as_bytes()).into_owned());
            }
            Flow::Continue
        });
        let report = walk_page(&doc, &p, &mut v);
        assert_eq!(report.forms_entered, 0);
        assert_eq!(shown, vec!["after"]);
    }

    #[test]
    fn spans_reported_during_a_walk_map_back_to_their_object() {
        let doc = Document::open(rasura_cos::testutil::classic_with_flate_content()).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let mut sources: Vec<Option<ObjId>> = Vec::new();
        let mut v = FnVisitor(|op: &Op, _s: &mut StateMachine, ctx: &WalkContext<'_>| {
            sources.push(ctx.content.source_of(op.span.start));
            Flow::Continue
        });
        walk_page(&doc, &p, &mut v);
        assert!(!sources.is_empty());
        assert!(sources.iter().all(|s| *s == Some(ObjId::new(4, 0))), "{sources:?}");
    }
}
