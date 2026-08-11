//! Glyph injection into a TrueType font. Spec 8.4.
//!
//! An embedded font is almost always subset: type a character the document
//! never used and there is no glyph to draw. Spec 8.1 calls resolving that
//! "the product". This is the half that resolves it *exactly* — take the
//! outline from a registered source font and put it in the target — as opposed
//! to §8.5's substitution, which resolves it approximately and says so.
//!
//! Three properties shape the implementation:
//!
//! - **Original glyph ids never move.** Spec 8.6's sparse-preserving default:
//!   new glyphs go on the end and nothing is renumbered, because renumbering
//!   would require rewriting every content stream that references the font —
//!   precisely the non-local change §2 forbids.
//! - **Untouched tables are copied byte for byte.** Only `glyf`, `loca`,
//!   `hmtx`, `hhea` and `maxp` change. Hinting programs, `cmap`, `name`,
//!   `OS/2` and everything else survive unchanged, which is why the sfnt is
//!   modelled as a table directory over byte ranges rather than as parsed
//!   structures.
//! - **A composite glyph pulls its components, transitively**, and their glyph
//!   indices are *renumbered* as they are copied. Those indices live inside the
//!   glyph data; injecting `Á` without rewriting them yields a glyph that
//!   references whatever happens to sit at those ids in the target font.

use crate::error::{FontError, Result};
use crate::sfnt::{Sfnt, loca_needs_long_format};
use std::collections::{BTreeSet, HashMap};

/// How deep a composite glyph may nest before the font is assumed hostile.
/// Real fonts rarely exceed two.
const MAX_COMPONENT_DEPTH: usize = 8;

/// The outcome of an injection.
#[derive(Debug, Clone)]
pub struct Injection {
    /// The rebuilt font.
    pub bytes: Vec<u8>,
    /// Source glyph id to its new id in the target, for every glyph added.
    pub mapping: HashMap<u16, u16>,
    /// Whether `loca` had to be widened to the long format. Spec 8.4 names
    /// getting this wrong as "a common silent corruption".
    pub loca_widened: bool,
    /// Components pulled in transitively beyond the glyphs asked for.
    pub components_pulled: usize,
    /// The target's own `loca` disagreed with its `glyf` or `maxp` -- offsets
    /// past the end of the data, or fewer entries than the glyph count.
    ///
    /// Reported rather than repaired: the offsets are clamped so every glyph's
    /// readable bytes stay exactly as they were, and a caller that wants the
    /// font mended can ask for that separately.
    pub target_loca_inconsistent: bool,
}

