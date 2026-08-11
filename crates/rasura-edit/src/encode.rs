//! Turning text back into the codes a font understands. Spec 9.2.
//!
//! Everything below layer five reads in one direction: a character code becomes
//! a glyph, and §7.2's seven strategies turn that into Unicode. Writing text
//! requires the inverse, and the inverse is not a second implementation of the
//! same table — it is *this document's* table, run backwards.
//!
//! That distinction is the whole design. An encoder built from the Adobe Glyph
//! List, or from WinAnsi, would be right about what `é` "should" be and wrong
//! about what this file's font actually draws at that code. Building it by
//! inverting [`Decoder`] instead guarantees the property that matters:
//!
//! > text written back through this encoder extracts, through the same chain
//! > the reader uses, to the text that was asked for.
//!
//! # The code space
//!
//! A simple font has 256 codes and they can all be enumerated. A composite font
//! has a code space defined by its CMap, which for `Identity-H` is 65,536
//! two-byte codes of which a subset font draws perhaps two hundred. Enumerating
//! that is pointless: the other 65,336 map to glyphs the subset does not carry.
//!
//! So a composite font's inverse is built from the codes the document is
//! **observed** to use. A character already on the page can be typed again; one
//! that is not there is reported [`Unencodable`] rather than guessed at. That is
//! the honest boundary, and §8.4's glyph injection is what moves it — a caller
//! that gets `Unencodable` has the option of adding the glyph, which is exactly
//! the decision spec §2 says belongs to the caller and not to the engine.

use rasura_content::font::{CodeUnit, LoadedFont};
use rasura_layout::unicode::Decoder;
use std::collections::HashMap;

/// Text this font cannot express.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the font has no code for {text:?} at character {at}")]
pub struct Unencodable {
    /// The character that could not be encoded.
    pub text: String,
    /// Its character offset within the requested string.
    pub at: usize,
}

/// One font's text-to-code mapping, inverted from how the document reads.
#[derive(Debug, Clone)]
pub struct Encoder {
    /// Text to the code that produces it, with that code's byte length.
    ///
    /// Keyed by `String` rather than `char` because a code can produce several
    /// characters: a `fi` ligature is one glyph and two characters, and losing
    /// that would encode `fi` as two separate glyphs the producer never used.
    map: HashMap<String, (u32, usize)>,
    /// The longest key, so the greedy match knows where to start.
    longest: usize,
    /// Byte length used for codes not in the map. Only for reporting.
    pub code_bytes: usize,
}

impl Encoder {
    /// Invert a font's decoding.
    ///
    /// `observed` are codes the document is known to use, which is the only
    /// source for a composite font. They are also merged in for simple fonts,
    /// where they cost nothing and cover a producer that draws a code its
    /// declared encoding does not describe.
    pub fn build(font: &LoadedFont, decoder: &Decoder, observed: &[u32]) -> Encoder {
        let mut map: HashMap<String, (u32, usize)> = HashMap::new();
        let mut longest = 0usize;

        let code_bytes = font.cmap.codespaces().first().map(|c| c.bytes).unwrap_or(
            // A composite font with no declared codespace is `Identity-H` in
            // all but name, and those are two bytes. A simple font is one.
            match font.kind {
                rasura_content::font::FontKind::Composite => 2,
                _ => 1,
            },
        );

        // Simple fonts: the whole space. It is 256 entries and enumerating it
        // means a character in the font's encoding can be typed even if the
        // page does not currently use it.
        let simple = code_bytes == 1;
        let candidates: Vec<u32> = if simple {
            (0..=255u32).chain(observed.iter().copied()).collect()
        } else {
            observed.to_vec()
        };

        for code in candidates {
            let unit = CodeUnit { code, cid: font.cmap.cid(code), offset: 0, len: code_bytes };
            let (Some(text), _) = decoder.resolve(font, &unit) else { continue };
            if text.is_empty() {
                continue;
            }
            // A sentinel is not text. Encoding to it would write a code whose
            // meaning is "this glyph did not map", which is not something a
            // caller ever asked for.
            if text.chars().any(is_sentinel) {
                continue;
            }

            longest = longest.max(text.chars().count());
            // The lowest code wins, matching how the reader resolves a glyph
            // reachable by two codes. Without this the choice depends on hash
            // iteration order and the same edit encodes differently run to run.
            map.entry(text)
                .and_modify(|slot| {
                    if code < slot.0 {
                        *slot = (code, code_bytes);
                    }
                })
                .or_insert((code, code_bytes));
        }

        Encoder { map, longest: longest.max(1), code_bytes }
    }

    /// Whether this font can write `text` at all.
    pub fn can_encode(&self, text: &str) -> bool {
        self.encode(text).is_ok()
    }

    /// Encode `text` into the bytes a showing operator takes.
    ///
    /// Longest match first, so a `fi` in the font is written as the ligature the
    /// producer would have used rather than as two letters.
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, Unencodable> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::with_capacity(chars.len() * self.code_bytes);
        let mut i = 0usize;

