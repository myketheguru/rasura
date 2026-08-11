//! Locating and identifying the embedded font program. Spec 8.2.
//!
//! Before anything can be parsed, the right bytes have to be found and their
//! flavour established. That is less obvious than the spec table suggests:
//! `/FontFile3` covers three different formats distinguished by `/Subtype`,
//! OpenType wraps *either* outline flavour, and a depressing number of real
//! files declare one thing and embed another.
//!
//! So the declared flavour is recorded and the actual one is **sniffed from the
//! bytes**, and where they disagree the bytes win. A file that says
//! `/FontFile2` and contains a CFF is not hypothetical; refusing it would lose
//! a font that every viewer renders.

use rasura_cos::{Dictionary, Document, Object};

/// What kind of font program the bytes are.
///
/// Named for the **container and outline format**, not for the PDF key it
/// arrived under. `/FontFile2` and `/FontFile3 /OpenType` can deliver byte-
/// identical sfnt-with-`glyf` programs, so separate `TrueType` and
/// `OpenTypeGlyf` variants would describe the packaging rather than the font —
/// and comparing them would report half the corpus as mislabelled, which is
/// exactly what a first version of this enum did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Type 1, PFB or bare PFA. eexec-encrypted charstrings.
    Type1,
    /// An sfnt with `glyf` outlines: what `/FontFile2` usually holds, and what
    /// `/FontFile3 /OpenType` sometimes does.
    Glyf,
    /// Bare name-keyed CFF, `/Subtype /Type1C`.
    Cff,
    /// Bare CID-keyed CFF, `/Subtype /CIDFontType0C`.
    CidCff,
    /// An sfnt wrapping a `CFF ` table.
    SfntCff,
}

impl Flavour {
    /// Whether the program is an sfnt container, which decides how it is taken
    /// apart and rebuilt.
    pub fn is_sfnt(self) -> bool {
        matches!(self, Flavour::Glyf | Flavour::SfntCff)
    }

    /// Whether the outlines are Type 2 charstrings in a CFF.
    pub fn is_cff(self) -> bool {
        matches!(self, Flavour::Cff | Flavour::CidCff | Flavour::SfntCff)
    }

    pub fn name(self) -> &'static str {
        match self {
            Flavour::Type1 => "Type1",
            Flavour::Glyf => "sfnt/glyf",
            Flavour::Cff => "CFF",
            Flavour::CidCff => "CFF/CID",
            Flavour::SfntCff => "sfnt/CFF",
        }
    }
}

/// Which `/FontFile*` key the program came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    FontFile,
    FontFile2,
    FontFile3,
}

impl Slot {
    pub fn key(self) -> &'static str {
        match self {
            Slot::FontFile => "FontFile",
            Slot::FontFile2 => "FontFile2",
            Slot::FontFile3 => "FontFile3",
        }
    }
}

/// An embedded font program, located and identified.
#[derive(Debug, Clone)]
pub struct Program {
    pub bytes: Vec<u8>,
    pub slot: Slot,
    /// What the file said, where the declaration pins it down at all.
    ///
    /// `None` for `/FontFile3 /Subtype /OpenType`, which spec 8.2 says may hold
    /// "either outline flavour" — the declaration is honest and simply does not
    /// say, so treating it as a specific claim would manufacture disagreements.
    pub declared: Option<Flavour>,
    /// What the bytes actually are. Where these differ the bytes win.
    pub flavour: Flavour,
}

impl Program {
    /// Whether the bytes contradict a declaration that did pin the format down.
    pub fn is_mislabelled(&self) -> bool {
        self.declared.is_some_and(|d| d != self.flavour)
    }
}

