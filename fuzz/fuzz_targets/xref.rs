//! Cross-reference parsing and the recovery scan.
//!
//! The recovery path walks the whole buffer looking for object headers, so it is
//! the routine most exposed to pathological input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rasura_cos::{recovery, xref};

fuzz_target!(|data: &[u8]| {
    let header = xref::find_header(data).unwrap_or(0);
    let _ = xref::load(data, header);
    let _ = recovery::reconstruct(data);
});
