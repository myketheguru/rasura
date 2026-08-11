//! Supplying a font the document does not have. Spec 11.3, 8.4.
//!
//! > The browser cannot see system fonts. Make this explicit and pleasant.
//!
//! [`crate::fonts`] answers "what is missing". This answers "here it is".
//!
//! # What registering a font actually does
//!
//! Nothing, until an edit needs it. A registered font is bytes held against the
//! moment a caller types a character the document's own embedded font cannot
//! draw — at which point §8.4's glyph injection takes the outline out of the
//! registered font and puts it *into* the document's, and the page carries on
//! using one typeface.
//!
//! That is the difference between this and substitution, and it is the whole
//! reason the [`Fidelity::Reembedded`](crate::edit::Fidelity::Reembedded) rung
//! exists. Substituting means the amended text is set in a different typeface
//! from the text around it, which is visible to a reader and invisible in a
//! diff. Injecting means the document contains one more glyph of the typeface
//! it already used.
//!
//! # What is supported, and what declines
//!
//! **TrueType simple fonts with `/FontFile2`.** That is the shape §8.4's
//! injection is built for and proven against — `examples/realfont` puts `É`
//! into a subset of Roboto and pdfium renders it.
//!
//! Everything else declines by name:
//!
//! - **Not embedded.** There is no program to inject into. The document names a
//!   font and expects the reader to find it, so what a reader shows is already
//!   not what the author saw; adding a glyph to a font that is not there would
//!   be writing into nothing.
//! - **CFF (`/FontFile3`).** Charstring injection exists
//!   ([`rasura_font::inject_cff`]) and the dictionary plumbing around it
//!   does not. Named rather than half-built.
//! - **Composite fonts.** A Type0's codes come from a CMap, so adding a glyph
//!   means adding a CID *and* extending the encoding — a different operation
//!   from adding a code to `/Differences`.
//! - **Characters with no WinAnsi code.** The code a page draws has to come from
//!   somewhere; for a simple font that is `/Differences` over a base encoding,
//!   and picking a free code for a character outside Latin-1 means choosing
//!   which of the document's existing codes to shadow. Declined rather than
//!   guessed.

use crate::error::{Code, Error, Result};
use crate::page::Page;
use rasura_cos::object::{Dictionary, Name, Object, Stream};
use rasura_cos::{Document, ObjId};
use rasura_font::{Added, Sfnt};

/// A font a caller has supplied. Spec 11.3.
#[derive(Debug, Clone)]
pub struct Registered {
    pub(crate) bytes: Vec<u8>,
    /// The typeface this font is for, when the caller said. `None` means "use
    /// it for anything", which is what a caller supplying one fallback wants.
    pub(crate) match_for: Option<String>,
}

/// A handle to a registered font. Spec 11.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontHandle(pub(crate) usize);

/// How to register a font.
#[derive(Debug, Clone, Default)]
pub struct RegisterOptions {
    /// Bind this font to one of the document's, by `/BaseFont` family name.
    ///
    /// Optional, and worth setting. Without it the font is a candidate for
    /// every typeface in the document, which is right for a single fallback
    /// and wrong when a caller has supplied the real Minion and the real
    /// Helvetica and expects each to be used for its own text.
    pub match_for: Option<String>,
}

/// Why glyphs could not be supplied.
fn decline(reason: &str) -> Error {
    Error::new(Code::FontUnavailable, reason)
}

/// What supplying glyphs changed.
#[derive(Debug, Clone)]
pub struct Supplied {
    pub changes: Vec<(ObjId, Object)>,
    /// The characters that were injected.
    pub characters: Vec<char>,
}

