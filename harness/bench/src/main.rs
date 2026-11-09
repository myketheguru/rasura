//! Runner for the performance benchmarks. Spec 13.
//!
//! ```text
//! cargo run -p rasura-bench                      # measure, print, gate
//! cargo run -p rasura-bench -- --update-baseline # record this machine's baseline
//! ```
//!
//! Exits non-zero when an operation has regressed more than 20% against the
//! baseline, so CI can gate on it. With no baseline recorded it prints the
//! measurements and succeeds, because there is nothing yet to regress against.

use rasura_bench::{format_baseline, parse_baseline, regressions, run};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.txt")
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn ms_ns(ns: u128) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.0)
}

fn main() -> ExitCode {
    let update = std::env::args().skip(1).any(|a| a == "--update-baseline");
    let samples = run();

    println!(
        "{:<32} {:<16} {:>11} {:>14}",
        "operation", "document", "median", "budget (wasm)"
    );
    println!("{}", "-".repeat(76));
    for s in &samples {
        println!(
            "{:<32} {:<16} {:>11} {:>14}",
            s.name,
            s.document,
            ms(s.median),
            ms(s.budget)
        );
    }
    println!(
        "\nbudgets are spec 13's, measured in WASM on a 2020 laptop — shown for \
         reference, not gated (see the crate docs)."
    );

    let path = baseline_path();
    if update {
        return match std::fs::write(&path, format_baseline(&samples)) {
            Ok(()) => {
                println!("wrote baseline to {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error writing baseline: {e}");
                ExitCode::FAILURE
            }
        };
    }

    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let regs = regressions(&samples, &parse_baseline(&text));
            if regs.is_empty() {
                println!("no regression over 20% against the baseline.");
                ExitCode::SUCCESS
            } else {
                eprintln!("\n{} operation(s) regressed more than 20%:", regs.len());
                for r in &regs {
                    let pct = (r.now_ns as f64 / r.was_ns as f64 - 1.0) * 100.0;
                    eprintln!("  {}: {} -> {} (+{pct:.0}%)", r.name, ms_ns(r.was_ns), ms_ns(r.now_ns));
                }
                ExitCode::FAILURE
            }
        }
        Err(_) => {
            println!("no baseline at {} — record one with --update-baseline.", path.display());
            ExitCode::SUCCESS
        }
    }
}
