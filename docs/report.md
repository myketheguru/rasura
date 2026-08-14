# Rasura: build report and spec parity

**As of this writing.** 1,192 Rust tests and 41 JavaScript tests passing. 1,026
corpus files green on the invariant suite, zero failing. `cargo deny check` green
on all four checks. 448.8 KB gzipped against a 900 KB budget. Nine
CI jobs green, and the documentation site and editor deployed at
<https://myketheguru.github.io/rasura/> — checked in a real browser, against the
deployed origin, on every push.

This document is the honest account: what exists, what does not, what was
refused on purpose, and what was got wrong and then fixed. The companion
[spec-coverage.md](spec-coverage.md) is organised by specification section and
carries the design rationale; this one is organised for someone deciding whether
to depend on the library.

---

## 1. Where it stands

| Layer | Crate | State |
|---|---|---|
| Objects, xref, filters, encryption, writer | `rasura-cos` | complete |
| Content streams, graphics/text state, layers | `rasura-content` | complete |
| Reconstruction: runs → lines → paragraphs → model | `rasura-layout` | complete |
| Font parsing, shaping, injection, subsetting | `rasura-font` | feature-complete; 2 of 4 viewers gating |
| Edit operations, reflow, stream patching | `rasura-edit` | complete |
| Flow model, layout, document mode and composition | `rasura-flow` | `docs/flow-model.md` complete, plus `compose` |
| Rust facade | `rasura` | §11 surface built, `create` included |
| WASM surface | `rasura-wasm` | builds, driven from node |
| Worker protocol and npm package | `js/` | `npm i rasura` works |
| Documentation site and editor | `web/` | React, Vite, deployed to Pages |

