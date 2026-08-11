//! The Unicode derivation chain. Spec 7.2.
//!
//! > Try in order; stop at the first that yields a mapping. Record which
//! > strategy won per font -- this is a headline diagnostic.
//!
//! The order matters and so does stopping: a `/ToUnicode` that maps a code is
//! authoritative even when a later strategy would disagree, because the producer
//! knew what it meant and we are guessing.
//!
//! # What this does not do
//!
//! Step 6's shape matching is not implemented and, on Q1's evidence, is not
//! justified: the glyph-name heuristics account for 0.03% of glyphs across the
//! corpus. Where nothing resolves, the chain stops at step 7 and says so.
//!
//! Step 5 -- reverse lookup through the embedded font's `cmap` -- is here, and
//! reaches only fonts whose PDF dictionary offers no encoding at all. Q1 sized
//! that gap: of the fonts with no usable `/ToUnicode`, 47% carry `/Differences`
//! glyph names that step 2 resolves and only 12% have nothing at the PDF level.

use crate::agl;
use crate::glyphdata::{
    MAC_ROMAN_ENCODING, STANDARD_ENCODING, SYMBOL_ENCODING, WIN_ANSI_ENCODING,
    ZAPF_DINGBATS_ENCODING,
};
use rasura_content::font::{CodeUnit, LoadedFont};
use rasura_cos::document::Document;
use rasura_cos::{Dictionary, Object};
use std::collections::HashMap;

/// Which of §7.2's strategies produced a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Strategy {
    /// 1. The `/ToUnicode` CMap.
    ToUnicode,
    /// 2. `/Encoding` `/Differences` glyph names through the Adobe Glyph List.
    Differences,
    /// 2/3. A base encoding, named or implied.
    BaseEncoding,
    /// 3. The standard-14 built-in encoding.
    BuiltinEncoding,
    /// 4. A composite font's `/Encoding` CMap and CID system info.
    CidSystemInfo,
    /// 5. Reverse lookup through the embedded font's `cmap`. Phase 4.
    FontCmap,
    /// 6. Glyph-name heuristics: `uniXXXX`, `name.alt`, ligature names.
    GlyphNameHeuristic,
    /// 7. Nothing worked. The glyph gets a Private Use Area sentinel and the
    ///    containing text is marked degraded.
    Failed,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::ToUnicode => "/ToUnicode",
            Strategy::Differences => "/Differences + AGL",
            Strategy::BaseEncoding => "base encoding",
            Strategy::BuiltinEncoding => "built-in encoding",
            Strategy::CidSystemInfo => "CID system info",
            Strategy::FontCmap => "font cmap (Phase 4)",
            Strategy::GlyphNameHeuristic => "glyph-name heuristic",
            Strategy::Failed => "failed",
        }
    }

    /// Whether a mapping from this strategy is authoritative or inferred.
    pub fn is_authoritative(self) -> bool {
        matches!(self, Strategy::ToUnicode | Strategy::Differences | Strategy::BaseEncoding)
    }
}

/// How much of a run's text can be trusted. Mirrors the public
/// `textConfidence` of spec 11.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextConfidence {
    /// Every glyph mapped, and by an authoritative route.
    Exact,
    /// Some glyphs mapped by inference, or some did not map at all.
    Partial,
    /// Nothing mapped.
    None,
}

/// Private Use Area sentinel for a glyph that resolved to nothing.
///
/// Spec 7.2 step 7: assign a sentinel and mark confidence, rather than dropping
/// the glyph. Dropping would silently shorten the text and misalign every
/// offset after it; a sentinel keeps positions honest and is visibly not text.
pub const PUA_BASE: u32 = 0xE000;

pub fn pua_sentinel(code: u32) -> char {
    char::from_u32(PUA_BASE + (code & 0x0fff)).unwrap_or('\u{fffd}')
}

/// A font's resolved code-to-text mapping, with the provenance of each answer.
#[derive(Debug, Clone)]
pub struct Decoder {
    map: HashMap<u32, (String, Strategy)>,
    /// The strategy that produced most of this font's mappings.
    pub dominant: Strategy,
    /// Codes that resolved to nothing.
    pub failures: usize,
    /// True when the font's glyph names are all `g34`-style, so nothing short
    /// of the font program (step 5) or shape matching can help.
    pub opaque_names: bool,
}