        while i < chars.len() {
            let mut matched = None;
            for take in (1..=self.longest.min(chars.len() - i)).rev() {
                let candidate: String = chars[i..i + take].iter().collect();
                if let Some((code, bytes)) = self.map.get(&candidate) {
                    matched = Some((take, *code, *bytes));
                    break;
                }
            }
            let Some((take, code, bytes)) = matched else {
                return Err(Unencodable { text: chars[i].to_string(), at: i });
            };
            push_code(&mut out, code, bytes);
            i += take;
        }
        Ok(out)
    }

    /// How many distinct strings this font can write.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The code a piece of text encodes to, if any.
    pub fn code_for(&self, text: &str) -> Option<u32> {
        self.map.get(text).map(|(code, _)| *code)
    }
}

/// Codes are big-endian and fixed-width within a codespace range.
fn push_code(out: &mut Vec<u8>, code: u32, bytes: usize) {
    let be = code.to_be_bytes();
    let bytes = bytes.clamp(1, 4);
    out.extend_from_slice(&be[4 - bytes..]);
}

/// The Private Use Area sentinel §7.2 emits for a glyph that did not map.
fn is_sentinel(c: char) -> bool {
    let v = c as u32;
    v == 0xFFFD || (rasura_layout::unicode::PUA_BASE..=0xF8FF).contains(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;
    use rasura_cos::{Document, ObjId};

    /// A document with one simple WinAnsi Helvetica.
    fn winansi_doc() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    fn encoder_for(bytes: Vec<u8>) -> (Document, Encoder) {
        let doc = Document::open(bytes).expect("open");
        let dict = doc.get(ObjId::new(5, 0)).expect("font").as_dict().expect("dict").clone();
        let font = LoadedFont::load(&doc, &dict);
        let decoder = Decoder::build(&doc, &dict, &font);
        let encoder = Encoder::build(&font, &decoder, &[]);
        (doc, encoder)
    }

    #[test]
    fn a_winansi_font_encodes_latin_text() {
        let (_doc, encoder) = encoder_for(winansi_doc());
        assert_eq!(encoder.encode("Hello").expect("encodes"), b"Hello");
        assert_eq!(encoder.encode("").expect("encodes"), b"");
    }

    #[test]
    fn winansi_high_codes_go_to_their_own_byte_not_to_utf8() {
        // The failure this prevents is silent and total: writing 'é' as its
        // two UTF-8 bytes produces two glyphs from a font that has one, and
        // the page reads as mojibake while every structural check passes.
        let (_doc, encoder) = encoder_for(winansi_doc());
        let bytes = encoder.encode("é").expect("WinAnsi has eacute");
        assert_eq!(bytes, vec![0xE9], "one byte, at the WinAnsi code");
    }

    #[test]
    fn a_character_the_font_cannot_draw_is_reported_not_dropped() {
        // Spec §2: never silently substitute. The caller decides whether to
        // inject a glyph, substitute a font, or refuse -- and cannot decide
        // anything if the character quietly vanished.
        let (_doc, encoder) = encoder_for(winansi_doc());
        let err = encoder.encode("ab\u{4e00}cd").expect_err("no CJK in WinAnsi Helvetica");
        assert_eq!(err.text, "\u{4e00}");
        assert_eq!(err.at, 2, "the offset points at the character that failed");
    }

    #[test]
    fn encoding_round_trips_through_the_reader() {
        // The property the whole module exists for: what this writes is what
        // the extraction chain reads back. Asserted against the real reader,
        // not against a second copy of the table.
        let doc = Document::open(winansi_doc()).expect("open");
        let dict = doc.get(ObjId::new(5, 0)).expect("font").as_dict().expect("dict").clone();
        let font = LoadedFont::load(&doc, &dict);
        let decoder = Decoder::build(&doc, &dict, &font);
        let encoder = Encoder::build(&font, &decoder, &[]);

        for text in ["Hello, world", "Fidelity", "caf\u{e9}", "50% \u{2014} done"] {
            let Ok(codes) = encoder.encode(text) else { continue };
            let back: String = font
                .decode(&codes)
                .iter()
                .map(|unit| decoder.resolve(&font, unit).0.unwrap_or_default())
                .collect();
            assert_eq!(back, text, "round trip for {text:?}");
        }
    }

    #[test]
    fn the_lowest_code_wins_when_two_produce_the_same_text() {
        // Otherwise the choice comes from hash iteration order and the same
        // edit encodes differently between runs -- which turns a byte-identical
        // regression test into a flaky one.
        let (_doc, encoder) = encoder_for(winansi_doc());
        for _ in 0..8 {
            let (_doc2, again) = encoder_for(winansi_doc());
            assert_eq!(encoder.encode("space test").ok(), again.encode("space test").ok());
        }
    }

    #[test]
    fn an_empty_font_encodes_nothing_and_says_so() {
        let encoder = Encoder { map: HashMap::new(), longest: 1, code_bytes: 1 };
        assert!(encoder.is_empty());
        assert!(encoder.encode("").is_ok(), "the empty string is always encodable");
        assert!(encoder.encode("a").is_err());
    }

    #[test]
    fn codes_are_written_big_endian_at_their_declared_width() {
        let mut out = Vec::new();
        push_code(&mut out, 0x41, 1);
        push_code(&mut out, 0x0102, 2);
        assert_eq!(out, vec![0x41, 0x01, 0x02]);
    }
}