All eight delivery phases are complete except one item that cannot be started
(§12.1's threaded build; see §7 below). 62,834 lines of Rust across 115 files;
2,612 lines of JavaScript and TypeScript across 12.

**Composition landed after the phases were written.** The specification has
carried `static create(opts?: CreateOptions)` since version 1.0 and nothing
implemented it, because until recently nothing could: `rasura_cos::Document` had
no constructor, an empty page tree could not be given a first page, and no code
in the workspace could write a `/FontFile2` for a typeface a document had never
seen. All three are now closed, and the library makes documents as well as
editing them.

### What the library can do

Open a PDF — including encrypted, damaged, or one whose cross-reference table
has to be rebuilt. Read it as pages, paragraphs, tables, images and reading
order. Replace, insert and delete text with the reflow policy you choose. Move,
scale, replace and delete images. Insert, delete and reorder pages with their
navigation fixed up. Fill form fields and flatten them. Create, edit and delete
annotations. Redact text and verify the removal against the saved bytes.
Encrypt a document or change its password. Prune embedded fonts to what the
document draws. Supply a font the document lacks and have the glyph injected
into its own embedded font. Undo any of it exactly. Save incrementally, leaving
the untouched bytes byte-identical.

And **make one that did not exist**: describe the content as blocks, name a
typeface, and the layout engine decides the measure, the leading, the pagination
and the position of every line. The typeface is embedded and subset to the
characters actually used — 515 KB of Roboto becomes a 14.5 KB subset for two
dozen glyphs — as a simple `/TrueType` font when every character has a WinAnsi
code and a `/Type0` with `/Identity-H` when one does not, chosen from the text
rather than asked of the caller. A composed document is an ordinary one: it
reads back through the same reader, edits through the same session, and saves
through the same writer.

### Which of it the API actually exposes

Nearly all of it, on all three layers. This was the sharpest gap in the project
until the catalogue was wired through; what remains is listed as `—` below and
each entry has a reason, not a backlog position.

| Operation | `rasura-edit` | Facade | JS |
|---|---|---|---|
| Text: replace, insert, delete | yes | yes | yes |
| Images: move, scale, delete | yes | yes | yes |
| Images: **add** | yes | — | — |
| Images: replace pixels | yes | — | — |
| Pages: delete, reorder | yes | yes | yes |
| Pages: insert | yes | — | — |
| **Composition: document from nothing** | `rasura-flow` | yes | yes |
| **Font embedding, subset, simple or Type0** | `rasura-font` | via `create` | via `create` |
| Annotations: create, list, delete | yes | yes | yes |
| Form fields: read, fill, flatten | yes | yes | yes |
| Table cells | yes | yes | yes |
| Redaction, and its verification | yes | yes | yes |
| Encryption, password change | `rasura-cos` | yes | yes |
| Font compaction | yes | yes | yes |
| Arbitrary object writes | `set_objects` | via `raw_mut()` | — |

Everything in a session shares one undo stack, on every layer: an image move
and a text edit staged together come off in reverse order and leave the file
byte-identical. `js/test/catalogue.test.mjs` checks exactly that, through a real
Worker, against bytes read back from `commit()`.

Four rows are `—`, and the reasons now differ from each other:

- **Image pixel replacement** needs a codec to re-encode with, which §4.2's
  no-vendored-C++ rule makes a real decision rather than a small one.
- **Adding an image** is new and not yet lifted to the facade. It allocates the
  XObject, registers it under a name nothing else uses, and **appends** a
  content stream rather than editing the existing one — so every object already
  in the file keeps its bytes, which is §2's first property holding for an
  operation that could easily have broken it. Inherited `/Resources` are copied
  rather than shadowed; writing a fresh dictionary holding only the image would
  have taken every font on the page with it.
- **`insert_page`** is no longer blocked on the draw emitter — composition draws
  pages now — but is still not lifted, because a caller wanting a page with
  content on it should reach for `create` rather than assemble one by hand.
- **Arbitrary object writes** stay off the JS surface deliberately. §11.1's
  second principle is that no PDF concepts leak by default; `document.raw` is
  the escape hatch in Rust, and a JS caller who needs one is better served by
  the Rust facade than by a `setObject(number, generation, dict)` that would
  make every §11.5 error code a guess.

Encryption on the JS surface needed one decision that is worth naming: the
WASM module has no random number generator and does not get one. `protect()`
takes 32 bytes of entropy, and the JS wrapper fills them from
`crypto.getRandomValues` — better than anything the module could bundle, and it
keeps the provenance of the key material visible at the call site rather than
dependent on which target the crate happened to be compiled for.

### What you cannot do at all

No rendering (a draw-command emitter exists; it is not a renderer). No OCR. No
XFA. No digital signature creation. No image resampling. No structural table
editing. No CFF font embedding, and no composed document in more than one face.
Details and reasons in §5 and §6.

---

## 2. The three correctness properties

Spec §2 defines correctness in priority order. Each is enforced by a mechanism
rather than by intention.

### Non-locality is forbidden

> An edit on page 40 must not change the rendered output of any other page by a
> single pixel, nor alter the bytes of any object it did not need to touch.

The object half is invariant **I2**: 872 corpus files, zero failures. The pixel
half runs pdfium in CI on a two-page fixture — a word replaced on page one,
page two rendered **pixel-identical**. That check needed an edit to exist before
it could be written, and it catches the failure no single-page fixture can
express: an edit leaking through something the pages share.

The mechanism is that untouched bytes are *copied*, never regenerated.
`patch::splice` copies unclaimed bytes verbatim from the original buffer. That
is the only implementation of this property that cannot drift as the layers
above it grow.

### Fidelity is reported, never assumed

Every operation returns a typed report. Twelve `Compromise` variants exist, each
naming one specific thing that was lost or changed — regenerated kerning,
re-broken lines, an overflowed block, glyphs injected, a subset retained after
redaction, an edit landing in a hidden layer. `requireFidelity` turns the report
into a gate: an operation that cannot reach the floor fails rather than
degrading.

### The file remains a valid PDF

Invariant **I3**: 1,012 files, zero failures, checking the *output* of a save
rather than the input — checking the input would conflate a defect we
introduced with one the file arrived carrying. `qpdf --check` runs on every
generated file in CI.

---

## 3. Spec parity, section by section

Legend: ● complete · ◐ partial, with the gap named · ○ not built · ✕ refused on
purpose.

### §4 Architecture

| Item | | Notes |
|---|---|---|
| 4.1 Crate graph, strict layering | ● | cos → content → font → layout → edit → facade → wasm |
| 4.2 The no-C++ rule | ● | pdfium is test-only and banned from the shipped graph by `cargo-deny` `wrappers` |
| 4.3 Permissive licences only | ● | hard CI gate; MIT/Apache-2.0/BSD/ISC/Zlib/Unicode-3.0 |

### §5 Object layer

| Item | | Notes |
|---|---|---|
| 5.1 Object model | ● | original encoded bytes retained for names and strings |
| 5.2 Lexer and parser | ● | panic-free by construction; fuzzed |
| 5.3 Cross-reference resolution | ● | classic, streams, object streams, hybrid `/XRefStm`, `/Prev` chains, reconstruction |
| 5.4 Stream filters | ● | Flate, LZW, ASCIIHex, ASCII85, RunLength, PNG and TIFF predictors |
| 5.5 Encryption — reading | ● | `/V` 1,2,4,5 · `/R` 2–6 · RC4 40/128 · AES-128 · AES-256 with the `/R` 6 loop |
| 5.5 Encryption — creating | ● | AES-256 `/R` 6 default, AES-128 `/R` 4 for old readers |
| 5.5 Password change, removal | ● | forces a full rewrite, enforced in the writer |
| 5.5 Writing RC4 or `/R` 5 | ✕ | read for legacy files; never written |
| 5.6 Writer, both modes | ● | incremental leaves every original byte; full rewrite compacts |

### §6 Content layer

| Item | | Notes |
|---|---|---|
| 6.1 Complete operator set | ● | 1.99M operators walked across the corpus |
| 6.2 Span preservation | ● | every operator carries its byte range; the property surgical editing rests on |
| 6.3 Graphics and text state | ● | including the two routinely got wrong: `Tw` on single-byte code 32 only, and the text matrices not being graphics state |
| 6.4 Resource resolution, recursion | ● | form XObjects with `/Matrix` composed, cycle-guarded |

### §7 Reconstruction

| Item | | Notes |
|---|---|---|
| 7.1 Glyph run extraction | ● | |
| 7.2 Unicode derivation, seven strategies | ● | the AGL carries 47% of all glyphs — more than `/ToUnicode` |
| 7.3 Word segmentation | ● | |
| 7.4 Line assembly | ● | including vertical writing (Phase 8) |
| 7.5 Block and column detection | ● | recursive XY-cut with a fallback |
| 7.6 Paragraph and style reconstruction | ● | alignment, leading, indents, hyphenation reported not assumed |
| 7.7 Tables, headers, footers | ● | detection; restructuring declines — see §5 below |
| 7.8 Document model | ● | reading order 89.8% concordant against tagged documents, **n = 50** |
| 7.8 Vector provenance, clipping, shading | ● | paths, colours, patterns, operator spans and the clip; `sh` modelled; clip approximation reported |
| 7.8 Frame inference | ● | `rasura_layout::frames`; 99.6% containment, median tightness 1.01 over 628 corpus files |
| 7.8 Flow model, layout engine, emission | ● | `rasura_flow::layout` and `::emit`; I8 holds through a written, re-opened file |

### §8 Font layer

| Item | | Notes |
|---|---|---|
| 8.1–8.2 Parsing all five containers | ● | Type1, TrueType, CFF, CID CFF, OpenType |
| 8.3 Shaping | ● | rustybuzz; script and direction derived from run content |
| 8.4 Glyph injection | ● | validated against Roboto by pdfium and pdf.js |
| 8.4 **Embedding a font the document never had** | ● | `rasura_font::create`; `/FontFile2`, descriptor and `/Widths` synthesised from the program. Simple or `/Type0`, chosen from the text |
| 8.4 **`/FontDescriptor` from the font program** | ● | `rasura_font::describe`; `head`, `post`, `hhea`, `OS/2`, `name`. Nothing here was read anywhere in the workspace before |
| 8.4 CFF embedding | ○ | a different stream and subsetter; declined by name rather than mislabelled |
| 8.5 Font matching for substitution | ◐ | the matcher exists; not wired to editing |
| 8.6 Sparse-preserving subsetting | ● | the default, and what injection already does |
| 8.6 Compaction subsetting | ● | Roboto 515 KB → 12.8 KB, pixel-identical |
| 8.7 Type 3 fonts | ● | read; editing declines with the missing codes named |

### §9 Edit layer

| Item | | Notes |
|---|---|---|
| 9.1 Transaction model, undo/redo | ● | byte-image inverses; invariant I5 |
| 9.1 Sessions across a boundary | ● | `SessionState` suspend/resume |
| 9.2 `replace_text`, `insert_text`, `delete_range` | ● | |
| 9.2 Image move, scale, replace, delete | ● | by wrapping in `q … cm … Q`, never rewriting the transform |
| 9.2 Page insert, delete, reorder | ● | with §10.9 navigation fix-up |
| 9.2 `set_cell` | ● | |
| 9.2 Structural table operations | ✕ | needs a producer-declared structure; declines by name |
| 9.2 `move_vector` | ◐ | wraps the whole path; declines when its operators are interleaved. Not on the facade or JS |
| 9.2 **Composition from nothing** | ● | `rasura_flow::compose`, `Document::create`, `Pdf.create`. Page geometry, columns, pagination, keep-with-next |
| 9.2 Composition: tables, lists, figures | ◐ | placed as their text; counted in `approximated` rather than dropped |
| 9.2 Composition: more than one face | ○ | one embedded font per document; bold and italic need a second and third |
| 9.2 `set_style`, `insert_paragraph`, `set_z_order` | ○ | |
| 9.3 Greedy line breaking | ● | the default, and a fidelity decision |
| 9.3 Knuth–Plass | ● | opt-in; no hyphenation |
| 9.3 Justification mechanism detection | ● | reproduces the mechanism, not just the width |
| 9.3 Overflow `Refuse`/`Allow` | ● | |
| 9.3 Overflow `Grow`/`Shrink` | ◐ | typed; the shape is reported, the caller applies it |
| 9.4 Stream patching, all five steps | ● | including producer-matching number formatting |
| 9.5 Commit and save | ● | |
| 9.6 Signatures | ● | detected, impact reported, creation refused |

### §10 Beyond text

| Item | | Notes |
|---|---|---|
| 10.1 Tagged PDF maintenance | ● | invariant I6; `Degraded` reported separately from `Tagged` |
| 10.2 Optional content | ● | layers, `/OCMD` policies, `/VE` expressions; never flattened |
| 10.3 Metadata, both surfaces | ● | disagreements exposed rather than resolved |
| 10.4 Images | ● | except `resample_image` |
| 10.4 **Adding an image** | ● | `rasura_edit::images`; appends a content stream, so nothing already in the file moves. Not on the facade |
| 10.4 `resample_image` | ○ | the only piece needing a pixel codec |
| 10.5 Vector content | ◐ | detected and preserved; no provenance to move a path by |
| 10.6 Redaction, 7 of 9 steps | ● | invariant I7; verification is a public API |
| 10.6 Steps 2 and 6 | ○ | image data and font-subset glyphs; **reported on every redaction** |
| 10.7 Annotations | ● | 17 subtypes read/deleted; created where geometry determines appearance |
| 10.7 Annotations read into the model | ● | `rasura_layout::annots`, with `/V` inherited up the field tree |
| 10.8 Flattening | ● | invokes the existing `/AP`, never re-renders `/V` |
| 10.9 Navigation structures | ● | `/A` `/D` actions included, which outnumber `/Dest` 3.6 : 1 |

### §11 Public API

| Item | | Notes |
|---|---|---|
| 11.1 Design principles | ● | no PDF vocabulary by default; `raw()` is a deliberate cliff |
| 11.2 Core surface | ● | `Document`, `Page`, `Paragraph`, `Block` |
| 11.2 `documentKind` | ● | written for this; nothing below classified scans |
| 11.3 `fontRequirements` | ● | measured against the embedded program, not the encoding |
| 11.3 `registerFont` | ● | injection on demand; `reembedded` rung |
| 11.4 Fidelity contract, `requireFidelity` | ● | two of four rungs reachable |
| 11.4 Editing surface beyond text | ◐ | images, pages, annotations, forms, tables, redaction and encryption all reach JS; `insert_page`, `add_image` and image pixel replacement do not |
| 11 **`create`** | ● | specified since version 1.0, unimplemented until now. `Document::create`, `createDocument`, `Pdf.create` |
| 11.5 Coded errors, never bare | ● | 14 codes; survives the Worker boundary. The fourteenth, `invalid-argument`, is the only one not in the original list: every other code describes a condition of a *document*, and composition introduced the first operations with no document to describe |
| 11.6 pdf.js pairing | ● | documented and used by the test harness |
| 11.6 Draw-command emitter | ◐ | `Canvas` exists in Rust; no JS surface yet |
| 11.7 Rust facade, synchronous | ● | designed first, per the spec's instruction |

### §12 WASM and packaging

| Item | | Notes |
|---|---|---|
| 12.1 Single-threaded default | ● | honestly reported by `isThreaded()` |
| 12.1 `rasura/threaded` | ○ | see §7 below |
| 12.2 Worker by default | ● | request/response with ids |
| 12.2 Transfer rather than copy | ● | both directions; detachment documented |
| 12.2 `{ worker: false }` | ● | same code, same answers |
| 12.3 Build flags, size gate in CI | ● | 448.8 KB gzipped, 49.9% of budget |
| 12.3 Lazy chunk splitting | ○ | one chunk today; `fonts` does not load lazily |
| 12.4 ESM primary, CJS shim | ● | |
| 12.4 Hand-written `.d.ts`, no `any` | ● | `tsc --noEmit` under `strict` gates it |
| 12.4 `.wasm` asset, `wasmUrl` override | ● | |
| 12.4 No postinstall, no build step | ● | proven by installing with `--ignore-scripts` |
| 12.5 Memory, `memoryUsage()` | ◐ | reported; no LRU cap on the page-model cache |

### §13–§16

| Item | | Notes |
|---|---|---|
| 13 Performance budgets | ○ | no benchmarks, no regression gate |
| 14 Corpus, invariants, fuzzing, cross-viewer | ● | see §4 below |
| 14 Browser verification | ● | headless Chrome over the DevTools protocol, both routes, before every deploy |
| 15 Repository layout | ● | |
| 16 Documentation deliverables | ◐ | this file, `spec-coverage.md`, `flow-model.md`, two Q write-ups, two READMEs; no tutorial or API reference site |

---

## 4. How it is verified

Nothing here is checked only against itself. The escalation, in order of how
much it proves:

**Unit tests — 1,192**, plus 41 JavaScript tests. Plus two TypeScript gates, both green: the
package's declarations, and the site, held to the same `strict` and no-`any`
setting because a rule about what the project ships is worth nothing if the site
documenting it is loose.

**The JavaScript suite hung, and the library was implicated after all.** It
passes now, 41 tests. Two faults, and how they were told apart is worth keeping.

The proximate cause was a bad module: the nodejs-target glue, which is CommonJS,
copied into a slot that imports the web-target ESM glue. A mistake in a hand-run
build step, not in the library.

The real fault was underneath it. Nothing listened for the Worker's `error`
event, so a Worker that died at module load took every request in flight with it
and answered none. The symptom was not a failure but a hang, which is worse: it
reads as a slow parser, and in CI as an infrastructure problem.

An earlier draft of this document said the library was not implicated, on the
evidence that `harness/wasm-size/api.mjs` passes. That evidence does not reach
that claim. The size harness drives the module directly; the thing that hung is
the Worker transport, which is the default path for every consumer of the
package. The passing and failing evidence sat on opposite sides of the boundary
in question, and §7 already records two earlier bugs with the same symptom in the
same subsystem. The claim was wrong, and the reasoning behind it is the kind this
document exists to refuse.

**The corpus — 1,026 files.** Mozilla's pdf.js test suite (974 files, two decades
of cases kept precisely because they broke something), 20 generated fixtures, 13
LaTeX documents, 3 Chrome print-to-PDF. Fetched rather than vendored: they are
other people's files under other people's licences.

