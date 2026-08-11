//! Font matching for substitution. Spec 8.5.
//!
//! When no source font is registered for the original typeface, a registered
//! font is scored against the original's `/FontDescriptor`:
//!
//! ```text
//! score = w1·|ΔStemV| + w2·|ΔItalicAngle| + w3·|ΔCapHeight| + w4·|ΔXHeight|
//!       + w5·flagMismatch(Serif, FixedPitch, Script, Symbolic)
//!       + w6·avgWidthDelta   // over the glyphs both fonts share
//! ```
//!
//! Spec 8.5 says which term matters: "`avgWidthDelta` dominates in practice: a
//! metric-compatible substitute (Liberation for Arial, TeX Gyre for the
//! URW/Adobe families) reflows almost identically, while a visually similar but
//! metrically different font shifts every line." The weights below encode that
//! — a font that looks right and measures wrong loses to one that measures
//! right, because the second reflows the page and the first moves every line
//! after the substitution.
//!
//! **Substitution is never silent.** The result carries the chosen font and the
//! score so a caller can reject it, which is §2's rule that fidelity is
//! reported rather than assumed. There is no "just pick something" entry point.

use rasura_cos::{Dictionary, Document};
use std::collections::HashMap;

/// Weights. Lower scores are better; every term is a penalty.
///
/// Chosen so that the width term dominates as spec 8.5 requires: a mean width
/// difference of 20 units per glyph outweighs any single flag mismatch, and a
/// metric-compatible clone with quite different stem weights still wins.
const W_WIDTH: f64 = 1.0;
const W_STEM_V: f64 = 0.15;
const W_ITALIC: f64 = 2.0;
const W_CAP_HEIGHT: f64 = 0.05;
const W_X_HEIGHT: f64 = 0.05;
const W_FLAG: f64 = 20.0;

/// `/Flags` bits that describe the typeface rather than its encoding.
/// ISO 32000-1 Table 123.
const FLAG_FIXED_PITCH: u32 = 1 << 0;
const FLAG_SERIF: u32 = 1 << 1;
const FLAG_SYMBOLIC: u32 = 1 << 2;
const FLAG_SCRIPT: u32 = 1 << 3;

/// The parts of a `/FontDescriptor` that say what a typeface looks like.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Descriptor {
    pub stem_v: Option<f64>,
    pub italic_angle: Option<f64>,
    pub cap_height: Option<f64>,
    pub x_height: Option<f64>,
    pub flags: u32,
}

impl Descriptor {
    pub fn from_dict(doc: &Document, dict: &Dictionary) -> Descriptor {
        let num = |key: &str| doc.get_entry(dict, key).ok().flatten().and_then(|o| o.as_f64());
        Descriptor {
            stem_v: num("StemV"),
            italic_angle: num("ItalicAngle"),
            cap_height: num("CapHeight"),
            x_height: num("XHeight"),
            flags: doc
                .get_entry(dict, "Flags")
                .ok()
                .flatten()
                .and_then(|o| o.as_i64())
                .unwrap_or(0)
                .max(0) as u32,
        }
    }

    fn mismatched_flags(&self, other: &Descriptor) -> u32 {
        let interesting = FLAG_FIXED_PITCH | FLAG_SERIF | FLAG_SYMBOLIC | FLAG_SCRIPT;
        ((self.flags ^ other.flags) & interesting).count_ones()
    }
}

/// A font offered as a substitute.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub descriptor: Descriptor,
    /// Advance widths by glyph name, in 1/1000 em.
    pub widths: HashMap<String, f64>,
}

/// Why a candidate scored as it did.
///
/// Broken out per term rather than reduced to a number, because a caller
/// deciding whether to accept a substitution wants to know *what* is wrong: a
/// score of 30 from a slant mismatch and a score of 30 from metric drift are
/// different problems.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Score {
    pub total: f64,
    pub width: f64,
    pub stem_v: f64,
    pub italic_angle: f64,
    pub cap_height: f64,
    pub x_height: f64,
    pub flags: f64,
    /// Glyphs both fonts have, over which the width term was measured.
    pub shared_glyphs: usize,
    /// Mean absolute width difference over those glyphs, in 1/1000 em, before
    /// weighting. The number a caller most wants to see.
    pub mean_width_delta: f64,
}