/// Which of `text`'s characters the run's embedded font has no outline for.
///
/// # Why the encoder cannot answer this
///
/// It looked as though it could, and it does not. [`crate::edit::Session`]
/// originally injected when `replace_text` returned `Unencodable` — and on the
/// case this feature exists for, it never did.
///
/// The encoder inverts the *decoder*: it knows which code produces which
/// character, because that is what `/Encoding` says. A simple font with
/// `/WinAnsiEncoding` therefore "can write" every character in Latin-1, since
/// every one of them has a code. Whether the embedded program contains an
/// outline at that code is a different question, and the encoder never asks it.
///
/// So a document embedding seven letters of Roboto accepted `É` happily, wrote
/// code 0xC9, and drew nothing. That is precisely the silent failure §8.4
/// exists to prevent, and it was found by a test asserting the injection had
/// happened rather than by one asserting the edit succeeded.
///
/// The real question is asked of the font program: resolve the character
/// through the embedded `cmap` and see whether a glyph comes back.
///
/// Returns empty when the font is **not** embedded. A reader substitutes its
/// own font for those, so what a glyph looks like is already out of our hands —
/// and there is no program to inject into.
pub(crate) fn missing_glyphs(doc: &Document, page: &Page, run: usize, text: &str) -> Vec<char> {
    let Some(program) = embedded_program(doc, page, run) else { return Vec::new() };
    let Ok(font) = Sfnt::parse(&program) else { return Vec::new() };
    let Some(cmap) = rasura_font::Cmap::parse(&program, &font) else {
        // A TrueType simple font with no `cmap` reaches glyphs some other way,
        // and guessing which would produce injections nothing needed.
        return Vec::new();
    };
    let Some(table) = cmap.best_unicode() else { return Vec::new() };

    let mut missing = Vec::new();
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        let has = table.lookup(&program, c as u32).is_some_and(|gid| gid != 0);
        if !has && !missing.contains(&c) {
            missing.push(c);
        }
    }
    missing
}

/// The embedded TrueType program behind a run's font, if there is one.
fn embedded_program(doc: &Document, page: &Page, run: usize) -> Option<Vec<u8>> {
    let name = page.raw().runs.get(run)?.run.font_name.clone()?;
    let (_, font_dict) = font_object(doc, page, &name)?;
    let (_, descriptor) = descriptor_of(doc, &font_dict)?;
    let id = descriptor.get("FontFile2")?.as_reference()?;
    doc.decoded_stream(id).ok().map(|b| b.to_vec())
}

