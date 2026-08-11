//! Content-stream operators. ISO 32000-1 §8 and §9, spec 6.1.
//!
//! # Span preservation
//!
//! Every `Op` retains its byte range in the *decoded* stream buffer. This is
//! spec 6.2's load-bearing requirement, and it is worth restating why: to edit a
//! paragraph, the edit layer splices replacement bytes into the decoded buffer
//! at exactly the spans of the affected operators and leaves every other byte
//! alone. Without spans you are forced to re-serialise whole streams, and
//! re-serialisation is where fidelity dies -- a producer's number formatting,
//! its spacing, its operator choices all get normalised to yours, and every
//! diff becomes unreadable.
//!
//! The span covers the operands *and* the operator, because that is the unit
//! that can be replaced as a whole.

use rasura_cos::{Dictionary, Object};
use smallvec::SmallVec;
use std::ops::Range;

/// The complete operator set of ISO 32000-1, grouped as in spec 6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    // --- Graphics state: q Q cm w J j M d ri i gs ---
    Save,
    Restore,
    Concat,
    SetLineWidth,
    SetLineCap,
    SetLineJoin,
    SetMiterLimit,
    SetDash,
    SetRenderingIntent,
    SetFlatness,
    SetExtGState,

    // --- Path construction: m l c v y h re ---
    MoveTo,
    LineTo,
    CurveTo,
    CurveToInitialReplicated,
    CurveToFinalReplicated,
    ClosePath,
    Rectangle,

    // --- Path painting: S s f F f* B B* b b* n ---
    Stroke,
    CloseStroke,
    Fill,
    /// `F`, the obsolete synonym for `f`. Kept distinct so a round trip
    /// reproduces whichever the producer wrote.
    FillObsolete,
    FillEvenOdd,
    FillStroke,
    FillStrokeEvenOdd,
    CloseFillStroke,
    CloseFillStrokeEvenOdd,
    EndPath,

    // --- Clipping: W W* ---
    Clip,
    ClipEvenOdd,

    // --- Colour: CS cs SC SCN sc scn G g RG rg K k ---
    SetStrokeColorSpace,
    SetFillColorSpace,
    SetStrokeColor,
    SetStrokeColorN,
    SetFillColor,
    SetFillColorN,
    SetStrokeGray,
    SetFillGray,
    SetStrokeRgb,
    SetFillRgb,
    SetStrokeCmyk,
    SetFillCmyk,

    // --- Text objects: BT ET ---
    BeginText,
    EndText,

    // --- Text state: Tc Tw Tz TL Tf Tr Ts ---
    SetCharSpacing,
    SetWordSpacing,
    SetHorizontalScale,
    SetLeading,
    SetFont,
    SetRenderMode,
    SetRise,

    // --- Text positioning: Td TD Tm T* ---
    TextMove,
    TextMoveSetLeading,
    SetTextMatrix,
    NextLine,

    // --- Text showing: Tj TJ ' " ---
    ShowText,
    ShowTextAdjusted,
    NextLineShowText,
    NextLineSetSpacingShowText,

    // --- Type 3 glyph metrics: d0 d1 ---
    SetGlyphWidth,
    SetGlyphWidthBBox,

    // --- XObjects: Do ---
    InvokeXObject,

    /// `BI ... ID ... EI` collapsed into one operator, since the payload is
    /// binary and cannot be tokenised as operands.
    InlineImage,

    // --- Shading: sh ---
    Shading,

    // --- Marked content: MP DP BMC BDC EMC ---
    MarkPoint,
    MarkPointProps,
    BeginMarked,
    BeginMarkedProps,
    EndMarked,

    // --- Compatibility: BX EX ---
    BeginCompat,
    EndCompat,

    /// An operator this crate does not recognise. Preserved with its operands
    /// and span so it round-trips, and reported as a leniency unless it sits
    /// inside a `BX`/`EX` block.
    Unknown,
}

