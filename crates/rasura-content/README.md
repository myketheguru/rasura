# rasura-content

[![crates.io](https://img.shields.io/crates/v/rasura-content.svg)](https://crates.io/crates/rasura-content)
[![docs.rs](https://docs.rs/rasura-content/badge.svg)](https://docs.rs/rasura-content)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**Content streams.**
The content-stream tokenizer, the graphics and text state machine, optional
content groups, and the serializer that writes operators back.

Sits above `rasura-cos` and below the document model. It knows what `Tj` and
`cm` mean and nothing about what a paragraph is.

`ust
let pages = rasura_content::page::pages(&doc)?;
let text = rasura_content::text::extract_page(&doc, &pages.pages[0]);
`

Tracks the full graphics state including clipping, shading and patterns, and
reports where a clip could only be approximated rather than pretending it was
exact.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.