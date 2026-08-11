# Rasura — Engineering Specification

**A browser-native SDK for true PDF editing.**

Version 1.0 of this document. This is the complete product specification, not an
MVP scope. Delivery phasing is in §17.

---

## 1. Name and positioning

**Rasura** — Latin for a scraping: the erasure of a parchment so the page can be
written over, with traces of the earlier text surviving beneath. That is
precisely the file model: new revisions appended, prior revisions intact
underneath.

- npm package: `rasura` (scoped alternates: `@rasura/core`, `@rasura/fonts`)
- Rust workspace: `rasura-*` crates
- Public type prefix: none. Types are `Document`, `Page`, `Paragraph`, `EditSession`.

**Positioning statement.** Every existing browser PDF library does one of two
things: it renders (pdf.js), or it draws new content on top of old (pdf-lib,
jsPDF). None of them *edit*. Rasura edits — it reconstructs the semantic
document from the page description, mutates it, and writes it back with the
untouched 99% of the file byte-identical.

---

## 2. What "true editing" means here

A PDF is a page-description format. There are no paragraphs, no words, frequently
no spaces — only positioned glyph runs. "True editing" is the round trip:

```
bytes → objects → content streams → glyph runs → document model
                                                       ↓ mutate
bytes ← incremental append ← patched streams ← reflowed runs
```

Three properties define correctness, in priority order:

1. **Non-locality is forbidden.** An edit on page 40 must not change the rendered
   output of any other page by a single pixel, nor alter the bytes of any object
   it did not need to touch.
2. **Fidelity is reported, never assumed.** When the engine cannot make an exact
   edit, it says so in a typed result. It never silently substitutes a font,
   silently drops kerning, or silently overlays a text box.
3. **The file remains a valid PDF.** Output passes `qpdf --check` and veraPDF
   structural validation, and opens in Acrobat, Preview, Chrome, and Firefox
   without repair prompts.

Anything that violates (1) or (2) is a bug of the highest severity, even if the
result looks correct.

---

## 3. Non-goals

Explicitly out of scope. Do not build these; reject them at the API boundary with
a typed error.

| Not doing | Why | Behaviour |
|---|---|---|
| Scanned / image-only PDFs | Requires OCR + raster inpainting; a different product | `open()` succeeds, `paragraphs()` returns empty, `documentKind === 'scanned'` |
| XFA forms | Deprecated by ISO; Adobe-proprietary | Detect `/XFA` in AcroForm, expose `hasXfa`, refuse form edits |
| Rendering as the primary product | pdf.js does this well and is Apache-2.0 | Ship an optional draw-command emitter (§11.6); interop with pdf.js documented |
| Digital signature creation | Needs key custody; regulatory surface | Detect, preserve, report invalidation. Never create. |
| Server-side rendering farm | Not the shape of the business | Rust crates are runtime-agnostic; someone else can build it |
| Encryption *creation* with new passwords | Decryption is needed to edit; re-encryption is a separate feature | Phase 8, opt-in |

---

## 4. Architecture

### 4.1 Crate graph

```
rasura-cos        objects, lexer, xref, filters, decryption, writer
       ↑
rasura-content    content-stream tokenizer, graphics state machine, serializer
       ↑
rasura-font       font parsing, shaping, subsetting, glyph injection
       ↑
rasura-layout     glyph runs → lines → blocks → document model
       ↑
rasura-edit       edit operations, reflow, stream patching, transactions
       ↑
rasura            facade crate — the Rust public API
       ↑
rasura-wasm       wasm-bindgen surface, worker protocol
       ↑
js/                   TypeScript wrapper, Worker harness, type definitions
```

Strict layering. A crate may depend only on crates below it. No cycles. No
crate above `cos` may know what a cross-reference table is; no crate below
`layout` may know what a paragraph is.

`rasura-render` (optional, Phase 6) hangs off `content` and is not on the
critical path.

### 4.2 The no-C++ rule

Everything is Rust. Do not vendor qpdf, MuPDF, Poppler, or PDFium into the
build. Mixing Emscripten output with `wasm-bindgen` is a build-system tarpit and
doubles the bundle. The object layer is the best-specified part of PDF — ISO
32000 tells you exactly what to do. Write it.

`pdfium` may be used as a **test-only** reference renderer for the pixel-diff
harness (§14), invoked from the native test runner, never shipped.

### 4.3 Licensing constraint

Every runtime dependency must be MIT, Apache-2.0, BSD, ISC, or Zlib. **No GPL,
no LGPL, no AGPL, no MPL-with-file-scope-ambiguity.** This is a hard build gate:
`cargo-deny` runs in CI with an allowlist, and a violation fails the build.

Approved runtime dependencies:

| Crate | Purpose | Licence |
|---|---|---|
| `read-fonts` / `write-fonts` / `skrifa` (fontations) | font parsing, outline access, table writing | MIT/Apache-2.0 |
| `rustybuzz` | HarfBuzz-compatible shaping | MIT |
| `allsorts` | subsetting, CFF charstrings (evaluate vs write-fonts) | Apache-2.0 |
| `flate2` + `miniz_oxide` | FlateDecode/Encode | MIT/Apache-2.0 |
| `weezl` | LZWDecode | MIT/Apache-2.0 |
| `aes`, `cbc`, `rc4`, `sha2`, `md-5` (RustCrypto) | standard security handler | MIT/Apache-2.0 |
| `wasm-bindgen`, `js-sys`, `web-sys` | WASM boundary | MIT/Apache-2.0 |
| `thiserror` | error types | MIT/Apache-2.0 |

Deliberately rejected: `lopdf` (too thin, awkward object model), `pdf-rs`
(incomplete, unstable), anything wrapping MuPDF or Poppler.

---

## 5. Layer 1 — `rasura-cos` (object layer)

The Carousel Object System. Reference: ISO 32000-1 §7.

### 5.1 Object model

```rust
pub enum Object {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(PdfString),      // preserves literal-vs-hex origin
    Name(Name),             // #-decoded, but original bytes retained
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjId),       // (number, generation)
}
```

**Byte-preservation requirement.** `PdfString` and `Name` retain their original
encoded bytes alongside the decoded value. Re-serialising an unmodified object
must reproduce its input bytes exactly, including escape-sequence choices and
`#`-hex name encoding. This is what makes the "open → save with no edits →
byte-identical" invariant achievable (§14.2, invariant I1).

`Dictionary` preserves key insertion order. Do not use a `HashMap` as the
backing store; use an order-preserving map with a hash index.

`Stream` holds the *raw* (still-filtered) bytes plus the dictionary. Decoding is
lazy and cached. A stream that is never decoded is never re-encoded.

### 5.2 Lexer and parser

A byte-level lexer over the whole file buffer. Must handle, without exception:

- Comments (`%` to EOL), including `%PDF-x.y` header and `%%EOF`
- All three EOL conventions, including a lone `\r` after `stream`
- Literal strings with nested parens, `\n \r \t \b \f \( \) \\`, octal `\ddd`,
  and line continuations
