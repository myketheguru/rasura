//! Text extraction with the full derivation chain applied. Spec 7.2.
//!
//! `rasura-content` positions glyphs and resolves Unicode through
//! `/ToUnicode` alone -- strategy 1 of seven. This runs the rest of the chain
//! over the result, filling in what strategy 1 left blank and recording which
//! strategy won.
//!
//! Layered this way rather than folded into the content layer because the two
//! answer different questions. Content asks "where is this glyph"; that is
//! mechanical and cannot fail. Reconstruction asks "what does this glyph mean";
//! that is inference, it fails routinely, and the failures have to be reported.

use crate::unicode::{Decoder, Strategy, TextConfidence, confidence, pua_sentinel};
use rasura_content::GlyphRun;
use rasura_content::font::LoadedFont;
use rasura_content::page::Page;
use rasura_cos::document::Document;
use rasura_cos::{Dictionary, Name};
use std::collections::HashMap;

/// A glyph run with the chain applied.
#[derive(Debug, Clone)]
pub struct ResolvedRun {
    pub run: GlyphRun,
    /// One entry per glyph, aligned with `run.glyphs`.
    pub text: Vec<Option<String>>,
    /// Which strategy produced each glyph's text.
    pub strategies: Vec<Strategy>,
    pub confidence: TextConfidence,
}

impl ResolvedRun {
    /// The run's text. Glyphs that resolved to nothing contribute a Private Use
    /// Area sentinel rather than nothing at all: dropping them would silently
    /// shorten the string and misalign every offset after it.
    pub fn text(&self) -> String {
        self.text
            .iter()
            .zip(&self.run.glyphs)
            .map(|(t, g)| match t {
                Some(s) => s.clone(),
                None => pua_sentinel(g.code).to_string(),
            })
            .collect()
    }

    /// The run's text with unresolved glyphs omitted, for callers that would
    /// rather have short text than sentinels.
    pub fn text_lossy(&self) -> String {
        self.text.iter().flatten().cloned().collect::<Vec<_>>().concat()
    }

    pub fn unmapped(&self) -> usize {
        self.text.iter().filter(|t| t.is_none()).count()
    }
}

/// What the chain achieved on a page.
#[derive(Debug, Clone, Default)]
pub struct ResolveReport {
    pub glyphs: usize,
    pub mapped: usize,
    pub authoritative: usize,
    /// How many glyphs each strategy accounted for.
    pub by_strategy: Vec<(Strategy, usize)>,
    /// Fonts whose glyph names are all opaque, so nothing short of the font
    /// program can map them.
    pub opaque_fonts: Vec<String>,
}

impl ResolveReport {
    pub fn confidence(&self) -> TextConfidence {
        confidence(self.glyphs, self.mapped, self.authoritative)
    }
}

/// Extract a page and run the full derivation chain over it.
pub fn resolve_page(doc: &Document, page: &Page) -> (Vec<ResolvedRun>, ResolveReport) {
    // Extraction is given the standard-14 metrics. A font that names one of the
    // 14 and embeds nothing routinely omits `/Widths` too -- the whole point of
    // naming them is to leave the metrics out -- and without this every advance
    // on such a page is zero and every glyph lands on the same spot.
    let (runs, _text_report, _walk) =
        rasura_content::text::extract_page_with(doc, page, &crate::Standard14Widths);
    resolve_runs(doc, page, runs)
}

