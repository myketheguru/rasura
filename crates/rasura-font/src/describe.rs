//! Font-file metrics that a PDF `/FontDescriptor` requires. ISO 32000-1 §9.8.
//!
//! [`crate::sfnt`] deliberately decodes only the five scalars glyph injection
//! has to agree with, because a model that parsed every table into structs
//! would re-serialise every table on every save, and spec 2 forbids rewriting
//! what an edit did not touch. That is right for editing a font a document
//! already carries.
//!
//! Embedding a font the document has *never seen* is the opposite problem.
//! Nothing in the file can be inherited, so §9.8.1's whole table has to be
//! produced from the font program: the bounding box from `head`, the angle and
//! pitch from `post`, the vertical metrics from `hhea` or `OS/2`, the flags
//! from several places at once. None of that was read anywhere in this
//! workspace before — every `/FontDescriptor` in the tree was either read out
//! of an existing PDF or hand-written in a test fixture.
//!
//! # What is guessed, and said so
//!
//! One value in §9.8.1 cannot be computed: **`/StemV`**, the vertical stem
//! width. No sfnt table records it. Type 1 fonts carried it, TrueType never
//! did, and every producer estimates. [`Description::stem_v`] is an estimate
//! from `OS/2.usWeightClass` and says as much; [`Description::stem_v_guessed`]
//! is true whenever it was not measured, which is always for a TrueType font.
//! Reporting it rather than presenting it as fact is the same rule the rest of
//! the library follows for fidelity.

use crate::error::{FontError, Result};
use crate::sfnt::Sfnt;

/// `/Flags`, ISO 32000-1 Table 121.
///
/// Only the bits derivable from a font program are set. `/Nonsymbolic` and
/// `/Symbolic` are mutually exclusive by the standard's own wording, and a
/// misjudged pair here is what makes a viewer ignore an `/Encoding` and read
/// the font's built-in `cmap` instead — the characteristic "right file, wrong
/// glyphs" failure.
pub mod flags {
    pub const FIXED_PITCH: u32 = 1 << 0;
    pub const SERIF: u32 = 1 << 1;
    pub const SYMBOLIC: u32 = 1 << 2;
    pub const SCRIPT: u32 = 1 << 3;
    pub const NONSYMBOLIC: u32 = 1 << 5;
    pub const ITALIC: u32 = 1 << 6;
    pub const ALL_CAP: u32 = 1 << 16;
    pub const SMALL_CAP: u32 = 1 << 17;
    pub const FORCE_BOLD: u32 = 1 << 18;
}

/// Everything a `/FontDescriptor` needs, read from the font program.
///
/// Lengths are in **glyph space**: thousandths of an em, which is what PDF
/// wants and what `units_per_em` is not.
#[derive(Debug, Clone, PartialEq)]
pub struct Description {
    /// The PostScript name from the `name` table, or `None` when the font has
    /// no usable one and the caller must supply it.
    pub postscript_name: Option<String>,
    /// `[xMin, yMin, xMax, yMax]` from `head`, scaled.
    pub bbox: [f64; 4],
    pub italic_angle: f64,
    pub ascent: f64,
    pub descent: f64,
    /// `OS/2.sCapHeight` where the table is new enough to have it, else an
    /// estimate from the ascent. Required for a nonsymbolic font.
    pub cap_height: f64,
    /// `OS/2.sxHeight`, when present. Optional in the standard.
    pub x_height: Option<f64>,
    /// Estimated. See the module note; never measured for TrueType.
    pub stem_v: f64,
    pub stem_v_guessed: bool,
    pub flags: u32,
    /// `hmtx`'s advance for `.notdef`, which is what a missing code gets.
    pub missing_width: f64,
    /// `units_per_em`, kept so a caller can scale its own measurements the
    /// same way this did.
    pub units_per_em: f64,
    /// True when `head` gave a degenerate bounding box and one was derived
    /// from the vertical metrics instead. A zero box makes some viewers clip
    /// every glyph away, so it is corrected rather than passed through — and
    /// reported, because a corrected value is not a measured one.
    pub bbox_estimated: bool,
}

/// Scale a font-unit measurement into PDF glyph space.
///
/// This division was written out by hand in four places in this workspace, all
/// identical, none of them a function. It is one here.
pub fn to_glyph_space(value: f64, units_per_em: u16) -> f64 {
    value * 1000.0 / f64::from(units_per_em.max(1))
}

