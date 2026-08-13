//! Put a font the document has never seen into it. ISO 32000-1 §9.6, §9.7.
//!
//! [`crate::embed`] amends a font a document already carries: it widens
//! `/Widths`, appends to `/Differences`, merges `/ToUnicode`. Every value it
//! needs is already in the file. This module has no such luxury — there is no
//! font dictionary, no descriptor, no `/FontFile2`, and no encoding. All of it
//! has to be produced from the font program, which is what
//! [`crate::describe`] is for.
//!
//! # Simple or composite, decided by the text
//!
//! A PDF simple font addresses glyphs by single byte, so it can reach at most
//! 256 of them and only through an encoding. WinAnsi (Annex D) is the one
//! every viewer agrees on, and if every character being embedded has a WinAnsi
//! code then a `/TrueType` font is the better output: smaller, more
//! compatible, and readable by tools that never learned about CID fonts.
//!
//! The moment one character does not — a Greek letter, a CJK ideograph, an
//! arrow — a simple font cannot express it at all. Then this emits a Type0
//! font with `/Identity-H`, where a code *is* a glyph id and the 256-glyph
//! ceiling disappears.
//!
//! The choice is made from the characters, not asked of the caller, because a
//! caller who has to know which of the two their string needs is being asked
//! about PDF internals to write a sentence. [`Embedded::composite`] reports
//! which was used.
//!
//! # What is not here
//!
//! CFF outlines. A `/FontFile3` with an OpenType subtype is a different stream
//! and a different subsetter, and [`crate::describe::check_embeddable`]
//! declines them by name rather than writing CFF bytes into a key that claims
//! TrueType — which would pass every structural check and render nothing.

use crate::cmap::Cmap;
use crate::cmap_write;
use crate::describe::{self, Description, to_glyph_space};
use crate::embed::to_unicode_cmap_with;
use crate::error::{FontError, Result};
use crate::sfnt::Sfnt;
use crate::subset;
use rasura_cos::object::{Dictionary, Name, ObjId, Object, PdfString, Stream};
use std::collections::{BTreeMap, BTreeSet};

/// How to embed.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Characters that must be drawable. Anything outside this set is not in
    /// the subset and cannot be shown later without embedding again.
    pub characters: BTreeSet<char>,
    /// Overrides the name from the font's own `name` table. Only needed when
    /// the font has none.
    pub base_font: Option<String>,
    /// Emit a Type0 font even when the text would fit a simple one.
    ///
    /// Useful for a document that will accumulate text later: a simple font
    /// cannot grow past 256 glyphs, and finding that out after the fact means
    /// re-embedding.
    pub force_composite: bool,
}

impl Options {
    /// Embed exactly the characters this text uses.
    pub fn for_text(text: &str) -> Options {
        Options {
            characters: text.chars().filter(|c| !c.is_control()).collect(),
            ..Options::default()
        }
    }
}

/// A font, ready to be put in a document.
#[derive(Debug, Clone)]
pub struct Embedded {
    /// Every object to write, the font dictionary first.
    pub objects: Vec<(ObjId, Object)>,
    /// The id to name in a page's `/Resources /Font`.
    pub font: ObjId,
    /// `/BaseFont`, subset tag and all.
    pub base_font: String,
    /// True when this is a Type0 font and text must be written as two-byte
    /// glyph ids rather than as bytes. [`Embedded::encode`] handles both.
    pub composite: bool,
    /// Characters asked for that the font has no glyph for. They are not in
    /// the subset and [`Embedded::encode`] drops them — reported rather than
    /// silently substituted, as spec 2 requires.
    pub missing: Vec<char>,
    /// The descriptor that was written, including whether its `/StemV` and
    /// `/FontBBox` were measured or estimated.
    pub description: Description,
    /// Character to the code that draws it, and its width in glyph space.
    metrics: BTreeMap<char, (u16, f64)>,
}

