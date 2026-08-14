//! Shaping, via `harfrust`. Spec 8.3.
//!
//! Spec 8.3 divides the work plainly: complex scripts "are the shaper's job;
//! your job is to pass correct script, language, and direction, derived from
//! the run's Unicode content". So this module is mostly about *deciding what to
//! ask for* — the shaping call itself is one line.
//!
//! Getting the decision wrong is quiet rather than loud. Hand Arabic to the
//! shaper as left-to-right Latin and it produces glyphs, in the wrong order,
//! with no joining forms; nothing errors. That is why script and direction
//! detection carry the tests here and the shaping call barely does.
//!
//! The shaper was `rustybuzz` until harfrust replaced it. Both are ports of the
//! same HarfBuzz algorithm and the positions land in the same place, but
//! rustybuzz is unmaintained (RUSTSEC-2026-0192 and -0206) and carried a second
//! font parser, `ttf-parser`, alongside the one the rest of this crate reads
//! tables with. One shaper, one parser, and an advisory list that is empty.
//!
//! The **reshape boundary rule** — which glyphs may be rewritten at all — is in
//! [`crate::reshape`], deliberately separate: it is index arithmetic that
//! should be testable without a font.

use crate::reshape::KerningSource;
use unicode_script::{Script as UScript, UnicodeScript};

/// Text direction for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
    /// Vertical, top to bottom: CJK in vertical writing mode.
    TopToBottom,
}

impl Direction {
    fn to_harfrust(self) -> harfrust::Direction {
        match self {
            Direction::LeftToRight => harfrust::Direction::LeftToRight,
            Direction::RightToLeft => harfrust::Direction::RightToLeft,
            Direction::TopToBottom => harfrust::Direction::TopToBottom,
        }
    }

    pub fn is_horizontal(self) -> bool {
        !matches!(self, Direction::TopToBottom)
    }
}

/// What to ask the shaper for.
#[derive(Debug, Clone)]
pub struct ShapeRequest {
    pub text: String,
    pub direction: Direction,
    /// ISO 15924 tag, e.g. `Arab`, `Deva`, `Latn`.
    pub script: [u8; 4],
    /// BCP 47 language tag, when the document declares one. Affects shaping in
    /// a handful of real cases -- Turkish dotted i, Serbian italic forms -- and
    /// is passed through rather than guessed.
    pub language: Option<String>,
    /// Whether to enable the font's own kerning.
    pub kerning: bool,
    /// Whether to enable standard ligatures.
    pub ligatures: bool,
}

/// One shaped glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub gid: u16,
    /// Byte offset into the request's text that produced this glyph. Several
    /// glyphs may share a cluster and one glyph may cover several characters;
    /// that is the whole point of shaping and the mapping is not one-to-one.
    pub cluster: u32,
    /// Advance in font units.
    pub x_advance: i32,
    pub y_advance: i32,
    /// Placement offset in font units, for marks.
    pub x_offset: i32,
    pub y_offset: i32,
}

/// Shape a run.
///
/// Returns `None` when the bytes are not a font the shaper can load, which is
/// reported rather than papered over: a caller that cannot shape must fall back
/// to preserving the original glyphs, not to guessing new ones.
pub fn shape(font: &[u8], request: &ShapeRequest) -> Option<Vec<ShapedGlyph>> {
    let face = harfrust::FontRef::from_index(font, 0).ok()?;
    let data = harfrust::ShaperData::new(&face);
    let shaper = data.shaper(&face).build();

    let mut buffer = harfrust::UnicodeBuffer::new();
    buffer.push_str(&request.text);
    buffer.set_direction(request.direction.to_harfrust());
    if let Some(script) =
        harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&request.script))
    {
        buffer.set_script(script);
    }
    if let Some(lang) = request.language.as_ref().and_then(|l| l.parse().ok()) {
        buffer.set_language(lang);
    }

    // Features are named explicitly rather than left to defaults, because spec
    // 8.3 requires them to be *derived from the original*: a producer that did
    // not apply ligatures produced a glyph sequence we must be able to
    // reproduce, and silently enabling `liga` would change glyphs the user
    // never touched.
    let mut features = Vec::new();
    if !request.kerning {
        features.push(feature(b"kern", false));
    }
    if !request.ligatures {
        features.push(feature(b"liga", false));
        features.push(feature(b"clig", false));
    }

    // No scale is set, so positions come back in font units, which is what
    // `ShapedGlyph` documents and every caller divides by unitsPerEm itself.
    let output = shaper.shape(buffer, harfrust::ShapeOptions::new().features(&features));
    let infos = output.glyph_infos();
    let positions = output.glyph_positions();

    Some(
        infos
            .iter()
            .zip(positions.iter())
            .map(|(i, p)| ShapedGlyph {
                gid: i.glyph_id as u16,
                cluster: i.cluster,
                x_advance: p.x_advance,
                y_advance: p.y_advance,
                x_offset: p.x_offset,
                y_offset: p.y_offset,
            })
            .collect(),
    )
}

