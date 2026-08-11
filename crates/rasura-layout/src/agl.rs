//! Glyph name to Unicode. Spec 7.2 step 2 and step 6.
//!
//! Q1 measured this as the load-bearing component of the whole derivation
//! chain: of the 653 fonts in the corpus with no usable `/ToUnicode`, 300
//! resolve through the Adobe Glyph List and nothing else, while only six fonts
//! in 1390 carry the opaque `g34`-style names the spec expected to dominate.
//!
//! So the table is the complete AGL, not a selection of common names, and the
//! heuristics around it are cheap additions rather than the main event.

use crate::glyphdata::AGL;

/// Resolve a glyph name to the text it represents.
///
/// Tries, in order:
///
/// 1. the Adobe Glyph List proper,
/// 2. the `uniXXXX` / `uXXXXX` conventions, which encode the code point,
/// 3. the `name.alt` suffix convention, where `f_i.sc` is a styled variant of
///    `f_i` and carries the same text,
/// 4. ligature names joined by underscores, `f_i` being "fi",
/// 5. `Xnn` numeric fallbacks that some producers emit for ASCII.
///
/// Returns `None` for a name that carries no information -- `g34`, `cid7`,
/// `index12` -- because inventing text for those is exactly the silent
/// degradation spec 2 forbids.
pub fn lookup(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if let Some(text) = exact(name) {
        return Some(text.to_string());
    }
    if let Some(text) = uni_convention(name) {
        return Some(text);
    }

    // `name.alt`: a variant of a base glyph. The suffix is presentational --
    // small caps, oldstyle figures, a swash -- so the text is the base's.
    if let Some((base, _suffix)) = name.split_once('.')
        && !base.is_empty()
        && let Some(text) = lookup_without_suffix(base)
    {
        return Some(text);
    }

    // Underscore-joined ligature components: `f_i`, `f_f_l`.
    if name.contains('_') {
        let parts: Vec<&str> = name.split('_').filter(|p| !p.is_empty()).collect();
        if parts.len() > 1 {
            let mut out = String::new();
            // All or nothing: a ligature name whose components do not all
            // resolve is not a ligature this table knows, and half of one is
            // worse than none.
            for part in &parts {
                out.push_str(&lookup_without_suffix(part)?);
            }
            if !out.is_empty() {
                return Some(out);
            }
        }
    }

    None
}

/// The parts of `lookup` that do not recurse into suffix or ligature handling,
/// so a pathological name cannot loop.
fn lookup_without_suffix(name: &str) -> Option<String> {
    if let Some(text) = exact(name) {
        return Some(text.to_string());
    }
    if let Some(text) = uni_convention(name) {
        return Some(text);
    }
    // A bare `.`-suffixed component inside a ligature name.
    if let Some((base, _)) = name.split_once('.')
        && let Some(text) = exact(base)
    {
        return Some(text.to_string());
    }
    None
}

/// The Adobe Glyph List proper.
pub fn exact(name: &str) -> Option<&'static str> {
    AGL.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| AGL[i].1)
}

/// The glyph name for a character: the Adobe Glyph List, run backwards.
///
/// Needed by anything that *writes* a `/Differences` array — §9.6.6.4 says a
/// TrueType simple font resolves a difference name through the AGL before its
/// `cmap` sees it, so the name has to be the one the list uses. `Eacute`, not
/// `uni00C9`, and certainly not a name invented here.
///
/// A linear scan, deliberately. The table is sorted by name, so there is no
/// index to binary-search on the value, and building a reverse map would cost
/// 4,281 allocations to serve a call that happens once per injected character.
/// The first match wins, which matters because several names share a character
/// — `Delta` and `uni2206` are both `U+2206` — and the AGL's own ordering is
/// the only tie-break available.
pub fn name_of(c: char) -> Option<&'static str> {
    let mut buffer = [0u8; 4];
    let target: &str = c.encode_utf8(&mut buffer);
    AGL.iter().find(|(_, value)| *value == target).map(|(name, _)| *name)
}

/// `uniXXXX` (one or more UTF-16 code units) and `uXXXX`..`uXXXXXX`.
fn uni_convention(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix("uni")
        && rest.len() >= 4
        && rest.len() % 4 == 0
        && rest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        // Successive UTF-16 code units, so a surrogate pair must be joined.
        let units: Vec<u16> = rest
            .as_bytes()
            .chunks(4)
            .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
            .collect();
        let text = String::from_utf16_lossy(&units);
        return (!text.is_empty() && !text.contains('\u{fffd}')).then_some(text);
    }

    if let Some(rest) = name.strip_prefix('u')
        && (4..=6).contains(&rest.len())
        && rest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let v = u32::from_str_radix(rest, 16).ok()?;
        return char::from_u32(v).map(|c| c.to_string());
    }

    None
}

