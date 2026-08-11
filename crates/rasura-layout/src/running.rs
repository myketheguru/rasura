//! Running elements and footnotes. Spec 7.7.
//!
//! Headers and footers are the first thing in this crate that **cannot be
//! decided from one page**. A line at the top of page 1 reading "Chapter 3" is
//! indistinguishable from a heading; what makes it a running header is that
//! pages 2, 3 and 4 have one in the same place. So the entry point takes every
//! page at once, and nothing here has a single-page equivalent.
//!
//! Footnotes are per-page and live here because they are the other thing that
//! is structurally *not* body text.

use crate::Region;
use crate::lines::Line;
use rasura_content::matrix::Rect;

/// Spec 7.7: "content within the top/bottom 12% of the media box".
const MARGIN_FRACTION: f64 = 0.12;

/// Spec 7.7: "repeats in position across ≥3 pages".
const MIN_REPEATS: usize = 3;

/// Two candidates within this many points count as the same position.
const POSITION_TOLERANCE: f64 = 3.0;

/// A footnote rule is short relative to the text width: that is what
/// distinguishes it from a full-width border.
const FOOTNOTE_RULE_MAX: f64 = 0.5;

/// A footnote is a sentence. Fewer words than this and it is a folio, a
/// datestamp, or a figure number.
const MIN_NOTE_WORDS: usize = 3;

/// Where on the page a running element sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Header,
    Footer,
}

/// A header or footer, identified across pages.
#[derive(Debug, Clone)]
pub struct RunningElement {
    pub placement: Placement,
    /// Page indices on which this element appears, ascending.
    pub pages: Vec<usize>,
    /// The block index within each of those pages, parallel to `pages`.
    pub blocks: Vec<usize>,
    /// The text, with any varying numeric field replaced by `{}`.
    pub template: String,
    /// The literal text on each page, parallel to `pages`.
    pub instances: Vec<String>,
    pub bbox: Rect,
    /// Whether the varying field looks like a page number. Spec 7.7 wants this
    /// so that editing one can optionally propagate to all -- and propagating a
    /// page number to every page would be wrong.
    pub is_page_number: bool,
}

impl RunningElement {
    /// Whether every instance is identical, so an edit propagates cleanly.
    pub fn is_constant(&self) -> bool {
        !self.template.contains("{}")
    }
}

/// A page's blocks, as the cross-page pass needs them.
pub struct PageRegions<'a> {
    pub regions: &'a [Region],
    pub media_box: Rect,
}