/// Find and identify the font program of a font descriptor.
///
/// Takes the *descriptor*, not the font dictionary: for a composite font the
/// descriptor lives on the descendant, and the caller has already had to walk
/// there.
pub fn from_descriptor(doc: &Document, descriptor: &Dictionary) -> Option<Program> {
    for slot in [Slot::FontFile2, Slot::FontFile3, Slot::FontFile] {
        // The entry must be a reference: a font program is always an indirect
        // stream object, and `decoded_stream` is keyed by object id so the
        // decoded bytes are cached across the many glyphs that need them.
        let Some(id) = descriptor.get(slot.key()).and_then(Object::as_reference) else { continue };
        let Ok(obj) = doc.get(id) else { continue };
        let Some(stream) = obj.as_stream() else { continue };
        let Ok(bytes) = doc.decoded_stream(id) else { continue };
        if bytes.is_empty() {
            continue;
        }

        let declared = declared_flavour(slot, &stream.dict);
        // The bytes win. A file declaring one format and embedding another is
        // not hypothetical, and refusing it would lose a font every viewer
        // renders. The declaration is the fallback only when nothing in the
        // bytes is recognisable.
        let flavour = sniff(&bytes).or(declared).unwrap_or(Flavour::Type1);
        return Some(Program { bytes: bytes.to_vec(), slot, declared, flavour });
    }
    None
}

fn declared_flavour(slot: Slot, dict: &Dictionary) -> Option<Flavour> {
    match slot {
        Slot::FontFile => Some(Flavour::Type1),
        Slot::FontFile2 => Some(Flavour::Glyf),
        Slot::FontFile3 => {
            match dict.get("Subtype").and_then(Object::as_name).and_then(|n| n.as_str()) {
                Some("Type1C") => Some(Flavour::Cff),
                Some("CIDFontType0C") => Some(Flavour::CidCff),
                // Spec 8.2: OpenType is "either outline flavour". The file is not
                // claiming which, so nothing here should either.
                Some("OpenType") => None,
                _ => None,
            }
        }
    }
}

/// Identify a font program from its first bytes.
///
/// Returns `None` when nothing recognisable is there, which leaves the caller
/// with the declared flavour -- a guess, but the file's own guess rather than
/// this function's.
pub fn sniff(bytes: &[u8]) -> Option<Flavour> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[..4] {
        // An sfnt: 0x00010000 for TrueType outlines, "OTTO" for CFF, "true"
        // and "ttcf" from the Apple lineage.
        b"OTTO" => Some(Flavour::SfntCff),
        [0x00, 0x01, 0x00, 0x00] | b"true" | b"ttcf" => Some(sfnt_outline_flavour(bytes)),
        // A bare CFF starts with its header: major 1, minor 0, hdrSize, offSize.
        [1, 0, hdr, off] if *hdr >= 4 && (1..=4).contains(off) => Some(cff_flavour(bytes)),
        // Type 1 in PFB segments, or a bare PFA.
        [0x80, 1, ..] => Some(Flavour::Type1),
        _ if bytes.starts_with(b"%!PS-AdobeFont") || bytes.starts_with(b"%!FontType1") => {
            Some(Flavour::Type1)
        }
        _ => None,
    }
}

/// Whether an sfnt carries `glyf` or `CFF ` outlines.
fn sfnt_outline_flavour(bytes: &[u8]) -> Flavour {
    if find_table(bytes, b"CFF ").is_some() { Flavour::SfntCff } else { Flavour::Glyf }
}

/// Whether a bare CFF is CID-keyed, which decides how glyphs are addressed.
fn cff_flavour(bytes: &[u8]) -> Flavour {
    match crate::cff::Cff::parse(bytes) {
        Ok(cff) if cff.is_cid => Flavour::CidCff,
        _ => Flavour::Cff,
    }
}

