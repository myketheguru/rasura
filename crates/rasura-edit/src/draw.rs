//! The draw-command emitter. Spec 17, Phase 6.
//!
//! > Block operations, image replace/resample, page insert/delete/reorder with
//! > navigation fix-up, **draw-command emitter**.
//!
//! Everything else in this crate rewrites content that already exists. This is
//! the one piece that produces content that never did — a new page, a new
//! paragraph, a caption under a figure — and it is deliberately the smallest
//! thing that can: a builder that appends operators to a buffer, with a `q`/`Q`
//! nesting check and nothing else.
//!
//! # Why it stays small
//!
//! The temptation is a drawing API: shapes, styles, layout, a colour type per
//! space. That is a graphics library, and PDF already has one — the operators
//! *are* the API, and every abstraction over them is a place where the bytes
//! this crate emits stop resembling the bytes a producer would have written.
//! Spec 9.4's number formatting rule points the same way: the goal is output
//! that reads like its neighbours, not output that reads like this crate.
//!
//! So this emits operators, in the producer's own number style, and leaves
//! composition to the caller.
//!
//! # The one thing it enforces
//!
//! `q` and `Q` must balance. An unbalanced `q` leaks its graphics state into
//! everything drawn after it — a fill colour, a clip, a transform — and the
//! symptom appears in unrelated content further down the page, which is the
//! hardest kind of bug to trace back. [`Canvas::finish`] refuses rather than
//! emitting one.

use crate::numfmt::NumberStyle;
use rasura_content::matrix::Matrix;
use rasura_content::op::OpKind;
use rasura_cos::object::{Name, Object, PdfString};

/// Why a drawing could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DrawError {
    /// More `Q` than `q`.
    #[error("restore with no matching save")]
    Unbalanced,
    /// `q` left open at the end.
    ///
    /// Refused rather than auto-closed: a caller that forgot a `Q` has usually
    /// forgotten *where*, and closing at the end puts content inside a state it
    /// was not meant to be in rather than fixing anything.
    #[error("{0} save(s) left open")]
    Unclosed(usize),
}

/// A content-stream fragment under construction.
pub struct Canvas {
    ops: Vec<u8>,
    style: NumberStyle,
    depth: usize,
    unbalanced: bool,
}

impl Canvas {
    /// Start a fragment that writes numbers the way `style` does.
    pub fn new(style: NumberStyle) -> Canvas {
        Canvas { ops: Vec::new(), style, depth: 0, unbalanced: false }
    }

    fn push(&mut self, kind: OpKind, operands: impl IntoIterator<Item = Object>) -> &mut Canvas {
        if !self.ops.is_empty() {
            self.ops.push(b'\n');
        }
        crate::emit::write_op(&mut self.ops, &crate::emit::op(kind, operands), &self.style);
        self
    }

    fn reals(&mut self, kind: OpKind, values: &[f64]) -> &mut Canvas {
        let operands: Vec<Object> = values.iter().copied().map(Object::Real).collect();
        self.push(kind, operands)
    }

    // --- graphics state ---------------------------------------------------

    pub fn save(&mut self) -> &mut Canvas {
        self.depth += 1;
        self.push(OpKind::Save, [])
    }

    pub fn restore(&mut self) -> &mut Canvas {
        match self.depth.checked_sub(1) {
            Some(d) => self.depth = d,
            // Recorded rather than panicking, so a caller building a fragment
            // finds out at `finish` along with everything else.
            None => self.unbalanced = true,
        }
        self.push(OpKind::Restore, [])
    }

    pub fn concat(&mut self, m: Matrix) -> &mut Canvas {
        self.reals(OpKind::Concat, &[m.a, m.b, m.c, m.d, m.e, m.f])
    }

    pub fn fill_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Canvas {
        self.reals(OpKind::SetFillRgb, &[r, g, b])
    }

    pub fn stroke_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Canvas {
        self.reals(OpKind::SetStrokeRgb, &[r, g, b])
    }

    pub fn fill_gray(&mut self, level: f64) -> &mut Canvas {
        self.reals(OpKind::SetFillGray, &[level])
    }

    pub fn line_width(&mut self, w: f64) -> &mut Canvas {
        self.reals(OpKind::SetLineWidth, &[w])
    }

    // --- paths ------------------------------------------------------------

    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Canvas {
        self.reals(OpKind::MoveTo, &[x, y])
    }

    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Canvas {
        self.reals(OpKind::LineTo, &[x, y])
    }

    pub fn curve_to(&mut self, c1: (f64, f64), c2: (f64, f64), to: (f64, f64)) -> &mut Canvas {
        self.reals(OpKind::CurveTo, &[c1.0, c1.1, c2.0, c2.1, to.0, to.1])
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) -> &mut Canvas {
        self.reals(OpKind::Rectangle, &[x, y, w, h])
    }

    pub fn close(&mut self) -> &mut Canvas {
        self.push(OpKind::ClosePath, [])
    }

    pub fn fill(&mut self) -> &mut Canvas {
        self.push(OpKind::Fill, [])
    }

