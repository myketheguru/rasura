//! A complete PDF with an injected font, for external validation. Spec 17.
//!
//! Phase 4's exit criterion is that "injection round-trips validate in all four
//! viewers". Everything else in this crate checks its own output: the parsers
//! re-read what the writers produced, and `rustybuzz` loads the rebuilt font.
//! That catches a great deal and proves nothing to anyone else.
//!
//! This builds a real document — a page that draws text through a font whose
//! program has had a glyph injected, with `/Widths`, `/Differences` and
//! `/ToUnicode` updated to match — so that `qpdf`, veraPDF and a viewer have
//! something to disagree with. It is added to the seed corpus, which CI already
//! runs `qpdf --check` over.
//!
//! The font here is synthesised rather than vendored: shipping a real typeface
//! would mean shipping its licence, and the point is to exercise the injection
//! machinery, not to render beautifully.

use crate::embed::{Added, update_simple_font};
use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, Object};

/// A minimal but valid TrueType with `n` drawable glyphs.
///
/// Glyph 0 is `.notdef` and empty; the rest are simple triangles, which are the
/// smallest thing that produces visible marks and a non-trivial outline for the
/// injector to move.
pub fn truetype(n: u16, units_per_em: u16) -> Vec<u8> {
    let mut glyf = Vec::new();
    let mut offsets = vec![0u32];

    // .notdef: no outline.
    offsets.push(0);

    for i in 1..n {
        let scale = units_per_em as i16 / 2;
        let lift = (i as i16 % 3) * 40;
        let mut g = Vec::new();
        g.extend_from_slice(&1i16.to_be_bytes()); // one contour
        g.extend_from_slice(&0i16.to_be_bytes()); // xMin
        g.extend_from_slice(&0i16.to_be_bytes()); // yMin
        g.extend_from_slice(&scale.to_be_bytes()); // xMax
        g.extend_from_slice(&(scale + lift).to_be_bytes()); // yMax
        g.extend_from_slice(&2u16.to_be_bytes()); // endPtsOfContours: 3 points
        g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength

        // Flag 0x01 is ON_CURVE_POINT alone: the short-vector and repeat bits
        // are absent, so each coordinate is a signed 16-bit delta.
        g.extend_from_slice(&[0x01u8; 3]);

        // Coordinates are deltas from the previous point, not absolutes --
        // a triangle at (0,0), (scale,0), (0,scale+lift).
        let deltas: [(i16, i16); 3] = [(0, 0), (scale, 0), (-scale, scale + lift)];
        for (dx, _) in deltas {
            g.extend_from_slice(&dx.to_be_bytes());
        }
        for (_, dy) in deltas {
            g.extend_from_slice(&dy.to_be_bytes());
        }
        while g.len() % 2 != 0 {
            g.push(0);
        }
        glyf.extend_from_slice(&g);
        offsets.push(glyf.len() as u32);
    }

    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&units_per_em.to_be_bytes());
    head[44..46].copy_from_slice(&0u16.to_be_bytes());
    head[50..52].copy_from_slice(&0i16.to_be_bytes()); // short loca

    let mut maxp = vec![0u8; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&n.to_be_bytes());

    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[4..6].copy_from_slice(&(units_per_em as i16 * 4 / 5).to_be_bytes());
    hhea[6..8].copy_from_slice(&(-(units_per_em as i16) / 5).to_be_bytes());
    hhea[34..36].copy_from_slice(&n.to_be_bytes());

    let mut hmtx = Vec::new();
    for _ in 0..n {
        hmtx.extend_from_slice(&(units_per_em / 2).to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
    }

    // A (3,1) cmap mapping 'A'..'A'+n-2 to glyphs 1..n-1.
    let last = 0x41u16 + n - 2;
    let mut sub = Vec::new();
    sub.extend_from_slice(&4u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&4u16.to_be_bytes());
    sub.extend_from_slice(&[0; 6]);
    sub.extend_from_slice(&last.to_be_bytes());
    sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&0x41u16.to_be_bytes());
    sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
    sub.extend_from_slice(&1u16.wrapping_sub(0x41).to_be_bytes());
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    let len = sub.len() as u16;
    sub[2..4].copy_from_slice(&len.to_be_bytes());

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&3u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&sub);

    let loca: Vec<u8> = offsets.iter().flat_map(|o| ((*o / 2) as u16).to_be_bytes()).collect();

    // Tags must be in ascending order in the directory, which some validators
    // check and all of them assume.
    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];

    let count = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    out.extend_from_slice(&(count as u16).to_be_bytes());
    let entry_selector = (usize::BITS - 1 - count.leading_zeros()) as u16;
    let search_range = (1u16 << entry_selector) * 16;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((count as u16 * 16).wrapping_sub(search_range)).to_be_bytes());

    let mut offset = 12 + count * 16;
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&checksum(body).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len().next_multiple_of(4);
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn checksum(body: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for chunk in body.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

/// A PDF embedding `font`, drawing `text` on one page.
fn document(
    font: &[u8],
    text: &str,
    widths: &str,
    extra_font: &str,
    to_unicode: Option<&[u8]>,
) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 1 0 0 1 72 700 Tm ({text}) Tj ET\n");
    let mut b = ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", content.as_bytes())
        .object(
            5,
            &format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /RasuraTest \
                 /FontDescriptor 6 0 R {widths} {extra_font} >>"
            ),
        )
        .object(
            6,
            "<< /Type /FontDescriptor /FontName /RasuraTest /Flags 32 \
             /FontBBox [0 -200 1000 800] /ItalicAngle 0 /Ascent 800 /Descent -200 \
             /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>",
        )
        .stream(7, &format!("/Length1 {}", font.len()), font);
    if let Some(cmap) = to_unicode {
        b = b.stream(8, "", cmap);
    }
    b.finish("/Root 1 0 R")
}