/// Locate a table in an sfnt by tag, returning its byte range.
///
/// Kept here rather than in `sfnt` because sniffing needs it before a full
/// parse, and a full parse is exactly what a hostile file wants to provoke.
pub fn find_table(bytes: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    if bytes.len() < 12 {
        return None;
    }
    let num_tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    // 16 bytes per record after the 12-byte header. A count that cannot fit is
    // a malformed or hostile file.
    if 12 + num_tables * 16 > bytes.len() {
        return None;
    }
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if &bytes[rec..rec + 4] == tag {
            let offset = u32::from_be_bytes(bytes[rec + 8..rec + 12].try_into().ok()?) as usize;
            let length = u32::from_be_bytes(bytes[rec + 12..rec + 16].try_into().ok()?) as usize;
            let end = offset.checked_add(length)?;
            if end > bytes.len() {
                // A table claiming to run past the end. Clamp rather than
                // reject: truncated fonts are common and mostly still usable.
                return Some((offset.min(bytes.len()), bytes.len()));
            }
            return Some((offset, end));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal sfnt with the given table tags, each holding four bytes.
    fn sfnt(magic: &[u8; 4], tags: &[&[u8; 4]]) -> Vec<u8> {
        let n = tags.len();
        let mut out = Vec::new();
        out.extend_from_slice(magic);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
        let body = 12 + n * 16;
        for (i, tag) in tags.iter().enumerate() {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]); // checksum
            out.extend_from_slice(&((body + i * 4) as u32).to_be_bytes());
            out.extend_from_slice(&4u32.to_be_bytes());
        }
        for _ in tags {
            out.extend_from_slice(b"data");
        }
        out
    }

    #[test]
    fn a_truetype_sfnt_is_recognised() {
        let f = sniff(&sfnt(&[0x00, 0x01, 0x00, 0x00], &[b"glyf", b"loca"])).unwrap();
        assert_eq!(f, Flavour::Glyf);
        assert!(f.is_sfnt() && !f.is_cff());
    }

    #[test]
    fn an_otto_sfnt_is_recognised_as_cff_outlines() {
        let f = sniff(&sfnt(b"OTTO", &[b"CFF ", b"cmap"])).unwrap();
        assert_eq!(f, Flavour::SfntCff);
        assert!(f.is_sfnt() && f.is_cff());
    }

    #[test]
    fn a_version_one_sfnt_carrying_cff_is_still_cff() {
        // Real files do this. The magic says TrueType outlines; the table
        // directory says otherwise, and the tables are the truth.
        let f = sniff(&sfnt(&[0x00, 0x01, 0x00, 0x00], &[b"CFF ", b"cmap"])).unwrap();
        assert_eq!(f, Flavour::SfntCff);
    }

    #[test]
    fn apples_true_magic_is_accepted() {
        assert!(sniff(&sfnt(b"true", &[b"glyf"])).is_some());
    }

    #[test]
    fn a_type1_pfb_is_recognised() {
        assert_eq!(sniff(&[0x80, 0x01, 0x10, 0x00]), Some(Flavour::Type1));
    }

    #[test]
    fn a_bare_pfa_is_recognised() {
        assert_eq!(sniff(b"%!PS-AdobeFont-1.0: Foo 001.000"), Some(Flavour::Type1));
        assert_eq!(sniff(b"%!FontType1-1.0: Foo"), Some(Flavour::Type1));
    }

    #[test]
    fn noise_is_not_a_font() {
        assert!(sniff(b"").is_none());
        assert!(sniff(b"ab").is_none());
        assert!(sniff(b"not a font at all").is_none());
        // A plausible-looking CFF header with an impossible offSize.
        assert!(sniff(&[1, 0, 4, 9]).is_none());
    }

    #[test]
    fn a_table_is_found_by_tag() {
        let bytes = sfnt(&[0x00, 0x01, 0x00, 0x00], &[b"head", b"glyf", b"loca"]);
        let (start, end) = find_table(&bytes, b"glyf").expect("glyf");
        assert_eq!(&bytes[start..end], b"data");
        assert!(find_table(&bytes, b"CFF ").is_none());
    }

    #[test]
    fn a_table_running_past_the_end_is_clamped_not_rejected() {
        // Truncated fonts are common and mostly still usable, so a table whose
        // declared length overruns is clamped rather than refused.
        let mut bytes = sfnt(&[0x00, 0x01, 0x00, 0x00], &[b"glyf"]);
        let rec = 12;
        bytes[rec + 12..rec + 16].copy_from_slice(&0xFFFFu32.to_be_bytes());
        let (start, end) = find_table(&bytes, b"glyf").expect("clamped");
        assert!(end <= bytes.len() && start <= end);
    }

    #[test]
    fn an_impossible_table_count_is_rejected() {
        // 12-byte header claiming 65535 tables in 20 bytes.
        let mut bytes = sfnt(&[0x00, 0x01, 0x00, 0x00], &[b"glyf"]);
        bytes[4..6].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert!(find_table(&bytes, b"glyf").is_none());
    }

    #[test]
    fn the_slot_keys_are_the_spec_names() {
        assert_eq!(Slot::FontFile.key(), "FontFile");
        assert_eq!(Slot::FontFile2.key(), "FontFile2");
        assert_eq!(Slot::FontFile3.key(), "FontFile3");
    }
}