**Invariants**, run over the whole corpus on every CI run:

| Invariant | Passed | Failed | Skipped |
|---|---|---|---|
| I1 identity | 872 | 0 | 140 |
| I2 locality (objects) | 872 | 0 | 140 |
| I3 validity (structural) | 1,012 | 0 | 0 |
| I4 round-trip stability | 872 | 0 | 140 |
| I5 undo exactness | 763 | 0 | 249 |
| I6 tag integrity | 50 | 0 | 962 |
| I7 redaction completeness | 329 | 0 | 683 |
| I8 model stability | 820 | 0 | 192 |
| §10.9 destinations resolve | 199 | 0 | 813 |
| Object round-trip | 1,012 | 0 | 0 |

I8 is model stability: build the flow model, lay it out, write a PDF, reopen it,
build the model again, and compare. It was cited for composition before it
appeared here, which made it a claim with no number behind it. Its 192 skips are
files with no reconstructable text, where there is no model to be stable.

**Every skip is itemised with its reason.** A suite that reports green for
checks it did not run is worse than no suite. I5's 249 skips are 140
recovery-mode files, 105 pages with no content stream, and 2 with no page tree —
none a failure in disguise. I6 skips 919 untagged documents rather than passing
them, because a pass would claim the tagging survived an edit on a file that has
none.

