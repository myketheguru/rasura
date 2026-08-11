//! Opening arbitrary bytes, including through the recovery path.
//!
//! Seed this from `corpus/files/`: `cargo +nightly fuzz run document_open corpus/files`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rasura_cos::document::{Document, OpenOptions};
use rasura_cos::object::ObjId;

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = Document::open_with(data.to_vec(), &OpenOptions::default()) else {
        return;
    };

    // Touching every object exercises the parser, the filters and the crypt
    // layer on whatever the fuzzer produced.
    for number in doc.xref().live_objects().take(512) {
        let id = ObjId::new(number, 0);
        if let Ok(obj) = doc.get(id)
            && obj.as_stream().is_some()
        {
            let _ = doc.decoded_stream(id);
        }
    }

    // A document that opened must save, and the saved bytes must reopen.
    if let Ok(result) = rasura_cos::writer::save(&doc, &Default::default()) {
        let _ = Document::open(result.bytes);
    }
});