/// Inject the characters a run's font cannot write, from the registry.
///
/// `next_object` is the document's object counter; a `/ToUnicode` stream may
/// need a number that does not collide with anything a session allocates later.
pub(crate) fn supply(
    doc: &Document,
    page: &Page,
    run: usize,
    wanted: &[char],
    registry: &[Registered],
    next_object: &mut u32,
) -> Result<Supplied> {
    if registry.is_empty() {
        return Err(decline(
            "no fonts have been registered; call registerFont with a font that has these glyphs",
        ));
    }

    let editable = page.raw();
    let resolved = editable.runs.get(run).ok_or_else(|| decline("no such run on this page"))?;
    let font_name =
        resolved.run.font_name.clone().ok_or_else(|| decline("the run names no font"))?;

    let (font_id, font_dict) = font_object(doc, page, &font_name)
        .ok_or_else(|| decline("the run's font is not an object this layer can rewrite"))?;

    match font_dict.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str()) {
        Some("TrueType") => {}
        Some("Type0") => {
            return Err(decline(
                "this is a composite font; adding a glyph means extending its CMap, \
                 which is a different operation",
            ));
        }
        other => {
            return Err(decline(&format!(
                "glyph injection is built for TrueType fonts; this is {}",
                other.unwrap_or("untyped")
            )));
        }
    }

    let (descriptor_id, descriptor) = descriptor_of(doc, &font_dict).ok_or_else(|| {
        decline("the font has no descriptor, so nothing says where its program is")
    })?;
    if descriptor.get("FontFile3").is_some() {
        return Err(decline(
            "this font's program is CFF; charstring injection exists and the \
             dictionary plumbing around it does not",
        ));
    }
    let program_id = descriptor
        .get("FontFile2")
        .and_then(Object::as_reference)
        .ok_or_else(|| decline("this font is not embedded, so there is no program to add to"))?;
    let program = doc.decoded_stream(program_id).map_err(|e| {
        Error::from_layer(Code::FontUnavailable, "the font program will not decode", e)
    })?;

    // Which registered font has each character, and at which glyph.
    let family = font_dict
        .get("BaseFont")
        .and_then(Object::as_name)
        .and_then(|n| n.as_str())
        .map(strip_subset_prefix)
        .unwrap_or_default();

    let mut plan: Vec<(char, u8, usize, u16)> = Vec::new();
    for c in wanted {
        let code = winansi_code(*c).ok_or_else(|| {
            decline(&format!(
                "{c:?} has no WinAnsi code, so there is no code for the page to draw it with"
            ))
        })?;
        let (index, gid) = find_glyph(registry, &family, *c)
            .ok_or_else(|| decline(&format!("no registered font has a glyph for {c:?}")))?;
        plan.push((*c, code, index, gid));
    }

    // Every character has to come from one source font: `inject_truetype` takes
    // one donor, and two donors would mean two injections whose glyph
    // renumbering had to be composed. Declined rather than composed, because a
    // caller supplying one typeface is the case that matters and a caller
    // supplying two can inject twice.
    let source_index = plan[0].2;
    if plan.iter().any(|(_, _, i, _)| *i != source_index) {
        return Err(decline(
            "these characters are spread across more than one registered font; \
             supply them in separate operations",
        ));
    }
    let source = &registry[source_index].bytes;
    let source_font = Sfnt::parse(source)
        .map_err(|e| Error::from_layer(Code::FontUnavailable, "the registered font", e))?;

    let gids: Vec<u16> = plan.iter().map(|(_, _, _, g)| *g).collect();
    let injection = rasura_font::inject_truetype(&program, source, &gids)
        .map_err(|e| Error::from_layer(Code::FontUnavailable, "injection failed", e))?;

    // A TrueType simple font reaches a glyph through its own `cmap`: the
    // /Differences name becomes a character through the Adobe Glyph List, and
    // the character becomes a glyph id there. Without this the glyph is in the
    // file, every dictionary describes it, every structural check passes, and
    // it draws nothing. Only a renderer notices.
    let injected = Sfnt::parse(&injection.bytes)
        .map_err(|e| Error::from_layer(Code::FontUnavailable, "the rebuilt font", e))?;
    let mappings: Vec<(u32, u16)> =
        plan.iter().map(|(c, _, _, gid)| (*c as u32, injection.mapping[gid])).collect();
    let rebuilt = rasura_font::add_mappings(&injection.bytes, &injected, &mappings)
        .map_err(|e| Error::from_layer(Code::FontUnavailable, "the cmap rebuild", e))?;

    // Widths are in glyph space, so the donor's own units have to be scaled.
    // Roboto is 2048 units per em; assuming 1000 sets every advance at half its
    // width, and the injected letter overlaps the next one.
    let per_em = source_font.units_per_em.max(1) as f64;
    let added: Vec<Added> = plan
        .iter()
        .map(|(c, code, _, gid)| Added {
            code: *code,
            // The AGL name, because §9.6.6.4 resolves a TrueType /Differences
            // name through the Adobe Glyph List before the cmap sees it.
            name: rasura_layout::agl::name_of(*c)
                .map(str::to_string)
                .unwrap_or_else(|| format!("uni{:04X}", *c as u32)),
            width: source_font.advance(source, *gid).unwrap_or(0) as f64 * 1000.0 / per_em,
            unicode: c.to_string(),
            bbox: None,
        })
        .collect();

    let update = rasura_font::update_simple_font(doc, &font_dict, &descriptor, &added)
        .map_err(|e| Error::from_layer(Code::FontUnavailable, "updating the font dictionary", e))?;

    // `/ToUnicode` is a stream, so it needs an object of its own. Reuse the
    // one the font already points at when there is one — replacing it keeps the
    // object graph the size it was.
    let to_unicode_id =
        font_dict.get("ToUnicode").and_then(Object::as_reference).unwrap_or_else(|| {
            let id = ObjId::new(*next_object, 0);
            *next_object += 1;
            id
        });

    let mut font_out = update.font.clone();
    font_out.insert(Name::new("ToUnicode"), Object::Reference(to_unicode_id));

    let mut to_unicode = Stream::new(Dictionary::new(), Vec::new());
    to_unicode.set_decoded(update.to_unicode.clone());
    to_unicode.dict.insert(Name::new("Length"), Object::Integer(update.to_unicode.len() as i64));

    let original = doc
        .get(program_id)
        .ok()
        .and_then(|o| o.as_stream().cloned())
        .ok_or_else(|| decline("the font program is not a stream"))?;
    let mut program_out = original;
    program_out.set_decoded(rebuilt.clone());
    program_out.dict.insert(Name::new("Length1"), Object::Integer(rebuilt.len() as i64));

    Ok(Supplied {
        changes: vec![
            (program_id, Object::Stream(program_out)),
            (font_id, Object::Dictionary(font_out)),
            (descriptor_id, Object::Dictionary(update.descriptor)),
            (to_unicode_id, Object::Stream(to_unicode)),
        ],
        characters: plan.iter().map(|(c, _, _, _)| *c).collect(),
    })
}