impl Embedded {
    /// Encode text for a `Tj`, in whatever the font's code space is.
    ///
    /// Characters with no glyph are dropped and counted. A caller that needs to
    /// know before drawing has [`Embedded::missing`].
    pub fn encode(&self, text: &str) -> (Vec<u8>, usize) {
        let mut out = Vec::new();
        let mut dropped = 0;
        for ch in text.chars() {
            match self.metrics.get(&ch) {
                Some((code, _)) if self.composite => out.extend_from_slice(&code.to_be_bytes()),
                Some((code, _)) => out.push(*code as u8),
                None => dropped += 1,
            }
        }
        (out, dropped)
    }

    /// Width of `text` at `size`, in points.
    ///
    /// The same numbers written into `/Widths`, so a line measured with this
    /// and drawn with this font is the width it was measured at — which is the
    /// property the layout engine's `Measurer` exists to provide and the one
    /// that makes justified text land on the margin.
    pub fn width(&self, text: &str, size: f64) -> f64 {
        let total: f64 = text
            .chars()
            .map(|c| self.metrics.get(&c).map_or(self.description.missing_width, |(_, w)| *w))
            .sum();
        total / 1000.0 * size
    }

    /// Every character this font can draw.
    pub fn covers(&self, ch: char) -> bool {
        self.metrics.contains_key(&ch)
    }
}

