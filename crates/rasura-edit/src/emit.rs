//! Writing operators back out as bytes. Spec 9.4, step 2.
//!
//! > Generate replacement operator bytes for the new content: `Tf` if the font
//! > changed, `Tm`/`Td` for each new line origin, `TJ`/`Tj` with the new glyph
//! > codes and adjustments.
//!
//! The content layer tokenises operators and never serialises them, which is
//! the right split: a reader that could write would be tempted to round-trip
//! streams through a parsed form, and §2 forbids exactly that. Nothing here
//! rewrites an operator that was already in the file. This module exists only
//! to produce operators that did not exist before, for [`crate::patch::splice`]
//! to place among the bytes that did.
//!
//! Every number goes through the stream's own [`NumberStyle`], so generated
//! operators read like their neighbours rather than like this crate.

use crate::numfmt::NumberStyle;
use rasura_content::op::{Op, OpKind};
use rasura_cos::object::{Name, Object, PdfString};

/// Serialise one operand.
///
/// Streams and references cannot appear in a content stream, so they are
/// written as `null` rather than silently skipped: a missing operand shifts
/// every operand after it into the wrong position, which is a far worse
/// failure than a visible `null`.
pub fn write_operand(out: &mut Vec<u8>, operand: &Object, style: &NumberStyle) {
    match operand {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => out.extend_from_slice(style.format(*r).as_bytes()),
        Object::Name(n) => n.write_to(out),
        Object::String(s) => s.write_to(out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                // A string next to a number needs no separator -- `(a)-30(b)`
                // is how every producer writes a TJ array, and inserting spaces
                // would inflate the stream for nothing.
                if i > 0 && needs_space(&items[i - 1], item) {
                    out.push(b' ');
                }
                write_operand(out, item, style);
            }
            out.push(b']');
        }
        Object::Dictionary(_) | Object::Stream(_) | Object::Reference(_) => {
            out.extend_from_slice(b"null")
        }
    }
}

/// Whether two adjacent operands would run together without a separator.
///
/// Delimiters are self-terminating; two bare tokens are not. `12 34` must keep
/// its space or it becomes `1234`.
fn needs_space(previous: &Object, next: &Object) -> bool {
    !self_delimiting_end(previous) && !self_delimiting_start(next)
}

fn self_delimiting_end(o: &Object) -> bool {
    matches!(o, Object::String(_) | Object::Array(_) | Object::Dictionary(_))
}

fn self_delimiting_start(o: &Object) -> bool {
    matches!(o, Object::String(_) | Object::Array(_) | Object::Dictionary(_) | Object::Name(_))
}

/// Serialise a complete operator: operands, then the keyword.
///
/// An [`OpKind::Unknown`] writes back through its `raw_keyword`, because an
/// operator this layer does not understand is still one the file's own reader
/// might, and dropping it would change the page.
pub fn write_op(out: &mut Vec<u8>, op: &Op, style: &NumberStyle) {
    for (i, operand) in op.operands.iter().enumerate() {
        if i > 0 && needs_space(&op.operands[i - 1], operand) {
            out.push(b' ');
        }
        write_operand(out, operand, style);
    }

    let keyword: &[u8] = match op.kind.keyword() {
        Some(k) => k.as_bytes(),
        None => match &op.raw_keyword {
            Some(raw) => raw,
            // No keyword and no raw bytes is not an operator. Writing nothing
            // is the only answer that does not corrupt the stream.
            None => return,
        },
    };
    // A separator only where one is needed. `[...]TJ` and `(x)Tj` are how every
    // producer writes them, and `12 Tf` cannot lose its space.
    if op.operands.last().is_some_and(|last| !self_delimiting_end(last)) {
        out.push(b' ');
    }
    out.extend_from_slice(keyword);
}

/// Serialise a run of operators, one per line.
pub fn write_ops(ops: &[Op], style: &NumberStyle) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        write_op(&mut out, op, style);
    }
    out
}

// --- the text operators reflow generates ---------------------------------

/// Build an operator that came from nowhere.
///
/// `Op::span` addresses the buffer an operator was *read* from, and these were
/// not read from anything — they are about to be written into a buffer whose
/// layout is not decided until [`crate::patch::splice`] runs. An empty span
/// says that honestly. Nothing consumes the span of a generated operator; the
/// placement that matters comes back from [`crate::patch::Placement`].
pub fn op(kind: OpKind, operands: impl IntoIterator<Item = Object>) -> Op {
    Op::new(kind, operands.into_iter().collect(), 0..0)
}

/// `/name size Tf`
pub fn set_font(name: &Name, size: f64) -> Op {
    op(OpKind::SetFont, [Object::Name(name.clone()), Object::Real(size)])
}

/// `a b c d e f Tm`
pub fn set_text_matrix(m: [f64; 6]) -> Op {
    op(OpKind::SetTextMatrix, m.map(Object::Real))
}

/// `tx ty Td`
pub fn text_move(tx: f64, ty: f64) -> Op {
    op(OpKind::TextMove, [Object::Real(tx), Object::Real(ty)])
}

/// `(string) Tj`
pub fn show_text(codes: &[u8]) -> Op {
    op(OpKind::ShowText, [Object::String(PdfString::new_literal(codes))])
}

/// One element of a `TJ` array: a string of codes, or a position adjustment.
///
/// The adjustment is in thousandths of an em and **subtracts** from the
/// displacement — a positive number moves the next glyph left. The sign trips
/// everyone once; it is the spec's, not a choice made here.
#[derive(Debug, Clone, PartialEq)]
pub enum Adjusted {
    Codes(Vec<u8>),
    Adjust(f64),
}