impl Score {
    /// Whether the width term could be measured at all.
    ///
    /// Spec 8.5 makes it the dominant term, so a score computed without it is
    /// much weaker evidence and should be labelled as such rather than
    /// presented as a like-for-like comparison.
    pub fn metrics_compared(&self) -> bool {
        self.shared_glyphs > 0
    }
}

/// The chosen substitute.
#[derive(Debug, Clone)]
pub struct Substitution {
    /// Index into the candidate list.
    pub index: usize,
    pub name: String,
    pub score: Score,
    /// The runner-up's score, when there was one. A caller can see how clear
    /// the decision was: two candidates within a point of each other means the
    /// choice was arbitrary, whatever the winner's absolute score.
    pub runner_up: Option<f64>,
}

/// Score one candidate against the original.
pub fn score(
    original: &Descriptor,
    original_widths: &HashMap<String, f64>,
    candidate: &Candidate,
) -> Score {
    let mut s = Score::default();

    // A metric either side does not state contributes nothing. Penalising a
    // missing value would rank a font that declines to describe itself below
    // one that describes itself badly.
    let delta = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => 0.0,
    };

    s.stem_v = W_STEM_V * delta(original.stem_v, candidate.descriptor.stem_v);
    s.italic_angle = W_ITALIC * delta(original.italic_angle, candidate.descriptor.italic_angle);
    s.cap_height = W_CAP_HEIGHT * delta(original.cap_height, candidate.descriptor.cap_height);
    s.x_height = W_X_HEIGHT * delta(original.x_height, candidate.descriptor.x_height);
    s.flags = W_FLAG * original.mismatched_flags(&candidate.descriptor) as f64;

    // The dominant term: how differently the two fonts would set the same text.
    let mut total_delta = 0.0;
    for (name, width) in original_widths {
        if let Some(other) = candidate.widths.get(name) {
            total_delta += (width - other).abs();
            s.shared_glyphs += 1;
        }
    }
    if s.shared_glyphs > 0 {
        s.mean_width_delta = total_delta / s.shared_glyphs as f64;
        s.width = W_WIDTH * s.mean_width_delta;
    }

    s.total = s.width + s.stem_v + s.italic_angle + s.cap_height + s.x_height + s.flags;
    s
}