/// Find headers and footers across a document.
///
/// Takes every page because the question "is this a running header?" has no
/// single-page answer.
pub fn running_elements(pages: &[PageRegions<'_>]) -> Vec<RunningElement> {
    if pages.len() < MIN_REPEATS {
        return Vec::new();
    }

    let mut out = Vec::new();
    for placement in [Placement::Header, Placement::Footer] {
        // Gather every candidate block in the margin band, keyed loosely by
        // vertical position so that "same place on the page" is decidable.
        let mut candidates: Vec<Candidate> = Vec::new();
        for (pi, page) in pages.iter().enumerate() {
            for (bi, block) in page.regions.iter().enumerate() {
                if !in_band(block, page.media_box, placement) {
                    continue;
                }
                let text = block.text();
                if text.trim().is_empty() {
                    continue;
                }
                candidates.push(Candidate {
                    page: pi,
                    block: bi,
                    y: block.bbox.y0,
                    text,
                    bbox: block.bbox,
                });
            }
        }

        // Group by position, then by shape. Position first: two different
        // running elements can share a band (a header and a rule caption), and
        // grouping by text alone would merge them.
        let mut used = vec![false; candidates.len()];
        for i in 0..candidates.len() {
            if used[i] {
                continue;
            }
            let mut group = vec![i];
            used[i] = true;
            for j in i + 1..candidates.len() {
                if used[j] || candidates[j].page == candidates[i].page {
                    continue;
                }
                let same_place = (candidates[j].y - candidates[i].y).abs() <= POSITION_TOLERANCE;
                if same_place && template_of(&candidates[i].text, &candidates[j].text).is_some() {
                    group.push(j);
                    used[j] = true;
                }
            }
            if group.len() < MIN_REPEATS {
                // Not a repeat. Release the members so they can join another
                // group -- a page-number footer and a title footer in the same
                // band must not block each other.
                for &g in &group {
                    used[g] = false;
                }
                used[i] = true;
                continue;
            }

            let instances: Vec<String> =
                group.iter().map(|&g| candidates[g].text.clone()).collect();
            let template = instances
                .iter()
                .skip(1)
                .try_fold(instances[0].clone(), |acc, t| template_of(&acc, t))
                .unwrap_or_else(|| instances[0].clone());

            let page_indices: Vec<usize> = group.iter().map(|&g| candidates[g].page).collect();
            let varying: Vec<&str> = instances.iter().map(|s| s.as_str()).collect();

            out.push(RunningElement {
                placement,
                is_page_number: looks_like_page_numbering(&template, &varying, &page_indices),
                pages: page_indices,
                blocks: group.iter().map(|&g| candidates[g].block).collect(),
                template,
                instances,
                bbox: candidates[i].bbox,
            });
        }
    }
    out
}

struct Candidate {
    page: usize,
    block: usize,
    y: f64,
    text: String,
    bbox: Rect,
}

fn in_band(block: &Region, media: Rect, placement: Placement) -> bool {
    // Device space: y grows downward from the top of the page, so the header
    // band is low y and the footer band is high y.
    let height = media.y1 - media.y0;
    if height <= 0.0 {
        return false;
    }
    let band = height * MARGIN_FRACTION;
    match placement {
        Placement::Header => block.bbox.y1 <= media.y0 + band,
        Placement::Footer => block.bbox.y0 >= media.y1 - band,
    }
}

/// Reduce two strings to a common template, allowing **one** varying run of
/// digits. Spec 7.7: "allowing a numeric field to vary".
///
/// Returns `None` when they differ in any other way, which is what stops two
/// unrelated headings in the same band from being merged into one running
/// element.
fn template_of(a: &str, b: &str) -> Option<String> {
    if a == b {
        return Some(a.to_string());
    }
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());

    // Common prefix and suffix; whatever differs in between must be numeric on
    // both sides, or already the placeholder.
    let mut p = 0;
    while p < ac.len() && p < bc.len() && ac[p] == bc[p] {
        p += 1;
    }
    let mut s = 0;
    while s < ac.len() - p && s < bc.len() - p && ac[ac.len() - 1 - s] == bc[bc.len() - 1 - s] {
        s += 1;
    }

    // Neither boundary may fall *inside* a run of digits. Without this, "Page
    // 9" and "Page 10" share the prefix "Page 1", so the varying field is read
    // as `0` against nothing and a single running header splits in two at the
    // page-10 boundary -- which is exactly what freeculture.pdf did, yielding
    // "Page {}" for pages 1-9 and "Page 1{}" for 10-15.
    while p > 0 && ac[p - 1].is_ascii_digit() {
        p -= 1;
    }
    while s > 0 && ac[ac.len() - s].is_ascii_digit() {
        s -= 1;
    }
    if p + s > ac.len() || p + s > bc.len() {
        return None;
    }

    let amid: String = ac[p..ac.len() - s].iter().collect();
    let bmid: String = bc[p..bc.len() - s].iter().collect();

    let numeric = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    let ok = |t: &str| numeric(t) || t == "{}";
    if !ok(&amid) || !ok(&bmid) {
        return None;
    }

    let prefix: String = ac[..p].iter().collect();
    let suffix: String = ac[ac.len() - s..].iter().collect();
    Some(format!("{prefix}{{}}{suffix}"))
}

/// Whether the varying field increments with the page index.
///
/// A field that varies but does not increment is a running head quoting a
/// section number, and propagating an edit across it would be wrong -- so this
/// is deliberately stricter than "contains a number".
fn looks_like_page_numbering(template: &str, instances: &[&str], pages: &[usize]) -> bool {
    numbering(template, instances, pages).unwrap_or(false)
}