**External judges.** Nothing about correctness is decided by our own parsers
alone:

- **pdf.js** — text extraction compared page by page across the corpus; reads
  back injected glyphs, encrypted documents, redacted output.
- **pdfium** — pixel diffs in four modes, each asking a different question:
  did anything change left of the original ink; did anything change at all; did
  anything change before column N; did anything change *outside* these columns.
  Asking the wrong one produces a green tick that means nothing.
- **qpdf** — `--check` on every generated file, with passwords for the encrypted
  ones.

**Composition is judged by all three, and needs to be.** A font-embedding path
fails characteristically: it produces a document that passes every structural
check and renders as blank boxes. So CI composes five documents from nothing —
two by positioning text directly, three through the layout engine, one of them
Greek in a `/Type0` font — then has pdf.js build the operator list, which forces
the embedded program to be parsed and every glyph translated, and has pdfium
render them. The render check does not trust an exit code: it fails on
`ink ends at column -1`, because a page that drew nothing renders perfectly
happily.

**The site is loaded in a real browser before it deploys.** `demo/browser.mjs`
drives headless Chrome over the DevTools protocol — no puppeteer, no install —
and checks that the documentation rendered and that the editor compiled the
module, got a version back from the library, and opened and modelled the sample.
It also points at the deployed origin, which is the only way to learn whether
what shipped is what was tested.

