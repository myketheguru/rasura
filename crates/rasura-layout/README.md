# rasura-layout

[![crates.io](https://img.shields.io/crates/v/rasura-layout.svg)](https://crates.io/crates/rasura-layout)
[![docs.rs](https://docs.rs/rasura-layout/badge.svg)](https://docs.rs/rasura-layout)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**Reconstruction.**
Glyph runs to lines to blocks to a document model. This is the part that makes
editing possible: a PDF contains positioned glyphs, and a paragraph has to be
rebuilt from them before anything can change a word in it.

`ust
let pages = rasura_content::page::pages(&doc)?;
let text = rasura_layout::page_text(&doc, &pages.pages[0]);
`

Six steps, each reporting rather than assuming: extract runs, resolve each glyph
to a character through seven strategies, segment words, assemble lines, cut the
page into blocks and columns, and reconstruct paragraphs with their alignment,
leading and hyphenation.

Reading order comes from the structure tree when the document has one and from
geometry when it does not, and which was used is reported.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.