fn numbering(template: &str, instances: &[&str], pages: &[usize]) -> Option<bool> {
    if !template.contains("{}") || instances.len() < MIN_REPEATS {
        return Some(false);
    }
    // Read the field the template identified, not the first digits in the
    // string. freeculture.pdf's running head is a QuarkXPress slug --
    // "14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page 1" -- where the first
    // digits are the job number and never change, so a naive scan concludes
    // nothing increments.
    let (prefix, suffix) = template.split_once("{}")?;
    let values: Vec<i64> = instances
        .iter()
        .filter_map(|t| t.strip_prefix(prefix)?.strip_suffix(suffix)?.trim().parse().ok())
        .collect();
    if values.len() != instances.len() {
        return Some(false);
    }
    // Strictly increasing, and in step with the page order.
    Some(values.windows(2).zip(pages.windows(2)).all(|(v, p)| v[1] > v[0] && p[1] > p[0]))
}

// --- footnotes ----------------------------------------------------------------

/// A footnote block at the foot of a page.
#[derive(Debug, Clone)]
pub struct Footnote {
    /// Index into the page's blocks.
    pub block: usize,
    pub bbox: Rect,
    /// The marker that starts the note, if it is numeric.
    pub marker: Option<String>,
    /// Modal font size of the note, which is smaller than the body's.
    pub size: f64,
    /// Whether a short rule was found above it.
    pub separated_by_rule: bool,
    /// Where the in-text marker referring to this note was found, if anywhere.
    /// Spec 7.7's strongest signal: a note something refers to is a note.
    pub marker_site: Option<MarkerSite>,
}

/// Detect footnotes on one page.
///
/// Spec 7.7: "a block at the page bottom, separated by a short rule, with a
/// smaller modal font size".
pub fn footnotes(blocks: &[Region], rules: &[crate::Rule], media: Rect) -> Vec<Footnote> {
    let body = modal_size(blocks);
    if body <= 0.0 {
        return Vec::new();
    }
    let height = media.y1 - media.y0;
    let lower_third = media.y1 - height / 3.0;

    let mut out = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        if block.bbox.y0 < lower_third || block.is_empty() {
            continue;
        }
        let size = block_size(block);
        // "Smaller" has to mean meaningfully smaller. A 9.5pt line under 10pt
        // body text is a typesetter's optical correction, not a footnote.
        if size <= 0.0 || size > body * 0.9 {
            continue;
        }

        // A short rule above it, per spec 7.7. Short is the point: a full-width
        // rule is a border or a table edge.
        //
        // Measured against the page's whole text extent rather than the widest
        // single block, because on a page where the note itself is the widest
        // block the latter would compare the rule to the note.
        let left = blocks.iter().map(|b| b.bbox.x0).fold(f64::MAX, f64::min);
        let right = blocks.iter().map(|b| b.bbox.x1).fold(f64::MIN, f64::max);
        let text_width = (right - left).max(0.0);
        let separated = rules.iter().any(|r| {
            r.horizontal
                && r.position() < block.bbox.y0
                && r.position() > block.bbox.y0 - height * 0.15
                && r.length() <= text_width * FOOTNOTE_RULE_MAX
        });

        // Position and size alone are not enough, and neither is adding the
        // rule. A page number, a caption, a URL and a copyright line all sit at
        // the bottom in smaller type; `Test-plusminus.pdf` is an engineering
        // drawing whose title block puts DRAWN / CHECKED / SCALE in small type
        // between short rules, and every one of them satisfied position, size
        // and separation. What none of them has is a marker -- and spec 7.7's
        // own linking clause presupposes one, since a footnote nothing refers
        // to is not a footnote.
        let Some(marker) = leading_marker(block) else { continue };

        out.push(Footnote {
            block: i,
            bbox: block.bbox,
            marker: Some(marker),
            size,
            separated_by_rule: separated,
            marker_site: None,
        });
    }

    // Link first, then filter. A note is prose, not a numeral -- without a
    // length test every page footer opening with a digit qualifies, and `19`,
    // `06/12/2023, 14:12` and `2 sur 7` all did. But the test must not outrank
    // the link: applying it first removed precisely the notes that *were*
    // linked, which are the ones spec 7.7's own criterion vouches for.
    let sites = link_markers(blocks, &out);
    let mut kept = Vec::with_capacity(out.len());
    for (mut note, site) in out.into_iter().zip(sites) {
        note.marker_site = site;
        let words: usize = blocks[note.block]
            .lines
            .iter()
            .map(|l| crate::segment(l).iter().filter(|w| !w.is_empty()).count())
            .sum();
        if site.is_some() || words >= MIN_NOTE_WORDS {
            kept.push(note);
        }
    }
    kept
}