fn be_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn be_i16(data: &[u8], at: usize) -> Option<i16> {
    be_u16(data, at).map(|v| v as i16)
}

/// Read the descriptor values out of a font program.
///
/// Missing tables are tolerated the way [`Sfnt::parse`] tolerates them: a font
/// without `OS/2` or `post` is unusual but readable, and refusing it would
/// decline fonts that render perfectly well. Everything absent falls back to a
/// value derived from what *is* present, never to a constant that pretends to
/// be a measurement.
pub fn describe(data: &[u8], font: &Sfnt) -> Result<Description> {
    let upem = font.units_per_em.max(1);
    let scale = |v: f64| to_glyph_space(v, upem);

    // --- head: the bounding box ---------------------------------------------
    let head = font.table_data(data, b"head");
    let raw_bbox = head.and_then(|h| {
        Some([
            f64::from(be_i16(h, 36)?),
            f64::from(be_i16(h, 38)?),
            f64::from(be_i16(h, 40)?),
            f64::from(be_i16(h, 42)?),
        ])
    });

    // --- hhea and OS/2: the vertical metrics --------------------------------
    let hhea = font.table_data(data, b"hhea");
    let hhea_ascent = hhea.and_then(|h| be_i16(h, 4)).map(f64::from);
    let hhea_descent = hhea.and_then(|h| be_i16(h, 6)).map(f64::from);

    let os2 = font.table_data(data, b"OS/2");
    let os2_version = os2.and_then(|o| be_u16(o, 0)).unwrap_or(0);
    // sTypoAscender/Descender sit at 68/70 in every version. sCapHeight and
    // sxHeight arrived in version 2, so reading them from an older table would
    // be reading whatever follows it.
    let typo_ascent = os2.and_then(|o| be_i16(o, 68)).map(f64::from).filter(|v| *v != 0.0);
    let typo_descent = os2.and_then(|o| be_i16(o, 70)).map(f64::from).filter(|v| *v != 0.0);
    let cap_height =
        (os2_version >= 2).then(|| os2.and_then(|o| be_i16(o, 88)).map(f64::from)).flatten();
    let x_height =
        (os2_version >= 2).then(|| os2.and_then(|o| be_i16(o, 86)).map(f64::from)).flatten();
    let weight_class = os2.and_then(|o| be_u16(o, 4)).filter(|w| *w > 0);
    let fs_selection = os2.and_then(|o| be_u16(o, 62));
    let family_class = os2.and_then(|o| be_i16(o, 30)).map(|v| (v >> 8) as u8);

    // hhea first: it is the metric a rasteriser actually lays lines out with,
    // and OS/2's typo values are a typographer's preference that many fonts
    // leave at zero.
    let ascent = hhea_ascent.filter(|v| *v != 0.0).or(typo_ascent).unwrap_or(f64::from(upem) * 0.8);
    let descent =
        hhea_descent.filter(|v| *v != 0.0).or(typo_descent).unwrap_or(f64::from(upem) * -0.2);

    // --- post: angle and pitch ----------------------------------------------
    let post = font.table_data(data, b"post");
    // A 16.16 fixed-point signed value.
    let italic_angle = post
        .and_then(|p| {
            let raw = i32::from_be_bytes([*p.first()?, *p.get(1)?, *p.get(2)?, *p.get(3)?]);
            // Byte 0 is the version's integer part; the angle is at 4.
            let angle = i32::from_be_bytes([*p.get(4)?, *p.get(5)?, *p.get(6)?, *p.get(7)?]);
            let _ = raw;
            Some(f64::from(angle) / 65536.0)
        })
        .unwrap_or(0.0);
    let fixed_pitch = post
        .and_then(|p| {
            Some(u32::from_be_bytes([*p.get(12)?, *p.get(13)?, *p.get(14)?, *p.get(15)?]) != 0)
        })
        .unwrap_or(false);

    // --- flags ---------------------------------------------------------------
    let mut flag_bits = 0u32;
    if fixed_pitch {
        flag_bits |= flags::FIXED_PITCH;
    }
    // OS/2 sFamilyClass: 1..=7 are the serif classes, 8 is sans, 10 is script.
    match family_class {
        Some(1..=7) => flag_bits |= flags::SERIF,
        Some(10) => flag_bits |= flags::SCRIPT,
        _ => {}
    }
    if italic_angle != 0.0 || fs_selection.is_some_and(|s| s & 0x0001 != 0) {
        flag_bits |= flags::ITALIC;
    }
    if fs_selection.is_some_and(|s| s & 0x0020 != 0) && weight_class.is_some_and(|w| w >= 700) {
        flag_bits |= flags::FORCE_BOLD;
    }
    // Nonsymbolic is the caller's to override: it depends on the *encoding*
    // chosen for this embedding, not on the font. A font embedded with
    // WinAnsiEncoding is nonsymbolic by §9.8.1's definition whatever its cmap
    // says, and a symbolic font with a (3,0) cmap is not. The default is the
    // common case and `Description::as_symbolic` flips it.
    flag_bits |= flags::NONSYMBOLIC;

    // --- widths --------------------------------------------------------------
    let missing_width = font.advance(data, 0).map(f64::from).unwrap_or(0.0);

    // A zero or inverted box is degenerate. Some viewers clip every glyph to
    // /FontBBox, so passing one through makes an otherwise perfect font render
    // as nothing at all -- the exact failure mode this whole path is most
    // likely to produce and least likely to notice.
    let (bbox, bbox_estimated) = match raw_bbox {
        Some(b) if b[2] > b[0] && b[3] > b[1] => {
            ([scale(b[0]), scale(b[1]), scale(b[2]), scale(b[3])], false)
        }
        _ => ([0.0, scale(descent), scale(f64::from(upem)), scale(ascent)], true),
    };

    // --- StemV: the one value nobody can measure ----------------------------
    //
    // usWeightClass runs 100..900 and 400 is regular. The linear fit below puts
    // regular near 80 -- the value every fixture in this repository hardcodes,
    // and the one Acrobat writes for a regular face -- and bold near 140.
    let (stem_v, stem_v_guessed) = match weight_class {
        Some(w) => ((f64::from(w) * 13.0 / 65.0).clamp(20.0, 220.0), true),
        None => (80.0, true),
    };

    Ok(Description {
        postscript_name: postscript_name(data, font),
        bbox,
        italic_angle,
        ascent: scale(ascent),
        descent: scale(descent),
        // Falling back to 70% of the ascent: an estimate, and a much better one
        // than zero, which would be a lie a viewer might act on.
        cap_height: scale(cap_height.filter(|v| *v != 0.0).unwrap_or(ascent * 0.7)),
        x_height: x_height.filter(|v| *v != 0.0).map(scale),
        stem_v,
        stem_v_guessed,
        flags: flag_bits,
        missing_width: scale(missing_width),
        units_per_em: f64::from(upem),
        bbox_estimated,
    })
}