This check exists because for a long time nothing here loaded the page at all.
The module is built with `--omit-default-module-path`, whose entire effect is to
delete the glue's `import.meta.url` fallback, and the demo called `init()` with
no argument on the strength of a comment asserting the opposite. It reached
`WebAssembly.instantiate(undefined, …)` every time, had never started on any
host since it was written, and passed every check that avoided a browser.

**Fuzzing.** cargo-fuzz targets for the lexer, document open, the filters and the
cross-reference parser, wired into CI as a 60-second smoke run each. No long
campaign has been run.

**Harnesses**, six of them: the invariant suite, a content-layer walk over every
page, a text-extraction differential against pdf.js, a font survey, the
pixel-diff runner, and the WASM size gate.

**Eleven runnable examples**, each writing files an outside tool judges:
`textedit`, `moveimage`, `newpage`, `redact`, `compactfont`, `registerfont`,
`realfont`, `protect`, `paragraphs`, `running`, `fontdump`.

---

## 5. What was refused, and why

Refusals are not gaps. Each of these is a decision that half-building would be
worse than declining.

| Refused | Why |
|---|---|
| OCR / scanned PDFs | Not our problem to solve. `documentKind === 'scanned'` says so, and `paragraphs()` is empty because there is no text. |
| XFA forms | The real content is an XML payload the AcroForm shadows. Editing the AcroForm produces a file whose two halves disagree. |
| Digital signature creation | Detect, preserve, report invalidation. Never create. |
| Rendering as the product | A draw-command emitter, deliberately small. The operators *are* the API. |
| Writing RC4 or `/R` 5 protection | Read for legacy files. Creating new protection with a broken cipher is not a feature. |
| Structural table operations | They move content on a grid that was *inferred*. A misdetected column edge becomes a visibly broken table. Needs `/StructTreeRoot` table elements. |
| Flattening optional content | The decision depends on a configuration the viewer owns and the user can change after saving. |
| Enforcing `/P` permission bits | Reported, never enforced. Enforcement in a library whose source you can read is theatre. |
| CFF font embedding | A `/FontFile3` with an OpenType subtype is a different stream and a different subsetter. Declined by name rather than written into a `/FontFile2`, which would pass every structural check and render nothing. |
| Composite font *injection* | Adding a glyph to a Type0 font a document already embeds. The `/W` array, `/CIDToGIDMap` and the descendant's own subset all have to agree, and getting one wrong silently shifts every glyph after it. |
| Composite font *embedding* | **Not refused.** Composition writes a `/Type0` font with `/Identity-H` whenever the text needs one, which is a different operation from injection: there is no existing font to agree with. |