/// A before/after pair built from a real typeface.
#[derive(Debug, Clone)]
pub struct RealFontPair {
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    /// Glyphs the injection pulled in beyond the one asked for -- the
    /// components of a composite.
    pub components_pulled: usize,
    /// Glyphs in the subset before and after.
    pub glyphs_before: u16,
    pub glyphs_after: u16,
}

/// Subset a real font the way a producer would, then inject a glyph it lacks.
///
/// The synthesised font elsewhere in this module exercises the injection
/// machinery, but it shares this library's assumptions about what a well-formed
/// sfnt looks like — it was built by the same code that reads it. A real
/// typeface does not: it has composite glyphs, a `post` table, hinting
/// programs, `GSUB`, a `cmap` with several subtables, and 2048 units per em
/// rather than 1000.
///
/// The sequence is the real one. A producer embeds only the glyphs its document
/// used; someone then types a character that was subset away; the missing glyph
/// has to come from the full font. That is spec 8.1's whole premise, and it is
/// worth running end to end rather than approximating.
pub fn inject_into_real_font(
    full: &[u8],
    present: &str,
    added: char,
) -> crate::Result<RealFontPair> {
    use crate::error::FontError;

    let font = crate::Sfnt::parse(full)?;
    let cmap = crate::Cmap::parse(full, &font).ok_or(FontError::MissingTable("cmap"))?;

    // The Unicode subtable directly, not `simple_glyph`. That function
    // implements §9.6.6.4's *code* resolution, which tries MacRoman first — and
    // MacRoman would answer for É at 0xC9 with whatever glyph it puts there.
    // Here the input is a character, so the Unicode table is the only correct
    // one.
    let unicode = cmap.best_unicode().ok_or(FontError::MissingTable("cmap"))?;
    let gid_of = |c: char| {
        unicode
            .lookup(full, c as u32)
            .filter(|g| *g != 0)
            .ok_or(FontError::Malformed("the font lacks a character"))
    };
    let present_gids: Vec<u16> = present.chars().map(gid_of).collect::<crate::Result<_>>()?;
    let added_gid = gid_of(added)?;

    // Widths are in glyph space, so the font's own units have to be scaled.
    // Roboto is 2048 units per em; assuming 1000 would set every advance at
    // half its width.
    let per_em = font.units_per_em.max(1) as f64;
    let width_of = |gid: u16| font.advance(full, gid).unwrap_or(0) as f64 * 1000.0 / per_em;

    // 1. The producer's subset: only the glyphs the document used.
    let subset = crate::compact_truetype(full, &present_gids)?;
    let subset_font = crate::Sfnt::parse(&subset.bytes)?;
    let mapped: Vec<(u32, u16)> = present
        .chars()
        .zip(&present_gids)
        .map(|(c, gid)| (c as u32, subset.mapping[gid]))
        .collect();
    let before_program = crate::add_mappings(&subset.bytes, &subset_font, &mapped)?;

    // 2. Someone types a character the subset does not have.
    let injection = crate::inject_truetype(&before_program, full, &[added_gid])?;
    let new_gid = injection.mapping[&added_gid];
    let injected_font = crate::Sfnt::parse(&injection.bytes)?;
    let after_program =
        crate::add_mappings(&injection.bytes, &injected_font, &[(added as u32, new_gid)])?;

    // `/Widths` is indexed by character code from `/FirstChar`, not by order of
    // appearance. "Hamburg" occupies codes 72, 97, 98, 103, 109, 114 and 117 —
    // seven letters spread across forty-six codes — so the array has to span
    // the range and carry a zero where the document uses nothing. Writing seven
    // widths from /FirstChar 72 gives 'a' the width of 'A'+25, and the letters
    // pile up on top of each other.
    let mut by_code: std::collections::BTreeMap<u32, i64> = Default::default();
    for (c, gid) in present.chars().zip(&present_gids) {
        let code = u32::from(winansi_code(c).ok_or(FontError::Malformed("no WinAnsi code"))?);
        by_code.insert(code, width_of(*gid) as i64);
    }
    let (first, last) = match (by_code.keys().next(), by_code.keys().next_back()) {
        (Some(f), Some(l)) => (*f, *l),
        _ => return Err(FontError::Malformed("no text")),
    };
    let widths: Vec<String> =
        (first..=last).map(|c| by_code.get(&c).copied().unwrap_or(0).to_string()).collect();
    let before_widths =
        format!("/FirstChar {first} /LastChar {last} /Widths [{}]", widths.join(" "));

    let before = document(
        &before_program,
        &escape(present),
        &before_widths,
        "/Encoding /WinAnsiEncoding",
        None,
    );

    // 3. The PDF-level updates for the new code.
    let doc = Document::open(before.clone()).map_err(|_| FontError::Malformed("fixture"))?;
    let font_dict = doc
        .get(rasura_cos::ObjId::new(5, 0))
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .ok_or(FontError::Malformed("font dict"))?;
    let descriptor = doc
        .get(rasura_cos::ObjId::new(6, 0))
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .ok_or(FontError::Malformed("descriptor"))?;

    // WinAnsi puts the character at a code; that code is what the page draws.
    let code = winansi_code(added).ok_or(FontError::Malformed("no WinAnsi code"))?;
    let update = update_simple_font(
        &doc,
        &font_dict,
        &descriptor,
        &[Added {
            code,
            // The AGL name, because §9.6.6.4 resolves a TrueType /Differences
            // name through the Adobe Glyph List before the cmap sees it.
            name: agl_name(added).to_string(),
            width: width_of(added_gid),
            unicode: added.to_string(),
            bbox: None,
        }],
    )?;

    let after_text: String = present.chars().chain(std::iter::once(added)).collect();
    let after = document(
        &after_program,
        &escape(&after_text),
        &render_widths(&update.font),
        &format!(
            "/Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding \
             /Differences [{code} /{}] >> /ToUnicode 8 0 R",
            agl_name(added)
        ),
        Some(&update.to_unicode),
    );

    Ok(RealFontPair {
        before,
        after,
        components_pulled: injection.components_pulled,
        glyphs_before: subset.glyphs_after,
        glyphs_after: injected_font.num_glyphs,
    })
}

