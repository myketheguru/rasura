//! Rebuilding a CFF with glyphs appended. Spec 8.4's CFF path, steps 2 and 3.
//!
//! > Append to the CharStrings INDEX, extend the charset.
//!
//! A CFF is a web of offsets into itself, and appending anything moves most of
//! them. The awkward part is that the offsets live in the Top DICT, whose own
//! encoded length depends on how large those offsets are — so writing the DICT
//! changes the offsets it contains. The way out is to encode every offset in
//! the fixed five-byte form (`29` plus four bytes) whether or not the value
//! needs it: the DICT then has a length known before the values are, and one
//! pass suffices.
//!
//! Glyph names travel as **SIDs**. Standard SIDs — below 391 — mean the same
//! string in every CFF, so they are copied unchanged; anything above is a name
//! private to the source font and its string is copied into the target's String
//! INDEX with a fresh SID. That avoids needing the 391-entry standard strings
//! table to move a glyph between fonts.

use crate::cff::{Cff, Index};
use crate::charstring::inline_subrs;
use crate::error::{FontError, Result};
use std::collections::HashMap;

/// The first SID that is not one of the CFF standard strings.
const STANDARD_STRINGS: u16 = 391;

/// Inject glyphs from one CFF into another. Spec 8.4's CFF path.
///
/// The target's existing glyph ids are unchanged: new glyphs are appended, per
/// spec 8.6's sparse-preserving rule.
pub fn inject_cff(target: &[u8], source: &[u8], glyphs: &[u16]) -> Result<super::Injection> {
    let dst = Cff::parse(target)?;
    let src = Cff::parse(source)?;

    if glyphs.is_empty() {
        return Err(FontError::Malformed("no glyphs to inject"));
    }

    // Refused before any work, not discovered while writing. A CJK subset that
    // preserves CID = GID sits at the CFF ceiling of 65,535 charstrings, and
    // there is no 65,536th slot to append to — the INDEX count is a Card16.
    // Nothing downstream can rescue that, so saying so here is both cheaper and
    // more honest than a failure emerging from the serialiser.
    let would_be = dst.glyph_count() + glyphs.len();
    if would_be > MAX_INDEX_ENTRIES {
        return Err(FontError::Full {
            what: "the CFF CharStrings INDEX",
            have: dst.glyph_count(),
            limit: MAX_INDEX_ENTRIES,
        });
    }

    if dst.is_cid {
        return inject_cid_cff(target, source, glyphs, &dst, &src);
    }

    let first_new = dst.glyph_count();
    let mut mapping: HashMap<u16, u16> = HashMap::new();
    let mut added: Vec<Vec<u8>> = Vec::new();
    let mut added_sids: Vec<u16> = Vec::new();
    // Custom strings copied across, appended to the target's String INDEX.
    let mut new_strings: Vec<Vec<u8>> = Vec::new();

    for (i, &gid) in glyphs.iter().enumerate() {
        let cs = src
            .charstring(source, gid as usize)
            .ok_or(FontError::Malformed("source glyph is not in the font"))?;

        // Step 1: a charstring that depends on no subroutine index.
        let local = src.local_subrs_for(gid as usize);
        let standalone = inline_subrs(source, cs, local, &src.global_subrs)?;
        added.push(standalone);

        // The glyph's name, as a SID.
        let sid = src.glyph_id_for(gid as usize).unwrap_or(0);
        let new_sid = if sid < STANDARD_STRINGS {
            sid
        } else {
            let name = src
                .strings
                .get(source, (sid - STANDARD_STRINGS) as usize)
                .ok_or(FontError::Malformed("charset names a string the font lacks"))?;
            let at = dst.strings.len() + new_strings.len();
            new_strings.push(name.to_vec());
            u16::try_from(STANDARD_STRINGS as usize + at)
                .map_err(|_| FontError::Malformed("too many strings"))?
        };
        added_sids.push(new_sid);

        mapping.insert(
            gid,
            u16::try_from(first_new + i).map_err(|_| FontError::Malformed("too many glyphs"))?,
        );
    }

    // --- the pieces ------------------------------------------------------
    let name_index = raw_index(target, &dst.names);
    let strings: Vec<Vec<u8>> = (0..dst.strings.len())
        .map(|i| dst.strings.get(target, i).unwrap_or_default().to_vec())
        .chain(new_strings)
        .collect();
    let string_index = build_index(&strings)?;
    let gsubr_index = raw_index(target, &dst.global_subrs);

    let charstrings: Vec<Vec<u8>> = (0..dst.glyph_count())
        .map(|i| dst.charstring(target, i).unwrap_or_default().to_vec())
        .chain(added)
        .collect();
    let charstrings_index = build_index(&charstrings)?;

    // Charset format 0: a SID per glyph after .notdef.
    let mut charset = vec![0u8];
    for gid in 1..dst.glyph_count() {
        charset.extend_from_slice(&dst.glyph_id_for(gid).unwrap_or(0).to_be_bytes());
    }
    for sid in &added_sids {
        charset.extend_from_slice(&sid.to_be_bytes());
    }

    // The private dict and its local subroutines, copied as one block so the
    // Subrs offset inside it -- which is relative to the dict -- stays valid.
    let private = private_block(target, &dst);

    // --- lay it out ------------------------------------------------------
    // Offsets are five-byte encoded, so the Top DICT's length does not depend
    // on their values and one pass is enough.
    let header: [u8; 4] = [1, 0, 4, 4];
    let dict_len = top_dict(0, 0, &private, 0).len();
    let top_index_len = build_index(&[vec![0u8; dict_len]])?.len();

    let mut at = header.len() + name_index.len() + top_index_len;
    at += string_index.len() + gsubr_index.len();
    let charset_at = at;
    at += charset.len();
    let charstrings_at = at;
    at += charstrings_index.len();
    let private_at = at;

    let dict = top_dict(charset_at, charstrings_at, &private, private_at);
    debug_assert_eq!(dict.len(), dict_len, "the fixed encoding kept the dict stable");

    let mut out = Vec::with_capacity(private_at + private.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&name_index);
    out.extend_from_slice(&build_index(&[dict])?);
    out.extend_from_slice(&string_index);
    out.extend_from_slice(&gsubr_index);
    out.extend_from_slice(&charset);
    out.extend_from_slice(&charstrings_index);
    out.extend_from_slice(&private);

    Ok(super::Injection {
        bytes: out,
        mapping,
        loca_widened: false,
        components_pulled: 0,
        // A CFF has no `loca`.
        target_loca_inconsistent: false,
    })
}

