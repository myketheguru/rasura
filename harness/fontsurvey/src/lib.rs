//! Measurement harness for spec §18 question Q1.
//!
//! > Across the corpus, what fraction of embedded fonts have a usable
//! > `/ToUnicode` CMap? Subset LaTeX fonts frequently do not. If coverage is
//! > below roughly 85%, step 6 of §7.2 (glyph-name heuristics) and possibly a
//! > shape-matching fallback become Phase 3 work rather than Phase 8.
//!
//! # What this is not
//!
//! The CMap parser here is deliberately minimal and lives in the harness, not
//! in the library. The production one belongs in `rasura-layout` (§7.2) and
//! has to handle surrogate pairs, array destinations, and the whole
//! seven-strategy fallback chain. This one answers one question: *would the
//! first strategy work at all?* Building it in the harness keeps a measurement
//! tool from quietly becoming the real implementation.
//!
//! # What "usable" means here
//!
//! A `/ToUnicode` is counted usable when it
//!
//! 1. resolves to a stream that decodes,
//! 2. parses to at least one mapping, and
//! 3. is not wholly degenerate — every mapping landing on U+0000, U+FFFD, or
//!    the Private Use Area means the producer emitted a placeholder rather than
//!    a translation.
//!
//! Point 3 matters more than it looks. A `/ToUnicode` that maps every code to
//! U+0000 is worse than none at all: strategy 1 in §7.2 succeeds, the chain
//! stops, and the text extracts as nulls. Counting those as coverage would flatter
//! the number and produce exactly the wrong architectural decision.

use rasura_cos::document::Document;
use rasura_cos::object::{Dictionary, ObjId, Object};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FontKind {
    Type1,
    TrueType,
    Type0,
    Type3,
    Other,
}

impl FontKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FontKind::Type1 => "Type1",
            FontKind::TrueType => "TrueType",
            FontKind::Type0 => "Type0",
            FontKind::Type3 => "Type3",
            FontKind::Other => "other",
        }
    }
}

/// Why a `/ToUnicode` was not usable, or that it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToUnicodeState {
    Usable,
    /// No `/ToUnicode` entry at all.
    Absent,
    /// Present but the stream would not decode.
    Undecodable,
    /// Decoded but produced no mappings.
    Empty,
    /// Every mapping lands on U+0000, U+FFFD, or the Private Use Area.
    Degenerate,
}

impl ToUnicodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            ToUnicodeState::Usable => "usable",
            ToUnicodeState::Absent => "absent",
            ToUnicodeState::Undecodable => "undecodable",
            ToUnicodeState::Empty => "empty",
            ToUnicodeState::Degenerate => "degenerate",
        }
    }
}

/// What §7.2's later strategies would have to work with, for a font whose
/// `/ToUnicode` is unusable. This is the part that decides how much of step 6
/// is actually needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fallback {
    /// `/Encoding` supplies `/Differences` with glyph names. §7.2 step 2 applies,
    /// and the Adobe Glyph List does the rest.
    EncodingDifferences,
    /// A base encoding name only (`/WinAnsiEncoding` and friends). Step 2 applies.
    BaseEncodingOnly,
    /// A composite font with a predefined `/Encoding` CMap. Step 4 applies.
    PredefinedCmap,
    /// Nothing at the PDF level. Falls through to reading the embedded font's
    /// own `cmap` (step 5) or to glyph-name heuristics (step 6).
    FontProgramOnly,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::EncodingDifferences => "/Differences glyph names",
            Fallback::BaseEncodingOnly => "base encoding only",
            Fallback::PredefinedCmap => "predefined CMap",
            Fallback::FontProgramOnly => "font program only",
        }
    }
}

/// What the glyph names in `/Differences` look like.
///
/// This is the crux of how much of §7.2 step 6 is actually needed. Step 2 says
/// "glyph name -> Unicode via the Adobe Glyph List", and that works only if the
/// names *are* AGL names. The spec warns that LaTeX subset fonts emit names like
/// `g34`, which the AGL cannot resolve and which step 6's heuristics exist for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphNameStyle {
    /// No `/Differences` to look at.
    None,
    /// Recognisable names: `fi`, `adieresis`, `space`. The AGL resolves these.
    AdobeGlyphList,
    /// `uniXXXX` / `uXXXXX`. Trivially decodable, step 6's easy half.
    UniHex,
    /// `g34`, `cid12`, `index7`, `glyph200`. Carries no Unicode information at
    /// all -- these are what force step 5 or a shape-matching fallback.
    Opaque,
    /// A mixture, which in practice means some codes resolve and some do not.
    Mixed,
}

impl GlyphNameStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            GlyphNameStyle::None => "no /Differences",
            GlyphNameStyle::AdobeGlyphList => "AGL names",
            GlyphNameStyle::UniHex => "uniXXXX",
            GlyphNameStyle::Opaque => "opaque (gNN/cidNN)",
            GlyphNameStyle::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FontRecord {
    pub base_font: String,
    pub kind: FontKind,
    pub embedded: bool,
    pub subset: bool,
    pub tounicode: ToUnicodeState,
    /// How many codes the `/ToUnicode` mapped, usable or not.
    pub mappings: usize,
    /// Only meaningful when `tounicode != Usable`.
    pub fallback: Fallback,
    /// What the `/Differences` glyph names look like, if there are any.
    pub glyph_names: GlyphNameStyle,
    /// The embedded font program, when there is one. Spec 8.2.
    pub program: Option<ProgramRecord>,
}

