//! Re-breaking a paragraph. Spec 9.3.
//!
//! > Scope is the paragraph. Never wider unless the caller opts into overflow
//! > propagation.
//!
//! # Greedy is a fidelity decision
//!
//! > `Greedy` (default) — matches what most producers did, so re-breaking a
//! > paragraph after a small edit usually reproduces the original break points.
//! >
//! > `KnuthPlass` — better typography, but will re-break lines the user did not
//! > touch. Opt-in.
//! >
//! > The greedy default is a fidelity decision, not a laziness one. Document it.
//!
//! So: an editor is judged on the diff it produces, not on the typography it
//! could have produced. A user who fixes a typo in line 3 and finds lines 4
//! through 11 re-broken has been given a worse result than the one they asked
//! for, however much better the spacing is.
//!
//! Knuth–Plass is implemented (spec §17's Phase 8) and is **still not the
//! default**, which is the whole point of the paragraph above: it is available
//! to a caller setting a paragraph fresh, and wrong for a caller fixing a typo.
//! The two algorithms differ in exactly the way that matters here — greedy
//! decides each line without looking ahead, so an edit late in the paragraph
//! cannot move an earlier break, while Knuth–Plass optimises the whole
//! paragraph at once and a single added character can shift every line in it.
//!
//! # Justification is a mechanism, not an effect
//!
//! > If the paragraph was justified, the original inter-word spacing was
//! > achieved by some combination of `Tw`, `Tz`, and `TJ` adjustments. Detect
//! > which the producer used and reproduce *that* mechanism; a paragraph
//! > justified with `Tw` that you re-justify with `TJ` arrays will look subtly
//! > different and will diff visually.
//!
//! Two paragraphs justified to the same measure by different mechanisms are
//! identical in width and different in every glyph position between the first
//! word and the last. Reproducing the width while changing the mechanism is
//! precisely the kind of "correct" result that fails a pixel diff.

use rasura_layout::lines::Line;
use rasura_layout::paragraphs::Alignment;

/// How to choose line breaks. Spec 9.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Breaking {
    /// Fill each line as far as it goes. What most producers did.
    #[default]
    Greedy,
    /// Optimise breaks over the whole paragraph, minimising total demerits.
    ///
    /// Better typography and a worse diff: a change anywhere can move a break
    /// anywhere, including before the edit. Opt in for a paragraph being set
    /// fresh, not for one being corrected.
    KnuthPlass,
}

/// What to do when reflowed text no longer fits its block. Spec 9.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    /// Fail and change nothing.
    #[default]
    Refuse,
    /// Extend the block downward, pushing what follows. Spec §17 Phase 6:
    /// cascading across pages changes page count, which needs the page
    /// operations that phase adds.
    Grow,
    /// Let it overflow; the caller renders and decides.
    Allow,
    /// Reduce size or leading within a bounded range to fit.
    Shrink,
}

/// How the producer achieved justification. Spec 9.3.
///
/// Detected from what the paragraph's operators actually contain, not from the
/// alignment: a paragraph can be visually justified and use any of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justification {
    /// Word spacing, set with `Tw`. The most common, and the cheapest to
    /// reproduce: one operand changes per line.
    WordSpacing,
    /// Per-pair adjustments inside `TJ` arrays.
    Adjustments,
    /// Horizontal scaling with `Tz`. Rare, and visible — it changes glyph
    /// shapes rather than the gaps between them.
    HorizontalScale,
    /// Nothing detectable: the paragraph is not justified, or its spacing comes
    /// from somewhere this layer cannot see.
    None,
}

/// A paragraph's reflow settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Policy {
    pub breaking: Breaking,
    pub overflow: Overflow,
}

/// Why a reflow could not be performed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReflowError {
    /// The text no longer fits and the policy is `Refuse`.
    #[error("the text overflows its block by {lines_over} line(s)")]
    Overflow { lines_over: usize },

    /// A word is wider than the whole measure, so no break helps.
    #[error("{word:?} is {width:.1} wide and the measure is {measure:.1}")]
    Unbreakable { word: String, width: f64, measure: f64 },

    /// The paragraph has no measurable width to break against.
    #[error("the paragraph has no measure to break against")]
    NoMeasure,
}