/// Where an in-text footnote marker was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerSite {
    pub block: usize,
    pub line: usize,
    /// Glyph range within that line.
    pub start: usize,
    pub end: usize,
}

/// Match footnotes to their in-text markers by numeral. Spec 7.7.
///
/// Matching runs over **glyphs, not words**. A superscript marker almost always
/// abuts the word it annotates with no intervening space, so §7.4 segments
/// `text` + superscript `1` into the single word `text1`, which never equals
/// the marker `1`. Going through words scored zero matches on the whole corpus;
/// this scores what is actually there.
pub fn link_markers(blocks: &[Region], notes: &[Footnote]) -> Vec<Option<MarkerSite>> {
    notes
        .iter()
        .map(|note| {
            let marker = note.marker.as_ref()?;
            for (bi, block) in blocks.iter().enumerate() {
                if bi == note.block {
                    continue;
                }
                let body = block_size(block);
                for (li, line) in block.lines.iter().enumerate() {
                    for (start, end) in superscript_runs(line, body) {
                        let text: String = line.glyphs[start..end]
                            .iter()
                            .filter_map(|g| g.text.as_deref())
                            .collect();
                        if text.trim() == marker {
                            return Some(MarkerSite { block: bi, line: li, start, end });
                        }
                    }
                }
            }
            None
        })
        .collect()
}

/// Maximal runs of consecutive superscript glyphs in a line.
fn superscript_runs(line: &Line, body_size: f64) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, g) in line.glyphs.iter().enumerate() {
        if is_superscript(g, body_size) {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push((s, i));
        }
    }
    if let Some(s) = start {
        out.push((s, line.glyphs.len()));
    }
    out
}

/// A superscript sits above the baseline, or is set smaller, or both.
///
/// `Ts` alone is not enough: plenty of producers set a superscript by shrinking
/// the font and shifting the text matrix, leaving rise at zero.
fn is_superscript(g: &crate::PlacedGlyph, body_size: f64) -> bool {
    g.rise > 0.0 || (body_size > 0.0 && g.size > 0.0 && g.size < body_size * 0.85)
}

/// Traditional non-numeric footnote markers, in Chicago's order.
const SYMBOL_MARKERS: [char; 6] = ['*', '\u{2020}', '\u{2021}', '\u{a7}', '\u{b6}', '#'];

/// The leading marker of a block: a numeral, or one of the traditional symbols.
fn leading_marker(block: &Region) -> Option<String> {
    let text = block.text();
    let text = text.trim_start();
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        return Some(digits);
    }
    // `*`, `†`, `‡` and friends, possibly doubled as in `††`.
    let first = text.chars().next()?;
    if !SYMBOL_MARKERS.contains(&first) {
        return None;
    }
    Some(text.chars().take_while(|c| *c == first).collect())
}

fn modal_size(blocks: &[Region]) -> f64 {
    let mut sizes: Vec<f64> = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .filter(|l| !l.is_empty())
        .map(|l| l.size)
        .collect();
    modal(&mut sizes)
}

fn block_size(block: &Region) -> f64 {
    let mut sizes: Vec<f64> =
        block.lines.iter().filter(|l| !l.is_empty()).map(|l| l.size).collect();
    modal(&mut sizes)
}

fn modal(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mut best, mut best_count, mut i) = (values[0], 0usize, 0usize);
    while i < values.len() {
        let mut j = i;
        while j < values.len() && (values[j] - values[i]).abs() < 0.5 {
            j += 1;
        }
        if j - i > best_count {
            best_count = j - i;
            best = values[i];
        }
        i = j;
    }
    best
}

