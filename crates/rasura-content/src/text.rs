//! Text extraction with positions. Phase 2's exit criterion.
//!
//! Turns text-showing operators into positioned glyphs: for each character
//! code, where it lands in device space and how far the pen moved afterwards.
//!
//! # What this is and is not
//!
//! This is *geometry*. Each glyph carries its code, its CID, its advance and its
//! device-space origin, and that is enough to reconstruct lines and blocks.
//!
//! It is not *reconstruction*. Unicode comes only from `/ToUnicode` -- §7.2
//! strategy 1 -- and when that is missing the glyph says so rather than
//! guessing. The remaining six strategies, word segmentation, line assembly and
//! paragraph inference are `rasura-layout`, Phase 3.

use crate::font::{CodeUnit, LoadedFont};
use crate::matrix::{Matrix, Point};
use crate::op::{Op, OpKind};
use crate::page::Page;
use crate::state::{Colour, StateMachine, word_spacing_applies};
use crate::walker::{ContentVisitor, Flow, WalkContext, WalkReport, walk_page};
use rasura_cos::document::Document;
use rasura_cos::{Name, ObjId, Object};
use std::collections::HashMap;

/// One glyph, positioned.
#[derive(Debug, Clone)]
pub struct PositionedGlyph {
    pub code: u32,
    pub cid: u32,
    /// From `/ToUnicode` only. `None` means "this layer does not know", not
    /// "there is no text".
    pub unicode: Option<String>,
    /// Advance in text space, before the text matrix.
    pub advance: f64,
    /// Origin in device space, after the full text rendering matrix.
    pub origin: Point,
    /// Byte range within the showing operator's string operand.
    pub span: std::ops::Range<usize>,
    /// True when the font supplied no width for this glyph, so `advance` is a
    /// fallback rather than a measurement.
    pub width_missing: bool,
}

/// A maximal sequence of glyphs sharing one font, size and text state.
#[derive(Debug, Clone)]
pub struct GlyphRun {
    /// The `/Font` resource name given to `Tf`.
    pub font_name: Option<Name>,
    pub base_font: String,
    pub size: f64,
    /// `Tz`, as a percentage. Carried because the text rendering matrix folds
    /// it in with the font size, and a consumer converting text-space advances
    /// to device space needs to divide both out again.
    pub horizontal_scale: f64,
    /// Text rendering matrix at the run's first glyph.
    pub trm: Matrix,
    /// Fill colour in force. Part of a style run's identity (spec 7.6), and the
    /// rendering matrix cannot carry it.
    pub fill: Colour,
    /// `Ts`. Folded into the rendering matrix, but needed on its own to tell a
    /// superscript from a small line.
    pub rise: f64,
    pub glyphs: Vec<PositionedGlyph>,
    /// Byte range of the operator that produced this run, in the logical buffer.
    pub op_span: std::ops::Range<usize>,
    /// Enclosing marked-content id, when the document is tagged.
    pub mcid: Option<u32>,
    pub render_mode: i64,
    /// Which content stream the operator came from.
    pub source: Option<ObjId>,
    /// Form XObject nesting depth.
    pub depth: usize,
    pub vertical: bool,
}

impl GlyphRun {
    /// The text this run carries, as far as `/ToUnicode` goes. Glyphs with no
    /// mapping contribute nothing, so an empty string from a non-empty run
    /// means the mapping is missing, not that the run is blank.
    pub fn text(&self) -> String {
        self.glyphs.iter().filter_map(|g| g.unicode.as_deref()).collect()
    }

    /// How many glyphs have no Unicode mapping.
    pub fn unmapped(&self) -> usize {
        self.glyphs.iter().filter(|g| g.unicode.is_none()).count()
    }
}

/// Extraction diagnostics, so a caller can tell "no text" from "text we could
/// not read".
#[derive(Debug, Clone, Default)]
pub struct TextReport {
    pub glyphs: usize,
    pub unmapped_glyphs: usize,
    pub glyphs_without_widths: usize,
    /// Font resource names that `/Font` did not define.
    pub missing_fonts: Vec<String>,
    /// Fonts whose `/Encoding` CMap had to be approximated.
    pub approximate_cmaps: Vec<String>,
}

/// A `ContentVisitor` that accumulates glyph runs.
#[derive(Default)]
pub struct TextExtractor<'a> {
    runs: Vec<GlyphRun>,
    report: TextReport,
    /// Consulted for fonts whose dictionary carries no `/Widths`. See
    /// [`WidthSource`](crate::font::WidthSource).
    widths: Option<&'a dyn crate::font::WidthSource>,
    /// Loaded fonts, keyed by resource name plus scope depth so a form that
    /// redefines a name gets its own entry.
    cache: HashMap<(usize, Vec<u8>), Option<LoadedFont>>,
    /// Open `BDC` marked-content ids.
    mcid_stack: Vec<Option<u32>>,
}