impl Decoder {
    /// Build the chain for one font.
    pub fn build(doc: &Document, font_dict: &Dictionary, font: &LoadedFont) -> Decoder {
        let mut map: HashMap<u32, (String, Strategy)> = HashMap::new();
        let mut opaque = 0usize;
        let mut named = 0usize;

        // --- Step 2: /Differences glyph names, resolved through the AGL. -----
        // Done before the base encoding so that a code appearing in both takes
        // the /Differences answer, which is what the producer meant.
        let encoding = doc.get_entry(font_dict, "Encoding").ok().flatten();
        if let Some(enc) = encoding.as_deref().and_then(Object::as_dict)
            && let Ok(Some(diffs)) = doc.get_entry(enc, "Differences")
            && let Some(items) = diffs.as_array()
        {
            let mut code: i64 = 0;
            for item in items {
                match item {
                    Object::Integer(v) => code = *v,
                    Object::Real(v) => code = *v as i64,
                    Object::Name(n) => {
                        let name = String::from_utf8_lossy(n.as_bytes()).into_owned();
                        named += 1;
                        if agl::is_opaque(&name) {
                            opaque += 1;
                        } else if let Some(text) = agl::lookup(&name)
                            && let Ok(c) = u32::try_from(code)
                        {
                            // A name resolved by the AGL proper is step 2; one
                            // that needed the uniXXXX or ligature conventions is
                            // step 6, and the distinction is worth recording.
                            let strategy = if agl::exact(&name).is_some() {
                                Strategy::Differences
                            } else {
                                Strategy::GlyphNameHeuristic
                            };
                            map.insert(c, (text, strategy));
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }

        // --- Steps 2/3: the base encoding. ----------------------------------
        let base_table = base_encoding_table(doc, font_dict, font);
        if let Some((table, strategy)) = base_table {
            for (code, entry) in table.iter().enumerate() {
                if let Some(text) = entry {
                    map.entry(code as u32).or_insert_with(|| (text.to_string(), strategy));
                }
            }
        }

        // --- Step 5: the embedded font's own cmap, reversed. ----------------
        // Reached only when there was no base encoding at all -- that is, a
        // symbolic font whose PDF dictionary says nothing about how its codes
        // map. Everywhere else the producer has told us something, and a
        // reversed cmap is inference: a font's cmap says which glyph a
        // character produces, and running it backwards can only guess which
        // character a glyph came from when several map to one.
        //
        // Gating on the base table rather than on how full the map happens to
        // be also keeps the cost honest: step 5 parses the font program, and
        // doing that for every font to fill a handful of gaps is a lot of work
        // for a rounding error.
        if base_table.is_none() {
            for (code, ch) in font_cmap_mappings(doc, font_dict) {
                map.entry(code).or_insert_with(|| (ch.to_string(), Strategy::FontCmap));
            }
        }

        let dominant = dominant_strategy(&map);
        Decoder { map, dominant, failures: 0, opaque_names: named > 0 && opaque * 2 > named }
    }

    /// Resolve one code unit, consulting the font's `/ToUnicode` first.
    ///
    /// `/ToUnicode` is checked here rather than folded into the map because it
    /// is keyed by code and always wins, and because `LoadedFont` already owns
    /// it -- duplicating it would risk the two disagreeing.
    pub fn resolve(&self, font: &LoadedFont, unit: &CodeUnit) -> (Option<String>, Strategy) {
        if let Some(text) = font.unicode(unit) {
            return (Some(text.to_string()), Strategy::ToUnicode);
        }
        if let Some((text, strategy)) = self.map.get(&unit.code) {
            return (Some(text.clone()), *strategy);
        }
        // Step 4: a composite font with an Identity encoding gives no Unicode
        // at all -- the CID *is* the glyph index. Recording that separately
        // matters because it is not the same failure as an unmapped simple font.
        (None, Strategy::Failed)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Which base encoding applies, and which step of §7.2 it counts as.
fn base_encoding_table(
    doc: &Document,
    font_dict: &Dictionary,
    font: &LoadedFont,
) -> Option<(&'static [Option<&'static str>; 256], Strategy)> {
    // A composite font's codes are not single bytes, so a byte encoding cannot
    // apply to it.
    if font.kind == rasura_content::FontKind::Composite {
        return None;
    }

    let named = doc.get_entry(font_dict, "Encoding").ok().flatten().and_then(|e| match &*e {
        Object::Name(n) => Some(n.as_bytes().to_vec()),
        Object::Dictionary(d) => {
            d.get("BaseEncoding").and_then(Object::as_name).map(|n| n.as_bytes().to_vec())
        }
        _ => None,
    });

    match named.as_deref() {
        Some(b"WinAnsiEncoding") => Some((&WIN_ANSI_ENCODING, Strategy::BaseEncoding)),
        Some(b"MacRomanEncoding") => Some((&MAC_ROMAN_ENCODING, Strategy::BaseEncoding)),
        Some(b"StandardEncoding") => Some((&STANDARD_ENCODING, Strategy::BaseEncoding)),
        // MacExpertEncoding is a different glyph repertoire entirely; treating
        // it as Standard would produce confidently wrong text, so it is left to
        // /Differences and the font program.
        Some(b"MacExpertEncoding") => None,
        Some(_) => Some((&STANDARD_ENCODING, Strategy::BaseEncoding)),
        // ISO 32000-1 §9.6.6.2: with no /Encoding, a simple font uses its
        // built-in encoding.
        None => {
            // Symbol and ZapfDingbats are the two standard-14 faces whose
            // built-in encoding is their own -- neither Standard nor WinAnsi
            // describes them, and reading them as Standard yields Latin text
            // for a page of mathematics.
            let base = crate::metrics_face(font_dict);
            match base {
                Some("Symbol") => Some((&SYMBOL_ENCODING, Strategy::BuiltinEncoding)),
                Some("ZapfDingbats") => Some((&ZAPF_DINGBATS_ENCODING, Strategy::BuiltinEncoding)),
                // Any other symbolic font's encoding is in its own program.
                _ if is_symbolic(doc, font_dict) => None,
                _ => Some((&STANDARD_ENCODING, Strategy::BuiltinEncoding)),
            }
        }
    }
}

/// Spec 7.2 step 5: character codes resolved through the embedded font's own
/// `cmap`.
///
/// Two lookups chained. The code becomes a glyph id the way ISO 32000-1
/// §9.6.6.4 says a simple font's does — through the symbol table with the
/// 0xF000 offset, then the Macintosh table, then Unicode — and the glyph id
/// becomes a character by reversing the font's Unicode subtable.
///
/// Returns nothing for a font with no `cmap`, which includes every CFF-only
/// program: those carry glyph *names*, not character mappings.
fn font_cmap_mappings(doc: &Document, font_dict: &Dictionary) -> Vec<(u32, char)> {
    let Some(descriptor) = descriptor_of(doc, font_dict) else { return Vec::new() };
    let Some(program) = rasura_font::program::from_descriptor(doc, &descriptor) else {
        return Vec::new();
    };
    if !program.flavour.is_sfnt() {
        return Vec::new();
    }
    let Ok(sfnt) = rasura_font::Sfnt::parse(&program.bytes) else { return Vec::new() };
    let Some(cmap) = rasura_font::Cmap::parse(&program.bytes, &sfnt) else {
        return Vec::new();
    };

    let reverse = cmap.glyph_to_char(&program.bytes);
    if reverse.is_empty() {
        return Vec::new();
    }
    (0u32..256)
        .filter_map(|code| {
            let gid = cmap.simple_glyph(&program.bytes, code)?;
            let ch = *reverse.get(&gid)?;
            // A private-use answer is not text. It means the font's Unicode
            // table is itself a symbol mapping, and reporting it would look
            // like success while yielding characters no one can read.
            (!('\u{E000}'..='\u{F8FF}').contains(&ch)).then_some((code, ch))
        })
        .collect()
}

/// The font descriptor, following a composite font to its descendant.
fn descriptor_of(doc: &Document, font_dict: &Dictionary) -> Option<Dictionary> {
    let owner = match doc.get_entry(font_dict, "DescendantFonts").ok().flatten() {
        Some(d) => d
            .as_array()
            .and_then(|a| a.first().cloned())
            .and_then(|o| doc.resolve(&o).ok())
            .and_then(|o| o.as_dict().cloned())?,
        None => font_dict.clone(),
    };
    doc.get_entry(&owner, "FontDescriptor").ok().flatten().and_then(|d| d.as_dict().cloned())
}

/// `/Flags` bit 3 (value 4) marks a symbolic font, whose built-in encoding is
/// its own and not one of the standard tables.
fn is_symbolic(doc: &Document, font_dict: &Dictionary) -> bool {
    doc.get_entry(font_dict, "FontDescriptor")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().and_then(|d| d.get("Flags")).and_then(Object::as_i64))
        .is_some_and(|flags| flags & 4 != 0 && flags & 32 == 0)
}

fn dominant_strategy(map: &HashMap<u32, (String, Strategy)>) -> Strategy {
    let mut counts: HashMap<Strategy, usize> = HashMap::new();
    for (_, s) in map.values() {
        *counts.entry(*s).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s).unwrap_or(Strategy::Failed)
}

/// Confidence for a run, from how its glyphs resolved.
pub fn confidence(total: usize, mapped: usize, authoritative: usize) -> TextConfidence {
    if total == 0 || mapped == 0 {
        TextConfidence::None
    } else if mapped == total && authoritative == total {
        TextConfidence::Exact
    } else {
        TextConfidence::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn build(objects: &[(u32, &str)], font_obj: u32) -> (Document, LoadedFont, Decoder) {
        let mut b = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R >>");
        for (n, body) in objects {
            b = b.object(*n, body);
        }
        let doc = Document::open(b.finish("/Root 1 0 R")).unwrap();
        let dict = doc.get(rasura_cos::ObjId::new(font_obj, 0)).unwrap();
        let d = dict.as_dict().unwrap().clone();
        let font = LoadedFont::load(&doc, &d);
        let decoder = Decoder::build(&doc, &d, &font);
        (doc, font, decoder)
    }

    fn text(font: &LoadedFont, dec: &Decoder, bytes: &[u8]) -> String {
        font.decode(bytes).iter().filter_map(|u| dec.resolve(font, u).0).collect()
    }

    #[test]
    fn win_ansi_base_encoding_resolves() {
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            )],
            5,
        );
        assert_eq!(text(&f, &dec, b"Hello"), "Hello");
        // 0xE9 is e-acute in CP1252.
        assert_eq!(text(&f, &dec, &[0xe9]), "é");
        assert_eq!(dec.dominant, Strategy::BaseEncoding);
    }

    #[test]
    fn mac_roman_differs_from_win_ansi_where_it_should() {
        let (_d, mac, mac_dec) =
            build(&[(5, "<< /Type /Font /Subtype /Type1 /Encoding /MacRomanEncoding >>")], 5);
        let (_d2, win, win_dec) =
            build(&[(5, "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding >>")], 5);
        // 0x8C is a-ring in Mac Roman and the OE ligature in CP1252 -- the sort
        // of divergence that turns a Mac-produced document into mojibake if the
        // wrong table is used.
        assert_eq!(text(&mac, &mac_dec, &[0x8c]), "å");
        assert_eq!(text(&win, &win_dec, &[0x8c]), "Œ");
        // And 0x8E is e-acute on the Mac, Ž on Windows.
        assert_eq!(text(&mac, &mac_dec, &[0x8e]), "é");
        assert_eq!(text(&win, &win_dec, &[0x8e]), "Ž");
    }

    #[test]
    fn differences_override_the_base_encoding() {
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding << /BaseEncoding /WinAnsiEncoding \
                  /Differences [65 /bullet 66 /fi] >> >>",
            )],
            5,
        );
        // 'A' and 'B' are remapped; 'C' still comes from WinAnsi.
        assert_eq!(text(&f, &dec, b"ABC"), "\u{2022}\u{fb01}C");
    }

    #[test]
    fn differences_advance_the_code_between_names() {
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding << /Differences [97 /a /b /c 100 /x] >> >>",
            )],
            5,
        );
        assert_eq!(text(&f, &dec, b"abcd"), "abcx");
    }

    #[test]
    fn differences_resolve_ligature_and_uni_names() {
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding \
                  << /Differences [1 /fi 2 /uni0041 3 /f_l 4 /A.sc] >> >>",
            )],
            5,
        );
        // /fi -> the ligature, /uni0041 -> A, /f_l -> "fl", /A.sc -> A.
        assert_eq!(text(&f, &dec, &[1, 2, 3, 4]), "\u{fb01}AflA");
    }

    #[test]
    fn no_encoding_falls_back_to_the_builtin_standard_encoding() {
        let (_d, f, dec) =
            build(&[(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>")], 5);
        assert_eq!(text(&f, &dec, b"Hi"), "Hi");
        assert_eq!(dec.dominant, Strategy::BuiltinEncoding);
        // StandardEncoding's 0xA9 is quotesingle, not the copyright sign.
        assert_eq!(text(&f, &dec, &[0xa9]), "'");
    }

    #[test]
    fn a_symbolic_font_gets_no_base_encoding_guess() {
        // Its built-in encoding lives in the font program, which is Phase 4.
        // Guessing StandardEncoding would produce confidently wrong text.
        let (_d, f, dec) = build(
            &[
                (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Wingdings /FontDescriptor 6 0 R >>"),
                (6, "<< /Type /FontDescriptor /Flags 4 >>"),
            ],
            5,
        );
        assert!(dec.is_empty());
        assert_eq!(text(&f, &dec, b"A"), "");
    }

    /// A TrueType program with a (3,0) symbol cmap and a (3,1) Unicode cmap
    /// that disagree about which codes reach which glyphs -- exactly the shape
    /// step 5 exists to unpick.
    fn symbolic_truetype() -> Vec<u8> {
        /// One format-4 segment mapping `start..=end` through idDelta.
        fn seg(start: u16, end: u16, delta: u16) -> Vec<u8> {
            let mut t = Vec::new();
            t.extend_from_slice(&4u16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
            t.extend_from_slice(&4u16.to_be_bytes()); // segCountX2 for 2 segments
            t.extend_from_slice(&[0; 6]);
            t.extend_from_slice(&end.to_be_bytes());
            t.extend_from_slice(&0xFFFFu16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
            t.extend_from_slice(&start.to_be_bytes());
            t.extend_from_slice(&0xFFFFu16.to_be_bytes());
            t.extend_from_slice(&delta.to_be_bytes());
            t.extend_from_slice(&1u16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
            let len = t.len() as u16;
            t[2..4].copy_from_slice(&len.to_be_bytes());
            t
        }

        // (3,0): codes 0xF041..0xF043 -> gids 1..3.
        // (3,1): characters 'X','Y','Z' -> gids 1..3.
        let symbol = seg(0xF041, 0xF043, 1u16.wrapping_sub(0xF041));
        let unicode = seg(0x58, 0x5A, 1u16.wrapping_sub(0x58));

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&2u16.to_be_bytes());
        let base = 4 + 2 * 8;
        cmap.extend_from_slice(&3u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&(base as u32).to_be_bytes());
        cmap.extend_from_slice(&3u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&((base + symbol.len()) as u32).to_be_bytes());
        cmap.extend_from_slice(&symbol);
        cmap.extend_from_slice(&unicode);

        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(b"cmap");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&28u32.to_be_bytes());
        out.extend_from_slice(&(cmap.len() as u32).to_be_bytes());
        out.extend_from_slice(&cmap);
        out
    }

    #[test]
    fn a_symbolic_truetype_resolves_through_its_own_cmap() {
        // Spec 7.2 step 5, and the last strategy that was still missing. The
        // PDF says nothing: no /ToUnicode, no /Encoding, symbolic flags. Only
        // the font program knows that code 0x41 draws the glyph for 'X'.
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                "<< /Type /Font /Subtype /TrueType /BaseFont /AAAAAA+Sym \
                 /FontDescriptor 6 0 R >>",
            )
            .object(6, "<< /Type /FontDescriptor /Flags 4 /FontFile2 7 0 R >>")
            .stream(7, "", &symbolic_truetype())
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let dict = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let font = LoadedFont::load(&doc, &dict);
        let dec = Decoder::build(&doc, &dict, &font);

        assert_eq!(dec.dominant, Strategy::FontCmap);
        assert_eq!(text(&font, &dec, &[0x41, 0x42, 0x43]), "XYZ");
    }

    #[test]
    fn a_font_with_a_base_encoding_does_not_reach_step_five() {
        // The producer said what the codes mean; a reversed cmap is inference
        // and must not override it -- nor cost a font-program parse.
        let bytes = rasura_cos::testutil::ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                "<< /Type /Font /Subtype /TrueType /BaseFont /AAAAAA+Sym \
                 /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
            )
            .object(6, "<< /Type /FontDescriptor /Flags 4 /FontFile2 7 0 R >>")
            .stream(7, "", &symbolic_truetype())
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let dict = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let font = LoadedFont::load(&doc, &dict);
        let dec = Decoder::build(&doc, &dict, &font);

        assert_eq!(dec.dominant, Strategy::BaseEncoding);
        assert_eq!(text(&font, &dec, &[0x41]), "A", "WinAnsi, not the font's cmap");
    }

    #[test]
    fn symbol_uses_its_own_built_in_encoding() {
        // Carried as a gap since Phase 2. Symbol is symbolic and names no
        // /Encoding, so it used to resolve to nothing; reading it as
        // StandardEncoding instead would turn a page of mathematics into Latin
        // letters, which is worse than blank.
        let (_d, f, dec) = build(
            &[
                (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol /FontDescriptor 6 0 R >>"),
                (6, "<< /Type /FontDescriptor /Flags 4 >>"),
            ],
            5,
        );
        assert_eq!(dec.dominant, Strategy::BuiltinEncoding);
        // 0x61 is alpha, 0x62 beta, 0x70 pi -- not a, b, p.
        assert_eq!(text(&f, &dec, &[0x61, 0x62, 0x70]), "\u{3b1}\u{3b2}\u{3c0}");
    }

    #[test]
    fn zapf_dingbats_uses_its_own_built_in_encoding() {
        let (_d, f, dec) = build(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats \
                     /FontDescriptor 6 0 R >>",
                ),
                (6, "<< /Type /FontDescriptor /Flags 4 >>"),
            ],
            5,
        );
        assert_eq!(dec.dominant, Strategy::BuiltinEncoding);
        // 0x61 is a solid circle (a9 in the dingbat repertoire), not `a`.
        let got = text(&f, &dec, &[0x61]);
        assert_ne!(got, "a", "read as Latin");
        assert!(!got.is_empty(), "and not left blank");
    }

    #[test]
    fn an_explicit_encoding_still_wins_over_the_built_in_one() {
        // A font may name /Symbol and then override with WinAnsi; the file's
        // own statement outranks the face's default.
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol \
                 /Encoding /WinAnsiEncoding >>",
            )],
            5,
        );
        assert_eq!(dec.dominant, Strategy::BaseEncoding);
        assert_eq!(text(&f, &dec, b"ab"), "ab");
    }

    #[test]
    fn tounicode_wins_over_every_later_strategy() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R >>")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>",
            )
            .stream(6, "", b"1 beginbfchar\n<41> <2022>\nendbfchar")
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let d = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let font = LoadedFont::load(&doc, &d);
        let dec = Decoder::build(&doc, &d, &font);

        // WinAnsi says 0x41 is 'A'; /ToUnicode says bullet. The producer wins.
        let unit = font.decode(b"A")[0];
        let (text, strategy) = dec.resolve(&font, &unit);
        assert_eq!(text.as_deref(), Some("\u{2022}"));
        assert_eq!(strategy, Strategy::ToUnicode);
    }

    #[test]
    fn opaque_glyph_names_are_detected_and_not_invented() {
        let (_d, f, dec) = build(
            &[(
                5,
                "<< /Type /Font /Subtype /Type1 /Encoding \
                  << /Differences [1 /g3 2 /g4 3 /g5 4 /g6] >> >>",
            )],
            5,
        );
        assert!(dec.opaque_names, "a font of gNN names must be flagged");
        // They resolve through StandardEncoding's builtin fallback or not at
        // all, but never to invented text from the names themselves.
        let unit = f.decode(&[1])[0];
        assert_eq!(dec.resolve(&f, &unit).0, None);
    }

    #[test]
    fn a_composite_font_gets_no_byte_encoding() {
        let (_d, _f, dec) = build(
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
                ),
                (6, "<< /Type /Font /Subtype /CIDFontType2 >>"),
            ],
            5,
        );
        assert!(dec.is_empty(), "byte encodings cannot apply to two-byte codes");
    }

    #[test]
    fn pua_sentinels_are_distinct_and_in_range() {
        let a = pua_sentinel(1);
        let b = pua_sentinel(2);
        assert_ne!(a, b);
        for code in [0u32, 1, 255, 4095, 65535] {
            let c = pua_sentinel(code);
            assert!(('\u{e000}'..='\u{f8ff}').contains(&c), "{c:?} outside the PUA");
        }
    }

    #[test]
    fn confidence_reflects_what_actually_resolved() {
        assert_eq!(confidence(0, 0, 0), TextConfidence::None);
        assert_eq!(confidence(10, 0, 0), TextConfidence::None);
        assert_eq!(confidence(10, 10, 10), TextConfidence::Exact);
        assert_eq!(confidence(10, 10, 4), TextConfidence::Partial, "inferred is not exact");
        assert_eq!(confidence(10, 7, 7), TextConfidence::Partial);
    }

    #[test]
    fn strategies_are_ordered_by_trustworthiness() {
        assert!(Strategy::ToUnicode.is_authoritative());
        assert!(Strategy::Differences.is_authoritative());
        assert!(!Strategy::GlyphNameHeuristic.is_authoritative());
        assert!(!Strategy::Failed.is_authoritative());
    }
}