/// What became of one embedded font program.
#[derive(Debug, Clone)]
pub struct ProgramRecord {
    /// The flavour sniffed from the bytes.
    pub flavour: &'static str,
    /// Whether the bytes disagree with what the PDF declared.
    pub mislabelled: bool,
    /// Glyph count on success, or why it could not be read.
    pub parsed: std::result::Result<usize, String>,
    /// For Type 1: the fraction of charstrings that open with `hsbw`/`sbw`.
    /// A parse that "succeeded" while producing shifted bytes shows up here
    /// and nowhere else.
    pub soundness: Option<f64>,
    /// For Type 1: where the font's own encoding came from. Spec 8.2 requires
    /// reading it, and §7.2 has no other source for a symbolic font whose PDF
    /// dictionary supplies no `/Encoding`.
    pub builtin_encoding: Option<&'static str>,
    /// For CFF: how the Type 2 charstrings fared under the §8.4 walker and
    /// subroutine inliner.
    pub charstrings: Option<CharstringStats>,
    /// What §8.4's injection did when tried on this font.
    pub injection: Option<InjectionOutcome>,
    /// Why it did not round-trip, for the cases that did not.
    pub injection_defect: Option<InjectionDefect>,
}

/// What walking and inlining a CFF font's charstrings produced.
#[derive(Debug, Clone, Default)]
pub struct CharstringStats {
    pub total: usize,
    /// Charstrings whose token walk consumed exactly their length. A `hintmask`
    /// miscounted by one byte desynchronises the walk, and the leftover shows
    /// up here.
    pub walked_exactly: usize,
    /// Charstrings that inlined without error.
    pub inlined: usize,
    /// Inlined charstrings with no `callsubr` left. One that survives would
    /// index the *destination* font's subroutines, which belong to another
    /// font entirely.
    pub fully_inlined: usize,
    pub had_subrs: usize,
    pub walked_over: usize,
    pub walked_short: usize,
    /// Zero-length INDEX entries: glyphs the subset dropped.
    pub empty: usize,
    pub short_sample: Option<String>,
}

/// Why a self-injection did not round-trip, for the cases that do not.
#[derive(Debug, Clone, Default)]
pub struct InjectionDefect {
    pub flavour: &'static str,
    pub detail: String,
    /// The file the font came from. Filled in by `survey`, which knows it;
    /// `try_injection` sees only bytes. Without it a defect can be counted but
    /// not reproduced, and a defect nobody can reproduce does not get fixed.
    pub source: String,
}

/// What happened when §8.4's injection was tried on a real font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionOutcome {
    /// Rebuilt, re-parsed, and the injected glyph came back intact.
    Verified,
    /// Rebuilt and re-parsed, but the glyph did not survive the round trip.
    GlyphLost,
    /// Rebuilt into something that will not parse.
    Unreadable,
    /// Refused, with a reason -- a CID-keyed CFF, a font with no outlines.
    Refused,
    /// The target font's own `loca` contradicts itself, so no rebuild can
    /// reproduce it. Not our defect, and counted apart from ones that are.
    TargetBroken,
    /// The font is already as large as its container format allows.
    ///
    /// Counted apart from `Refused` because it is a different kind of answer.
    /// A refusal is a choice this code made; this is a wall in the CFF spec —
    /// 65,535 charstrings, 65,536 CIDs — that a CJK subset preserving CID = GID
    /// reaches for real. Folding it into a bucket of 363 refusals would hide
    /// the only cases in the corpus where an edit is genuinely impossible.
    Full,
}