impl<'a> TextExtractor<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// An extractor that can fall back to supplied metrics.
    pub fn with_widths(widths: &'a dyn crate::font::WidthSource) -> Self {
        TextExtractor { widths: Some(widths), ..Default::default() }
    }

    pub fn finish(self) -> (Vec<GlyphRun>, TextReport) {
        (self.runs, self.report)
    }

    fn current_mcid(&self) -> Option<u32> {
        self.mcid_stack.iter().rev().find_map(|m| *m)
    }

    fn font_for(
        &mut self,
        doc: &Document,
        ctx: &WalkContext<'_>,
        name: &Name,
    ) -> Option<&LoadedFont> {
        let key = (ctx.resources.depth(), name.as_bytes().to_vec());
        if !self.cache.contains_key(&key) {
            let loaded = ctx
                .resources
                .font(doc, name)
                .and_then(|obj| obj.as_dict().map(|d| LoadedFont::load_with(doc, d, self.widths)));
            match &loaded {
                None => {
                    let n = String::from_utf8_lossy(name.as_bytes()).into_owned();
                    if !self.report.missing_fonts.contains(&n) {
                        self.report.missing_fonts.push(n);
                    }
                }
                Some(f)
                    if f.approximate_cmap
                        && !self.report.approximate_cmaps.contains(&f.base_font) =>
                {
                    self.report.approximate_cmaps.push(f.base_font.clone());
                }
                _ => {}
            }
            self.cache.insert(key.clone(), loaded);
        }
        self.cache.get(&key).and_then(|f| f.as_ref())
    }

    /// Show one string, advancing the text matrix glyph by glyph.
    #[allow(clippy::too_many_arguments)]
    fn show(
        &mut self,
        bytes: &[u8],
        base_offset: usize,
        state: &mut StateMachine,
        font_name: Option<Name>,
        glyphs: &mut Vec<PositionedGlyph>,
        doc: &Document,
        ctx: &WalkContext<'_>,
    ) {
        let Some(name) = font_name else { return };
        // Copy what is needed out of the borrow before mutating `self`.
        let Some((units, widths, unicodes, vertical)) = self.font_for(doc, ctx, &name).map(|f| {
            let units: Vec<CodeUnit> = f.decode(bytes);
            let widths: Vec<Option<f64>> = units.iter().map(|u| f.width(u)).collect();
            let unicodes: Vec<Option<String>> =
                units.iter().map(|u| f.unicode(u).map(|s| s.to_string())).collect();
            (units, widths, unicodes, f.is_vertical())
        }) else {
            return;
        };

        for ((unit, width), unicode) in units.iter().zip(widths).zip(unicodes) {
            // The origin is the pen position *before* this glyph advances.
            let trm = state.text_rendering_matrix();
            let origin = trm.apply(Point::new(0.0, 0.0));

            let applies = word_spacing_applies(unit.code, unit.len);
            // A missing width would place every glyph on top of the last. Half
            // an em is the conventional stand-in, and `width_missing` says the
            // position is a fallback rather than a measurement.
            let w = width.unwrap_or(0.5);
            let advance = if vertical {
                state.text().displacement_vertical(w, 0.0, applies)
            } else {
                state.text().displacement(w, 0.0, applies)
            };

            if width.is_none() {
                self.report.glyphs_without_widths += 1;
            }
            if unicode.is_none() {
                self.report.unmapped_glyphs += 1;
            }
            self.report.glyphs += 1;

            glyphs.push(PositionedGlyph {
                code: unit.code,
                cid: unit.cid,
                unicode,
                advance,
                origin,
                span: base_offset + unit.offset..base_offset + unit.offset + unit.len,
                width_missing: width.is_none(),
            });

            if vertical {
                state.advance_text(0.0, -advance);
            } else {
                state.advance_text(advance, 0.0);
            }
        }
    }
}

