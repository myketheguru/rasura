//! The text operations. Spec 9.2.
//!
//! > - `replace_text(para, range, text)`
//! > - `insert_text(para, offset, text)`
//! > - `delete_range(para, range)`
//! > - `set_alignment(para, align)`, `set_leading(para, value)`,
//! >   `set_indent(para, IndentSpec)`
//!
//! Each returns an [`Edit`] — patches plus the fidelity achieved — and applies
//! nothing. The session decides when bytes move, so a caller can inspect what an
//! operation *would* cost before accepting it, which is what spec §2's second
//! property is for.
//!
//! # The unit of replacement is the showing operator
//!
//! An edit could in principle rewrite the exact bytes of the characters it
//! touched, leaving the rest of the operator's string alone. It does not, for a
//! reason worth stating: a code's byte length is a property of the font's
//! codespace, not of the character, so replacing `e` with `é` in a composite
//! font changes the string's length in a way that invalidates every glyph span
//! measured against it. Regenerating the whole operator from the whole run's
//! text keeps one source of truth.
//!
//! It also bounds the damage. Everything outside that operator is copied
//! verbatim by the splice, so an edit to one word cannot move a different line
//! however wrong the encoding turns out to be.

use crate::encode::Encoder;
use crate::locate::{EditablePage, ParagraphId, Selection, select};
use crate::numfmt::NumberStyle;
use crate::patch::Patch;
use crate::reflow::{self, Measure, Policy};
use crate::session::{Compromise, Fidelity};
use rasura_content::font::{CodeUnit, LoadedFont};
use rasura_content::op::OpKind;
use rasura_content::tokenizer::tokenize;
use rasura_cos::object::Object;
use std::ops::Range;