/// Build the font objects for `program`, allocating ids from `next_id`.
///
/// Nothing is written to a document here. The caller applies `objects` through
/// an edit session, so an embedding is as undoable as any other operation.
pub fn embed_truetype(
    program: &[u8],
    opts: &Options,
    mut next_id: impl FnMut() -> ObjId,
) -> Result<Embedded> {
    let font = Sfnt::parse(program)?;
    describe::check_embeddable(&font)?;

    let cmap = Cmap::parse(program, &font).ok_or(FontError::MissingTable("cmap"))?;
    // A font with only a (3,0) symbol table addresses glyphs by a code in the
    // 0xF000 range, not by character. Embedding one for arbitrary text would
    // need the caller to know that mapping, which they do not.
    let unicode = cmap.best_unicode().ok_or(FontError::Unsupported(
        "the font has no Unicode cmap, so characters cannot be located",
    ))?;

    // --- which glyphs -------------------------------------------------------
    let mut wanted: BTreeMap<char, u16> = BTreeMap::new();
    let mut missing = Vec::new();
    for &ch in &opts.characters {
        match unicode.lookup(program, ch as u32).filter(|g| *g != 0) {
            Some(gid) => {
                wanted.insert(ch, gid);
            }
            // A space with no glyph is normal and not worth reporting: it draws
            // nothing either way, and every font that has one puts it at a
            // code the encoding already covers.
            None if ch == ' ' => {}
            None => missing.push(ch),
        }
    }
    if wanted.is_empty() {
        return Err(FontError::Unsupported("the font has no glyph for any character requested"));
    }

    // --- subset -------------------------------------------------------------
    let gids: Vec<u16> = wanted.values().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let reduced = subset::compact_truetype(program, &gids)?;

    // `compact_truetype` drops `cmap` -- it is rebuilding outlines, not
    // encodings. A simple font needs one back, because a viewer resolves a
    // WinAnsi code to a glyph *name* to a character and then asks the font's
    // own cmap for the glyph. Without it the text is invisible: this is the
    // same failure that made an injected glyph draw nothing, found then only
    // by rendering.
    //
    // A composite font addresses glyphs directly and needs no cmap at all, but
    // one costs a few hundred bytes and makes the embedded program usable on
    // its own, which is worth more than the space.
    let renumber = |gid: u16| reduced.mapping.get(&gid).copied().unwrap_or(0);
    let remapped: Vec<(u32, u16)> =
        wanted.iter().map(|(ch, gid)| (*ch as u32, renumber(*gid))).collect();
    let subset_font = Sfnt::parse(&reduced.bytes)?;
    let program_bytes = cmap_write::add_mappings(&reduced.bytes, &subset_font, &remapped)?;

    // --- the descriptor -----------------------------------------------------
    //
    // Described from the *original* program, not the subset: subsetting keeps
    // `head`, `OS/2`, `post` and `name` verbatim, but reading the original is
    // the honest source for metrics that describe the typeface rather than
    // this particular cut of it.
    let mut description = describe::describe(program, &font)?;
    let name = opts
        .base_font
        .clone()
        .or_else(|| description.postscript_name.clone())
        .unwrap_or_else(|| "Embedded".to_string());
    let base_font = format!("{}+{}", describe::subset_tag(&name, &gids), name);

    let composite = opts.force_composite || wanted.keys().any(|c| win_ansi_code(*c).is_none());
    if composite {
        // Addressed by glyph id through /Identity-H rather than through an
        // encoding, which is §9.8.1's definition of symbolic.
        description = description.as_symbolic();
    }

    let per_em = font.units_per_em;
    let width_of =
        |gid: u16| to_glyph_space(f64::from(font.advance(program, gid).unwrap_or(0)), per_em);

    let file_id = next_id();
    let descriptor_id = next_id();
    let to_unicode_id = next_id();
    let font_id = next_id();

    let mut objects = Vec::new();

    // --- /FontFile2 ---------------------------------------------------------
    //
    // /Length1 is the uncompressed length, which is what a reader uses to know
    // where the sfnt ends. The writer computes /Length.
    let mut file = Stream::new(Dictionary::new(), Vec::new());
    file.dict.insert("Length1", Object::Integer(program_bytes.len() as i64));
    file.set_decoded(program_bytes);

    // --- /FontDescriptor ----------------------------------------------------
    let mut descriptor = Dictionary::new();
    descriptor.insert("Type", Object::name("FontDescriptor"));
    descriptor.insert("FontName", Object::Name(Name::new(&base_font)));
    descriptor.insert("Flags", Object::Integer(i64::from(description.flags)));
    descriptor.insert(
        "FontBBox",
        Object::Array(description.bbox.iter().map(|v| Object::Real(v.round())).collect()),
    );
    descriptor.insert("ItalicAngle", Object::Real(description.italic_angle));
    descriptor.insert("Ascent", Object::Real(description.ascent.round()));
    descriptor.insert("Descent", Object::Real(description.descent.round()));
    descriptor.insert("CapHeight", Object::Real(description.cap_height.round()));
    descriptor.insert("StemV", Object::Real(description.stem_v.round()));
    if let Some(x) = description.x_height {
        descriptor.insert("XHeight", Object::Real(x.round()));
    }
    descriptor.insert("MissingWidth", Object::Real(description.missing_width.round()));
    descriptor.insert("FontFile2", Object::Reference(file_id));

    let mut metrics: BTreeMap<char, (u16, f64)> = BTreeMap::new();
    let mut to_unicode: Vec<(u32, String)> = Vec::new();

    let font_dict = if composite {
        let descendant_id = next_id();
        for (ch, gid) in &wanted {
            let code = renumber(*gid);
            metrics.insert(*ch, (code, width_of(*gid)));
            to_unicode.push((u32::from(code), ch.to_string()));
        }
        objects.push((
            descendant_id,
            Object::Dictionary(descendant(&base_font, descriptor_id, &metrics)),
        ));
        composite_parent(&base_font, descendant_id, to_unicode_id)
    } else {
        let mut widths: BTreeMap<u8, f64> = BTreeMap::new();
        for (ch, gid) in &wanted {
            // Unwrapped safely: `composite` is false exactly when every
            // character has a code.
            let code = win_ansi_code(*ch).expect("checked above");
            let width = width_of(*gid);
            metrics.insert(*ch, (u16::from(code), width));
            widths.insert(code, width);
            to_unicode.push((u32::from(code), ch.to_string()));
        }
        simple(&base_font, descriptor_id, to_unicode_id, &widths, description.missing_width)
    };

    let mut unicode_stream = Stream::new(Dictionary::new(), Vec::new());
    unicode_stream.set_decoded(to_unicode_cmap_with(&to_unicode, if composite { 2 } else { 1 }));

    objects.push((font_id, Object::Dictionary(font_dict)));
    objects.push((descriptor_id, Object::Dictionary(descriptor)));
    objects.push((file_id, Object::Stream(file)));
    objects.push((to_unicode_id, Object::Stream(unicode_stream)));

    Ok(Embedded { objects, font: font_id, base_font, composite, missing, description, metrics })
}

