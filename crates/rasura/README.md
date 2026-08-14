# rasura

[![crates.io](https://img.shields.io/crates/v/rasura.svg)](https://crates.io/crates/rasura)
[![docs.rs](https://docs.rs/rasura/badge.svg)](https://docs.rs/rasura)
[![docs](https://img.shields.io/badge/guide-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
**True PDF editing.**
Read a PDF as paragraphs and blocks, change them, and write the file back with
the untouched bytes byte-identical. Also composes documents that did not exist.

`ust
use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("in.pdf")?)?;
let page = doc.page(0)?;
println!("{}", page.paragraphs()[0].text);

let mut session = doc.edit();
session.replace_text(&page, page.paragraphs()[0].id, 0..5, "Hello")?;
let saved = session.commit(&SaveOptions::default())?;
`

Or make one:

`ust
use rasura::create::{Content, Options};

let (doc, report) = Document::create(
    &[Content::heading(1, "Report"), Content::paragraph("Revenue rose.")],
    &Options::with_font(std::fs::read("Inter-Regular.ttf")?),
)?;
`

## Three rules

1. **Non-locality is forbidden.** An edit on page 40 does not change any other
   page by a pixel, nor alter the bytes of any object it did not need to touch.
   An unedited save returns the input byte for byte.
2. **Fidelity is reported, never assumed.** Operations return the rung they
   reached: `exact`, `reembedded`, `substituted` or `overlaid`. Set a
   floor and anything below it is refused rather than quietly degraded.
3. **The file stays a valid PDF.** Output passes `qpdf --check` and opens in
   Acrobat, Preview, Chrome and Firefox without repair prompts.

## What it refuses

Rendering (pair with pdf.js), scanned documents, XFA forms, and creating digital
signatures. Each is a decision with a reason rather than a gap.

## In the browser

This crate compiles to WebAssembly. For JavaScript, install
[`rasura`](https://www.npmjs.com/package/rasura) from npm.
Part of [Rasura](https://github.com/myketheguru/rasura), a browser-native SDK for
true PDF editing. Guides and API reference: <https://myketheguru.github.io/rasura/>

Licensed under MIT or Apache-2.0, at your option.