/// Why a text operation could not be performed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TextError {
    /// No such paragraph on this page.
    #[error("no paragraph {0:?}")]
    NoParagraph(ParagraphId),

    /// The range selects glyphs drawn by more than one showing operator.
    ///
    /// Declined rather than approximated. Regenerating several operators means
    /// deciding what happens to the positioning between them, and a producer
    /// that split a line across operators usually did so *because* something
    /// sits in the gap — a colour change, a font change, a rise.
    #[error("the range spans {runs} showing operators; only one at a time is supported")]
    Fragmented { runs: usize },

    /// The text is drawn inside a form XObject.
    ///
    /// Its byte spans address the form's own content stream, not the page's,
    /// so a patch built from them would splice into the wrong buffer. Refused
    /// rather than translated: a form may be invoked from several pages, and
    /// editing it changes all of them.
    #[error("the text is inside a form XObject at depth {depth}; its spans are not the page's")]
    InsideForm { depth: usize },

    /// The font cannot write the requested text.
    #[error(transparent)]
    Unencodable(#[from] crate::encode::Unencodable),

    /// The result no longer fits, and the policy forbids overflowing.
    #[error(transparent)]
    Reflow(#[from] reflow::ReflowError),

    /// The operation is defined by the spec but not built yet.
    #[error("{0} is not implemented")]
    NotImplemented(&'static str),

    /// The page's own bytes did not parse back the way they were read.
    #[error("the operator at {0}..{1} could not be re-read")]
    Unreadable(usize, usize),
}

/// What an operation would do, before it does it.
#[derive(Debug, Clone)]
pub struct Edit {
    pub patches: Vec<Patch>,
    pub fidelity: Fidelity,
    /// The paragraph's text after the edit, so a caller can confirm.
    pub text_after: String,
}

/// Everything needed to write text in one run's font.
pub struct FontContext {
    pub font: LoadedFont,
    pub encoder: Encoder,
    pub size: f64,
}

impl FontContext {
    /// Build the context for the font a run uses.
    pub fn for_run(
        doc: &rasura_cos::Document,
        page: &EditablePage,
        run: usize,
    ) -> Option<FontContext> {
        let resolved = page.runs.get(run)?;
        let name = resolved.run.font_name.clone()?;
        let dict = font_dict(doc, page, &name)?;

        let font = LoadedFont::load_with(doc, &dict, Some(&rasura_layout::Standard14Widths));
        let decoder = rasura_layout::unicode::Decoder::build(doc, &dict, &font);

        // Every code the page draws in this font, so a composite font can write
        // back anything already on the page.
        let observed: Vec<u32> = page
            .runs
            .iter()
            .filter(|r| r.run.font_name.as_ref() == Some(&name))
            .flat_map(|r| r.run.glyphs.iter().map(|g| g.code))
            .collect();

        let encoder = Encoder::build(&font, &decoder, &observed);
        Some(FontContext { font, encoder, size: resolved.run.size })
    }

    /// Advance width of text at size 1, in text space.
    fn width_at_unit_size(&self, text: &str) -> Option<f64> {
        let codes = self.encoder.encode(text).ok()?;
        let mut total = 0.0;
        for unit in self.font.decode(&codes) {
            total += self.font.width(&unit)?;
        }
        Some(total)
    }
}

impl Measure for FontContext {
    fn width_of(&self, text: &str) -> Option<f64> {
        self.width_at_unit_size(text)
    }
}

/// The `/Font` resource dictionary a name refers to.
fn font_dict(
    doc: &rasura_cos::Document,
    page: &EditablePage,
    name: &rasura_cos::object::Name,
) -> Option<rasura_cos::object::Dictionary> {
    let pages = rasura_content::page::pages(doc).ok()?;
    let page = pages.pages.get(page.index)?;
    let resources = page.resources.as_ref()?.as_dict()?;
    let fonts = doc.get_entry(resources, "Font").ok()??;
    let fonts = fonts.as_dict()?;
    let entry = doc.get_entry(fonts, name.as_str()?).ok()??;
    entry.as_dict().cloned()
}

/// Replace a character range in a paragraph. Spec 9.2.
pub fn replace_text(
    doc: &rasura_cos::Document,
    page: &EditablePage,
    id: ParagraphId,
    range: Range<usize>,
    replacement: &str,
    policy: Policy,
) -> Result<Edit, TextError> {
    let selection = select(page, id, range.clone()).ok_or(TextError::NoParagraph(id))?;

    // An empty selection is an insertion, and an insertion still has to know
    // which run it lands in. The glyph *before* the point decides, because text
    // typed at a boundary belongs to the word it continues.
    let run = match selection.runs.as_slice() {
        [only] => *only,
        [] => run_at(page, id, range.start).ok_or(TextError::NoParagraph(id))?,
        many => return Err(TextError::Fragmented { runs: many.len() }),
    };

    let ctx = FontContext::for_run(doc, page, run).ok_or(TextError::NoParagraph(id))?;
    let resolved = page.runs.get(run).ok_or(TextError::NoParagraph(id))?;

    // A run drawn inside a form XObject has an `op_span` into the *form's*
    // content stream, while the patch would be applied against the *page's*.
    // The two are different buffers, so the span addresses different bytes --
    // and when the page stream is the longer of the two, the splice succeeds
    // and quietly rewrites something else.
    //
    // Refused rather than translated: a form may be invoked from several pages,
    // so editing its stream changes every one of them, and deciding that is the
    // caller's rather than this function's.
    if resolved.run.depth > 0 {
        return Err(TextError::InsideForm { depth: resolved.run.depth });
    }

    // The run's own text, with the selected glyphs replaced. Working in the
    // run's coordinates rather than the paragraph's keeps the edit local to one
    // operator even when the paragraph spans several.
    let (glyph_from, glyph_to) = run_glyph_range(&selection, run, page, id, range.start);
    let mut after = String::new();
    for (i, text) in resolved.text.iter().enumerate() {
        if i == glyph_from {
            after.push_str(replacement);
        }
        if i >= glyph_from && i < glyph_to {
            continue;
        }
        after.push_str(text.as_deref().unwrap_or(""));
    }
    if glyph_from >= resolved.text.len() {
        after.push_str(replacement);
    }

    let codes = ctx.encoder.encode(&after)?;

    // Does it still fit? A wrapped paragraph supplies its own measure; one that
    // never wrapped supplies none, and falls back to the page's visible edge --
    // a weaker bound, but a true one.
    let mut compromises = Vec::new();
    let lines = page.lines_of(id).unwrap_or_default();
    let measure =
        reflow::measure_of(lines).or_else(|| reflow::available_width(lines, page.crop_box.x1));
    let new_width = ctx.width_at_unit_size(&after).unwrap_or(0.0) * ctx.size;

    let mut broken = None;
    if let Some(measure) = measure.filter(|m| new_width > *m) {
        let out = reflow::reflow(&after, measure, ctx.size, lines.len(), policy, &ctx)?;
        if !out.same_shape {
            compromises
                .push(Compromise::LinesRebroken { before: out.before, after: out.lines.len() });
            // PDF has no flow. A paragraph that gained a line does not push the
            // block beneath it down -- that block is positioned absolutely and
            // will simply be overlapped. The caller asked for this by choosing
            // a policy other than `Refuse`, and is told what it bought.
            if out.lines.len() > out.before {
                compromises
                    .push(Compromise::Overflowed { lines_over: out.lines.len() - out.before });
            }
        }
        broken = Some(out);
    }

    // Regenerate the operator. Kerning the producer put in a `TJ` array is lost
    // and said so, which is spec §2's second property in its smallest form.
    let span = resolved.run.op_span.clone();
    let original =
        page.content.data().get(span.clone()).ok_or(TextError::Unreadable(span.start, span.end))?;
    if had_adjustments(original) {
        compromises.push(Compromise::KerningRegenerated);
    }

    // Spec 10.2. The bytes change and the page does not: no viewer draws a
    // layer the default configuration turns off. A caller given an exact result
    // and an unchanged-looking document concludes the library is broken, so the
    // one thing that explains it is said out loud.
    if let Some(region) = page.hidden_layer_at(span.start) {
        compromises.push(Compromise::EditedHiddenLayer { layer: region.layers.join(", ") });
    }

    let leading = page.paragraph(id).map(|p| p.leading).filter(|l| *l > 0.0).unwrap_or(ctx.size);
    let bytes = match broken.as_ref().filter(|out| out.lines.len() > 1) {
        Some(out) => render_lines(out, &ctx, leading, &page.style)?,
        None => render_show(&codes, &page.style),
    };

    let fidelity =
        if compromises.is_empty() { Fidelity::Exact } else { Fidelity::Degraded(compromises) };

    Ok(Edit { patches: vec![Patch::new(span, bytes)], fidelity, text_after: after })
}

/// Insert text at a character offset. Spec 9.2.
pub fn insert_text(
    doc: &rasura_cos::Document,
    page: &EditablePage,
    id: ParagraphId,
    at: usize,
    text: &str,
    policy: Policy,
) -> Result<Edit, TextError> {
    replace_text(doc, page, id, at..at, text, policy)
}

/// Delete a character range. Spec 9.2.
pub fn delete_range(
    doc: &rasura_cos::Document,
    page: &EditablePage,
    id: ParagraphId,
    range: Range<usize>,
    policy: Policy,
) -> Result<Edit, TextError> {
    replace_text(doc, page, id, range, "", policy)
}

/// Spec 9.2's remaining text operations, which need Phase 6's block geometry.
///
/// Named rather than absent: an API that silently lacks `set_alignment` looks
/// like an oversight, and one that returns `NotImplemented` says the shape is
/// known and the work is scheduled. Each of these moves *every* line of a
/// paragraph, which means computing new line origins against a block box —
/// geometry the edit layer does not own until Phase 6 adds block operations.
pub fn set_alignment(
    _id: ParagraphId,
    _alignment: rasura_layout::paragraphs::Alignment,
) -> Result<Edit, TextError> {
    Err(TextError::NotImplemented("set_alignment"))
}

pub fn set_leading(_id: ParagraphId, _leading: f64) -> Result<Edit, TextError> {
    Err(TextError::NotImplemented("set_leading"))
}

pub fn split_paragraph(_id: ParagraphId, _at: usize) -> Result<Edit, TextError> {
    Err(TextError::NotImplemented("split_paragraph"))
}

pub fn merge_paragraphs(_a: ParagraphId, _b: ParagraphId) -> Result<Edit, TextError> {
    Err(TextError::NotImplemented("merge_paragraphs"))
}

/// The run drawing the glyph at a character offset, for an empty selection.
fn run_at(page: &EditablePage, id: ParagraphId, at: usize) -> Option<usize> {
    // Look one character back, then one forward. An insertion point at the very
    // start of a paragraph has nothing behind it.
    for probe in [at.saturating_sub(1)..at, at..at + 1] {
        if let Some(sel) = select(page, id, probe)
            && let Some(first) = sel.runs.first()
        {
            return Some(*first);
        }
    }
    None
}

/// The selected glyphs' index range within their run.
fn run_glyph_range(
    selection: &Selection,
    run: usize,
    page: &EditablePage,
    id: ParagraphId,
    at: usize,
) -> (usize, usize) {
    let indices: Vec<usize> =
        selection.glyphs.iter().filter(|g| g.run == run).map(|g| g.index).collect();
    match (indices.iter().min(), indices.iter().max()) {
        (Some(lo), Some(hi)) => (*lo, *hi + 1),
        // An insertion: the point sits after the glyph before it.
        _ => {
            let before = select(page, id, at.saturating_sub(1)..at)
                .and_then(|s| s.glyphs.iter().find(|g| g.run == run).map(|g| g.index + 1))
                .unwrap_or(0);
            (before, before)
        }
    }
}

/// Whether an operator's bytes carry non-zero `TJ` adjustments.
///
/// Read from the bytes rather than from the glyph positions, because a `TJ`
/// whose adjustments happen to sum to zero still positioned its glyphs with
/// them, and regenerating it as a `Tj` would move them.
fn had_adjustments(operator: &[u8]) -> bool {
    let (ops, _) = tokenize(operator);
    ops.iter().filter(|op| op.kind == OpKind::ShowTextAdjusted).any(|op| {
        op.operands.first().and_then(Object::as_array).is_some_and(|items| {
            items.iter().filter_map(Object::as_f64).any(|v| v.abs() > f64::EPSILON)
        })
    })
}

/// A showing operator for a run of codes.
fn render_show(codes: &[u8], style: &NumberStyle) -> Vec<u8> {
    let mut out = Vec::new();
    crate::emit::write_op(&mut out, &crate::emit::show_text(codes), style);
    out
}

/// Several lines, positioned relative to the line this operator started on.
///
/// `Td` moves the *line* matrix, which the preceding `Tm` established and which
/// a showing operator does not disturb. So each subsequent line is one leading
/// below the last, starting at the same x — exactly what a producer writes.
///
/// # The restoring move at the end
///
/// `Td` is cumulative. Leaving the line matrix `n × leading` lower than it was
/// would shift every *following* operator in the same text object that
/// positions itself relatively — and a producer that used `Td` or `T*` for its
/// next paragraph did exactly that. So the last thing written is a `Td` putting
/// the line matrix back where it was found.
///
/// The net effect on anything outside this operator is therefore nil, which is
/// what lets a paragraph gain a line without disturbing the rest of the stream.
/// It also means the new lines *overlap* whatever is below rather than pushing
/// it down — see [`Compromise::Overflowed`], which is how the caller is told.
fn render_lines(
    out: &reflow::Reflowed,
    ctx: &FontContext,
    leading: f64,
    style: &NumberStyle,
) -> Result<Vec<u8>, TextError> {
    let mut bytes = Vec::new();
    let mut moved = 0.0f64;

    for (i, line) in out.lines.iter().enumerate() {
        if i > 0 {
            crate::emit::write_op(&mut bytes, &crate::emit::text_move(0.0, -leading), style);
            bytes.push(b' ');
            moved += leading;
        }
        let codes = ctx.encoder.encode(&line.text)?;
        crate::emit::write_op(&mut bytes, &crate::emit::show_text(&codes), style);
        if i + 1 < out.lines.len() {
            bytes.push(b' ');
        }
    }

    if moved != 0.0 {
        bytes.push(b' ');
        crate::emit::write_op(&mut bytes, &crate::emit::text_move(0.0, moved), style);
    }
    Ok(bytes)
}

/// The advance width of a run's glyphs as the file positioned them.
///
/// Used to tell whether an edit changed a line's extent at all — an edit that
/// did not cannot have moved anything after it, whatever the fit check says.
pub fn run_width(font: &LoadedFont, codes: &[u8]) -> Option<f64> {
    let mut total = 0.0;
    for unit in font.decode(codes) {
        total += font.width(&unit)?;
    }
    Some(total)
}

/// The width of one code unit, for callers measuring a partial run.
pub fn code_width(font: &LoadedFont, code: u32) -> Option<f64> {
    font.width(&CodeUnit { code, cid: font.cmap.cid(code), offset: 0, len: 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::EditSession;
    use rasura_cos::testutil::ClassicBuilder;
    use rasura_cos::{Document, SaveOptions};

    fn page_bytes(content: &[u8]) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    fn simple() -> Vec<u8> {
        page_bytes(b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello world) Tj ET\n")
    }

    fn analysed(bytes: Vec<u8>) -> (Document, EditablePage) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");
        (doc, page)
    }

    fn allow() -> Policy {
        Policy { breaking: reflow::Breaking::Greedy, overflow: reflow::Overflow::Allow }
    }

    /// Apply an edit, save, reopen, and read the text back.
    ///
    /// The oracle is the **layout** layer, not `page_text`. A WinAnsi Helvetica
    /// carries no `/ToUnicode`, so the content layer alone -- which implements
    /// §7.2 strategy 1 and nothing else -- correctly reports that it does not
    /// know. Reading through the full derivation chain is both the honest
    /// check and the one a consumer actually performs.
    fn apply_and_reread(mut doc: Document, page: EditablePage, edit: Edit) -> String {
        let mut session = EditSession::new(&mut doc);
        session.patch_content("test", &page.content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let reopened = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&reopened).expect("pages");
        let after = EditablePage::analyse(&reopened, &pages.pages[0]).expect("re-analyse");
        after.paragraphs.iter().map(|(id, _)| after.text_of(*id)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn replacing_a_word_changes_that_word_and_nothing_else() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        let at = text.find("world").expect("found");

        let edit = replace_text(&doc, &page, id, at..at + 5, "there", allow()).expect("replace");
        assert!(edit.fidelity.is_exact(), "{:?}", edit.fidelity);
        assert_eq!(edit.text_after, "Hello there");

        let after = apply_and_reread(doc, page, edit);
        assert!(after.contains("Hello there"), "{after:?}");
    }

    #[test]
    fn the_edit_reads_back_through_an_independent_extraction() {
        // The end-to-end claim. Not "the bytes we wrote are the bytes we
        // intended" -- that is circular -- but "the reader gets the text back",
        // through the same seven-strategy chain any consumer uses.
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let edit = replace_text(&doc, &page, id, 0..5, "Howdy", allow()).expect("replace");
        let after = apply_and_reread(doc, page, edit);
        assert!(after.contains("Howdy world"), "{after:?}");
    }

    #[test]
    fn inserting_text_puts_it_where_asked() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let edit = insert_text(&doc, &page, id, 5, ",", allow()).expect("insert");
        assert_eq!(edit.text_after, "Hello, world");

        let after = apply_and_reread(doc, page, edit);
        assert!(after.contains("Hello, world"), "{after:?}");
    }

    #[test]
    fn inserting_at_the_start_works_with_nothing_behind_it() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let edit = insert_text(&doc, &page, id, 0, ">> ", allow()).expect("insert");
        assert_eq!(edit.text_after, ">> Hello world");
    }

    #[test]
    fn deleting_a_range_removes_exactly_it() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        let at = text.find(" world").expect("found");

        let edit = delete_range(&doc, &page, id, at..at + 6, allow()).expect("delete");
        assert_eq!(edit.text_after, "Hello");

        let after = apply_and_reread(doc, page, edit);
        assert!(after.contains("Hello"), "{after:?}");
        assert!(!after.contains("world"), "{after:?}");
    }

    #[test]
    fn text_the_font_cannot_write_is_refused_before_anything_moves() {
        let (mut doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;

        let err = replace_text(&doc, &page, id, 0..5, "\u{4e00}\u{4e8c}", allow())
            .expect_err("Helvetica has no CJK");
        assert!(matches!(err, TextError::Unencodable(_)), "{err:?}");

        // And the refusal really did happen before any bytes were staged.
        let session = EditSession::new(&mut doc);
        assert!(!session.document().is_dirty());
    }

    #[test]
    fn losing_producer_kerning_is_reported_not_hidden() {
        // A TJ array's adjustments are the producer's positioning decisions.
        // Regenerating the operator as a plain Tj discards them, which is
        // acceptable -- but only if the caller is told.
        let (doc, page) = analysed(page_bytes(
            b"BT /F1 12 Tf 1 0 0 1 72 700 Tm [(He)-40(llo)-25( world)]TJ ET\n",
        ));
        let id = page.paragraphs[0].0;
        let edit = replace_text(&doc, &page, id, 0..5, "Howdy", allow()).expect("replace");

        match &edit.fidelity {
            Fidelity::Degraded(list) => {
                assert!(list.contains(&Compromise::KerningRegenerated), "{list:?}");
            }
            other => panic!("expected a reported compromise, got {other:?}"),
        }
    }

    /// A page with one word inside a layer that is turned off and one outside.
    fn layered_page() -> Vec<u8> {
        let content = b"/OC /L1 BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (hidden text) Tj ET EMC\n\
                        BT /F1 12 Tf 1 0 0 1 72 600 Tm (visible text) Tj ET\n";
        ClassicBuilder::new()
            .object(
                1,
                "<< /Type /Catalog /Pages 2 0 R /OCProperties \
                 << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>",
            )
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /Properties << /L1 6 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(6, "<< /Type /OCG /Name (Draft stamp) >>")
            .finish("/Root 1 0 R")
    }

    #[test]
    fn editing_a_hidden_layer_succeeds_and_says_the_page_will_not_change() {
        // Spec 10.2. The edit is real and the bytes change; the page does not,
        // because no viewer draws that layer. A caller handed an exact result
        // and an unchanged-looking document concludes we are broken.
        let (doc, page) = analysed(layered_page());
        let hidden = page
            .paragraphs
            .iter()
            .find(|(id, _)| page.text_of(*id).contains("hidden"))
            .map(|(id, _)| *id)
            .expect("the hidden paragraph is still extracted");

        let edit = replace_text(&doc, &page, hidden, 0..6, "shown!", allow()).expect("replace");
        match &edit.fidelity {
            Fidelity::Degraded(list) => assert!(
                list.iter().any(|c| matches!(
                    c,
                    Compromise::EditedHiddenLayer { layer } if layer == "Draft stamp"
                )),
                "{list:?}"
            ),
            other => panic!("expected the hidden-layer compromise, got {other:?}"),
        }
    }

    #[test]
    fn editing_visible_content_on_a_layered_page_reports_nothing() {
        // Otherwise every edit to any document with layers carries a warning it
        // did not earn, and a report that cries wolf stops being read.
        let (doc, page) = analysed(layered_page());
        let visible = page
            .paragraphs
            .iter()
            .find(|(id, _)| page.text_of(*id).contains("visible"))
            .map(|(id, _)| *id)
            .expect("the visible paragraph");

        let edit = replace_text(&doc, &page, visible, 0..7, "showing", allow()).expect("replace");
        assert!(edit.fidelity.is_exact(), "{:?}", edit.fidelity);
    }

    #[test]
    fn a_tj_without_real_adjustments_is_not_reported_as_lost_kerning() {
        // Otherwise every edit to a TJ-using producer reports a compromise it
        // did not make, and a report that cries wolf stops being read.
        let (doc, page) =
            analysed(page_bytes(b"BT /F1 12 Tf 1 0 0 1 72 700 Tm [(Hello world)]TJ ET\n"));
        let id = page.paragraphs[0].0;
        let edit = replace_text(&doc, &page, id, 0..5, "Howdy", allow()).expect("replace");
        assert!(edit.fidelity.is_exact(), "{:?}", edit.fidelity);
    }

    #[test]
    fn an_edit_spanning_two_operators_is_declined_by_name() {
        // Regenerating both would have to decide what happens to whatever the
        // producer put between them, and it put something there for a reason.
        // Two operators, set flush so they read as one line: "Hello " is about
        // 30.7 units at 12pt, so `world` continues at x = 103.
        let (doc, page) = analysed(page_bytes(
            b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello ) Tj 1 0 0 1 103 700 Tm (world) Tj ET\n",
        ));
        let id = page.paragraphs[0].0;
        let text = page.text_of(id);
        let err = replace_text(&doc, &page, id, 0..text.chars().count(), "x", allow())
            .expect_err("two operators");
        assert!(matches!(err, TextError::Fragmented { runs: 2 }), "{err:?}");
    }

    #[test]
    fn growing_text_past_the_measure_refuses_under_the_default_policy() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        // Long enough to actually pass the page edge: the line starts at x=72
        // and the crop box ends at 612, so it has 540 units, and this is
        // roughly 900 at 12pt.
        let long = "Hello world and a great deal more text than ever fitted on this line \
                    before, long enough that it must run past the right hand edge of the \
                    page no matter how generously one measures Helvetica at twelve point";
        let err =
            replace_text(&doc, &page, id, 0..11, long, Policy::default()).expect_err("overflows");
        assert!(matches!(err, TextError::Reflow(reflow::ReflowError::Overflow { .. })), "{err:?}");
    }

    #[test]
    fn growing_text_reports_the_rebreak_when_overflow_is_allowed() {
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        // Long enough to actually pass the page edge: the line starts at x=72
        // and the crop box ends at 612, so it has 540 units, and this is
        // roughly 900 at 12pt.
        let long = "Hello world and a great deal more text than ever fitted on this line \
                    before, long enough that it must run past the right hand edge of the \
                    page no matter how generously one measures Helvetica at twelve point";
        let edit = replace_text(&doc, &page, id, 0..11, long, allow()).expect("allowed");

        match &edit.fidelity {
            Fidelity::Degraded(list) => {
                assert!(
                    list.iter().any(|c| matches!(c, Compromise::LinesRebroken { .. })),
                    "{list:?}"
                );
            }
            other => panic!("expected a rebreak report, got {other:?}"),
        }
    }

    #[test]
    fn a_rebroken_paragraph_actually_emits_the_extra_lines() {
        // The bug this pins: reflow computed the new break points and the
        // emitter threw them away, writing one long line while *reporting*
        // `LinesRebroken`. Every structural check passed and the page had text
        // running off the right edge.
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let long = "Hello world and a great deal more text than ever fitted on this line \
                    before, long enough that it must run past the right hand edge of the \
                    page no matter how generously one measures Helvetica at twelve point";
        let edit = replace_text(&doc, &page, id, 0..11, long, allow()).expect("allowed");

        let written = String::from_utf8_lossy(&edit.patches[0].bytes).to_string();
        let shows = written.matches("Tj").count();
        assert!(shows >= 2, "more than one line was written: {written}");
        assert!(written.contains("Td"), "the lines are positioned: {written}");
    }

    #[test]
    fn the_extra_lines_leave_the_line_matrix_where_they_found_it() {
        // `Td` is cumulative, so lines added inside one operator would shift
        // every following operator that positions itself relatively. The
        // restoring move is what keeps the edit local; without it, a paragraph
        // gaining a line drags the rest of the text object down with it.
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let long = "Hello world and a great deal more text than ever fitted on this line \
                    before, long enough that it must run past the right hand edge of the \
                    page no matter how generously one measures Helvetica at twelve point";
        let edit = replace_text(&doc, &page, id, 0..11, long, allow()).expect("allowed");

        let written = String::from_utf8_lossy(&edit.patches[0].bytes).to_string();
        let net: f64 = written
            .split(" Td")
            .filter_map(|chunk| chunk.rsplit_once(' ').map(|(_, y)| y))
            .filter_map(|y| y.parse::<f64>().ok())
            .sum();
        assert!(net.abs() < 1e-6, "the vertical moves cancel, net {net}: {written}");
    }

    #[test]
    fn a_paragraph_that_gained_a_line_says_it_will_overlap_what_is_below() {
        // PDF has no flow: the block beneath is positioned absolutely and does
        // not move out of the way. That is not something this layer can fix,
        // so it is something it has to report.
        let (doc, page) = analysed(simple());
        let id = page.paragraphs[0].0;
        let long = "Hello world and a great deal more text than ever fitted on this line \
                    before, long enough that it must run past the right hand edge of the \
                    page no matter how generously one measures Helvetica at twelve point";
        let edit = replace_text(&doc, &page, id, 0..11, long, allow()).expect("allowed");

        match &edit.fidelity {
            Fidelity::Degraded(list) => assert!(
                list.iter().any(|c| matches!(c, Compromise::Overflowed { .. })),
                "the overlap is reported: {list:?}"
            ),
            other => panic!("expected a reported overflow, got {other:?}"),
        }
    }

    #[test]
    fn an_edit_touches_only_the_operator_it_had_to() {
        // Spec 2, property 1, at this layer: everything outside the rewritten
        // operator is byte-identical.
        let original = simple();
        let (doc, page) = analysed(original.clone());
        let id = page.paragraphs[0].0;
        let edit = replace_text(&doc, &page, id, 0..5, "Howdy", allow()).expect("replace");

        assert_eq!(edit.patches.len(), 1, "one operator, one patch");
        let span = edit.patches[0].span.clone();
        let before = page.content.data();
        let spliced = crate::splice(before, &edit.patches).expect("splice");

        assert_eq!(&spliced.bytes[..span.start], &before[..span.start]);
        let tail_from = spliced.remap(span.end).expect("the end of the patch survives");
        assert_eq!(&spliced.bytes[tail_from..], &before[span.end..]);
    }

    #[test]
    fn the_unimplemented_operations_say_which_they_are() {
        let id = ParagraphId { region: 0, index: 0 };
        for err in [
            set_alignment(id, rasura_layout::paragraphs::Alignment::Left).unwrap_err(),
            set_leading(id, 14.0).unwrap_err(),
            split_paragraph(id, 3).unwrap_err(),
            merge_paragraphs(id, id).unwrap_err(),
        ] {
            assert!(matches!(err, TextError::NotImplemented(_)), "{err:?}");
        }
    }
}