/// Lines of a block that are not part of any running element.
pub fn body_lines<'a>(
    block: &'a Region,
    running: &[RunningElement],
    page: usize,
    index: usize,
) -> &'a [Line] {
    let is_running = running
        .iter()
        .any(|r| r.pages.iter().zip(r.blocks.iter()).any(|(&p, &b)| p == page && b == index));
    if is_running { &[] } else { &block.lines }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::build_page;

    const MEDIA: Rect = Rect { x0: 0.0, y0: 0.0, x1: 600.0, y1: 800.0 };

    /// Build N pages, each from its own content stream.
    fn pages_of(sources: &[String]) -> Vec<Vec<Region>> {
        sources.iter().map(|s| build_page(s).0).collect()
    }

    fn running_of(sources: &[String]) -> Vec<RunningElement> {
        let per_page = pages_of(sources);
        let refs: Vec<PageRegions<'_>> =
            per_page.iter().map(|b| PageRegions { regions: b, media_box: MEDIA }).collect();
        running_elements(&refs)
    }

    /// A page with a header, some body text, and a footer.
    fn page(header: &str, body: &str, footer: &str) -> String {
        let mut c = String::new();
        // y=760 in user space is 40pt from the top: inside the 12% band.
        c.push_str(&format!("BT /F1 10 Tf 1 0 0 1 72 760 Tm ({header}) Tj ET\n"));
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm ({body} line) Tj ET\n",
                600 - i * 12
            ));
        }
        c.push_str(&format!("BT /F1 10 Tf 1 0 0 1 72 40 Tm ({footer}) Tj ET\n"));
        c
    }

    #[test]
    fn a_repeated_header_is_found() {
        let src: Vec<String> =
            (0..4).map(|i| page("A Running Head", &format!("body{i}"), "x")).collect();
        let found = running_of(&src);
        let header = found.iter().find(|r| r.placement == Placement::Header).expect("header");
        assert_eq!(header.pages.len(), 4);
        assert_eq!(header.template, "A Running Head");
        assert!(header.is_constant());
        assert!(!header.is_page_number);
    }

    #[test]
    fn a_page_number_footer_is_detected_as_one() {
        let src: Vec<String> = (1..5).map(|i| page("Head", "body", &format!("Page {i}"))).collect();
        let found = running_of(&src);
        let footer = found.iter().find(|r| r.placement == Placement::Footer).expect("footer");
        assert_eq!(footer.template, "Page {}");
        assert!(footer.is_page_number, "an incrementing field is a page number");
        assert!(!footer.is_constant());
    }

    #[test]
    fn a_varying_field_that_does_not_increment_is_not_a_page_number() {
        // A running head quoting a section number varies but does not count
        // upward with the page. Propagating an edit across it would be wrong.
        let numbers = [7, 3, 9, 2];
        let src: Vec<String> =
            numbers.iter().map(|n| page(&format!("Section {n}"), "body", "x")).collect();
        let found = running_of(&src);
        let header = found.iter().find(|r| r.placement == Placement::Header).expect("header");
        assert_eq!(header.template, "Section {}");
        assert!(!header.is_page_number);
    }

    #[test]
    fn two_repeats_are_not_a_running_element() {
        // Spec 7.7 says three pages.
        let src: Vec<String> = (0..2).map(|_| page("Head", "body", "foot")).collect();
        assert!(running_of(&src).is_empty());
    }

    #[test]
    fn body_text_is_never_a_running_element() {
        let src: Vec<String> = (0..4).map(|_| page("Head", "identical body", "foot")).collect();
        let found = running_of(&src);
        // The body repeats verbatim on every page, but it is not in the margin
        // band, so it must not be picked up.
        assert!(
            found.iter().all(|r| !r.template.contains("identical")),
            "body text was mistaken for a running element"
        );
    }

    #[test]
    fn unrelated_text_in_the_same_band_is_not_merged() {
        // Two headers that differ non-numerically must stay separate rather
        // than collapsing into one template.
        let names = ["Alpha", "Beta", "Gamma", "Delta"];
        let src: Vec<String> = names.iter().map(|n| page(n, "body", "foot")).collect();
        let found = running_of(&src);
        assert!(
            found.iter().all(|r| r.placement != Placement::Header),
            "four different headers are not one running element"
        );
    }

    #[test]
    fn a_header_and_footer_are_found_independently() {
        let src: Vec<String> = (1..5).map(|i| page("Title", "body", &format!("{i}"))).collect();
        let found = running_of(&src);
        assert!(found.iter().any(|r| r.placement == Placement::Header));
        assert!(found.iter().any(|r| r.placement == Placement::Footer));
    }

    #[test]
    fn template_reduction_allows_exactly_one_numeric_field() {
        assert_eq!(template_of("Page 1", "Page 2"), Some("Page {}".into()));
        assert_eq!(template_of("1 of 9", "2 of 9"), Some("{} of 9".into()));
        assert_eq!(template_of("same", "same"), Some("same".into()));
        // Non-numeric variation is not a running element.
        assert_eq!(template_of("Alpha", "Beta"), None);
        // Two independently varying fields are beyond what spec 7.7 allows.
        assert_eq!(template_of("1 of 9", "2 of 8"), None);
    }

    #[test]
    fn page_numbering_reads_the_templates_field_not_the_first_digits() {
        // freeculture.pdf's running head is a QuarkXPress slug whose leading
        // digits are an unchanging job number.
        let t = "14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page {}";
        let instances = [
            "14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page 1",
            "14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page 2",
            "14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page 3",
        ];
        assert!(looks_like_page_numbering(t, &instances, &[16, 17, 18]));
    }

    #[test]
    fn a_field_that_grows_a_digit_stays_one_template() {
        // freeculture.pdf: "Page 9" and "Page 10" share the prefix "Page 1", so
        // a naive scan splits one running header into two at page 10.
        assert_eq!(template_of("Page 9", "Page 10"), Some("Page {}".into()));
        assert_eq!(template_of("Page 10", "Page 11"), Some("Page {}".into()));
        assert_eq!(template_of("Page 99", "Page 100"), Some("Page {}".into()));
        // The same, with a suffix on the far side of the number.
        assert_eq!(template_of("p9 of 20", "p10 of 20"), Some("p{} of 20".into()));
    }

    // --- footnotes ------------------------------------------------------------

    /// Body text, a short rule, and a smaller note beneath it.
    fn page_with_footnote() -> String {
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page with a marker) Tj ET\n",
                700 - i * 12
            ));
        }
        // The in-text superscript marker.
        c.push_str("BT /F1 6 Tf 1 0 0 1 380 704 Tm 3 Ts (1) Tj ET\n");
        // A short rule: 100pt against a ~300pt measure.
        c.push_str("72 120 m 172 120 l S\n");
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (1 The note itself) Tj ET\n");
        c
    }

    #[test]
    fn a_smaller_block_under_a_short_rule_is_a_footnote() {
        let (blocks, rules, _) = build_page(&page_with_footnote());
        let notes = footnotes(&blocks, &rules, MEDIA);
        assert_eq!(notes.len(), 1, "expected one footnote");
        assert!(notes[0].separated_by_rule);
        assert_eq!(notes[0].marker.as_deref(), Some("1"));
        assert!(notes[0].size < 10.0);
    }

    #[test]
    fn a_footnote_links_to_its_in_text_marker() {
        let (blocks, rules, _) = build_page(&page_with_footnote());
        let notes = footnotes(&blocks, &rules, MEDIA);
        let site = notes[0].marker_site.expect("the superscript 1 should have been found");
        assert_ne!(site.block, notes[0].block, "a note must not link to itself");
    }

    #[test]
    fn a_linked_note_survives_the_length_test() {
        // The length test exists to reject folios, but it must not outrank the
        // link: applying it first removed exactly the notes spec 7.7's own
        // criterion vouches for.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page here) Tj ET\n",
                700 - i * 12
            ));
        }
        c.push_str("BT /F1 6 Tf 1 0 0 1 380 688 Tm 3 Ts (1) Tj ET\n");
        c.push_str("72 120 m 172 120 l S\n");
        // Two words, below MIN_NOTE_WORDS, but something refers to it.
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (1 Ibid.) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        let notes = footnotes(&blocks, &rules, MEDIA);
        assert_eq!(notes.len(), 1, "a short but linked note is still a note");
        assert!(notes[0].marker_site.is_some());
    }

    #[test]
    fn an_unlinked_folio_is_rejected_by_the_length_test() {
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page here) Tj ET\n",
                700 - i * 12
            ));
        }
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (19) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        assert!(footnotes(&blocks, &rules, MEDIA).is_empty(), "a folio is not a footnote");
    }

    #[test]
    fn a_marker_abutting_the_word_before_it_is_still_found() {
        // The case that scored zero across the whole corpus when matching went
        // through words: no space before the superscript, so §7.4 segments
        // `marker1` as one word and the numeral is unreachable.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure with a marker) Tj ET\n",
                700 - i * 12
            ));
        }
        // Directly abutting: the previous line ends at x=307, this starts there.
        c.push_str("BT /F1 10 Tf 1 0 0 1 72 640 Tm (annotated) Tj /F1 6 Tf 3 Ts (1) Tj ET\n");
        c.push_str("72 120 m 172 120 l S\n");
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (1 The note itself) Tj ET\n");

        let (blocks, rules, _) = build_page(&c);
        let notes = footnotes(&blocks, &rules, MEDIA);
        assert_eq!(notes.len(), 1);
        let links = link_markers(&blocks, &notes);
        assert!(links[0].is_some(), "an abutting marker must still be found");
    }

    #[test]
    fn a_symbol_marked_footnote_is_found() {
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page here) Tj ET\n",
                700 - i * 12
            ));
        }
        c.push_str("72 120 m 172 120 l S\n");
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (* A starred note) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        let notes = footnotes(&blocks, &rules, MEDIA);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].marker.as_deref(), Some("*"));
    }

    #[test]
    fn small_bottom_text_with_no_marker_is_not_a_footnote() {
        // Test-plusminus.pdf: an engineering drawing whose title block sets
        // DRAWN / CHECKED / SCALE in small type between short rules. Position,
        // size and separation all fire; only the missing marker rejects it.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page here) Tj ET\n",
                700 - i * 12
            ));
        }
        c.push_str("72 120 m 172 120 l S\n");
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (DRAWN) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        assert!(footnotes(&blocks, &rules, MEDIA).is_empty());
    }

    #[test]
    fn body_text_at_the_page_bottom_is_not_a_footnote() {
        // Same position, same size as the body: the size test is what makes it
        // a footnote, and it must not fire on ordinary text that happens to
        // reach the bottom of the page.
        let mut c = String::new();
        for i in 0..20 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm (ordinary body text here) Tj ET\n",
                700 - i * 30
            ));
        }
        let (blocks, rules, _) = build_page(&c);
        assert!(footnotes(&blocks, &rules, MEDIA).is_empty());
    }

    #[test]
    fn a_full_width_rule_does_not_count_as_a_footnote_separator() {
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm \
                 (body text running the full measure of the page here) Tj ET\n",
                700 - i * 12
            ));
        }
        // Full width: a border, not a footnote rule.
        c.push_str("72 120 m 540 120 l S\n");
        c.push_str("BT /F1 7 Tf 1 0 0 1 72 100 Tm (1 smaller text) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        let notes = footnotes(&blocks, &rules, MEDIA);
        // Still a footnote candidate by size and position, but the separator
        // claim must be honest.
        assert!(notes.iter().all(|n| !n.separated_by_rule));
    }

    #[test]
    fn a_slightly_smaller_line_is_not_a_footnote() {
        // 9.5pt under 10pt body is an optical correction, not a note.
        let mut c = String::new();
        for i in 0..6 {
            c.push_str(&format!(
                "BT /F1 10 Tf 1 0 0 1 72 {} Tm (body text goes here now) Tj ET\n",
                700 - i * 12
            ));
        }
        c.push_str("BT /F1 9.5 Tf 1 0 0 1 72 100 Tm (a caption perhaps) Tj ET\n");
        let (blocks, rules, _) = build_page(&c);
        assert!(footnotes(&blocks, &rules, MEDIA).is_empty());
    }
}
