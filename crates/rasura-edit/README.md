# rasura-edit

[![crates.io](https://img.shields.io/crates/v/rasura-edit.svg)](https://crates.io/crates/rasura-edit)
[![docs.rs](https://docs.rs/rasura-edit/badge.svg)](https://docs.rs/rasura-edit)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**Edit operations.**
Text replacement with reflow, image and page operations, annotations, form
fields, redaction, and the stream patching underneath all of it.

`ust
let mut session = doc.edit();
let outcome = session.replace_text(&page, id, 0..5, "Hello")?;
println!("{:?}", outcome.fidelity);
`

Every operation returns the fidelity rung it reached rather than throwing on
degradation, and a session can be given a floor below which operations are
refused instead.

Redaction removes content and then proves it, forcing a full rewrite so the
original bytes cannot survive in an earlier revision. An image overlapping the
text refuses the operation, because image data is not searched.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.