/// `/Type /Font /Subtype /TrueType` — a simple font, addressed by byte.
fn simple(
    base_font: &str,
    descriptor: ObjId,
    to_unicode: ObjId,
    widths: &BTreeMap<u8, f64>,
    missing_width: f64,
) -> Dictionary {
    let first = widths.keys().copied().min().unwrap_or(0);
    let last = widths.keys().copied().max().unwrap_or(0);

    let mut dict = Dictionary::new();
    dict.insert("Type", Object::name("Font"));
    dict.insert("Subtype", Object::name("TrueType"));
    dict.insert("BaseFont", Object::Name(Name::new(base_font)));
    dict.insert("FirstChar", Object::Integer(i64::from(first)));
    dict.insert("LastChar", Object::Integer(i64::from(last)));
    // Dense from FirstChar to LastChar, as §9.6.2.1 requires. A code in the
    // range with no glyph gets MissingWidth rather than a gap, because the
    // array is positional and a short one silently shifts every width after it.
    dict.insert(
        "Widths",
        Object::Array(
            (first..=last)
                .map(|code| {
                    Object::Real(widths.get(&code).copied().unwrap_or(missing_width).round())
                })
                .collect(),
        ),
    );
    dict.insert("Encoding", Object::name("WinAnsiEncoding"));
    dict.insert("FontDescriptor", Object::Reference(descriptor));
    dict.insert("ToUnicode", Object::Reference(to_unicode));
    dict
}

/// `/Type /Font /Subtype /Type0` with `/Identity-H`.
fn composite_parent(base_font: &str, descendant: ObjId, to_unicode: ObjId) -> Dictionary {
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::name("Font"));
    dict.insert("Subtype", Object::name("Type0"));
    dict.insert("BaseFont", Object::Name(Name::new(base_font)));
    // Identity-H: a two-byte code is the CID, unchanged. With /CIDToGIDMap
    // /Identity below, the CID is also the glyph id -- which is why the codes
    // this module emits are subset glyph ids.
    dict.insert("Encoding", Object::name("Identity-H"));
    dict.insert("DescendantFonts", Object::Array(vec![Object::Reference(descendant)]));
    dict.insert("ToUnicode", Object::Reference(to_unicode));
    dict
}

/// The `/CIDFontType2` a Type0 font descends to.
fn descendant(
    base_font: &str,
    descriptor: ObjId,
    metrics: &BTreeMap<char, (u16, f64)>,
) -> Dictionary {
    let mut system = Dictionary::new();
    system.insert("Registry", Object::String(PdfString::new_literal(b"Adobe")));
    system.insert("Ordering", Object::String(PdfString::new_literal(b"Identity")));
    system.insert("Supplement", Object::Integer(0));

    // `/W` in the `c [w]` form, one group per CID, ordered. The run-compressing
    // `cFirst cLast w` form would be smaller for a contiguous subset, and the
    // saving is a few hundred bytes on a font of tens of kilobytes.
    let by_cid: BTreeMap<u16, f64> = metrics.values().map(|(cid, w)| (*cid, *w)).collect();
    let mut w = Vec::new();
    for (cid, width) in &by_cid {
        w.push(Object::Integer(i64::from(*cid)));
        w.push(Object::Array(vec![Object::Real(width.round())]));
    }

    let mut dict = Dictionary::new();
    dict.insert("Type", Object::name("Font"));
    dict.insert("Subtype", Object::name("CIDFontType2"));
    dict.insert("BaseFont", Object::Name(Name::new(base_font)));
    dict.insert("CIDSystemInfo", Object::Dictionary(system));
    dict.insert("FontDescriptor", Object::Reference(descriptor));
    dict.insert("DW", Object::Integer(1000));
    dict.insert("W", Object::Array(w));
    dict.insert("CIDToGIDMap", Object::name("Identity"));
    dict
}