/// One line produced by reflow.
#[derive(Debug, Clone, PartialEq)]
pub struct Broken {
    /// The text on this line, without the break itself.
    pub text: String,
    /// Its advance width in text space.
    pub width: f64,
    /// Whether this is the paragraph's last line, which justification exempts.
    pub last: bool,
}

/// The result of re-breaking a paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct Reflowed {
    pub lines: Vec<Broken>,
    /// The measure the lines were broken against.
    pub measure: f64,
    /// How many lines the paragraph had before.
    pub before: usize,
    /// Whether the line count changed. When it did not, an in-place edit is
    /// possible and every line after the edit keeps its original position.
    pub same_shape: bool,
    /// The algorithm that actually produced these lines.
    ///
    /// Normally the one requested. The final Knuth–Plass pass accepts any line
    /// that is not overfull, and greedy's own breaks are never overfull, so an
    /// arrangement exists whenever greedy would have found one — the fallback
    /// is a guard rather than an expected path. It is reported anyway, because
    /// a caller who is told "optimal" and given greedy has been misinformed
    /// about the only thing that distinguishes them.
    pub breaking: Breaking,
}

/// Measure text, one character at a time, in text space at size 1.
///
/// Taking a closure rather than a font keeps this module testable without a
/// document, and keeps the width source a decision made once by the caller —
/// which matters because the same paragraph can be measured against the
/// producer's `/Widths` or against a substitute font's, and they differ.
pub trait Measure {
    /// The advance of `text` at font size 1. `None` when a character has no
    /// metric, which the caller reports rather than treats as zero.
    fn width_of(&self, text: &str) -> Option<f64>;
}

impl<F: Fn(&str) -> Option<f64>> Measure for F {
    fn width_of(&self, text: &str) -> Option<f64> {
        self(text)
    }
}

/// Break `text` to `measure`, at `size`. Spec 9.3.
pub fn reflow(
    text: &str,
    measure: f64,
    size: f64,
    before: usize,
    policy: Policy,
    metrics: &dyn Measure,
) -> Result<Reflowed, ReflowError> {
    // Written positively so a NaN measure -- which a degenerate text matrix can
    // produce -- falls into the refusal rather than through it.
    if !(measure.is_finite() && measure > 0.0 && size.is_finite() && size > 0.0) {
        return Err(ReflowError::NoMeasure);
    }

    let width_of = |s: &str| metrics.width_of(s).unwrap_or(0.0) * size;
    let space = width_of(" ");

    let words: Vec<(&str, f64)> =
        text.split(' ').filter(|w| !w.is_empty()).map(|w| (w, width_of(w))).collect();

    // Checked before either algorithm, because neither can help: no break makes
    // a word narrower. Reported rather than overflowed silently -- the caller's
    // options (hyphenate, shrink, widen the block) are all theirs to pick.
    if let Some((word, w)) = words.iter().find(|(_, w)| *w > measure) {
        return Err(ReflowError::Unbreakable { word: (*word).to_string(), width: *w, measure });
    }

    let breaks = match policy.breaking {
        Breaking::Greedy => None,
        Breaking::KnuthPlass => knuth_plass(&words, space, measure),
    };
    // Which algorithm the caller actually got. Knuth–Plass declining to find a
    // feasible set of breaks is not an error -- greedy always produces
    // *something* -- but it is not what was asked for either.
    let breaking = if breaks.is_some() { Breaking::KnuthPlass } else { Breaking::Greedy };

    let mut lines = match breaks {
        Some(ends) => assemble(&words, space, &ends),
        None => greedy(&words, space, measure),
    };
    if let Some(last) = lines.last_mut() {
        last.last = true;
    }

    let same_shape = lines.len() == before;
    if !same_shape && lines.len() > before {
        let lines_over = lines.len() - before;
        match policy.overflow {
            Overflow::Refuse => return Err(ReflowError::Overflow { lines_over }),
            // The remaining policies are the caller's problem to render; this
            // function's job is to report the shape it produced.
            Overflow::Grow | Overflow::Allow | Overflow::Shrink => {}
        }
    }

    Ok(Reflowed { lines, measure, before, same_shape, breaking })
}