impl ContentVisitor for TextExtractor<'_> {
    fn visit(&mut self, op: &Op, state: &mut StateMachine, ctx: &WalkContext<'_>) -> Flow {
        match op.kind {
            OpKind::BeginMarked => self.mcid_stack.push(None),
            OpKind::BeginMarkedProps => {
                let mcid = op
                    .operands
                    .get(1)
                    .and_then(Object::as_dict)
                    .and_then(|d| d.get("MCID"))
                    .and_then(Object::as_i64)
                    .and_then(|v| u32::try_from(v).ok());
                self.mcid_stack.push(mcid);
            }
            OpKind::EndMarked => {
                self.mcid_stack.pop();
            }
            _ => {}
        }

        if !op.kind.shows_text() {
            return Flow::Continue;
        }

        let font_name = state.text().font.clone();
        let size = state.text().font_size;
        let horizontal_scale = state.text().horizontal_scale;
        let render_mode = state.text().render_mode;
        let rise = state.text().rise;
        let fill = state.state().fill_colour.clone();
        let trm = state.text_rendering_matrix();
        let mut glyphs = Vec::new();
        let doc = ctx.doc;

        // Load the font once up front: the adjustment arithmetic needs to know
        // the writing mode, and the run needs the base font name.
        let (base_font, vertical) = match font_name.as_ref() {
            Some(n) => self
                .font_for(doc, ctx, n)
                .map(|f| (f.base_font.clone(), f.is_vertical()))
                .unwrap_or_default(),
            None => (String::new(), false),
        };

        match op.kind {
            OpKind::ShowText | OpKind::NextLineShowText | OpKind::NextLineSetSpacingShowText => {
                if let Some(s) = op.operands.last().and_then(Object::as_string) {
                    let bytes = s.as_bytes().to_vec();
                    self.show(&bytes, 0, state, font_name.clone(), &mut glyphs, doc, ctx);
                }
            }
            OpKind::ShowTextAdjusted => {
                let Some(items) = op.operands.last().and_then(Object::as_array) else {
                    return Flow::Continue;
                };
                let items = items.to_vec();
                let mut offset = 0usize;
                for item in &items {
                    match item {
                        Object::String(s) => {
                            let bytes = s.as_bytes().to_vec();
                            let len = bytes.len();
                            self.show(
                                &bytes,
                                offset,
                                state,
                                font_name.clone(),
                                &mut glyphs,
                                doc,
                                ctx,
                            );
                            offset += len;
                        }
                        other => {
                            // A number moves the pen. Neither `Tc` nor `Tw`
                            // applies to a bare adjustment -- only the font size
                            // and, horizontally, the horizontal scale.
                            if let Some(adj) = other.as_f64() {
                                let t = state.text();
                                let d = -adj / 1000.0 * t.font_size;
                                if vertical {
                                    state.advance_text(0.0, -d);
                                } else {
                                    state.advance_text(d * (t.horizontal_scale / 100.0), 0.0);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if !glyphs.is_empty() {
            self.runs.push(GlyphRun {
                font_name,
                base_font,
                size,
                horizontal_scale,
                trm,
                fill,
                rise,
                glyphs,
                op_span: op.span.clone(),
                mcid: self.current_mcid(),
                render_mode,
                source: ctx.content.source_of(op.span.start),
                depth: ctx.depth,
                vertical,
            });
        }
        Flow::Continue
    }
}

/// Extract every glyph run on a page, in content order.
pub fn extract_page(doc: &Document, page: &Page) -> (Vec<GlyphRun>, TextReport, WalkReport) {
    let mut extractor = TextExtractor::new();
    let report = walk_page(doc, page, &mut extractor);
    let (runs, text_report) = extractor.finish();
    (runs, text_report, report)
}

/// As [`extract_page`], with metrics supplied for fonts that carry none.
///
/// Kept separate rather than made the default: this layer has no way to obtain
/// a [`WidthSource`](crate::font::WidthSource) — the metrics live above it —
/// so the caller that has one passes it in.
pub fn extract_page_with(
    doc: &Document,
    page: &Page,
    widths: &dyn crate::font::WidthSource,
) -> (Vec<GlyphRun>, TextReport, WalkReport) {
    let mut extractor = TextExtractor::with_widths(widths);
    let report = walk_page(doc, page, &mut extractor);
    let (runs, text_report) = extractor.finish();
    (runs, text_report, report)
}

/// The text of a whole page, in content order, from `/ToUnicode` only.
///
/// Content order is not reading order -- that is §7.5's job, in Phase 3. For a
/// single-column document they usually coincide, which is why this is useful
/// now and not sufficient later.
pub fn page_text(doc: &Document, page: &Page) -> String {
    let (runs, _, _) = extract_page(doc, page);
    let mut out = String::new();
    for run in &runs {
        out.push_str(&run.text());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page;
    use rasura_cos::testutil::ClassicBuilder;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A page using a simple font with known widths, so positions are exact.
    fn simple_page(content: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(
                5,
                // Every glyph 500/1000 wide, codes 32..122, with a /ToUnicode.
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 \
                  /LastChar 122 /Widths [500 500 500 500 500 500 500 500 500 500 500 500 500 \
                  500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 \
                  500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 \
                  500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 \
                  500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500 500] \
                  /ToUnicode 6 0 R >>",
            )
            .stream(
                6,
                "",
                b"1 begincodespacerange\n<00> <ff>\nendcodespacerange\n\
                             1 beginbfrange\n<20> <7a> <0020>\nendbfrange",
            )
            .finish("/Root 1 0 R")
    }

    fn extract(bytes: Vec<u8>) -> (Vec<GlyphRun>, TextReport) {
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, tr, _) = extract_page(&doc, &p);
        (runs, tr)
    }

    #[test]
    fn extracts_text_and_positions() {
        let (runs, report) = extract(simple_page("BT /F1 10 Tf 72 700 Td (AB) Tj ET"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "AB");
        assert_eq!(report.glyphs, 2);
        assert_eq!(report.unmapped_glyphs, 0);
        assert_eq!(report.glyphs_without_widths, 0);

        // Page is 800 tall and the base CTM flips y, so user y=700 is device 100.
        let g = &runs[0].glyphs;
        assert!(close(g[0].origin.x, 72.0), "{:?}", g[0].origin);
        assert!(close(g[0].origin.y, 100.0), "{:?}", g[0].origin);
        // 500/1000 * 10pt = 5pt advance.
        assert!(close(g[0].advance, 5.0));
        assert!(close(g[1].origin.x, 77.0), "{:?}", g[1].origin);
    }

    #[test]
    fn char_spacing_and_horizontal_scale_affect_the_advance() {
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 2 Tc 200 Tz 0 0 Td (AB) Tj ET"));
        let g = &runs[0].glyphs;
        // ((0.5 * 10) + 2) * 2 = 14
        assert!(close(g[0].advance, 14.0), "{}", g[0].advance);
        assert!(close(g[1].origin.x - g[0].origin.x, 14.0));
    }

    #[test]
    fn word_spacing_applies_to_a_simple_font_space() {
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 5 Tw 0 0 Td (A B) Tj ET"));
        let g = &runs[0].glyphs;
        // 'A' then ' ' (which gets Tw) then 'B'.
        assert!(close(g[1].origin.x - g[0].origin.x, 5.0), "no Tw on 'A'");
        assert!(close(g[2].origin.x - g[1].origin.x, 10.0), "Tw applies to the space");
    }

    #[test]
    fn tj_adjustments_move_the_pen_without_char_or_word_spacing() {
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 3 Tc 7 Tw 0 0 Td [(A) -1000 (B)] TJ ET"));
        let g = &runs[0].glyphs;
        // 'A' advances (0.5*10 + 3) = 8, then the adjustment adds 1000/1000*10
        // = 10 with neither Tc nor Tw applied.
        assert!(close(g[1].origin.x - g[0].origin.x, 18.0), "{}", g[1].origin.x - g[0].origin.x);
    }

    #[test]
    fn a_tj_array_is_one_run_with_contiguous_spans() {
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 0 0 Td [(AB) -200 (CD)] TJ ET"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "ABCD");
        let spans: Vec<_> = runs[0].glyphs.iter().map(|g| g.span.clone()).collect();
        assert_eq!(spans, vec![0..1, 1..2, 2..3, 3..4]);
    }

    #[test]
    fn quote_operators_move_to_the_next_line_before_showing() {
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 20 TL 50 700 Td (A) ' (B) '"));
        assert_eq!(runs.len(), 2);
        let a = runs[0].glyphs[0].origin;
        let b = runs[1].glyphs[0].origin;
        assert!(close(a.x, 50.0) && close(b.x, 50.0));
        // Each ' drops one leading; device y increases downwards.
        assert!(close(b.y - a.y, 20.0), "{a:?} {b:?}");
    }

    #[test]
    fn text_inside_a_form_is_positioned_by_the_form_matrix() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /XObject << /Fm 7 0 R >> /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"/Fm Do")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 \
                  /LastChar 65 /Widths [500] >>",
            )
            .stream(
                7,
                "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Matrix [1 0 0 1 100 200]",
                b"BT /F1 10 Tf 0 0 Td (A) Tj ET",
            )
            .finish("/Root 1 0 R");
        let (runs, _) = extract(bytes);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].depth, 1);
        let o = runs[0].glyphs[0].origin;
        assert!(close(o.x, 100.0) && close(o.y, 600.0), "{o:?}");
    }

    #[test]
    fn composite_font_glyphs_use_two_byte_codes() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 0 0 Td <00410042> Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H \
                  /DescendantFonts [6 0 R] >>",
            )
            .object(6, "<< /Type /Font /Subtype /CIDFontType2 /DW 1000 >>")
            .finish("/Root 1 0 R");
        let (runs, report) = extract(bytes);
        assert_eq!(report.glyphs, 2, "four bytes are two two-byte codes");
        assert_eq!(runs[0].glyphs[0].cid, 0x41);
        assert_eq!(runs[0].glyphs[1].cid, 0x42);
        // /DW 1000 at 10pt.
        assert!(close(runs[0].glyphs[1].origin.x - runs[0].glyphs[0].origin.x, 10.0));
        assert_eq!(runs[0].glyphs[0].span, 0..2);
    }

    #[test]
    fn word_spacing_is_not_applied_to_a_two_byte_code_32() {
        // The rule spec 6.3 warns about, end to end.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 50 Tw 0 0 Td <00200020> Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
            )
            .object(6, "<< /Type /Font /Subtype /CIDFontType2 /DW 1000 >>")
            .finish("/Root 1 0 R");
        let (runs, _) = extract(bytes);
        let g = &runs[0].glyphs;
        assert!(
            close(g[1].origin.x - g[0].origin.x, 10.0),
            "Tw must not apply: got {}",
            g[1].origin.x - g[0].origin.x
        );
    }

    #[test]
    fn missing_widths_are_reported_rather_than_silently_zero() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 0 0 Td (AB) Tj ET")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .finish("/Root 1 0 R");
        let (runs, report) = extract(bytes);
        assert_eq!(report.glyphs_without_widths, 2);
        assert!(runs[0].glyphs.iter().all(|g| g.width_missing));
        // Glyphs must still be separated, not stacked at the same x.
        assert!(runs[0].glyphs[1].origin.x > runs[0].glyphs[0].origin.x);
    }

    #[test]
    fn a_missing_font_is_reported_and_does_not_stop_extraction() {
        let (runs, report) =
            extract(simple_page("BT /Nope 10 Tf 0 0 Td (X) Tj /F1 10 Tf 0 20 Td (Y) Tj ET"));
        assert_eq!(report.missing_fonts, vec!["Nope"]);
        assert_eq!(runs.len(), 1, "the good run still comes through");
        assert_eq!(runs[0].text(), "Y");
    }

    #[test]
    fn unmapped_glyphs_are_counted_not_invented() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 10 Tf 0 0 Td (AB) Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /FirstChar 65 /LastChar 66 /Widths [500 500] >>",
            )
            .finish("/Root 1 0 R");
        let (runs, report) = extract(bytes);
        assert_eq!(report.unmapped_glyphs, 2);
        assert_eq!(runs[0].text(), "", "no /ToUnicode means no text, not made-up text");
        assert_eq!(runs[0].unmapped(), 2);
        assert_eq!(runs[0].glyphs.len(), 2, "the glyphs are still positioned");
    }

    #[test]
    fn marked_content_ids_are_attached_to_runs() {
        let (runs, _) = extract(simple_page(
            "/P << /MCID 7 >> BDC BT /F1 10 Tf 0 0 Td (A) Tj ET EMC BT /F1 10 Tf (B) Tj ET",
        ));
        assert_eq!(runs[0].mcid, Some(7));
        assert_eq!(runs[1].mcid, None);
    }

    #[test]
    fn runs_carry_the_span_and_source_of_their_operator() {
        let doc = Document::open(simple_page("BT /F1 10 Tf 0 0 Td (A) Tj ET")).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        let (runs, _, _) = extract_page(&doc, &p);
        assert_eq!(runs[0].source, Some(rasura_cos::ObjId::new(4, 0)));
        let content = crate::page_content(&doc, &p.dict).unwrap().0;
        assert_eq!(&content.data()[runs[0].op_span.clone()], b"(A) Tj");
    }

    #[test]
    fn invisible_text_is_extracted_and_flagged() {
        // Tr 3 is how OCR text is layered under a scan; extraction wants it,
        // rendering does not, so it is kept with its render mode recorded.
        let (runs, _) = extract(simple_page("BT /F1 10 Tf 3 Tr 0 0 Td (A) Tj ET"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].render_mode, 3);
    }

    #[test]
    fn page_text_concatenates_in_content_order() {
        let text = {
            let doc = Document::open(simple_page(
                "BT /F1 10 Tf 0 700 Td (Hello) Tj 0 -20 Td (World) Tj ET",
            ))
            .unwrap();
            let p = page::pages(&doc).unwrap().pages.remove(0);
            page_text(&doc, &p)
        };
        assert_eq!(text, "HelloWorld");
    }
}