/// Spec 8.4's third CFF step: "For CID-keyed CFF, place in the correct FD and
/// extend FDSelect."
///
/// The glyph gets a **new FD carrying the source's own private dict**, rather
/// than being assigned to whichever existing FD looks closest. A private dict
/// holds the hinting parameters — blue zones, standard stem widths — that the
/// outline was drawn against; putting a Times glyph under a Helvetica FD hints
/// it against the wrong stems and it renders subtly wrong at small sizes, with
/// nothing to show for it in any structural check.
///
/// The new FD needs no `Subrs`: §8.4 step 1 has already inlined them, which is
/// the other reason inlining beats merging.
fn inject_cid_cff(
    target: &[u8],
    source: &[u8],
    glyphs: &[u16],
    dst: &Cff,
    src: &Cff,
) -> Result<super::Injection> {
    let first_new = dst.glyph_count();
    let mut mapping: HashMap<u16, u16> = HashMap::new();
    let mut added: Vec<Vec<u8>> = Vec::new();
    let mut added_cids: Vec<u16> = Vec::new();
    // One new FD per source FD actually used, so two glyphs from the same
    // source FD share one rather than duplicating the private dict.
    let mut new_fds: Vec<Vec<u8>> = Vec::new();
    let mut fd_for_source: HashMap<u8, u8> = HashMap::new();
    let mut added_fd_index: Vec<u8> = Vec::new();

    let existing_fds = dst.fd_local_subrs.len().max(1);
    let mut next_cid = dst.charset.iter().copied().max().unwrap_or(0);

    for &gid in glyphs {
        let cs = src
            .charstring(source, gid as usize)
            .ok_or(FontError::Malformed("source glyph is not in the font"))?;
        added.push(inline_subrs(source, cs, src.local_subrs_for(gid as usize), &src.global_subrs)?);

        // A CID-keyed charset holds CIDs. A fresh one past the highest in use
        // avoids colliding with a CID the document already draws.
        //
        // A second ceiling, independent of the CharStrings one: a font can have
        // room for another charstring and still have no CID to give it, because
        // CIDs are u16 and this allocates above the highest in use. `Full`
        // rather than `Malformed` because the font is not defective -- it is
        // simply out of space, and the caller's recourse is a different font
        // rather than a repair.
        next_cid = next_cid.checked_add(1).ok_or(FontError::Full {
            what: "the CID space",
            have: u16::MAX as usize + 1,
            limit: u16::MAX as usize + 1,
        })?;
        added_cids.push(next_cid);

        let source_fd = src.fd_select.get(gid as usize).copied().unwrap_or(0);
        let fd = match fd_for_source.get(&source_fd) {
            Some(fd) => *fd,
            None => {
                let private = fd_private_block(source, src, source_fd);
                let index = u8::try_from(existing_fds + new_fds.len())
                    .map_err(|_| FontError::Malformed("more than 255 font dicts"))?;
                new_fds.push(private);
                fd_for_source.insert(source_fd, index);
                index
            }
        };
        added_fd_index.push(fd);

        mapping.insert(
            gid,
            u16::try_from(first_new + mapping.len())
                .map_err(|_| FontError::Malformed("too many glyphs"))?,
        );
    }

    let name_index = raw_index(target, &dst.names);
    let string_index = raw_index(target, &dst.strings);
    let gsubr_index = raw_index(target, &dst.global_subrs);

    let charstrings: Vec<Vec<u8>> = (0..dst.glyph_count())
        .map(|i| dst.charstring(target, i).unwrap_or_default().to_vec())
        .chain(added)
        .collect();
    let charstrings_index = build_index(&charstrings)?;

    // Charset format 0, holding CIDs.
    let mut charset = vec![0u8];
    for gid in 1..dst.glyph_count() {
        charset.extend_from_slice(&dst.glyph_id_for(gid).unwrap_or(0).to_be_bytes());
    }
    for cid in &added_cids {
        charset.extend_from_slice(&cid.to_be_bytes());
    }

    // FDSelect format 0: one byte per glyph. Chosen over format 3's ranges
    // because appending to a range table means recomputing every range, and a
    // byte per glyph is a few kilobytes on a font that already carries
    // hundreds.
    let mut fd_select = vec![0u8];
    for gid in 0..dst.glyph_count() {
        fd_select.push(dst.fd_select.get(gid).copied().unwrap_or(0));
    }
    fd_select.extend_from_slice(&added_fd_index);

    // The FDArray: the target's font dicts, then the new ones. Each entry is a
    // dict naming its own private dict, so those move too.
    let mut fd_dicts: Vec<Vec<u8>> = Vec::new();
    let mut fd_privates: Vec<Vec<u8>> = Vec::new();
    if let Some(at) = top_operator(dst.top_dicts.get(target, 0).unwrap_or_default(), 0x0c24) {
        let array = crate::cff::Index::default();
        let _ = array;
        if let Ok(fd_array) = read_index_at(target, at as usize) {
            for i in 0..fd_array.len() {
                let dict = fd_array.get(target, i).unwrap_or_default();
                fd_privates.push(
                    private_location(dict)
                        .and_then(|(size, off)| target.get(off..off + size).map(<[u8]>::to_vec))
                        .unwrap_or_default(),
                );
                fd_dicts.push(dict.to_vec());
            }
        }
    }
    while fd_privates.len() < existing_fds {
        fd_privates.push(Vec::new());
        fd_dicts.push(Vec::new());
    }
    for private in new_fds {
        fd_privates.push(private);
        fd_dicts.push(Vec::new());
    }

    // --- lay it out ------------------------------------------------------
    let header: [u8; 4] = [1, 0, 4, 4];
    let dict_len = cid_top_dict(0, 0, 0, 0).len();
    let top_index_len = build_index(&[vec![0u8; dict_len]])?.len();

    let mut at = header.len() + name_index.len() + top_index_len;
    at += string_index.len() + gsubr_index.len();
    let charset_at = at;
    at += charset.len();
    let fd_select_at = at;
    at += fd_select.len();
    let charstrings_at = at;
    at += charstrings_index.len();

    // Each font dict names a private dict laid out after the array, so the
    // array's own size has to be known before those offsets can be written --
    // and it depends on them. The five-byte encoding settles it again.
    let fd_entry_len = font_dict(0, 0).len();
    let fd_array = build_index(&vec![vec![0u8; fd_entry_len]; fd_privates.len()])?;
    let mut private_at = at + fd_array.len();

    let mut real_dicts = Vec::with_capacity(fd_privates.len());
    let mut privates_blob = Vec::new();
    for private in &fd_privates {
        real_dicts.push(font_dict(private.len(), private_at));
        privates_blob.extend_from_slice(private);
        private_at += private.len();
    }
    let fd_array = build_index(&real_dicts)?;

    let dict = cid_top_dict(charset_at, charstrings_at, at, fd_select_at);
    debug_assert_eq!(dict.len(), dict_len);

    let mut out = Vec::new();
    out.extend_from_slice(&header);
    out.extend_from_slice(&name_index);
    out.extend_from_slice(&build_index(&[dict])?);
    out.extend_from_slice(&string_index);
    out.extend_from_slice(&gsubr_index);
    out.extend_from_slice(&charset);
    out.extend_from_slice(&fd_select);
    out.extend_from_slice(&charstrings_index);
    out.extend_from_slice(&fd_array);
    out.extend_from_slice(&privates_blob);

    Ok(super::Injection {
        bytes: out,
        mapping,
        loca_widened: false,
        components_pulled: 0,
        // A CFF has no `loca`.
        target_loca_inconsistent: false,
    })
}

