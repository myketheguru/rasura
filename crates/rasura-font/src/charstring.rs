//! Type 2 charstrings: walking them, and inlining subroutines. Spec 8.4.
//!
//! > Extract and re-encode the Type 2 charstring, resolving local and global
//! > subroutines from the source font (inline them — do not attempt to merge
//! > subr indexes).
//!
//! Inlining rather than merging is the right call and worth stating: two fonts'
//! subroutine indexes are independent, so merging them would renumber every
//! `callsubr` in the target — the same non-local rewrite §2 forbids elsewhere.
//! Inlining costs a few bytes per glyph and touches nothing.
//!
//! # `hintmask` is why this needs a real walker
//!
//! Almost every Type 2 operator has a fixed length, so a naive scan gets most
//! of a charstring right. `hintmask` and `cntrmask` do not: they are followed
//! by *one bit per stem hint declared so far*, rounded up to whole bytes. To
//! know how many bytes to skip you must have counted the arguments to every
//! `hstem`, `vstem`, `hstemhm` and `vstemhm` — and the implicit `vstem` that
//! `hintmask` itself performs when arguments are pending.
//!
//! Miscount by one and the walk desynchronises: mask bytes get read as
//! operators, and a `callsubr` a few bytes later inlines the wrong subroutine.
//! The result is a glyph that draws something plausible and wrong.

use crate::cff::Index;
use crate::error::{FontError, Result};

/// How deep subroutine calls may nest. The Type 2 spec's own limit is 10.
const MAX_DEPTH: usize = 10;

/// The bias added to a subroutine number, which depends on how many there are.
///
/// Type 2 numbers subroutines from the middle outwards so the common ones fit
/// in a single-byte operand. Type 1 has no bias at all, and applying this to a
/// Type 1 font looks up the wrong subroutine every time.
pub fn bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

/// One token of a charstring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token {
    /// A numeric operand, with its value.
    Operand(f64),
    /// An operator, with its code (`12 x` escapes are `0x0c00 | x`).
    Operator(u16),
    /// `hintmask` or `cntrmask` and its mask bytes, which are data not code.
    Mask { operator: u16, bytes: usize },
}

/// Walk a charstring, yielding `(byte offset, byte length, token)`.
///
/// Tracks the stem count so `hintmask` and `cntrmask` are measured correctly.
/// Stops at the end of the data or on a byte that cannot begin a token.
pub fn tokens(data: &[u8]) -> Vec<(usize, usize, Token)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut stems = 0usize;
    let mut pending = 0usize;

    while at < data.len() {
        let b = data[at];
        match b {
            // Stem hints: each takes a pair of arguments.
            1 | 3 | 18 | 23 => {
                stems += pending / 2;
                pending = 0;
                out.push((at, 1, Token::Operator(b as u16)));
                at += 1;
            }
            // hintmask/cntrmask. Any pending arguments are an implicit vstem,
            // which is the clause most implementations forget.
            19 | 20 => {
                stems += pending / 2;
                pending = 0;
                let mask = stems.div_ceil(8).max(1);
                out.push((at, 1 + mask, Token::Mask { operator: b as u16, bytes: mask }));
                at += 1 + mask;
            }
            12 => {
                let Some(&b1) = data.get(at + 1) else { return out };
                pending = 0;
                out.push((at, 2, Token::Operator(0x0c00 | b1 as u16)));
                at += 2;
            }
            // 28 is a 16-bit operand, not an operator, and it sits inside the
            // operator range -- so it has to be matched *before* the catch-all
            // below. Ordered the other way it reads as an operator and every
            // token after it is off by two bytes.
            28 => {
                let Some(v) = data.get(at + 1..at + 3) else { return out };
                pending += 1;
                out.push((at, 3, Token::Operand(i16::from_be_bytes([v[0], v[1]]) as f64)));
                at += 3;
            }
            // Every other operator.
            0..=31 => {
                pending = 0;
                out.push((at, 1, Token::Operator(b as u16)));
                at += 1;
            }
            32..=246 => {
                pending += 1;
                out.push((at, 1, Token::Operand(b as f64 - 139.0)));
                at += 1;
            }
            247..=250 => {
                let Some(&b1) = data.get(at + 1) else { return out };
                pending += 1;
                out.push((at, 2, Token::Operand((b as f64 - 247.0) * 256.0 + b1 as f64 + 108.0)));
                at += 2;
            }
            251..=254 => {
                let Some(&b1) = data.get(at + 1) else { return out };
                pending += 1;
                out.push((at, 2, Token::Operand(-(b as f64 - 251.0) * 256.0 - b1 as f64 - 108.0)));
                at += 2;
            }
            255 => {
                let Some(v) = data.get(at + 1..at + 5) else { return out };
                pending += 1;
                // 16.16 fixed point.
                let raw = i32::from_be_bytes([v[0], v[1], v[2], v[3]]);
                out.push((at, 5, Token::Operand(raw as f64 / 65536.0)));
                at += 5;
            }
        }
    }
    out
}