/// Inject glyphs from `source` into `target`. Spec 8.4's TrueType path.
///
/// Returns the glyphs' new ids in the rebuilt font. Ids already present are not
/// deduplicated against the target's contents: this layer cannot know whether
/// the target's glyph 40 is the same shape as the source's, and assuming so
/// would substitute a glyph while claiming to have injected one.
pub fn inject_truetype(target: &[u8], source: &[u8], glyphs: &[u16]) -> Result<Injection> {
    let target_font = Sfnt::parse(target)?;
    let source_font = Sfnt::parse(source)?;

    if !target_font.has(b"glyf") || target_font.loca.is_empty() {
        return Err(FontError::MissingTable("glyf/loca"));
    }
    if !source_font.has(b"glyf") || source_font.loca.is_empty() {
        return Err(FontError::MissingTable("source glyf/loca"));
    }

    // A glyph is expressed in its own font's units. Injecting a 2048-unit
    // outline into a 1000-unit font draws it at twice the intended size, and
    // correcting that means re-encoding every coordinate in the outline --
    // which is a different operation from copying one. Reported rather than
    // done silently or done wrong.
    if target_font.units_per_em != source_font.units_per_em {
        return Err(FontError::Malformed("unitsPerEm differs; the outline would need rescaling"));
    }

    // Transitive closure over composite components, in ascending source order
    // so the result is deterministic.
    let mut wanted: BTreeSet<u16> = BTreeSet::new();
    for &gid in glyphs {
        collect(&source_font, source, gid, 0, &mut wanted)?;
    }
    if wanted.is_empty() {
        return Err(FontError::Malformed("no glyphs to inject"));
    }
    let components_pulled = wanted.len().saturating_sub(glyphs.len());

    // New ids are assigned by appending, so every original id keeps its value.
    let first_new = target_font.num_glyphs;
    let mapping: HashMap<u16, u16> =
        wanted.iter().enumerate().map(|(i, &src)| (src, first_new + i as u16)).collect();
    let new_count = u16::try_from(first_new as usize + wanted.len())
        .map_err(|_| FontError::Malformed("more than 65535 glyphs"))?;

    // --- glyf -----------------------------------------------------------
    let (glyf_at, glyf_len) = target_font.table(b"glyf").expect("checked above");
    let mut glyf = target.get(glyf_at..glyf_at + glyf_len).unwrap_or_default().to_vec();

    // The target's own loca may be shorter than its glyf -- padding, or a
    // truncated table -- so offsets are rebuilt from the existing ones and
    // continue from the end of the data actually present.
    let mut offsets: Vec<u32> = target_font.loca.clone();
    offsets.truncate(first_new as usize + 1);
    while offsets.len() < first_new as usize + 1 {
        offsets.push(glyf.len() as u32);
    }

    // A `loca` that claims more than `glyf` holds is not rare, and the honest
    // response is to clamp the offsets rather than grow `glyf` to match. Growing
    // it would *repair* the font -- turning glyphs a reader currently sees as
    // truncated into zero-filled ones -- and repairing something the edit did
    // not ask about is the non-local change §2 forbids. Clamping leaves every
    // glyph's readable bytes exactly as they were.
    // Non-monotonic offsets are the third shape of this: a `loca` where one
    // glyph's end is before its start, or where two glyphs overlap. Those are
    // not reproducible by any rebuild -- the table does not describe a layout
    // -- so they are reported rather than silently normalised into one.
    let target_loca_inconsistent = offsets.iter().any(|o| *o as usize > glyf.len())
        || target_font.loca.len() < first_new as usize + 1
        || offsets.windows(2).any(|w| w[1] < w[0]);
    for o in &mut offsets {
        *o = (*o).min(glyf.len() as u32);
    }

    // Append exactly where the last original glyph ends. `loca[first_new]` is
    // *both* that end and the start of the first new glyph -- it is one slot --
    // so writing a padded position into it would extend the last original
    // glyph's data range and change a glyph the edit never touched.
    //
    // Any bytes past that point are padding outside every glyph's range and are
    // dropped rather than carried.
    let append_at = *offsets.last().expect("non-empty") as usize;
    glyf.truncate(append_at.min(glyf.len()));

    // Whether the rebuilt `loca` will be short, which is the only reason to pad.
    // Decided before appending, from the worst case: every glyph at full size.
    let appended: usize = wanted
        .iter()
        .map(|g| source_font.glyph_data(source, *g).map(<[u8]>::len).unwrap_or(0) + 1)
        .sum();
    let will_be_long =
        target_font.index_to_loc_format != 0 || loca_needs_long_format(glyf.len() + appended);

    for &src in &wanted {
        let data = source_font.glyph_data(source, src).unwrap_or_default();
        let mut data = data.to_vec();
        renumber_components(&mut data, &mapping)?;
        glyf.extend_from_slice(&data);
        // Padded to an even length *only* for the short `loca` format, which
        // stores offsets halved and so cannot address an odd one. Padding a
        // long-format font grows every odd-length glyph by a byte for nothing,
        // and that byte is inside the glyph's declared range -- so the outline
        // that comes back out is not the one that went in.
        if !will_be_long {
            while glyf.len() % 2 != 0 {
                glyf.push(0);
            }
        }
        offsets.push(glyf.len() as u32);
    }

    // --- loca -----------------------------------------------------------
    let loca_widened = loca_needs_long_format(glyf.len()) && target_font.index_to_loc_format == 0;
    let long = target_font.index_to_loc_format != 0 || loca_widened;
    let loca = encode_loca(&offsets, long);

    // --- hmtx and hhea --------------------------------------------------
    // Rebuilt with a full metric for every glyph rather than preserving the
    // format's compressed tail. `hmtx` stores full metrics for the first
    // `numberOfHMetrics` glyphs and bare left side bearings after; appending
    // full entries behind a compressed tail would be invalid, and expanding is
    // the reading that is correct whatever the target was doing.
    let mut hmtx = Vec::with_capacity(new_count as usize * 4);
    for gid in 0..first_new {
        let advance = target_font.advance(target, gid).unwrap_or(0);
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&left_side_bearing(&target_font, target, gid).to_be_bytes());
    }
    for &src in &wanted {
        let advance = source_font.advance(source, src).unwrap_or(0);
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&left_side_bearing(&source_font, source, src).to_be_bytes());
    }

    let mut hhea = target_font.table_data(target, b"hhea").unwrap_or_default().to_vec();
    if hhea.len() >= 36 {
        hhea[34..36].copy_from_slice(&new_count.to_be_bytes());
    }

    let mut maxp = target_font.table_data(target, b"maxp").unwrap_or_default().to_vec();
    if maxp.len() >= 6 {
        maxp[4..6].copy_from_slice(&new_count.to_be_bytes());
    }

    let mut head = target_font.table_data(target, b"head").unwrap_or_default().to_vec();
    if head.len() >= 52 {
        head[50..52].copy_from_slice(&(if long { 1i16 } else { 0i16 }).to_be_bytes());
    }

    let mut replacements: Vec<([u8; 4], Vec<u8>)> =
        vec![(*b"glyf", glyf), (*b"loca", loca), (*b"hmtx", hmtx)];
    if !hhea.is_empty() {
        replacements.push((*b"hhea", hhea));
    }
    if !maxp.is_empty() {
        replacements.push((*b"maxp", maxp));
    }
    if !head.is_empty() {
        replacements.push((*b"head", head));
    }

    Ok(Injection {
        bytes: rebuild(target, &target_font, &replacements),
        mapping,
        loca_widened,
        components_pulled,
        target_loca_inconsistent,
    })
}