/// The private dict of one FD in a CID-keyed font, without its `Subrs`.
///
/// The local subroutines are dropped deliberately: the injected charstring no
/// longer calls any, and carrying an index whose offset would have to be fixed
/// up is work for nothing.
fn fd_private_block(data: &[u8], cff: &Cff, fd: u8) -> Vec<u8> {
    let Some(top) = cff.top_dicts.get(data, 0) else { return Vec::new() };
    let Some(at) = top_operator(top, 0x0c24) else { return Vec::new() };
    let Ok(array) = read_index_at(data, at as usize) else { return Vec::new() };
    let Some(dict) = array.get(data, fd as usize) else { return Vec::new() };
    let Some((size, offset)) = private_location(dict) else { return Vec::new() };
    strip_subrs(data.get(offset..offset + size).unwrap_or_default())
}

/// A private dict with any `Subrs` operator removed.
fn strip_subrs(dict: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dict.len());
    let mut operands_start = 0usize;
    let mut i = 0usize;
    while i < dict.len() {
        let b = dict[i];
        match b {
            0..=21 => {
                let end = if b == 12 { i + 2 } else { i + 1 };
                let op =
                    if b == 12 { 0x0c00 | *dict.get(i + 1).unwrap_or(&0) as u16 } else { b as u16 };
                // 19 is Subrs; its operand goes with it.
                if op != 19 {
                    out.extend_from_slice(dict.get(operands_start..end).unwrap_or_default());
                }
                i = end;
                operands_start = i;
            }
            28 => i += 3,
            29 => i += 5,
            30 => {
                i += 1;
                while i < dict.len() && dict[i] & 0x0f != 0x0f && dict[i] >> 4 != 0x0f {
                    i += 1;
                }
                i += 1;
            }
            247..=254 => i += 2,
            _ => i += 1,
        }
    }
    out
}

/// A font dict for the FDArray, naming its private dict.
fn font_dict(private_size: usize, private_at: usize) -> Vec<u8> {
    let mut out = Vec::new();
    push_offset(&mut out, private_size as i32);
    push_offset(&mut out, private_at as i32);
    out.push(18); // Private
    out
}