/// Fill each line as far as it goes.
fn greedy(words: &[(&str, f64)], space: f64, measure: f64) -> Vec<Broken> {
    let mut lines: Vec<Broken> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0.0f64;

    for (word, w) in words {
        let with_word = if current.is_empty() { *w } else { current_width + space + w };
        if with_word <= measure || current.is_empty() {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            current_width = with_word;
        } else {
            lines.push(Broken {
                text: std::mem::take(&mut current),
                width: current_width,
                last: false,
            });
            current.push_str(word);
            current_width = *w;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Broken { text: current, width: current_width, last: false });
    }
    lines
}

/// Turn a set of break positions into lines.
///
/// `ends[k]` is the index of the last word on line `k`.
fn assemble(words: &[(&str, f64)], space: f64, ends: &[usize]) -> Vec<Broken> {
    let mut lines = Vec::with_capacity(ends.len());
    let mut start = 0usize;
    for &end in ends {
        let slice = &words[start..=end];
        let text = slice.iter().map(|(w, _)| *w).collect::<Vec<_>>().join(" ");
        let width: f64 =
            slice.iter().map(|(_, w)| w).sum::<f64>() + space * (slice.len() - 1) as f64;
        lines.push(Broken { text, width, last: false });
        start = end + 1;
    }
    lines
}

// ---------------------------------------------------------------------------
// Knuth–Plass
// ---------------------------------------------------------------------------

/// How far interword space may stretch and shrink, as a fraction of itself.
///
/// TeX takes these from the font's `\fontdimen`s, which a PDF does not carry:
/// `/Widths` gives the space's natural advance and nothing about its
/// elasticity. These are the values TeX's own Computer Modern uses, and they
/// are the reason a paragraph can be justified at all — with no stretch, every
/// line but a perfectly-filled one is infinitely bad.
const STRETCH: f64 = 0.5;
const SHRINK: f64 = 1.0 / 3.0;

/// `\linepenalty`: a constant cost per line, which is what stops the algorithm
/// preferring a ten-line paragraph of perfect spacing to a nine-line paragraph
/// of nearly-perfect spacing.
const LINE_PENALTY: f64 = 10.0;

/// `\adjdemerits`: charged when consecutive lines are in fitness classes more
/// than one apart. A tight line next to a very loose one is worse to read than
/// either is on its own, and nothing in the badness of either says so.
const ADJACENT_DEMERITS: f64 = 10_000.0;

/// Stretch a line has beyond its interword glue, in multiples of a space.
///
/// TeX gives a line with no glue in it — one word, short of the measure —
/// *infinite* badness, then relies on `\emergencystretch` to make it finite
/// again. Copying that here would mean any paragraph containing a one-word line
/// has no feasible breaks at all, which is most paragraphs, and the optimiser
/// would decline them rather than choose between arrangements that are all
/// imperfect.
///
/// One space of floor makes every line's badness finite and *ordered*: a line
/// five units short still costs more than one two units short, which is the
/// comparison the algorithm exists to make. It is a modelling choice, not
/// something ISO 32000 or TeX specifies, and it is the smallest amount that
/// makes the model total.
const STRETCH_FLOOR: f64 = 1.0;

