//! Every filter decoder, on arbitrary bytes.
//!
//! Decompression bombs are the specific worry here: a few hundred bytes of
//! Flate can claim to expand to gigabytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rasura_cos::filters::{self, FilterChain};
use rasura_cos::object::Object;

fuzz_target!(|data: &[u8]| {
    for name in [
        "FlateDecode",
        "LZWDecode",
        "ASCIIHexDecode",
        "ASCII85Decode",
        "RunLengthDecode",
    ] {
        let chain = FilterChain::build(Some(&Object::name(name)), None);
        let _ = filters::decode(&chain, data);
    }

    // Predictors, which index into buffers and are the likeliest source of a
    // panic on a malformed /DecodeParms.
    for predictor in [2i64, 10, 12, 15] {
        let mut parms = rasura_cos::object::Dictionary::new();
        parms.insert(
            rasura_cos::object::Name::new("Predictor"),
            Object::Integer(predictor),
        );
        parms.insert(rasura_cos::object::Name::new("Colors"), Object::Integer(3));
        parms.insert(rasura_cos::object::Name::new("Columns"), Object::Integer(7));
        let chain = FilterChain::build(
            Some(&Object::name("FlateDecode")),
            Some(&Object::Dictionary(parms)),
        );
        let _ = filters::decode(&chain, data);
    }
});