/// Choose the best substitute from a registered set.
///
/// Returns `None` when nothing was registered. There is deliberately no
/// fallback to a built-in default: spec 8.1 promotes "the developer must supply
/// fonts" into the public API, and quietly substituting something the caller
/// never registered is the silent behaviour this layer exists to avoid.
pub fn best_match(
    original: &Descriptor,
    original_widths: &HashMap<String, f64>,
    candidates: &[Candidate],
) -> Option<Substitution> {
    let mut scored: Vec<(usize, Score)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (i, score(original, original_widths, c)))
        .collect();
    // Ties break towards the earlier candidate, so a caller's registration
    // order is the tie-breaker and the result is reproducible.
    scored.sort_by(|a, b| {
        a.1.total.partial_cmp(&b.1.total).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
    });

    let (index, best) = scored.first().copied()?;
    Some(Substitution {
        index,
        name: candidates[index].name.clone(),
        score: best,
        runner_up: scored.get(1).map(|(_, s)| s.total),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(n, w)| ((*n).to_string(), *w)).collect()
    }

    /// Helvetica-ish: sans, upright, moderate stems.
    fn helvetica() -> Descriptor {
        Descriptor {
            stem_v: Some(88.0),
            italic_angle: Some(0.0),
            cap_height: Some(718.0),
            x_height: Some(523.0),
            flags: 32, // nonsymbolic
        }
    }

    fn candidate(name: &str, descriptor: Descriptor, w: &[(&str, f64)]) -> Candidate {
        Candidate { name: name.into(), descriptor, widths: widths(w) }
    }

    const ARIAL_WIDTHS: [(&str, f64); 4] =
        [("A", 667.0), ("B", 667.0), ("space", 278.0), ("i", 222.0)];

    #[test]
    fn an_identical_font_scores_zero() {
        let c = candidate("Helvetica", helvetica(), &ARIAL_WIDTHS);
        let s = score(&helvetica(), &widths(&ARIAL_WIDTHS), &c);
        assert_eq!(s.total, 0.0);
        assert_eq!(s.shared_glyphs, 4);
        assert!(s.metrics_compared());
    }

    #[test]
    fn a_metric_compatible_clone_beats_a_visually_similar_one() {
        // Spec 8.5's central claim. Liberation Sans has Arial's metrics and
        // rather different stem weights; a "similar-looking" sans with its own
        // metrics shifts every line after the substitution.
        let original = helvetica();
        let original_widths = widths(&ARIAL_WIDTHS);

        let liberation = candidate(
            "Liberation Sans",
            Descriptor { stem_v: Some(120.0), cap_height: Some(700.0), ..helvetica() },
            &ARIAL_WIDTHS,
        );
        let lookalike = candidate(
            "Some Other Sans",
            helvetica(),
            &[("A", 610.0), ("B", 640.0), ("space", 250.0), ("i", 260.0)],
        );

        let best = best_match(&original, &original_widths, &[lookalike, liberation.clone()])
            .expect("a match");
        assert_eq!(best.name, "Liberation Sans", "metrics won, as spec 8.5 says they should");
        assert!(best.score.mean_width_delta < 1.0);
    }

    #[test]
    fn a_serif_mismatch_is_penalised() {
        let original = helvetica();
        let sans = candidate("Sans", helvetica(), &ARIAL_WIDTHS);
        let serif =
            candidate("Serif", Descriptor { flags: 32 | FLAG_SERIF, ..helvetica() }, &ARIAL_WIDTHS);

        let s_sans = score(&original, &widths(&ARIAL_WIDTHS), &sans);
        let s_serif = score(&original, &widths(&ARIAL_WIDTHS), &serif);
        assert!(s_serif.total > s_sans.total);
        assert_eq!(s_serif.flags, W_FLAG, "exactly one flag differs");
    }

    #[test]
    fn several_flag_mismatches_cost_more_than_one() {
        let original = helvetica();
        let one = candidate("One", Descriptor { flags: 32 | FLAG_SERIF, ..helvetica() }, &[]);
        let three = candidate(
            "Three",
            Descriptor { flags: 32 | FLAG_SERIF | FLAG_FIXED_PITCH | FLAG_SCRIPT, ..helvetica() },
            &[],
        );
        let empty = widths(&[]);
        assert_eq!(score(&original, &empty, &one).flags, W_FLAG);
        assert_eq!(score(&original, &empty, &three).flags, W_FLAG * 3.0);
    }

    #[test]
    fn a_slant_mismatch_is_visible_in_its_own_term() {
        let original = helvetica();
        let italic = candidate(
            "Italic",
            Descriptor { italic_angle: Some(-12.0), ..helvetica() },
            &ARIAL_WIDTHS,
        );
        let s = score(&original, &widths(&ARIAL_WIDTHS), &italic);
        assert_eq!(s.italic_angle, W_ITALIC * 12.0);
        // And the breakdown says which term it was, which a bare total cannot.
        assert_eq!(s.width, 0.0);
        assert_eq!(s.flags, 0.0);
    }

    #[test]
    fn a_missing_metric_costs_nothing_rather_than_being_penalised() {
        // Penalising absence would rank a font that declines to describe itself
        // below one that describes itself badly.
        let original = helvetica();
        let silent = candidate(
            "Silent",
            Descriptor {
                stem_v: None,
                italic_angle: None,
                cap_height: None,
                x_height: None,
                flags: 32,
            },
            &ARIAL_WIDTHS,
        );
        let s = score(&original, &widths(&ARIAL_WIDTHS), &silent);
        assert_eq!(s.stem_v, 0.0);
        assert_eq!(s.italic_angle, 0.0);
        assert_eq!(s.total, 0.0);
    }

    #[test]
    fn the_width_term_is_measured_only_over_shared_glyphs() {
        let original_widths = widths(&[("A", 667.0), ("B", 667.0), ("zcaron", 500.0)]);
        let c = candidate("Partial", helvetica(), &[("A", 700.0), ("B", 700.0)]);
        let s = score(&helvetica(), &original_widths, &c);

        assert_eq!(s.shared_glyphs, 2, "zcaron is in neither comparison");
        assert_eq!(s.mean_width_delta, 33.0);
    }

    #[test]
    fn no_shared_glyphs_is_reported_not_scored_as_perfect() {
        // The dominant term could not be measured. A total of zero here means
        // "nothing was compared", not "a perfect match", and the caller has to
        // be able to tell those apart.
        let c = candidate("Disjoint", helvetica(), &[("alpha", 500.0)]);
        let s = score(&helvetica(), &widths(&ARIAL_WIDTHS), &c);
        assert_eq!(s.shared_glyphs, 0);
        assert!(!s.metrics_compared());
        assert_eq!(s.width, 0.0);
    }

    #[test]
    fn width_dominates_a_single_flag_mismatch() {
        // Spec 8.5: a metrically wrong font "shifts every line". Twenty units
        // of mean drift is worse than being a serif when a sans was asked for.
        let original = helvetica();
        let ow = widths(&ARIAL_WIDTHS);

        let wrong_metrics = candidate(
            "Wrong metrics",
            helvetica(),
            &[("A", 692.0), ("B", 692.0), ("space", 303.0), ("i", 247.0)],
        );
        let wrong_class = candidate(
            "Wrong class",
            Descriptor { flags: 32 | FLAG_SERIF, ..helvetica() },
            &ARIAL_WIDTHS,
        );

        assert!(
            score(&original, &ow, &wrong_metrics).total > score(&original, &ow, &wrong_class).total
        );
    }

    #[test]
    fn the_runner_up_shows_how_close_the_decision_was() {
        // Two candidates within a point of each other means the choice was
        // arbitrary, whatever the winner's absolute score.
        let original = helvetica();
        let ow = widths(&ARIAL_WIDTHS);
        let a = candidate("A", helvetica(), &ARIAL_WIDTHS);
        let b = candidate(
            "B",
            helvetica(),
            &[("A", 668.0), ("B", 667.0), ("space", 278.0), ("i", 222.0)],
        );

        let best = best_match(&original, &ow, &[a, b]).unwrap();
        assert_eq!(best.name, "A");
        let gap = best.runner_up.unwrap() - best.score.total;
        assert!(gap < 1.0, "the decision was nearly a coin toss: {gap}");
    }

    #[test]
    fn an_empty_registry_substitutes_nothing() {
        // Spec 8.1 promotes "the developer must supply fonts" into the API.
        // Quietly falling back to a built-in default is the silent behaviour
        // this layer exists to avoid.
        assert!(best_match(&helvetica(), &widths(&ARIAL_WIDTHS), &[]).is_none());
    }

    #[test]
    fn ties_break_towards_the_earlier_registration() {
        let original = helvetica();
        let ow = widths(&ARIAL_WIDTHS);
        let first = candidate("First", helvetica(), &ARIAL_WIDTHS);
        let second = candidate("Second", helvetica(), &ARIAL_WIDTHS);
        let best = best_match(&original, &ow, &[first, second]).unwrap();
        assert_eq!(best.name, "First", "reproducible, and the caller's order decides");
        assert_eq!(best.index, 0);
    }

    #[test]
    fn a_descriptor_reads_from_a_font_dictionary() {
        use rasura_cos::testutil::ClassicBuilder;
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << >> >>",
            )
            .stream(4, "", b"")
            .object(
                5,
                "<< /Type /FontDescriptor /FontName /X /Flags 34 /StemV 88 \
                 /ItalicAngle -12 /CapHeight 718 /XHeight 523 >>",
            )
            .finish("/Root 1 0 R");
        let doc = Document::open(bytes).unwrap();
        let dict = doc.get(rasura_cos::ObjId::new(5, 0)).unwrap().as_dict().unwrap().clone();

        let d = Descriptor::from_dict(&doc, &dict);
        assert_eq!(d.stem_v, Some(88.0));
        assert_eq!(d.italic_angle, Some(-12.0));
        assert_eq!(d.flags, 34);
        // 34 is nonsymbolic (32) plus serif (2).
        assert_eq!(d.mismatched_flags(&Descriptor { flags: 32, ..Default::default() }), 1);
    }
}
