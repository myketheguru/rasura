//! Recovery mode: rebuild the cross-reference table by scanning the file.
//! Spec 5.3.
//!
//! Triggered when `startxref` is wrong, the table is malformed, or `/Root` is
//! unresolvable. Recovery forces `SaveMode::FullRewrite` -- appending a revision
//! onto a file whose cross-reference table you had to guess is not safe, and the
//! writer enforces that rather than documenting it.

use crate::error::{CosError, Leniency, LeniencyKind, Result};
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, Object, is_whitespace};
use crate::parser::{NoResolve, Parser};
use crate::xref::{XrefEntry, XrefStyle, XrefTable};
use std::collections::HashMap;

pub struct Reconstruction {
    pub table: XrefTable,
    pub leniencies: Vec<Leniency>,
    /// Object numbers whose objects are `/Type /ObjStm`. The document expands
    /// these once it can decrypt, registering the objects they contain.
    pub object_streams: Vec<u32>,
}

/// Scan the whole file for `N G obj` and rebuild.
pub fn reconstruct(buf: &[u8]) -> Result<Reconstruction> {
    let mut leniencies = vec![Leniency::new(
        LeniencyKind::XrefReconstructed,
        0,
        "cross-reference table rebuilt by scanning the file",
    )];

    // (number, generation, offset), in file order.
    let mut found: Vec<(u32, u16, usize)> = Vec::new();
    for at in scan_obj_headers(buf) {
        found.push(at);
    }
    if found.is_empty() {
        return Err(CosError::RecoveryFailed("no 'N G obj' headers found".into()));
    }

    let mut table = XrefTable::default();
    let mut object_streams = Vec::new();

    // Spec 5.3: take the highest-generation instance of each object number. For
    // equal generations the later occurrence wins, since a later revision of the
    // same object supersedes the earlier one.
    let mut best: std::collections::HashMap<u32, (u16, usize)> = std::collections::HashMap::new();
    for (num, generation, offset) in &found {
        match best.get(num) {
            Some(&(g, _)) if g > *generation => {}
            _ => {
                best.insert(*num, (*generation, *offset));
            }
        }
    }

    for (num, (generation, offset)) in best {
        table.insert(num, XrefEntry::InFile { offset, generation });
    }

    // Identify object streams so the caller can expand them, and pick up any
    // trailer-shaped dictionary along the way.
    let mut trailer = Dictionary::new();
    for (num, entry) in table.iter().collect::<Vec<_>>() {
        let XrefEntry::InFile { offset, .. } = entry else { continue };
        let mut parser = Parser::at(buf, offset);
        let Ok(io) = parser.parse_indirect_object(&NoResolve) else { continue };
        let Some(dict) = io.object.as_dict() else { continue };
        match dict.type_name().map(|t| t.as_bytes().to_vec()).as_deref() {
            Some(b"ObjStm") => object_streams.push(num),
            Some(b"XRef") => {
                // Its dictionary doubles as a trailer and is the best source of
                // /Root and /Encrypt in a file whose xref we could not follow.
                merge_trailer(&mut trailer, dict);
                table.style = XrefStyle::Stream;
            }
            _ => {}
        }
    }

    // Explicit `trailer` dictionaries, latest last so they win.
    for offset in scan_keyword(buf, b"trailer") {
        let mut parser = Parser::at(buf, offset + b"trailer".len());
        if let Ok(Object::Dictionary(d)) = parser.parse_object() {
            merge_trailer(&mut trailer, &d);
            if table.style != XrefStyle::Stream {
                table.style = XrefStyle::Classic;
            }
        }
    }

    if trailer.get("Root").is_none() {
        // Spec 5.3: fall back to scanning for /Type /Catalog.
        if let Some(id) = find_catalog(buf, &table) {
            leniencies.push(Leniency::new(
                LeniencyKind::CatalogScanned,
                0,
                format!("no trailer /Root; using {id} found by scanning for /Type /Catalog"),
            ));
            trailer.insert(crate::object::Name::new("Root"), Object::Reference(id));
        } else {
            return Err(CosError::RecoveryFailed(
                "no trailer /Root and no /Type /Catalog object in the file".into(),
            ));
        }
    }

    trailer
        .insert(crate::object::Name::new("Size"), Object::Integer(table.next_free_number() as i64));
    table.trailer = trailer;
    table.reconstructed = true;

    Ok(Reconstruction { table, leniencies, object_streams })
}