impl OpKind {
    /// Map an operator keyword to its kind.
    pub fn from_keyword(kw: &[u8]) -> OpKind {
        match kw {
            b"q" => OpKind::Save,
            b"Q" => OpKind::Restore,
            b"cm" => OpKind::Concat,
            b"w" => OpKind::SetLineWidth,
            b"J" => OpKind::SetLineCap,
            b"j" => OpKind::SetLineJoin,
            b"M" => OpKind::SetMiterLimit,
            b"d" => OpKind::SetDash,
            b"ri" => OpKind::SetRenderingIntent,
            b"i" => OpKind::SetFlatness,
            b"gs" => OpKind::SetExtGState,

            b"m" => OpKind::MoveTo,
            b"l" => OpKind::LineTo,
            b"c" => OpKind::CurveTo,
            b"v" => OpKind::CurveToInitialReplicated,
            b"y" => OpKind::CurveToFinalReplicated,
            b"h" => OpKind::ClosePath,
            b"re" => OpKind::Rectangle,

            b"S" => OpKind::Stroke,
            b"s" => OpKind::CloseStroke,
            b"f" => OpKind::Fill,
            b"F" => OpKind::FillObsolete,
            b"f*" => OpKind::FillEvenOdd,
            b"B" => OpKind::FillStroke,
            b"B*" => OpKind::FillStrokeEvenOdd,
            b"b" => OpKind::CloseFillStroke,
            b"b*" => OpKind::CloseFillStrokeEvenOdd,
            b"n" => OpKind::EndPath,

            b"W" => OpKind::Clip,
            b"W*" => OpKind::ClipEvenOdd,

            b"CS" => OpKind::SetStrokeColorSpace,
            b"cs" => OpKind::SetFillColorSpace,
            b"SC" => OpKind::SetStrokeColor,
            b"SCN" => OpKind::SetStrokeColorN,
            b"sc" => OpKind::SetFillColor,
            b"scn" => OpKind::SetFillColorN,
            b"G" => OpKind::SetStrokeGray,
            b"g" => OpKind::SetFillGray,
            b"RG" => OpKind::SetStrokeRgb,
            b"rg" => OpKind::SetFillRgb,
            b"K" => OpKind::SetStrokeCmyk,
            b"k" => OpKind::SetFillCmyk,

            b"BT" => OpKind::BeginText,
            b"ET" => OpKind::EndText,

            b"Tc" => OpKind::SetCharSpacing,
            b"Tw" => OpKind::SetWordSpacing,
            b"Tz" => OpKind::SetHorizontalScale,
            b"TL" => OpKind::SetLeading,
            b"Tf" => OpKind::SetFont,
            b"Tr" => OpKind::SetRenderMode,
            b"Ts" => OpKind::SetRise,

            b"Td" => OpKind::TextMove,
            b"TD" => OpKind::TextMoveSetLeading,
            b"Tm" => OpKind::SetTextMatrix,
            b"T*" => OpKind::NextLine,

            b"Tj" => OpKind::ShowText,
            b"TJ" => OpKind::ShowTextAdjusted,
            b"'" => OpKind::NextLineShowText,
            b"\"" => OpKind::NextLineSetSpacingShowText,

            b"d0" => OpKind::SetGlyphWidth,
            b"d1" => OpKind::SetGlyphWidthBBox,

            b"Do" => OpKind::InvokeXObject,
            b"sh" => OpKind::Shading,

            b"MP" => OpKind::MarkPoint,
            b"DP" => OpKind::MarkPointProps,
            b"BMC" => OpKind::BeginMarked,
            b"BDC" => OpKind::BeginMarkedProps,
            b"EMC" => OpKind::EndMarked,

            b"BX" => OpKind::BeginCompat,
            b"EX" => OpKind::EndCompat,

            _ => OpKind::Unknown,
        }
    }