/// Apply the chain to already-extracted runs.
pub fn resolve_runs(
    doc: &Document,
    page: &Page,
    runs: Vec<GlyphRun>,
) -> (Vec<ResolvedRun>, ResolveReport) {
    let mut report = ResolveReport::default();
    let mut counts: HashMap<Strategy, usize> = HashMap::new();
    // Decoders are per font and expensive to build -- the AGL lookups add up --
    // so they are cached for the page.
    let mut cache: HashMap<Vec<u8>, Option<(LoadedFont, Decoder)>> = HashMap::new();
    let mut out = Vec::with_capacity(runs.len());

    for run in runs {
        let key = run.font_name.as_ref().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        if !cache.contains_key(&key) {
            let built =
                run.font_name.as_ref().and_then(|name| font_dict(doc, page, name)).map(|dict| {
                    let font = LoadedFont::load(doc, &dict);
                    let decoder = Decoder::build(doc, &dict, &font);
                    if decoder.opaque_names && !report.opaque_fonts.contains(&font.base_font) {
                        report.opaque_fonts.push(font.base_font.clone());
                    }
                    (font, decoder)
                });
            cache.insert(key.clone(), built);
        }

        let mut text = Vec::with_capacity(run.glyphs.len());
        let mut strategies = Vec::with_capacity(run.glyphs.len());
        let mut mapped = 0usize;
        let mut authoritative = 0usize;

        match cache.get(&key).and_then(|c| c.as_ref()) {
            Some((font, decoder)) => {
                for glyph in &run.glyphs {
                    // The content layer already resolved through /ToUnicode; if
                    // it did, that answer stands and the chain stops.
                    let (t, s) = match &glyph.unicode {
                        Some(existing) => (Some(existing.clone()), Strategy::ToUnicode),
                        None => {
                            let unit = rasura_content::CodeUnit {
                                code: glyph.code,
                                cid: glyph.cid,
                                offset: 0,
                                len: glyph.span.len().max(1),
                            };
                            decoder.resolve(font, &unit)
                        }
                    };
                    if t.is_some() {
                        mapped += 1;
                        if s.is_authoritative() {
                            authoritative += 1;
                        }
                    }
                    *counts.entry(s).or_default() += 1;
                    text.push(t);
                    strategies.push(s);
                }
            }
            None => {
                // No font resource: every glyph is unresolved, and says so.
                for glyph in &run.glyphs {
                    let _ = glyph;
                    text.push(None);
                    strategies.push(Strategy::Failed);
                    *counts.entry(Strategy::Failed).or_default() += 1;
                }
            }
        }

        report.glyphs += run.glyphs.len();
        report.mapped += mapped;
        report.authoritative += authoritative;
        let conf = confidence(run.glyphs.len(), mapped, authoritative);
        out.push(ResolvedRun { run, text, strategies, confidence: conf });
    }

    report.by_strategy = {
        let mut v: Vec<(Strategy, usize)> = counts.into_iter().collect();
        v.sort_by_key(|(s, n)| (std::cmp::Reverse(*n), *s));
        v
    };
    (out, report)
}

/// A page's text, in content order, with the chain applied.
///
/// Content order is not reading order -- §7.5 -- so this is useful for
/// comparison and not yet for presentation.
pub fn page_text(doc: &Document, page: &Page) -> String {
    let (runs, _) = resolve_page(doc, page);
    runs.iter().map(|r| r.text_lossy()).collect()
}