Each refusal is a named error at the call site, not a silent no-op. A caller
discovers the limit where they hit it, and the message says what would make the
operation possible.

---

## 6. Findings that changed the design

Measurement decided these, not preference.

**The Adobe Glyph List carries more than `/ToUnicode`.** Q1 predicted LaTeX
subset fonts with opaque `g34` names; modern pdfTeX emits `/ToUnicode` for
everything and only six fonts in 1,390 carry opaque names. The AGL resolves 47%
of all glyphs — more than `/ToUnicode` does. A shape-matching fallback is not
justified by any evidence in the corpus.

**`/A` `/D` actions outnumber `/Dest` 3.6 : 1** on links and 4.5 : 1 on outlines.
An implementation following §10.9's sentence literally would find a quarter of
what is there and report the rest as clean.

**`/Threads` has zero real article threads in 960 documents.** Reported and
deliberately not traversed. A traversal nothing can test will be wrong when it
finally matters.

**36% of the corpus's images are rotated or skewed.** A move implemented by
rewriting a bounding box would flatten every one of them — which is why images
are moved by wrapping the drawing operator, never by editing the transform.

**71% of images are inline**, their pixel payload inside the content stream.
That is why the wrap carries the original bytes through untouched.

**Two thirds of optional-content layers are off**, and 96% of the regions on a
page belong to one. "A hidden layer's text is in the document" is the normal
state of every file that has layers, not a corner case.

**Compaction does not have to reach the content streams.** §8.6 warns that
renumbering "would require rewriting every content stream that references the
font". A composite font's streams hold *CIDs*, so rewriting `/CIDToGIDMap`
absorbs the renumbering and no content stream changes.

---

## 7. Mistakes found and fixed

The ones worth recording, because each was silent.

**The demo had never started.** `--omit-default-module-path` deletes the glue's
`import.meta.url` fallback and the page called `init()` bare, on the strength of
a comment claiming the opposite. It reached
`WebAssembly.instantiate(undefined, …)` on every host since it was written. The
failure banner then blamed a missing `wasm-unsafe-eval` for *every* cause,
including that one, so the first person to load the page was sent to look at a
content-security policy that was never involved. Three checks stood between the
bug and the deploy and none of them loaded the page.

**The fuzzer found a real one.** `/N` on an object stream is a number the file
chooses, and both copies of the header parser reserved a `Vec` for it before
reading anything — so a stream claiming more pairs than the vector can address
panicked with `capacity overflow` inside the allocator, reachable straight from
`Document::open`. The loop was already bounded by the header's own length; that
now sizes the reservation, and the two copies are one. The job was also deleting
its own evidence: a crash gave a stack of unsymbolised addresses and threw the
input away with the runner.

**Headings were drawn at body size.** The emitter asked a helper for a block's
size and the helper ignored both its arguments. So a heading was broken to fit at
24pt and drawn at 11pt, on a baseline computed for 24 — wrong glyph size, wrong
position, and short lines that read as a layout bug rather than an emitter one.
The size is now carried on the placed line, where it was known and thrown away.

**Keep-with-next was wrong three times over**, and each fix improved the rendered
page and still left a stranded heading. The reservation left out the gap between
a heading and its section; then it reserved one line while the orphan rule
refuses to leave fewer than two; then it reserved lines at all, when a block is
only split if it starts at the top of a frame and otherwise moves whole. None of
the three had a test, because each needs a particular arrangement to show. The
regression test now sweeps 1,200 frame heights and derives the unsatisfiable
threshold from the same function the engine calls — I computed it by hand twice
and got it wrong twice.

**A float comparison cost a line per column.** `cursor.y` is a running sum of
line heights, so a line needing 13.2 points was offered 13.199999999999989 and
pushed to the next column over one part in 10^15.

**Ink was themed; paper was not.** The editor's canvas drew text in the theme's
foreground onto a permanently white sheet, so dark mode rendered near-white text
on white paper — unreadable, while every control around it looked correct.

**The site was served where it was not built to live.** The browser check mounted
`web/dist` at `/` while the build set `base=/rasura/` for Pages, so every asset
404'd. That one is the check working: it is precisely the deploy it exists to
prevent, and it found it before anyone saw it.

**`/Widths` is indexed by code, not by order of appearance.** "Hamburg" occupies
codes 72, 97, 98, 103, 109, 114, 117 — seven letters across forty-six codes.
Writing seven widths from `/FirstChar 72` piled the letters on top of each other.
Caught by a pixel diff: ink ended at column 225 instead of 353.

**A panic in the object layer.** `pdf_doc_encoding_char` indexed a 32-entry table
at `c - 0x80 + 8`, so any string containing a byte from 0x98 upward panicked, and
the `0x18..=0x1F` range read the wrong table entirely. Found while purging
`/Info` during redaction. §14.4 says the parser must never panic.

**Redaction was page-scoped while verification was document-scoped.** A name
removed from page one and left on page five verifies as *failed*, which is the
right answer. Caught by the corpus within a minute.

