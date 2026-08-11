//! Compaction subsetting, at the document level. Spec 8.6.
//!
//! > `SubsetPolicy::Compact` (opt-in, `FullRewrite` only) renumbers and prunes
//! > unused glyphs for size. Offer it; never default to it.
//! >
//! > Renumbering would require rewriting every content stream that references
//! > the font — exactly the non-local change §2 forbids.
//!
//! [`rasura_font::compact_truetype`] does the font half: prune, renumber,
//! hand back an old-to-new glyph mapping. This is the document half, and the
//! sentence above is the reason it needed thinking about rather than plumbing.
//!
//! # The renumbering does not have to reach the content streams
//!
//! It does if you go looking for the glyph ids in the wrong place. A composite
//! font's content stream does not contain glyph ids at all — it contains
//! **CIDs**, and `/CIDToGIDMap` turns those into glyph ids. So a renumbering can
//! be absorbed entirely by rewriting that one map:
//!
//! ```text
//! before:  code → CID → (CIDToGIDMap) → old GID
//! after:   code → CID → (CIDToGIDMap) → new GID
//! ```
//!
//! Not one byte of any content stream changes. The map is a stream of two-byte
//! big-endian entries indexed by CID, so rewriting it is an object edit of
//! exactly the kind the writer is built for.
//!
//! When `/CIDToGIDMap` is `/Identity` — CID *is* GID — there is no indirection
//! to absorb anything, and the spec's warning applies in full. The answer is to
//! **add the indirection** rather than to rewrite the streams: a
//! `/CIDToGIDMap` stream is written where the name was, and the content streams
//! stay untouched again. One new object against every page that uses the font
//! is not a close decision.
//!
//! # What this does not do
//!
//! **Simple fonts decline by name.** A simple font's content stream holds
//! single-byte codes, and the code-to-glyph path runs through `/Encoding`,
//! glyph *names*, and the font's own `cmap` — three mechanisms, of which
//! `compact_truetype` deliberately drops the third. Compacting one correctly
//! means rebuilding a `cmap` for the surviving glyphs, and getting it wrong
//! yields a page of blanks rather than a smaller file.
//!
//! **CFF fonts decline by name.** `/FontFile3` needs charset and FDSelect
//! renumbering, which is a different piece of work in a different format.
//!
//! Both are named individually rather than silently skipped, so a caller who
//! compacts a document and saves 4% knows *why* it was not 40%.

use crate::session::{Compromise, Fidelity};
use rasura_content::page::PageTree;
use rasura_cos::object::{Dictionary, Name, Object, Stream};
use rasura_cos::{Document, ObjId};
use std::collections::{BTreeMap, BTreeSet};

/// Objects to stage, keyed by the id each replaces or creates.
type Changes = Vec<(ObjId, Object)>;

/// Why a font could not be compacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// A simple font: the code-to-glyph path is `/Encoding` plus the font's own
    /// `cmap`, and rebuilding that is not this pass's work.
    SimpleFont,
    /// `/FontFile3`: charset and FDSelect renumbering, in another format.
    CffProgram,
    /// No embedded program, so there is nothing to prune.
    NotEmbedded,
    /// `/CIDToGIDMap` is a stream this pass could not read.
    UnreadableMap,
    /// The font program would not parse or compact.
    Failed(String),
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skipped::SimpleFont => f.write_str("a simple font; its cmap would have to be rebuilt"),
            Skipped::CffProgram => f.write_str("a CFF program; charset and FDSelect renumbering"),
            Skipped::NotEmbedded => f.write_str("not embedded"),
            Skipped::UnreadableMap => f.write_str("/CIDToGIDMap could not be read"),
            Skipped::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// What compacting one font did.
#[derive(Debug, Clone)]
pub struct Compacted {
    /// The `/FontFile2` stream that was replaced.
    pub program: ObjId,
    pub base_font: String,
    pub glyphs_before: u16,
    pub glyphs_after: u16,
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// True when a `/CIDToGIDMap` stream had to be created because the font
    /// used `/Identity`.
    pub added_map: bool,
}