/// Inject a font's own last glyph back into it, and check the result.
///
/// Self-injection is a real exercise of the whole rebuild path: the tables are
/// a real font's, the charstring is a real charstring, and the answer is known
/// in advance. Fixtures cannot say whether §8.4 survives contact with the
/// fonts people actually ship.
fn try_injection(
    bytes: &[u8],
    flavour: &'static str,
    defect: &mut Option<InjectionDefect>,
) -> InjectionOutcome {
    if flavour == "sfnt/glyf" {
        let Ok(font) = rasura_font::Sfnt::parse(bytes) else {
            return InjectionOutcome::Refused;
        };
        if font.num_glyphs < 2 || font.loca.is_empty() {
            return InjectionOutcome::Refused;
        }
        let gid = font.num_glyphs - 1;
        let Ok(out) = rasura_font::inject_truetype(bytes, bytes, &[gid]) else {
            return InjectionOutcome::Refused;
        };
        let rebuilt = match rasura_font::Sfnt::parse(&out.bytes) {
            Ok(r) => r,
            Err(e) => {
                *defect = Some(InjectionDefect {
                    flavour,
                    detail: format!("the rebuilt font does not parse: {e}"),
                    ..Default::default()
                });
                return InjectionOutcome::Unreadable;
            }
        };
        let Some(&new_gid) = out.mapping.get(&gid) else {
            *defect = Some(InjectionDefect {
                flavour,
                detail: format!("glyph {gid} is absent from the returned mapping"),
                ..Default::default()
            });
            return InjectionOutcome::GlyphLost;
        };

        let src_glyph = font.glyph_data(bytes, gid).unwrap_or_default().to_vec();
        let new_glyph = rebuilt.glyph_data(&out.bytes, new_gid).unwrap_or_default().to_vec();

        // A composite glyph's bytes *should* differ: its component ids were
        // renumbered on the way in, which is the point. Mapping them back is
        // both the fair comparison and a check that the renumbering was right.
        let inverse: std::collections::HashMap<u16, u16> =
            out.mapping.iter().map(|(s, n)| (*n, *s)).collect();
        let mut restored = new_glyph.clone();
        let mut renumbering_ok = true;
        for (offset, component) in rasura_font::inject::components(&new_glyph) {
            match inverse.get(&component) {
                Some(original) => {
                    restored[offset..offset + 2].copy_from_slice(&original.to_be_bytes())
                }
                None => renumbering_ok = false,
            }
        }

        // Only glyphs the original font can actually describe. A `loca` with
        // fewer entries than `maxp` claims leaves the rest undescribed, and
        // "we did not reproduce data that was never there" is not a defect.
        let describable = font.loca.len().saturating_sub(1).min(font.num_glyphs as usize) as u16;
        let changed: Vec<u16> = (0..describable)
            .filter(|g| rebuilt.glyph_data(&out.bytes, *g) != font.glyph_data(bytes, *g))
            .collect();

        if renumbering_ok && restored == src_glyph && changed.is_empty() {
            InjectionOutcome::Verified
        } else if out.target_loca_inconsistent {
            *defect = Some(InjectionDefect {
                flavour,
                detail: format!(
                    "the target's own loca is inconsistent ({} glyphs, {} loca entries)",
                    font.num_glyphs,
                    font.loca.len()
                ),
                ..Default::default()
            });
            InjectionOutcome::TargetBroken
        } else {
            *defect = Some(InjectionDefect {
                flavour,
                detail: if !renumbering_ok {
                    "a component id was not in the mapping".into()
                } else if restored != src_glyph {
                    format!(
                        "the injected outline differs: {} bytes in, {} out",
                        src_glyph.len(),
                        new_glyph.len()
                    )
                } else {
                    format!(
                        "{} original glyph(s) changed, first {:?} ({} glyphs, loca {} entries)",
                        changed.len(),
                        changed.first(),
                        font.num_glyphs,
                        font.loca.len()
                    )
                },
                ..Default::default()
            });
            InjectionOutcome::GlyphLost
        }
    } else if flavour == "CFF" || flavour == "CFF/CID" {
        let Ok(font) = rasura_font::Cff::parse(bytes) else {
            return InjectionOutcome::Refused;
        };
        // A glyph with an actual charstring; a subset leaves empty entries.
        let Some(gid) = (0..font.glyph_count())
            .rev()
            .find(|g| font.charstring(bytes, *g).is_some_and(|c| !c.is_empty()))
        else {
            return InjectionOutcome::Refused;
        };
        let out = match rasura_font::inject_cff(bytes, bytes, &[gid as u16]) {
            Ok(out) => out,
            // "The font is full" is not the same answer as "we declined", and
            // the two used to land in the same bucket. It is the only outcome
            // here that no amount of work on this library can change.
            Err(e @ rasura_font::FontError::Full { .. }) => {
                *defect =
                    Some(InjectionDefect { flavour, detail: e.to_string(), ..Default::default() });
                return InjectionOutcome::Full;
            }
            Err(_) => return InjectionOutcome::Refused,
        };
        // Every failure below records *why*. These paths used to return bare,
        // which is how two CID CFF failures sat undiagnosed for a phase: the
        // summary said "2 glyph lost" and had nothing further to say, so there
        // was no thread to pull. A count without a reason is not a finding.
        let rebuilt = match rasura_font::Cff::parse(&out.bytes) {
            Ok(r) => r,
            Err(e) => {
                *defect = Some(InjectionDefect {
                    flavour,
                    detail: format!("the rebuilt font does not parse: {e}"),
                    ..Default::default()
                });
                return InjectionOutcome::Unreadable;
            }
        };
        let Some(&new_gid) = out.mapping.get(&(gid as u16)) else {
            *defect = Some(InjectionDefect {
                flavour,
                detail: format!("glyph {gid} is absent from the returned mapping"),
                ..Default::default()
            });
            return InjectionOutcome::GlyphLost;
        };
        if rebuilt.glyph_count() != font.glyph_count() + 1 {
            *defect = Some(InjectionDefect {
                flavour,
                detail: format!(
                    "glyph count {} -> {}, expected {}",
                    font.glyph_count(),
                    rebuilt.glyph_count(),
                    font.glyph_count() + 1
                ),
                ..Default::default()
            });
            return InjectionOutcome::GlyphLost;
        }
        // The injected charstring is the inlined form of the original.
        let expected = rasura_font::charstring::inline_subrs(
            bytes,
            font.charstring(bytes, gid).unwrap_or_default(),
            font.local_subrs_for(gid),
            &font.global_subrs,
        );
        let got = rebuilt.charstring(&out.bytes, new_gid as usize).map(|c| c.to_vec());
        let originals = (0..font.glyph_count())
            .all(|g| rebuilt.charstring(&out.bytes, g) == font.charstring(bytes, g));
        match (expected, got) {
            (Ok(e), Some(g)) if e == g && originals => InjectionOutcome::Verified,
            (e, g) => {
                *defect = Some(InjectionDefect {
                    flavour,
                    detail: format!(
                        "charstring {}, originals {}, inline {}",
                        match (&e, &g) {
                            (Ok(e), Some(g)) if e == g => "matches",
                            (_, None) => "missing",
                            _ => "differs",
                        },
                        if originals { "intact" } else { "CHANGED" },
                        if e.is_ok() { "ok" } else { "failed" }
                    ),
                    ..Default::default()
                });
                InjectionOutcome::GlyphLost
            }
        }
    } else {
        InjectionOutcome::Refused
    }
}