- Hex strings with whitespace and an odd final digit (pad with `0`)
- Names with `#xx` escapes, including `#20` and names containing `#` itself
- Reals in the forms `.5`, `-.002`, `4.`, `34.5`, `+17`, and the illegal-but-common
  `6.02e23` (accept, note as a leniency)
- Integers exceeding `i32` (clamp per spec, but do not panic)
- `stream` keyword followed by `\r\n` or `\n` but **not** a bare `\r`
- `/Length` given as an indirect reference (resolve; if resolution is impossible
  or wrong, scan forward for `endstream` and use the scanned length — record a
  `Leniency::LengthRecovered`)

**Leniency log.** Every deviation from spec that the parser tolerates is recorded
in `Document::leniencies() -> Vec<Leniency>`. This is a diagnostic surface, and
it is also the honest answer to "why did this file behave oddly."

### 5.3 Cross-reference resolution

Support all four forms and their combinations:

1. Classic xref tables (`xref` / `trailer`), with `/Prev` chains
2. Cross-reference streams (`/Type /XRef`, `/W`, `/Index`, `/Prev`) — §7.5.8
3. Object streams (`/Type /ObjStm`, `/N`, `/First`) — §7.5.7
4. Hybrid-reference files (`/XRefStm` in a classic trailer)

Multi-revision files: walk `/Prev` to build the full revision chain. Retain the
chain — `Document::revisions()` exposes it, and the redaction path (§10.6) needs
to know that old bytes are still present.

**Recovery mode.** If the startxref offset is wrong, the xref is malformed, or
`/Root` is unresolvable, fall back to a full-file scan:

- Regex-free scan for `\d+ \d+ obj` at line starts, building a synthetic xref
- Take the highest-generation instance of each object number
- Locate the trailer by scanning for a dictionary containing `/Root`; if absent,
  scan for an object with `/Type /Catalog`
- Record `Leniency::XrefReconstructed`

Recovery mode forces `SaveMode::FullRewrite` (§9.5) — incremental append onto a
file whose xref you had to guess is not safe.

### 5.4 Stream filters

| Filter | Decode | Encode | Notes |
|---|---|---|---|
| `FlateDecode` | yes | yes | with `/DecodeParms` predictors: PNG 10–15, TIFF 2 |
| `LZWDecode` | yes | no | `/EarlyChange` 0 and 1 |
| `ASCIIHexDecode` | yes | yes | |
| `ASCII85Decode` | yes | yes | `z` shorthand, `~>` terminator |
| `RunLengthDecode` | yes | yes | |
| `DCTDecode` | pass-through | pass-through | image data stays JPEG |
| `JPXDecode` | pass-through | pass-through | JPEG 2000; no transcode |
| `JBIG2Decode` | pass-through | pass-through | preserve `/JBIG2Globals` |
| `CCITTFaxDecode` | pass-through | pass-through | |
| `Crypt` | yes | yes | identity + security handler |

Image filters are pass-through by design: Rasura does not decode image
pixels unless an image-editing operation demands it (§10.4). Filter chains
(arrays of filters) must be applied in order.

**Re-encoding policy.** When a stream's content is unchanged, re-emit the
original raw bytes verbatim. When changed, re-encode with the *same* filter
chain and, for Flate, the same compression level where detectable. Never
"helpfully" recompress an untouched stream.

### 5.5 Encryption

Standard security handler, ISO 32000-1 §7.6. Required support:

- `/V` 1, 2, 4, 5 and `/R` 2, 3, 4, 5, 6
- RC4 40-bit and 128-bit; AES-128 (`AESV2`); AES-256 (`AESV3`, `/R` 6, the
  ISO 32000-2 algorithm with SHA-256/384/512 hardening loop)
- Crypt filters: `/CF`, `/StmF`, `/StrF`, `/EFF`, `Identity`
- Empty user password (the overwhelmingly common case) attempted automatically
- Owner password path for permission bypass
- `/EncryptMetadata` false
- Per-object key derivation (object number + generation salt) for RC4/AES-128;
  single file key for AES-256

Permission bits (`/P`) are **advisory in this library**. Rasura reports them
via `document.permissions` and does not enforce them; enforcement is the
consuming application's legal and product decision, not the parser's. Document
this clearly in the README — it is a question every evaluator will ask.

Encrypted documents saved incrementally must re-encrypt new strings and streams
with the existing file key. Changing the password is Phase 8.

### 5.6 Writer

Two modes:

**`SaveMode::Incremental` (default).** Append to the original bytes:

```
<original file bytes, unmodified>
<updated and new indirect objects>
<xref section covering only changed objects>
<trailer with /Prev → previous startxref>
startxref
%%EOF
```

Rules:
- Match the original file's xref style. A file with xref streams gets an xref
  stream; a classic file gets a classic table. Do not "upgrade" the format.
- Preserve `/ID[0]`; regenerate `/ID[1]` per spec.
- Preserve `/Root`, `/Info`, `/Encrypt` references unless changed.
- If the original was linearised, the appended revision breaks linearisation.
  Remove `/Linearized` from the *new* trailer's view and record
  `Warning::LinearizationBroken`. Do not attempt to re-linearise.
- Objects that live inside an `/ObjStm` and are modified are promoted to
  top-level indirect objects in the new revision. Do not rewrite the original
  object stream.

**`SaveMode::FullRewrite` (opt-in, forced for redaction and recovery).**
Serialise the whole document fresh. Compacts, drops unreferenced objects,
optionally re-linearises. Invalidates the byte-identity invariant by design —
callers must ask for it.

---

## 6. Layer 2 — `rasura-content` (content streams)

Reference: ISO 32000-1 §8 (graphics) and §9 (text).

### 6.1 Operator coverage

The tokenizer must handle the complete operator set. Grouped:

- **Graphics state**: `q Q cm w J j M d ri i gs`
- **Path construction**: `m l c v y h re`
- **Path painting**: `S s f F f* B B* b b* n`
- **Clipping**: `W W*`
- **Colour**: `CS cs SC SCN sc scn G g RG rg K k`
- **Text objects**: `BT ET`
- **Text state**: `Tc Tw Tz TL Tf Tr Ts`
- **Text positioning**: `Td TD Tm T*`
- **Text showing**: `Tj TJ ' "`
- **Type 3 glyphs**: `d0 d1`
- **XObjects**: `Do`
- **Inline images**: `BI ID EI` (binary payload — the tokenizer must skip to the
  matching `EI` using length heuristics plus a whitespace-delimited check)
- **Shading**: `sh`
- **Marked content**: `MP DP BMC BDC EMC`
- **Compatibility**: `BX EX`

Unknown operators inside `BX`/`EX` are skipped silently; outside, they are
recorded as a leniency and skipped.

### 6.2 Span preservation — the load-bearing requirement

Every parsed operator retains its byte span in the *decoded* stream buffer:

```rust
pub struct Op {
    pub kind: OpKind,
    pub operands: SmallVec<[Object; 4]>,
    pub span: Range<usize>,   // into the decoded stream
}
```

This is what makes surgical patching possible. To edit a paragraph, the edit
layer splices replacement bytes into the decoded buffer at exactly the spans of
the affected operators and leaves every other byte alone. Without spans you are
forced to re-serialise whole streams, and re-serialisation is where fidelity
dies.

### 6.3 Graphics and text state machine

Full state stack with `q`/`Q`. Text state per ISO 32000-1 §9.3:
`Tc` (char spacing), `Tw` (word spacing), `Tz` (horizontal scale, percent),
`TL` (leading), `Tf`/`Tfs` (font, size), `Tr` (render mode), `Ts` (rise).

Glyph displacement, horizontal writing mode:

```
tx = ((w0 − Tj/1000) × Tfs + Tc + Tw) × (Tz/100)
```

Critical details that are routinely got wrong:

- `Tw` applies **only** to a single-byte character code 32, and therefore does
  **not** apply to most CID fonts (where code 32 is usually not a two-byte
  space). Getting this wrong misplaces every subsequent glyph on the line.
- `Tj` here is the number from a `TJ` array element, applied *before* scaling.
- Vertical writing mode (`/WMode 1`) uses `ty` and `w1` — support it; CJK
  documents depend on it.
- `Tm` replaces the text matrix outright; `Td`/`TD` translate the *line* matrix;
  `T*` is `0 −TL Td`.
- The `'` operator is `T*` then `Tj`. The `"` operator sets `Tw` and `Tc` then
  does `'`.

### 6.4 Resource resolution and recursion

- Page attribute inheritance up the `/Pages` tree: `/Resources`, `/MediaBox`,
  `/CropBox`, `/Rotate`.
- `/Contents` may be an array of streams; they concatenate into one logical
  stream **with a whitespace separator**, and a token may not span the boundary.
  Retain the mapping from logical offsets back to (stream index, offset) so
  patches land in the right object.
- Form XObjects (`/Subtype /Form` via `Do`): recurse with the CTM composed by
  `/Matrix`, using the form's own `/Resources` (falling back to the page's).
  Guard against cycles with a depth limit and a visited set.
- Tiling patterns and Type 3 glyph procedures are content streams too. Parse
  them with the same machinery.

---

## 7. Layer 3 — `rasura-layout` (reconstruction)

This layer and §8 are the product. Everything else is table stakes.

### 7.1 Glyph run extraction

```rust
pub struct PositionedGlyph {
    pub gid: u16,
    pub code: u32,          // the code as it appeared in the string
    pub unicode: SmolStr,   // may be multi-char (ligature) or empty (unknown)
    pub advance: f32,       // text-space
    pub origin: Point,      // device-space, after CTM × Tm
    pub span: Range<usize>, // byte range within the showing operator
}

pub struct GlyphRun {
    pub font: FontRef,
    pub size: f32,
    pub ctm: Matrix,
    pub text_state: TextState,
    pub fill: Colour,
    pub glyphs: Vec<PositionedGlyph>,
    pub op_span: Range<usize>,
    pub mcid: Option<u32>,   // enclosing marked-content id, if tagged
}
```

### 7.2 Unicode derivation

Try in order; stop at the first that yields a mapping. Record which strategy won
per font — this is a headline diagnostic.

1. `/ToUnicode` CMap (§9.10.3). Parse `bfchar` and `bfrange` fully, including
   ranges whose destination is an array and multi-code-unit UTF-16BE values with
   surrogate pairs.
2. For simple fonts: `/Encoding` base (`/WinAnsiEncoding`, `/MacRomanEncoding`,
   `/MacExpertEncoding`, `/StandardEncoding`) plus `/Differences`, then glyph
   name → Unicode via the Adobe Glyph List.