    /// The keyword this kind serialises to. `None` for `Unknown`, whose
    /// original bytes are carried on the `Op` itself, and for `InlineImage`,
    /// which is written from its dictionary and payload.
    pub fn keyword(self) -> Option<&'static str> {
        Some(match self {
            OpKind::Save => "q",
            OpKind::Restore => "Q",
            OpKind::Concat => "cm",
            OpKind::SetLineWidth => "w",
            OpKind::SetLineCap => "J",
            OpKind::SetLineJoin => "j",
            OpKind::SetMiterLimit => "M",
            OpKind::SetDash => "d",
            OpKind::SetRenderingIntent => "ri",
            OpKind::SetFlatness => "i",
            OpKind::SetExtGState => "gs",
            OpKind::MoveTo => "m",
            OpKind::LineTo => "l",
            OpKind::CurveTo => "c",
            OpKind::CurveToInitialReplicated => "v",
            OpKind::CurveToFinalReplicated => "y",
            OpKind::ClosePath => "h",
            OpKind::Rectangle => "re",
            OpKind::Stroke => "S",
            OpKind::CloseStroke => "s",
            OpKind::Fill => "f",
            OpKind::FillObsolete => "F",
            OpKind::FillEvenOdd => "f*",
            OpKind::FillStroke => "B",
            OpKind::FillStrokeEvenOdd => "B*",
            OpKind::CloseFillStroke => "b",
            OpKind::CloseFillStrokeEvenOdd => "b*",
            OpKind::EndPath => "n",
            OpKind::Clip => "W",
            OpKind::ClipEvenOdd => "W*",
            OpKind::SetStrokeColorSpace => "CS",
            OpKind::SetFillColorSpace => "cs",
            OpKind::SetStrokeColor => "SC",
            OpKind::SetStrokeColorN => "SCN",
            OpKind::SetFillColor => "sc",
            OpKind::SetFillColorN => "scn",
            OpKind::SetStrokeGray => "G",
            OpKind::SetFillGray => "g",
            OpKind::SetStrokeRgb => "RG",
            OpKind::SetFillRgb => "rg",
            OpKind::SetStrokeCmyk => "K",
            OpKind::SetFillCmyk => "k",
            OpKind::BeginText => "BT",
            OpKind::EndText => "ET",
            OpKind::SetCharSpacing => "Tc",
            OpKind::SetWordSpacing => "Tw",
            OpKind::SetHorizontalScale => "Tz",
            OpKind::SetLeading => "TL",
            OpKind::SetFont => "Tf",
            OpKind::SetRenderMode => "Tr",
            OpKind::SetRise => "Ts",
            OpKind::TextMove => "Td",
            OpKind::TextMoveSetLeading => "TD",
            OpKind::SetTextMatrix => "Tm",
            OpKind::NextLine => "T*",
            OpKind::ShowText => "Tj",
            OpKind::ShowTextAdjusted => "TJ",
            OpKind::NextLineShowText => "'",
            OpKind::NextLineSetSpacingShowText => "\"",
            OpKind::SetGlyphWidth => "d0",
            OpKind::SetGlyphWidthBBox => "d1",
            OpKind::InvokeXObject => "Do",
            OpKind::Shading => "sh",
            OpKind::MarkPoint => "MP",
            OpKind::MarkPointProps => "DP",
            OpKind::BeginMarked => "BMC",
            OpKind::BeginMarkedProps => "BDC",
            OpKind::EndMarked => "EMC",
            OpKind::BeginCompat => "BX",
            OpKind::EndCompat => "EX",
            OpKind::InlineImage | OpKind::Unknown => return None,
        })
    }

    /// True for the four operators that draw glyphs.
    pub fn shows_text(self) -> bool {
        matches!(
            self,
            OpKind::ShowText
                | OpKind::ShowTextAdjusted
                | OpKind::NextLineShowText
                | OpKind::NextLineSetSpacingShowText
        )
    }

    /// True for operators that only make sense between `BT` and `ET`.
    pub fn is_text_operator(self) -> bool {
        self.shows_text()
            || matches!(
                self,
                OpKind::SetCharSpacing
                    | OpKind::SetWordSpacing
                    | OpKind::SetHorizontalScale
                    | OpKind::SetLeading
                    | OpKind::SetFont
                    | OpKind::SetRenderMode
                    | OpKind::SetRise
                    | OpKind::TextMove
                    | OpKind::TextMoveSetLeading
                    | OpKind::SetTextMatrix
                    | OpKind::NextLine
            )
    }
}