/// Rewrite a charstring with every `callsubr` and `callgsubr` replaced by the
/// subroutine's body.
///
/// The result depends on no subroutine index and can be dropped into any CFF.
pub fn inline_subrs(
    data: &[u8],
    charstring: &[u8],
    local: &Index,
    global: &Index,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(charstring.len() * 2);
    expand(data, charstring, local, global, 0, &mut out)?;
    Ok(out)
}

fn expand(
    data: &[u8],
    cs: &[u8],
    local: &Index,
    global: &Index,
    depth: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(FontError::Malformed("charstring subroutines nest too deeply"));
    }

    let toks = tokens(cs);
    // Where in `out` the most recent operand token was written, so a subroutine
    // number can be removed when the call that consumes it is inlined.
    let mut last_operand: Option<(usize, f64)> = None;

    for (offset, len, token) in &toks {
        match token {
            Token::Operand(v) => {
                last_operand = Some((out.len(), *v));
                out.extend_from_slice(&cs[*offset..offset + len]);
            }

            // `return` ends a subroutine. Inlining splices the body in place,
            // so the return is dropped rather than copied -- a `return` in the
            // middle of a charstring would end it early.
            Token::Operator(11) => return Ok(()),

            Token::Operator(op @ (10 | 29)) => {
                let (start, number) =
                    last_operand.take().ok_or(FontError::Malformed("callsubr with no operand"))?;
                // The pushed number belongs to the call, not to the glyph.
                out.truncate(start);

                let subrs = if *op == 10 { local } else { global };
                let biased = number as i32 + bias(subrs.len());
                let sub = usize::try_from(biased)
                    .ok()
                    .and_then(|i| subrs.get(data, i))
                    .ok_or(FontError::Malformed("callsubr to a subroutine that is not there"))?;

                expand(data, sub, local, global, depth + 1, out)?;
            }

            // Everything else, including hintmask and its data bytes, is copied
            // verbatim.
            _ => {
                last_operand = None;
                out.extend_from_slice(&cs[*offset..offset + len]);
            }
        }
    }
    Ok(())
}

