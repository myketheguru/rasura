//! Runner for the spec §18 Q1 measurement.
//!
//! ```text
//! cargo run --release -p rasura-fontsurvey                 # all corpora
//! cargo run --release -p rasura-fontsurvey -- path/to/dir
//! cargo run --release -p rasura-fontsurvey -- --csv out.csv path/to/dir
//! ```

use rasura_cos::document::{Document, OpenOptions};
use rasura_fontsurvey::{
    DocumentSurvey, Fallback, FontKind, GlyphNameStyle, ToUnicodeState, survey,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    let mut csv: Option<PathBuf> = None;
    if let Some(pos) = args.iter().position(|a| a == "--csv") {
        if pos + 1 >= args.len() {
            eprintln!("--csv needs a path");
            return std::process::ExitCode::FAILURE;
        }
        csv = Some(PathBuf::from(args.remove(pos + 1)));
        args.remove(pos);
    }

    let dirs: Vec<PathBuf> = if args.is_empty() {
        vec![PathBuf::from("corpus/files"), PathBuf::from("corpus/external")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        match collect_pdfs(dir) {
            Ok(found) => files.extend(found),
            Err(e) => println!("note: {} not read ({e})", dir.display()),
        }
    }
    if files.is_empty() {
        eprintln!("no PDFs found");
        return std::process::ExitCode::FAILURE;
    }

    let mut surveys: Vec<DocumentSurvey> = Vec::new();
    let mut unopened = 0usize;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else { continue };
        match Document::open_with(bytes, &OpenOptions::default()) {
            Ok(doc) => surveys.push(survey(&path.display().to_string(), &doc)),
            Err(_) => unopened += 1,
        }
    }

    report(&surveys, files.len(), unopened);

    if let Some(path) = csv
        && let Err(e) = write_csv(&path, &surveys)
    {
        eprintln!("csv: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn report(surveys: &[DocumentSurvey], total_files: usize, unopened: usize) {
    // The question is about *embedded* fonts. A non-embedded Helvetica has no
    // subset to be missing from, and its encoding is known, so it is not what
    // Q1 is asking about. Type 3 fonts have no font program at all and are
    // counted separately for the same reason.
    let embedded: Vec<_> = surveys
        .iter()
        .flat_map(|s| s.fonts.iter().map(move |f| (s, f)))
        .filter(|(_, f)| f.embedded && f.kind != FontKind::Type3)
        .collect();

    println!("=== spec 18 Q1: /ToUnicode coverage ===\n");
    println!("{total_files} file(s) scanned, {unopened} would not open");
    println!("{} document(s) surveyed", surveys.len());
    println!(
        "{} embedded font(s), {} of which are subsets\n",
        embedded.len(),
        embedded.iter().filter(|(_, f)| f.subset).count()
    );

    if embedded.is_empty() {
        println!("no embedded fonts found; nothing to measure");
        return;
    }

    let usable = embedded.iter().filter(|(_, f)| f.tounicode == ToUnicodeState::Usable).count();
    let pct = 100.0 * usable as f64 / embedded.len() as f64;

    println!("--- overall ---");
    let mut states: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, f) in &embedded {
        *states.entry(f.tounicode.as_str()).or_default() += 1;
    }
    for (state, n) in &states {
        println!("  {state:<14} {n:>6}  {:>5.1}%", 100.0 * *n as f64 / embedded.len() as f64);
    }
    println!("\n  USABLE COVERAGE: {usable}/{} = {pct:.1}%", embedded.len());
    println!(
        "  spec threshold:  85.0%  ->  {}",
        if pct < 85.0 {
            "BELOW: glyph-name heuristics (7.2 step 6) become Phase 3 work"
        } else {
            "at or above: step 6 can stay in Phase 8"
        }
    );

    println!("\n--- by producer family (embedded fonts) ---");
    println!("  {:<20} {:>7} {:>8} {:>9}", "producer", "fonts", "usable", "coverage");
    let mut by_producer: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (s, f) in &embedded {
        let e = by_producer.entry(s.producer_family).or_default();
        e.0 += 1;
        if f.tounicode == ToUnicodeState::Usable {
            e.1 += 1;
        }
    }
    let mut rows: Vec<_> = by_producer.into_iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (family, (n, ok)) in rows {
        println!("  {family:<20} {n:>7} {ok:>8} {:>8.1}%", 100.0 * ok as f64 / n as f64);
    }

    println!("\n--- by font kind (embedded fonts) ---");
    let mut by_kind: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (_, f) in &embedded {
        let e = by_kind.entry(f.kind.as_str()).or_default();
        e.0 += 1;
        if f.tounicode == ToUnicodeState::Usable {
            e.1 += 1;
        }
    }
    for (kind, (n, ok)) in by_kind {
        println!("  {kind:<20} {n:>7} {ok:>8} {:>8.1}%", 100.0 * ok as f64 / n as f64);
    }

    // The decision-grade part: for fonts where strategy 1 fails, what do the
    // later strategies in 7.2 have to work with?
    let failing: Vec<_> =
        embedded.iter().filter(|(_, f)| f.tounicode != ToUnicodeState::Usable).collect();
    println!(
        "\n--- fallback available for the {} font(s) without usable /ToUnicode ---",
        failing.len()
    );
    if failing.is_empty() {
        println!("  (none)");
    } else {
        let mut by_fallback: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, f) in &failing {
            *by_fallback.entry(f.fallback.as_str()).or_default() += 1;
        }
        for (fb, n) in &by_fallback {
            println!("  {fb:<26} {n:>6}  {:>5.1}%", 100.0 * *n as f64 / failing.len() as f64);
        }
        let needs_step6 =
            failing.iter().filter(|(_, f)| f.fallback == Fallback::FontProgramOnly).count();
        println!(
            "\n  {needs_step6} font(s) ({:.1}% of all embedded) have nothing at the PDF level\n  \
             and fall through to 7.2 step 5 (embedded cmap) or step 6 (glyph names).",
            100.0 * needs_step6 as f64 / embedded.len() as f64
        );

        // Step 2 says "glyph name -> Unicode via the Adobe Glyph List", which
        // only works when the names are AGL names. This is what decides how
        // much of step 6 has to exist.
        println!("\n--- glyph names in /Differences, for those same fonts ---");
        let mut by_names: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, f) in &failing {
            *by_names.entry(f.glyph_names.as_str()).or_default() += 1;
        }
        for (style, n) in &by_names {
            println!("  {style:<22} {n:>6}  {:>5.1}%", 100.0 * *n as f64 / failing.len() as f64);
        }
        let opaque = failing
            .iter()
            .filter(|(_, f)| {
                matches!(f.glyph_names, GlyphNameStyle::Opaque | GlyphNameStyle::Mixed)
            })
            .count();
        println!(
            "\n  {opaque} font(s) ({:.1}% of all embedded) carry opaque glyph names.\n  \
             These are the ones the Adobe Glyph List cannot rescue.",
            100.0 * opaque as f64 / embedded.len() as f64
        );
    }

    report_programs(&embedded);
}

