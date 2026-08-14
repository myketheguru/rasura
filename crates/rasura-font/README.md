# rasura-font

[![crates.io](https://img.shields.io/crates/v/rasura-font.svg)](https://crates.io/crates/rasura-font)
[![docs.rs](https://docs.rs/rasura-font/badge.svg)](https://docs.rs/rasura-font)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**Fonts.**
Parsing, shaping, subsetting, glyph injection and embedding for all five font
containers a PDF can carry: Type 1, TrueType, CFF, CID-keyed CFF and OpenType.

`ust
use rasura_font::create::{Options, embed_truetype};

// Embed a typeface a document has never seen, subset to what it draws.
let embedded = embed_truetype(&font_bytes, &Options::for_text("Hello"), next_id)?;
println!("{} glyphs, {}", embedded.base_font, if embedded.composite { "Type0" } else { "simple" });
`

Two things it does that are unusual. **Glyph injection** adds an outline to a
font a document already embeds, extending its `cmap`, `/Widths` and
`/ToUnicode`, so new text is set in the same typeface as its neighbours rather
than in a second font. **Embedding** synthesises a complete `/FontDescriptor`
from the font program, choosing a simple or a Type0 font from the text.

`/StemV` is estimated and says so: no sfnt table records it.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.