/// Walk and inline every charstring in a CFF font.
fn walk_charstrings(data: &[u8], cff: &rasura_font::Cff) -> CharstringStats {
    use rasura_font::charstring;
    let mut s = CharstringStats::default();
    for gid in 0..cff.glyph_count() {
        let Some(cs) = cff.charstring(data, gid) else { continue };
        // A subset CFF keeps an INDEX entry for every glyph it dropped, with
        // zero length. Those are not charstrings and counting them as ones the
        // walker failed on says 31% when the answer is 100%.
        if cs.is_empty() {
            s.empty += 1;
            continue;
        }
        s.total += 1;

        // Did the walk stay in step? The last token must end exactly at the
        // end of the data.
        let toks = charstring::tokens(cs);
        match toks.last().map(|(o, l, _)| o + l) {
            Some(end) if end == cs.len() => s.walked_exactly += 1,
            Some(end) if end > cs.len() => s.walked_over += 1,
            _ => {
                s.walked_short += 1;
                if s.short_sample.is_none() {
                    let end = toks.last().map(|(o, l, _)| o + l).unwrap_or(0);
                    s.short_sample = Some(format!(
                        "{} of {} bytes, tail {:?}, last tokens {:?}",
                        end,
                        cs.len(),
                        &cs[end.min(cs.len())..],
                        toks.iter().rev().take(3).map(|(_, _, t)| *t).collect::<Vec<_>>()
                    ));
                }
            }
        }
        if charstring::calls_subroutine(cs) {
            s.had_subrs += 1;
        }

        let local = cff.local_subrs_for(gid);
        if let Ok(out) = charstring::inline_subrs(data, cs, local, &cff.global_subrs) {
            s.inlined += 1;
            if !charstring::calls_subroutine(&out) {
                s.fully_inlined += 1;
            }
        }
    }
    s
}

#[derive(Debug, Clone)]
pub struct DocumentSurvey {
    pub name: String,
    pub producer: String,
    pub producer_family: &'static str,
    pub fonts: Vec<FontRecord>,
}

/// Survey every font in one document.
pub fn survey(name: &str, doc: &Document) -> DocumentSurvey {
    let producer = read_producer(doc);
    let producer_family = classify_producer(&producer);

    // A Type0 font's descendant is itself a /Type /Font object. Counting both
    // would double-count every CJK and every modern subset font, and would
    // count the descendant as having no /ToUnicode -- which lives on the parent.
    let mut descendants: HashSet<ObjId> = HashSet::new();
    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        let Ok(obj) = doc.get(id) else { continue };
        let Some(dict) = obj.as_dict() else { continue };
        if !is_font(dict) {
            continue;
        }
        if let Some(Object::Array(kids)) = dict.get("DescendantFonts") {
            for k in kids {
                if let Some(kid) = k.as_reference() {
                    descendants.insert(kid);
                }
            }
        } else if let Some(r) = dict.get("DescendantFonts").and_then(Object::as_reference) {
            // Some producers write a reference to the array rather than the
            // array itself.
            if let Ok(arr) = doc.resolve(&Object::Reference(r))
                && let Some(items) = arr.as_array()
            {
                for k in items {
                    if let Some(kid) = k.as_reference() {
                        descendants.insert(kid);
                    }
                }
            }
        }
    }

    let mut fonts = Vec::new();
    let mut seen: HashSet<ObjId> = HashSet::new();
    for number in doc.xref().live_objects() {
        let id = ObjId::new(number, generation_of(doc, number));
        if descendants.contains(&id) || !seen.insert(id) {
            continue;
        }
        let Ok(obj) = doc.get(id) else { continue };
        let Some(dict) = obj.as_dict() else { continue };
        if !is_font(dict) {
            continue;
        }
        fonts.push(examine_font(doc, id, dict, name));
    }

    DocumentSurvey { name: name.to_string(), producer, producer_family, fonts }
}

fn is_font(dict: &Dictionary) -> bool {
    dict.type_name().is_some_and(|t| t.as_bytes() == b"Font")
}