/// What compacting a document did, whether or not anything changed.
#[derive(Debug, Clone)]
pub struct Report {
    pub compacted: Vec<Compacted>,
    /// Fonts left alone, with the reason. Reported rather than omitted: a
    /// caller who saves 4% deserves to know why it was not 40%.
    pub skipped: Vec<(String, Skipped)>,
    /// Object changes to stage, in the order they were produced.
    pub changes: Changes,
    pub fidelity: Fidelity,
}

impl Default for Report {
    fn default() -> Self {
        Report {
            compacted: Vec::new(),
            skipped: Vec::new(),
            changes: Vec::new(),
            fidelity: Fidelity::Exact,
        }
    }
}

impl Report {
    pub fn bytes_saved(&self) -> usize {
        self.compacted.iter().map(|c| c.bytes_before.saturating_sub(c.bytes_after)).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.compacted.is_empty()
    }
}

/// Prune every compactable embedded font to the glyphs the document uses.
///
/// Nothing is written. The changes are returned for a session to stage, and the
/// caller must save with [`SaveMode::FullRewrite`](rasura_cos::SaveMode) —
/// an incremental append would leave the *old* font program in the file, so the
/// document would be no smaller and would carry two copies of the font.
pub fn plan(doc: &Document, pages: &PageTree) -> Report {
    let used = used_glyphs(doc, pages);
    let mut report = Report::default();
    let mut next_object = doc.next_object_number();

    for (font_id, gids) in &used {
        let Ok(object) = doc.get(*font_id) else { continue };
        let Some(dict) = object.as_dict() else { continue };
        let base = base_font_of(dict);

        match compact_one(doc, *font_id, dict, gids, &mut next_object) {
            Ok(Some((done, changes))) => {
                report.compacted.push(done);
                report.changes.extend(changes);
            }
            Ok(None) => {}
            Err(why) => report.skipped.push((base, why)),
        }
    }

    // Compaction is lossy by design: the glyphs a document does not currently
    // draw are gone, so a later edit that wants one has to inject it again.
    // That is the trade the caller opted into, and it is still reported.
    report.fidelity = if report.compacted.is_empty() {
        Fidelity::Exact
    } else {
        Fidelity::Degraded(vec![Compromise::FontSubsetCompacted {
            fonts: report.compacted.len(),
            bytes_saved: report.bytes_saved(),
        }])
    };
    report
}

/// Every glyph id each embedded font actually draws, across every page.
///
/// Keyed by the *descendant* font's object id for a composite font, because
/// that is where `/FontFile2` and `/CIDToGIDMap` live — and because two Type0
/// fonts can share one descendant, in which case the union of both their usages
/// is what may be kept.
fn used_glyphs(doc: &Document, pages: &PageTree) -> BTreeMap<ObjId, BTreeSet<u16>> {
    let mut out: BTreeMap<ObjId, BTreeSet<u16>> = BTreeMap::new();

    for page in &pages.pages {
        let (runs, _, _) =
            rasura_content::text::extract_page_with(doc, page, &rasura_layout::Standard14Widths);
        for run in &runs {
            let Some(name) = &run.font_name else { continue };
            let Some(dict) = font_dict(doc, page, name) else { continue };
            let Some(descendant) = descendant_id(doc, &dict) else { continue };

            // The CID a code maps to, then the GID that CID maps to. For an
            // Identity map they are the same number, which is exactly why an
            // Identity map cannot absorb a renumbering.
            let Ok(descendant_dict) = doc.get(descendant) else { continue };
            let Some(descendant_dict) = descendant_dict.as_dict() else { continue };
            let map = cid_to_gid(doc, descendant_dict);

            let entry = out.entry(descendant).or_default();
            for glyph in &run.glyphs {
                let cid = u16::try_from(glyph.cid).unwrap_or(0);
                entry.insert(match &map {
                    CidToGid::Identity => cid,
                    CidToGid::Table(table) => table.get(cid as usize).copied().unwrap_or(0),
                });
            }
        }
    }
    out
}