/// WinAnsi puts most of Latin-1 where Latin-1 does.
fn winansi_code(c: char) -> Option<u8> {
    u8::try_from(c as u32).ok().filter(|b| *b >= 32)
}

/// The Adobe Glyph List name for the handful of characters this fixture uses.
fn agl_name(c: char) -> &'static str {
    match c {
        'É' => "Eacute",
        'é' => "eacute",
        'Ç' => "Ccedilla",
        'ø' => "oslash",
        _ => "question",
    }
}

/// PDF literal strings need their delimiters escaped, and bytes above 127
/// written as octal so the file stays seven-bit.
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 128 => out.push(c),
            c => out.push_str(&format!("\\{:03o}", c as u32 & 0xFF)),
        }
    }
    out
}

/// The same document *before* injection, drawing only the glyphs it had.
///
/// Exists so §14.3's pixel diff has a baseline. Comparing the injected file
/// against nothing can only answer "does it open"; comparing it against this
/// answers the question §2 actually cares about — whether the text that was
/// already there moved.
pub fn uninjected_truetype_pdf() -> Vec<u8> {
    document(
        &truetype(3, 1000),
        "AB",
        "/FirstChar 65 /LastChar 66 /Widths [500 500]",
        "/Encoding /WinAnsiEncoding",
        None,
    )
}