    pub fn stroke(&mut self) -> &mut Canvas {
        self.push(OpKind::Stroke, [])
    }

    pub fn fill_and_stroke(&mut self) -> &mut Canvas {
        self.push(OpKind::FillStroke, [])
    }

    /// End the path without painting it — `n`.
    pub fn end_path(&mut self) -> &mut Canvas {
        self.push(OpKind::EndPath, [])
    }

    // --- text -------------------------------------------------------------

    /// One line of pre-encoded text at an absolute position.
    ///
    /// The codes are the font's own, not UTF-8: use
    /// [`Encoder`](crate::Encoder) to produce them. Taking bytes rather than a
    /// `&str` keeps this module out of the encoding question entirely — it
    /// cannot silently write the wrong glyph for a character it does not
    /// understand, because it never sees characters.
    pub fn text_line(
        &mut self,
        font: &Name,
        size: f64,
        x: f64,
        y: f64,
        codes: &[u8],
    ) -> &mut Canvas {
        self.push(OpKind::BeginText, []);
        self.push(OpKind::SetFont, [Object::Name(font.clone()), Object::Real(size)]);
        self.reals(OpKind::SetTextMatrix, &[1.0, 0.0, 0.0, 1.0, x, y]);
        self.push(OpKind::ShowText, [Object::String(PdfString::new_literal(codes))]);
        self.push(OpKind::EndText, [])
    }

    /// Several lines, each one leading below the last.
    pub fn text_block(
        &mut self,
        font: &Name,
        size: f64,
        x: f64,
        y: f64,
        leading: f64,
        lines: &[Vec<u8>],
    ) -> &mut Canvas {
        if lines.is_empty() {
            return self;
        }
        self.push(OpKind::BeginText, []);
        self.push(OpKind::SetFont, [Object::Name(font.clone()), Object::Real(size)]);
        self.reals(OpKind::SetTextMatrix, &[1.0, 0.0, 0.0, 1.0, x, y]);
        for (i, codes) in lines.iter().enumerate() {
            if i > 0 {
                self.reals(OpKind::TextMove, &[0.0, -leading]);
            }
            self.push(OpKind::ShowText, [Object::String(PdfString::new_literal(codes.clone()))]);
        }
        self.push(OpKind::EndText, [])
    }

    // --- images -----------------------------------------------------------

    /// Place an image XObject in the rectangle `(x, y, w, h)`.
    ///
    /// An image is always drawn into the unit square, so the placement *is* the
    /// transform. Wrapped in `q`/`Q` so the transform does not leak.
    pub fn image(&mut self, name: &Name, x: f64, y: f64, w: f64, h: f64) -> &mut Canvas {
        self.save();
        self.concat(Matrix::new(w, 0.0, 0.0, h, x, y));
        self.push(OpKind::InvokeXObject, [Object::Name(name.clone())]);
        self.restore()
    }

    /// Clip to the current path and discard it — `W n`.
    pub fn clip_and_end(&mut self) -> &mut Canvas {
        self.push(OpKind::Clip, []);
        self.push(OpKind::EndPath, [])
    }

    /// `BT` on its own, for callers assembling a text object piecemeal.
    pub fn begin_text(&mut self) -> &mut Canvas {
        self.push(OpKind::BeginText, [])
    }

    /// `ET`.
    pub fn end_text(&mut self) -> &mut Canvas {
        self.push(OpKind::EndText, [])
    }

    /// `x y Td`.
    pub fn text_at(&mut self, x: f64, y: f64) -> &mut Canvas {
        self.reals(OpKind::TextMove, &[x, y])
    }

    /// `(codes) Tj` with pre-encoded bytes.
    pub fn show_raw(&mut self, codes: &[u8]) -> &mut Canvas {
        self.push(OpKind::ShowText, [Object::String(PdfString::new_literal(codes))])
    }

    /// Splice operator bytes in verbatim.
    ///
    /// The escape hatch, and it exists for one case: a form field's `/DA`
    /// string is a fragment of content stream the producer wrote — a font
    /// selection and a colour, in a colour space this module does not model.
    /// Parsing and re-emitting it would silently change what it says, so it is
    /// carried through as bytes, exactly as [`crate::blocks`] carries a drawing
    /// operator through.
    ///
    /// Nothing validates the bytes. A caller passing something unbalanced gets
    /// an unbalanced stream, which is why the only caller is one splicing a
    /// value the file already contained.
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Canvas {
        if !self.ops.is_empty() {
            self.ops.push(b'\n');
        }
        self.ops.extend_from_slice(bytes);
        self
    }

    /// Invoke a form or image XObject by name, with no transform of its own.
    ///
    /// The caller is expected to have set the transform already -- flattening
    /// an annotation maps the form''s `/BBox` onto the annotation''s `/Rect`,
    /// which is a matrix only the caller can compute.
    pub fn push_xobject(&mut self, name: &Name) -> &mut Canvas {
        self.push(OpKind::InvokeXObject, [Object::Name(name.clone())])
    }

