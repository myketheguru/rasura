//! Writing numbers the way the producer wrote them. Spec 9.4.
//!
//! > Number formatting in generated operators: match the original's precision.
//! > A producer that wrote `72.0` should not get `72` back; a producer that
//! > wrote `0.0001` should not get `1e-4`. Sample the original stream's numeric
//! > formatting and mirror it. **This matters because diffs are how users audit
//! > you.**
//!
//! That last sentence is the whole argument. Every number this layer emits is
//! numerically equivalent whichever way it is written, so nothing renders
//! differently — but a commit that rewrites `72.0` as `72` across a page
//! produces a diff full of changes that mean nothing, and buries the one change
//! that means something. An editor whose diffs cannot be read is an editor
//! nobody can check.
//!
//! `rasura_cos::format_real` already produces the shortest exact decimal,
//! which is the right default for objects nobody wrote before. Content streams
//! are different: there *is* a prior author, and the polite thing is to write
//! like them.

use rasura_cos::object::format_real;

/// How a particular content stream writes its numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberStyle {
    /// Digits after the point on the reals that had one — the **median** of
    /// what the stream contained.
    ///
    /// The question is "what does this producer usually write", and the median
    /// is the statistic that answers it. Not the mean, which a handful of
    /// six-decimal `cm` entries drags upward from the two decimals every
    /// coordinate on the page uses; not the maximum, which one such entry sets
    /// outright; and not an upper quantile, which is the same failure at a
    /// smaller dose — a real pdfTeX page turns out to be a third matrix entries,
    /// enough to carry the 75th percentile.
    pub decimals: usize,
    /// Whether integral values were written with a fractional part (`72.0`
    /// rather than `72`).
    pub integral_keeps_point: bool,
    /// Whether values below one carried a leading zero (`0.5` rather than `.5`).
    pub leading_zero: bool,
}

impl Default for NumberStyle {
    /// What `format_real` does, for a stream with nothing to learn from.
    fn default() -> NumberStyle {
        NumberStyle { decimals: 2, integral_keeps_point: false, leading_zero: true }
    }
}

impl NumberStyle {
    /// Format `value` the way this stream's author would have.
    pub fn format(&self, value: f64) -> String {
        if !value.is_finite() {
            return "0".into();
        }

        let integral = value == value.trunc() && value.abs() < 1e15;
        if integral && !self.integral_keeps_point {
            return format!("{}", value as i64);
        }

        let mut s = format!("{value:.*}", self.decimals);

        // An exact decimal beats a rounded one. If the sampled precision cannot
        // represent this value, widen until it can rather than silently moving
        // a glyph: fidelity to the number outranks fidelity to the formatting.
        if s.parse::<f64>() != Ok(value) {
            s = format_real(value);
            if integral && self.integral_keeps_point && !s.contains('.') {
                s.push_str(".0");
            }
        }

        if !self.leading_zero {
            if let Some(rest) = s.strip_prefix("0.") {
                s = format!(".{rest}");
            } else if let Some(rest) = s.strip_prefix("-0.") {
                s = format!("-.{rest}");
            }
        }
        s
    }
}

/// Learn a stream's numeric habits by reading its numbers.
///
/// Scans for numeric literals and ignores everything else. It does not need to
/// tokenise properly: a byte run that looks like a number inside a string or a
/// comment still tells us what this producer's digits look like, and being
/// wrong about one costs nothing — the worst case is a style slightly closer to
/// some other part of the same file.
pub fn sample(stream: &[u8]) -> NumberStyle {
    let mut decimal_counts: Vec<usize> = Vec::new();
    let mut integers_with_point = 0usize;
    let mut integers_plain = 0usize;
    let mut with_leading_zero = 0usize;
    let mut without_leading_zero = 0usize;

    let mut i = 0usize;
    while i < stream.len() {
        let b = stream[i];
        if !(b.is_ascii_digit()
            || ((b == b'.' || b == b'-' || b == b'+') && starts_number(stream, i)))
        {
            i += 1;
            continue;
        }

        let start = i;
        if matches!(stream[i], b'-' | b'+') {
            i += 1;
        }
        let int_start = i;
        while i < stream.len() && stream[i].is_ascii_digit() {
            i += 1;
        }
        let int_digits = i - int_start;

        let mut frac_digits = 0usize;
        let mut had_point = false;
        if i < stream.len() && stream[i] == b'.' {
            had_point = true;
            i += 1;
            let frac_start = i;
            while i < stream.len() && stream[i].is_ascii_digit() {
                i += 1;
            }
            frac_digits = i - frac_start;
        }
        if int_digits == 0 && frac_digits == 0 {
            i = start + 1;
            continue;
        }

        if had_point {
            // `72.0` counts twice over: it says this producer gives integers a
            // fractional part, *and* that the part is one digit long. The
            // second half is easy to throw away by treating the zeros as
            // padding — and then a stream written entirely in `1.0`/`0.0`
            // matrix entries teaches nothing about its own precision.
            let integral = frac_digits > 0 && stream[i - frac_digits..i].iter().all(|c| *c == b'0');
            if integral {
                integers_with_point += 1;
            }
            decimal_counts.push(frac_digits);

            if int_digits == 0 {
                without_leading_zero += 1;
            } else if int_digits == 1 && stream[int_start] == b'0' {
                with_leading_zero += 1;
            }
        } else {
            integers_plain += 1;
        }
    }

    let decimals = median(&mut decimal_counts).unwrap_or(2);
    NumberStyle {
        decimals: decimals.min(8),
        // Only when it is the producer's consistent habit. One `1.0` among
        // three hundred plain integers is a stray, not a convention.
        integral_keeps_point: integers_with_point > integers_plain,
        // Leading zeros are the overwhelming default, so they are assumed
        // unless this stream visibly drops them.
        leading_zero: without_leading_zero == 0 || with_leading_zero >= without_leading_zero,
    }
}