fn examine_font(doc: &Document, id: ObjId, dict: &Dictionary, name: &str) -> FontRecord {
    let subtype = dict
        .get("Subtype")
        .and_then(Object::as_name)
        .map(|n| n.as_bytes().to_vec())
        .unwrap_or_default();
    let kind = match subtype.as_slice() {
        b"Type1" | b"MMType1" => FontKind::Type1,
        b"TrueType" => FontKind::TrueType,
        b"Type0" => FontKind::Type0,
        b"Type3" => FontKind::Type3,
        _ => FontKind::Other,
    };

    let base_font = dict
        .get("BaseFont")
        .and_then(Object::as_name)
        .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
        .unwrap_or_else(|| "(none)".into());

    // A subset is tagged with six uppercase letters and a plus sign.
    let subset = base_font.len() > 7
        && base_font.as_bytes()[6] == b'+'
        && base_font.as_bytes()[..6].iter().all(|b| b.is_ascii_uppercase());

    // For a Type0 the font program hangs off the descendant, not the parent.
    let descriptor_owner = if kind == FontKind::Type0 {
        descendant_dict(doc, dict).unwrap_or_else(|| dict.clone())
    } else {
        dict.clone()
    };
    let embedded = match kind {
        // Type 3 glyphs are content streams in the file: always "embedded",
        // but they carry no font program, so they are reported separately.
        FontKind::Type3 => true,
        _ => doc
            .get_entry(&descriptor_owner, "FontDescriptor")
            .ok()
            .flatten()
            .and_then(|d| {
                d.as_dict().map(|d| {
                    ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.contains_key(k))
                })
            })
            .unwrap_or(false),
    };

    let (tounicode, mappings) = examine_tounicode(doc, dict);
    let fallback = classify_fallback(doc, dict, &descriptor_owner, kind);

    let glyph_names = classify_glyph_names(doc, dict);

    // Phase 4: can the embedded program actually be read? Until this parses,
    // nothing above it -- shaping, injection, substitution -- can be built on
    // it, so the number to know first is how much of the corpus is reachable.
    let program = doc
        .get_entry(&descriptor_owner, "FontDescriptor")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().and_then(|d| rasura_font::program::from_descriptor(doc, d)))
        .map(|p| {
            let mut soundness = None;
            let mut builtin_encoding = None;
            let mut charstrings = None;
            let parsed = if p.flavour.is_sfnt() {
                rasura_font::Sfnt::parse(&p.bytes)
                    .map(|f| f.num_glyphs as usize)
                    .map_err(|e| e.to_string())
            } else if p.flavour.is_cff() {
                rasura_font::Cff::parse(&p.bytes)
                    .inspect(|c| charstrings = Some(walk_charstrings(&p.bytes, c)))
                    .map(|c| c.glyph_count())
                    .map_err(|e| e.to_string())
            } else {
                rasura_font::Type1::parse(&p.bytes)
                    .inspect(|f| {
                        soundness = Some(f.soundness());
                        builtin_encoding = if f.standard_encoding {
                            Some("StandardEncoding")
                        } else if !f.encoding.is_empty() {
                            Some("built-in array")
                        } else {
                            Some("none")
                        };
                    })
                    .map(|f| f.glyph_count())
                    .map_err(|e| e.to_string())
            };
            let mut injection_defect: Option<InjectionDefect> = None;
            let injection = parsed
                .as_ref()
                .ok()
                .map(|_| try_injection(&p.bytes, p.flavour.name(), &mut injection_defect));
            if let Some(d) = injection_defect.as_mut() {
                d.source = name.to_string();
            }
            ProgramRecord {
                injection,
                injection_defect,
                flavour: p.flavour.name(),
                mislabelled: p.is_mislabelled(),
                parsed,
                soundness,
                builtin_encoding,
                charstrings,
            }
        });

    let _ = id;
    FontRecord {
        base_font,
        kind,
        embedded,
        subset,
        tounicode,
        mappings,
        fallback,
        glyph_names,
        program,
    }
}

/// Inspect `/Encoding` `/Differences` and judge what the names are made of.
fn classify_glyph_names(doc: &Document, dict: &Dictionary) -> GlyphNameStyle {
    let Ok(Some(encoding)) = doc.get_entry(dict, "Encoding") else {
        return GlyphNameStyle::None;
    };
    let Some(enc) = encoding.as_dict() else { return GlyphNameStyle::None };
    let Ok(Some(differences)) = doc.get_entry(enc, "Differences") else {
        return GlyphNameStyle::None;
    };
    let Some(items) = differences.as_array() else { return GlyphNameStyle::None };

    let (mut agl, mut unihex, mut opaque) = (0usize, 0usize, 0usize);
    for item in items {
        let Some(name) = item.as_name() else { continue };
        match name_style(name.as_bytes()) {
            GlyphNameStyle::AdobeGlyphList => agl += 1,
            GlyphNameStyle::UniHex => unihex += 1,
            GlyphNameStyle::Opaque => opaque += 1,
            _ => {}
        }
    }

    match (agl, unihex, opaque) {
        (0, 0, 0) => GlyphNameStyle::None,
        (a, 0, 0) if a > 0 => GlyphNameStyle::AdobeGlyphList,
        (0, u, 0) if u > 0 => GlyphNameStyle::UniHex,
        (0, 0, o) if o > 0 => GlyphNameStyle::Opaque,
        _ => GlyphNameStyle::Mixed,
    }
}

