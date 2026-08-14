# rasura-cos

[![crates.io](https://img.shields.io/crates/v/rasura-cos.svg)](https://crates.io/crates/rasura-cos)
[![docs.rs](https://docs.rs/rasura-cos/badge.svg)](https://docs.rs/rasura-cos)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**The object layer.**
Objects, the cross-reference table, stream filters, decryption and the writer.
The bottom of the stack: it knows what a PDF object is and nothing about what a
paragraph is.

Use this crate directly if you want the object model without the document model.

`ust
use rasura_cos::{Document, SaveOptions};

let doc = Document::open(std::fs::read("in.pdf")?)?;
println!("{} revisions", doc.revisions().len());

// Every specification deviation tolerated to open the file.
for l in doc.leniencies() {
    println!("{} at {}: {}", l.kind, l.offset, l.detail);
}
`

Reads classic tables, cross-reference streams, object streams and hybrid files,
and rebuilds the table by scanning when the real one cannot be followed. Reads
RC4 and AES encryption at every revision from 2 to 6; writes AES-256 only.

The writer keeps the byte span each object occupied in the source file and
replays anything unmodified verbatim, which is the mechanism behind the
library's central property: an unedited save returns the input byte for byte.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.