/// Knuth–Plass total-fit line breaking.
///
/// Returns the index of the last word on each line, or `None` when no set of
/// breaks is feasible even at the loosest tolerance — in which case the caller
/// falls back to greedy and says so.
///
/// This is the algorithm from *Breaking Paragraphs into Lines* (Knuth & Plass,
/// 1981) with one deliberate omission: **no hyphenation.** Discretionary breaks
/// are where most of Knuth–Plass's advantage comes from, and they need a
/// hyphenation dictionary per language — which is a bundle-size decision (§12.3)
/// rather than a line-breaking one. Without them this is still optimal over the
/// breakpoints that exist, which is the claim being made and no more.
///
/// O(n²) over the paragraph's words. TeX prunes the active list to keep it near
/// linear; a paragraph is a few hundred words at most, and the pruning is where
/// the subtle bugs live.
fn knuth_plass(words: &[(&str, f64)], space: f64, measure: f64) -> Option<Vec<usize>> {
    if words.is_empty() {
        return None;
    }

    // Prefix sums, so a line's natural width is a subtraction rather than a
    // walk: the inner loop runs O(n²) times and the walk would make it O(n³).
    let mut prefix = Vec::with_capacity(words.len() + 1);
    prefix.push(0.0);
    for (_, w) in words {
        prefix.push(prefix[prefix.len() - 1] + w);
    }

    // Escalating tolerance, as TeX does. The first pass insists on lines that
    // are close to filling the measure; each later one accepts worse. Trying
    // the loose tolerance first would take a bad arrangement whenever a good
    // one existed, because both are feasible and the demerits of a *feasible*
    // paragraph are compared across the whole paragraph rather than per line.
    //
    // The last pass accepts any line that is not overfull, so it always finds
    // an arrangement when one exists at all — greedy's own breaks are never
    // overfull, so that is whenever greedy would have worked.
    for tolerance in [200.0f64, 10_000.0, f64::INFINITY] {
        if let Some(breaks) = optimise(words, &prefix, space, measure, tolerance) {
            return Some(breaks);
        }
    }
    None
}

fn optimise(
    words: &[(&str, f64)],
    prefix: &[f64],
    space: f64,
    measure: f64,
    tolerance: f64,
) -> Option<Vec<usize>> {
    let n = words.len();

    // `best[i]` is the least total demerits for a paragraph whose first `i`
    // words are already set, together with where the last line began and which
    // fitness class it fell into.
    let mut best: Vec<Option<(f64, usize, usize)>> = vec![None; n + 1];
    best[0] = Some((0.0, 0, 1));

    for end in 1..=n {
        for start in 0..end {
            let Some((so_far, _, previous_class)) = best[start] else { continue };

            let gaps = (end - start - 1) as f64;
            let natural = prefix[end] - prefix[start] + space * gaps;
            let last_line = end == n;

            // The last line is not justified. TeX models this with a
            // `\parfillskip` of infinite stretch, which makes any final line
            // perfectly fitting however short it is -- and is why a paragraph
            // does not end with its last two words spread across the measure.
            let ratio = if last_line {
                if natural > measure { continue } else { 0.0 }
            } else {
                adjustment_ratio(natural, measure, gaps * space, space)
            };
            // Below -1 the line is overfull: it would have to shrink further
            // than its spaces are willing to go, which means glyphs past the
            // margin. That is the one outcome no tolerance permits.
            if ratio < -1.0 {
                continue;
            }

            let badness = 100.0 * ratio.abs().powi(3);
            if !badness.is_finite() || badness > tolerance {
                continue;
            }
            let class = fitness_class(ratio);
            let mut demerits = (LINE_PENALTY + badness).powi(2);
            if class.abs_diff(previous_class) > 1 {
                demerits += ADJACENT_DEMERITS;
            }

            let total = so_far + demerits;
            if best[end].is_none_or(|(existing, _, _)| total < existing) {
                best[end] = Some((total, start, class));
            }
        }
    }

    best[n]?;

    // Walk the chain back to the start.
    let mut ends = Vec::new();
    let mut at = n;
    while at > 0 {
        let (_, start, _) = best[at]?;
        ends.push(at - 1);
        at = start;
    }
    ends.reverse();
    Some(ends)
}