/// Whether a charstring still calls a subroutine.
///
/// Used to check that inlining actually finished: a charstring that reaches a
/// CFF with a `callsubr` still in it will index that CFF's subroutines, which
/// are a different font's.
pub fn calls_subroutine(charstring: &[u8]) -> bool {
    tokens(charstring)
        .iter()
        .any(|(_, _, t)| matches!(t, Token::Operator(10) | Token::Operator(29)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a small integer operand the way a charstring does.
    fn num(v: i32) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![(v + 139) as u8]
        } else {
            let mut out = vec![28];
            out.extend_from_slice(&(v as i16).to_be_bytes());
            out
        }
    }

    /// Build an INDEX over the given entries, returning it and the whole buffer
    /// so offsets resolve.
    fn index_of(entries: &[Vec<u8>]) -> (Vec<u8>, Index) {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        if entries.is_empty() {
            return (buf.clone(), Index { items: Vec::new(), end: 2 });
        }
        buf.push(4); // offSize
        let mut offset = 1u32;
        buf.extend_from_slice(&offset.to_be_bytes());
        for e in entries {
            offset += e.len() as u32;
            buf.extend_from_slice(&offset.to_be_bytes());
        }
        let base = buf.len() - 1;
        let mut items = Vec::new();
        let mut cursor = base + 1;
        for e in entries {
            items.push((cursor, cursor + e.len()));
            cursor += e.len();
            buf.extend_from_slice(e);
        }
        (buf, Index { items, end: cursor })
    }

    #[test]
    fn operands_decode_in_every_form() {
        let mut cs = Vec::new();
        cs.extend(num(0)); // 139
        cs.extend(num(100));
        cs.extend(num(-100));
        cs.extend_from_slice(&[247, 0]); // 108
        cs.extend_from_slice(&[251, 0]); // -108
        cs.extend_from_slice(&[28, 0x01, 0x00]); // 256
        cs.extend_from_slice(&[255, 0x00, 0x01, 0x80, 0x00]); // 1.5
        cs.push(14); // endchar

        let values: Vec<f64> = tokens(&cs)
            .iter()
            .filter_map(|(_, _, t)| match t {
                Token::Operand(v) => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![0.0, 100.0, -100.0, 108.0, -108.0, 256.0, 1.5]);
    }

    #[test]
    fn hintmask_bytes_follow_the_stem_count() {
        // Four stems -> one mask byte; nine -> two. Miscounting desynchronises
        // the walk and every later token is nonsense.
        for (pairs, expect) in [(1usize, 1usize), (4, 1), (5, 1), (8, 1), (9, 2), (17, 3)] {
            let mut cs = Vec::new();
            for _ in 0..pairs * 2 {
                cs.extend(num(10));
            }
            cs.push(1); // hstem
            cs.push(19); // hintmask
            cs.extend(std::iter::repeat_n(0xFF, expect));
            cs.push(14); // endchar

            let toks = tokens(&cs);
            let mask = toks
                .iter()
                .find_map(|(_, _, t)| match t {
                    Token::Mask { bytes, .. } => Some(*bytes),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no mask for {pairs} stems"));
            assert_eq!(mask, expect, "{pairs} stems");
            // And the walk stayed in step: endchar is the last token.
            assert_eq!(toks.last().map(|(_, _, t)| *t), Some(Token::Operator(14)));
        }
    }

    #[test]
    fn hintmask_counts_its_own_implicit_vstem() {
        // Arguments pending when hintmask appears are an implicit vstem. Two
        // stems from hstem plus two more here is four -- still one mask byte,
        // but the clause matters at the eight-stem boundary.
        let mut cs = Vec::new();
        for _ in 0..16 {
            cs.extend(num(10)); // eight stems' worth
        }
        cs.push(1); // hstem -> 8 stems
        cs.extend(num(10));
        cs.extend(num(20)); // one more pending pair
        cs.push(19); // hintmask -> implicit vstem, 9 stems -> 2 bytes
        cs.extend_from_slice(&[0xFF, 0xFF]);
        cs.push(14);

        let toks = tokens(&cs);
        let mask = toks.iter().find_map(|(_, _, t)| match t {
            Token::Mask { bytes, .. } => Some(*bytes),
            _ => None,
        });
        assert_eq!(mask, Some(2), "the implicit vstem was not counted");
        assert_eq!(toks.last().map(|(_, _, t)| *t), Some(Token::Operator(14)));
    }

    #[test]
    fn the_bias_follows_the_subroutine_count() {
        // Type 2 numbers subroutines from the middle outwards. Type 1 has no
        // bias at all, and applying this there looks up the wrong one.
        assert_eq!(bias(0), 107);
        assert_eq!(bias(1239), 107);
        assert_eq!(bias(1240), 1131);
        assert_eq!(bias(33899), 1131);
        assert_eq!(bias(33900), 32768);
    }

    #[test]
    fn a_local_subroutine_is_inlined_and_its_number_removed() {
        // subr 0 draws something and returns.
        let body = {
            let mut b = num(50);
            b.extend(num(60));
            b.push(21); // rmoveto
            b.push(11); // return
            b
        };
        let (data, local) = index_of(&[body]);
        let (_, global) = index_of(&[]);

        // The charstring calls it: `-107 callsubr` reaches index 0 at bias 107.
        let mut cs = num(0 - 107);
        cs.push(10);
        cs.push(14);

        let out = inline_subrs(&data, &cs, &local, &global).expect("inline");
        assert!(!calls_subroutine(&out), "no call survives: {out:?}");

        let ops: Vec<Token> = tokens(&out).iter().map(|(_, _, t)| *t).collect();
        assert_eq!(
            ops,
            vec![
                Token::Operand(50.0),
                Token::Operand(60.0),
                Token::Operator(21),
                Token::Operator(14),
            ],
            "the body was spliced and the return dropped"
        );
    }

    #[test]
    fn a_global_subroutine_is_inlined_from_its_own_index() {
        let (gdata, global) = index_of(&[{
            let mut b = num(7);
            b.push(11);
            b
        }]);
        let (_, local) = index_of(&[]);

        let mut cs = num(0 - 107);
        cs.push(29); // callgsubr
        cs.push(14);

        let out = inline_subrs(&gdata, &cs, &local, &global).unwrap();
        let ops: Vec<Token> = tokens(&out).iter().map(|(_, _, t)| *t).collect();
        assert_eq!(ops, vec![Token::Operand(7.0), Token::Operator(14)]);
    }

    #[test]
    fn nested_subroutines_are_inlined_transitively() {
        // subr 1 calls subr 0.
        let inner = {
            let mut b = num(9);
            b.push(11);
            b
        };
        let outer = {
            let mut b = num(0 - 107);
            b.push(10); // callsubr -> subr 0
            b.extend(num(8));
            b.push(11);
            b
        };
        let (data, local) = index_of(&[inner, outer]);
        let (_, global) = index_of(&[]);

        let mut cs = num(1 - 107);
        cs.push(10);
        cs.push(14);

        let out = inline_subrs(&data, &cs, &local, &global).unwrap();
        assert!(!calls_subroutine(&out));
        let values: Vec<f64> = tokens(&out)
            .iter()
            .filter_map(|(_, _, t)| match t {
                Token::Operand(v) => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![9.0, 8.0], "inner first, then the rest of outer");
    }

    #[test]
    fn a_hintmask_inside_a_subroutine_survives_intact() {
        // The mask bytes are data. Copied as operators they would be read as a
        // `callsubr` sooner or later.
        let body = {
            let mut b = num(10);
            b.extend(num(20));
            b.push(1); // hstem -> 1 stem
            b.push(19); // hintmask
            b.push(0b1000_0000);
            b.push(11);
            b
        };
        let (data, local) = index_of(&[body]);
        let (_, global) = index_of(&[]);

        let mut cs = num(0 - 107);
        cs.push(10);
        cs.push(14);

        let out = inline_subrs(&data, &cs, &local, &global).unwrap();
        let masks: Vec<Token> = tokens(&out)
            .iter()
            .map(|(_, _, t)| *t)
            .filter(|t| matches!(t, Token::Mask { .. }))
            .collect();
        assert_eq!(masks, vec![Token::Mask { operator: 19, bytes: 1 }]);
        assert_eq!(tokens(&out).last().map(|(_, _, t)| *t), Some(Token::Operator(14)));
    }

    #[test]
    fn a_missing_subroutine_is_an_error_not_a_silent_skip() {
        let (data, local) = index_of(&[]);
        let (_, global) = index_of(&[]);
        let mut cs = num(5);
        cs.push(10);
        cs.push(14);
        assert!(inline_subrs(&data, &cs, &local, &global).is_err());
    }

    #[test]
    fn a_call_with_no_operand_is_an_error() {
        let (data, local) = index_of(&[vec![11]]);
        let (_, global) = index_of(&[]);
        assert!(inline_subrs(&data, &[10, 14], &local, &global).is_err());
    }

    #[test]
    fn a_recursive_subroutine_terminates() {
        // subr 0 calls itself.
        let body = {
            let mut b = num(0 - 107);
            b.push(10);
            b.push(11);
            b
        };
        let (data, local) = index_of(&[body]);
        let (_, global) = index_of(&[]);
        let mut cs = num(0 - 107);
        cs.push(10);
        assert!(inline_subrs(&data, &cs, &local, &global).is_err(), "depth limit");
    }

    #[test]
    fn a_charstring_with_no_calls_is_unchanged() {
        let mut cs = num(10);
        cs.extend(num(20));
        cs.push(21);
        cs.push(14);
        let (data, local) = index_of(&[]);
        let (_, global) = index_of(&[]);
        assert_eq!(inline_subrs(&data, &cs, &local, &global).unwrap(), cs);
    }

    #[test]
    fn a_truncated_charstring_does_not_panic() {
        for cs in [vec![28], vec![255, 0], vec![247], vec![12], vec![19]] {
            let _ = tokens(&cs);
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0xABCDEF01u32;
        for _ in 0..2000 {
            let mut cs = Vec::new();
            for _ in 0..48 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                cs.push((seed >> 24) as u8);
            }
            let _ = tokens(&cs);
            let _ = calls_subroutine(&cs);
        }
    }
}