**Form XObject spans address the form's stream, not the page's.** When the page
stream is longer, the splice succeeds and rewrites unrelated bytes — leaving
redacted text exactly where it was while appearing to have removed it. Both
`replace_text` and `redact` now refuse.

**Redaction closed up the line.** Removing glyphs and writing the remainder as
one `Tj` let the tail slide left — so the black box the caller draws over the
*original* rectangle covers words nobody asked to hide, while the ones that moved
out from under it stay legible. Now a `TJ` with adjustments that pin every
surviving glyph.

**An empty owner password left the door open.** `/O` and `/U` are checked
independently, so setting a user password and leaving the owner password blank
produced a file that prompts for a password and opens without one. It looked
protected to whoever made it.

**Vertical writing made every character its own line.** `/WMode 1` belongs to the
font's CMap, not the text matrix, so a vertical run has an unrotated matrix and
glyphs that share an x. Rotating the basis a quarter turn fixed clustering *and*
column order at once — the existing "sort by ascending normal" already orders
right to left.

**The encoder cannot tell you a glyph is missing.** It inverts the *decoder*, so
a WinAnsi font "can write" all of Latin-1 — every character has a code. Whether
the embedded program holds an outline at that code is a different question. A
document embedding seven letters of Roboto accepted `É`, wrote code 0xC9, and
drew nothing. The same mistake was in `fontRequirements`, which measured
coverage by asking whether a *width* existed and reported the seven-letter
subset as covering all of Latin.

**Two worker bugs found by the test runner hanging, not failing.** A failed
`open` leaked its Worker — in node a script that never exits, in a browser a
thread per malformed file. And `terminate()` was not awaited, so node's thread
outlived it.

**The dependency gate was red.** Discovered while checking whether
`wasm-bindgen` could be added: `pdfium-render` was banned *and* present, internal
path dependencies tripped the wildcard check, and two new RUSTSEC advisories had
landed. All three were config, and all three are now honest rather than
suppressed.

---

## 8. What remains

| | Where | Note |
|---|---|---|
| Document mode on the facade and in JS | `docs/flow-model.md` | Built in `rasura-flow` behind `accept_regeneration`; not exposed above it. Deliberate: it is the one operation where §2's first property cannot hold, and it should stay hard to reach by accident. **Composition is the other side of the same code and *is* exposed** — because a document that did not exist has no prior bytes to preserve, so there is nothing to break. |
| `add_image` on the facade and in JS | §10.4 | Built in `rasura-edit`; not lifted yet. |
| `insert_page` on the facade and in JS | §11.4 | No longer blocked — composition draws pages — but a caller wanting a page with content should reach for `create`. |
| `replace_image` pixels | §10.4 | Needs a codec to re-encode with — see `resample_image` below. |
| Composition: tables, lists, images | §9.2 | Placed as their text, without rules, bullets or pictures. Counted in `Composition.approximated` rather than dropped silently. |
| Composition: more than one face | §9.2 | One embedded font per document. Bold and italic in a composed document need a second and third, and the `Measurer` ignores the flags rather than faking them. |
| `standalone.html` | — | The single-file inlined artefact did not survive the move to Vite. Recoverable with `vite-plugin-singlefile`. |
| Editor: thumbnails, image drag, font supply | — | The library calls exist and are on the WASM surface; the controls are not wired in the React port. |
| Performance budgets and regression gate | §13 | No benchmarks exist. The largest gap in *verification*. |
| Lazy chunk splitting | §12.3 | One chunk today. `fonts` should load on the first shaping edit. |
| Threaded build | §12.1 | **Blocked, not pending.** It is a second artefact with `SharedArrayBuffer`; the single-threaded package had to exist first. Now it can be started. |
| `resample_image` | §10.4 | Needs a pixel codec — the one piece that does. |
| Redaction steps 2 and 6 | §10.6 | Image data and font-subset glyphs. Reported on every redaction. |
| Font substitution wired to editing | §8.5, §11.4 | The matcher exists; the `substituted` rung is never produced. |
| `set_style`, `insert_paragraph`, `set_z_order` | §9.2 | |
| Vector path provenance | §10.5 | Nothing to move a path *by*. |
| Draw-command emitter's JS surface | §11.6 | Exists in Rust. |
| LRU cap on the page cache | §12.5 | For thousand-page documents. |
| ~~Tutorial and API reference site~~ | §16 | **Done.** `web/`, deployed to Pages. |

### Honest caveats

- **Two of four viewers gate font injection.** pdf.js and pdfium run in CI.
  Acrobat and Preview do not, and §17's Phase 4 exit criterion asks for four.
- **`/R` 5 and 6 reading is tested against our own implementation** of the same
  algorithms, which is weaker than the primitives get (those are pinned to
  published test vectors). Creation now supplies part of the missing evidence —
  pdf.js and pdfium opening a `/R` 6 file we wrote is independent — but a `/R` 6
  file from a real producer would still be worth having.