fn font_dict(doc: &Document, page: &Page, name: &Name) -> Option<Dictionary> {
    // Page-level resources only. A font defined solely inside a form XObject
    // needs the walker's scope stack, which `resolve_runs` does not carry;
    // those runs fall back to their /ToUnicode result.
    let resources = page.resources.as_ref()?;
    let dict = resources.as_dict()?;
    let fonts = doc.get_entry(dict, "Font").ok()??;
    let value = fonts.as_dict()?.get_name(name)?.clone();
    doc.resolve(&value).ok()?.as_dict().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::page;
    use rasura_cos::testutil::ClassicBuilder;

    fn page_of(content: &str, font: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content.as_bytes())
            .object(5, font)
            .finish("/Root 1 0 R")
    }

    fn resolve(bytes: Vec<u8>) -> (Vec<ResolvedRun>, ResolveReport) {
        let doc = Document::open(bytes).unwrap();
        let p = page::pages(&doc).unwrap().pages.remove(0);
        resolve_page(&doc, &p)
    }

    #[test]
    fn a_font_with_no_tounicode_now_extracts() {
        // This is the whole point of the phase: before the chain, this page
        // produced nothing.
        let (runs, report) = resolve(page_of(
            "BT /F1 12 Tf 0 0 Td (Hello) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding \
              /FirstChar 32 /LastChar 122 /Widths [500] >>",
        ));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text_lossy(), "Hello");
        assert_eq!(report.mapped, 5);
        assert_eq!(report.confidence(), TextConfidence::Exact);
    }

    #[test]
    fn ligatures_from_differences_resolve() {
        // The characters that vanish from LaTeX output with no /ToUnicode.
        let (runs, _) = resolve(page_of(
            "BT /F1 12 Tf 0 0 Td (\\001\\002) Tj ET",
            "<< /Type /Font /Subtype /Type1 /Encoding << /BaseEncoding /WinAnsiEncoding \
              /Differences [1 /fi 2 /ffl] >> >>",
        ));
        assert_eq!(runs[0].text_lossy(), "\u{fb01}\u{fb04}");
    }

    #[test]
    fn unresolved_glyphs_get_a_sentinel_rather_than_disappearing() {
        // Dropping them would shorten the string and misalign every offset
        // after it, which is worse than a visible placeholder.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 0 0 Td (AB) Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Wingdings /FontDescriptor 6 0 R >>",
            )
            // Symbolic: its built-in encoding lives in the font program.
            .object(6, "<< /Type /FontDescriptor /FontName /Wingdings /Flags 4 >>")
            .finish("/Root 1 0 R");
        let (runs, report) = resolve(bytes);
        assert_eq!(report.mapped, 0);
        assert_eq!(report.confidence(), TextConfidence::None);
        let t = runs[0].text();
        assert_eq!(t.chars().count(), 2, "length is preserved");
        assert!(t.chars().all(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)), "{t:?}");
        assert_eq!(runs[0].text_lossy(), "");
    }

    #[test]
    fn confidence_is_partial_when_only_some_glyphs_map() {
        let (runs, report) = resolve(page_of(
            "BT /F1 12 Tf 0 0 Td (A\\001) Tj ET",
            "<< /Type /Font /Subtype /Type1 /Encoding << /BaseEncoding /WinAnsiEncoding \
              /Differences [1 /g99] >> >>",
        ));
        // 'A' maps through WinAnsi; code 1 is an opaque name over a WinAnsi
        // position that is undefined.
        assert_eq!(runs[0].unmapped(), 1);
        assert_eq!(report.confidence(), TextConfidence::Partial);
    }

    #[test]
    fn the_winning_strategy_is_recorded_per_glyph() {
        let (runs, report) = resolve(page_of(
            "BT /F1 12 Tf 0 0 Td (AB) Tj ET",
            "<< /Type /Font /Subtype /Type1 /Encoding << /BaseEncoding /WinAnsiEncoding \
              /Differences [65 /bullet] >> >>",
        ));
        assert_eq!(runs[0].strategies[0], Strategy::Differences, "A came from /Differences");
        assert_eq!(runs[0].strategies[1], Strategy::BaseEncoding, "B came from WinAnsi");
        assert!(report.by_strategy.iter().any(|(s, _)| *s == Strategy::Differences));
    }

    #[test]
    fn tounicode_still_wins_when_present() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 0 0 Td (A) Tj ET")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>",
            )
            .stream(6, "", b"1 beginbfchar\n<41> <2022>\nendbfchar")
            .finish("/Root 1 0 R");
        let (runs, _) = resolve(bytes);
        assert_eq!(runs[0].text_lossy(), "\u{2022}");
        assert_eq!(runs[0].strategies[0], Strategy::ToUnicode);
    }

    #[test]
    fn opaque_fonts_are_named_in_the_report() {
        let (_runs, report) = resolve(page_of(
            "BT /F1 12 Tf 0 0 Td (\\001\\002\\003) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Weird /Encoding \
              << /Differences [1 /g1 2 /g2 3 /g3] >> >>",
        ));
        assert_eq!(report.opaque_fonts, vec!["ABCDEF+Weird"]);
    }

    #[test]
    fn page_text_joins_runs_in_content_order() {
        let text = {
            let doc = Document::open(page_of(
                "BT /F1 12 Tf 0 700 Td (One) Tj 0 -20 Td (Two) Tj ET",
                "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding >>",
            ))
            .unwrap();
            let p = page::pages(&doc).unwrap().pages.remove(0);
            page_text(&doc, &p)
        };
        assert_eq!(text, "OneTwo");
    }
}