3. For the standard 14 fonts with no `/Encoding`: the font's built-in encoding
   (StandardEncoding, or Symbol/ZapfDingbats' own).
4. For composite fonts: the `/Encoding` CMap (predefined name such as
   `UniJIS-UCS2-H`, or embedded stream) → CID, then `/CIDSystemInfo`
   registry-ordering to a Unicode CMap for the known Adobe collections.
5. Reverse lookup through the embedded font's `cmap` table (GID → code point).
6. Glyph-name heuristics: `uniXXXX`, `uXXXXX`, `gNN`, `cidNN`, `index NN`,
   and the `name.alt` suffix convention.
7. Failure: assign a Private Use Area sentinel, mark the glyph
   `unicode_confidence: None`, and set the containing paragraph's
   `textConfidence` to `Partial`.

**Do not silently succeed at step 7.** A paragraph containing unmapped glyphs is
still editable in the sense that the glyphs can be moved, but text-level editing
of it is degraded and the API must say so.

Subset fonts produced by LaTeX (`pdftex`, `dvips`) are the standard failure
case: no `/ToUnicode`, and glyph names like `g34`. Measure coverage across the
corpus early (§18, Q1) — the answer determines how much of step 6 you need.

### 7.3 Word segmentation

Within a run, insert a word boundary when any of:

- An explicit space glyph appears (Unicode U+0020, U+00A0, U+2000–U+200A).
- A `TJ` negative adjustment exceeds a threshold. Threshold: `0.20 × Tfs` in
  text space, but calibrate against the font's own space advance where the font
  has a space glyph — some typesetters use adjustments of `−200` routinely for
  kerning.
- A positional gap between consecutive glyph origins exceeds the expected
  advance by more than `0.25 × Tfs`, after accounting for `Tc`, `Tw`, `Tz`.
- A new `Tm`/`Td` moves the pen non-monotonically.

For scripts without inter-word spaces (Thai, Khmer, CJK), do not segment; treat
the run as a single word and let the shaper handle it.

### 7.4 Line assembly

Cluster glyphs into lines by baseline in **device space** (after CTM), not text
space — rotated and skewed text must still form lines.

- Project each glyph origin onto the baseline normal; cluster with tolerance
  `0.3 × Tfs`.
- Superscripts and subscripts: detected by non-zero `Ts` or by a size drop
  greater than 20% with a baseline offset less than `0.6 × Tfs`. They join the
  parent line as a styled run, not a separate line.
- Handle interleaved runs: many producers emit text out of visual order
  (footnote markers, ligature fixups). Sort by position within the line, but
  retain the original operator order for patching.
- Detect and merge the "one `Tj` per character" pattern that some producers emit.

### 7.5 Block and column detection

Two-stage, with a fallback:

**Stage 1 — recursive XY-cut.** Build a projection profile of glyph bounding
boxes on both axes. Cut at the widest valley exceeding a threshold
(`1.5 × median line height` vertically, `0.8 × median char width`
horizontally). Recurse. This yields a tree whose leaves are candidate blocks and
whose traversal gives reading order.

**Stage 2 — docstrum fallback.** When XY-cut produces a single undividable
region with high internal variance (magazine layouts, wrapped text around
figures), fall back to nearest-neighbour angle-and-distance clustering.

**Ruling lines matter.** Collect stroked paths from the content stream before
cutting. A horizontal rule is a strong cut hint; a rectangle grid is a table
signal (§7.7).

### 7.6 Paragraph and style reconstruction

Within a block, split into paragraphs on:

- Leading discontinuity: inter-line gap exceeding `1.3 ×` the block's modal gap
- First-line indent: a line whose left edge is indented relative to the modal
  left edge, where the *previous* line ended short of the right edge
- Style discontinuity: a change in font, size, or colour at a line boundary
- Explicit tagging: an `/MCID` boundary in a tagged document (authoritative —
  prefer it over heuristics when `/StructTreeRoot` is present)

Infer per paragraph:

- **Alignment**: from left-edge and right-edge variance across lines.
  Left-aligned = low left variance, high right variance. Justified = both low,
  with the last line excepted. Centred = both vary, midpoints stable.
- **Leading**: modal inter-baseline distance.
- **Indent**: first-line offset, left margin, right margin.
- **Style runs**: contiguous glyph spans sharing font, size, colour, `Tr`, `Ts`.
- **Hyphenation**: a line ending in U+002D/U+00AD where the next line begins
  lower-case is a soft break. Record it so the reflow engine can un-hyphenate
  and re-hyphenate. Store the original as `hyphenationWasPresent`.

### 7.7 Tables, headers, footers

- **Tables**: detected from ruling-line grids, or from ≥3 lines sharing ≥2
  aligned column edges. Expose as `Table { rows, cols, cells: Vec<Paragraph> }`.
  Cell-level editing reflows within the cell; column-width changes are a
  separate, explicit operation.
- **Headers and footers**: content within the top/bottom 12% of the media box
  that repeats in position across ≥3 pages, allowing a numeric field to vary.
  Expose as `RunningElement` with `isPageNumber` detection so that editing one
  can optionally propagate to all.
- **Footnotes**: a block at the page bottom, separated by a short rule, with a
  smaller modal font size. Link to in-text markers by matching superscript
  numerals.

### 7.8 Document model

```rust
pub struct DocumentModel {
    pub pages: Vec<PageModel>,
    pub structure: Option<StructTree>,  // from /StructTreeRoot when tagged
    pub reading_order: Vec<BlockId>,
}

pub struct PageModel {
    pub blocks: Vec<Block>,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotate: i32,
}

pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image(ImageBlock),
    Vector(VectorBlock),
    Running(RunningElement),
    Unknown(RawBlock),      // preserved verbatim, never reflowed
}
```

`Block::Unknown` is important. Anything the reconstruction cannot confidently
classify is preserved as opaque content, rendered and moved but never reflowed.
Guessing is worse than declining.

---

## 8. Layer 4 — `rasura-font` (the hard part)

### 8.1 Why this layer decides the product

Embedded fonts are almost always **subset** — they contain only the glyphs the
document actually used. Type a character outside that set and there is no glyph
to draw. Every competitor resolves this silently and badly. Rasura resolves
it explicitly.

There is no system font access in a browser. The developer must supply fonts.
This constraint is promoted into the public API (§11.3).

### 8.2 Parsing

| Font type | Container | Required |
|---|---|---|
| Type 1 | `/FontFile` | eexec decryption, charstring Type 1 interpretation, `/Encoding` from the font program |
| TrueType | `/FontFile2` | `glyf`, `loca`, `cmap`, `hmtx`, `head`, `hhea`, `maxp`, `post`, `OS/2`, `vmtx`/`vhea` for vertical |
| CFF / Type 1C | `/FontFile3` `/Subtype /Type1C` | charstring Type 2, charset, encoding, private dict, subrs |
| CID CFF | `/FontFile3` `/Subtype /CIDFontType0C` | FDArray, FDSelect |
| OpenType | `/FontFile3` `/Subtype /OpenType` | either outline flavour |
| Type 3 | `/CharProcs` | glyphs are content streams; parse with `rasura-content` |

Non-embedded fonts: the standard 14 (Helvetica, Times, Courier ×4 styles,
Symbol, ZapfDingbats) plus arbitrary named fonts. Ship AFM metrics for the
standard 14 so that layout is correct even without the outlines, and map them to
registered substitutes for rendering and editing.

### 8.3 Shaping

Use `rustybuzz`. The subtlety: PDF stores **post-shaping glyph IDs**. You cannot
know which shaper, features, or version produced the original sequence. Therefore:

**Reshape boundary rule.** Only reshape the minimal span that contains the edit,
expanded outward to the nearest word boundaries on both sides, and never beyond
the enclosing line. Glyphs outside that span keep their original GIDs and
positions byte-for-byte. This bounds the blast radius of a shaper disagreement
to the words the user actually touched.

When reshaping, derive features from the original where inferable: if the
original sequence contains a GID that the font's `GSUB` maps only under `liga`,
enable `liga`; if a `kern`-adjusted pair appears with a `TJ` adjustment matching
the font's kern value, the producer applied font kerning and you should too.
Record `kerningSource: 'font' | 'producer' | 'none'`.

Complex scripts: Arabic, Indic, Hebrew, Thai, and CJK vertical must work. This
is `rustybuzz`'s job; your job is to pass correct script, language, and
direction, derived from the run's Unicode content via a bundled script-property
table (not full ICU — a compact `unicode-script` table).

### 8.4 Glyph injection

When an edit needs a glyph absent from the embedded subset, and a suitable
source font is registered:

**TrueType (`/FontFile2`)**
1. Extract the outline from the source font.
2. Append to `glyf`, rebuild `loca` (widen to long format if the table crosses
   the 32k short-format limit — this is a common silent corruption).
3. Extend `hmtx` and bump `hhea.numberOfHMetrics`.
4. Update `maxp.numGlyphs`.
5. If composite glyphs are pulled in, pull their components too, transitively.
6. Drop hinting (`fpgm`, `prep`, `cvt `) from the *injected* glyph only if the
   source hinting is incompatible; never strip the existing tables.

**CFF (`/FontFile3`)**
1. Extract and re-encode the Type 2 charstring, resolving local and global
   subroutines from the source font (inline them — do not attempt to merge subr
   indexes).
2. Append to the CharStrings INDEX, extend the charset.
3. For CID-keyed CFF, place in the correct FD and extend FDSelect.

**PDF-level updates, both flavours**
- Simple fonts: extend `/Widths`, adjust `/FirstChar`/`/LastChar`, add
  `/Differences` entries in `/Encoding`.
- Composite fonts: extend `/W`, `/CIDToGIDMap` (if a stream, rewrite; if
  `/Identity`, no change), `/DW` unchanged.
- Extend `/ToUnicode` with the new mappings. **Always.** A font you injected
  into must remain text-extractable.
- `/FontDescriptor`: widen `/FontBBox` if the new glyph exceeds it; leave
  `/StemV`, `/Flags`, `/ItalicAngle` alone.

Result: `Fidelity::Reembedded`.

### 8.5 Font matching for substitution

When no source font is registered for the original typeface, score registered
fonts against the original's `/FontDescriptor`:

```
score = w1·|ΔStemV| + w2·|ΔItalicAngle| + w3·|ΔCapHeight| + w4·|ΔXHeight|
      + w5·flagMismatch(Serif, FixedPitch, Script, Symbolic)
      + w6·avgWidthDelta   // over the glyphs both fonts share
```

The `avgWidthDelta` term dominates in practice: a metric-compatible substitute
(Liberation for Arial, TeX Gyre for the URW/Adobe families) reflows almost
identically, while a visually similar but metrically different font shifts every
line.

Substitution always returns `Fidelity::Substituted` with the chosen font and the
score, so the caller can reject it. Never substitute without saying so.

### 8.6 Subsetting on save

Default: **sparse-preserving.** Keep the original GID numbering, add injected
glyphs at the end, do not renumber. Renumbering would require rewriting every
content stream that references the font — exactly the non-local change §2
forbids.

`SubsetPolicy::Compact` (opt-in, `FullRewrite` only) renumbers and prunes unused
glyphs for size. Offer it; never default to it.

### 8.7 Type 3 fonts

Glyph procedures are content streams. Reading works. Editing text in a Type 3
font is supported only when every needed glyph already exists; there is no
sensible way to synthesise a new procedure. Return
`EditError::Type3GlyphMissing` with the list of missing codes.

---

## 9. Layer 5 — `rasura-edit` (mutation and commit)

### 9.1 Transaction model

All mutation goes through an `EditSession`. Operations are accumulated, each
returning a report; nothing touches the document until `commit()`.

```rust
let mut session = doc.edit();
let report = session.replace_text(paragraph_id, range, "new text")?;
if report.fidelity != Fidelity::Exact { /* caller decides */ }
session.commit()?;
```

- Operations are recorded in an op log with inverses, giving undo/redo for free.
- `commit()` is atomic: either all patches apply or none do.
- A session holds a snapshot version; concurrent sessions on the same document
  conflict and the second `commit()` fails with `EditError::StaleSession`.

### 9.2 Operation catalogue

**Text**
- `replace_text(para, range, text)`
- `insert_text(para, offset, text)`
- `delete_range(para, range)`
- `set_style(para, range, StyleDelta)` — font, size, colour, bold/italic
  (resolved to a real family member, never synthesised obliquing), underline,
  strike
- `split_paragraph(para, offset)` / `merge_paragraphs(a, b)`
- `set_alignment(para, align)`, `set_leading(para, value)`,
  `set_indent(para, IndentSpec)`

**Blocks**
- `move_block(block, point)`, `resize_block(block, rect)`
- `delete_block(block)`, `insert_paragraph(page, rect, text, style)`
- `set_z_order(block, index)`

**Tables**
- `set_cell(table, row, col, text)`, `insert_row`, `delete_row`,
  `insert_column`, `delete_column`, `set_column_width`

**Images and vectors** — §10.4, §10.5

**Pages**
- `insert_page(index, PageSpec)`, `delete_page`, `move_page`, `rotate_page`,
  `set_crop_box`, `import_pages_from(other_doc, range)`

**Annotations and forms** — §10.7, §10.8

**Redaction** — §10.6

### 9.3 Reflow

Scope is the paragraph. Never wider unless the caller opts into overflow
propagation.

**Line breaking.** Two algorithms, selectable:
- `Greedy` (default) — matches what most producers did, so re-breaking a
  paragraph after a small edit usually reproduces the original break points.
- `KnuthPlass` — better typography, but will re-break lines the user did not
  touch. Opt-in.

The greedy default is a fidelity decision, not a laziness one. Document it.

**Justification.** If the paragraph was justified, the original inter-word
spacing was achieved by some combination of `Tw`, `Tz`, and `TJ` adjustments.
Detect which the producer used and reproduce *that* mechanism; a paragraph
justified with `Tw` that you re-justify with `TJ` arrays will look subtly
different and will diff visually.

**Overflow policy**, set per session:

| Policy | Behaviour |
|---|---|
| `Refuse` (default) | If reflow exceeds the original block box, fail with `EditError::Overflow { lines_over }` and change nothing |
| `Grow` | Extend the block downward, pushing subsequent blocks on the page; cascade to following pages if needed |
| `Allow` | Let it overflow the box; caller renders and decides |
| `Shrink` | Reduce size/leading within a bounded range to fit; report the applied scale |

`Grow` cascading across pages is the hardest case: it changes page count, which
means fixing up outlines, destinations, link annotations, and the structure
tree. Implement it, gate it behind an explicit flag, and test it hard.

### 9.4 Stream patching

The core routine. Given a reflowed paragraph:

1. Compute the set of affected operators from the paragraph's glyph runs — this
   is the union of their `op_span`s, expanded to enclosing `BT`/`ET`.
2. Generate replacement operator bytes for the new content: `Tf` if the font
   changed, `Tm`/`Td` for each new line origin, `TJ`/`Tj` with the new glyph
   codes and adjustments.
3. Splice into the decoded stream buffer at the affected spans. **Every byte
   outside those spans is copied verbatim.**
4. Re-encode the stream with its original filter chain.
5. Mark the containing object dirty.

Number formatting in generated operators: match the original's precision. A
producer that wrote `72.0` should not get `72` back; a producer that wrote
`0.0001` should not get `1e-4`. Sample the original stream's numeric formatting
and mirror it. This matters because diffs are how users audit you.

### 9.5 Commit and save

1. Serialise dirty objects.
2. Choose save mode: `Incremental` unless the document is in recovery mode, a
   redaction occurred, or the caller asked for `FullRewrite`.
3. Write per §5.6.
4. Return the new bytes plus a `SaveReport { mode, bytes_appended,
   objects_written, invariants_checked }`.

### 9.6 Digital signatures

Detect `/Sig` fields with a `/ByteRange`. On save:

- Incremental append **preserves** the signed revision — a validator can still
  verify the earlier byte range and will report "signed version available,
  document modified afterwards." That is the correct and honest outcome.
- Full rewrite **destroys** it irrecoverably.

Report `SignatureImpact::{ PriorRevisionPreserved, Destroyed }` before saving,
and require an explicit acknowledgement flag when the impact is `Destroyed`.

---

## 10. Beyond text

### 10.1 Tagged PDF and accessibility

If `/StructTreeRoot` is present, the structure tree is authoritative for reading
order and paragraph boundaries (§7.6), and it **must be maintained across
edits.** Nobody else does this, and it is a genuine differentiator for anyone
under WCAG, Section 508, EN 301 549, or the European Accessibility Act.

Requirements:
- Maintain the `/MCID` → structure element mapping through patching. Rewriting a
  `BDC`/`EMC` pair must keep the same MCID unless the element genuinely changed.
- Renumber MCIDs consistently when content is inserted or removed.
- Update `/ParentTree` and `/ParentTreeNextKey`.
- Preserve `/Alt`, `/ActualText`, `/E`, `/Lang` on affected elements; when text
  is replaced and `/ActualText` duplicated it, update both or clear the
  duplicate.
- New paragraphs inserted into a tagged document get a `/P` element in the right
  tree position, not appended at the end.
- Expose `document.taggedStatus: 'untagged' | 'tagged' | 'tagged-degraded'` and
  a `validateTags()` returning the same class of findings veraPDF would.

### 10.2 Optional content (layers)

Preserve `/OCProperties`, `/OCGs`, `/OCMDs`. Expose layer visibility state.
Content inside a `BDC /OC` block belongs to that layer; edits must stay inside
it. Do not flatten layers.

### 10.3 Metadata

Dual-surface: the `/Info` dictionary and the XMP `/Metadata` stream. They
routinely disagree. Expose both, expose the disagreement, and write both on
update. Update `xmp:ModifyDate` and append a `pdf:Producer` history entry
identifying Rasura and its version — traceability is a feature for regulated
users.

### 10.4 Images

- `replace_image(image_block, bytes, format)` — swap an XObject's data. If the
  new image has different dimensions, either preserve the placement rectangle
  (default, stretch) or preserve the aspect ratio (opt-in, letterbox).
- `resample_image(image_block, dpi)` — decode, resample, re-encode. Requires an
  image codec; use `zune-jpeg` / `png` (both permissive). JPEG 2000 and JBIG2
  are read-only.
- `delete_image`, `move_image`, `resize_image` — content-stream level, no pixel
  work.
- `/SMask` and `/Mask` must move with the image.
- Inline images (`BI`/`ID`/`EI`) are editable in place.

### 10.5 Vector content

Expose paths as `VectorBlock` with their construction operators. Support
transform, delete, restyle (stroke colour/width, fill). Do not attempt boolean
path operations or node-level editing — that is a vector editor, not a PDF
editor.

### 10.6 Redaction — the one that must be correct

Redaction is not drawing a black rectangle. A correct implementation:

1. Removes the glyph-showing operators (or the glyph subranges) covering the
   redacted region from the content stream.
2. Removes image data intersecting the region — decode, blank the pixels,
   re-encode; or delete the XObject if fully covered.
3. Removes intersecting annotation content, form field values, and link targets.
4. Strips the text from `/StructTreeRoot` `/ActualText` and `/Alt`.
5. Purges the strings from `/Info` and XMP if they appear there.
6. Removes the glyphs from the embedded font subset if no longer used anywhere
   (otherwise a subset's glyph inventory leaks the alphabet used).
7. **Forces `SaveMode::FullRewrite`.** Incremental append leaves the original
   bytes in the file, which would make the redaction cosmetic. This is
   non-negotiable and must be enforced in code, not documentation.
8. Removes prior revisions from the `/Prev` chain entirely.
9. Optionally draws the redaction box as new content.

Add a `verify_redaction(doc, strings)` that re-parses the output and asserts
none of the redacted strings appear in any object, decoded stream, or metadata
field. Ship it as a public API — it is the assurance a legal customer needs.

### 10.7 Annotations

Full CRUD for: `/Text`, `/Link`, `/FreeText`, `/Line`, `/Square`, `/Circle`,
`/Polygon`, `/PolyLine`, `/Highlight`, `/Underline`, `/Squiggly`,
`/StrikeOut`, `/Stamp`, `/Ink`, `/Popup`, `/FileAttachment`, `/Widget`.

Appearance streams (`/AP` `/N`, `/R`, `/D`) must be generated for any annotation
Rasura creates or modifies — viewers that do not synthesise appearances
(most of them, for most types) will otherwise show nothing.

### 10.8 AcroForm

- Field tree: `/Fields`, `/Kids`, partial and fully-qualified names.
- Types: `/Btn` (push, check, radio), `/Tx`, `/Ch` (list, combo), `/Sig`.
- Set values (`/V`, `/DV`), regenerate `/AP` from `/DA` and `/MK`.
- Respect `/NeedAppearances`; if set, you may skip appearance generation, but
  generate anyway — many viewers ignore the flag.
- Rich text fields (`/RV`) — preserve, edit as plain text.
- Field flattening: convert widget appearances into page content and remove the
  fields. Common request; implement it.
- XFA: detect and refuse per §3.

### 10.9 Navigation structures

Any operation that changes page count or order must fix up: `/Outlines` and
their `/Dest`, named destinations (`/Dests`, `/Names`), `/Link` annotation
destinations, `/OpenAction`, article threads (`/Threads`), and page labels
(`/PageLabels`). A dangling destination is a silent corruption; add an
invariant check for it.

---

## 11. Public API

### 11.1 Design principles

1. **The API is the product.** It is a developer SDK; the surface is what gets
   evaluated in the first ten minutes.
2. **No PDF concepts leak by default.** A developer replacing text should never
   see the word "xref". A power-user escape hatch (`document.raw`) exposes the
   object layer for those who need it.
3. **Fidelity is a return value, not an exception.** Degradation is normal and
   must be handled, not thrown.
4. **Everything is `async` at the JS boundary** because everything crosses a
   Worker.

### 11.2 Core surface (TypeScript)

```ts
class Pdf {
  static open(src: ArrayBuffer | Uint8Array | Blob, opts?: OpenOptions): Promise<Document>;
  static create(opts?: CreateOptions): Promise<Document>;
}

interface OpenOptions {
  password?: string;
  recovery?: 'auto' | 'never';        // default 'auto'
  eager?: boolean;                     // parse all pages up front; default false
}

interface Document {
  readonly pageCount: number;
  readonly documentKind: 'born-digital' | 'scanned' | 'mixed';
  readonly taggedStatus: 'untagged' | 'tagged' | 'tagged-degraded';
  readonly permissions: Permissions;
  readonly leniencies: readonly Leniency[];
  readonly revisions: readonly RevisionInfo[];
  readonly hasXfa: boolean;
  readonly fonts: readonly FontInfo[];

  page(index: number): Promise<Page>;
  registerFont(bytes: ArrayBuffer, opts?: FontRegisterOptions): Promise<FontHandle>;
  edit(): EditSession;
  metadata(): Promise<Metadata>;
  save(opts?: SaveOptions): Promise<SaveResult>;
  close(): void;

  readonly raw: RawObjectAccess;      // escape hatch
}

interface Page {
  readonly index: number;
  readonly mediaBox: Rect;
  readonly rotate: number;
  blocks(): Promise<readonly Block[]>;
  paragraphs(): Promise<readonly Paragraph[]>;
  paragraphAt(point: Point): Promise<Paragraph | null>;
  textContent(): Promise<TextContent>;
  images(): Promise<readonly ImageBlock[]>;
  annotations(): Promise<readonly Annotation[]>;
}

interface Paragraph {
  readonly id: BlockId;
  readonly text: string;
  readonly textConfidence: 'exact' | 'partial' | 'none';
  readonly box: Rect;
  readonly runs: readonly StyleRun[];
  readonly alignment: Alignment;
  readonly leading: number;
  readonly lineCount: number;
}
```

### 11.3 Font registry

The browser cannot see system fonts. Make this explicit and pleasant:

```ts
const handle = await doc.registerFont(minionBytes, {
  matchFor: 'MinionPro-Regular',   // optional explicit binding
});

// What does this document actually need?
const needs = await doc.fontRequirements();
// [{ pdfFont: 'ABCDEF+MinionPro-Regular', embedded: true, subset: true,
//    coverage: 'partial', missingForFullLatin: 41, registered: false }]
```

`fontRequirements()` run immediately after `open()` lets a consuming application
fetch exactly the fonts it needs before the user starts typing. It turns the
worst constraint of the platform into a solvable, visible task. Lead with it in
the docs.

### 11.4 Editing and the fidelity contract

```ts
const session = doc.edit({ overflow: 'refuse', lineBreaking: 'greedy' });

const r = await session.replaceText(para.id, { start: 0, end: 10 }, 'Q4 net revenue');

r.fidelity;         // 'exact' | 'reembedded' | 'substituted' | 'overlaid'
r.missingGlyphs;    // ['Q']
r.substitutedFont;  // { from, to, score } | null
r.reflowedLines;    // 3
r.linesChanged;     // [4, 5, 6]
r.kerningSource;    // 'font' | 'producer' | 'none'
r.tagsUpdated;      // true
r.warnings;         // Warning[]

if (r.fidelity !== 'exact') await session.undo();

const out = await session.commit().then(() => doc.save());
```

```ts
type Fidelity =
  | 'exact'        // original embedded glyphs, original metrics, original mechanism
  | 'reembedded'   // glyphs injected into the embedded font from a registered source
  | 'substituted'  // a different typeface was used
  | 'overlaid';    // original content masked, new text drawn on top (last resort)
```

A strict caller sets `session.requireFidelity('exact')` and every operation that
cannot meet it fails instead of degrading. A contract-redlining tool sets
`'exact'`; a form-filler accepts `'substituted'`. That single knob is worth more
than any feature in this document.

### 11.5 Errors

```ts
class PdfError extends Error { code: PdfErrorCode; detail: unknown; }

type PdfErrorCode =
  | 'malformed' | 'encrypted-password-required' | 'encrypted-unsupported'
  | 'scanned-no-text' | 'xfa-unsupported' | 'type3-glyph-missing'
  | 'font-unavailable' | 'overflow' | 'stale-session'
  | 'fidelity-below-required' | 'signature-would-be-destroyed'
  | 'unsupported-filter' | 'internal';
```

Never throw a bare `Error`. Every failure is coded and actionable.

### 11.6 Rendering interop

Rasura is not a renderer. Two supported paths:

1. **pdf.js pairing** (recommended, documented with a working example): render
   with pdf.js, edit with Rasura, re-render the saved bytes.
2. **Draw-command emitter** (`rasura-render`, Phase 6):
   `page.drawCommands()` returns a flat, serialisable command list a caller can
   replay into Canvas2D or WebGL. Not a full renderer — no blend modes, no
   shading types 4–7 — but enough to build an editing overlay with correct
   glyph positions.

### 11.7 Rust API

The facade crate exposes the same model synchronously for native consumers
(CLI, server, tests). The WASM layer is a thin async adapter over it. Design the
Rust API first; do not let WASM ergonomics distort the core.

---

## 12. WASM and packaging

### 12.1 Threading

`rayon` in WASM requires `SharedArrayBuffer`, which requires COOP/COEP headers
on the *consumer's* site. That is a hostile install requirement for an npm
package.

- Ship **single-threaded by default**.
- Offer `rasura/threaded` as a separate entry point with documented header
  requirements.
- Feature-detect and fall back automatically in the default build.

### 12.2 Worker by default

The JS wrapper spawns a Worker and runs everything in it. The main thread never
blocks. Transfer `ArrayBuffer`s rather than copying. Provide
`Pdf.open(src, { worker: false })` for callers who manage their own.

### 12.3 Module splitting

| Chunk | Contents | Budget (gzipped) |
|---|---|---|
| `core` | cos, content, layout | ≤ 900 KB |
| `fonts` | font parsing, shaping, subsetting, script tables | ≤ 600 KB |
| `render` | draw-command emitter | ≤ 250 KB |
| `codecs` | image resampling | ≤ 300 KB |

`core` loads on `Pdf.open()`. `fonts` loads lazily on the first edit that needs
shaping — reading and extracting text does not pay for it. Enforce budgets in CI
with a size-limit gate that fails the build on regression.

Build flags: `-C opt-level=z`, `lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`, `wasm-opt -Oz --strip-debug`. Avoid `std::fmt` in hot paths;
it is a surprising fraction of binary size.

### 12.4 Distribution

- ESM primary, with a CJS shim.
- Full TypeScript declarations, hand-checked, no `any` in the public surface.
- `.wasm` shipped as an asset with a documented `wasmUrl` override for CSP-strict
  and CDN-hosted consumers.
- No postinstall scripts. No native build step. `npm i rasura` and it works.

### 12.5 Memory

Documents are held as the original `ArrayBuffer` plus a lazily-populated object
cache. Target peak ≤ 3× file size for a read-and-edit workflow. Expose
`document.memoryUsage()` and an LRU cap on the page-model cache for
thousand-page documents.

---

## 13. Performance budgets

Measured on a 2020-class laptop, Chrome, single-threaded WASM.

| Operation | Document | Budget |
|---|---|---|
| `open()` to first page metadata | 500 pages, 20 MB | ≤ 120 ms |
| `page(n).paragraphs()` cold | dense text page | ≤ 80 ms |
| `replaceText` single paragraph | — | ≤ 16 ms |
| `save()` incremental, one edit | 20 MB | ≤ 200 ms |
| Full text extraction | 500 pages | ≤ 4 s |
| Memory peak, read + one edit | 20 MB file | ≤ 60 MB |

Benchmarks live in CI with a regression gate. A 20% regression fails the build.

---

## 14. Testing and conformance

This section is not optional and is not a phase. It is built alongside Phase 1.

### 14.1 Corpus

Assemble and version a corpus with a manifest recording producer, PDF version,
encryption, tagging, font types, and known quirks:

- **Generated**: LaTeX (pdftex, xetex, lualatex), LibreOffice, Word, InDesign,
  Chrome print-to-PDF, Ghostscript, Quartz, wkhtmltopdf, Prince
- **Public**: govdocs1 sample, the pdf.js test corpus, PDF Association test
  files, Ghent Workgroup output suite, veraPDF test corpus
- **Adversarial**: malformed xrefs, wrong `/Length`, truncated files, cyclic
  page trees, deeply nested forms, 10k-page documents, mixed encodings

### 14.2 Invariants

These are assertions, run over the whole corpus on every commit:

- **I1 — Identity.** `open(bytes)` then `save()` with zero edits produces
  byte-identical output. Any file that fails I1 is a parser-fidelity bug.
- **I2 — Locality.** After editing page *N*, every other page renders
  pixel-identical (via the pdfium reference renderer) and every object not on
  page *N* is byte-identical.
- **I3 — Validity.** Output passes `qpdf --check` with no errors and veraPDF
  structural validation.
- **I4 — Round-trip stability.** Text extraction before and after a no-op edit
  cycle is identical.
- **I5 — Undo exactness.** Any operation followed by `undo()` restores the exact
  prior byte state.
- **I6 — Tag integrity.** For tagged documents, the structure tree after an edit
  contains the same element count and ordering, with MCIDs resolving.
- **I7 — Redaction completeness.** `verify_redaction` finds no trace of redacted
  strings anywhere in the output.

### 14.3 Pixel-diff harness

Render before and after with pdfium at 150 dpi. Compare with a perceptual
threshold that ignores anti-aliasing noise (per-pixel ΔE below a small bound)
but catches any glyph position shift above a quarter pixel. Store failures as
side-by-side artefacts in CI.

### 14.4 Fuzzing

`cargo-fuzz` targets for: the object lexer, the content-stream tokenizer, the
xref parser, each filter decoder, the CMap parser, and each font table parser.
Seed from the corpus. Run continuously; the parser must never panic, never
infinite-loop, and never allocate unboundedly on adversarial input. All parsing
is `#![forbid(unsafe_code)]`.

### 14.5 Property tests

Generate random valid edit sequences with `proptest` and assert I5 and I3 hold
for every prefix. Generate random documents and assert I1.

### 14.6 Cross-viewer verification

A nightly job opens output in Chrome, Firefox, Acrobat (via a container), and
Preview (via a macOS runner) and checks for repair prompts or render failures.
Manual quarterly review of a sampled diff set.

---

## 15. Repository layout

```
rasura/
  Cargo.toml                  workspace
  crates/
    rasura-cos/
    rasura-content/
    rasura-font/
    rasura-layout/
    rasura-edit/
    rasura-render/
    rasura/               facade
    rasura-wasm/
  js/
    src/                      TypeScript wrapper + worker
    test/
    package.json
  corpus/
    manifest.toml
    files/                    git-lfs
  harness/
    pixeldiff/                pdfium-based, test-only
    invariants/
    bench/
  fuzz/
  examples/
    01-extract-text/
    02-replace-paragraph/
    03-fill-form/
    04-redact/
    05-with-pdfjs/
  docs/
```

---

## 16. Documentation deliverables

Treated as product, not afterthought:

- **Ten-minute quickstart** that ends with a real edited PDF downloaded.
- **The fidelity guide** — explaining subsetting, why `'exact'` sometimes fails,
  and how to design around it. This document is a sales asset.
- **Font provisioning guide** — `fontRequirements()`, where to legally source
  metric-compatible fonts, how to lazy-load them.
- **Interop recipes** — pdf.js, React, Vue, Svelte, Next.js (SSR caveats).
- **Spec-coverage matrix** — which ISO 32000 features are supported, partial, or
  refused. Publishing your gaps builds more trust than hiding them.
- **Runnable examples** in `examples/`, each a standalone page.

---

## 17. Delivery phases

Each phase ends with the invariant suite green on the corpus.

**Phase 1 — Object layer.** `rasura-cos` complete: lexer, all xref forms,
object streams, all filters, encryption, recovery mode, incremental writer.
Exit: I1 green on the full corpus. This is the foundation; do not move on with
I1 failures.

**Phase 2 — Content layer.** Full operator coverage, graphics/text state
machine, span preservation, resource resolution, form XObject recursion.
Exit: text extraction with correct positions across the corpus.

**Phase 3 — Reconstruction.** Unicode derivation chain, word/line/block
assembly, paragraph and style inference. Exit: extraction quality benchmarked
against pdftotext and pdf.js; reading order correct on multi-column corpus.

**Phase 4 — Font engine.** Parsing all flavours, shaping, glyph injection,
substitution matching, sparse subsetting. Exit: injection round-trips validate
in all four viewers.

**Phase 5 — Editing.** Transaction model, text operations, reflow, stream
patching, fidelity reporting. Exit: I2 and I5 green. **This is the first
shippable release.**

**Phase 6 — Blocks, images, pages.** Block operations, image replace/resample,
page insert/delete/reorder with navigation fix-up, draw-command emitter.

**Phase 7 — Documents as documents.** Tables, tagged PDF maintenance,
annotations, AcroForm, flattening, redaction with verification. Exit: I6 and I7
green.

**Phase 8 — Long tail.** Vertical writing mode polish, optional content,
encryption creation and password change, `KnuthPlass` breaking, compaction
subsetting, threaded build.

---

## 18. Open questions to resolve with measurement

These change the architecture and should be answered before the phase that
depends on them, not after.

**Q1 — `/ToUnicode` coverage.** Across the corpus, what fraction of embedded
fonts have a usable `/ToUnicode` CMap? Subset LaTeX fonts frequently do not. If
coverage is below roughly 85%, step 6 of §7.2 (glyph-name heuristics) and
possibly a shape-matching fallback become Phase 3 work rather than Phase 8.
*Cost: a weekend. Value: potentially a quarter.* Do this first.

**Q2 — Break-point reproduction.** After a one-word edit, how often does greedy
re-breaking reproduce the original line breaks exactly? If it is above ~90%, the
greedy default is clearly right. If not, consider inferring the producer's
algorithm from the original break positions.

**Q3 — Justification mechanism distribution.** What proportion of justified
paragraphs use `Tw`, versus `Tz`, versus `TJ` arrays? Determines how much of
§9.3's mechanism-matching is actually needed.

**Q4 — Metric-compatible substitution quality.** For the top 20 non-embedded
fonts in the corpus, how far does a metric-compatible substitute shift line
endings? Calibrates the §8.5 weights.

**Q5 — Subsetter choice.** `write-fonts` versus `allsorts` for glyph injection
and CFF charstring manipulation. Build a spike against both; pick on correctness
across the corpus, then on binary size.

**Q6 — Bundle floor.** What is the smallest `core` chunk that can still parse
and extract? If it exceeds 900 KB gzipped, the layout engine may need to become
a third lazy chunk.

---

## 19. What success looks like

A developer runs `npm i rasura`, opens a 40-page LaTeX-generated contract in
their own React app, changes one word on page 12, saves, and diffs the output —
and finds that the file grew by 2 KB, that pages 1–11 and 13–40 are byte-identical,
that the edited line kept its original kerning, and that the library told them
before they committed that the edit was `'exact'`.

No browser library has ever done that.