/// How far a line's glue must stretch (positive) or shrink (negative) to fill
/// the measure, as a multiple of what it is willing to do.
///
/// `glue` is the line's total interword space at its natural width, and `space`
/// one space's own width — the unit the [`STRETCH_FLOOR`] is measured in, so
/// the floor scales with the font rather than being a bare number of user-space
/// units.
///
/// The floor applies on the stretching side only. A line can always be made
/// *longer* by spreading it; one with no space in it cannot be made shorter at
/// all, which is why a single word wider than the measure is caught before any
/// of this runs.
fn adjustment_ratio(natural: f64, measure: f64, glue: f64, space: f64) -> f64 {
    let difference = measure - natural;
    if difference.abs() < f64::EPSILON {
        return 0.0;
    }
    if difference > 0.0 {
        let available = (glue + STRETCH_FLOOR * space) * STRETCH;
        return if available > 0.0 { difference / available } else { f64::INFINITY };
    }
    let available = glue * SHRINK;
    if available > 0.0 { difference / available } else { f64::NEG_INFINITY }
}

/// TeX's four fitness classes. Adjacent lines more than one class apart are
/// charged extra, which is what keeps a tight line from sitting under a very
/// loose one.
fn fitness_class(ratio: f64) -> usize {
    if ratio < -0.5 {
        0 // tight
    } else if ratio <= 0.5 {
        1 // decent
    } else if ratio <= 1.0 {
        2 // loose
    } else {
        3 // very loose
    }
}

/// The measure a paragraph was originally set to, if it can be known.
///
/// Taken from the widest line rather than from the block's bounding box. A
/// bounding box is the union of what was drawn, so on a paragraph whose lines
/// all fall short it understates the measure and reflow then breaks earlier
/// than the producer did — re-breaking lines nobody touched, which is the exact
/// failure the greedy default exists to avoid.
///
/// **`None` for a single-line paragraph**, and that is the important case. A
/// paragraph that never wrapped is a paragraph whose producer never showed us
/// where it *would* wrap: the one line is as wide as its text and no wider, so
/// taking it as the measure means every added character overflows. A heading
/// gaining one letter would report itself re-broken into two lines, which is
/// both wrong and the kind of false alarm that teaches callers to ignore the
/// fidelity report.
///
/// The caller supplies a page-level bound for that case; see
/// [`available_width`].
pub fn measure_of(lines: &[Line]) -> Option<f64> {
    if lines.len() < 2 {
        return None;
    }
    let widest = lines
        .iter()
        .map(|l| {
            let (start, end) = l.extent();
            end - start
        })
        .fold(0.0f64, f64::max);
    (widest > 0.0).then_some(widest)
}

/// How much room a line has before it runs off the page.
///
/// The fallback when a paragraph never wrapped. It is a weaker bound than a
/// real measure — text can legitimately be narrower than the page — but it is a
/// *true* one: glyphs past the crop box are not visible, and no producer
/// intended that. Reporting only what can be checked is better than inventing a
/// margin nobody specified.
pub fn available_width(lines: &[Line], crop_right: f64) -> Option<f64> {
    let first = lines.first()?;
    let (start, _) = first.extent();
    (crop_right > start).then_some(crop_right - start)
}

/// Which mechanism justified a paragraph. Spec 9.3.
///
/// `word_spacings` and `scales` are the `Tw` and `Tz` values in force on each
/// line; `adjustments` is the count of non-zero `TJ` adjustments across the
/// paragraph, excluding those the font's own kerning explains.
pub fn justification(
    alignment: Alignment,
    word_spacings: &[f64],
    scales: &[f64],
    unexplained_adjustments: usize,
) -> Justification {
    if alignment != Alignment::Justified {
        return Justification::None;
    }
    // Order matters. A justified paragraph can carry all three at once -- a
    // producer setting Tw for the gaps and TJ for kerning is ordinary -- and
    // the mechanism to reproduce is the one carrying the *justification*, which
    // is whichever varies from line to line.
    if word_spacings.iter().any(|w| *w != 0.0) && varies(word_spacings) {
        return Justification::WordSpacing;
    }
    if varies(scales) && scales.iter().any(|s| (*s - 100.0).abs() > 0.01) {
        return Justification::HorizontalScale;
    }
    if unexplained_adjustments > 0 {
        return Justification::Adjustments;
    }
    // A justified paragraph with none of the three is one whose words happened
    // to fill the measure, or one this layer cannot read. Either way there is
    // no mechanism to reproduce.
    Justification::None
}

