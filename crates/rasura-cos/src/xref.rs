//! Cross-reference resolution. ISO 32000-1 §7.5, spec 5.3.
//!
//! All four forms and their combinations:
//!
//! 1. classic `xref` / `trailer` tables with `/Prev` chains,
//! 2. cross-reference streams (`/Type /XRef`),
//! 3. object streams (`/Type /ObjStm`) referenced by type-2 entries,
//! 4. hybrid-reference files, where a classic trailer carries `/XRefStm`.
//!
//! The revision chain is retained rather than flattened away. `revisions()`
//! exposes it, and the redaction path needs it to know that earlier bytes are
//! still sitting in the file.

use crate::error::{CosError, Leniency, LeniencyKind, Result};
use crate::filters::{self, FilterChain};
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, Object};
use crate::parser::{NoResolve, Parser, rfind_bytes};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// Type 0. Retained because a free-list generation matters when an object
    /// number is reused.
    Free { next_free: u32, generation: u16 },
    /// Type 1: a byte offset into the file.
    InFile { offset: usize, generation: u16 },
    /// Type 2: inside an object stream, at the given index.
    InObjStm { container: u32, index: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XrefStyle {
    /// `xref` keyword and a `trailer` dictionary.
    #[default]
    Classic,
    /// A `/Type /XRef` stream.
    Stream,
}

/// One revision of the file, newest first in `XrefTable::revisions`.
#[derive(Debug, Clone)]
pub struct RevisionInfo {
    /// Offset of this revision's cross-reference section.
    pub xref_offset: usize,
    pub style: XrefStyle,
    /// True when a classic table in this revision carried `/XRefStm`.
    pub hybrid: bool,
    pub trailer: Dictionary,
    /// How many entries this revision defined.
    pub entry_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct XrefTable {
    entries: BTreeMap<u32, XrefEntry>,
    /// Trailer of the newest revision, which is the one that counts for
    /// `/Root`, `/Encrypt`, `/Info` and `/ID`.
    pub trailer: Dictionary,
    /// Newest first.
    pub revisions: Vec<RevisionInfo>,
    /// The style to reproduce on save. Spec 5.6: do not "upgrade" the format.
    pub style: XrefStyle,
    /// True when the table had to be rebuilt by scanning (spec 5.3). Forces
    /// `SaveMode::FullRewrite`.
    pub reconstructed: bool,
}

impl XrefTable {
    pub fn get(&self, number: u32) -> Option<XrefEntry> {
        self.entries.get(&number).copied()
    }

    pub fn insert(&mut self, number: u32, entry: XrefEntry) {
        self.entries.insert(number, entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, XrefEntry)> + '_ {
        self.entries.iter().map(|(k, v)| (*k, *v))
    }

    /// Object numbers that resolve to something loadable.
    pub fn live_objects(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries.iter().filter(|(_, e)| !matches!(e, XrefEntry::Free { .. })).map(|(k, _)| *k)
    }

    /// Highest object number plus one, i.e. what `/Size` should say.
    pub fn next_free_number(&self) -> u32 {
        self.entries.keys().next_back().map_or(1, |n| n + 1)
    }

    pub fn trailer_size(&self) -> u32 {
        self.trailer.get("Size").and_then(Object::as_i64).unwrap_or(0).max(0) as u32
    }
}

/// Where the file header sits and where `startxref` points.
pub struct XrefLoad {
    pub table: XrefTable,
    pub leniencies: Vec<Leniency>,
}

/// Build the cross-reference table by walking the `/Prev` chain from
/// `startxref`.
///
/// Returns `Err` when the chain cannot be followed at all; the caller falls back
/// to `recovery::reconstruct`.
pub fn load(buf: &[u8], header_offset: usize) -> Result<XrefLoad> {
    let mut leniencies = Vec::new();
    let start = find_startxref(buf, &mut leniencies)?;

    let mut table = XrefTable::default();
    let mut visited: HashSet<usize> = HashSet::new();
    let mut next = Some(start);
    let mut first = true;

    while let Some(raw_offset) = next {
        // Offsets are relative to the header, which is not always at byte 0.
        let offset = resolve_offset(buf, raw_offset, header_offset);
        if !visited.insert(offset) {
            leniencies.push(Leniency::new(
                LeniencyKind::MalformedXrefSection,
                offset,
                "/Prev chain loops; stopping",
            ));
            break;
        }
        if offset >= buf.len() {
            leniencies.push(Leniency::new(
                LeniencyKind::BadStartxref,
                offset,
                "cross-reference offset is past end of file",
            ));
            break;
        }

        let section = parse_section(buf, offset, header_offset, &mut leniencies)?;
        if first {
            table.trailer = section.trailer.clone();
            table.style = section.style;
            first = false;
        }

        // Newest revision wins: only fill entries not already defined.
        let mut count = 0usize;
        for (num, entry) in section.entries {
            table.entries.entry(num).or_insert_with(|| {
                count += 1;
                entry
            });
        }

        table.revisions.push(RevisionInfo {
            xref_offset: offset,
            style: section.style,
            hybrid: section.hybrid,
            trailer: section.trailer.clone(),
            entry_count: count,
        });

        next = section
            .trailer
            .get("Prev")
            .and_then(Object::as_i64)
            .and_then(|v| if v >= 0 { Some(v as usize) } else { None });
    }

    if table.entries.is_empty() {
        return Err(CosError::RecoveryFailed("cross-reference chain yielded no entries".into()));
    }
    Ok(XrefLoad { table, leniencies })
}

/// One cross-reference section: a classic table plus its trailer, or one xref
/// stream, plus whatever a `/XRefStm` contributed.
struct Section {
    entries: Vec<(u32, XrefEntry)>,
    trailer: Dictionary,
    style: XrefStyle,
    hybrid: bool,
}

fn parse_section(
    buf: &[u8],
    offset: usize,
    header_offset: usize,
    leniencies: &mut Vec<Leniency>,
) -> Result<Section> {
    let mut lx = Lexer::at(buf, offset);
    let peek = lx.peek_token();

    if matches!(&peek.token, Token::Keyword(kw) if &**kw == b"xref") {
        let mut section = parse_classic(buf, offset, leniencies)?;

        // Hybrid: /XRefStm supplies entries the classic table marks free.
        if let Some(stm_off) = section.trailer.get("XRefStm").and_then(Object::as_i64)
            && stm_off >= 0
        {
            let stm_off = resolve_offset(buf, stm_off as usize, header_offset);
            match parse_xref_stream(buf, stm_off, leniencies) {
                Ok(stream_section) => {
                    section.hybrid = true;
                    let defined: std::collections::HashMap<u32, XrefEntry> =
                        section.entries.iter().copied().collect();
                    for (num, entry) in stream_section.entries {
                        match defined.get(&num) {
                            // The classic table in a hybrid file deliberately
                            // marks compressed objects free; the stream is the
                            // real source for those.
                            Some(XrefEntry::Free { .. }) | None => {
                                section.entries.push((num, entry));
                            }
                            Some(_) => {}
                        }
                    }
                }
                Err(e) => leniencies.push(Leniency::new(
                    LeniencyKind::MalformedXrefSection,
                    stm_off,
                    format!("/XRefStm at {stm_off} unreadable: {e}"),
                )),
            }
        }
        return Ok(section);
    }

    // Otherwise it must be a cross-reference stream.
    parse_xref_stream(buf, offset, leniencies)
}

/// `xref` \n subsections \n `trailer` \n dict
fn parse_classic(buf: &[u8], offset: usize, leniencies: &mut Vec<Leniency>) -> Result<Section> {
    let mut lx = Lexer::at(buf, offset);
    lx.next_token(); // `xref`

    let mut entries = Vec::new();
    loop {
        let save = lx.pos();
        let t = lx.next_token();
        match t.token {
            Token::Keyword(ref kw) if &**kw == b"trailer" => break,
            Token::Integer(first_num) => {
                let count = match lx.next_token().token {
                    Token::Integer(c) if c >= 0 => c as usize,
                    _ => {
                        leniencies.push(Leniency::new(
                            LeniencyKind::MalformedXrefSection,
                            save,
                            "subsection header without a count",
                        ));
                        break;
                    }
                };
                if first_num < 0 {
                    leniencies.push(Leniency::new(
                        LeniencyKind::MalformedXrefSection,
                        save,
                        "negative first object number",
                    ));
                    break;
                }
                read_classic_subsection(&mut lx, first_num as u32, count, &mut entries, leniencies);
            }
            Token::Eof => {
                return Err(CosError::malformed(save, "xref section ran off the end of the file"));
            }
            _ => {
                leniencies.push(Leniency::new(
                    LeniencyKind::MalformedXrefSection,
                    save,
                    "unexpected token in xref table",
                ));
                break;
            }
        }
    }

    // Parse the trailer dictionary that follows.
    let mut parser = Parser::at(buf, lx.pos());
    let trailer = match parser.parse_object() {
        Ok(Object::Dictionary(d)) => d,
        _ => {
            leniencies.push(Leniency::new(
                LeniencyKind::MalformedXrefSection,
                lx.pos(),
                "trailer is not a dictionary",
            ));
            Dictionary::new()
        }
    };
    leniencies.extend(parser.take_leniencies());
    leniencies.extend(lx.take_leniencies());

    Ok(Section { entries, trailer, style: XrefStyle::Classic, hybrid: false })
}

fn read_classic_subsection(
    lx: &mut Lexer<'_>,
    first_num: u32,
    count: usize,
    entries: &mut Vec<(u32, XrefEntry)>,
    leniencies: &mut Vec<Leniency>,
) {
    // Entries are nominally exactly 20 bytes, but 19-byte entries (a lone EOL
    // instead of two characters) are everywhere. Tokenising rather than slicing
    // fixed widths handles both without a special case.
    for i in 0..count {
        let save = lx.pos();
        let offset = match lx.next_token().token {
            Token::Integer(v) if v >= 0 => v as usize,
            _ => {
                leniencies.push(Leniency::new(
                    LeniencyKind::MalformedXrefSection,
                    save,
                    format!("entry {i} of subsection starting at {first_num} has no offset"),
                ));
                lx.seek(save);
                return;
            }
        };
        let generation = match lx.next_token().token {
            Token::Integer(v) if (0..=u16::MAX as i64).contains(&v) => v as u16,
            _ => {
                lx.seek(save);
                return;
            }
        };
        let kind = lx.next_token();
        let num = first_num.saturating_add(i as u32);
        match &kind.token {
            Token::Keyword(kw) if &**kw == b"n" => {
                entries.push((num, XrefEntry::InFile { offset, generation }));
            }
            Token::Keyword(kw) if &**kw == b"f" => {
                entries.push((
                    num,
                    XrefEntry::Free { next_free: offset.min(u32::MAX as usize) as u32, generation },
                ));
            }
            _ => {
                leniencies.push(Leniency::new(
                    LeniencyKind::MalformedXrefSection,
                    kind.span.start,
                    "xref entry is neither 'n' nor 'f'",
                ));
                lx.seek(save);
                return;
            }
        }
    }
}

fn parse_xref_stream(buf: &[u8], offset: usize, leniencies: &mut Vec<Leniency>) -> Result<Section> {
    let mut parser = Parser::at(buf, offset);
    let io = parser.parse_indirect_object(&NoResolve)?;
    leniencies.extend(parser.take_leniencies());

    let Object::Stream(stream) = &io.object else {
        return Err(CosError::malformed(
            offset,
            "cross-reference offset does not point at a stream",
        ));
    };
    let dict = &stream.dict;
    if dict.type_name().is_some_and(|t| t.as_bytes() != b"XRef") {
        return Err(CosError::malformed(offset, "stream at the xref offset is not /Type /XRef"));
    }

    // Cross-reference streams are never encrypted, so decoding needs no key.
    let chain = FilterChain::build(dict.get("Filter"), dict.get("DecodeParms"));
    let data = filters::decode(&chain, stream.raw())?.data;

    let widths: Vec<usize> = dict
        .get("W")
        .and_then(Object::as_array)
        .ok_or_else(|| CosError::malformed(offset, "xref stream has no /W"))?
        .iter()
        .map(|o| o.as_i64().unwrap_or(0).clamp(0, 8) as usize)
        .collect();
    if widths.len() < 3 {
        return Err(CosError::malformed(offset, "/W must have at least three entries"));
    }
    let row = widths.iter().sum::<usize>();
    if row == 0 {
        return Err(CosError::malformed(offset, "/W sums to zero"));
    }

    let size = dict.get("Size").and_then(Object::as_i64).unwrap_or(0).max(0) as u32;
    let index: Vec<i64> = match dict.get("Index").and_then(Object::as_array) {
        Some(a) => a.iter().filter_map(Object::as_i64).collect(),
        None => vec![0, size as i64],
    };

    let mut entries = Vec::new();
    let mut cursor = 0usize;
    for pair in index.chunks(2) {
        let (&first, &count) = match pair {
            [f, c] => (f, c),
            _ => break,
        };
        if first < 0 || count < 0 {
            continue;
        }
        for i in 0..count as usize {
            if cursor + row > data.len() {
                leniencies.push(Leniency::new(
                    LeniencyKind::MalformedXrefSection,
                    offset,
                    "xref stream data is shorter than /Index implies",
                ));
                break;
            }
            let mut fields = [0u64; 3];
            let mut p = cursor;
            for (f, &w) in fields.iter_mut().zip(widths.iter()) {
                let mut v = 0u64;
                for _ in 0..w {
                    v = v << 8 | data[p] as u64;
                    p += 1;
                }
                *f = v;
            }
            cursor += row;

            // ISO 32000-1 §7.5.8.2: a /W[0] of 0 means type 1.
            let kind = if widths[0] == 0 { 1 } else { fields[0] };
            let num = (first as u64 + i as u64).min(u32::MAX as u64) as u32;
            let entry = match kind {
                0 => XrefEntry::Free {
                    next_free: fields[1].min(u32::MAX as u64) as u32,
                    generation: fields[2].min(u16::MAX as u64) as u16,
                },
                1 => XrefEntry::InFile {
                    offset: fields[1] as usize,
                    generation: fields[2].min(u16::MAX as u64) as u16,
                },
                2 => XrefEntry::InObjStm {
                    container: fields[1].min(u32::MAX as u64) as u32,
                    index: fields[2].min(u32::MAX as u64) as u32,
                },
                // Types beyond 2 are reserved; treat as free rather than guess.
                _ => XrefEntry::Free { next_free: 0, generation: 0 },
            };
            entries.push((num, entry));
        }
    }

    Ok(Section {
        entries,
        // An xref stream's own dictionary *is* the trailer.
        trailer: dict.clone(),
        style: XrefStyle::Stream,
        hybrid: false,
    })
}

/// Objects packed inside a `/Type /ObjStm`. ISO 32000-1 §7.5.7.
pub struct ObjStmContents {
    /// `(object number, offset of its start within the decoded stream)`.
    pub offsets: Vec<(u32, usize)>,
    pub data: Vec<u8>,
    pub first: usize,
}

/// Parse the header of an object stream's decoded contents.
pub fn parse_objstm(dict: &Dictionary, data: Vec<u8>) -> Result<ObjStmContents> {
    let n = dict.get("N").and_then(Object::as_i64).unwrap_or(0).max(0) as usize;
    let first = dict.get("First").and_then(Object::as_usize).unwrap_or(0);

    // One implementation, rather than the two that had drifted apart here: the
    // recovery path had the same header parser, and the unbounded reservation
    // that went with it, so a fix applied to one of them would have left the
    // other panicking.
    let offsets = crate::recovery::objstm_pairs(&data, n, first);

    Ok(ObjStmContents { offsets, data, first })
}

/// Find the byte offset the trailing `startxref` points at.
fn find_startxref(buf: &[u8], leniencies: &mut Vec<Leniency>) -> Result<usize> {
    // The keyword must be within the last 1024 bytes per spec, but files with
    // trailing garbage are common; widen the window before giving up.
    for window in [1024usize, 4096, buf.len()] {
        let from = buf.len().saturating_sub(window);
        if let Some(rel) = rfind_bytes(&buf[from..], b"startxref") {
            let at = from + rel;
            let mut lx = Lexer::at(buf, at + b"startxref".len());
            if let Token::Integer(v) = lx.next_token().token
                && v >= 0
            {
                if window > 1024 {
                    leniencies.push(Leniency::new(
                        LeniencyKind::BadStartxref,
                        at,
                        "startxref is further than 1024 bytes from end of file",
                    ));
                }
                return Ok(v as usize);
            }
        }
        if window >= buf.len() {
            break;
        }
    }
    Err(CosError::RecoveryFailed("no usable startxref".into()))
}

/// Offsets in a PDF are measured from the `%PDF-` header, which is not always at
/// byte 0. When the raw offset does not land on an object header, retry with the
/// header shift applied.
fn resolve_offset(buf: &[u8], raw: usize, header_offset: usize) -> usize {
    if header_offset == 0 {
        return raw;
    }
    if looks_like_xref_start(buf, raw) {
        raw
    } else if looks_like_xref_start(buf, raw + header_offset) {
        raw + header_offset
    } else {
        raw
    }
}

/// Whether a cross-reference section plausibly starts at `at`.
///
/// `N G` alone is far too weak a signal: the body of a classic xref table is
/// nothing but pairs of integers, so a wrong offset landing inside one would be
/// accepted. The `obj` keyword has to be there.
fn looks_like_xref_start(buf: &[u8], at: usize) -> bool {
    if at >= buf.len() {
        return false;
    }
    let mut lx = Lexer::at(buf, at);
    match lx.next_token().token {
        Token::Keyword(kw) => &*kw == b"xref",
        // A cross-reference stream begins `N G obj`.
        Token::Integer(_) => {
            matches!(lx.next_token().token, Token::Integer(_))
                && matches!(lx.next_token().token, Token::Keyword(kw) if &*kw == b"obj")
        }
        _ => false,
    }
}

/// Locate `%PDF-` and report how far into the file it sits.
///
/// ISO 32000-1 requires the header at byte zero, and Acrobat's own leniency
/// note allows it within the first 1024. In practice files arrive with HTTP
/// preambles, shell wrappers, mail headers, and byte-order marks in front of
/// it, so the whole buffer is searched: a file whose header is at byte 5000 is
/// still a file every viewer opens.
pub fn find_header(buf: &[u8]) -> Option<usize> {
    crate::parser::find_bytes(buf, b"%PDF-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjId;

    /// A minimal but genuinely valid classic-xref file.
    fn classic_file() -> Vec<u8> {
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");
        for (n, content) in [
            (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ] {
            offsets.push((n, body.len()));
            body.extend_from_slice(format!("{n} 0 obj\n{content}\nendobj\n").as_bytes());
        }
        let xref_at = body.len();
        body.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for (_, off) in &offsets {
            body.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        body.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );
        body
    }

    #[test]
    fn loads_a_classic_table() {
        let buf = classic_file();
        let loaded = load(&buf, 0).unwrap();
        assert_eq!(loaded.table.style, XrefStyle::Classic);
        assert_eq!(loaded.table.len(), 4);
        assert!(matches!(loaded.table.get(0), Some(XrefEntry::Free { .. })));
        assert!(matches!(loaded.table.get(2), Some(XrefEntry::InFile { .. })));
        assert_eq!(
            loaded.table.trailer.get("Root").unwrap().as_reference(),
            Some(ObjId::new(1, 0))
        );
        assert_eq!(loaded.table.revisions.len(), 1);
    }

    #[test]
    fn newest_revision_wins_over_prev() {
        let base = classic_file();
        let base_xref = crate::parser::rfind_bytes(&base, b"xref\n0 4").unwrap();

        // Append a revision redefining object 3.
        let mut buf = base.clone();
        let new_obj_at = buf.len();
        buf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Rotate 90 >>\nendobj\n");
        let new_xref_at = buf.len();
        buf.extend_from_slice(
            format!(
                "xref\n3 1\n{new_obj_at:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {base_xref} >>\nstartxref\n{new_xref_at}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let loaded = load(&buf, 0).unwrap();
        assert_eq!(loaded.table.revisions.len(), 2, "the /Prev chain must be retained");
        match loaded.table.get(3).unwrap() {
            XrefEntry::InFile { offset, .. } => assert_eq!(offset, new_obj_at),
            other => panic!("{other:?}"),
        }
        // The older revision still supplies objects it alone defined.
        assert!(matches!(loaded.table.get(1), Some(XrefEntry::InFile { .. })));
    }

    #[test]
    fn tolerates_nineteen_byte_entries() {
        // Entries are nominally 20 bytes. Producers that write 19 (one EOL
        // character instead of two) are common enough that refusing them would
        // reject a meaningful slice of the world's PDFs.
        let original = classic_file();
        let shortened = String::from_utf8_lossy(&original).replace(" n \n", " n\n").into_bytes();
        assert_ne!(shortened, original, "the fixture must actually have changed");

        // The table moved, so startxref has to follow it. It did not move here
        // because only bytes after the table changed -- assert that holds.
        let at = crate::parser::rfind_bytes(&shortened, b"\nxref\n").unwrap() + 1;
        assert_eq!(at, crate::parser::rfind_bytes(&original, b"\nxref\n").unwrap() + 1);

        let loaded = load(&shortened, 0).unwrap();
        assert_eq!(loaded.table.live_objects().count(), 3);
        assert!(matches!(loaded.table.get(3), Some(XrefEntry::InFile { .. })));
    }

    #[test]
    fn stops_on_a_prev_loop() {
        let mut buf = classic_file();
        let at = crate::parser::rfind_bytes(&buf, b"xref\n0 4").unwrap();
        // Point /Prev at this very section.
        let tail = String::from_utf8_lossy(&buf[at..])
            .replace("/Root 1 0 R", &format!("/Root 1 0 R /Prev {at}"));
        buf.truncate(at);
        buf.extend_from_slice(tail.as_bytes());
        let loaded = load(&buf, 0).unwrap();
        assert_eq!(loaded.table.revisions.len(), 1);
        assert!(loaded.leniencies.iter().any(|l| l.kind == LeniencyKind::MalformedXrefSection));
    }

    #[test]
    fn finds_the_header() {
        assert_eq!(find_header(b"%PDF-1.7\n..."), Some(0));
        assert_eq!(find_header(b"junk\n%PDF-1.7\n"), Some(5));
        assert_eq!(find_header(b"no header here"), None);
    }
}
