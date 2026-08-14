# Rasura

[![crates.io](https://img.shields.io/crates/v/rasura.svg)](https://crates.io/crates/rasura)
[![npm](https://img.shields.io/npm/v/rasura.svg)](https://www.npmjs.com/package/rasura)
[![docs](https://img.shields.io/badge/docs-rasura-1f9a63)](https://myketheguru.github.io/rasura/)
[![licence](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

**Edit the text in a PDF. In the browser. Without regenerating the file.**

[Documentation](https://myketheguru.github.io/rasura/) ·
[Live editor](https://myketheguru.github.io/rasura/editor) ·
[Use cases](https://myketheguru.github.io/rasura/use-cases) ·
[Build report](docs/report.md)

---

## The problem

A PDF has no paragraphs. It has no words, and frequently no spaces. What it has
is instructions: draw glyph 36 here, then glyph 82 four units to the right.

That is why every browser PDF library does one of two things. It renders the
page, like pdf.js. Or it draws new content on top of the old, like pdf-lib and
jsPDF. Neither can change a word in a sentence that is already there, because
doing that means rebuilding the sentence from glyph positions first, resolving
each glyph back to a character, re-breaking the lines at the width the document
actually used, and patching the content stream in place.

Rasura does that. It is Rust compiled to WebAssembly, and it runs in the tab.

## Try it

```bash
npm install rasura
```

```js
import { Pdf } from 'rasura'

const doc = await Pdf.open(await file.arrayBuffer())
const page = await doc.page(0)

console.log(page.paragraphs[0].text)
// "Prepared for the board, and for anyone curious about what a PDF editor can see."

const outcome = await doc.replaceText(
  { page: 0, paragraph: page.paragraphs[0].id, from: 0, to: 8 },
  'Written',
)
console.log(outcome.fidelity)  // 'exact'

const { bytes, bytesAppended } = await doc.commit()
// The original file, plus 1,204 bytes. Everything else is byte-identical.
```

Or in Rust:

```toml
[dependencies]
rasura = "0.1"
```

```rust
use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("in.pdf")?)?;
let page = doc.page(0)?;

let mut session = doc.edit();
session.replace_text(&page, page.paragraphs()[0].id, 0..8, "Written")?;
std::fs::write("out.pdf", session.commit(&SaveOptions::default())?.bytes)?;
```

There is a [live editor](https://myketheguru.github.io/rasura/editor) running the
real library. Nothing you open in it is uploaded anywhere, because there is
nowhere to upload it to.

## It also makes documents

Describe the content as blocks and name a typeface. The layout engine decides
the measure, the leading, where lines break, where pages break, and keeps a
heading with the section under it.

```js
const font = await fetch('/Inter-Regular.ttf').then((r) => r.arrayBuffer())

const { document, report } = await Pdf.create(
  [
    { kind: 'heading', level: 1, text: 'Invoice 0042' },
    { kind: 'paragraph', text: 'Due 30 days from receipt.' },
    { kind: 'list', items: ['Design, 12 hours', 'Build, 30 hours'] },
  ],
  font,
  { pageSize: 'a4' },
)
```

The typeface is embedded and subset to the characters actually drawn. 515 KB of
Roboto becomes 14.5 KB for the two dozen glyphs a short document uses. Text
outside WinAnsi gets a Type0 font automatically, so Greek and Latin share one
document without you choosing.

## Three rules

Everything in the library follows from these, in this order.

**1. Non-locality is forbidden.** An edit on page 40 does not change the
rendered output of any other page by a pixel, and does not alter the bytes of
any object it did not need to touch. An unedited save returns the input byte for
byte. This is checked across 1,030 real PDFs on every build.

**2. Fidelity is reported, never assumed.** Operations return the rung they
reached rather than throwing on degradation:

| Rung | Meaning |
|---|---|
| `exact` | The glyphs were already in the embedded font. Nothing was approximated. |
| `reembedded` | A glyph was injected into the document's own font from a typeface you supplied. |
| `substituted` | A different face was used. The text is right, the letterforms are not. |
| `overlaid` | Old content covered, new content drawn on top. A last resort. |

Set a floor and anything below it is refused instead of quietly degraded:

```js
await doc.configureSession({ requireFidelity: 'exact' })
```

**3. The file stays a valid PDF.** Output passes `qpdf --check` and opens in
Acrobat, Preview, Chrome and Firefox without repair prompts.

## What it refuses

Each of these is a decision with a reason, not a gap in a roadmap.

- **Rendering.** pdf.js does it well and is Apache-2.0. Pair with it for display.
- **Scanned documents.** Editing them needs OCR and raster inpainting, which is
  a different product. `open()` succeeds and reports `documentKind: 'scanned'`.
- **XFA forms.** Deprecated by ISO and Adobe-proprietary. Detected, exposed as
  `hasXfa`, and form edits refused.
- **Creating digital signatures.** Needs key custody and carries regulatory
  weight. Existing signatures are detected, preserved, and their invalidation
  reported before you save.
- **Redacting text an image overlaps.** Image data is not searched, so a scan of
  the same words would survive. The operation fails rather than half-succeeding.

## What is in the box

| Crate | What it does |
|---|---|
| [`rasura`](https://crates.io/crates/rasura) | The facade. Start here. |
| [`rasura-cos`](https://crates.io/crates/rasura-cos) | Objects, xref, filters, encryption, the writer |
| [`rasura-content`](https://crates.io/crates/rasura-content) | Content streams, graphics and text state |
| [`rasura-font`](https://crates.io/crates/rasura-font) | Parsing, shaping, subsetting, injection, embedding |
| [`rasura-layout`](https://crates.io/crates/rasura-layout) | Glyph runs to lines to blocks to a document model |
| [`rasura-edit`](https://crates.io/crates/rasura-edit) | Edit operations, reflow, patching, sessions |
| [`rasura-flow`](https://crates.io/crates/rasura-flow) | The flow model, layout engine and composition |

Dependencies only go upward. Nothing in `rasura-cos` knows what a paragraph is,
and nothing in `rasura-layout` knows how to write a file. Each crate is usable
on its own.

## How it is checked

The library reconstructs documents nobody controls, so most of the verification
effort goes on evidence from outside the project.

- **1,196 Rust tests and 41 JavaScript tests.**
- **1,030 real PDFs**, mostly two decades of pdf.js regression cases, run
  through eight invariants on every build. Skips are itemised with reasons
  rather than counted as passes.
- **Three independent judges.** pdf.js extracts text and builds fonts, pdfium
  renders pages for pixel comparison, qpdf validates structure. None of them has
  a stake in agreeing with this library.
- **Fuzzing** on the lexer, document open, the filters and the xref parser.
- **A real browser** loads the site before anything deploys, because a check
  that never executes the artifact reports green for ever. That one exists
  because a demo shipped that had never started on any host.

[`docs/report.md`](docs/report.md) is the long version: what exists, what does
not, what was refused, and the mistakes that were found and fixed. It includes
the ones that were embarrassing.

## Limits worth knowing before you depend on it

- One embedded typeface per composed document. No bold or italic yet.
- Composition draws tables and lists as their text, without rules or bullets.
  The count is reported rather than the loss being silent.
- CFF outlines cannot be embedded. Declined by name rather than written into a
  key that claims TrueType and renders nothing.
- `/StemV` in a generated font descriptor is estimated. No TrueType table
  records it, and the result says so.
- No benchmarks yet. The largest gap in verification.

## Building

```bash
cargo test --workspace
./crates/rasura-wasm/build.sh          # the WASM module, measured against its budget
npm --prefix js test                   # the package
npm --prefix web run dev               # the docs site and editor
```

The corpus and the reference renderer are fetched rather than vendored, because
they are other people's files under other people's licences:

```bash
./corpus/fetch.sh              # pdf.js test suite, Apache-2.0
./corpus/fetch-font.sh         # Roboto, Apache-2.0
./harness/pixeldiff/fetch.sh   # pdfium, test-only and never shipped
```

## Licence

MIT or Apache-2.0, at your option.

The name is Latin for a scraping: the erasure of a parchment so the page can be
written over, with traces of the earlier text surviving underneath. That is the
file format's own model, where new revisions are appended and the old ones stay
where they were.