/// The first registered font that has a glyph for `c`.
///
/// A font bound to this typeface wins over an unbound one, so a caller who
/// supplied the real Minion and a generic fallback gets Minion for Minion's
/// text. Among equals the registration order decides, which is the only rule a
/// caller can predict.
fn find_glyph(registry: &[Registered], family: &str, c: char) -> Option<(usize, u16)> {
    let matches = |r: &Registered| r.match_for.as_deref().is_some_and(|m| m == family);

    for pass in [true, false] {
        for (i, entry) in registry.iter().enumerate() {
            if matches(entry) != pass {
                continue;
            }
            let Ok(font) = Sfnt::parse(&entry.bytes) else { continue };
            let Some(cmap) = rasura_font::Cmap::parse(&entry.bytes, &font) else { continue };
            let Some(table) = cmap.best_unicode() else { continue };
            if let Some(gid) = table.lookup(&entry.bytes, c as u32)
                && gid != 0
            {
                return Some((i, gid));
            }
        }
    }
    None
}

/// The `/Font` resource entry, unresolved, so its object id survives.
fn font_object(doc: &Document, page: &Page, name: &Name) -> Option<(ObjId, Dictionary)> {
    let pages = rasura_content::page::pages(doc).ok()?;
    let raw = pages.pages.get(page.index())?;
    let resources = raw.resources.as_ref()?.as_dict()?;
    let fonts = doc.get_entry(resources, "Font").ok()??;
    let fonts = fonts.as_dict()?;
    let id = fonts.get_name(name)?.as_reference()?;
    let object = doc.get(id).ok()?;
    Some((id, object.as_dict()?.clone()))
}

fn descriptor_of(doc: &Document, font: &Dictionary) -> Option<(ObjId, Dictionary)> {
    let id = font.get("FontDescriptor")?.as_reference()?;
    let object = doc.get(id).ok()?;
    Some((id, object.as_dict()?.clone()))
}

/// ISO 32000-1 §9.6.4: six uppercase letters and a plus sign.
fn strip_subset_prefix(name: &str) -> String {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase()) {
        name[7..].to_string()
    } else {
        name.to_string()
    }
}

/// WinAnsi puts most of Latin-1 where Latin-1 does.
fn winansi_code(c: char) -> Option<u8> {
    u8::try_from(c as u32).ok().filter(|b| *b >= 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subset_prefix_is_stripped_and_a_plus_sign_is_not() {
        assert_eq!(strip_subset_prefix("ABCDEF+MinionPro"), "MinionPro");
        assert_eq!(strip_subset_prefix("Foo+Bar"), "Foo+Bar");
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
    }

    #[test]
    fn a_font_bound_to_the_typeface_wins_over_an_unbound_one() {
        // A caller who supplied the real Minion and a generic fallback expects
        // Minion for Minion's text. Registration order decides only among
        // equals, because it is the one rule a caller can predict.
        let roboto = std::fs::read("../../corpus/fonts/Roboto-Regular.ttf");
        let Ok(roboto) = roboto else {
            eprintln!("skipping: corpus/fonts/Roboto-Regular.ttf absent");
            return;
        };
        let registry = vec![
            Registered { bytes: roboto.clone(), match_for: None },
            Registered { bytes: roboto, match_for: Some("Probe".into()) },
        ];
        let (index, gid) = find_glyph(&registry, "Probe", 'A').expect("Roboto has an A");
        assert_eq!(index, 1, "the bound font was preferred");
        assert!(gid != 0);
    }

    #[test]
    fn a_character_no_registered_font_has_is_not_found() {
        let registry = vec![Registered { bytes: vec![0, 1, 2, 3], match_for: None }];
        assert!(find_glyph(&registry, "Anything", 'A').is_none());
    }

    #[test]
    fn characters_outside_latin1_have_no_winansi_code() {
        assert_eq!(winansi_code('A'), Some(65));
        assert_eq!(winansi_code('é'), Some(233));
        assert_eq!(winansi_code('\u{4e00}'), None);
        // Control codes are not drawable positions.
        assert_eq!(winansi_code('\n'), None);
    }
}