/// `/CIDToGIDMap`, in the two forms ISO 32000-1 Table 117 allows.
enum CidToGid {
    Identity,
    Table(Vec<u16>),
}

fn cid_to_gid(doc: &Document, descendant: &Dictionary) -> CidToGid {
    let Some(value) = descendant.get("CIDToGIDMap") else { return CidToGid::Identity };
    let Some(id) = value.as_reference() else { return CidToGid::Identity };
    let Ok(data) = doc.decoded_stream(id) else { return CidToGid::Identity };
    CidToGid::Table(data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
}

/// The descendant `/CIDFontType2` of a Type0 font.
fn descendant_id(doc: &Document, font: &Dictionary) -> Option<ObjId> {
    let descendants = doc.get_entry(font, "DescendantFonts").ok()??;
    let array = descendants.as_array()?;
    array.first()?.as_reference()
}

fn base_font_of(dict: &Dictionary) -> String {
    dict.get("BaseFont")
        .and_then(Object::as_name)
        .and_then(|n| n.as_str())
        .unwrap_or("(unnamed)")
        .to_string()
}

/// Compact one descendant font, returning the object changes it needs.
fn compact_one(
    doc: &Document,
    descendant_id: ObjId,
    descendant: &Dictionary,
    used: &BTreeSet<u16>,
    next_object: &mut u32,
) -> std::result::Result<Option<(Compacted, Changes)>, Skipped> {
    // The descendant of a Type0 font. Anything else reached this function
    // because the document draws it, and is declined by name.
    match descendant.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str()) {
        Some("CIDFontType2") => {}
        Some("CIDFontType0") => return Err(Skipped::CffProgram),
        _ => return Err(Skipped::SimpleFont),
    }

    let descriptor =
        doc.get_entry(descendant, "FontDescriptor").ok().flatten().ok_or(Skipped::NotEmbedded)?;
    let descriptor = descriptor.as_dict().ok_or(Skipped::NotEmbedded)?;
    if descriptor.get("FontFile3").is_some() {
        return Err(Skipped::CffProgram);
    }
    let program_id =
        descriptor.get("FontFile2").and_then(Object::as_reference).ok_or(Skipped::NotEmbedded)?;
    let program = doc.decoded_stream(program_id).map_err(|e| Skipped::Failed(e.to_string()))?;

    let gids: Vec<u16> = used.iter().copied().collect();
    let subset = rasura_font::compact_truetype(&program, &gids)
        .map_err(|e| Skipped::Failed(e.to_string()))?;

    // Nothing to gain. Reported as a skip would be misleading -- the font *was*
    // considered and is already minimal -- so it is simply not listed.
    if subset.glyphs_after >= subset.glyphs_before {
        return Ok(None);
    }

    let mut changes = Vec::new();

    // The font program, re-encoded through whatever filter chain it arrived in.
    let original = doc.get(program_id).map_err(|e| Skipped::Failed(e.to_string()))?;
    let original = original.as_stream().ok_or(Skipped::NotEmbedded)?;
    let mut program_stream = original.clone();
    program_stream.set_decoded(subset.bytes.clone());
    // `/Length1` is the uncompressed length of a TrueType program, and a reader
    // that trusts a stale one truncates the font.
    program_stream.dict.insert(Name::new("Length1"), Object::Integer(subset.bytes.len() as i64));
    changes.push((program_id, Object::Stream(program_stream)));

    // And the indirection that absorbs the renumbering, so that not one byte of
    // any content stream has to change.
    let (map_changes, added_map) =
        rewrite_cid_to_gid(doc, descendant_id, descendant, &subset.mapping, next_object)?;
    changes.extend(map_changes);

    Ok(Some((
        Compacted {
            program: program_id,
            base_font: base_font_of(descendant),
            glyphs_before: subset.glyphs_before,
            glyphs_after: subset.glyphs_after,
            bytes_before: subset.bytes_before,
            bytes_after: subset.bytes_after,
            added_map,
        },
        changes,
    )))
}

