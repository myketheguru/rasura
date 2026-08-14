# Rasura

[![crates.io](https://img.shields.io/crates/v/rasura.svg)](https://crates.io/crates/rasura)
[![npm](https://img.shields.io/npm/v/rasura.svg)](https://www.npmjs.com/package/rasura)
[![docs](https://img.shields.io/badge/docs-rasura-1f9a63)](https://myketheguru.github.io/rasura/)

**A browser-native SDK for true PDF editing.**

*Rasura* is Latin for a scraping — the erasure of a parchment so the page can be
written over, with traces of the earlier text surviving beneath. That is
precisely the file model: new revisions appended, prior revisions intact
underneath.

> **Status: all eight phases complete bar one blocked item.** A PDF becomes a
> document model of paragraphs, tables, images and reading order; fonts parse,
> shape, accept injected glyphs and **prune to what a document draws**; text can
> be **replaced, inserted and deleted**; images can be moved, scaled and
> deleted; pages can be **removed or reordered with their navigation fixed up**;
> form fields can be filled and flattened, annotations created with generated
> appearances, layers read without being flattened, documents **encrypted and
> their passwords changed**, and text **redacted with the removal verified
> against the saved bytes** — all through a transaction with exact undo. There
> is a **Rust facade, a WASM module and an npm package**: `npm i rasura`
> installs with no build step and no postinstall, and opens, reads, edits and
> saves a real PDF from a Worker at 419 KB gzipped, with multi-step sessions,
> undo, and fonts a caller supplies injected on demand. §13's performance gate
> is not built. See
> [Where this actually is](#where-this-actually-is) before planning around it.

---

## What "true editing" means

Every existing browser PDF library does one of two things: it renders (pdf.js),
or it draws new content on top of old (pdf-lib, jsPDF). None of them *edit*.
Rasura edits — it reconstructs the semantic document from the page
description, mutates it, and writes it back with the untouched 99% of the file
byte-identical.

```
bytes → objects → content streams → glyph runs → document model
                                                      ↓ mutate
bytes ← incremental append ← patched streams ← reflowed runs
```

Three properties define correctness, in priority order:

1. **Non-locality is forbidden.** An edit on page 40 must not change the
   rendered output of any other page by a single pixel, nor alter the bytes of
   any object it did not need to touch.
2. **Fidelity is reported, never assumed.** When the engine cannot make an exact
   edit, it says so in a typed result. It never silently substitutes a font,
   silently drops kerning, or silently overlays a text box.
3. **The file remains a valid PDF.**

## Where this actually is

| Layer | Crate | State |
|---|---|---|
| Objects, xref, filters, encryption, writer | `rasura-cos` | **Phase 1 complete**; protection creation added in Phase 8 |
| Content streams, graphics/text state, layers | `rasura-content` | **Phase 2 complete**; optional content added in Phase 8 |
| Reconstruction: runs → lines → paragraphs → document model | `rasura-layout` | **Phase 3 complete** |
| Font parsing, shaping, glyph injection | `rasura-font` | **Phase 4 feature-complete**; 2 of 4 viewers gating |
| Edit operations, reflow, stream patching | `rasura-edit` | **Phases 5-8 complete**; I5, I6, I7 green |
| Flow model, layout and document mode | `rasura-flow` | **the flow-model plan, complete**; export, frame inference, layout engine, PDF emission behind a flag, I8 holding throughout |
| Rust facade | `rasura` | **§11 surface built** |
| WASM surface | `rasura-wasm` | **builds, 419 KB gzipped, driven from node** |
| Worker protocol and npm package | `js/` | **`npm i rasura` works; the full edit catalogue, sessions, undo, `registerFont`** |

Phase 5 was the first shippable release. What remains before this is worth
publishing is §13's performance gate and §12.3's lazy chunk splitting.

The whole catalogue reaches JavaScript, not just text: images move, scale and
delete; pages delete and reorder; annotations are created with generated
appearances and removed again; form fields fill and flatten; table cells are
replaced; documents are redacted, verified, encrypted and re-keyed; embedded
fonts prune to what the document draws. Two operations deliberately stop at the
Rust facade — `insert_page`, which needs the draw emitter to be worth anything,
and arbitrary object writes, which §11.1 keeps off the JS surface on purpose.

```js
import { Pdf } from "rasura";

const doc = await Pdf.open(await file.arrayBuffer());
const page = await doc.page(0);

const session = doc.edit({ requireFidelity: "exact" });
await session.replaceText(page.paragraphs()[0].id, { start: 0, end: 5 }, "Q4 net revenue");
if ((await session.status()).canUndo) { /* await session.undo() */ }

const saved = await session.commit();   // nothing is written before this
download(saved.bytes);
await doc.close();
```

**[Try the demonstration editor →](https://myketheguru.github.io/rasura/)**
Static files, no server, no network: the document never leaves your machine.
Source in [demo/](demo/), deployed by
[.github/workflows/pages.yml](.github/workflows/pages.yml).
Its specification — architecture,
feature set, and a ten-minute script — is in
[docs/demo-editor.md](docs/demo-editor.md).

Everything is async because everything crosses a Worker — note the direction:
nothing inside the WASM module is asynchronous, and `{ worker: false }` resolves
on the same tick. See [js/README.md](js/README.md) for the four things worth
knowing before you start, of which the sharpest is that `open()` **detaches**
your buffer.

The delivery surface, built in the order §11.7 requires:

```bash
cargo install wasm-bindgen-cli --locked
npm i -g wasm-opt
./crates/rasura-wasm/build.sh
```

That compiles the module with §12.3's flags, measures it against the 900 KB
budget — **419 KB gzipped, 47% used** — and then drives the artefact from node:
opens a corpus PDF, reads its paragraphs, edits one, saves, reopens the saved
bytes and checks the text changed.

The node check is not a nicety. Everything on the WASM surface returns a
`JsValue`, and constructing one traps on a host target, so `cargo test` cannot
reach any of it: a binding that compiles and cannot open a file passes every
test in the crate. It also asserts its own preconditions, because the first run
picked a fixture with no text, skipped every edit check, and printed a clean
result.

```rust
use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("input.pdf")?)?;
let page = doc.page(0)?;
let id = page.paragraphs()[0].id;

let mut session = doc.edit();
session.require(rasura::edit::Fidelity::Exact);   // fail rather than degrade
let outcome = session.replace_text(&page, id, 0..5, "Q4 net revenue")?;
let saved = session.commit(&SaveOptions::default())?;
```

`require` is §11.4's knob and the reason the two callers who want opposite
things can share one code path: a form-filler would rather have the field filled
in a substituted typeface than not filled, and a contract-redlining tool would
rather fail loudly than hand a lawyer a clause set in a font the original never
used — because the second outcome is invisible in a diff and visible in court.

What Phase 5 adds:

- **Text operations** (§9.2): `replace_text`, `insert_text`, `delete_range`, on
  a character range of a paragraph. Each returns what it *would* do — patches
  plus fidelity — and applies nothing until the session says so.
- **Writing text means running §7.2 backwards.** The encoder inverts *this
  document's* decoder rather than reimplementing the Adobe Glyph List, which
  buys the property that matters: text written back extracts, through the chain
  a reader uses, to the text that was asked for. A character the font cannot
  draw is reported, never dropped.
- **Greedy line breaking** (§9.3), because an editor is judged on the diff it
  produces. Knuth–Plass would re-break lines the user did not touch; it is
  implemented in Phase 8 and opt-in, never the default.
- **Justification detected as a mechanism**, not an effect. Two paragraphs
  justified to the same measure by `Tw` and by `TJ` are identical in width and
  different in every glyph position between the first word and the last.

```rust
let page = EditablePage::analyse(&doc, &pages.pages[0]).unwrap();
let (id, _) = page.paragraphs[0];
let at = page.text_of(id).find("hello").unwrap();

let edit = replace_text(&doc, &page, id, at..at + 5, "WORLD", Policy::default())?;
assert!(edit.fidelity.is_exact());          // or a typed list of compromises

let mut session = EditSession::new(&mut doc);
session.patch_content("fix greeting", &page.content, &edit.patches, edit.fidelity)?;
let out = session.commit(&SaveOptions::default())?;
```

- An **`EditSession`** (§9.1): operations accumulate, each returns a fidelity
  report, nothing reaches the document until `commit()`, and a failure part-way
  leaves it exactly as it was.
- **Undo that restores bytes, not values.** The recorded inverse is the object's
  prior image rather than a replay instruction, because a replayed inverse is
  only exact if it reproduces every incidental byte — the `/Length`, the
  compression level, the operand spacing. Invariant **I5 now runs on the corpus:
  763 files, zero failures.**
- **Splicing that copies verbatim** (§9.4). Untouched bytes are copied from the
  original buffer, never regenerated from a parsed form. That is the only
  implementation of §2's first property that cannot drift as the layers above it
  grow.
- **Patches in logical coordinates**, resolved to the right object. A page whose
  `/Contents` is an array is one buffer to the layers above and several objects
  underneath; a span that crosses the join is refused rather than split, because
  splitting it writes half an operator into each stream.
- **Filter chains survive.** An edited flate stream comes back flate-compressed,
  at the level it was.
- **Generated numbers match the producer's** (§9.4). A file that wrote `72.0`
  gets `72.0` back, and one that wrote `.5` keeps writing `.5` — sampled by
  median so a handful of six-decimal `cm` entries do not set the precision for a
  whole page. This is a diff-legibility decision: every such number renders
  identically, and a commit that "corrects" them all buries the one change that
  mattered.

The invariant runner now reports **per-invariant coverage with skip reasons**,
so "not checked" cannot hide inside a total. I5's 249 skips are 140 recovery-mode
files, 105 pages with no content stream, and 2 with no page tree — none of them a
failure in disguise.

**I2's pixel half now runs**, which needed an edit to exist before it could be
written. A two-page document, a word replaced on page one, and page two renders
**pixel-identical** — the check that catches an edit leaking through something
the pages share, which no single-page fixture can express. On page one the
claim is narrower and still real: nothing upstream of the edit moved. The
boundary column is computed from the layout layer's own coordinates, and pdfium
put the first changed pixel on exactly that column.

What Phase 8 adds:

- **Creating protection**, and changing or removing a password (§5.5). AES-256
  `/R` 6 by default, AES-128 `/R` 4 for readers that predate it. RC4 and `/R` 5
  are read and **never written** — the read path already calls RC4 "a broken
  cipher we support only to read legacy files", so there is no variant for it
  rather than an option that could be forced.
- **The randomness comes from the caller.** This crate has no RNG and
  `wasm32-unknown-unknown` has none; the object layer needing no filesystem,
  clock or randomness is what lets it run unchanged in a Worker, and pulling in
  `getrandom` for a salt would spend that property on four bytes. `Entropy`
  takes 32 bytes and expands them, and says plainly that the result is only as
  good as they are.
- **An empty owner password no longer leaves the door open.** `/O` and `/U` are
  checked independently, so setting a user password and leaving the owner
  password blank produced a file that prompts for a password and opens without
  one — protected-looking to whoever made it. It now means "same as the user
  password", and the substitution is reported rather than assumed understood.
- **A protection change forces a full rewrite**, beside redaction's rule and for
  a related but distinct reason: appending would leave prior objects under the
  old key or none, and a reader has one file key for the whole document.
- **Streams are recrypted, not re-encoded.** Encryption sits outside the filter
  chain, so peeling one layer off and putting the new one back leaves the
  compressed bytes exactly as they were.
- **Knuth–Plass line breaking** (§9.3) — and greedy is **still the default**,
  which is the point of that decision rather than a contradiction of it. Greedy
  decides each line without looking ahead, so an edit late in a paragraph cannot
  move an earlier break; Knuth–Plass optimises the whole paragraph and one added
  character can shift every line. Optimal is right for a paragraph being set,
  greedy for one being corrected, and correcting is what this library does.
  No hyphenation: discretionary breaks are where most of the advantage lives and
  they need a dictionary per language, which is a bundle-size decision. The gain
  is checked against the textbook counterexample rather than asserted.
- **Optional content** (§10.2): layers, visibility, `/OCMD` policies and `/VE`
  expressions, and the regions on a page each governs — including the ones that
  come from an *XObject's own* `/OC`, where a walker looking for `BDC` sees a
  hidden watermark as ordinary drawn content. **Layers are never flattened**:
  the decision depends on a configuration the viewer owns and the user can
  change after saving.
- **A hidden layer's text is in the document**, and 96% of the corpus's
  optional-content regions are hidden. So redaction ignores visibility entirely
  — skipping it would be a cosmetic removal on a page that renders identically
  either way — while an *edit* to hidden content is reported, because the bytes
  change and the page does not.
- **Vertical writing** (§7.4). `/WMode 1` belongs to the font's CMap, not the
  text matrix, so a vertical run has an unrotated matrix and glyphs that share
  an x — and every character became its own line. Rotating the basis a quarter
  turn fixes both halves at once: `tangent` becomes distance down the column,
  and `normal` becomes `−x`, so the existing "sort by ascending normal" already
  orders columns right to left. No ordering code changed.
- **Compaction subsetting** (§8.6), and the spec's own caveat turns out to be
  avoidable. It warns that renumbering "would require rewriting every content
  stream that references the font" — but a composite font's streams hold
  **CIDs**, not glyph ids, so rewriting `/CIDToGIDMap` absorbs the renumbering
  and no content stream changes. Where the map is `/Identity` the indirection is
  *added* rather than the streams rewritten. Roboto in full — 515,100 bytes,
  3,387 glyphs — becomes 12,856 bytes and 13 glyphs, and pdfium renders the
  result pixel-identical.

What Phase 7 adds:

- **Redaction that is checked, not asserted** (§10.6). Every failure mode here
  is silent and total: a redaction that left the text behind renders identically
  to one that worked. So `redact::verify` reopens the *saved bytes* and looks
  for the words again — through the whole document, not the page that was
  edited, which is the difference that caught the first version on the corpus
  within a minute. **I7: 329 files, zero failures.** Two of the nine steps are
  not implemented — intersecting image data and font-subset glyph removal — and
  are **reported on every redaction** rather than left for a reader to discover.
- **The glyphs that stay do not move.** Writing the remainder of a line as one
  `Tj` lets its tail close up into the gap — the word is gone, the file is
  valid, verification is clean, and the black box the caller draws over the
  original rectangle now covers words nobody asked to hide while the ones that
  slid out from under it stay legible. Each removed stretch becomes a `TJ`
  adjustment of exactly the advance it contributed, so every surviving glyph
  keeps its position and the pen ends where it would have.
- **Redaction forces a full rewrite in `rasura-cos`, not in a comment.**
  A redacted document saved incrementally still contains the text in the prior
  revision. The writer checks the redaction flag *before* the caller's requested
  mode, so a caller asking for `Incremental` gets `FullRewrite` anyway.
- **Tagged PDF maintenance** (§10.1), validated against what the pages actually
  draw rather than against a model built by the same walk. `Degraded` — a
  structure tree that no longer describes its content — is reported separately
  from `Tagged`, because assistive technology reads such a file *worse* than an
  untagged one. **I6: 50 files, zero failures**; the other 962 are skipped with
  reasons, because 919 of them are untagged and a pass would be a lie.
- **AcroForm fields** (§10.4) with fully-qualified names per §12.7.3.2, so
  `billing.city` and `address.city` are not confused for one another.
  `set_text_value` regenerates `/AP` **and** sets `/NeedAppearances`, because
  neither alone is sufficient: viewers that ignore the flag would otherwise show
  the old value. `/DA` is spliced through verbatim rather than re-emitted.
- **Flattening that draws what the viewer drew** (§10.8). The obvious
  implementation re-renders `/V` through `/DA`, which reproduces the data and
  not the appearance — and the appearance is what the person filling the form
  approved. Instead the existing `/AP` `/N` is invoked as a form XObject and
  mapped `/BBox`→`/Rect` per §12.5.5, honouring `/AS` so a check box draws the
  state it is actually in.
- **Annotations** (§10.7): all seventeen subtypes read and deleted; created only
  where the appearance is *determined* by geometry — `/Square`, `/Circle`,
  `/Line`, `/Ink` and the four quad-based markup types. A `/Stamp` or a note
  icon is a design decision no specification makes, so those decline by name
  rather than inventing a look no other reader would draw.
- **`set_cell` on a detected table** — and the five structural operations
  declining individually, each naming what would make it possible. They move
  content on a grid that was *inferred*; a misdetected column edge becomes a
  visibly broken table with no single place to look.

What Phase 6 adds:

- **Image operations**: move, scale and delete, all by wrapping the drawing
  operator in `q … cm … Q` rather than rewriting the transform that positioned
  it. A CTM is accumulated from the page matrix, enclosing `q` blocks and form
  `/Matrix` entries — there is no single "the `cm`" to edit, and the last one is
  not privileged. Wrapping is local by construction, and carries the original
  bytes through untouched, which matters because 71% of the corpus's images are
  inline and their pixel payload lives inside the operator.
- **`delete_page` and `move_page`, with the §10.9 fix-up.** Deleting removes the
  one `/Kids` entry, decrements `/Count` up the whole ancestry, retargets every
  destination that named the page — and **refuses outright if any of them could
  not be retargeted**, because a half-fixed document is the silent corruption
  the spec warns about.
- **A draw-command emitter** (`Canvas`): the one piece that produces content
  which never existed. Deliberately small — the operators *are* the API, and
  every abstraction over them is a place where the bytes stop resembling what a
  producer would have written. It enforces one thing: `q`/`Q` must balance, and
  it refuses to auto-close, because a caller who forgot a `Q` has usually
  forgotten *where*.
- **`insert_page`**, and `replace_image` with stretch or letterbox. Replacing
  needs no pixel work: the caller supplies encoded bytes and the filter they are
  in, which is what separates it from `resample_image` — the one operation in
  §10.4 that genuinely needs a codec.
- **A destination check that already found real defects.** 199 corpus documents
  have destinations that all resolve; **8 already dangle** — `section*.2`,
  `subsection.10.2.1`, `cite_note-…`, LaTeX and MediaWiki names left behind when
  someone extracted pages without fixing up. Exactly what §10.9 describes.

The corpus decided the design twice. `/A` `/D` actions outnumber bare `/Dest`
**3.6 : 1** on links and **4.5 : 1** on outlines, so an implementation following
§10.9's sentence literally would find a quarter of what is there. And
`/Threads`, which the spec lists, has **zero real article threads in 960
documents** — it is reported and deliberately not traversed.

What Phase 3 adds:

- Derives Unicode through the full seven-step chain of §7.2. The **Adobe Glyph
  List** carries 47% of all glyphs — more than `/ToUnicode` — which contradicted
  the spec's own expectation and is written up in
  [docs/q1-tounicode-coverage.md](docs/q1-tounicode-coverage.md).
- Assembles glyphs into lines, words, regions, paragraphs and style runs, with
  alignment, leading, indents and hyphenation reported rather than assumed.
- Detects tables from drawn grids or aligned columns, running headers and
  footers across pages, and footnotes linked to their in-text markers.
- Reads `/StructTreeRoot`, so a tagged document's reading order comes from the
  producer instead of from geometry — and provides the **only oracle** the
  ordering heuristics have.
- Classifies everything into a `Block` enum with an explicit `Unknown` variant.
  Content that cannot be confidently classified is preserved opaque and never
  reflowed, because guessing is worse than declining.

**3,095 images** (2,188 inline, 2,214 stencil masks, **1,129 rotated or
skewed**) and **635 vector blocks** across the corpus — counted and asserted,
where before they were modelled and nothing checked they arrived.

Four partitions are asserted on every page of the corpus and gated in CI: region
detection loses no glyph, paragraph splitting loses no line, cell assignment
loses no glyph, and the model lists every block exactly once. Reading order
scores **89.8% concordant** against the 87 tagged documents in the corpus.

What Phase 2 adds:

- Tokenises the complete operator set, with **every operator carrying its byte
  span** in the decoded stream — the property that makes surgical editing
  possible in Phase 5.
- Tracks the full graphics and text state, including the two rules that are
  routinely got wrong: word spacing applies only to a single-byte code 32, and
  the text matrices are *not* part of the graphics state.
- Concatenates `/Contents` arrays while keeping the map back to which object
  each byte came from, so a patch lands in the right stream.
- Recurses into form XObjects with `/Matrix` composed and resources scoped,
  guarded against cycles.
- Extracts positioned glyphs: code, CID, advance, device-space origin, and the
  byte span within the showing operator.

Validated by walking every page of the corpus — 1.99M operators, 1,484 pages,
zero span or attribution defects — and by differential comparison against pdf.js.

What Phase 1 does today:

- Parses every object form in ISO 32000-1 §7.3, retaining the original encoded
  bytes of names and strings.
- Resolves all four cross-reference forms — classic tables with `/Prev` chains,
  cross-reference streams, object streams, and hybrid `/XRefStm` files — and
  keeps the revision chain rather than flattening it.
- Rebuilds the table by scanning when `startxref` is wrong, the table is
  malformed, or `/Root` will not resolve.
- Decodes Flate, LZW, ASCIIHex, ASCII85 and RunLength with PNG and TIFF
  predictors; passes image codecs through untouched.
- Decrypts the standard security handler: `/V` 1, 2, 4, 5 and `/R` 2–6, RC4
  40/128-bit, AES-128, AES-256 with the `/R` 6 hardening loop.
- Writes an incremental revision that leaves every original byte in place, or a
  full rewrite that compacts.
- Records every spec deviation it tolerated, so "why did this file behave oddly"
  has an answer.

## Try it

```bash
cargo test --workspace                      # 1049 tests
cargo run -p rasura-invariants          # seed corpus

./corpus/fetch.sh                           # ~119 MB, Apache-2.0
pwsh corpus/latex/build.ps1                 # 13 LaTeX samples, needs TeX
cargo run --release -p rasura-invariants
cargo run --release -p rasura-fontsurvey
```

Glyph injection, end to end, against a typeface this library did not write:

```bash
./corpus/fetch-font.sh                      # Roboto, Apache-2.0
./harness/pixeldiff/fetch.sh                # pdfium, test-only
cargo run -p rasura-font --example realfont -- \
    corpus/fonts/Roboto-Regular.ttf target/realfont
cargo run --release -p rasura-pixeldiff -- \
    target/realfont/before.pdf target/realfont/after.pdf
```

It subsets Roboto the way a producer would, then injects a character the subset
threw away — `É`, which is a *composite*, so it is only correct if the
components come with it and their glyph ids are renumbered inside the
composite's own body. pdfium renders before and after; pdf.js reads the text
back. Neither has any stake in agreeing with us.

A text edit, judged the same way:

```bash
cargo run -p rasura-edit --example textedit -- target/textedit
cargo run --release -p rasura-pixeldiff -- \
    target/textedit/before.pdf target/textedit/after.pdf --page 2 --identical
```

Pruning a real embedded font, judged by a renderer:

```bash
./corpus/fetch-font.sh                      # Roboto, Apache-2.0
cargo run -p rasura-edit --example compactfont -- target/compactfont
cargo run --release -p rasura-pixeldiff -- \
    target/compactfont/before.pdf target/compactfont/after.pdf --identical
```

Compaction is where a mistake is least visible from inside: renumber a font's
glyphs and lose track of which is which, and the document opens, validates,
extracts the right text through `/ToUnicode`, and **draws the wrong letters**.
So the whole of Roboto goes in, 3,374 of its 3,387 glyphs come out, and the
claim is that not one pixel changed.

Protecting a document, judged the only way encryption can be:

```bash
cargo run -p rasura-cos --example protect -- target/protect
node harness/textdiff/validate-injected.mjs target/protect/aes256.pdf \
    --password hunter2 "Account balance: 4,200"
cargo run --release -p rasura-pixeldiff -- \
    target/protect/plain.pdf target/protect/aes256.pdf \
    --password "" --password hunter2 --identical
```

Testing encryption against your own inverse proves nothing: a key derivation
wrong in a self-consistent way encrypts and decrypts perfectly and produces a
file nobody else can open. So the check is entirely external. pdf.js derives the
key from the password and reads the text — and fails without the password, and
with a wrong one. pdfium renders the protected file **pixel-identical** to the
plain original, which is the sharp claim: encryption must change no pixels.

A redaction, judged by someone who does not believe it:

```bash
cargo run -p rasura-edit --example redact -- target/redact
cargo run --release -p rasura-pixeldiff -- \
    target/redact/before.pdf target/redact/after.pdf --changed-within 420 580
```

One word is planted in five places a page does not show — the showing operator,
`/Info` `/Subject`, the XMP packet, an outline title, and an **indirect**
`/ActualText` — each of which is somewhere the corpus found a word surviving a
redaction that had reported itself clean. The example refuses to trust its own
verifier: it first checks the verifier *fails* on the input, then asks for an
incremental save and asserts it was overridden, then scans the output bytes for
the word the way `strings` would. The pixel diff confirms the last claim, that
only the word's own pixels changed — which holds because the glyphs that stay
are pinned in place with `TJ` adjustments rather than being allowed to close up
into the gap the black box is going to cover.

The second run adds mozilla/pdf.js's 974 committed test files — two decades of
cases kept precisely because they broke something. **1,026 of 1,026 green.**
Fourteen files are not opened and say why: eleven need a password that was not
supplied, and three are declined with typed errors — two have no `/Type
/Catalog` anywhere and one has an `/Encrypt` that does not resolve to a
dictionary. Fetching that corpus found five
real defects in a day-old parser; they are written up in
[docs/phase-1-notes.md](docs/phase-1-notes.md).

```rust
use rasura_cos::{Document, ObjId, Object, Name, SaveOptions, save};

let doc = Document::open(std::fs::read("input.pdf")?)?;

println!("{} objects, PDF {}", doc.xref().len(), doc.version());
for l in doc.leniencies() {
    println!("tolerated: {l}");   // empty for a well-formed file
}

// Saving an unedited document returns the input, byte for byte.
assert_eq!(save(&doc, &SaveOptions::default())?.bytes, doc.bytes());
```

Editing an object appends a revision and leaves everything before it alone:

```rust
let mut doc = Document::open(bytes)?;
let page = doc.get(ObjId::new(3, 0))?;
let mut dict = page.as_dict().unwrap().clone();
dict.insert(Name::new("Rotate"), Object::Integer(90));
doc.set(ObjId::new(3, 0), Object::Dictionary(dict));

let out = save(&doc, &SaveOptions::default())?;
assert!(out.bytes.starts_with(doc.bytes()));   // nothing original was rewritten
```

## Permissions are advisory

The `/P` permission bits are reported through `document.permissions()` and are
**not enforced**. Whether to honour a bit that says "printing not allowed" is the
consuming application's legal and product decision, not the parser's — and
enforcing it in a library whose source you can read would be theatre rather than
security.

## Design constraints

**Everything is Rust.** No qpdf, MuPDF, Poppler or PDFium is vendored into the
build. Mixing Emscripten output with `wasm-bindgen` is a build-system tarpit and
doubles the bundle. PDFium may be used as a *test-only* reference renderer for
the pixel-diff harness; it is never shipped.

**Permissive licences only.** Every runtime dependency must be MIT, Apache-2.0,
BSD, ISC or Zlib. No GPL, no LGPL, no AGPL. `cargo-deny` runs in CI with an
allowlist and a violation fails the build.

**No unsafe.** `rasura-cos` is `#![forbid(unsafe_code)]`.

## Repository layout

```
crates/rasura-cos/   the object layer (Phase 1)
crates/rasura-edit/  mutation and commit (Phase 5)
harness/invariants/      the invariant suite, run in CI
harness/pixeldiff/       pdfium-based, test-only (spec 14.3)
corpus/                  test corpus and its manifest
fuzz/                    cargo-fuzz targets
docs/                    spec coverage and design notes
spec.md                  the full engineering specification
```

## Open question Q1 is answered

The spec (§18) says to measure `/ToUnicode` coverage before anything else,
because the answer decides how much of the Unicode-derivation chain has to exist
in Phase 3. It does: **53.0% across 1390 embedded fonts**, well below the 85%
threshold.

But the spec's diagnosis was wrong. It expected LaTeX subset fonts with `g34`
glyph names; modern pdfTeX emits `/ToUnicode` for everything, and only **six
fonts in 1390** carry opaque names at all. The component that actually carries
the load is the **Adobe Glyph List** — 300 of the 653 failures resolve through it
and nothing else — which §7.2 mentions only in passing. A shape-matching fallback
is not justified by any evidence in the corpus.

Full write-up: [docs/q1-tounicode-coverage.md](docs/q1-tounicode-coverage.md).

**And the prediction held.** With the derivation chain built, the AGL resolves
**47% of all glyphs** across the corpus — more than `/ToUnicode` does — while the
glyph-name heuristics account for **0.03%**. Unmapped glyphs fell from 75% to
10.1%, and pages where extraction agrees with pdf.js went from 279 to 562.

**Q6 is also answered**: the object layer is **123 KB gzipped** as WASM, 13.6% of
the 900 KB `core` budget, so the module split in §12.3 stands and layout does not
need to become a third lazy chunk. That has since been confirmed on the complete
chunk rather than the floor — cos, content, layout, font, `rustybuzz`,
`ttf-parser` and every generated table together come to **413 KB gzipped, 45.9%
of budget**. The probe runs in node against the fixture
corpus — 16 of 16, encryption included — which is the first evidence that nothing
in the object layer needs a filesystem, a clock, or randomness.
[docs/q6-bundle-floor.md](docs/q6-bundle-floor.md).

## Documentation

- [Build report and spec parity](docs/report.md) — what exists, what does not,
  what was refused on purpose, and what was got wrong and then fixed. Start
  here if you are deciding whether to depend on this.
- [Q1: `/ToUnicode` coverage](docs/q1-tounicode-coverage.md) — the measurement,
  and what it changes about Phase 3.
- [Q6: the bundle floor](docs/q6-bundle-floor.md) — WASM size, and the CI gate.
- [Spec coverage matrix](docs/spec-coverage.md) — what is supported, partial, or
  refused. Publishing the gaps builds more trust than hiding them.
- [Phase 1 notes](docs/phase-1-notes.md) — decisions taken, deviations from the
  spec, and what the next phase has to pick up.
- [Phase 2 notes](docs/phase-2-notes.md) — why the obvious exit gate was the
  wrong one, and three rounds of the differential harness being wrong.
- [The flow model](docs/flow-model.md) — why a PDF has no flow to preserve, and
  what building one would cost. Surgical and document modes, the risks of
  regenerating from a heuristic reconstruction, and the order of work if it is
  pursued. **Design, not built.**
- [spec.md](spec.md) — the full engineering specification.

## Licence

MIT OR Apache-2.0.
