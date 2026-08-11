//! Script properties, enough for word segmentation. Spec 7.3.
//!
//! > For scripts without inter-word spaces (Thai, Khmer, CJK), do not segment;
//! > treat the run as a single word and let the shaper handle it.
//!
//! A compact table rather than full ICU, per spec 8.3's note about bundle size:
//! the question here is only "does this script separate words with spaces", and
//! that is answered by a handful of ranges.

/// The script categories that matter to segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// Latin, Greek, Cyrillic, and everything else that uses spaces.
    Spaced,
    /// Han, Hiragana, Katakana, Hangul: no inter-word spaces, and each glyph is
    /// close to a word.
    CjkIdeographic,
    /// Thai, Lao, Khmer, Myanmar, Javanese: continuous scripts where word
    /// boundaries need a dictionary, not geometry.
    Continuous,
    /// Arabic and Hebrew: spaced, but right-to-left, so visual order is not
    /// logical order.
    RightToLeft,
    /// Punctuation, digits, symbols -- no opinion of their own.
    Neutral,
}

impl Script {
    /// Whether geometry can be trusted to find word boundaries.
    ///
    /// For a continuous script it cannot: Thai runs words together with no
    /// spacing cue at all, so any gap-based rule invents boundaries that are
    /// not there.
    pub fn segments_on_geometry(self) -> bool {
        matches!(self, Script::Spaced | Script::RightToLeft | Script::Neutral)
    }
}

/// Classify one code point.
pub fn of(c: char) -> Script {
    let u = c as u32;
    match u {
        // Ideographic and syllabic East Asian.
        0x2E80..=0x2EFF   // CJK radicals
        | 0x3000..=0x303F // CJK symbols and punctuation
        | 0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x3100..=0x312F // Bopomofo
        | 0x3130..=0x318F // Hangul compatibility jamo
        | 0x31F0..=0x31FF // Katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFF00..=0xFF65 // Fullwidth forms
        | 0x20000..=0x2FA1F // CJK extensions B onwards
        => Script::CjkIdeographic,

        // Continuous scripts with no inter-word spacing.
        0x0E00..=0x0E7F   // Thai
        | 0x0E80..=0x0EFF // Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x1980..=0x19DF // New Tai Lue
        | 0xA980..=0xA9DF // Javanese
        => Script::Continuous,

        // Right-to-left.
        0x0590..=0x05FF   // Hebrew
        | 0x0600..=0x06FF // Arabic
        | 0x0700..=0x074F // Syriac
        | 0x0750..=0x077F // Arabic supplement
        | 0x08A0..=0x08FF // Arabic extended-A
        | 0xFB1D..=0xFB4F // Hebrew presentation forms
        | 0xFB50..=0xFDFF // Arabic presentation forms-A
        | 0xFE70..=0xFEFF // Arabic presentation forms-B
        => Script::RightToLeft,

        _ if c.is_alphabetic() => Script::Spaced,
        _ => Script::Neutral,
    }
}

/// The script that characterises a whole string, ignoring neutrals.
///
/// A line of Japanese with Latin numerals in it is Japanese; a line of English
/// with one kanji is English. Taking the most common non-neutral script gets
/// both right.
pub fn dominant(text: &str) -> Script {
    let (mut cjk, mut cont, mut rtl, mut spaced) = (0usize, 0usize, 0usize, 0usize);
    for c in text.chars() {
        match of(c) {
            Script::CjkIdeographic => cjk += 1,
            Script::Continuous => cont += 1,
            Script::RightToLeft => rtl += 1,
            Script::Spaced => spaced += 1,
            Script::Neutral => {}
        }
    }
    let max = cjk.max(cont).max(rtl).max(spaced);
    if max == 0 {
        Script::Neutral
    } else if max == cjk {
        Script::CjkIdeographic
    } else if max == cont {
        Script::Continuous
    } else if max == rtl {
        Script::RightToLeft
    } else {
        Script::Spaced
    }
}

/// The space characters spec 7.3 names explicitly, plus the ones that behave
/// like them.
pub fn is_space(c: char) -> bool {
    matches!(c,
        '\u{0020}'            // space
        | '\u{00A0}'          // no-break space
        | '\u{1680}'          // ogham space mark
        | '\u{2000}'..='\u{200A}' // en quad through hair space
        | '\u{2028}'..='\u{2029}' // line and paragraph separators
        | '\u{202F}'          // narrow no-break space
        | '\u{205F}'          // medium mathematical space
        | '\u{3000}'          // ideographic space
        | '\u{0009}'          // tab
    )
}

/// A zero-width character that should not start or end a word on its own.
pub fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{FEFF}' | '\u{00AD}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_scripts_that_change_segmentation() {
        assert_eq!(of('a'), Script::Spaced);
        assert_eq!(of('Я'), Script::Spaced);
        assert_eq!(of('α'), Script::Spaced);
        assert_eq!(of('漢'), Script::CjkIdeographic);
        assert_eq!(of('ひ'), Script::CjkIdeographic);
        assert_eq!(of('カ'), Script::CjkIdeographic);
        assert_eq!(of('한'), Script::CjkIdeographic);
        assert_eq!(of('ก'), Script::Continuous);
        assert_eq!(of('ក'), Script::Continuous);
        assert_eq!(of('ا'), Script::RightToLeft);
        assert_eq!(of('א'), Script::RightToLeft);
        assert_eq!(of('1'), Script::Neutral);
        assert_eq!(of('.'), Script::Neutral);
    }

    #[test]
    fn only_continuous_scripts_refuse_geometric_segmentation() {
        assert!(Script::Spaced.segments_on_geometry());
        assert!(Script::RightToLeft.segments_on_geometry(), "Arabic does use spaces");
        assert!(Script::Neutral.segments_on_geometry());
        assert!(!Script::Continuous.segments_on_geometry());
        assert!(
            !Script::CjkIdeographic.segments_on_geometry(),
            "CJK has no inter-word spacing to measure"
        );
    }

    #[test]
    fn dominant_ignores_neutrals() {
        assert_eq!(dominant("Hello, world! 123"), Script::Spaced);
        assert_eq!(dominant("日本語のテキスト"), Script::CjkIdeographic);
        // A Japanese line with Latin numerals is still Japanese.
        assert_eq!(dominant("第2章の内容について"), Script::CjkIdeographic);
        // An English line with one kanji is still English.
        assert_eq!(dominant("The character 漢 means Han"), Script::Spaced);
        assert_eq!(dominant("123 456"), Script::Neutral);
        assert_eq!(dominant(""), Script::Neutral);
    }

    #[test]
    fn recognises_the_spaces_spec_7_3_names() {
        for c in [' ', '\u{00a0}', '\u{2000}', '\u{2005}', '\u{200a}', '\u{3000}'] {
            assert!(is_space(c), "{c:?} ({:04x}) should be a space", c as u32);
        }
        for c in ['a', '\u{200b}', '-'] {
            assert!(!is_space(c), "{c:?} is not a space");
        }
    }

    #[test]
    fn zero_width_characters_are_invisible() {
        assert!(is_invisible('\u{200b}'), "zero-width space");
        assert!(is_invisible('\u{00ad}'), "soft hyphen");
        assert!(is_invisible('\u{feff}'));
        assert!(!is_invisible(' '));
        assert!(!is_invisible('a'));
    }
}