/// A Top DICT for a CID-keyed font.
fn cid_top_dict(charset: usize, charstrings: usize, fd_array: usize, fd_select: usize) -> Vec<u8> {
    let mut out = Vec::new();
    // ROS: three operands then 12 30. The SIDs name the registry and ordering
    // strings; 0 is `.notdef`, which is wrong but harmless -- readers key off
    // the operator's presence, not its operands, and inventing string entries
    // would grow the String INDEX for nothing.
    push_offset(&mut out, 0);
    push_offset(&mut out, 0);
    push_offset(&mut out, 0);
    out.extend_from_slice(&[12, 30]);

    push_offset(&mut out, charset as i32);
    out.push(15);
    push_offset(&mut out, charstrings as i32);
    out.push(17);
    push_offset(&mut out, fd_array as i32);
    out.extend_from_slice(&[12, 36]); // FDArray
    push_offset(&mut out, fd_select as i32);
    out.extend_from_slice(&[12, 37]); // FDSelect
    out
}

/// The operand of a one- or two-byte Top DICT operator.
fn top_operator(top: &[u8], want: u16) -> Option<i64> {
    let mut operands: Vec<i64> = Vec::new();
    let mut i = 0;
    while i < top.len() {
        let b = top[i];
        match b {
            0..=21 => {
                let op = if b == 12 {
                    i += 1;
                    0x0c00 | *top.get(i)? as u16
                } else {
                    b as u16
                };
                if op == want {
                    return operands.last().copied();
                }
                operands.clear();
                i += 1;
            }
            28 => {
                operands.push(i16::from_be_bytes([*top.get(i + 1)?, *top.get(i + 2)?]) as i64);
                i += 3;
            }
            29 => {
                let v = top.get(i + 1..i + 5)?;
                operands.push(i32::from_be_bytes([v[0], v[1], v[2], v[3]]) as i64);
                i += 5;
            }
            30 => {
                i += 1;
                while i < top.len() && top[i] & 0x0f != 0x0f && top[i] >> 4 != 0x0f {
                    i += 1;
                }
                i += 1;
                operands.push(0);
            }
            32..=246 => {
                operands.push(b as i64 - 139);
                i += 1;
            }
            247..=250 => {
                operands.push((b as i64 - 247) * 256 + *top.get(i + 1)? as i64 + 108);
                i += 2;
            }
            251..=254 => {
                operands.push(-(b as i64 - 251) * 256 - *top.get(i + 1)? as i64 - 108);
                i += 2;
            }
            _ => i += 1,
        }
    }
    None
}

/// Read an INDEX at an absolute offset, reusing the reader the parser uses.
fn read_index_at(data: &[u8], at: usize) -> Result<crate::cff::Index> {
    crate::cff::read_index_public(data, at)
}

/// The private dict plus its local subroutines, as a contiguous block.
fn private_block(data: &[u8], cff: &Cff) -> Vec<u8> {
    let Some(top) = cff.top_dicts.get(data, 0) else { return Vec::new() };
    let Some((size, offset)) = private_location(top) else { return Vec::new() };
    let Some(dict) = data.get(offset..offset + size) else { return Vec::new() };

    let mut out = dict.to_vec();
    // Local subroutines follow, if any. Their offset in the dict is relative to
    // the dict's own start, so keeping the two adjacent keeps it correct --
    // provided the subroutines really do begin right after the dict, which is
    // how every producer writes them.
    if !cff.local_subrs.is_empty()
        && let Some(start) = cff.local_subrs.items.first().map(|(s, _)| *s)
    {
        let subr_start = offset + size;
        if start >= subr_start
            && let Some(tail) = data.get(subr_start..cff.local_subrs.end)
        {
            out.extend_from_slice(tail);
        }
    }
    out
}