/// Left side bearing for a glyph, from `hmtx`.
pub(crate) fn left_side_bearing(font: &Sfnt, data: &[u8], gid: u16) -> i16 {
    let Some(hmtx) = font.table_data(data, b"hmtx") else { return 0 };
    let n = font.number_of_h_metrics as usize;
    let at = if (gid as usize) < n {
        gid as usize * 4 + 2
    } else {
        // Past the full-metric run, `hmtx` stores bare side bearings.
        n * 4 + (gid as usize - n) * 2
    };
    hmtx.get(at..at + 2).map(|b| i16::from_be_bytes([b[0], b[1]])).unwrap_or(0)
}

/// Every glyph a glyph needs, itself included.
fn collect(
    font: &Sfnt,
    data: &[u8],
    gid: u16,
    depth: usize,
    out: &mut BTreeSet<u16>,
) -> Result<()> {
    if depth > MAX_COMPONENT_DEPTH {
        return Err(FontError::Malformed("composite glyph nests too deeply"));
    }
    if gid as usize >= font.num_glyphs as usize {
        return Err(FontError::Malformed("glyph id past the end of the font"));
    }
    if !out.insert(gid) {
        return Ok(());
    }
    let Some(glyph) = font.glyph_data(data, gid) else { return Ok(()) };
    for (_, component) in components(glyph) {
        collect(font, data, component, depth + 1, out)?;
    }
    Ok(())
}

/// The component glyph ids of a composite glyph, with the byte offset of each
/// index so it can be rewritten in place.
///
/// A simple glyph -- `numberOfContours >= 0` -- has none.
pub fn components(glyph: &[u8]) -> Vec<(usize, u16)> {
    let mut out = Vec::new();
    if glyph.len() < 10 {
        return out;
    }
    let contours = i16::from_be_bytes([glyph[0], glyph[1]]);
    if contours >= 0 {
        return out;
    }

    const ARGS_ARE_WORDS: u16 = 0x0001;
    const WE_HAVE_A_SCALE: u16 = 0x0008;
    const MORE_COMPONENTS: u16 = 0x0020;
    const X_AND_Y_SCALE: u16 = 0x0040;
    const TWO_BY_TWO: u16 = 0x0080;

    let mut at = 10;
    loop {
        let Some(f) = glyph.get(at..at + 4) else { return out };
        let flags = u16::from_be_bytes([f[0], f[1]]);
        let index = u16::from_be_bytes([f[2], f[3]]);
        out.push((at + 2, index));

        at += 4;
        at += if flags & ARGS_ARE_WORDS != 0 { 4 } else { 2 };
        if flags & WE_HAVE_A_SCALE != 0 {
            at += 2;
        } else if flags & X_AND_Y_SCALE != 0 {
            at += 4;
        } else if flags & TWO_BY_TWO != 0 {
            at += 8;
        }
        if flags & MORE_COMPONENTS == 0 {
            return out;
        }
        // A malformed font can loop for ever otherwise.
        if out.len() > 64 {
            return out;
        }
    }
}

