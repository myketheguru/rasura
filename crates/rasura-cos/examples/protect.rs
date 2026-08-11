//! Protecting a document, end to end, for someone else to check. Spec 5.5.
//!
//! Encryption is the one area where testing against your own inverse proves
//! *nothing*. A key derivation that is wrong in a self-consistent way encrypts
//! and decrypts perfectly and produces a file no other reader can open — and
//! the failure is silent until someone tries. So this writes real files and
//! leaves them for pdf.js and pdfium, neither of which has any stake in
//! agreeing with us.
//!
//! Four outputs:
//!
//! | File | What it is |
//! |---|---|
//! | `plain.pdf` | the input, unprotected |
//! | `aes256.pdf` | `/V` 5 `/R` 6, password `hunter2` |
//! | `aes128.pdf` | `/V` 4 `/R` 4, password `hunter2` |
//! | `unprotected.pdf` | `aes256.pdf` with the protection removed again |
//!
//! ```text
//! cargo run -p rasura-cos --example protect -- target/protect
//! node harness/textdiff/validate-injected.mjs target/protect/aes256.pdf \
//!     --password hunter2 "Account balance: 4,200"
//! ```
//!
//! The round trip through `unprotected.pdf` is not decoration. Removing
//! protection is the direction where a mistake is invisible in the obvious
//! check: a file whose `/Encrypt` was dropped while its streams stayed
//! ciphertext still opens, still passes a structural check, and renders
//! nothing.

use rasura_cos::protect::{Entropy, Policy, Strength, protect, unprotect};
use rasura_cos::testutil::ClassicBuilder;
use rasura_cos::{Document, ObjId, OpenOptions, SaveMode, SaveOptions};

const PASSWORD: &str = "hunter2";
const VISIBLE: &str = "Account balance: 4,200";

/// One page and an `/Info` title, both carrying text worth protecting.
fn document() -> Vec<u8> {
    let content = format!("BT /F1 18 Tf 1 0 0 1 72 700 Tm ({VISIBLE}) Tj ET\n").into_bytes();

    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "", &content)
        .object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        )
        .object(6, "<< /Title (Quarterly statement) >>")
        .finish("/Root 1 0 R /Info 6 0 R")
}

/// Entropy for a demonstration. A real caller passes bytes from the platform's
/// CSPRNG — `crypto.getRandomValues` in a browser, `getrandom` on a server.
/// This crate deliberately has no RNG of its own; see `protect`'s module note.
fn demo_entropy() -> Entropy {
    let bytes: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11));
    Entropy::new(bytes).expect("entropy")
}

fn main() -> std::process::ExitCode {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| "target/protect".into());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("FAIL: {out_dir}: {e}");
        return std::process::ExitCode::FAILURE;
    }

    let plain = document();
    let mut written: Vec<(String, Vec<u8>)> = vec![("plain.pdf".into(), plain.clone())];

    for (name, strength) in [("aes256.pdf", Strength::Aes256), ("aes128.pdf", Strength::Aes128)] {
        let mut doc = match Document::open(plain.clone()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL: the fixture did not open: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        let policy = Policy { user_password: PASSWORD.into(), strength, ..Policy::default() };
        let report = match protect(&mut doc, &policy, &demo_entropy()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("FAIL: protect: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        println!("{name}: {:?}", report.strength);
        for w in &report.weaknesses {
            println!("  reported: {w:?}");
        }

        // Asking for an incremental save on purpose. Appending would leave
        // every existing object under no key at all while the trailer claims
        // protection -- not a weaker file, an unreadable one.
        let opts = SaveOptions { mode: Some(SaveMode::Incremental), ..SaveOptions::default() };
        let saved = match rasura_cos::save(&doc, &opts) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("FAIL: save: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        if saved.mode != SaveMode::FullRewrite {
            eprintln!("FAIL: {name} was saved incrementally");
            return std::process::ExitCode::FAILURE;
        }
        for warning in &saved.warnings {
            println!("  warning: {warning}");
        }

        // The claim the whole module exists for: the text is not in the bytes.
        if find(&saved.bytes, VISIBLE.as_bytes()) {
            eprintln!("FAIL: {name} still contains the page text in the clear");
            return std::process::ExitCode::FAILURE;
        }
        if find(&saved.bytes, b"Quarterly statement") {
            eprintln!("FAIL: {name} still contains the /Info title in the clear");
            return std::process::ExitCode::FAILURE;
        }

        // ...and it is there again for whoever has the password.
        let opts = OpenOptions { password: PASSWORD.into(), ..OpenOptions::default() };
        let reopened = match Document::open_with(saved.bytes.clone(), &opts) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL: {name} does not open with its own password: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        match reopened.decoded_stream(ObjId::new(4, 0)) {
            Ok(bytes) if String::from_utf8_lossy(&bytes).contains(VISIBLE) => {}
            Ok(_) => {
                eprintln!("FAIL: {name} decrypted to the wrong content");
                return std::process::ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("FAIL: {name} content: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        if Document::open(saved.bytes.clone()).is_ok() {
            eprintln!("FAIL: {name} opens with no password");
            return std::process::ExitCode::FAILURE;
        }
        println!("  {} bytes, opens only with the password", saved.bytes.len());

        written.push((name.to_string(), saved.bytes));
    }

    // And back out again. The direction where a mistake is invisible: dropping
    // /Encrypt while leaving the streams as ciphertext produces a file that
    // opens and renders nothing.
    let aes256 = written.iter().find(|(n, _)| n == "aes256.pdf").map(|(_, b)| b.clone());
    if let Some(bytes) = aes256 {
        let opts = OpenOptions { password: PASSWORD.into(), ..OpenOptions::default() };
        let mut doc = match Document::open_with(bytes, &opts) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL: reopen: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        if let Err(e) = unprotect(&mut doc) {
            eprintln!("FAIL: unprotect: {e}");
            return std::process::ExitCode::FAILURE;
        }
        let saved = match rasura_cos::save(&doc, &SaveOptions::default()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("FAIL: save: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        let back = match Document::open(saved.bytes.clone()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL: the unprotected file does not open: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        if back.is_encrypted() {
            eprintln!("FAIL: the unprotected file still declares /Encrypt");
            return std::process::ExitCode::FAILURE;
        }
        match back.decoded_stream(ObjId::new(4, 0)) {
            Ok(b) if String::from_utf8_lossy(&b).contains(VISIBLE) => {
                println!("unprotected.pdf: content survived the round trip");
            }
            _ => {
                eprintln!("FAIL: the unprotected file's content did not survive");
                return std::process::ExitCode::FAILURE;
            }
        }
        written.push(("unprotected.pdf".into(), saved.bytes));
    }

    for (name, bytes) in &written {
        let path = format!("{out_dir}/{name}");
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("FAIL: {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote:   {path} ({} bytes)", bytes.len());
    }

    std::process::ExitCode::SUCCESS
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