- **No long fuzzing campaign** has been run, only the CI smoke.
- **`rustybuzz` and `ttf-parser` are unmaintained** (RUSTSEC-2026-0192/0206).
  Accepted with a documented rationale: they are §8.3's shaping engine, there is
  no maintained pure-Rust replacement, and the alternative is HarfBuzz, which
  §4.2 forbids. Both forbid `unsafe`, and the font layer is fuzzed, so a
  malformed font can panic a worker but not corrupt memory.
- **Published, and verified from the registry.** Seven crates on crates.io and
  `rasura@0.1.0` on npm, all at 0.1.0. The npm package was then installed from
  the registry into an empty directory with `--ignore-scripts` and used to edit a
  PDF: nothing local was involved, which is the only version of that check worth
  anything. What has *not* happened is anyone other than us depending on it.
- **`/StemV` is estimated for every embedded font, and always will be.** No sfnt
  table records it — Type 1 carried it, TrueType never did. It is derived from
  `OS/2.usWeightClass` and `stem_v_guessed` says so on every result, which is the
  same rule the fidelity ladder follows. A `/FontBBox` of zero is likewise
  corrected from the vertical metrics rather than passed through, because viewers
  that clip to it would draw nothing, and `bbox_estimated` reports that too.
- **CFF outlines cannot be embedded.** A `/FontFile3` with an OpenType subtype is
  a different stream and a different subsetter. Declined by name rather than
  written into a `/FontFile2`, which would pass every structural check and render
  nothing.
- **The composed page is checked for having ink, not for being right.** pdfium
  says the glyphs drew and pdf.js says the text reads; neither says the
  typesetting is good. The three keep-with-next bugs in §7 were all found by
  looking at a rendered page, and the suite was green after every one of them.
- **The editor's canvas is checked for happening, not for looking right.**
  Pointer handling, dragging and the editing gestures are not driven by any
  check, and Chrome is not Safari.

---

## 9. The two open questions, answered

**Q1 — `/ToUnicode` coverage.** 53.0% across 1,390 embedded fonts, well below the
85% threshold, so the full derivation chain had to exist. The spec's diagnosis
was wrong about *why*; see §6. Write-up:
[q1-tounicode-coverage.md](q1-tounicode-coverage.md).

**Q6 — the bundle floor.** The object layer is 123 KB gzipped as WASM. The whole
`core` chunk — cos, content, layout, font, rustybuzz, ttf-parser and every
generated table — is 413 KB, 45.9% of budget. The shipped module with the API on
top is 419 KB. The module split in §12.3 stands. Write-up:
[q6-bundle-floor.md](q6-bundle-floor.md).

Q2 through Q5 have not been measured.

---

## 10. Reproducing all of it

```bash
# Rust: 1,192 tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check

# The corpus: 1,026 files
./corpus/fetch.sh                     # ~119 MB, Apache-2.0
./corpus/fetch-font.sh                # Roboto, Apache-2.0
cargo run --release -p rasura-invariants
cargo run --release -p rasura-contentwalk

# The WASM module and the npm package
cargo install wasm-bindgen-cli --locked
npm i -g wasm-opt
./crates/rasura-wasm/build.sh     # builds, measures, drives it from node
cd js && npm install && npm test      # 41 tests + tsc --noEmit

# Composition, from no input file at all
cargo run -p rasura-edit --example compose  -- target/compose   # a page, a font, text
cargo run -p rasura-flow --example compose  -- target/composed  # a flow model

# Judged by outsiders
./harness/pixeldiff/fetch.sh          # pdfium, test-only
cargo run -p rasura-edit --example redact -- target/redact
cargo run --release -p rasura-pixeldiff -- \
    target/redact/before.pdf target/redact/after.pdf --changed-within 420 580
node harness/textdiff/validate-injected.mjs target/composed/greek.pdf \
    "Ελληνικά Δεν υπάρχει…"            # pdf.js builds the Type0 font

# The site, and the only check that proves it runs
mkdir -p web/public/wasm
cp target/pkg/web/rasura_wasm.* web/public/wasm/ && cp demo/sample.pdf web/public/
npm --prefix web ci && npm --prefix web run build
node demo/browser.mjs                 # headless Chrome, both routes
RASURA_DEMO_ORIGIN=https://myketheguru.github.io/rasura node demo/browser.mjs
```

---

## 11. Sizes

| | |
|---|---|
| WASM, raw after `wasm-opt -Oz` | 1,076.4 KB |
| **WASM, gzipped** | **448.8 KB** — 49.9% of the 900 KB budget |
| WASM, brotli | 351.5 KB |
| npm tarball | 459.1 KB |
| npm unpacked | 1.3 MB |
| Site JS, gzipped | 153.9 KB — React, Radix, the router and both pages |
| Site CSS, gzipped | 6.8 KB |

The site's JavaScript is not counted against §12.3's budget and should not be:
the budget is about what a *caller* ships when they install the package, and none
of React reaches them. The module the site loads is the same 419 KB artefact the
gate measures.

A subset is worth its own line, because it is the number that decides whether
composition is usable: **515 KB of Roboto becomes a 14.5 KB `/FontFile2` for the
24 glyphs a short document draws.**

The size probes measure the *floor* — what the layers cost with no API on top —
and the shipped artefact is measured separately, because the budget is about the
thing that ships.