/// Whether a sign or point at `i` begins a number rather than ending a name.
fn starts_number(stream: &[u8], i: usize) -> bool {
    stream.get(i + 1).is_some_and(u8::is_ascii_digit)
}

/// The lower median of a sample.
///
/// Lower rather than averaged, because the answer is a digit count and half a
/// digit is not one.
fn median(values: &mut [usize]) -> Option<usize> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    values.get((values.len() - 1) / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_decimal_producer_gets_two_decimals_back() {
        let style = sample(b"1 0 0 1 72.00 700.50 Tm (x) Tj 12.25 0 Td");
        assert_eq!(style.decimals, 2);
        assert_eq!(style.format(700.5), "700.50");
        assert_eq!(style.format(72.0), "72", "integers stay integers here");
    }

    #[test]
    fn a_producer_that_writes_seventy_two_point_zero_keeps_getting_it() {
        // Spec 9.4 names this case specifically. pdfTeX and several Office
        // exporters write every coordinate with a fractional part, and
        // "correcting" them produces a diff touching every line on the page.
        let style = sample(b"1.0 0.0 0.0 1.0 72.0 700.0 Tm 14.0 0.0 Td");
        assert!(style.integral_keeps_point);
        assert_eq!(style.format(72.0), "72.0");
    }

    #[test]
    fn one_stray_real_does_not_make_it_a_habit() {
        let style = sample(b"1 0 0 1 72 700 Tm 0 0 Td 1 0 0 1 90 700 Tm 2.0 Tw");
        assert!(!style.integral_keeps_point, "one 2.0 among a dozen integers is a stray");
        assert_eq!(style.format(72.0), "72");
    }

    #[test]
    fn a_producer_that_drops_leading_zeros_keeps_dropping_them() {
        let style = sample(b"BT .5 .25 .125 rg .75 Tw ET");
        assert!(!style.leading_zero);
        // Two decimals: the sample is .5, .25, .125, .75, whose median is 2.
        assert_eq!(style.format(0.5), ".50");
        assert_eq!(style.format(-0.5), "-.50");
    }

    #[test]
    fn leading_zeros_are_the_default() {
        let style = sample(b"0.5 0.25 rg");
        assert!(style.leading_zero);
        // The sample is one and two decimals; the lower median is one.
        assert_eq!(style.format(0.5), "0.5");
        assert_eq!(style.format(0.25), "0.25", "and precision widens where it must");
    }

    #[test]
    fn exponent_notation_is_never_emitted() {
        // PDF has no exponent form. `format!("{}", 0.0001f64)` is fine, but
        // 1e-7 is not, and a Tz or Tc value can get there.
        let style = NumberStyle::default();
        for v in [0.0001f64, 1e-7, 1e-12, 1.5e-9, 123456789.0] {
            let s = style.format(v);
            assert!(!s.contains('e') && !s.contains('E'), "{v} formatted as {s}");
        }
    }

    #[test]
    fn precision_is_widened_rather_than_rounding_a_value_away() {
        // The style says two decimals; the value needs four. Rounding it would
        // move the glyph, and a formatting convention is not worth a visible
        // change. Fidelity to the number wins.
        let style = NumberStyle { decimals: 2, ..NumberStyle::default() };
        assert_eq!(style.format(0.0001), "0.0001");
        assert_eq!(style.format(1.005), "1.005");
    }

    #[test]
    fn every_formatted_number_parses_back_to_itself() {
        let styles = [
            NumberStyle::default(),
            NumberStyle { decimals: 0, integral_keeps_point: false, leading_zero: true },
            NumberStyle { decimals: 6, integral_keeps_point: true, leading_zero: false },
        ];
        for style in styles {
            for v in [0.0, 1.0, -1.0, 0.5, -0.5, 72.0, 700.25, 1e-5, -1234.5678] {
                let s = style.format(v);
                let back: f64 = s.parse().unwrap_or_else(|_| panic!("{s:?} does not parse"));
                assert!((back - v).abs() < 1e-9, "{v} -> {s:?} -> {back}");
            }
        }
    }

    #[test]
    fn an_empty_stream_falls_back_to_the_default() {
        let style = sample(b"BT ET");
        assert_eq!(style, NumberStyle::default());
    }

    #[test]
    fn sampling_a_real_looking_content_stream() {
        // A pdfTeX-shaped stream: integers for most things, six-decimal matrix
        // entries in the odd `cm`. Those must not set the precision for the
        // whole page — and there are enough of them (two of six reals here)
        // that an upper quantile would let them, which is why this is a median.
        let stream = b"q 0.999702 0 0 0.999702 0 0 cm BT /F1 9.96 Tf 133.77 662.83 Td \
                       [(Hello)-333(world)]TJ 0 -11.95 Td (again) Tj ET Q";
        let style = sample(stream);
        assert_eq!(style.decimals, 2, "the 0.999702 entries do not set the precision");
        assert_eq!(style.format(133.77), "133.77");
        assert_eq!(style.format(72.0), "72");
    }
}