fn name_style(name: &[u8]) -> GlyphNameStyle {
    // `uniXXXX` and `uXXXXX` carry the code point in the name.
    if let Some(rest) = name.strip_prefix(b"uni")
        && rest.len() >= 4
        && rest.iter().all(|b| b.is_ascii_hexdigit())
    {
        return GlyphNameStyle::UniHex;
    }
    if let Some(rest) = name.strip_prefix(b"u")
        && (4..=6).contains(&rest.len())
        && rest.iter().all(|b| b.is_ascii_hexdigit())
    {
        return GlyphNameStyle::UniHex;
    }

    // `g34`, `cid12`, `index7`, `glyph200`: an identifier plus a number, and no
    // information about what the glyph means.
    for prefix in [b"cid".as_slice(), b"glyph".as_slice(), b"index".as_slice(), b"g".as_slice()] {
        if let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
            && rest.iter().all(|b| b.is_ascii_digit())
        {
            return GlyphNameStyle::Opaque;
        }
    }
    // A bare number is equally opaque.
    if !name.is_empty() && name.iter().all(|b| b.is_ascii_digit()) {
        return GlyphNameStyle::Opaque;
    }

    // Anything else is treated as a name the Adobe Glyph List might know. The
    // AGL itself is Phase 3 work; this counts candidates, not confirmed hits,
    // so the AGL column here is an upper bound.
    if name.is_empty() { GlyphNameStyle::None } else { GlyphNameStyle::AdobeGlyphList }
}

fn descendant_dict(doc: &Document, dict: &Dictionary) -> Option<Dictionary> {
    let arr = doc.get_entry(dict, "DescendantFonts").ok()??;
    let first = arr.as_array()?.first()?.clone();
    let resolved = doc.resolve(&first).ok()?;
    resolved.as_dict().cloned()
}

fn examine_tounicode(doc: &Document, dict: &Dictionary) -> (ToUnicodeState, usize) {
    let Some(reference) = dict.get("ToUnicode") else {
        return (ToUnicodeState::Absent, 0);
    };
    let Some(id) = reference.as_reference() else {
        return (ToUnicodeState::Undecodable, 0);
    };
    let Ok(data) = doc.decoded_stream(id) else {
        return (ToUnicodeState::Undecodable, 0);
    };

    let mappings = parse_cmap(&data);
    if mappings.is_empty() {
        return (ToUnicodeState::Empty, 0);
    }
    let n = mappings.len();
    let all_degenerate = mappings.values().all(|dst| dst.iter().all(|&c| is_degenerate(c)));
    if all_degenerate { (ToUnicodeState::Degenerate, n) } else { (ToUnicodeState::Usable, n) }
}

fn is_degenerate(c: u32) -> bool {
    // Private Use Area (BMP plus the two supplementary planes).
    c == 0
        || c == 0xfffd
        || (0xe000..=0xf8ff).contains(&c)
        || (0xf0000..=0xffffd).contains(&c)
        || (0x100000..=0x10fffd).contains(&c)
}

fn classify_fallback(
    doc: &Document,
    dict: &Dictionary,
    descriptor_owner: &Dictionary,
    kind: FontKind,
) -> Fallback {
    let _ = descriptor_owner;
    let encoding = doc.get_entry(dict, "Encoding").ok().flatten();
    match encoding.as_deref() {
        Some(Object::Dictionary(d)) => {
            if d.contains_key("Differences") {
                Fallback::EncodingDifferences
            } else if d.contains_key("BaseEncoding") {
                Fallback::BaseEncodingOnly
            } else {
                Fallback::FontProgramOnly
            }
        }
        Some(Object::Name(_)) => {
            if kind == FontKind::Type0 {
                // A predefined CMap name such as /UniJIS-UCS2-H, or /Identity-H
                // which supplies no Unicode at all.
                Fallback::PredefinedCmap
            } else {
                Fallback::BaseEncodingOnly
            }
        }
        // An embedded CMap stream for a composite font.
        Some(Object::Stream(_)) => Fallback::PredefinedCmap,
        _ => Fallback::FontProgramOnly,
    }
}

// ---------------------------------------------------------------------------
// Minimal CMap parsing
// ---------------------------------------------------------------------------

/// Parse `beginbfchar`/`beginbfrange` sections into a code -> scalars map.
///
/// Enough to answer "does this translate anything". The production parser in
/// §7.2 has more to do.
pub fn parse_cmap(data: &[u8]) -> BTreeMap<u32, Vec<u32>> {
    let mut out = BTreeMap::new();
    let mut i = 0usize;
    while i < data.len() {
        if data[i..].starts_with(b"beginbfchar") {
            i += b"beginbfchar".len();
            i = parse_bfchar(data, i, &mut out);
        } else if data[i..].starts_with(b"beginbfrange") {
            i += b"beginbfrange".len();
            i = parse_bfrange(data, i, &mut out);
        } else {
            i += 1;
        }
    }
    out
}