/// A PDF whose font has had a glyph injected, with every dictionary updated.
///
/// This is the whole of §8.4 exercised end to end and written out as a file an
/// external validator can judge.
pub fn injected_truetype_pdf() -> Vec<u8> {
    // A three-glyph font in the document, and a five-glyph one to take from.
    let embedded = truetype(3, 1000);
    let source = truetype(6, 1000);

    let injection = crate::inject_truetype(&embedded, &source, &[4])
        .expect("the fixture fonts are built to inject cleanly");
    let new_gid = injection.mapping[&4];

    // A TrueType simple font reaches a glyph through its own `cmap`: the
    // /Differences name becomes a character through the Adobe Glyph List, and
    // the character becomes a glyph id here. Without this the glyph is in the
    // file, /Widths and /ToUnicode describe it, every structural check passes,
    // and it draws nothing. Only a renderer notices.
    let program = {
        let sfnt = crate::Sfnt::parse(&injection.bytes).expect("the rebuilt font parses");
        crate::add_mappings(&injection.bytes, &sfnt, &[(u32::from('C'), new_gid)])
            .expect("the cmap rebuild")
    };

    // Before: the document uses codes 65 and 66.
    let base = document(
        &embedded,
        "AB",
        "/FirstChar 65 /LastChar 66 /Widths [500 500]",
        "/Encoding /WinAnsiEncoding",
        None,
    );
    let doc = Document::open(base).expect("the fixture opens");
    let font = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
    let descriptor = doc.get(rasura_cos::ObjId::new(6, 0)).unwrap().as_dict().unwrap().clone();

    // After: code 67 reaches the injected glyph.
    let update = update_simple_font(
        &doc,
        &font,
        &descriptor,
        &[Added {
            code: 67,
            name: "C".into(),
            width: 500.0,
            unicode: "C".into(),
            bbox: Some([0, 0, 500, 700]),
        }],
    )
    .expect("the PDF-level update");

    // The name is `C` rather than something invented: §9.6.6.4 resolves a
    // TrueType /Differences name through the Adobe Glyph List, so a name the
    // AGL does not know cannot reach any glyph however the cmap is built.
    document(
        &program,
        "ABC",
        &render_widths(&update.font),
        "/Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding \
         /Differences [67 /C] >> /ToUnicode 8 0 R",
        Some(&update.to_unicode),
    )
}