/// Rewrite a composite glyph's component ids to their new values.
pub(crate) fn renumber_components(glyph: &mut [u8], mapping: &HashMap<u16, u16>) -> Result<()> {
    for (offset, old) in components(glyph) {
        let new = mapping
            .get(&old)
            .ok_or(FontError::Malformed("composite component was not collected"))?;
        glyph
            .get_mut(offset..offset + 2)
            .ok_or(FontError::Truncated("composite glyph"))?
            .copy_from_slice(&new.to_be_bytes());
    }
    Ok(())
}

pub(crate) fn encode_loca(offsets: &[u32], long: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(offsets.len() * if long { 4 } else { 2 });
    for &o in offsets {
        if long {
            out.extend_from_slice(&o.to_be_bytes());
        } else {
            out.extend_from_slice(&((o / 2) as u16).to_be_bytes());
        }
    }
    out
}

/// Rebuild an sfnt, replacing some tables and copying the rest verbatim.
///
/// Tables keep their directory order. Every table not named in `replacements`
/// is copied byte for byte, which is the property that makes an injection a
/// local change: hinting programs, `cmap`, `name` and `OS/2` come through
/// untouched.
pub fn rebuild(data: &[u8], font: &Sfnt, replacements: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(font.tables.len());
    for (tag, offset, len) in &font.tables {
        match replacements.iter().find(|(t, _)| t == tag) {
            Some((_, body)) => tables.push((*tag, body.clone())),
            None => {
                tables.push((*tag, data.get(*offset..offset + len).unwrap_or_default().to_vec()))
            }
        }
    }
    // A replacement for a table the target lacks -- `loca` in a CFF font, say
    // -- is appended rather than dropped.
    for (tag, body) in replacements {
        if !tables.iter().any(|(t, _)| t == tag) {
            tables.push((*tag, body.clone()));
        }
    }

    let n = tables.len();
    let mut out = Vec::new();
    // The *sub-font's* version tag, which for a TrueType Collection is not the
    // `ttcf` at the front of the file. The output is one font, so stamping
    // `ttcf` on it produces bytes that announce a collection and then supply a
    // plain table directory -- a font no reader can open, this crate's own
    // parser included.
    let version = data.get(font.directory..font.directory + 4).unwrap_or(&[0x00, 0x01, 0x00, 0x00]);
    out.extend_from_slice(version);
    out.extend_from_slice(&(n as u16).to_be_bytes());
    // searchRange, entrySelector and rangeShift are a binary-search hint. They
    // are recomputed rather than copied, because the table count may have
    // changed and a stale hint sends a reader to the wrong record.
    let entry_selector = (usize::BITS - 1 - n.max(1).leading_zeros()) as u16;
    let search_range = (1u16 << entry_selector) * 16;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((n as u16 * 16).wrapping_sub(search_range)).to_be_bytes());

    let mut offset = 12 + n * 16;
    for (tag, body) in &tables {
        out.extend_from_slice(tag);
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

/// The sfnt table checksum: the sum of the table's 32-bit words.
fn checksum(body: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for chunk in body.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an sfnt from tables, in the order given.
    fn build_font(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let n = tables.len();
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + n * 16;
        for (tag, body) in tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            offset += body.len().next_multiple_of(4);
        }
        for (_, body) in tables {
            out.extend_from_slice(body);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    fn head(units_per_em: u16, long_loca: bool) -> Vec<u8> {
        let mut h = vec![0u8; 54];
        h[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        h[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h[18..20].copy_from_slice(&units_per_em.to_be_bytes());
        h[50..52].copy_from_slice(&(if long_loca { 1i16 } else { 0i16 }).to_be_bytes());
        h
    }

    fn maxp(n: u16) -> Vec<u8> {
        let mut m = vec![0u8; 32];
        m[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        m[4..6].copy_from_slice(&n.to_be_bytes());
        m
    }

    fn hhea(n: u16) -> Vec<u8> {
        let mut h = vec![0u8; 36];
        h[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        h[34..36].copy_from_slice(&n.to_be_bytes());
        h
    }

    /// A simple glyph with `n` bytes of (meaningless) outline data.
    fn simple_glyph(n: usize) -> Vec<u8> {
        let mut g = vec![0u8; 10];
        g[0..2].copy_from_slice(&1i16.to_be_bytes()); // one contour
        g.extend(std::iter::repeat_n(0xAB, n));
        g
    }

    /// A composite glyph referencing `parts`.
    fn composite_glyph(parts: &[u16]) -> Vec<u8> {
        let mut g = vec![0u8; 10];
        g[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        for (i, &p) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            // ARGS_ARE_WORDS, plus MORE_COMPONENTS unless it is the last.
            let flags: u16 = 0x0001 | if last { 0 } else { 0x0020 };
            g.extend_from_slice(&flags.to_be_bytes());
            g.extend_from_slice(&p.to_be_bytes());
            g.extend_from_slice(&0i16.to_be_bytes()); // arg1
            g.extend_from_slice(&0i16.to_be_bytes()); // arg2
        }
        g
    }

    /// A font whose glyphs are the given data blocks.
    fn font_with(glyphs: &[Vec<u8>], units_per_em: u16, extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut glyf = Vec::new();
        let mut offsets = vec![0u32];
        for g in glyphs {
            glyf.extend_from_slice(g);
            while glyf.len() % 4 != 0 {
                glyf.push(0);
            }
            offsets.push(glyf.len() as u32);
        }
        let loca = encode_loca(&offsets, false);
        let mut hmtx = Vec::new();
        for (i, _) in glyphs.iter().enumerate() {
            hmtx.extend_from_slice(&(500u16 + i as u16).to_be_bytes());
            hmtx.extend_from_slice(&(10i16 + i as i16).to_be_bytes());
        }

        let n = glyphs.len() as u16;
        let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"head", head(units_per_em, false)),
            (b"hhea", hhea(n)),
            (b"maxp", maxp(n)),
            (b"loca", loca),
            (b"glyf", glyf),
            (b"hmtx", hmtx),
        ];
        tables.extend(extra.iter().map(|(t, b)| (*t, b.clone())));
        build_font(&tables)
    }

    #[test]
    fn a_glyph_is_appended_and_original_ids_do_not_move() {
        // Spec 8.6's sparse-preserving rule: renumbering would mean rewriting
        // every content stream that references the font.
        let target = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), simple_glyph(8), simple_glyph(4)], 1000, &[]);

        let out = inject_truetype(&target, &source, &[1]).expect("inject");
        assert_eq!(out.mapping.get(&1), Some(&2), "appended after the target's two glyphs");

        let rebuilt = Sfnt::parse(&out.bytes).expect("rebuilt font parses");
        assert_eq!(rebuilt.num_glyphs, 3);

        // The originals are unchanged.
        let before = Sfnt::parse(&target).unwrap();
        for gid in 0..2 {
            assert_eq!(
                rebuilt.glyph_data(&out.bytes, gid),
                before.glyph_data(&target, gid),
                "glyph {gid} moved"
            );
        }
    }

    #[test]
    fn the_injected_outline_is_the_sources() {
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), simple_glyph(12)], 1000, &[]);
        let out = inject_truetype(&target, &source, &[1]).unwrap();

        let rebuilt = Sfnt::parse(&out.bytes).unwrap();
        let src = Sfnt::parse(&source).unwrap();
        let new_gid = out.mapping[&1];
        assert_eq!(
            rebuilt.glyph_data(&out.bytes, new_gid).unwrap(),
            src.glyph_data(&source, 1).unwrap()
        );
    }

    #[test]
    fn untouched_tables_are_copied_byte_for_byte() {
        // The property that makes an injection a local change. Hinting
        // programs especially: spec 8.4 says never to strip the existing ones.
        let extra: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", b"CMAPDATA".to_vec()),
            (b"fpgm", b"HINTINGPROGRAM".to_vec()),
            (b"prep", b"PREP".to_vec()),
            (b"name", b"NAMETABLE".to_vec()),
        ];
        let target = font_with(&[simple_glyph(4)], 1000, &extra);
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);

        let out = inject_truetype(&target, &source, &[1]).unwrap();
        let rebuilt = Sfnt::parse(&out.bytes).unwrap();
        let before = Sfnt::parse(&target).unwrap();

        for tag in [b"cmap", b"fpgm", b"prep", b"name"] {
            assert_eq!(
                rebuilt.table_data(&out.bytes, tag),
                before.table_data(&target, tag),
                "{} changed",
                String::from_utf8_lossy(tag)
            );
        }
    }

    #[test]
    fn metrics_follow_the_injected_glyph() {
        let target = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), simple_glyph(4), simple_glyph(4)], 1000, &[]);

        let out = inject_truetype(&target, &source, &[2]).unwrap();
        let rebuilt = Sfnt::parse(&out.bytes).unwrap();
        let src = Sfnt::parse(&source).unwrap();

        assert_eq!(rebuilt.num_glyphs, 3);
        assert_eq!(rebuilt.number_of_h_metrics, 3, "hhea was bumped");
        assert_eq!(
            rebuilt.advance(&out.bytes, out.mapping[&2]),
            src.advance(&source, 2),
            "the source's advance came with it"
        );
        // And the target's own advances are unchanged.
        let before = Sfnt::parse(&target).unwrap();
        for gid in 0..2 {
            assert_eq!(rebuilt.advance(&out.bytes, gid), before.advance(&target, gid));
        }
    }

    // --- composite glyphs ---------------------------------------------------

    #[test]
    fn a_composite_glyph_pulls_its_components() {
        // Spec 8.4 step 5. Injecting `Aacute` without `A` and `acute` gives a
        // glyph that draws nothing, or worse, draws whatever is at those ids.
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(
            &[simple_glyph(4), simple_glyph(4), simple_glyph(4), composite_glyph(&[1, 2])],
            1000,
            &[],
        );

        let out = inject_truetype(&target, &source, &[3]).unwrap();
        assert_eq!(out.components_pulled, 2, "A and acute came too");
        assert!(out.mapping.contains_key(&1) && out.mapping.contains_key(&2));
        assert_eq!(Sfnt::parse(&out.bytes).unwrap().num_glyphs, 4);
    }

    #[test]
    fn component_indices_are_renumbered_to_their_new_ids() {
        // The subtlety worth the whole test: those indices live *inside* the
        // glyph data. Copied unchanged, the composite would reference glyphs 1
        // and 2 of the target, which are different shapes entirely.
        let target = font_with(&[simple_glyph(4), simple_glyph(4), simple_glyph(4)], 1000, &[]);
        let source = font_with(
            &[simple_glyph(4), simple_glyph(4), simple_glyph(4), composite_glyph(&[1, 2])],
            1000,
            &[],
        );

        let out = inject_truetype(&target, &source, &[3]).unwrap();
        let rebuilt = Sfnt::parse(&out.bytes).unwrap();
        let injected = rebuilt.glyph_data(&out.bytes, out.mapping[&3]).unwrap();

        let refs: Vec<u16> = components(injected).into_iter().map(|(_, g)| g).collect();
        assert_eq!(refs, vec![out.mapping[&1], out.mapping[&2]]);
        assert!(refs.iter().all(|g| *g >= 3), "not the target's own ids: {refs:?}");
    }

    #[test]
    fn nested_composites_are_pulled_transitively() {
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(
            &[
                simple_glyph(4),
                simple_glyph(4),
                composite_glyph(&[1]),    // gid 2 -> 1
                composite_glyph(&[2, 1]), // gid 3 -> 2, 1
            ],
            1000,
            &[],
        );
        let out = inject_truetype(&target, &source, &[3]).unwrap();
        assert_eq!(out.mapping.len(), 3, "3, 2 and 1");
        assert!(out.mapping.contains_key(&1));
    }

    #[test]
    fn a_self_referential_composite_terminates() {
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), composite_glyph(&[1])], 1000, &[]);
        // Glyph 1 references itself; the visited set stops it.
        let out = inject_truetype(&target, &source, &[1]).expect("terminates");
        assert_eq!(out.mapping.len(), 1);
    }

    #[test]
    fn the_component_walker_reads_every_flag_form() {
        // Arg sizes and the transform flags change the record length; getting
        // one wrong desynchronises the walk and yields nonsense ids.
        let mut g = vec![0u8; 10];
        g[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        // Component 1: byte args, a 2x2 transform, more to come.
        g.extend_from_slice(&(0x0020u16 | 0x0080).to_be_bytes());
        g.extend_from_slice(&7u16.to_be_bytes());
        g.extend_from_slice(&[0, 0]); // byte args
        g.extend_from_slice(&[0; 8]); // 2x2
        // Component 2: word args, single scale, last.
        g.extend_from_slice(&(0x0001u16 | 0x0008).to_be_bytes());
        g.extend_from_slice(&9u16.to_be_bytes());
        g.extend_from_slice(&[0; 4]); // word args
        g.extend_from_slice(&[0; 2]); // scale

        let found: Vec<u16> = components(&g).into_iter().map(|(_, gid)| gid).collect();
        assert_eq!(found, vec![7, 9]);
    }

    #[test]
    fn a_simple_glyph_has_no_components() {
        assert!(components(&simple_glyph(8)).is_empty());
        assert!(components(&[]).is_empty());
    }

    // --- loca -----------------------------------------------------------------

    #[test]
    fn loca_widens_when_glyf_crosses_the_short_limit() {
        // Spec 8.4 names this "a common silent corruption": short loca halves
        // offsets into 16 bits, so past 128 KB every later glyph points at the
        // wrong bytes.
        // The target must itself still be *valid* short-format loca -- its glyf
        // under 128 KB -- and only cross the line once the new glyph lands.
        // Sized past the limit on both counts, the fixture would be testing a
        // font that was already broken.
        let big = simple_glyph(131_000);
        let target = font_with(&[simple_glyph(4), big], 1000, &[]);
        assert!(
            !loca_needs_long_format(
                Sfnt::parse(&target).unwrap().loca.last().copied().unwrap() as usize
            ),
            "the target starts out valid in the short format"
        );
        let source = font_with(&[simple_glyph(4), simple_glyph(400)], 1000, &[]);

        let out = inject_truetype(&target, &source, &[1]).unwrap();
        assert!(out.loca_widened, "the table crossed the limit");

        let rebuilt = Sfnt::parse(&out.bytes).unwrap();
        assert_eq!(rebuilt.index_to_loc_format, 1, "head records the new format");
        // And the glyph really is reachable at its new id.
        assert!(rebuilt.glyph_data(&out.bytes, out.mapping[&1]).is_some());
    }

    #[test]
    fn trailing_padding_does_not_extend_the_last_original_glyph() {
        // Found on the corpus: `loca[first_new]` is *one slot* serving as both
        // the end of the last original glyph and the start of the first new
        // one. Writing a padded append position into it extended the last
        // original glyph's data range -- silently changing a glyph the edit
        // never touched, on 270 of 681 real fonts.
        let target = {
            // A glyf with padding past the last glyph's declared end.
            let mut glyf = Vec::new();
            let mut offsets = vec![0u32];
            for g in [simple_glyph(4), simple_glyph(6)] {
                glyf.extend_from_slice(&g);
                offsets.push(glyf.len() as u32);
            }
            glyf.extend_from_slice(&[0u8; 8]); // padding outside every range
            build_font(&[
                (b"head", head(1000, false)),
                (b"hhea", hhea(2)),
                (b"maxp", maxp(2)),
                (b"loca", encode_loca(&offsets, false)),
                (b"glyf", glyf),
                (b"hmtx", vec![0u8; 8]),
            ])
        };
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);

        let before = Sfnt::parse(&target).unwrap();
        let out = inject_truetype(&target, &source, &[1]).expect("inject");
        let after = Sfnt::parse(&out.bytes).unwrap();

        for gid in 0..before.num_glyphs {
            assert_eq!(
                after.glyph_data(&out.bytes, gid),
                before.glyph_data(&target, gid),
                "glyph {gid}'s data range changed"
            );
        }
    }

    #[test]
    fn a_font_already_using_long_loca_stays_long() {
        let mut target = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        // Rewrite it as long-format loca.
        {
            let f = Sfnt::parse(&target).unwrap();
            let loca = encode_loca(&f.loca, true);
            let mut head_bytes = f.table_data(&target, b"head").unwrap().to_vec();
            head_bytes[50..52].copy_from_slice(&1i16.to_be_bytes());
            target = rebuild(&target, &f, &[(*b"loca", loca), (*b"head", head_bytes)]);
        }
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        let out = inject_truetype(&target, &source, &[1]).unwrap();
        assert!(!out.loca_widened, "it was already long");
        assert_eq!(Sfnt::parse(&out.bytes).unwrap().index_to_loc_format, 1);
    }

    // --- refusals -------------------------------------------------------------

    #[test]
    fn mismatched_units_per_em_is_refused_rather_than_drawn_wrong() {
        // A 2048-unit outline in a 1000-unit font draws at twice the size.
        // Correcting it means re-encoding every coordinate, which is a
        // different operation from copying one.
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 2048, &[]);
        assert!(inject_truetype(&target, &source, &[1]).is_err());
    }

    #[test]
    fn a_glyph_id_past_the_source_is_refused() {
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        assert!(inject_truetype(&target, &source, &[9]).is_err());
    }

    #[test]
    fn a_cff_target_is_refused() {
        // No glyf to append to. Spec 8.4's CFF path is a different procedure.
        let target = build_font(&[(b"head", head(1000, false)), (b"CFF ", vec![1, 0, 4, 1])]);
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);
        assert!(inject_truetype(&target, &source, &[1]).is_err());
    }

    #[test]
    fn injecting_nothing_is_refused() {
        let target = font_with(&[simple_glyph(4)], 1000, &[]);
        let source = font_with(&[simple_glyph(4)], 1000, &[]);
        assert!(inject_truetype(&target, &source, &[]).is_err());
    }

    // --- the rebuilt font -----------------------------------------------------

    #[test]
    fn the_rebuilt_font_is_shapeable() {
        // The end-to-end check: rustybuzz must load what came out. A wrong
        // table offset, length or checksum shows up here and nowhere else.
        let extra: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"cmap", {
            let mut sub = Vec::new();
            sub.extend_from_slice(&4u16.to_be_bytes());
            sub.extend_from_slice(&0u16.to_be_bytes());
            sub.extend_from_slice(&0u16.to_be_bytes());
            sub.extend_from_slice(&4u16.to_be_bytes());
            sub.extend_from_slice(&[0; 6]);
            sub.extend_from_slice(&0x41u16.to_be_bytes());
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
            cmap
        })];
        let target = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &extra);
        let source = font_with(&[simple_glyph(4), simple_glyph(4)], 1000, &[]);

        let out = inject_truetype(&target, &source, &[1]).unwrap();
        let request = crate::shape::request_for("A", false, crate::KerningSource::None, true, None);
        let shaped = crate::shape(&out.bytes, &request).expect("the rebuilt font loads");
        assert_eq!(shaped.len(), 1);
        assert_eq!(shaped[0].gid, 1, "the original mapping still works");
    }

    #[test]
    fn rebuilding_without_replacements_preserves_every_table() {
        let extra: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"cmap", b"ABCD".to_vec())];
        let original = font_with(&[simple_glyph(4)], 1000, &extra);
        let font = Sfnt::parse(&original).unwrap();
        let same = rebuild(&original, &font, &[]);
        let after = Sfnt::parse(&same).unwrap();

        assert_eq!(after.tables.len(), font.tables.len());
        for (tag, _, _) in &font.tables {
            assert_eq!(
                after.table_data(&same, tag),
                font.table_data(&original, tag),
                "{} changed",
                String::from_utf8_lossy(tag)
            );
        }
    }
}