    /// Whether anything has been drawn.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The operators, if the `q`/`Q` nesting balances.
    pub fn finish(self) -> Result<Vec<u8>, DrawError> {
        if self.unbalanced {
            return Err(DrawError::Unbalanced);
        }
        if self.depth > 0 {
            return Err(DrawError::Unclosed(self.depth));
        }
        Ok(self.ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::tokenizer::tokenize;

    fn plain() -> NumberStyle {
        NumberStyle { decimals: 2, integral_keeps_point: false, leading_zero: true }
    }

    fn drawn(f: impl FnOnce(&mut Canvas)) -> String {
        let mut c = Canvas::new(plain());
        f(&mut c);
        String::from_utf8(c.finish().expect("balanced")).expect("ascii")
    }

    #[test]
    fn a_filled_rectangle_looks_like_a_content_stream() {
        let out = drawn(|c| {
            c.save().fill_rgb(1.0, 0.0, 0.0).rect(72.0, 700.0, 100.0, 20.0).fill().restore();
        });
        assert_eq!(out, "q\n1 0 0 rg\n72 700 100 20 re\nf\nQ");
    }

    #[test]
    fn everything_emitted_tokenises_back() {
        // The content layer's own reader has to accept it, and it has no stake
        // in this module being right.
        let bytes = {
            let mut c = Canvas::new(plain());
            c.save()
                .concat(Matrix::translate(10.0, 20.0))
                .stroke_rgb(0.0, 0.0, 1.0)
                .line_width(1.5)
                .move_to(0.0, 0.0)
                .line_to(50.0, 50.0)
                .curve_to((1.0, 2.0), (3.0, 4.0), (5.0, 6.0))
                .close()
                .stroke()
                .restore();
            c.text_line(&Name::new("F1"), 12.0, 72.0, 700.0, b"Hello");
            c.image(&Name::new("Im1"), 100.0, 100.0, 200.0, 150.0);
            c.finish().expect("balanced")
        };

        let (ops, leniencies) = tokenize(&bytes);
        assert!(leniencies.is_empty(), "the tokenizer tolerated nothing: {leniencies:?}");
        assert!(ops.len() >= 18, "{}", String::from_utf8_lossy(&bytes));
        assert!(ops.iter().any(|o| o.kind == OpKind::CurveTo));
        assert!(ops.iter().any(|o| o.kind == OpKind::ShowText));
        assert!(ops.iter().any(|o| o.kind == OpKind::InvokeXObject));
    }

    #[test]
    fn an_unclosed_save_is_refused() {
        // It leaks its graphics state into everything drawn after it, and the
        // symptom shows up in unrelated content further down the page.
        let mut c = Canvas::new(plain());
        c.save().rect(0.0, 0.0, 1.0, 1.0).fill();
        assert_eq!(c.finish(), Err(DrawError::Unclosed(1)));
    }

    #[test]
    fn a_stray_restore_is_refused() {
        let mut c = Canvas::new(plain());
        c.restore();
        assert_eq!(c.finish(), Err(DrawError::Unbalanced));
    }

    #[test]
    fn nested_saves_balance() {
        let out = drawn(|c| {
            c.save().save().restore().restore();
        });
        assert_eq!(out, "q\nq\nQ\nQ");
    }

    #[test]
    fn placing_an_image_wraps_its_transform() {
        let out = drawn(|c| {
            c.image(&Name::new("Im1"), 10.0, 20.0, 200.0, 100.0);
        });
        assert_eq!(out, "q\n200 0 0 100 10 20 cm\n/Im1 Do\nQ");
    }

    #[test]
    fn a_text_block_positions_each_line_below_the_last() {
        let out = drawn(|c| {
            c.text_block(
                &Name::new("F1"),
                12.0,
                72.0,
                700.0,
                14.0,
                &[b"one".to_vec(), b"two".to_vec()],
            );
        });
        assert!(out.contains("1 0 0 1 72 700 Tm"), "{out}");
        assert!(out.contains("0 -14 Td"), "{out}");
        assert_eq!(out.matches("Tj").count(), 2, "{out}");
    }

    #[test]
    fn an_empty_text_block_draws_nothing() {
        let out = drawn(|c| {
            c.text_block(&Name::new("F1"), 12.0, 0.0, 0.0, 14.0, &[]);
        });
        assert!(out.is_empty());
    }

    #[test]
    fn numbers_follow_the_style_they_were_given() {
        let style = NumberStyle { decimals: 1, integral_keeps_point: true, leading_zero: true };
        let mut c = Canvas::new(style);
        c.rect(72.0, 700.0, 10.0, 20.0).fill();
        let out = String::from_utf8(c.finish().expect("balanced")).expect("ascii");
        assert_eq!(out, "72.0 700.0 10.0 20.0 re\nf");
    }

    #[test]
    fn text_takes_codes_not_characters() {
        // The module never sees a `char`, so it cannot write the wrong glyph
        // for one. Encoding is `Encoder`'s job and stays there.
        let out = drawn(|c| {
            c.text_line(&Name::new("F1"), 12.0, 0.0, 0.0, &[0xE9]);
        });
        assert!(out.contains("(\\351)"), "the byte is written as-is: {out}");
    }
}
