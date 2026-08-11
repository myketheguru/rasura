//! Runner for the invariant suite.
//!
//! ```text
//! cargo run -p rasura-invariants               # seed corpus + corpus/files
//! cargo run -p rasura-invariants -- path/to/dir
//! cargo run -p rasura-invariants -- --write-seed corpus/files
//! ```
//!
//! Exits non-zero on any failure, so CI can gate on it.

use rasura_invariants::{Status, check_file, check_full_rewrite, seed_corpus};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(pos) = args.iter().position(|a| a == "--write-seed") {
        let dir =
            args.get(pos + 1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("corpus/files"));
        return match write_seed(&dir) {
            Ok(n) => {
                println!("wrote {n} seed files to {}", dir.display());
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    // `--why <text>` names the files behind a summary line. A skip count says
    // how many were not checked and never which, and the difference matters
    // most for a reason that might be a defect in disguise -- there is no way
    // to audit "8 input defects" without being told where they are.
    let why_at = args.iter().position(|a| a == "--why");
    let why: Option<String> = why_at.and_then(|i| args.get(i + 1)).cloned();
    // The flag *and its value* come out, or the value is read as a directory to
    // scan -- which fails with a path error that says nothing about the cause.
    let args: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && Some(*i) != why_at.map(|w| w + 1))
        .map(|(_, a)| a.clone())
        .collect();

    let mut files: Vec<(String, Vec<u8>)> =
        seed_corpus().into_iter().map(|(n, b)| (format!("seed/{n}"), b)).collect();

    // `corpus/files` is ours; `corpus/external` holds other people's corpora,
    // fetched by corpus/fetch.sh and never committed.
    let dirs: Vec<PathBuf> = if args.is_empty() {
        vec![PathBuf::from("corpus/files"), PathBuf::from("corpus/external")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    for dir in &dirs {
        match collect_pdfs(dir) {
            Ok(found) => files.extend(found),
            Err(e) if args.is_empty() => {
                // The corpus directory is git-lfs and may be absent on a fresh
                // clone. Say so rather than pretending coverage.
                println!("note: {} not read ({e}); running the seed corpus only", dir.display());
            }
            Err(e) => {
                eprintln!("error: {}: {e}", dir.display());
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let mut failed = 0usize;
    let mut passed = 0usize;
    let mut skipped = 0usize;
    // Per invariant, so coverage is reported rather than inferred. A total
    // skip count says how much was not checked; it does not say *what*, and an
    // invariant that quietly skips most of the corpus looks identical to one
    // that passes it.
    let mut coverage: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
    // And *why* each skip happened. "Not checked" is only acceptable when the
    // reason is one nobody could do anything about; a skip whose reason is a
    // failure in disguise -- "the probe edit did not apply" -- looks exactly
    // like a legitimate one in a count.
    let mut skip_reasons: BTreeMap<&'static str, BTreeMap<String, usize>> = BTreeMap::new();

    for (name, bytes) in &files {
        let report = check_file(name, bytes);
        let rewrite = check_full_rewrite(bytes);
        let any_fail = report.failed() || rewrite.status == Status::Fail;

        if any_fail {
            failed += 1;
            print!("{report}");
            if rewrite.status == Status::Fail {
                println!("       {} FAIL  {}", rewrite.invariant, rewrite.detail);
            }
        } else {
            passed += 1;
        }
        for c in &report.checks {
            if let Some(needle) = &why
                && c.detail.contains(needle.as_str())
            {
                println!("  {name}\n      {} {}", c.invariant, c.detail);
            }
            let entry = coverage.entry(c.invariant).or_default();
            match c.status {
                Status::Pass => entry.0 += 1,
                Status::Fail => entry.1 += 1,
                Status::Skipped => {
                    entry.2 += 1;
                    skipped += 1;
                    // Reasons vary in their tail (file names, byte counts), so
                    // they are grouped by their opening clause.
                    let reason = c
                        .detail
                        .split(&[':', ';', '('][..])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    *skip_reasons.entry(c.invariant).or_default().entry(reason).or_default() += 1;
                }
            }
        }
    }

    println!("\n  invariant                     passed   failed  skipped");
    for (name, (pass, fail, skip)) in &coverage {
        println!("  {name:<28} {pass:>7} {fail:>8} {skip:>8}");
        let mut reasons: Vec<(&String, &usize)> =
            skip_reasons.get(name).map(|m| m.iter().collect()).unwrap_or_default();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (reason, n) in reasons.iter().take(3) {
            println!("  {:<28} {:>7}   {reason}", "", n);
        }
    }

    println!(
        "\n{} file(s): {passed} green, {failed} failing, {skipped} check(s) skipped",
        files.len()
    );
    if failed > 0 {
        println!("\nSpec 17: Phase 1 does not exit with I1 failures.");
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

fn collect_pdfs(dir: &Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_pdfs(&path)?);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            let bytes = std::fs::read(&path)?;
            out.push((path.display().to_string(), bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn write_seed(dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut n = 0;
    for (name, bytes) in seed_corpus() {
        std::fs::write(dir.join(format!("{name}.pdf")), &bytes)?;
        n += 1;
    }
    Ok(n)
}