impl Description {
    /// Mark this font as symbolic rather than nonsymbolic.
    ///
    /// The two flags are exclusive and the choice belongs to the embedding, not
    /// to the font: a font embedded with an `/Encoding` is nonsymbolic whatever
    /// its `cmap` contains, and one embedded to be addressed through its own
    /// built-in encoding is symbolic. Getting this wrong is what makes a viewer
    /// quietly ignore `/Differences`.
    pub fn as_symbolic(mut self) -> Description {
        self.flags &= !flags::NONSYMBOLIC;
        self.flags |= flags::SYMBOLIC;
        self
    }

    pub fn is_symbolic(&self) -> bool {
        self.flags & flags::SYMBOLIC != 0
    }
}

/// The PostScript name from the `name` table: name id 6.
///
/// Windows-platform UTF-16BE first, then Macintosh Roman, which between them
/// covers every font this is likely to meet. A name with characters outside
/// the range §9.8.1 permits — anything but printable ASCII, and no space,
/// bracket, brace, slash, parenthesis or percent — is rejected rather than
/// sanitised, because a `/BaseFont` that does not match the font program's own
/// name is a mismatch a validator will report.
fn postscript_name(data: &[u8], font: &Sfnt) -> Option<String> {
    let table = font.table_data(data, b"name")?;
    let count = be_u16(table, 2)?;
    let storage = be_u16(table, 4)? as usize;

    let mut best: Option<String> = None;
    for i in 0..count as usize {
        let rec = 6 + i * 12;
        let platform = be_u16(table, rec)?;
        let name_id = be_u16(table, rec + 6)?;
        if name_id != 6 {
            continue;
        }
        let length = be_u16(table, rec + 8)? as usize;
        let offset = be_u16(table, rec + 10)? as usize;
        let bytes = table.get(storage + offset..storage + offset + length)?;

        let decoded = match platform {
            // Windows: UTF-16BE.
            3 => bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .map(|u| char::from_u32(u32::from(u)))
                .collect::<Option<String>>(),
            // Macintosh Roman is ASCII over the range a PostScript name may use.
            1 => Some(bytes.iter().map(|&b| b as char).collect()),
            _ => None,
        };
        let Some(name) = decoded else { continue };
        if !name.is_empty() && name.bytes().all(is_name_byte) {
            // Windows wins when both are present, so keep looking only until
            // one from platform 3 turns up.
            if platform == 3 {
                return Some(name);
            }
            best.get_or_insert(name);
        }
    }
    best
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_graphic()
        && !matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// A six-letter subset tag, as ISO 32000-1 §9.6.4 requires: `ABCDEF+Name`.
///
/// Derived from the glyph set rather than drawn at random, for the reason the
/// rest of this library never uses an RNG — the same subset of the same font
/// must produce the same bytes, or two saves of one document differ and
/// invariant I8 fails. Two genuinely different subsets colliding costs nothing
/// worse than a viewer thinking two fonts are one, which is why a cheap hash is
/// enough.
pub fn subset_tag(font_name: &str, glyphs: &[u16]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in font_name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for gid in glyphs {
        for byte in gid.to_be_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    (0..6)
        .map(|i| {
            let shifted = (hash >> (i * 5)) & 0x1f;
            (b'A' + (shifted % 26) as u8) as char
        })
        .collect()
}

/// Refuse a font this path cannot embed, before anything is written.
pub fn check_embeddable(font: &Sfnt) -> Result<()> {
    if font.is_cff() {
        // A CFF-flavoured OpenType font goes in a /FontFile3 with subtype
        // /OpenType, and the subsetter here rebuilds `glyf`, which such a font
        // does not have. Declining by name beats writing a /FontFile2 whose
        // contents are not what the key claims.
        return Err(FontError::Unsupported(
            "this font has a CFF outline table; only TrueType outlines can be embedded",
        ));
    }
    if !font.has(b"glyf") || !font.has(b"loca") {
        return Err(FontError::Unsupported("the font has no glyf/loca outlines to embed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    #[test]
    fn the_synthesised_fixture_describes_without_its_optional_tables() {
        // The fixture has no post, no OS/2 and no name, which is the case that
        // must not panic or produce zeroes a viewer would act on.
        let data = fixture::truetype(6, 1000);
        let font = Sfnt::parse(&data).unwrap();
        let d = describe(&data, &font).unwrap();

        assert_eq!(d.postscript_name, None, "the fixture has no name table");
        assert_eq!(d.italic_angle, 0.0);
        assert!(d.stem_v_guessed, "TrueType never records StemV");

        // hhea carries 4/5 and -1/5 of the em in the fixture.
        assert_eq!(d.ascent, 800.0);
        assert_eq!(d.descent, -200.0);
        assert_eq!(d.cap_height, 560.0, "70% of the ascent, since OS/2 is absent");

        // The fixture's head has a zero bbox, which is exactly the degenerate
        // case: a viewer that clips to /FontBBox would draw nothing.
        assert!(d.bbox_estimated);
        assert_eq!(d.bbox, [0.0, -200.0, 1000.0, 800.0]);
        assert_eq!(d.flags & flags::NONSYMBOLIC, flags::NONSYMBOLIC);
    }

    #[test]
    fn units_are_scaled_into_glyph_space() {
        // 2048 units per em is what most real fonts use, and forgetting to
        // scale is the mistake that makes every width 2.048 times too wide.
        let data = fixture::truetype(4, 2048);
        let font = Sfnt::parse(&data).unwrap();
        let d = describe(&data, &font).unwrap();
        assert_eq!(d.units_per_em, 2048.0);
        // 4/5 of the em, whatever the em is — but font units are integers, so
        // 4/5 of 2048 is stored as 1638 and comes back as 799.8, not 800. The
        // tolerance is the quantisation, not slack in the arithmetic.
        assert!((d.ascent - 800.0).abs() < 0.5, "{}", d.ascent);
        assert_eq!(to_glyph_space(1024.0, 2048), 500.0);
    }

    #[test]
    fn a_subset_tag_is_six_capitals_and_depends_on_the_glyphs() {
        let a = subset_tag("Roboto", &[1, 2, 3]);
        let b = subset_tag("Roboto", &[1, 2, 4]);
        let c = subset_tag("Inter", &[1, 2, 3]);

        for tag in [&a, &b, &c] {
            assert_eq!(tag.len(), 6, "{tag}");
            assert!(tag.bytes().all(|b| b.is_ascii_uppercase()), "{tag}");
        }
        assert_ne!(a, b, "a different glyph set is a different subset");
        assert_ne!(a, c, "a different font is a different subset");

        // Determinism is the point: I8 fails if two saves of one document
        // disagree, and a random tag would make that happen every time.
        assert_eq!(a, subset_tag("Roboto", &[1, 2, 3]));
    }

    /// Roboto, when `corpus/fetch-font.sh` has been run.
    ///
    /// The synthesised fixture has no `post`, no `OS/2` and no `name`, so it
    /// exercises none of the parsing that is most likely to be wrong. This is
    /// the only test here that reads a typeface someone else drew — 2048 units
    /// per em, a real bounding box, a real weight class.
    fn roboto() -> Option<Vec<u8>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/fonts/Roboto-Regular.ttf");
        std::fs::read(path).ok()
    }

    #[test]
    fn a_real_typeface_describes_from_its_own_tables() {
        let Some(data) = roboto() else {
            eprintln!("skipped: run ./corpus/fetch-font.sh");
            return;
        };
        let font = Sfnt::parse(&data).unwrap();
        let d = describe(&data, &font).unwrap();

        assert_eq!(d.postscript_name.as_deref(), Some("Roboto-Regular"));
        assert_eq!(d.units_per_em, 2048.0);

        // Measured, not estimated: the whole point of reading `head`.
        assert!(!d.bbox_estimated, "Roboto has a real /FontBBox");
        assert!(d.bbox[0] < 0.0 && d.bbox[2] > 500.0, "{:?}", d.bbox);
        assert!(d.bbox[1] < 0.0 && d.bbox[3] > 500.0, "{:?}", d.bbox);

        // Upright, sans, and with a cap height read from OS/2 rather than
        // guessed off the ascent.
        assert_eq!(d.italic_angle, 0.0);
        assert_eq!(d.flags & flags::SERIF, 0, "Roboto is a sans");
        assert_eq!(d.flags & flags::ITALIC, 0);
        assert_eq!(d.flags & flags::FIXED_PITCH, 0);
        // Glyph space, so ~711 rather than the 1456 font units OS/2 records.
        assert!((700.0..=725.0).contains(&d.cap_height), "cap height {}", d.cap_height);
        assert!(d.x_height.is_some_and(|x| (515.0..=540.0).contains(&x)), "{:?}", d.x_height);
        // And it was read rather than guessed: the fallback is 70% of the
        // ascent, which for Roboto would be about 649.
        assert!((d.cap_height - d.ascent * 0.7).abs() > 20.0, "cap height looks like the fallback");

        // Regular weight lands near the 80 that every fixture in this
        // repository hardcodes -- and is still flagged as a guess, because no
        // sfnt table records it.
        assert!((70.0..=95.0).contains(&d.stem_v), "stem_v {}", d.stem_v);
        assert!(d.stem_v_guessed);
    }

    #[test]
    fn a_cff_font_is_declined_rather_than_mislabelled() {
        // Writing CFF bytes into a /FontFile2 produces a file that passes every
        // structural check and renders nothing.
        let mut data = fixture::truetype(4, 1000);
        // Rename `glyf` to `CFF ` so the directory claims CFF outlines.
        let at = data.windows(4).position(|w| w == b"glyf").expect("the fixture has glyf");
        data[at..at + 4].copy_from_slice(b"CFF ");
        let font = Sfnt::parse(&data).unwrap();
        assert!(check_embeddable(&font).is_err());
    }
}