fn parse_bfchar(data: &[u8], mut i: usize, out: &mut BTreeMap<u32, Vec<u32>>) -> usize {
    loop {
        let Some((src, next)) = next_hex(data, i) else { return i };
        if next > data.len() {
            return data.len();
        }
        // `endbfchar` before a pair completes means the section is done.
        let Some((dst, next2)) = next_hex(data, next) else { return next };
        i = next2;
        let code = be_value(&src);
        out.insert(code, utf16be_scalars(&dst));
        if section_ended(data, i, b"endbfchar") {
            return i;
        }
        if i >= data.len() {
            return i;
        }
    }
}

fn parse_bfrange(data: &[u8], mut i: usize, out: &mut BTreeMap<u32, Vec<u32>>) -> usize {
    loop {
        let Some((lo, n1)) = next_hex(data, i) else { return i };
        let Some((hi, n2)) = next_hex(data, n1) else { return n1 };
        let lo_v = be_value(&lo);
        let hi_v = be_value(&hi);

        // The destination is either a single value that increments across the
        // range, or an array with one entry per code.
        let after_ws = skip_ws(data, n2);
        if data.get(after_ws) == Some(&b'[') {
            let mut j = after_ws + 1;
            let mut code = lo_v;
            loop {
                let j2 = skip_ws(data, j);
                if data.get(j2) == Some(&b']') {
                    i = j2 + 1;
                    break;
                }
                let Some((dst, n)) = next_hex(data, j) else {
                    i = j2;
                    break;
                };
                out.insert(code, utf16be_scalars(&dst));
                code = code.saturating_add(1);
                j = n;
                if j >= data.len() {
                    i = j;
                    break;
                }
            }
        } else {
            let Some((dst, n3)) = next_hex(data, n2) else { return n2 };
            i = n3;
            let base = utf16be_scalars(&dst);
            // Ranges in the wild are occasionally enormous and occasionally
            // inverted. Cap the work; the count is what matters here.
            let span = hi_v.saturating_sub(lo_v).min(0xffff);
            for k in 0..=span {
                let mut scalars = base.clone();
                if let Some(last) = scalars.last_mut() {
                    *last = last.saturating_add(k);
                }
                out.insert(lo_v.saturating_add(k), scalars);
            }
        }

        if section_ended(data, i, b"endbfrange") || i >= data.len() {
            return i;
        }
    }
}

fn section_ended(data: &[u8], i: usize, keyword: &[u8]) -> bool {
    let j = skip_ws(data, i);
    data.get(j..).is_some_and(|s| s.starts_with(keyword))
}

fn skip_ws(data: &[u8], mut i: usize) -> usize {
    while i < data.len() && data[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Read the next `<...>` hex string, returning its bytes and the position after
/// it. Stops at a keyword so a missing `>` cannot run away.
fn next_hex(data: &[u8], mut i: usize) -> Option<(Vec<u8>, usize)> {
    while i < data.len() && data[i] != b'<' {
        // A letter here means the next token is a keyword, not a hex string.
        if data[i].is_ascii_alphabetic() || data[i] == b'[' || data[i] == b']' {
            return None;
        }
        i += 1;
    }
    if i >= data.len() {
        return None;
    }
    let start = i + 1;
    let mut j = start;
    while j < data.len() && data[j] != b'>' {
        j += 1;
    }
    if j >= data.len() {
        return None;
    }
    let mut bytes = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in &data[start..j] {
        let Some(v) = hex_val(b) else { continue };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                bytes.push(h << 4 | v);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        bytes.push(h << 4);
    }
    Some((bytes, j + 1))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn be_value(bytes: &[u8]) -> u32 {
    bytes.iter().take(4).fold(0u32, |acc, &b| acc << 8 | b as u32)
}

/// Decode a UTF-16BE destination, resolving surrogate pairs.
fn utf16be_scalars(bytes: &[u8]) -> Vec<u32> {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xd800..0xdc00).contains(&u) && i + 1 < units.len() {
            let lo = units[i + 1];
            if (0xdc00..0xe000).contains(&lo) {
                let c = 0x10000 + (((u as u32) - 0xd800) << 10) + ((lo as u32) - 0xdc00);
                out.push(c);
                i += 2;
                continue;
            }
        }
        out.push(u as u32);
        i += 1;
    }
    if out.is_empty() && !bytes.is_empty() {
        // An odd-length destination; treat the single byte as a code point so
        // it is not silently counted as "no mapping".
        out.push(bytes[0] as u32);
    }
    out
}

// ---------------------------------------------------------------------------
// Producers
// ---------------------------------------------------------------------------

fn read_producer(doc: &Document) -> String {
    let mut parts = Vec::new();
    if let Some(info) = doc.trailer().get("Info")
        && let Ok(resolved) = doc.resolve(info)
        && let Some(d) = resolved.as_dict()
    {
        for key in ["Producer", "Creator"] {
            if let Some(v) = d.get(key).and_then(Object::as_string) {
                let s = v.as_text();
                if !s.trim().is_empty() {
                    parts.push(s);
                }
            }
        }
    }
    parts.join(" | ")
}

/// Bucket a producer string into a family. Deliberately coarse: the question is
/// "is this TeX", not "which TeX".
pub fn classify_producer(producer: &str) -> &'static str {
    let p = producer.to_ascii_lowercase();
    let has = |needle: &str| p.contains(needle);
    if has("pdftex") || has("pdfetex") {
        "pdfTeX"
    } else if has("xetex") || has("xdvipdfmx") {
        "XeTeX"
    } else if has("luatex") || has("lualatex") {
        "LuaTeX"
    } else if has("dvips") || has("dvipdf") {
        "dvips/TeX"
    } else if has("tex") {
        "other TeX"
    } else if has("ghostscript") || has("gpl ghostscript") {
        "Ghostscript"
    } else if has("microsoft") || has("word") || has("powerpoint") || has("excel") {
        "Microsoft Office"
    } else if has("libreoffice") || has("openoffice") {
        "LibreOffice"
    } else if has("indesign") || has("acrobat") || has("adobe") || has("illustrator") {
        "Adobe"
    } else if has("skia") || has("chrome") || has("chromium") {
        "Chrome/Skia"
    } else if has("quartz") || has("mac os x") || has("cairo") && has("quartz") {
        "Quartz"
    } else if has("cairo") {
        "cairo"
    } else if has("wkhtmltopdf") {
        "wkhtmltopdf"
    } else if has("prince") {
        "Prince"
    } else if p.trim().is_empty() {
        "(no producer)"
    } else {
        "other"
    }
}