/// A character's WinAnsiEncoding code. ISO 32000-1 Annex D.
///
/// Latin-1 over most of the range, but **not** at 0x80..=0x9F, where Latin-1
/// has C1 control codes and WinAnsi has the typographic characters people
/// actually use — curly quotes, dashes, the ellipsis, the Euro. Treating that
/// window as Latin-1 is a real and easy mistake: it turns a right single quote
/// into a control code, and this library already has one function that does
/// exactly that.
pub fn win_ansi_code(ch: char) -> Option<u8> {
    let code = match ch {
        '\u{20AC}' => 0x80,
        '\u{201A}' => 0x82,
        '\u{0192}' => 0x83,
        '\u{201E}' => 0x84,
        '\u{2026}' => 0x85,
        '\u{2020}' => 0x86,
        '\u{2021}' => 0x87,
        '\u{02C6}' => 0x88,
        '\u{2030}' => 0x89,
        '\u{0160}' => 0x8A,
        '\u{2039}' => 0x8B,
        '\u{0152}' => 0x8C,
        '\u{017D}' => 0x8E,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201C}' => 0x93,
        '\u{201D}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '\u{02DC}' => 0x98,
        '\u{2122}' => 0x99,
        '\u{0161}' => 0x9A,
        '\u{203A}' => 0x9B,
        '\u{0153}' => 0x9C,
        '\u{017E}' => 0x9E,
        '\u{0178}' => 0x9F,
        // Printable ASCII and the Latin-1 supplement map to themselves.
        c if (' '..='~').contains(&c) => c as u32 as u8,
        c if ('\u{A0}'..='\u{FF}').contains(&c) => c as u32 as u8,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn ids() -> impl FnMut() -> ObjId {
        let next = Cell::new(10u32);
        move || {
            let n = next.get();
            next.set(n + 1);
            ObjId::new(n, 0)
        }
    }

    fn roboto() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/Roboto-Regular.ttf"))
            .ok()
    }

    #[test]
    fn win_ansi_gets_the_typographic_window_right() {
        // The 0x80..0x9F window is the whole reason this function exists rather
        // than a cast: Latin-1 has control codes here and WinAnsi does not.
        assert_eq!(win_ansi_code('\u{2019}'), Some(0x92), "right single quote");
        assert_eq!(win_ansi_code('\u{2014}'), Some(0x97), "em dash");
        assert_eq!(win_ansi_code('\u{20AC}'), Some(0x80), "euro");
        assert_eq!(win_ansi_code('A'), Some(65));
        assert_eq!(win_ansi_code('é'), Some(0xE9));
        assert_eq!(win_ansi_code('α'), None, "outside WinAnsi entirely");
        assert_eq!(win_ansi_code('中'), None);
    }

    #[test]
    fn latin_text_becomes_a_simple_font() {
        let Some(data) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let embedded =
            embed_truetype(&data, &Options::for_text("Hamburgefonstiv 123"), ids()).unwrap();

        assert!(!embedded.composite, "Latin text fits a simple font");
        assert!(embedded.missing.is_empty(), "{:?}", embedded.missing);
        assert!(embedded.base_font.contains("+Roboto-Regular"), "{}", embedded.base_font);
        assert_eq!(embedded.base_font.len(), 7 + "Roboto-Regular".len());

        let dict = embedded
            .objects
            .iter()
            .find(|(id, _)| *id == embedded.font)
            .and_then(|(_, o)| o.as_dict())
            .expect("the font dictionary");
        assert_eq!(dict.get("Subtype").and_then(Object::as_name), Some(&Name::new("TrueType")));
        assert_eq!(
            dict.get("Encoding").and_then(Object::as_name),
            Some(&Name::new("WinAnsiEncoding"))
        );

        // /Widths must be dense from /FirstChar to /LastChar, or every width
        // after a gap belongs to the wrong character.
        let first = dict.get("FirstChar").and_then(Object::as_i64).unwrap();
        let last = dict.get("LastChar").and_then(Object::as_i64).unwrap();
        let widths = dict.get("Widths").and_then(Object::as_array).unwrap();
        assert_eq!(widths.len() as i64, last - first + 1);

        // A code encodes to itself, and the width is the font's.
        let (codes, dropped) = embedded.encode("Ham");
        assert_eq!(codes, b"Ham");
        assert_eq!(dropped, 0);
        assert!(embedded.width("Ham", 12.0) > 0.0);
    }

    #[test]
    fn text_outside_win_ansi_becomes_a_composite_font() {
        let Some(data) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        // Greek: Roboto has these glyphs, and WinAnsi has no codes for them.
        let embedded = embed_truetype(&data, &Options::for_text("Ελλάδα"), ids()).unwrap();

        assert!(embedded.composite, "Greek cannot be a simple font");
        assert!(
            embedded.description.is_symbolic(),
            "Identity-H is addressed by glyph, not encoding"
        );

        let dict = embedded
            .objects
            .iter()
            .find(|(id, _)| *id == embedded.font)
            .and_then(|(_, o)| o.as_dict())
            .expect("the font dictionary");
        assert_eq!(dict.get("Subtype").and_then(Object::as_name), Some(&Name::new("Type0")));
        assert_eq!(dict.get("Encoding").and_then(Object::as_name), Some(&Name::new("Identity-H")));

        // Two bytes per character, big-endian, and they are glyph ids in the
        // subset -- which is what /Identity-H plus /CIDToGIDMap /Identity means.
        let (codes, dropped) = embedded.encode("Ελ");
        assert_eq!(dropped, 0);
        assert_eq!(codes.len(), 4, "two bytes per character");

        let descendant = embedded
            .objects
            .iter()
            .filter_map(|(_, o)| o.as_dict())
            .find(|d| {
                d.get("Subtype").and_then(Object::as_name) == Some(&Name::new("CIDFontType2"))
            })
            .expect("the descendant font");
        assert_eq!(
            descendant.get("CIDToGIDMap").and_then(Object::as_name),
            Some(&Name::new("Identity"))
        );
        assert!(descendant.get("W").and_then(Object::as_array).is_some_and(|w| !w.is_empty()));
    }

    #[test]
    fn a_character_the_font_lacks_is_reported_not_substituted() {
        let Some(data) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        // Roboto has no CJK. Spec 2: degradation is reported, never assumed.
        let embedded = embed_truetype(&data, &Options::for_text("Hello 中文"), ids()).unwrap();
        assert_eq!(
            embedded.missing,
            vec!['文', '中'].into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>()
        );
        assert!(!embedded.covers('中'));

        // Only characters that were embedded: the subset holds what was asked
        // for and nothing else, so an 'i' that was never requested is as absent
        // as the ideograph — which is the point of subsetting, and worth not
        // conflating with the coverage failure under test.
        let (codes, dropped) = embedded.encode("Hell 中");
        assert_eq!(dropped, 1, "the character it cannot draw is counted, not drawn");
        assert_eq!(codes, b"Hell ", "and the rest is drawn as normal");
    }

    #[test]
    fn the_subset_carries_only_what_was_asked_for() {
        let Some(data) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let embedded = embed_truetype(&data, &Options::for_text("AB"), ids()).unwrap();
        let file = embedded
            .objects
            .iter()
            .find_map(|(_, o)| o.as_stream())
            .expect("the /FontFile2 stream");
        let program = file.pending_decoded().expect("the content that was set");

        // Roboto is about 170 KB and two letters are a fraction of it. The
        // point of subsetting is that a document embedding one word does not
        // carry 3,387 glyphs.
        assert!(program.len() < data.len() / 4, "{} vs {}", program.len(), data.len());

        // And the subset is still a font: it parses, and it kept a cmap, which
        // `compact_truetype` drops and which a simple font cannot work without.
        let sub = Sfnt::parse(program).expect("the subset is a valid sfnt");
        let cmap = Cmap::parse(program, &sub).expect("the subset kept a cmap");
        let table = cmap.best_unicode().expect("a Unicode subtable");
        assert!(table.lookup(program, 'A' as u32).is_some_and(|g| g != 0));
        assert!(
            table.lookup(program, 'Z' as u32).is_none_or(|g| g == 0),
            "a glyph nobody asked for should not be in the subset"
        );
    }
}