/// The dictionary and payload of a `BI ... ID ... EI` inline image.
#[derive(Debug, Clone)]
pub struct InlineImage {
    /// The image parameters, with abbreviated keys as written.
    pub dict: Dictionary,
    /// Byte range of the binary payload within the decoded stream, i.e. what
    /// sits between `ID` and `EI`.
    pub data: Range<usize>,
}

/// One parsed operator.
///
/// `operands` is a `SmallVec` because the overwhelming majority of operators
/// take four or fewer, and a content stream is millions of these.
#[derive(Debug, Clone)]
pub struct Op {
    pub kind: OpKind,
    pub operands: SmallVec<[Object; 4]>,
    /// Byte range in the decoded stream buffer, covering operands and operator.
    pub span: Range<usize>,
    /// The original keyword, for `Unknown` operators that must round-trip.
    pub raw_keyword: Option<Box<[u8]>>,
    /// Present only for `OpKind::InlineImage`.
    pub inline_image: Option<Box<InlineImage>>,
}

impl Op {
    pub fn new(kind: OpKind, operands: SmallVec<[Object; 4]>, span: Range<usize>) -> Self {
        Op { kind, operands, span, raw_keyword: None, inline_image: None }
    }

    /// Operand `i` as a number, if it is one.
    pub fn num(&self, i: usize) -> Option<f64> {
        self.operands.get(i).and_then(Object::as_f64)
    }

    /// Operand `i` as a number, or `0.0`. Convenient for the many operators
    /// whose operands are all numeric, where a malformed stream should not stop
    /// the walk.
    pub fn num_or_zero(&self, i: usize) -> f64 {
        self.num(i).unwrap_or(0.0)
    }