fn generation_of(doc: &Document, number: u32) -> u16 {
    match doc.xref().get(number) {
        Some(rasura_cos::xref::XrefEntry::InFile { generation, .. }) => generation,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bfchar_pairs() {
        let cmap = b"1 beginbfchar\n<0041> <0061>\n<0042> <0062>\nendbfchar";
        let m = parse_cmap(cmap);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&0x41], vec![0x61]);
        assert_eq!(m[&0x42], vec![0x62]);
    }

    #[test]
    fn parses_bfrange_with_incrementing_destination() {
        let cmap = b"1 beginbfrange\n<0003> <0005> <0041>\nendbfrange";
        let m = parse_cmap(cmap);
        assert_eq!(m.len(), 3);
        assert_eq!(m[&3], vec![0x41]);
        assert_eq!(m[&4], vec![0x42]);
        assert_eq!(m[&5], vec![0x43]);
    }

    #[test]
    fn parses_bfrange_with_array_destination() {
        let cmap = b"1 beginbfrange\n<0010> <0012> [<0041> <0042> <0043>]\nendbfrange";
        let m = parse_cmap(cmap);
        assert_eq!(m.len(), 3);
        assert_eq!(m[&0x10], vec![0x41]);
        assert_eq!(m[&0x12], vec![0x43]);
    }

    #[test]
    fn resolves_surrogate_pairs() {
        // U+1D400 MATHEMATICAL BOLD CAPITAL A.
        let cmap = b"1 beginbfchar\n<0001> <D835DC00>\nendbfchar";
        let m = parse_cmap(cmap);
        assert_eq!(m[&1], vec![0x1d400]);
    }

    #[test]
    fn handles_multi_char_destinations() {
        // A ligature mapping to two code points.
        let cmap = b"1 beginbfchar\n<0001> <00660069>\nendbfchar";
        let m = parse_cmap(cmap);
        assert_eq!(m[&1], vec![0x66, 0x69]);
    }

    #[test]
    fn handles_both_sections_in_one_cmap() {
        let cmap = b"2 beginbfchar\n<01> <0041>\n<02> <0042>\nendbfchar\n\
                     1 beginbfrange\n<10> <12> <0061>\nendbfrange";
        let m = parse_cmap(cmap);
        assert_eq!(m.len(), 5);
    }

    #[test]
    fn degenerate_detection() {
        assert!(is_degenerate(0));
        assert!(is_degenerate(0xfffd));
        assert!(is_degenerate(0xf041), "PUA placeholders are not translations");
        assert!(!is_degenerate(0x41));
    }

    #[test]
    fn empty_or_garbage_cmap_yields_nothing_without_hanging() {
        assert!(parse_cmap(b"").is_empty());
        assert!(parse_cmap(b"beginbfchar endbfchar").is_empty());
        assert!(parse_cmap(b"beginbfrange <0001>").is_empty());
        // A missing '>' must not run away.
        assert!(parse_cmap(b"beginbfchar <0041").is_empty());
    }

    #[test]
    fn producer_families() {
        assert_eq!(classify_producer("pdfTeX-1.40.25"), "pdfTeX");
        assert_eq!(classify_producer("XeTeX output 2024"), "XeTeX");
        assert_eq!(classify_producer("LuaTeX-1.17"), "LuaTeX");
        assert_eq!(classify_producer("Skia/PDF m120"), "Chrome/Skia");
        assert_eq!(classify_producer("Microsoft: Word 2019"), "Microsoft Office");
        assert_eq!(classify_producer(""), "(no producer)");
    }
}