/// `[(a) -30 (b)] TJ`
pub fn show_text_adjusted(items: &[Adjusted]) -> Op {
    let array: Vec<Object> = items
        .iter()
        .map(|item| match item {
            Adjusted::Codes(c) => Object::String(PdfString::new_literal(c.clone())),
            Adjusted::Adjust(v) => Object::Real(*v),
        })
        .collect();
    op(OpKind::ShowTextAdjusted, [Object::Array(array)])
}

/// `BT` / `ET`
pub fn begin_text() -> Op {
    op(OpKind::BeginText, [])
}

pub fn end_text() -> Op {
    op(OpKind::EndText, [])
}

/// `value Tw`, `value Tc`, `value Tz`, `value TL`
pub fn set_word_spacing(v: f64) -> Op {
    op(OpKind::SetWordSpacing, [Object::Real(v)])
}

pub fn set_char_spacing(v: f64) -> Op {
    op(OpKind::SetCharSpacing, [Object::Real(v)])
}

pub fn set_horizontal_scale(v: f64) -> Op {
    op(OpKind::SetHorizontalScale, [Object::Real(v)])
}

pub fn set_leading(v: f64) -> Op {
    op(OpKind::SetLeading, [Object::Real(v)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::tokenizer::tokenize;

    fn plain() -> NumberStyle {
        NumberStyle { decimals: 2, integral_keeps_point: false, leading_zero: true }
    }

    fn emit(op: &Op) -> String {
        let mut out = Vec::new();
        write_op(&mut out, op, &plain());
        String::from_utf8(out).expect("ascii")
    }

    #[test]
    fn the_text_operators_look_like_a_content_stream() {
        assert_eq!(emit(&set_font(&Name::new("F1"), 12.0)), "/F1 12 Tf");
        assert_eq!(emit(&text_move(0.0, -14.5)), "0 -14.50 Td");
        assert_eq!(emit(&show_text(b"Hello")), "(Hello)Tj");
        assert_eq!(emit(&begin_text()), "BT");
        assert_eq!(emit(&set_text_matrix([1.0, 0.0, 0.0, 1.0, 72.0, 700.0])), "1 0 0 1 72 700 Tm");
    }

    #[test]
    fn a_tj_array_writes_the_way_producers_write_it() {
        let tj = show_text_adjusted(&[
            Adjusted::Codes(b"Hello".to_vec()),
            Adjusted::Adjust(-333.0),
            Adjusted::Codes(b"world".to_vec()),
        ]);
        assert_eq!(emit(&tj), "[(Hello)-333(world)]TJ");
    }

    #[test]
    fn adjacent_numbers_keep_their_separator() {
        // The failure this guards is silent and total: `12 34` written as
        // `1234` is still a valid stream, just one that says something else.
        let matrix = op(
            OpKind::SetTextMatrix,
            [
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
                Object::Integer(12),
                Object::Integer(34),
            ],
        );
        assert_eq!(emit(&matrix), "1 0 0 1 12 34 Tm");
    }

    #[test]
    fn everything_emitted_tokenises_back_to_what_it_was() {
        // The real test: the content layer's own reader has to agree. It has no
        // stake in this module being right.
        let ops = vec![
            begin_text(),
            set_font(&Name::new("F1"), 9.96),
            set_text_matrix([1.0, 0.0, 0.0, 1.0, 133.77, 662.83]),
            show_text_adjusted(&[
                Adjusted::Codes(b"Hello".to_vec()),
                Adjusted::Adjust(-333.0),
                Adjusted::Codes(b"world".to_vec()),
            ]),
            text_move(0.0, -11.95),
            show_text(b"again"),
            end_text(),
        ];
        let bytes = write_ops(&ops, &plain());
        let (back, leniencies) = tokenize(&bytes);

        assert!(leniencies.is_empty(), "the tokenizer tolerated nothing: {leniencies:?}");
        assert_eq!(back.len(), ops.len(), "{}", String::from_utf8_lossy(&bytes));
        for (a, b) in ops.iter().zip(&back) {
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.operands.len(), b.operands.len(), "{:?}", a.kind);
        }
    }

    #[test]
    fn a_string_with_delimiters_survives_the_round_trip() {
        // Text is arbitrary bytes. A paren or a backslash in the codes must not
        // end the string early, and the cos layer's escaping is what prevents
        // it -- this asserts it is actually being used.
        let awkward: &[u8] = b"a(b)c\\d\re\nf";
        let bytes = write_ops(&[show_text(awkward)], &plain());
        let (back, _) = tokenize(&bytes);
        assert_eq!(back.len(), 1);
        let Some(Object::String(s)) = back[0].operands.first() else {
            panic!("expected a string operand: {:?}", back[0].operands)
        };
        assert_eq!(s.as_bytes(), awkward);
    }

    #[test]
    fn generated_numbers_follow_the_streams_own_style() {
        let style = NumberStyle { decimals: 1, integral_keeps_point: true, leading_zero: true };
        let mut out = Vec::new();
        write_op(&mut out, &set_text_matrix([1.0, 0.0, 0.0, 1.0, 72.0, 700.0]), &style);
        assert_eq!(String::from_utf8(out).unwrap(), "1.0 0.0 0.0 1.0 72.0 700.0 Tm");
    }

    #[test]
    fn an_unknown_operator_writes_back_through_its_raw_keyword() {
        // Round-tripping an operator this layer does not model is the whole
        // reason `raw_keyword` exists. Dropping it would change the page.
        let (ops, _) = tokenize(b"1 2 zzz");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Unknown);
        assert_eq!(emit(&ops[0]), "1 2 zzz");
    }
}
