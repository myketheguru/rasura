//! Spec 14.4: the lexer must never panic, never infinite-loop, and never
//! allocate unboundedly on adversarial input.
//!
//! Run with `cargo +nightly fuzz run object_lexer`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rasura_cos::lexer::{Lexer, Token};
use rasura_cos::parser::Parser;

fuzz_target!(|data: &[u8]| {
    // The lexer must always terminate: every call either consumes a byte or
    // reports Eof. A hang here is as much a bug as a panic.
    let mut lx = Lexer::new(data);
    let mut steps = 0usize;
    loop {
        let t = lx.next_token();
        if t.token == Token::Eof {
            break;
        }
        steps += 1;
        assert!(steps <= data.len() + 1, "lexer did not make progress");
    }

    // And the parser must decline malformed input rather than panic.
    let _ = Parser::new(data).parse_object();
});