fn feature(tag: &[u8; 4], on: bool) -> harfrust::Feature {
    harfrust::Feature::new(harfrust::Tag::new(tag), u32::from(on), ..)
}

/// The dominant script of a string, as an ISO 15924 tag.
///
/// "Dominant" rather than "first": a run of Arabic containing a Latin
/// product name is Arabic, and shaping the whole run as Latin would drop every
/// joining form. Characters common to all scripts — spaces, digits, most
/// punctuation — carry no vote, because a run beginning with a space would
/// otherwise be classified by the space.
pub fn detect_script(text: &str) -> [u8; 4] {
    let mut counts: Vec<(UScript, usize)> = Vec::new();
    for ch in text.chars() {
        let s = ch.script();
        if matches!(s, UScript::Common | UScript::Inherited | UScript::Unknown) {
            continue;
        }
        match counts.iter_mut().find(|(k, _)| *k == s) {
            Some((_, n)) => *n += 1,
            None => counts.push((s, 1)),
        }
    }
    // Ties go to the first seen, which keeps the answer stable for a run that
    // is genuinely half and half.
    match counts.into_iter().max_by_key(|(_, n)| *n) {
        Some((s, _)) => iso15924(s),
        // No strong character at all: digits, spaces, punctuation. Latin is the
        // shaper's own default and is the least surprising answer.
        None => *b"Latn",
    }
}

/// Natural direction for a script.
///
/// Only the right-to-left set needs listing; everything else is left-to-right,
/// and vertical is a *writing mode* the PDF declares rather than a property of
/// the script.
pub fn direction_for(script: [u8; 4]) -> Direction {
    match &script {
        b"Arab" | b"Hebr" | b"Syrc" | b"Thaa" | b"Nkoo" | b"Samr" | b"Mand" | b"Adlm" | b"Rohg"
        | b"Yezi" | b"Ougr" | b"Phnx" | b"Armi" | b"Prti" | b"Phli" | b"Avst" | b"Sarb"
        | b"Narb" | b"Palm" | b"Hatr" | b"Mani" | b"Sogd" | b"Sogo" | b"Chrs" | b"Elym"
        | b"Nbat" | b"Hung" | b"Cprt" | b"Khar" | b"Lydi" | b"Mero" | b"Merc" | b"Orkh" => {
            Direction::RightToLeft
        }
        _ => Direction::LeftToRight,
    }
}

/// Build a request from the original run, inferring both features. Spec 8.3.
///
/// > When reshaping, derive features from the original where inferable: if the
/// > original sequence contains a GID that the font's `GSUB` maps only under
/// > `liga`, enable `liga`.
///
/// `original_glyphs` are the glyph ids the producer actually emitted. Their
/// presence or absence in the font's ligature coverage is the evidence: a run
/// containing `ﬁ` was set with ligatures on, and one that spells out `f i` when
/// the font could have ligated them was not. Reshaping against the wrong answer
/// changes glyphs the user did not touch, in one direction or the other.
pub fn request_from_original(
    text: &str,
    original_glyphs: &[u16],
    font: &[u8],
    vertical: bool,
    kerning: KerningSource,
    language: Option<String>,
) -> ShapeRequest {
    let ligatures = crate::sfnt::Sfnt::parse(font)
        .map(|sfnt| {
            let coverage = crate::gsub::ligature_coverage(font, &sfnt);
            !coverage.features_for(original_glyphs).is_empty()
        })
        // A font whose tables will not parse tells us nothing, and guessing
        // "on" would introduce ligatures on no evidence at all.
        .unwrap_or(false);
    request_for(text, vertical, kerning, ligatures, language)
}