/// `(size, offset)` of the Private DICT from a Top DICT.
fn private_location(top: &[u8]) -> Option<(usize, usize)> {
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < top.len() {
        let b = top[i];
        match b {
            0..=21 => {
                let op = if b == 12 {
                    i += 1;
                    0x0c00 | *top.get(i)? as u16
                } else {
                    b as u16
                };
                if op == 18 && operands.len() >= 2 {
                    return Some((operands[0] as usize, operands[1] as usize));
                }
                operands.clear();
                i += 1;
            }
            28 => {
                operands.push(i16::from_be_bytes([*top.get(i + 1)?, *top.get(i + 2)?]) as f64);
                i += 3;
            }
            29 => {
                let v = top.get(i + 1..i + 5)?;
                operands.push(i32::from_be_bytes([v[0], v[1], v[2], v[3]]) as f64);
                i += 5;
            }
            30 => {
                // A real; skipped rather than decoded, since no offset uses one.
                i += 1;
                while i < top.len() && top[i] & 0x0f != 0x0f && top[i] >> 4 != 0x0f {
                    i += 1;
                }
                i += 1;
                operands.push(0.0);
            }
            32..=246 => {
                operands.push(b as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                operands.push((b as f64 - 247.0) * 256.0 + *top.get(i + 1)? as f64 + 108.0);
                i += 2;
            }
            251..=254 => {
                operands.push(-(b as f64 - 251.0) * 256.0 - *top.get(i + 1)? as f64 - 108.0);
                i += 2;
            }
            _ => i += 1,
        }
    }
    None
}

/// A Top DICT with the offsets this rebuild needs, all five-byte encoded.
fn top_dict(charset: usize, charstrings: usize, private: &[u8], private_at: usize) -> Vec<u8> {
    let mut out = Vec::new();
    // ROS and the other CID operators are deliberately absent: this path
    // refuses CID-keyed fonts, so emitting them would describe a font that is
    // not what was written.
    push_offset(&mut out, charset as i32);
    out.push(15); // charset
    push_offset(&mut out, charstrings as i32);
    out.push(17); // CharStrings
    push_offset(&mut out, private.len() as i32);
    push_offset(&mut out, private_at as i32);
    out.push(18); // Private
    out
}

fn push_offset(out: &mut Vec<u8>, value: i32) {
    out.push(29);
    out.extend_from_slice(&value.to_be_bytes());
}

/// Copy an INDEX out of a font verbatim, rebuilding it from its entries.
///
/// Infallible because the entries came out of an INDEX that was already
/// serialised, so their count already fitted the count field.
fn raw_index(data: &[u8], index: &Index) -> Vec<u8> {
    let entries: Vec<Vec<u8>> =
        (0..index.len()).map(|i| index.get(data, i).unwrap_or_default().to_vec()).collect();
    build_index(&entries).unwrap_or_default()
}

/// The most entries a CFF INDEX can hold.
///
/// The count is a Card16 — two bytes — so this is a hard ceiling of the
/// container format, not a limit of this implementation. CFF2 widened it to
/// Card32; CFF 1 did not, and PDF embeds CFF 1.
pub const MAX_INDEX_ENTRIES: usize = u16::MAX as usize;

/// Serialise an INDEX.
///
/// Fallible for exactly one reason, and it is not hypothetical: a CJK subset
/// that keeps CID equal to GID reaches 65,535 charstrings, and the corpus has
/// two. Writing `entries.len() as u16` there produces a count of **zero** — a
/// font declaring no glyphs at all, which parses, passes a structural check,
/// and renders nothing. Returning an error instead is the difference between
/// declining an impossible edit and shipping a broken font.
pub fn build_index(entries: &[Vec<u8>]) -> Result<Vec<u8>> {
    if entries.len() > MAX_INDEX_ENTRIES {
        return Err(FontError::Full {
            what: "the CFF INDEX",
            have: entries.len(),
            limit: MAX_INDEX_ENTRIES,
        });
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    if entries.is_empty() {
        return Ok(out);
    }

    let total: usize = entries.iter().map(|e| e.len()).sum();
    // offSize is the smallest that can address one past the end.
    let off_size: u8 = match total + 1 {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    out.push(off_size);

    let mut offset: u32 = 1;
    let write = |out: &mut Vec<u8>, v: u32| {
        let bytes = v.to_be_bytes();
        out.extend_from_slice(&bytes[4 - off_size as usize..]);
    };
    write(&mut out, offset);
    for e in entries {
        offset += e.len() as u32;
        write(&mut out, offset);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_index` for fixtures, whose entry counts are fixed and tiny.
    ///
    /// The fallible form exists for one case — a font already holding 65,535
    /// charstrings — which no fixture here comes near. Threading `?` through
    /// every fixture to guard against it would obscure what they are testing.
    fn index(entries: &[Vec<u8>]) -> Vec<u8> {
        build_index(entries).expect("a fixture INDEX is far below the Card16 ceiling")
    }

    /// A minimal name-keyed CFF with `n` charstrings, each a distinct body.
    ///
    /// `wrapping_add` because the ceiling tests build this at 65,535 glyphs and
    /// plain addition panics in debug long before that. Bodies stay distinct
    /// modulo 256, which is all the small-`n` tests ask of them.
    fn minimal(n: usize) -> Vec<u8> {
        let charstrings: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                // `<i+10> <i+20> rmoveto endchar`
                let i = i as u8;
                vec![i.wrapping_add(149), i.wrapping_add(159), 21, 14]
            })
            .collect();
        let strings: Vec<Vec<u8>> = vec![b"custom".to_vec()];

        // charset format 0: standard SID for gid 1, a custom SID for gid 2.
        let mut charset = vec![0u8];
        for i in 1..n {
            let sid: u16 = if i == 1 { 66 } else { STANDARD_STRINGS };
            charset.extend_from_slice(&sid.to_be_bytes());
        }

        let private = vec![0u8; 4];
        let name_index = index(&[b"Test".to_vec()]);
        let string_index = index(&strings);
        let gsubr_index = index(&[]);
        let charstrings_index = index(&charstrings);

        let dict_len = top_dict(0, 0, &private, 0).len();
        let top_index_len = index(&[vec![0u8; dict_len]]).len();
        let mut at = 4 + name_index.len() + top_index_len + string_index.len() + gsubr_index.len();
        let charset_at = at;
        at += charset.len();
        let charstrings_at = at;
        at += charstrings_index.len();
        let private_at = at;

        let mut out = vec![1u8, 0, 4, 4];
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&index(&[top_dict(
            charset_at,
            charstrings_at,
            &private,
            private_at,
        )]));
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&gsubr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&charstrings_index);
        out.extend_from_slice(&private);
        out
    }

    #[test]
    fn the_fixture_is_a_valid_cff() {
        let bytes = minimal(3);
        let cff = Cff::parse(&bytes).expect("parse");
        assert_eq!(cff.glyph_count(), 3);
        assert_eq!(cff.charstring(&bytes, 1), Some(&[150u8, 160, 21, 14][..]));
        assert_eq!(cff.glyph_id_for(1), Some(66));
    }

    #[test]
    fn a_glyph_is_appended_and_the_originals_stay_put() {
        let target = minimal(2);
        let source = minimal(4);
        let out = inject_cff(&target, &source, &[3]).expect("inject");

        let rebuilt = Cff::parse(&out.bytes).expect("the rebuilt font parses");
        assert_eq!(rebuilt.glyph_count(), 3);
        assert_eq!(out.mapping.get(&3), Some(&2), "appended after the target's two");

        let before = Cff::parse(&target).unwrap();
        for gid in 0..2 {
            assert_eq!(
                rebuilt.charstring(&out.bytes, gid),
                before.charstring(&target, gid),
                "glyph {gid} changed"
            );
        }
    }

    #[test]
    fn the_injected_charstring_is_the_sources() {
        let target = minimal(1);
        let source = minimal(4);
        let out = inject_cff(&target, &source, &[2]).unwrap();

        let rebuilt = Cff::parse(&out.bytes).unwrap();
        let src = Cff::parse(&source).unwrap();
        assert_eq!(
            rebuilt.charstring(&out.bytes, out.mapping[&2] as usize),
            src.charstring(&source, 2)
        );
    }

    #[test]
    fn a_standard_sid_is_copied_unchanged() {
        // SIDs below 391 mean the same string in every CFF.
        let target = minimal(1);
        let source = minimal(3);
        let out = inject_cff(&target, &source, &[1]).unwrap();
        let rebuilt = Cff::parse(&out.bytes).unwrap();
        assert_eq!(rebuilt.glyph_id_for(out.mapping[&1] as usize), Some(66));
    }

    #[test]
    fn a_custom_name_travels_with_its_string() {
        // SID 391 is private to the source; the string has to come too, under
        // a SID that means the same thing in the target.
        let target = minimal(1);
        let source = minimal(3);
        let out = inject_cff(&target, &source, &[2]).unwrap();

        let rebuilt = Cff::parse(&out.bytes).unwrap();
        let sid = rebuilt.glyph_id_for(out.mapping[&2] as usize).expect("a sid");
        assert!(sid >= STANDARD_STRINGS, "a custom sid: {sid}");
        let name = rebuilt
            .strings
            .get(&out.bytes, (sid - STANDARD_STRINGS) as usize)
            .expect("the string came too");
        assert_eq!(name, b"custom");
    }

    #[test]
    fn subroutines_are_inlined_out_of_the_injected_glyph() {
        // The whole point of step 1: the charstring must not reach the target
        // still calling subroutines, which would index the target's index.
        let target = minimal(1);

        // A source whose glyph 1 calls local subr 0.
        let subr = vec![149u8, 159, 21, 11]; // rmoveto, return
        let charstrings: Vec<Vec<u8>> =
            vec![vec![14], vec![139 - 107, 10, 14], vec![140, 141, 21, 14]];
        let source = {
            let private = {
                // A private dict whose Subrs offset points just past itself.
                // The offset is relative to the dict's own start, and the dict
                // is exactly six bytes: a five-byte integer plus the operator.
                let mut d = Vec::new();
                d.push(29);
                d.extend_from_slice(&6i32.to_be_bytes());
                d.push(19); // Subrs
                assert_eq!(d.len(), 6);
                d
            };
            let subrs = index(&[subr]);
            let mut private_block = private.clone();
            private_block.extend_from_slice(&subrs);

            let name_index = index(&[b"Src".to_vec()]);
            let string_index = index(&[]);
            let gsubr_index = index(&[]);
            let charstrings_index = index(&charstrings);
            let charset = vec![0u8, 0, 66, 0, 67];

            let dict_len = top_dict(0, 0, &private, 0).len();
            let top_index_len = index(&[vec![0u8; dict_len]]).len();
            let mut at =
                4 + name_index.len() + top_index_len + string_index.len() + gsubr_index.len();
            let charset_at = at;
            at += charset.len();
            let charstrings_at = at;
            at += charstrings_index.len();
            let private_at = at;

            let mut out = vec![1u8, 0, 4, 4];
            out.extend_from_slice(&name_index);
            out.extend_from_slice(&index(&[top_dict(
                charset_at,
                charstrings_at,
                &private,
                private_at,
            )]));
            out.extend_from_slice(&string_index);
            out.extend_from_slice(&gsubr_index);
            out.extend_from_slice(&charset);
            out.extend_from_slice(&charstrings_index);
            out.extend_from_slice(&private_block);
            out
        };

        let src = Cff::parse(&source).expect("source parses");
        assert_eq!(src.local_subrs.len(), 1, "the fixture really has a subroutine");

        let out = inject_cff(&target, &source, &[1]).expect("inject");
        let rebuilt = Cff::parse(&out.bytes).unwrap();
        let injected = rebuilt.charstring(&out.bytes, out.mapping[&1] as usize).unwrap();

        assert!(!crate::charstring::calls_subroutine(injected), "a call survived: {injected:?}");
        assert_eq!(injected, &[149u8, 159, 21, 14], "the subr body was spliced in");
    }

    /// A CID-keyed CFF with `n` glyphs, all in FD 0.
    ///
    /// `wrapping_add` rather than `+` because the ceiling tests build this at
    /// 65,535 glyphs, and plain addition panics in debug long before that. The
    /// bodies stay distinct modulo 256, which is all the small-`n` tests here
    /// ask of them.
    fn minimal_cid(n: usize) -> Vec<u8> {
        let charstrings: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                let i = i as u8;
                vec![i.wrapping_add(149), i.wrapping_add(159), 21, 14]
            })
            .collect();

        let mut charset = vec![0u8];
        for i in 1..n {
            charset.extend_from_slice(&(i as u16).wrapping_add(100).to_be_bytes());
        }
        let mut fd_select = vec![0u8];
        fd_select.extend(std::iter::repeat_n(0u8, n));

        // One FD with a private dict carrying a recognisable hint parameter.
        let private = {
            let mut d = Vec::new();
            push_offset(&mut d, 55);
            d.push(10); // StdHW
            d
        };

        let name_index = index(&[b"CidTest".to_vec()]);
        let string_index = index(&[]);
        let gsubr_index = index(&[]);
        let charstrings_index = index(&charstrings);

        let dict_len = cid_top_dict(0, 0, 0, 0).len();
        let top_index_len = index(&[vec![0u8; dict_len]]).len();
        let mut at = 4 + name_index.len() + top_index_len + string_index.len() + gsubr_index.len();
        let charset_at = at;
        at += charset.len();
        let fd_select_at = at;
        at += fd_select.len();
        let charstrings_at = at;
        at += charstrings_index.len();

        let fd_entry_len = font_dict(0, 0).len();
        let fd_array_len = index(&[vec![0u8; fd_entry_len]]).len();
        let private_at = at + fd_array_len;
        let fd_array = index(&[font_dict(private.len(), private_at)]);

        let mut out = vec![1u8, 0, 4, 4];
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&index(&[cid_top_dict(
            charset_at,
            charstrings_at,
            at,
            fd_select_at,
        )]));
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&gsubr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&fd_select);
        out.extend_from_slice(&charstrings_index);
        out.extend_from_slice(&fd_array);
        out.extend_from_slice(&private);
        out
    }

    #[test]
    fn the_cid_fixture_is_a_cid_keyed_font() {
        let bytes = minimal_cid(3);
        let cff = Cff::parse(&bytes).expect("parses");
        assert!(cff.is_cid);
        assert_eq!(cff.glyph_count(), 3);
        assert_eq!(cff.glyph_id_for(1), Some(101), "the charset holds CIDs");
        assert_eq!(cff.fd_local_subrs.len(), 1, "one FD");
    }

    #[test]
    fn a_cid_keyed_target_gains_a_glyph_and_an_fd() {
        // Spec 8.4 step 3. The glyph gets a *new* FD carrying the source's own
        // private dict: hinting the outline against another font's stem widths
        // renders it subtly wrong with nothing to show for it structurally.
        let target = minimal_cid(3);
        let source = minimal_cid(5);
        let out = inject_cff(&target, &source, &[4]).expect("inject");

        let rebuilt = Cff::parse(&out.bytes).expect("the rebuilt CID font parses");
        assert!(rebuilt.is_cid);
        assert_eq!(rebuilt.glyph_count(), 4);
        assert_eq!(out.mapping.get(&4), Some(&3));

        // A second FD was added and the new glyph points at it.
        assert_eq!(rebuilt.fd_local_subrs.len(), 2, "the source's FD came too");
        assert_eq!(rebuilt.fd_select.get(3).copied(), Some(1), "the new glyph uses it");
        for gid in 0..3 {
            assert_eq!(rebuilt.fd_select.get(gid).copied(), Some(0), "originals kept FD 0");
        }
    }

    #[test]
    fn the_injected_cid_glyph_keeps_its_charstring_and_gets_a_fresh_cid() {
        let target = minimal_cid(3);
        let source = minimal_cid(5);
        let out = inject_cff(&target, &source, &[4]).unwrap();

        let rebuilt = Cff::parse(&out.bytes).unwrap();
        let src = Cff::parse(&source).unwrap();
        let new_gid = out.mapping[&4] as usize;
        assert_eq!(rebuilt.charstring(&out.bytes, new_gid), src.charstring(&source, 4));

        // A CID past every one already in use, so it cannot collide with a CID
        // the document already draws.
        let cid = rebuilt.glyph_id_for(new_gid).expect("a cid");
        assert!(cid > 102, "fresh: {cid}");
        for gid in 0..3 {
            assert_eq!(rebuilt.glyph_id_for(gid), Cff::parse(&target).unwrap().glyph_id_for(gid));
        }
    }

    #[test]
    fn two_glyphs_from_one_source_fd_share_one_new_fd() {
        // Duplicating the private dict per glyph would grow the font for
        // nothing and make two identical FDs a reader has to distinguish.
        let target = minimal_cid(2);
        let source = minimal_cid(6);
        let out = inject_cff(&target, &source, &[3, 4]).unwrap();
        let rebuilt = Cff::parse(&out.bytes).unwrap();

        assert_eq!(rebuilt.glyph_count(), 4);
        assert_eq!(rebuilt.fd_local_subrs.len(), 2, "one new FD, not two");
        assert_eq!(rebuilt.fd_select.get(2).copied(), Some(1));
        assert_eq!(rebuilt.fd_select.get(3).copied(), Some(1));
    }

    #[test]
    fn subrs_are_stripped_from_a_copied_private_dict() {
        // The injected charstring calls none, so an index whose offset would
        // have to be fixed up is work for nothing.
        let mut dict = Vec::new();
        push_offset(&mut dict, 55);
        dict.push(10); // StdHW
        push_offset(&mut dict, 999);
        dict.push(19); // Subrs
        push_offset(&mut dict, 42);
        dict.push(20); // StdVW

        let stripped = strip_subrs(&dict);
        // Exactly the Subrs entry -- its five-byte operand and the operator --
        // is gone, and the two hint parameters are untouched.
        assert_eq!(stripped.len(), dict.len() - 6);
        assert_eq!(top_operator(&stripped, 10), Some(55), "StdHW survived");
        assert_eq!(top_operator(&stripped, 20), Some(42), "StdVW survived");
        assert_eq!(top_operator(&stripped, 19), None, "Subrs is gone");
    }

    #[test]
    fn injecting_nothing_is_refused() {
        let target = minimal(2);
        let source = minimal(2);
        assert!(inject_cff(&target, &source, &[]).is_err());
    }

    #[test]
    fn a_glyph_past_the_source_is_refused() {
        let target = minimal(2);
        let source = minimal(2);
        assert!(inject_cff(&target, &source, &[9]).is_err());
    }

    #[test]
    fn index_offsets_size_themselves_to_the_data() {
        assert_eq!(index(&[]).len(), 2);
        let small = index(&[vec![0u8; 10]]);
        assert_eq!(small[2], 1, "one byte is enough for 11");
        let big = index(&[vec![0u8; 300]]);
        assert_eq!(big[2], 2, "301 needs two");
    }

    #[test]
    fn an_index_at_the_card16_ceiling_is_refused_rather_than_truncated() {
        // 65,535 entries is the most a CFF INDEX can count. One more wraps the
        // Card16 to zero, and a font declaring zero glyphs parses, passes a
        // structural check, and renders nothing -- which is precisely what this
        // used to produce.
        let ok = vec![vec![0u8; 1]; MAX_INDEX_ENTRIES];
        let built = build_index(&ok).expect("exactly at the ceiling is fine");
        assert_eq!(
            u16::from_be_bytes([built[0], built[1]]),
            u16::MAX,
            "the count field holds the full 65,535"
        );

        let one_too_many = vec![vec![0u8; 1]; MAX_INDEX_ENTRIES + 1];
        assert!(
            matches!(build_index(&one_too_many), Err(FontError::Full { .. })),
            "one past the ceiling is refused, not wrapped to a count of 0"
        );
    }

    #[test]
    fn a_full_font_is_refused_before_any_work_is_done() {
        // A CJK subset preserving CID = GID reaches the ceiling for real: the
        // corpus has two, both Noto Serif CJK JP. There is no slot to append
        // to, so the answer is a typed refusal the caller can act on -- try a
        // second font, or substitute -- and not a malformed-font error that
        // suggests the input was at fault. It was not; it is simply full.
        let full = minimal_cid(MAX_INDEX_ENTRIES);
        let source = minimal(2);
        match inject_cff(&full, &source, &[1]) {
            Err(FontError::Full { what, have, limit }) => {
                assert_eq!(have, MAX_INDEX_ENTRIES);
                assert_eq!(limit, MAX_INDEX_ENTRIES);
                assert!(what.contains("CharStrings"), "the message names what filled up: {what}");
            }
            other => panic!("expected a Full refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_font_one_short_of_full_still_accepts_a_glyph() {
        // The complement of the test above, and the one that would catch an
        // off-by-one turning the refusal into a blanket "large fonts cannot be
        // edited". The last free slot has to remain usable.
        let nearly = minimal(MAX_INDEX_ENTRIES - 1);
        let source = minimal(2);
        let out = inject_cff(&nearly, &source, &[1]).expect("the last slot is still free");
        let rebuilt = Cff::parse(&out.bytes).expect("parse");
        assert_eq!(rebuilt.glyph_count(), MAX_INDEX_ENTRIES);
    }

    #[test]
    fn a_cid_font_with_no_free_cid_is_refused_too() {
        // A second ceiling, and one that bites before the first: a CID font can
        // have a free CharStrings slot and still have no CID to put in it,
        // because CIDs are u16 and a new glyph takes one above the highest in
        // use. At this size the fixture's CIDs span the whole 16-bit space, as
        // a real CID-keyed font of the same size would.
        let full_cids = minimal_cid(MAX_INDEX_ENTRIES - 1);
        let source = minimal(2);
        match inject_cff(&full_cids, &source, &[1]) {
            Err(FontError::Full { what, .. }) => {
                assert!(what.contains("CID"), "the message names the CID space: {what}");
            }
            other => panic!("expected a Full refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_rebuilt_index_is_readable_by_the_parser() {
        // The writer and the reader have to agree, and the reader is the one
        // that will see this in a real font.
        let target = minimal(1);
        let source = minimal(4);
        let out = inject_cff(&target, &source, &[1, 2, 3]).expect("inject");
        let rebuilt = Cff::parse(&out.bytes).expect("parse");

        assert_eq!(rebuilt.glyph_count(), 4);
        assert_eq!(rebuilt.names.len(), 1);
        for gid in 0..4 {
            assert!(rebuilt.charstring(&out.bytes, gid).is_some(), "glyph {gid} unreachable");
        }
        // The strings the custom names needed came across too.
        assert!(!rebuilt.strings.is_empty());
    }
}