/// True for names that carry no information about what the glyph means.
///
/// These are what §7.2 step 6 exists for, and what a shape-matching fallback
/// would be for if the corpus justified one. It does not: six fonts in 1390.
pub fn is_opaque(name: &str) -> bool {
    for prefix in ["cid", "glyph", "index", "g"] {
        if let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ordinary_names() {
        assert_eq!(lookup("A").as_deref(), Some("A"));
        assert_eq!(lookup("space").as_deref(), Some(" "));
        assert_eq!(lookup("adieresis").as_deref(), Some("ä"));
        assert_eq!(lookup("germandbls").as_deref(), Some("ß"));
        assert_eq!(lookup("bullet").as_deref(), Some("\u{2022}"));
    }

    #[test]
    fn resolves_the_ligatures_that_break_extraction() {
        // The characters that vanish from a document with no /ToUnicode.
        assert_eq!(lookup("fi").as_deref(), Some("\u{fb01}"));
        assert_eq!(lookup("fl").as_deref(), Some("\u{fb02}"));
        assert_eq!(lookup("ffi").as_deref(), Some("\u{fb03}"));
    }

    #[test]
    fn resolves_the_uni_conventions() {
        assert_eq!(lookup("uni0041").as_deref(), Some("A"));
        assert_eq!(lookup("uni00E9").as_deref(), Some("é"));
        assert_eq!(lookup("u0041").as_deref(), Some("A"));
        assert_eq!(lookup("u1D400").as_deref(), Some("\u{1d400}"));
        // Multiple UTF-16 units, including a surrogate pair.
        assert_eq!(lookup("uni0066006A").as_deref(), Some("fj"));
        assert_eq!(lookup("uniD835DC00").as_deref(), Some("\u{1d400}"));
    }

    #[test]
    fn rejects_malformed_uni_names() {
        assert_eq!(lookup("uni00"), None, "too short");
        assert_eq!(lookup("uni00411"), None, "not a multiple of four");
        assert_eq!(lookup("uniZZZZ"), None, "not hex");
        assert_eq!(lookup("u12"), None);
    }

    #[test]
    fn variant_suffixes_carry_the_base_text() {
        // `A.sc` is small-cap A; the text is still "A".
        assert_eq!(lookup("A.sc").as_deref(), Some("A"));
        assert_eq!(lookup("one.oldstyle").as_deref(), Some("1"));
        assert_eq!(lookup("uni0041.alt2").as_deref(), Some("A"));
    }

    #[test]
    fn underscore_ligature_names_join_their_components() {
        assert_eq!(lookup("f_i").as_deref(), Some("fi"));
        assert_eq!(lookup("f_f_l").as_deref(), Some("ffl"));
        assert_eq!(lookup("f_i.sc").as_deref(), Some("fi"));
    }

    #[test]
    fn opaque_names_resolve_to_nothing() {
        // The spec's feared case. Inventing text here would be the silent
        // degradation spec 2 forbids.
        for name in ["g34", "cid7", "index12", "glyph200", "42"] {
            assert_eq!(lookup(name), None, "{name} must not resolve");
            assert!(is_opaque(name), "{name} should be recognised as opaque");
        }
    }

    #[test]
    fn real_names_are_not_mistaken_for_opaque_ones() {
        // `g` is a real glyph name; `gamma` and `guillemotleft` start with it.
        for name in ["g", "gamma", "guillemotleft", "germandbls", "cid"] {
            assert!(!is_opaque(name), "{name} is a real name");
        }
        assert_eq!(lookup("g").as_deref(), Some("g"));
    }

    #[test]
    fn the_table_is_sorted_and_complete_enough() {
        // Binary search depends on the ordering, and a generator change that
        // broke it would otherwise fail silently on some names only.
        for pair in crate::glyphdata::AGL.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} then {}", pair[0].0, pair[1].0);
        }
        assert!(crate::glyphdata::AGL.len() > 4000, "this must be the whole AGL");
    }

    #[test]
    fn every_agl_entry_resolves_through_lookup() {
        for (name, expected) in crate::glyphdata::AGL.iter() {
            assert_eq!(lookup(name).as_deref(), Some(*expected), "{name}");
        }
    }
}