/// Keep keys the first (i.e. newest-seen) trailer supplied, but let later
/// trailers fill in what is missing. Callers pass trailers oldest-first.
fn merge_trailer(into: &mut Dictionary, from: &Dictionary) {
    for key in ["Root", "Encrypt", "Info", "ID", "Size"] {
        if let Some(v) = from.get(key) {
            into.insert(crate::object::Name::new(key), v.clone());
        }
    }
}

/// Index every `N G obj` header in the file, keeping the best candidate per
/// object number: highest generation, and among equal generations the one
/// latest in the file, since a later revision supersedes an earlier one.
///
/// Used both by full reconstruction and by the targeted repair a document
/// performs when one cross-reference entry points somewhere wrong. Building the
/// whole index at once costs a single pass, where repairing entries one at a
/// time would rescan the file for each.
pub fn index_object_headers(buf: &[u8]) -> HashMap<u32, (usize, u16)> {
    let mut best = HashMap::new();
    for (num, generation, offset) in scan_obj_headers(buf) {
        match best.get(&num) {
            Some(&(_, g)) if g > generation => {}
            _ => {
                best.insert(num, (offset, generation));
            }
        }
    }
    best
}

/// Every `N G obj` header in the file, in order of appearance.
///
/// Deliberately allocation-light and regex-free (spec 5.3): walk to each `obj`
/// keyword and backtrack over the two integers that must precede it.
fn scan_obj_headers(buf: &[u8]) -> Vec<(u32, u16, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 <= buf.len() {
        if &buf[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        // `obj` must be a whole token.
        if buf.get(i + 3).is_some_and(|&b| crate::object::is_regular(b)) {
            i += 1;
            continue;
        }
        if let Some((num, generation, start)) = backtrack_header(buf, i) {
            out.push((num, generation, start));
        }
        i += 3;
    }
    out
}

/// From the position of `obj`, read backwards over `<ws> G <ws> N`.
fn backtrack_header(buf: &[u8], obj_at: usize) -> Option<(u32, u16, usize)> {
    let mut p = obj_at;
    let skip_ws_back = |buf: &[u8], mut p: usize| {
        while p > 0 && is_whitespace(buf[p - 1]) {
            p -= 1;
        }
        p
    };
    let digits_back = |buf: &[u8], mut p: usize| -> Option<(usize, u64)> {
        let end = p;
        while p > 0 && buf[p - 1].is_ascii_digit() {
            p -= 1;
        }
        if p == end {
            return None;
        }
        // Cap the length so a megabyte of digits cannot be mistaken for a
        // number; PDF object numbers are at most 10 digits.
        if end - p > 10 {
            return None;
        }
        let mut v = 0u64;
        for &d in &buf[p..end] {
            v = v.checked_mul(10)?.checked_add((d - b'0') as u64)?;
        }
        Some((p, v))
    };

    p = skip_ws_back(buf, p);
    if p == obj_at {
        return None; // `obj` must be preceded by whitespace
    }
    let (p, generation) = digits_back(buf, p)?;
    let p = skip_ws_back(buf, p);
    let (start, number) = digits_back(buf, p)?;

    // The header must begin at a token boundary.
    if start > 0 && crate::object::is_regular(buf[start - 1]) {
        return None;
    }
    Some((u32::try_from(number).ok()?, u16::try_from(generation).ok()?, start))
}

fn scan_keyword(buf: &[u8], keyword: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = crate::parser::find_bytes(&buf[from..], keyword) {
        let at = from + rel;
        let before_ok = at == 0 || !crate::object::is_regular(buf[at - 1]);
        let after_ok = buf.get(at + keyword.len()).is_none_or(|&b| !crate::object::is_regular(b));
        if before_ok && after_ok {
            out.push(at);
        }
        from = at + keyword.len();
    }
    out
}

fn find_catalog(buf: &[u8], table: &XrefTable) -> Option<crate::object::ObjId> {
    let mut candidates: Vec<(u32, crate::object::ObjId)> = Vec::new();
    for (num, entry) in table.iter() {
        let XrefEntry::InFile { offset, .. } = entry else { continue };
        let mut parser = Parser::at(buf, offset);
        let Ok(io) = parser.parse_indirect_object(&NoResolve) else { continue };
        let Some(dict) = io.object.as_dict() else { continue };
        if dict.type_name().is_some_and(|t| t.as_bytes() == b"Catalog") {
            candidates.push((num, io.id));
        }
    }
    // The last catalog in the file belongs to the newest revision.
    candidates.sort_by_key(|(n, _)| *n);
    candidates.pop().map(|(_, id)| id)
}

/// Read the object-stream header without needing the whole document machinery.
/// Shared with the document layer's ObjStm expansion.
pub(crate) fn objstm_pairs(data: &[u8], n: usize, first: usize) -> Vec<(u32, usize)> {
    let mut lx = Lexer::new(&data[..first.min(data.len())]);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let (Token::Integer(num), Token::Integer(off)) =
            (lx.next_token().token, lx.next_token().token)
        else {
            break;
        };
        if num < 0 || off < 0 {
            break;
        }
        out.push((num as u32, off as usize));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with_broken_xref() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"%PDF-1.4\n");
        b.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        b.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        b.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");
        // Deliberately nonsense offsets and a startxref into the void.
        b.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        b.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n");
        b
    }

    #[test]
    fn rebuilds_from_object_headers() {
        let buf = file_with_broken_xref();
        let r = reconstruct(&buf).unwrap();
        assert!(r.table.reconstructed);
        assert_eq!(r.table.live_objects().count(), 3);
        assert_eq!(
            r.table.trailer.get("Root").unwrap().as_reference(),
            Some(crate::object::ObjId::new(1, 0))
        );
        assert!(r.leniencies.iter().any(|l| l.kind == LeniencyKind::XrefReconstructed));
    }

    #[test]
    fn finds_the_catalog_when_the_trailer_has_no_root() {
        let mut buf = file_with_broken_xref();
        let s = String::from_utf8_lossy(&buf).replace("/Size 4 /Root 1 0 R", "/Size 4");
        buf = s.into_bytes();
        let r = reconstruct(&buf).unwrap();
        assert_eq!(
            r.table.trailer.get("Root").unwrap().as_reference(),
            Some(crate::object::ObjId::new(1, 0))
        );
        assert!(r.leniencies.iter().any(|l| l.kind == LeniencyKind::CatalogScanned));
    }

    #[test]
    fn later_revision_of_an_object_wins() {
        let mut buf = file_with_broken_xref();
        let second = buf.len();
        buf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Rotate 90 >>\nendobj\n");
        let r = reconstruct(&buf).unwrap();
        match r.table.get(3).unwrap() {
            XrefEntry::InFile { offset, .. } => assert_eq!(offset, second),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn higher_generation_wins_over_position() {
        let mut buf = file_with_broken_xref();
        let high = buf.len();
        buf.extend_from_slice(b"3 1 obj\n<< /Gen 1 >>\nendobj\n");
        buf.extend_from_slice(b"3 0 obj\n<< /Gen 0 >>\nendobj\n");
        let r = reconstruct(&buf).unwrap();
        match r.table.get(3).unwrap() {
            XrefEntry::InFile { offset, generation } => {
                assert_eq!(generation, 1);
                assert_eq!(offset, high);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn does_not_mistake_the_word_object_for_a_header() {
        let buf = b"%PDF-1.4\n1 0 obj\n(the word object appears here)\nendobj\n".to_vec();
        let headers = scan_obj_headers(&buf);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, 1);
    }

    #[test]
    fn fails_honestly_on_a_file_with_no_objects() {
        assert!(reconstruct(b"%PDF-1.4\nnothing here at all\n%%EOF").is_err());
    }
}