/// `/FirstChar`, `/LastChar` and `/Widths` from an updated font dictionary.
fn render_widths(font: &rasura_cos::Dictionary) -> String {
    let n = |key: &str| font.get(key).and_then(Object::as_i64).unwrap_or(0);
    let widths: Vec<String> = font
        .get("Widths")
        .and_then(Object::as_array)
        .map(|a| a.iter().map(|o| format!("{}", o.as_f64().unwrap_or(0.0) as i64)).collect())
        .unwrap_or_default();
    format!(
        "/FirstChar {} /LastChar {} /Widths [{}]",
        n("FirstChar"),
        n("LastChar"),
        widths.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthesised_font_is_a_font() {
        let bytes = truetype(4, 1000);
        let f = crate::Sfnt::parse(&bytes).expect("parses");
        assert_eq!(f.num_glyphs, 4);
        assert_eq!(f.units_per_em, 1000);
        assert!(f.glyph_data(&bytes, 1).is_some_and(|g| !g.is_empty()));

        // And a real shaper will load it, which is a stronger claim than
        // "my own parser agrees with my own writer".
        let request = crate::shape::request_for("A", false, crate::KerningSource::None, true, None);
        let shaped = crate::shape(&bytes, &request).expect("rustybuzz loads it");
        assert_eq!(shaped[0].gid, 1);
    }

    #[test]
    fn the_injected_pdf_opens_and_carries_the_new_glyph() {
        let bytes = injected_truetype_pdf();
        let doc = Document::open(bytes).expect("the document opens");

        let font = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        assert_eq!(font.get("LastChar").and_then(Object::as_i64), Some(67));

        // The font program grew a glyph.
        let program = doc.decoded_stream(rasura_cos::ObjId::new(7, 0)).expect("the program");
        let f = crate::Sfnt::parse(&program).expect("the embedded font parses");
        assert_eq!(f.num_glyphs, 4, "three original plus the injected one");

        // And /ToUnicode says what the new code means, which spec 8.4 requires
        // always.
        let cmap = doc.decoded_stream(rasura_cos::ObjId::new(8, 0)).expect("/ToUnicode");
        let text = String::from_utf8_lossy(&cmap);
        assert!(text.contains("<43> <0043>"), "code 67 maps to U+0043: {text}");
    }

    /// A typeface this library did not write, if it has been fetched.
    ///
    /// Downloaded rather than committed (`corpus/fetch-font.sh`), so the test
    /// declines rather than fails when it is absent — the same arrangement the
    /// corpus tests use. CI fetches it, so the skip is a local convenience and
    /// not a hole in the gate.
    fn real_font() -> Option<Vec<u8>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fonts/Roboto-Regular.ttf");
        let bytes = std::fs::read(path).ok()?;
        if bytes.is_empty() { None } else { Some(bytes) }
    }

    #[test]
    fn injection_survives_a_font_this_library_did_not_write() {
        let Some(full) = real_font() else {
            eprintln!("skipped: run corpus/fetch-font.sh for the real-font check");
            return;
        };

        let pair = inject_into_real_font(&full, "Hamburg", 'É').expect("injection");

        // É is a composite in every Latin typeface worth the name, so the
        // components must have come with it. If this ever reads 0 the test
        // below still passes while checking something much weaker, which is
        // the failure worth catching.
        assert!(pair.components_pulled > 0, "É should be composite and pull its parts");
        assert!(pair.glyphs_after > pair.glyphs_before);

        let after = Document::open(pair.after.clone()).expect("the after document opens");
        let program = after.decoded_stream(rasura_cos::ObjId::new(7, 0)).expect("the program");
        let sfnt = crate::Sfnt::parse(&program).expect("the rebuilt font parses");

        // The producer's subset is small; the injected font is that plus É and
        // its parts. Anything near 3387 would mean the whole face got embedded.
        assert!(sfnt.num_glyphs < 32, "the subset stayed a subset: {}", sfnt.num_glyphs);

        // The glyph has to be *reachable* the way a viewer reaches it: §9.6.6.4
        // turns /Differences /Eacute into U+00C9 through the AGL, then asks the
        // font's own cmap. That path is what drew nothing before `cmap_write`
        // existed, and no structural check notices when it breaks.
        let cmap = crate::Cmap::parse(&program, &sfnt).expect("cmap");
        let gid = cmap
            .best_unicode()
            .and_then(|t| t.lookup(&program, u32::from('É')))
            .filter(|g| *g != 0)
            .expect("É resolves through the rebuilt cmap");
        assert!(
            sfnt.glyph_data(&program, gid).is_some_and(|g| !g.is_empty()),
            "the injected glyph has an outline"
        );

        // And it was genuinely absent beforehand -- otherwise the whole
        // exercise is injecting something already there.
        let before = Document::open(pair.before).expect("the before document opens");
        let old = before.decoded_stream(rasura_cos::ObjId::new(7, 0)).expect("the program");
        let old_sfnt = crate::Sfnt::parse(&old).expect("parses");
        let absent = crate::Cmap::parse(&old, &old_sfnt)
            .and_then(|c| c.best_unicode().and_then(|t| t.lookup(&old, u32::from('É'))))
            .filter(|g| *g != 0);
        assert_eq!(absent, None, "the subset did not already contain É");
    }

    #[test]
    fn real_font_widths_are_scaled_out_of_the_fonts_own_units() {
        let Some(full) = real_font() else { return };

        // Roboto is 2048 units per em. A width written straight out of `hmtx`
        // into /Widths would be about twice the glyph's actual advance, and
        // every viewer would draw the text with growing gaps -- visible, but
        // only if someone looks. This is the arithmetic that prevents it.
        let font = crate::Sfnt::parse(&full).expect("parses");
        assert_ne!(font.units_per_em, 1000, "the point of this test is a font that is not 1000");

        let pair = inject_into_real_font(&full, "Hamburg", 'É').expect("injection");
        let after = Document::open(pair.after).expect("opens");
        let dict = after.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();
        let widths: Vec<f64> = dict
            .get("Widths")
            .and_then(Object::as_array)
            .expect("/Widths")
            .iter()
            .filter_map(Object::as_f64)
            .collect();

        // Latin text at 24pt: no advance in a normal face reaches an em, and
        // none of the letters here is blank.
        for (i, w) in widths.iter().enumerate() {
            assert!(*w <= 1000.0, "width {i} is {w}, which exceeds an em");
        }
        assert!(widths.iter().any(|w| *w > 400.0), "some width is a plausible letter: {widths:?}");
    }

    #[test]
    fn the_injected_pdf_extracts_the_text_it_draws() {
        // The end-to-end claim: a reader gets "ABC" back, including the
        // character that did not exist in the font before.
        let bytes = injected_truetype_pdf();
        let doc = Document::open(bytes).expect("opens");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let (runs, _) = {
            let (raw, _, _) = rasura_content::extract_page(&doc, &pages.pages[0]);
            (raw, ())
        };
        let codes: Vec<u32> = runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.code)).collect();
        assert_eq!(codes, vec![65, 66, 67]);
    }
}