/// Whether a per-line value is not the same on every line.
fn varies(values: &[f64]) -> bool {
    let Some(first) = values.first() else { return false };
    values.iter().any(|v| (v - first).abs() > 1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character one unit wide, so widths are character counts.
    fn monospace() -> impl Measure {
        |s: &str| Some(s.chars().count() as f64)
    }

    fn greedy() -> Policy {
        Policy { breaking: Breaking::Greedy, overflow: Overflow::Allow }
    }

    #[test]
    fn greedy_fills_each_line_as_far_as_it_goes() {
        let out = reflow("aaa bbb ccc ddd", 7.0, 1.0, 2, greedy(), &monospace()).expect("reflow");
        let texts: Vec<&str> = out.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["aaa bbb", "ccc ddd"]);
        assert!(out.same_shape, "two lines in, two out");
    }

    #[test]
    fn the_last_line_is_marked_because_justification_exempts_it() {
        let out = reflow("aaa bbb ccc", 7.0, 1.0, 2, greedy(), &monospace()).expect("reflow");
        assert!(!out.lines[0].last);
        assert!(out.lines.last().expect("a line").last);
    }

    #[test]
    fn a_word_wider_than_the_measure_is_reported_not_overflowed() {
        // Silently letting it overflow would put text outside the block with
        // nothing in the result saying so.
        let err = reflow("supercalifragilistic", 5.0, 1.0, 1, greedy(), &monospace())
            .expect_err("unbreakable");
        assert!(matches!(err, ReflowError::Unbreakable { .. }), "{err:?}");
    }

    #[test]
    fn refuse_is_the_default_and_fails_rather_than_growing() {
        let policy = Policy { breaking: Breaking::Greedy, overflow: Overflow::Refuse };
        let err = reflow("aaa bbb ccc ddd", 7.0, 1.0, 1, policy, &monospace())
            .expect_err("overflows one line into two");
        assert!(matches!(err, ReflowError::Overflow { lines_over: 1 }), "{err:?}");

        assert_eq!(Policy::default().overflow, Overflow::Refuse);
        assert_eq!(Policy::default().breaking, Breaking::Greedy);
    }

    #[test]
    fn allow_reports_the_new_shape_instead_of_failing() {
        let out = reflow("aaa bbb ccc ddd", 7.0, 1.0, 1, greedy(), &monospace()).expect("reflow");
        assert_eq!(out.lines.len(), 2);
        assert!(!out.same_shape, "the caller can see the paragraph grew");
        assert_eq!(out.before, 1);
    }

    fn optimal() -> Policy {
        Policy { breaking: Breaking::KnuthPlass, overflow: Overflow::Allow }
    }

    /// The sum over non-final lines of how far each falls short, squared.
    ///
    /// The quantity Knuth–Plass exists to reduce. Squared rather than summed,
    /// because the whole point is that one badly short line is worse than three
    /// slightly short ones — a plain sum is nearly equal for both and would not
    /// distinguish the algorithms at all.
    fn raggedness(out: &Reflowed) -> f64 {
        out.lines.iter().filter(|l| !l.last).map(|l| (out.measure - l.width).powi(2)).sum()
    }

    #[test]
    fn knuth_plass_produces_more_even_lines_than_greedy() {
        // The textbook counterexample to greedy. Greedy fills line one to the
        // margin, which strands "cc" alone on line two with a four-unit hole it
        // can never go back and fix. Moving one word off the first line costs
        // three units there and saves all four.
        let text = "aaa bb cc ddddd";
        let g = reflow(text, 6.0, 1.0, 3, greedy(), &monospace()).expect("greedy");
        let k = reflow(text, 6.0, 1.0, 3, optimal(), &monospace()).expect("knuth-plass");

        let texts = |r: &Reflowed| r.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&g), vec!["aaa bb", "cc", "ddddd"]);
        assert_eq!(texts(&k), vec!["aaa", "bb cc", "ddddd"]);

        assert_eq!(k.breaking, Breaking::KnuthPlass, "it ran, rather than falling back");
        assert!(
            raggedness(&k) < raggedness(&g),
            "knuth-plass {:.1} should beat greedy {:.1}",
            raggedness(&k),
            raggedness(&g),
        );
    }

    #[test]
    fn both_algorithms_keep_every_word_in_order() {
        // The property that must hold whatever the breaks are, and the one a
        // line-breaker gets wrong by dropping the word at a boundary.
        let text = "the quick brown fox jumps over the lazy dog and keeps on running";
        for policy in [greedy(), optimal()] {
            let out = reflow(text, 18.0, 1.0, 4, policy, &monospace()).expect("reflow");
            let rejoined = out.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
            assert_eq!(rejoined, text, "{policy:?}");
        }
    }

    #[test]
    fn no_line_exceeds_the_measure() {
        // Knuth–Plass permits a line to *shrink* below its natural width, but
        // never past the measure: a ratio below -1 is infeasible by definition.
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let out = reflow(text, 22.0, 1.0, 3, optimal(), &monospace()).expect("reflow");
        for line in &out.lines {
            assert!(line.width <= out.measure + 1e-9, "{:?} at {}", line.text, line.width);
        }
    }

    #[test]
    fn the_last_line_is_free_to_be_short() {
        // Without `\parfillskip`'s infinite stretch, a paragraph ending in two
        // words would be charged enormous badness for it and the algorithm
        // would contort the lines above to avoid it.
        let text = "aaaa bbbb cccc dddd eeee ff";
        let out = reflow(text, 14.0, 1.0, 2, optimal(), &monospace()).expect("reflow");
        let last = out.lines.last().expect("a line");
        assert!(last.last);
        assert!(last.width < out.measure, "the last line is short and that is fine");
    }

    #[test]
    fn a_one_word_line_is_set_rather_than_declared_impossible() {
        // TeX gives a line with no glue in it infinite badness, and copying
        // that would make every paragraph containing a one-word line
        // unbreakable -- which is most paragraphs. The stretch floor makes such
        // a line merely bad, and bad in proportion to how short it is.
        let out = reflow("aaaaa bb", 5.0, 1.0, 2, optimal(), &monospace()).expect("reflow");
        assert_eq!(out.breaking, Breaking::KnuthPlass, "{:?}", out.lines);
        assert_eq!(out.lines.len(), 2);
        assert_eq!(out.lines[0].text, "aaaaa");
    }

    #[test]
    fn the_shortest_of_several_bad_lines_is_preferred() {
        // What the stretch floor buys: with TeX's infinite badness, every
        // one-word line is equally impossible and the optimiser has nothing to
        // choose between. Here a line two units short still beats one four
        // units short.
        // Both arrangements strand one word on a line of its own; the question
        // is only which word. Greedy takes the first and leaves a seven-unit
        // hole; the optimiser takes the second and leaves six.
        let text = "aaaa bbbb ccc dddddddd";
        let g = reflow(text, 10.0, 1.0, 3, greedy(), &monospace()).expect("greedy");
        let k = reflow(text, 10.0, 1.0, 3, optimal(), &monospace()).expect("knuth-plass");

        let texts = |r: &Reflowed| r.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&g), vec!["aaaa bbbb", "ccc", "dddddddd"]);
        assert_eq!(texts(&k), vec!["aaaa", "bbbb ccc", "dddddddd"]);
        assert!(raggedness(&k) < raggedness(&g));
    }

    #[test]
    fn greedy_reports_itself_as_greedy() {
        let out = reflow("aaa bbb ccc", 7.0, 1.0, 2, greedy(), &monospace()).expect("reflow");
        assert_eq!(out.breaking, Breaking::Greedy);
    }

    #[test]
    fn an_unbreakable_word_is_reported_under_both_algorithms() {
        // Checked before either runs: no set of breaks makes a word narrower,
        // and Knuth–Plass silently returning `None` here would have the caller
        // fall back to greedy and overflow instead of being told.
        for policy in [greedy(), optimal()] {
            let err = reflow("ok supercalifragilistic", 5.0, 1.0, 1, policy, &monospace())
                .expect_err("unbreakable");
            assert!(matches!(err, ReflowError::Unbreakable { .. }), "{policy:?}: {err:?}");
        }
    }

    #[test]
    fn a_single_word_paragraph_works_under_both() {
        for policy in [greedy(), optimal()] {
            let out = reflow("alone", 20.0, 1.0, 1, policy, &monospace()).expect("reflow");
            assert_eq!(out.lines.len(), 1);
            assert_eq!(out.lines[0].text, "alone");
            assert!(out.lines[0].last);
        }
    }

    #[test]
    fn size_scales_the_measure_comparison() {
        // At size 2 every glyph is twice as wide, so half as much fits.
        let out = reflow("aaa bbb ccc ddd", 7.0, 2.0, 4, greedy(), &monospace()).expect("reflow");
        assert_eq!(out.lines.len(), 4, "one word per line at double size");
    }

    #[test]
    fn a_zero_measure_is_refused_rather_than_looping() {
        let err = reflow("a b", 0.0, 1.0, 1, greedy(), &monospace()).expect_err("no measure");
        assert!(matches!(err, ReflowError::NoMeasure), "{err:?}");
    }

    #[test]
    fn empty_text_produces_one_empty_line() {
        let out = reflow("", 10.0, 1.0, 1, greedy(), &monospace()).expect("reflow");
        assert_eq!(out.lines.len(), 1);
        assert_eq!(out.lines[0].text, "");
    }

    #[test]
    fn justification_reports_the_mechanism_that_varies_per_line() {
        // A producer using Tw for the gaps and TJ for kerning carries both. The
        // one to reproduce is the one doing the justifying, which is the one
        // that differs from line to line.
        assert_eq!(
            justification(Alignment::Justified, &[1.2, 2.4, 0.8], &[100.0, 100.0, 100.0], 40),
            Justification::WordSpacing
        );
        assert_eq!(
            justification(Alignment::Justified, &[0.0, 0.0, 0.0], &[100.0, 100.0, 100.0], 40),
            Justification::Adjustments
        );
        assert_eq!(
            justification(Alignment::Justified, &[0.0, 0.0], &[98.5, 101.2], 0),
            Justification::HorizontalScale
        );
    }

    #[test]
    fn an_unjustified_paragraph_has_no_mechanism() {
        // Tw is also used for ordinary letterspacing, so a left-aligned
        // paragraph carrying it must not be mistaken for a justified one.
        assert_eq!(
            justification(Alignment::Left, &[1.2, 2.4], &[100.0, 100.0], 40),
            Justification::None
        );
    }

    #[test]
    fn a_constant_word_spacing_is_not_justification() {
        // Every line the same means it is a paragraph-wide setting, not the
        // per-line adjustment that fills a measure.
        assert_eq!(
            justification(Alignment::Justified, &[1.5, 1.5, 1.5], &[100.0, 100.0, 100.0], 0),
            Justification::None
        );
    }

    #[test]
    fn a_paragraph_that_never_wrapped_has_no_known_measure() {
        // The producer never showed us where it would break, so there is
        // nothing to check a fit against. Guessing the line's own width means
        // every added character overflows -- a heading gaining one letter would
        // report itself re-broken, which is a false alarm that teaches callers
        // to ignore the fidelity report.
        assert_eq!(measure_of(&[]), None);
    }
}