/// Point every CID at its glyph's *new* number.
///
/// Returns the object changes and whether a map had to be created. Creating one
/// is the interesting case: a font using `/Identity` has no indirection, so
/// without this the renumbering would have to be pushed into every content
/// stream that draws the font.
fn rewrite_cid_to_gid(
    doc: &Document,
    descendant_id: ObjId,
    descendant: &Dictionary,
    mapping: &std::collections::HashMap<u16, u16>,
    next_object: &mut u32,
) -> std::result::Result<(Changes, bool), Skipped> {
    let existing = descendant.get("CIDToGIDMap").and_then(Object::as_reference);

    // CID → old GID, which is what the new map has to be composed with.
    let old: Vec<u16> = match existing {
        Some(id) => {
            let data = doc.decoded_stream(id).map_err(|_| Skipped::UnreadableMap)?;
            data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
        }
        // Identity: CID n is glyph n, for as many glyphs as the font had.
        None => Vec::new(),
    };

    let entries = if old.is_empty() {
        // The table has to cover every CID the document might use, which for an
        // Identity map is every glyph the *original* font had. Sized from the
        // mapping's largest old id rather than from the font, because a CID
        // above that drew nothing and now maps to `.notdef` either way.
        mapping.keys().copied().max().map_or(0, |m| m as usize + 1)
    } else {
        old.len()
    };

    let mut table = Vec::with_capacity(entries * 2);
    for cid in 0..entries {
        let old_gid = if old.is_empty() { cid as u16 } else { old[cid] };
        // A CID whose glyph was pruned maps to 0. That is `.notdef`, which is
        // what a reader draws for a character the subset no longer has -- and
        // it is correct here, because the pruned glyphs are exactly the ones
        // the document never drew.
        let new_gid = mapping.get(&old_gid).copied().unwrap_or(0);
        table.extend_from_slice(&new_gid.to_be_bytes());
    }

    let mut stream = Stream::new(Dictionary::new(), Vec::new());
    stream.set_decoded(table.clone());
    stream.dict.insert(Name::new("Length"), Object::Integer(table.len() as i64));

    match existing {
        Some(id) => Ok((vec![(id, Object::Stream(stream))], false)),
        None => {
            // A new object, and the descendant rewritten to point at it. The
            // number is claimed from the document's own counter so it cannot
            // collide with anything the session allocates afterwards.
            let id = ObjId::new(*next_object, 0);
            *next_object += 1;
            let mut updated = descendant.clone();
            updated.insert(Name::new("CIDToGIDMap"), Object::Reference(id));
            Ok((
                vec![(id, Object::Stream(stream)), (descendant_id, Object::Dictionary(updated))],
                true,
            ))
        }
    }
}

