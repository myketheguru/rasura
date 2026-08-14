# rasura-flow

[![crates.io](https://img.shields.io/crates/v/rasura-flow.svg)](https://crates.io/crates/rasura-flow)
[![docs.rs](https://docs.rs/rasura-flow/badge.svg)](https://docs.rs/rasura-flow)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**Layout and composition.**
The flow model, the layout engine, and composing documents that did not exist.

`ust
use rasura_flow::compose::{compose, Options};

let (doc, report) = compose(&flow, &font_bytes, &Options::default())?;
println!("{} pages, set in {}", report.pages, report.base_font);
`

The layout engine does measure, leading, line breaking, pagination, multiple
columns, widows and orphans, and keeping a heading with the section under it.
Text is broken to the width it is drawn at, because the widths used for breaking
come from the same table the font dictionary's own widths are written from.

Also runs in the other direction: read a document into a flow model, compare two
models, and check that a layout round-trips without losing structure.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.