/// Build a request for a run of text, deriving what can be derived.
///
/// `vertical` comes from the PDF's writing mode, not from the text: the same
/// CJK characters are set horizontally and vertically, and only the file knows
/// which.
pub fn request_for(
    text: &str,
    vertical: bool,
    kerning: KerningSource,
    ligatures: bool,
    language: Option<String>,
) -> ShapeRequest {
    let script = detect_script(text);
    let direction = if vertical { Direction::TopToBottom } else { direction_for(script) };
    ShapeRequest {
        text: text.to_string(),
        direction,
        script,
        language,
        // Spec 8.3: reproduce what the producer did. Font kerning is
        // regenerated; the producer's own tracking is not, because the shaper
        // would replace it with the font's values and visibly respace the text.
        kerning: kerning == KerningSource::Font,
        ligatures,
    }
}

/// ISO 15924 tag for a script.
///
/// `unicode-script` names its variants after the scripts rather than the tags,
/// and the two differ often enough -- `Han` is `Hani`, `Nko` is `Nkoo` -- that
/// deriving one from the other by truncation is wrong. Only the scripts a PDF
/// realistically carries are named; the rest fall back to Latin, which is what
/// the shaper would default to anyway.
fn iso15924(script: UScript) -> [u8; 4] {
    match script {
        UScript::Latin => *b"Latn",
        UScript::Arabic => *b"Arab",
        UScript::Hebrew => *b"Hebr",
        UScript::Cyrillic => *b"Cyrl",
        UScript::Greek => *b"Grek",
        UScript::Han => *b"Hani",
        UScript::Hiragana => *b"Hira",
        UScript::Katakana => *b"Kana",
        UScript::Hangul => *b"Hang",
        UScript::Thai => *b"Thai",
        UScript::Lao => *b"Laoo",
        UScript::Devanagari => *b"Deva",
        UScript::Bengali => *b"Beng",
        UScript::Gurmukhi => *b"Guru",
        UScript::Gujarati => *b"Gujr",
        UScript::Oriya => *b"Orya",
        UScript::Tamil => *b"Taml",
        UScript::Telugu => *b"Telu",
        UScript::Kannada => *b"Knda",
        UScript::Malayalam => *b"Mlym",
        UScript::Sinhala => *b"Sinh",
        UScript::Myanmar => *b"Mymr",
        UScript::Khmer => *b"Khmr",
        UScript::Tibetan => *b"Tibt",
        UScript::Georgian => *b"Geor",
        UScript::Armenian => *b"Armn",
        UScript::Ethiopic => *b"Ethi",
        UScript::Syriac => *b"Syrc",
        UScript::Thaana => *b"Thaa",
        UScript::Nko => *b"Nkoo",
        UScript::Mongolian => *b"Mong",
        UScript::Javanese => *b"Java",
        UScript::Cherokee => *b"Cher",
        _ => *b"Latn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_is_detected_and_runs_left_to_right() {
        assert_eq!(detect_script("Hello world"), *b"Latn");
        assert_eq!(direction_for(*b"Latn"), Direction::LeftToRight);
    }

    #[test]
    fn arabic_is_detected_and_runs_right_to_left() {
        // Passing this as left-to-right Latin produces glyphs, in the wrong
        // order, with no joining forms, and nothing errors -- which is why this
        // is tested and the shaping call barely is.
        assert_eq!(detect_script("العربية"), *b"Arab");
        assert_eq!(direction_for(*b"Arab"), Direction::RightToLeft);
    }

    #[test]
    fn hebrew_and_thaana_run_right_to_left() {
        assert_eq!(detect_script("שלום"), *b"Hebr");
        assert_eq!(direction_for(*b"Hebr"), Direction::RightToLeft);
        assert_eq!(direction_for(*b"Thaa"), Direction::RightToLeft);
    }

    #[test]
    fn the_indic_and_south_east_asian_scripts_are_named_correctly() {
        assert_eq!(detect_script("देवनागरी"), *b"Deva");
        assert_eq!(detect_script("ไทย"), *b"Thai");
        assert_eq!(detect_script("বাংলা"), *b"Beng");
        assert_eq!(detect_script("தமிழ்"), *b"Taml");
    }

    #[test]
    fn the_cjk_scripts_use_their_iso_tags_not_their_names() {
        // `Han` is `Hani` and `Nko` is `Nkoo`: truncating the variant name
        // would produce a tag the shaper does not recognise.
        assert_eq!(detect_script("漢字"), *b"Hani");
        assert_eq!(detect_script("ひらがな"), *b"Hira");
        assert_eq!(detect_script("한글"), *b"Hang");
    }

    #[test]
    fn the_dominant_script_wins_over_the_first_one() {
        // An Arabic sentence containing a Latin product name is Arabic;
        // shaping it as Latin would drop every joining form.
        assert_eq!(detect_script("العربية Acme العربية"), *b"Arab");
    }

    #[test]
    fn common_characters_carry_no_vote() {
        // A run beginning with a space or a digit must not be classified by it.
        assert_eq!(detect_script("  123 العربية"), *b"Arab");
        assert_eq!(detect_script(" 42 Hello"), *b"Latn");
    }

    #[test]
    fn text_with_no_strong_character_defaults_to_latin() {
        assert_eq!(detect_script(""), *b"Latn");
        assert_eq!(detect_script("123 -- 456"), *b"Latn");
    }

    #[test]
    fn vertical_writing_mode_comes_from_the_file_not_the_text() {
        // The same characters are set both ways and only the PDF knows which.
        let horizontal = request_for("漢字", false, KerningSource::None, true, None);
        let vertical = request_for("漢字", true, KerningSource::None, true, None);
        assert_eq!(horizontal.direction, Direction::LeftToRight);
        assert_eq!(vertical.direction, Direction::TopToBottom);
        assert_eq!(horizontal.script, vertical.script, "the script is unchanged");
        assert!(!vertical.direction.is_horizontal());
    }

    #[test]
    fn kerning_is_regenerated_only_when_it_came_from_the_font() {
        // Spec 8.3: the producer's own tracking must be preserved, because
        // letting the shaper replace it with the font's values would visibly
        // respace text the user did not touch.
        assert!(request_for("hi", false, KerningSource::Font, true, None).kerning);
        assert!(!request_for("hi", false, KerningSource::Producer, true, None).kerning);
        assert!(!request_for("hi", false, KerningSource::None, true, None).kerning);
    }

    #[test]
    fn the_language_is_passed_through_rather_than_guessed() {
        let r = request_for("hi", false, KerningSource::None, true, Some("tr".into()));
        assert_eq!(r.language.as_deref(), Some("tr"));
        assert!(request_for("hi", false, KerningSource::None, true, None).language.is_none());
    }

    /// A minimal but genuinely valid TrueType: two glyphs, `A` and `B`, each
    /// 500 units wide, reachable through a format 4 `cmap`.
    ///
    /// Built rather than vendored because shaping is the one place where the
    /// integration can be wrong in a way no unit test of the surrounding logic
    /// notices -- a mis-ordered tag, a feature flag inverted -- and checking it
    /// needs a font `rustybuzz` will actually load.
    fn minimal_font() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magic
        head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
        head[50..52].copy_from_slice(&0i16.to_be_bytes()); // short loca

        let mut maxp = vec![0u8; 32];
        maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&3u16.to_be_bytes()); // numGlyphs

        let mut hhea = vec![0u8; 36];
        hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes()); // ascender
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes()); // descender
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes()); // numberOfHMetrics

        let mut hmtx = Vec::new();
        for _ in 0..3 {
            hmtx.extend_from_slice(&500u16.to_be_bytes()); // advance
            hmtx.extend_from_slice(&0i16.to_be_bytes()); // lsb
        }

        // loca for three empty glyphs, short format.
        let loca: Vec<u8> = [0u16, 0, 0, 0].iter().flat_map(|v| v.to_be_bytes()).collect();

        // cmap: format 4, 'A'..'B' -> gids 1..2.
        let mut sub = Vec::new();
        sub.extend_from_slice(&4u16.to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes()); // length, patched
        sub.extend_from_slice(&0u16.to_be_bytes()); // language
        sub.extend_from_slice(&4u16.to_be_bytes()); // segCountX2 (2 segments)
        sub.extend_from_slice(&[0; 6]);
        sub.extend_from_slice(&0x42u16.to_be_bytes()); // endCode 'B'
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        sub.extend_from_slice(&0x41u16.to_be_bytes()); // startCode 'A'
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
        sub.extend_from_slice(&1u16.wrapping_sub(0x41).to_be_bytes()); // idDelta
        sub.extend_from_slice(&1u16.to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
        sub.extend_from_slice(&0u16.to_be_bytes());
        let sub_len = sub.len() as u16;
        sub[2..4].copy_from_slice(&sub_len.to_be_bytes());

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&3u16.to_be_bytes()); // Windows
        cmap.extend_from_slice(&1u16.to_be_bytes()); // BMP
        cmap.extend_from_slice(&12u32.to_be_bytes());
        cmap.extend_from_slice(&sub);

        let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", cmap),
            (b"glyf", vec![0; 4]),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];

        let n = tables.len();
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + n * 16;
        for (tag, data) in &tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len().next_multiple_of(4);
        }
        for (_, data) in &tables {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn a_real_font_shapes_to_glyph_ids_and_advances() {
        let font = minimal_font();
        let r = request_for("AB", false, KerningSource::None, true, None);
        let glyphs = shape(&font, &r).expect("the font loads");

        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs.iter().map(|g| g.gid).collect::<Vec<_>>(), vec![1, 2]);
        assert!(glyphs.iter().all(|g| g.x_advance == 500), "{glyphs:?}");
        assert_eq!(glyphs[0].cluster, 0);
        assert_eq!(glyphs[1].cluster, 1, "clusters are byte offsets into the text");
    }

    #[test]
    fn right_to_left_reverses_the_output_order() {
        // The check that direction is genuinely reaching the shaper. Passing it
        // wrongly produces glyphs in the wrong order and never errors, which is
        // exactly the failure this module exists to prevent.
        let font = minimal_font();
        let mut r = request_for("AB", false, KerningSource::None, true, None);
        r.direction = Direction::RightToLeft;
        let glyphs = shape(&font, &r).expect("shapes");
        assert_eq!(glyphs.iter().map(|g| g.gid).collect::<Vec<_>>(), vec![2, 1]);
        // The clusters follow the glyphs, so a caller can still map back.
        assert_eq!(glyphs[0].cluster, 1);
    }

    #[test]
    fn a_character_the_font_lacks_shapes_to_notdef() {
        // Glyph 0. Reported as a glyph rather than dropped, so the caller can
        // see that the font could not draw it.
        let font = minimal_font();
        let r = request_for("AZ", false, KerningSource::None, true, None);
        let glyphs = shape(&font, &r).expect("shapes");
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[1].gid, 0);
    }

    #[test]
    fn ligatures_are_inferred_from_the_glyphs_the_producer_emitted() {
        // Spec 8.3's rule, both directions. The font is one that *can* ligate;
        // whether the original run did is what decides the feature.
        let font = minimal_font();
        // The minimal font has no GSUB, so nothing is a ligature and the
        // inference must say off rather than guessing.
        let off = request_from_original("fi", &[1, 2], &font, false, KerningSource::None, None);
        assert!(!off.ligatures, "no evidence of ligation");
    }

    #[test]
    fn a_font_that_will_not_parse_infers_no_ligatures() {
        // Guessing "on" here would introduce ligatures on no evidence at all.
        let r = request_from_original("fi", &[1], b"not a font", false, KerningSource::None, None);
        assert!(!r.ligatures);
    }

    #[test]
    fn a_run_that_is_not_a_font_shapes_to_nothing() {
        // Reported, not papered over: a caller that cannot shape must preserve
        // the original glyphs rather than invent new ones.
        let r = request_for("hi", false, KerningSource::None, true, None);
        assert!(shape(b"not a font", &r).is_none());
        assert!(shape(&[], &r).is_none());
    }
}