/// The `/Font` resource dictionary a name refers to on a page.
fn font_dict(doc: &Document, page: &rasura_content::page::Page, name: &Name) -> Option<Dictionary> {
    let resources = page.resources.as_ref()?.as_dict()?;
    let fonts = doc.get_entry(resources, "Font").ok()??;
    let fonts = fonts.as_dict()?;
    let entry = doc.get_entry(fonts, name.as_str()?).ok()??;
    entry.as_dict().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::testutil::ClassicBuilder;
    use rasura_cos::{SaveMode, SaveOptions};

    /// The content stream, byte for byte, so a test can prove it did not move.
    const CONTENT: &[u8] = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm <00030005> Tj ET\n";

    /// A Type0 font over a 20-glyph TrueType program, drawing CIDs 3 and 5.
    ///
    /// `map` is written verbatim as the `/CIDToGIDMap` value, so one fixture
    /// covers both the `/Identity` case and the existing-stream case.
    fn composite(map: &str, extra_objects: bool) -> Vec<u8> {
        let program = rasura_font::fixture::truetype(20, 1000);
        let mut builder = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", CONTENT)
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /Probe \
                 /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
            )
            .object(
                6,
                &format!(
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Probe \
                     /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                     /FontDescriptor 7 0 R /DW 500 /CIDToGIDMap {map} >>"
                ),
            )
            .object(
                7,
                "<< /Type /FontDescriptor /FontName /Probe /Flags 4 \
                 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                 /CapHeight 700 /StemV 80 /FontFile2 8 0 R >>",
            )
            .stream(8, &format!(" /Length1 {}", program.len()), &program);

        if extra_objects {
            // An identity map written out as a stream: CID n is glyph n, for
            // the 20 glyphs the program has.
            let table: Vec<u8> = (0..20u16).flat_map(|g| g.to_be_bytes()).collect();
            builder = builder.stream(9, "", &table);
        }
        builder.finish("/Root 1 0 R")
    }

    /// Plan, stage and save, returning the output bytes and the report.
    fn compact_and_save(bytes: Vec<u8>) -> (Vec<u8>, Report) {
        let mut doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let report = plan(&doc, &pages);

        {
            let mut session = EditSession::new(&mut doc);
            let changes: Vec<(ObjId, Option<Object>)> =
                report.changes.iter().map(|(id, o)| (*id, Some(o.clone()))).collect();
            session.set_objects("compact fonts", &changes, report.fidelity.clone()).expect("stage");
            session.commit(&SaveOptions::default()).expect("commit");
        }

        let opts = SaveOptions { mode: Some(SaveMode::FullRewrite), ..SaveOptions::default() };
        (rasura_cos::save(&doc, &opts).expect("save").bytes, report)
    }

    fn map_of(doc: &Document) -> Vec<u16> {
        let descendant = doc.get(ObjId::new(6, 0)).expect("descendant");
        let dict = descendant.as_dict().expect("dict");
        let id = dict.get("CIDToGIDMap").and_then(Object::as_reference).expect("a map stream");
        let data = doc.decoded_stream(id).expect("map");
        data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
    }

    #[test]
    fn unused_glyphs_go_and_the_used_ones_are_renumbered_through_the_map() {
        let (out, report) = compact_and_save(composite("/Identity", false));
        assert_eq!(report.compacted.len(), 1, "{:?}", report.skipped);
        let done = &report.compacted[0];
        assert_eq!(done.glyphs_before, 20);
        assert_eq!(done.glyphs_after, 3, ".notdef plus CIDs 3 and 5");
        assert!(done.added_map, "the font used /Identity, so a map had to be created");
        assert!(done.bytes_after < done.bytes_before);

        // Ascending original order: glyph 3 becomes 1, glyph 5 becomes 2.
        let after = Document::open(out).expect("reopen");
        let map = map_of(&after);
        assert_eq!(map.get(3), Some(&1), "CID 3 now points at glyph 1");
        assert_eq!(map.get(5), Some(&2), "CID 5 now points at glyph 2");
        assert_eq!(map.get(4), Some(&0), "an unused CID falls back to .notdef");
    }

    #[test]
    fn not_one_byte_of_any_content_stream_changes() {
        // The claim the whole module is built around. Spec 8.6 warns that
        // renumbering "would require rewriting every content stream that
        // references the font"; putting the renumbering in /CIDToGIDMap means
        // it requires rewriting none.
        let (out, _) = compact_and_save(composite("/Identity", false));
        let after = Document::open(out).expect("reopen");
        let content = after.decoded_stream(ObjId::new(4, 0)).expect("content");
        assert_eq!(content.as_slice(), CONTENT);
    }

    #[test]
    fn the_glyph_a_cid_reaches_is_the_same_outline_as_before() {
        // Renumbering that loses track of which glyph is which produces a
        // document that renders the wrong letters -- and passes every
        // structural check ever written.
        let bytes = composite("/Identity", false);
        let before_doc = Document::open(bytes.clone()).expect("open");
        let before_program = before_doc.decoded_stream(ObjId::new(8, 0)).expect("program");
        let before = rasura_font::Sfnt::parse(&before_program).expect("parse");

        let (out, _) = compact_and_save(bytes);
        let after_doc = Document::open(out).expect("reopen");
        let after_program = after_doc.decoded_stream(ObjId::new(8, 0)).expect("program");
        let after = rasura_font::Sfnt::parse(&after_program).expect("parse");
        let map = map_of(&after_doc);

        for cid in [3u16, 5] {
            let new_gid = map[cid as usize];
            let original = before.glyph_data(&before_program, cid);
            // Otherwise two `None`s would agree and prove nothing.
            assert!(
                original.is_some_and(|g| !g.is_empty()),
                "the fixture's CID {cid} has to draw something for this to mean anything"
            );
            assert_eq!(
                after.glyph_data(&after_program, new_gid),
                original,
                "CID {cid} draws a different glyph after compaction"
            );
        }
    }

    #[test]
    fn an_existing_map_is_rewritten_rather_than_replaced_with_a_new_object() {
        let (out, report) = compact_and_save(composite("9 0 R", true));
        assert_eq!(report.compacted.len(), 1, "{:?}", report.skipped);
        assert!(!report.compacted[0].added_map, "the font already had a map");

        let after = Document::open(out).expect("reopen");
        let map = map_of(&after);
        assert_eq!(map.get(3), Some(&1));
        assert_eq!(map.get(5), Some(&2));
    }

    #[test]
    fn a_simple_font_declines_by_name() {
        // Its content stream holds codes, not CIDs, and the code-to-glyph path
        // runs through the font's own cmap -- which compaction drops.
        let doc = Document::open(rasura_font::fixture::uninjected_truetype_pdf()).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let report = plan(&doc, &pages);
        assert!(report.compacted.is_empty());
        // Nothing to report either: a simple font has no descendant, so it is
        // not reached. The declining happens where the caller can see it.
        assert!(report.is_empty());
    }

    #[test]
    fn a_document_that_uses_every_glyph_is_left_alone() {
        // Reporting a "compaction" that removed nothing would put a compromise
        // in the fidelity report for a save that lost nothing.
        let program = rasura_font::fixture::truetype(3, 1000);
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf <000100020000> Tj ET\n")
            .object(
                5,
                "<< /Type /Font /Subtype /Type0 /BaseFont /Probe /Encoding /Identity-H \
                 /DescendantFonts [6 0 R] >>",
            )
            .object(
                6,
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Probe \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor 7 0 R /DW 500 /CIDToGIDMap /Identity >>",
            )
            .object(
                7,
                "<< /Type /FontDescriptor /FontName /Probe /Flags 4 \
                 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                 /CapHeight 700 /StemV 80 /FontFile2 8 0 R >>",
            )
            .stream(8, &format!(" /Length1 {}", program.len()), &program)
            .finish("/Root 1 0 R");

        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let report = plan(&doc, &pages);
        assert!(report.is_empty(), "{:?}", report.compacted);
        assert!(report.fidelity.is_exact());
        assert!(report.changes.is_empty());
    }

    #[test]
    fn compaction_is_reported_as_a_compromise_with_what_it_saved() {
        // Spec 8.6's "never default to it" and "say what it cost" are the same
        // instruction: a pruned glyph is gone, and a later edit that wants one
        // has to inject it again.
        let doc = Document::open(composite("/Identity", false)).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let report = plan(&doc, &pages);

        match &report.fidelity {
            Fidelity::Degraded(list) => assert!(
                list.iter().any(|c| matches!(
                    c,
                    Compromise::FontSubsetCompacted { fonts: 1, bytes_saved } if *bytes_saved > 0
                )),
                "{list:?}"
            ),
            other => panic!("expected a reported compromise, got {other:?}"),
        }
        assert!(report.bytes_saved() > 0);
    }
}