/// Spec 8.2: how much of the corpus's embedded font programs can be read at
/// all. Nothing in Phase 4 above parsing -- shaping, injection, substitution --
/// can be built on a font that will not open, so this is the number that says
/// how much of the phase is reachable.
fn report_programs(embedded: &[(&DocumentSurvey, &rasura_fontsurvey::FontRecord)]) {
    let programs: Vec<&rasura_fontsurvey::ProgramRecord> =
        embedded.iter().filter_map(|(_, f)| f.program.as_ref()).collect();
    if programs.is_empty() {
        return;
    }

    println!("\n--- embedded font programs (spec 8.2) ---");
    let mut by_flavour: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for p in &programs {
        let e = by_flavour.entry(p.flavour).or_default();
        e.0 += 1;
        if p.parsed.is_ok() {
            e.1 += 1;
        }
    }
    for (flavour, (total, ok)) in &by_flavour {
        println!(
            "  {flavour:<16} {total:>6} found, {ok:>6} parsed  {:>5.1}%",
            100.0 * *ok as f64 / *total as f64
        );
    }

    let parsed = programs.iter().filter(|p| p.parsed.is_ok()).count();
    let glyphs: usize = programs.iter().filter_map(|p| p.parsed.as_ref().ok()).sum();
    println!(
        "\n  {parsed}/{} program(s) parsed ({:.1}%), {glyphs} glyph(s) reachable",
        programs.len(),
        100.0 * parsed as f64 / programs.len() as f64
    );

    // A file declaring one format and embedding another is not rare, and a
    // parser that trusted the declaration would lose every one of them.
    let mislabelled = programs.iter().filter(|p| p.mislabelled).count();
    if mislabelled > 0 {
        println!(
            "  {mislabelled} program(s) ({:.1}%) do not match their declared /Subtype; \
             the bytes were believed over the declaration.",
            100.0 * mislabelled as f64 / programs.len() as f64
        );
    }

    // "Parsed" is not "parsed correctly". A Type 1 charstring must open with
    // hsbw or sbw; get lenIV wrong and parsing still succeeds, yielding bytes
    // that are merely shifted. This is the only thing that notices.
    let scored: Vec<f64> = programs.iter().filter_map(|p| p.soundness).collect();
    if !scored.is_empty() {
        let mean = scored.iter().sum::<f64>() / scored.len() as f64;
        let perfect = scored.iter().filter(|s| **s >= 0.999).count();
        let poor = scored.iter().filter(|s| **s < 0.9).count();
        println!(
            "\n  Type 1 charstring soundness: mean {:.3}, {perfect}/{} font(s) at 1.000, \
             {poor} below 0.900",
            mean,
            scored.len()
        );
        let mut worst: Vec<(f64, &str, &str)> = embedded
            .iter()
            .filter_map(|(d, f)| {
                f.program
                    .as_ref()
                    .and_then(|p| p.soundness)
                    .map(|s| (s, d.name.as_str(), f.base_font.as_str()))
            })
            .filter(|(s, _, _)| *s < 0.9)
            .collect();
        worst.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (score, file, font) in worst.iter().take(6) {
            let short = file.rsplit(['/', '\\']).next().unwrap_or(file);
            println!("    {score:.3}  {short} :: {font}");
        }
    }

    // Spec 8.2 lists "/Encoding from the font program" as required, and §7.2
    // has no other source for a symbolic font whose PDF dictionary supplies
    // none -- the gap Symbol and ZapfDingbats have been sitting in since
    // Phase 2.
    let mut encodings: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &programs {
        if let Some(kind) = p.builtin_encoding {
            *encodings.entry(kind).or_default() += 1;
        }
    }
    if !encodings.is_empty() {
        println!("\n  Type 1 built-in encodings:");
        for (kind, n) in &encodings {
            println!("    {kind:<18} {n:>5}");
        }
    }

    // Spec 8.4 step 1, validated against real fonts. A charstring walker is
    // the kind of code that passes every fixture and desynchronises on the
    // first font with nine stem hints.
    let cs: Vec<&rasura_fontsurvey::CharstringStats> =
        programs.iter().filter_map(|p| p.charstrings.as_ref()).collect();
    if !cs.is_empty() {
        let sum = |f: fn(&rasura_fontsurvey::CharstringStats) -> usize| -> usize {
            cs.iter().map(|c| f(c)).sum()
        };
        let total = sum(|c| c.total);
        if total > 0 {
            let pct = |n: usize| n as f64 * 100.0 / total as f64;
            println!("\n  CFF charstrings (spec 8.4 step 1), {total} across {} font(s):", cs.len());
            println!(
                "    walked exactly    {:>7}  {:>5.1}%",
                sum(|c| c.walked_exactly),
                pct(sum(|c| c.walked_exactly))
            );
            println!(
                "    call a subroutine {:>7}  {:>5.1}%",
                sum(|c| c.had_subrs),
                pct(sum(|c| c.had_subrs))
            );
            println!(
                "    inlined           {:>7}  {:>5.1}%",
                sum(|c| c.inlined),
                pct(sum(|c| c.inlined))
            );
            println!(
                "    no call remains   {:>7}  {:>5.1}%",
                sum(|c| c.fully_inlined),
                pct(sum(|c| c.fully_inlined))
            );
            println!(
                "    walk overshot     {:>7}   short {}   (plus {} empty entries a subset left behind)",
                sum(|c| c.walked_over),
                sum(|c| c.walked_short),
                sum(|c| c.empty)
            );
            if let Some(sample) = cs.iter().find_map(|c| c.short_sample.as_ref()) {
                println!("    e.g. {sample}");
            }
        }
    }

    // Spec 8.4 against real fonts. Every embedded program has its own last
    // glyph injected back into it: the tables are a real font's, the outline is
    // a real outline, and the expected answer is known.
    let outcomes: Vec<rasura_fontsurvey::InjectionOutcome> =
        programs.iter().filter_map(|p| p.injection).collect();
    if !outcomes.is_empty() {
        use rasura_fontsurvey::InjectionOutcome::*;
        let count = |want: rasura_fontsurvey::InjectionOutcome| {
            outcomes.iter().filter(|o| **o == want).count()
        };
        let attempted = outcomes.len() - count(Refused);
        println!("\n  Glyph injection (spec 8.4), self-injected into {attempted} font(s):");
        println!("    verified          {:>7}", count(Verified));
        println!("    glyph lost        {:>7}", count(GlyphLost));
        println!("    unreadable output {:>7}", count(Unreadable));
        println!(
            "    refused           {:>7}  (CID-keyed CFF, Type 1, no outlines)",
            count(Refused)
        );
        println!(
            "    target broken     {:>7}  (the font's own loca contradicts itself)",
            count(TargetBroken)
        );
        println!(
            "    font full         {:>7}  (at the CFF ceiling; no edit is possible)",
            count(Full)
        );
        // Neither counts against the rate. A font whose own `loca` contradicts
        // itself cannot be reproduced by anyone, and a font at the format's
        // ceiling has no slot to append to -- both are properties of the input,
        // not of this code, and averaging them in would make the number measure
        // the corpus rather than the library.
        let attempted = attempted - count(TargetBroken) - count(Full);
        if attempted > 0 {
            println!(
                "    -> {:.1}% of attempts round-tripped intact",
                count(Verified) as f64 * 100.0 / attempted as f64
            );
        }
        let mut by_flavour: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for p in programs.iter().filter(|p| p.injection.is_some()) {
            let e = by_flavour.entry(p.flavour).or_default();
            if p.injection != Some(Refused) {
                e.0 += 1;
                if p.injection == Some(Verified) {
                    e.1 += 1;
                }
            }
        }
        for (flavour, (tried, ok)) in &by_flavour {
            if *tried > 0 {
                println!("      {flavour:<12} {ok:>5}/{tried:<5}");
            }
        }
        // The failures, named, *with the file that produced them*. A rounded-off
        // 98% hides whatever the last 2% is doing, and "not diagnosed" is only
        // honest once. Printing the reason without the file is barely better:
        // it says something is wrong and gives nothing to reproduce it with.
        let mut reasons: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for p in programs.iter().filter_map(|p| p.injection_defect.as_ref()) {
            let e = reasons.entry(format!("{}: {}", p.flavour, p.detail)).or_default();
            e.0 += 1;
            if e.1.is_empty() {
                e.1 = p.source.clone();
            }
        }
        for (reason, (n, source)) in reasons.iter().take(12) {
            println!("      {n:>4}x {reason}");
            println!("           in {source}");
        }
    }

    let mut failures: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &programs {
        if let Err(why) = &p.parsed {
            *failures.entry(why.as_str()).or_default() += 1;
        }
    }
    if !failures.is_empty() {
        println!("\n  why the rest did not parse:");
        let mut sorted: Vec<_> = failures.into_iter().collect();
        sorted.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (why, n) in sorted.iter().take(12) {
            println!("    {n:>6}  {why}");
        }
    }
}

fn write_csv(path: &Path, surveys: &[DocumentSurvey]) -> std::io::Result<()> {
    let mut out = String::from(
        "file,producer_family,producer,base_font,kind,embedded,subset,tounicode,mappings,fallback,glyph_names\n",
    );
    for s in surveys {
        for f in &s.fonts {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&s.name),
                s.producer_family,
                csv_escape(&s.producer),
                csv_escape(&f.base_font),
                f.kind.as_str(),
                f.embedded,
                f.subset,
                f.tounicode.as_str(),
                f.mappings,
                csv_escape(f.fallback.as_str()),
                csv_escape(f.glyph_names.as_str()),
            ));
        }
    }
    std::fs::write(path, out)
}

fn csv_escape(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if cleaned.contains([',', '"']) {
        format!("\"{}\"", cleaned.replace('"', "\"\""))
    } else {
        cleaned
    }
}

fn collect_pdfs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_pdfs(&path)?);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