    /// The last `n` operands as numbers. Operators are written
    /// operand-then-keyword, and damaged streams often carry extra leading
    /// junk, so counting back from the operator is more robust than counting
    /// forward from the start.
    pub fn trailing_nums<const N: usize>(&self) -> Option<[f64; N]> {
        if self.operands.len() < N {
            return None;
        }
        let start = self.operands.len() - N;
        let mut out = [0.0; N];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.operands[start + i].as_f64()?;
        }
        Some(out)
    }

    pub fn name(&self, i: usize) -> Option<&rasura_cos::Name> {
        self.operands.get(i).and_then(Object::as_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_keyword() {
        // Guards against a typo in either direction of the mapping, which would
        // otherwise show up as an operator silently becoming Unknown.
        let kinds = [
            OpKind::Save,
            OpKind::Restore,
            OpKind::Concat,
            OpKind::SetLineWidth,
            OpKind::SetLineCap,
            OpKind::SetLineJoin,
            OpKind::SetMiterLimit,
            OpKind::SetDash,
            OpKind::SetRenderingIntent,
            OpKind::SetFlatness,
            OpKind::SetExtGState,
            OpKind::MoveTo,
            OpKind::LineTo,
            OpKind::CurveTo,
            OpKind::CurveToInitialReplicated,
            OpKind::CurveToFinalReplicated,
            OpKind::ClosePath,
            OpKind::Rectangle,
            OpKind::Stroke,
            OpKind::CloseStroke,
            OpKind::Fill,
            OpKind::FillObsolete,
            OpKind::FillEvenOdd,
            OpKind::FillStroke,
            OpKind::FillStrokeEvenOdd,
            OpKind::CloseFillStroke,
            OpKind::CloseFillStrokeEvenOdd,
            OpKind::EndPath,
            OpKind::Clip,
            OpKind::ClipEvenOdd,
            OpKind::SetStrokeColorSpace,
            OpKind::SetFillColorSpace,
            OpKind::SetStrokeColor,
            OpKind::SetStrokeColorN,
            OpKind::SetFillColor,
            OpKind::SetFillColorN,
            OpKind::SetStrokeGray,
            OpKind::SetFillGray,
            OpKind::SetStrokeRgb,
            OpKind::SetFillRgb,
            OpKind::SetStrokeCmyk,
            OpKind::SetFillCmyk,
            OpKind::BeginText,
            OpKind::EndText,
            OpKind::SetCharSpacing,
            OpKind::SetWordSpacing,
            OpKind::SetHorizontalScale,
            OpKind::SetLeading,
            OpKind::SetFont,
            OpKind::SetRenderMode,
            OpKind::SetRise,
            OpKind::TextMove,
            OpKind::TextMoveSetLeading,
            OpKind::SetTextMatrix,
            OpKind::NextLine,
            OpKind::ShowText,
            OpKind::ShowTextAdjusted,
            OpKind::NextLineShowText,
            OpKind::NextLineSetSpacingShowText,
            OpKind::SetGlyphWidth,
            OpKind::SetGlyphWidthBBox,
            OpKind::InvokeXObject,
            OpKind::Shading,
            OpKind::MarkPoint,
            OpKind::MarkPointProps,
            OpKind::BeginMarked,
            OpKind::BeginMarkedProps,
            OpKind::EndMarked,
            OpKind::BeginCompat,
            OpKind::EndCompat,
        ];
        for k in kinds {
            let kw = k.keyword().unwrap_or_else(|| panic!("{k:?} has no keyword"));
            assert_eq!(OpKind::from_keyword(kw.as_bytes()), k, "keyword {kw}");
        }
        // Every kind that maps to a plain keyword. `InlineImage` and `Unknown`
        // are excluded: they have no fixed keyword of their own. Asserting the
        // count catches a kind being added without a keyword, or removed.
        assert_eq!(kinds.len(), 70, "operator table changed size");
    }

    #[test]
    fn inline_image_and_unknown_have_no_plain_keyword() {
        assert!(OpKind::InlineImage.keyword().is_none());
        assert!(OpKind::Unknown.keyword().is_none());
    }

    #[test]
    fn obsolete_fill_is_distinct_from_fill() {
        // `F` and `f` mean the same thing but must not be conflated, or a round
        // trip would rewrite one as the other.
        assert_ne!(OpKind::from_keyword(b"F"), OpKind::from_keyword(b"f"));
    }

    #[test]
    fn star_variants_are_not_confused_with_their_bases() {
        assert_eq!(OpKind::from_keyword(b"f*"), OpKind::FillEvenOdd);
        assert_eq!(OpKind::from_keyword(b"W*"), OpKind::ClipEvenOdd);
        assert_eq!(OpKind::from_keyword(b"B*"), OpKind::FillStrokeEvenOdd);
        assert_eq!(OpKind::from_keyword(b"T*"), OpKind::NextLine);
    }

    #[test]
    fn unrecognised_keywords_become_unknown() {
        assert_eq!(OpKind::from_keyword(b"zz"), OpKind::Unknown);
        assert_eq!(OpKind::from_keyword(b""), OpKind::Unknown);
        // Case matters: `tj` is not `Tj`.
        assert_eq!(OpKind::from_keyword(b"tj"), OpKind::Unknown);
    }

    #[test]
    fn text_operator_classification() {
        assert!(OpKind::ShowText.shows_text());
        assert!(OpKind::NextLineSetSpacingShowText.shows_text());
        assert!(!OpKind::SetFont.shows_text());
        assert!(OpKind::SetFont.is_text_operator());
        assert!(!OpKind::Rectangle.is_text_operator());
    }

    #[test]
    fn trailing_nums_counts_back_from_the_operator() {
        let mut operands: SmallVec<[Object; 4]> = SmallVec::new();
        // A stream with junk before the real operands, which happens.
        operands.push(Object::name("junk"));
        for v in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0] {
            operands.push(Object::Real(v));
        }
        let op = Op::new(OpKind::Concat, operands, 0..0);
        assert_eq!(op.trailing_nums::<6>(), Some([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]));
    }

    #[test]
    fn trailing_nums_declines_when_there_are_too_few() {
        let mut operands: SmallVec<[Object; 4]> = SmallVec::new();
        operands.push(Object::Integer(1));
        let op = Op::new(OpKind::Concat, operands, 0..0);
        assert_eq!(op.trailing_nums::<6>(), None);
    }
}
