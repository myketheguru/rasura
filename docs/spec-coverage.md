# Spec coverage matrix

Which parts of ISO 32000 and of `spec.md` are supported, partial, or refused.
Spec 16 puts it plainly: publishing your gaps builds more trust than hiding them.

Legend: **yes** implemented and tested · **partial** implemented with stated
limits · **no** not yet · **refused** deliberately out of scope

Last updated at the end of Phase 1.

---

## §5 — `rasura-cos`, the object layer

### 5.1 Object model

| Item | State | Notes |
|---|---|---|
| All ten object types | yes | |
| `PdfString` retains original bytes | yes | Literal-vs-hex origin preserved; escape choices survive |
| `Name` retains original bytes | yes | `#`-hex encoding reproduced exactly |
| Order-preserving `Dictionary` | yes | `IndexMap`; re-inserting a key keeps its position |
| Lazy, cached stream decoding | yes | Cached on the `Document`, keyed by object id |
| Unmodified stream never re-encoded | yes | The writer replays raw bytes |

`Object::Real` holds an `f64` and does not retain the producer's exact numeric
spelling. Byte identity for unmodified objects comes from the writer replaying
their source span verbatim, so this is not observable in output; it *would*
become observable if a future phase regenerated an object it had not edited.
`format_real` mirrors PDF conventions (decimal only, shortest round-trip) for
numbers that are genuinely new.

### 5.2 Lexer

| Item | State |
|---|---|
| Comments, including `%PDF-` and `%%EOF` | yes |
| All three EOL conventions | yes |
| Literal strings: nesting, all escapes, octal, line continuation | yes |
| Hex strings: whitespace, odd final digit padded | yes |
| Names with `#xx`, including `#20` and literal `#` | yes |
| Reals `.5 -.002 4. 34.5 +17` | yes |
| Exponent notation `6.02e23` | yes, recorded as a leniency |
| Integers exceeding `i64` | yes, clamped and recorded |
| `stream` followed by CRLF or LF, not bare CR | yes, bare CR accepted and recorded |
| `/Length` indirect, or wrong | yes, resolved or recovered by scanning |
| Leniency log | yes |

Every leniency kind in `LeniencyKind` is reachable and recorded; none are
silently swallowed.

### 5.3 Cross-reference resolution

| Item | State |
|---|---|
| Classic tables with `/Prev` chains | yes |
| Cross-reference streams (`/W`, `/Index`, `/Prev`) | yes |
| Object streams (`/ObjStm`) | yes |
| Hybrid-reference files (`/XRefStm`) | yes |
| 19-byte xref entries | yes |
| `/Prev` loop detection | yes |
| Revision chain retained and exposed | yes |
| Recovery: full-file scan for `N G obj` | yes |
| Recovery: highest generation wins | yes |
| Recovery: locate trailer, else scan for `/Type /Catalog` | yes |
| Recovery forces `SaveMode::FullRewrite` | yes, enforced in code |
| Recovery expands object streams | yes |
| Files with bytes before `%PDF-` | yes |

### 5.4 Filters

| Filter | Decode | Encode | Notes |
|---|---|---|---|
| `FlateDecode` | yes | yes | Predictors PNG 10–15 and TIFF 2; see the recovery rules below |
| `LZWDecode` | yes | refused | `/EarlyChange` 0 and 1. Encoding is refused by design — nobody should ship new LZW |
| `ASCIIHexDecode` | yes | yes | |
| `ASCII85Decode` | yes | yes | `z` shorthand, `<~` prefix, `~>` terminator |
| `RunLengthDecode` | yes | yes | |
| `DCTDecode` | pass-through | pass-through | |
| `JPXDecode` | pass-through | pass-through | |
| `JBIG2Decode` | pass-through | pass-through | `/JBIG2Globals` preserved as an ordinary reference |
| `CCITTFaxDecode` | pass-through | pass-through | |
| `Crypt` | yes | yes | Handled by the security handler at load time |

TIFF predictor 2 is implemented for 8- and 16-bit components. Sub-byte
components (1, 2, 4 bits) return a typed error rather than a wrong answer; no
file in the corpus uses one.

Compression level: `detect_flate_level` reads the zlib FLEVEL bits, but the
writer does not yet feed that back into re-encoding — a re-encoded stream uses
level 6. This only affects streams the caller actually changed.

**Flate recovery rules.** A stream with a valid zlib header may be truncated and
whatever inflated is kept — that damage is real and common. A stream *without* a
valid header must inflate cleanly to its end **and** consume essentially all of
its input before the result is believed. Both conditions are needed: arbitrary
bytes frequently begin a valid raw-deflate block (`0x2b` is BFINAL with fixed
Huffman) and report a clean finish after eating a handful of bytes, so
"reached the end marker" alone accepts noise. An earlier version did exactly
that, and a stream that failed to *decrypt* came back as plausible-looking
rubbish rather than an error.

### 5.5 Encryption

| Item | State |
|---|---|
| `/V` 1, 2, 4, 5 | yes |
| `/R` 2, 3, 4, 5, 6 | yes |
| RC4 40-bit and 128-bit | yes |
| AES-128 (`AESV2`) | yes |
| AES-256 (`AESV3`, `/R` 6 hardening loop) | yes |
| Crypt filters `/CF`, `/StmF`, `/StrF`, `Identity` | yes |
| `/EFF` (embedded-file filter) | no — embedded files are not yet a feature |
| Empty user password attempted automatically | yes |
| Owner password path | yes |
| `/EncryptMetadata false` | yes |
| Per-object key derivation | yes |
| Re-encrypting new content with the existing key | yes |
| **Creating protection** (AES-256 `/R` 6, AES-128 `/R` 4) | yes — Phase 8 |
| **Changing the password** | yes — Phase 8 |
| **Removing protection** | yes — Phase 8 |
| Creating RC4 or `/R` 5 protection | refused by name — see below |

### Creating protection is a different problem to reading it

Reading recovers a key someone else chose. Writing decides what a document's
protection *is*, and a mistake is not a file that fails to open — it is a file
that opens when it should not. Three things follow.

**Three refusals.** RC4 is read and never written: the read path already calls
it "a broken cipher we support only to *read* legacy files", and there is no
`Strength` variant for it rather than an option that could be forced. `/R` 5 —
Adobe's deprecated extension, a single SHA-256 with none of `/R` 6's hardening
loop — is likewise read and not written. And permissions stay advisory: `/P` is
written and signed into `/Perms`, and still not enforced on read.

**The randomness comes from the caller.** This crate has no RNG, and
`wasm32-unknown-unknown` provides none — a property §12 depends on, since the
object layer needing no filesystem, clock or randomness is what lets it run
unchanged in a Worker. Pulling in `getrandom` to generate a salt would spend
that on four bytes. So `Entropy` takes 32 bytes from the caller and expands them
with a counter-mode KDF, and the security of the result is bounded by their
quality — stated plainly rather than hidden behind an API that appears to
generate its own. `Entropy::new` rejects all-equal and counting input, which
catches the two mistakes that actually happen and cannot catch a bad RNG.

**A protection change forces a full rewrite**, checked in `effective_mode`
beside redaction's rule and for a related but distinct reason: an incremental
append leaves prior objects under the old key or none, and a reader has one file
key for the whole document. Adding protection incrementally does not make a
weakly protected file, it makes an unreadable one.

The writer consequences are larger than they look. The verbatim fast path — copy
an unchanged object's original bytes — is disabled, because after `unprotect`
those bytes are ciphertext while the file will claim to have none. And a stream
whose content did not change is *recrypted rather than re-encoded*: encryption
sits outside the filter chain, so peeling one layer off and putting the new one
back leaves the compressed bytes exactly as they were. Re-encoding would work
and would introduce filter drift for nothing.

### The empty owner password, which the tests caught

`/O` and `/U` are checked independently and a reader is in if either is
satisfied. So setting a user password and leaving the owner password blank —
the default — produced a document that prompts for a password and then opens
without one. It looked protected to whoever made it, which is the worst way for
this to be wrong.

An empty owner password now means "the same as the user password", which is what
Acrobat does and what a caller means, and the substitution is *reported* as
`Weakness::OwnerPasswordEqualsUser` rather than assumed to be understood. The
test asserts the negative directly: the file does not open with no password.

### Judged entirely from outside

Encryption is the one area where testing against your own inverse proves
nothing: a key derivation wrong in a self-consistent way encrypts and decrypts
perfectly and produces a file nobody else can open. So the gate is external and
runs in CI on both strengths:

- **pdf.js** derives the key from the password and reads the text back — and
  fails to open the same file without it, and with a wrong one.
- **pdfium** renders the protected file **pixel-identical** to the plain
  original, which is the sharp claim: encryption must change no pixels at all.
- **qpdf** `--check --password=` passes, a third implementation agreeing on the
  key.
- The unprotect round trip is checked the same way, because that is the
  direction where a mistake is invisible: a file whose `/Encrypt` was dropped
  while its streams stayed ciphertext still opens and renders nothing.

`/R` 5 and 6 *reading* is still tested against this crate's own implementation
of the same algorithms, which is weaker than the RC4 and AES primitives get
(those are pinned to published test vectors). Creation now supplies part of the
missing evidence in the other direction — pdf.js and pdfium agreeing on a `/R` 6
file we wrote is an independent check of the hash and key-wrapping steps — but a
`/R` 6 file from a real producer would still be worth having, and the corpus
manifest records it as a wanted gap.

Passwords are used as supplied rather than SASLprep-normalised. This is correct
for the ASCII passwords that exist in practice and wrong for a password
containing a non-normalised Unicode sequence. Three files in the pdf.js corpus
need exactly that (`saslprep-r6.pdf` among them) and are correctly refused
rather than opened with a wrong key.

An `/StmF` or `/StrF` naming a crypt filter that `/CF` does not define is a
producer bug. Rather than refuse the file, the cipher implied by `/V` is
assumed — AESV2 at `/V` 4, AESV3 at `/V` 5 — which is what viewers do. Guessing
wrong yields output the caller can see is wrong; refusing yields nothing.

### 5.6 Writer

| Item | State |
|---|---|
| `SaveMode::Incremental`, original bytes untouched | yes |
| Matches the original xref style, never upgrades | yes |
| Preserves `/ID[0]`, regenerates `/ID[1]` | yes |
| Preserves `/Root`, `/Info`, `/Encrypt` | yes |
| `Warning::LinearizationBroken` | yes |
| ObjStm members promoted to top level when modified | yes |
| `SaveMode::FullRewrite` | yes |
| Full rewrite drops unreferenced objects | yes |
| Re-linearisation | refused, per spec |

`/ID[1]` is derived from a hash of the revision's content rather than from a
random source: there is no RNG in this crate and none on
`wasm32-unknown-unknown` by default. The value is unique per distinct output,
which is what the identifier is for, and it makes saves reproducible.

---

## §6 — `rasura-content`, the content layer

### 6.1 Operator coverage

All 70 keyworded operators, plus inline images and a preserved `Unknown`.
Round-trip tested in both directions with a size assertion, so an operator added
without a keyword fails loudly rather than silently becoming `Unknown`. `F` is
kept distinct from `f` so a round trip reproduces whichever the producer wrote.

Unknown operators inside `BX`/`EX` are skipped silently; outside they are
recorded as a leniency.

Inline images resolve the `EI` ambiguity in three tiers: explicit `/L`, then
exact arithmetic from `/W`·`/H`·`/BPC`·`/CS` for unfiltered payloads, then a
whitespace-delimited scan with a "does the stream resume as text?" check.
Indexed and named colour spaces fall back to one component, because resolving
them needs `/Resources` the tokenizer does not have; if the arithmetic is wrong
the `EI` check rejects it and the scan takes over.

### 6.2 Span preservation

| Item | State |
|---|---|
| Every `Op` carries its byte range in the decoded buffer | yes |
| Span covers operands *and* operator | yes |
| Spans ordered, non-overlapping, within bounds | yes, asserted over the corpus |
| Every span attributable to a source object | yes, asserted over the corpus |

Verified at scale: 1,994,923 operators across 1,484 pages of 992 documents, with
zero span or attribution defects.

### 6.3 Graphics and text state

| Item | State |
|---|---|
| `q`/`Q` stack, bounded | yes |
| Text state saved/restored by `q`/`Q` | yes |
| `Tm`/`Tlm` *not* part of the graphics state; reset by `BT` | yes |
| `Td` moves from the line matrix, not the advanced text matrix | yes |
| `TD` sets leading to `-ty`; `T*` is `0 -TL Td`; `'` is `T*` then `Tj`; `"` sets `Tw`/`Tc` then `'` | yes |
| Displacement `tx = ((w0 − Tj/1000)·Tfs + Tc + Tw)·(Tz/100)` | yes |
| `Tw` applies only to single-byte code 32 | yes, isolated and tested |
| Vertical mode `ty`, with `Tz` correctly *not* applied | yes |
| Text rendering matrix folding size, scale and rise | yes |
| Colour: device spaces resolved, others carried unresolved | yes |

Colour spaces that need `/Resources` to interpret — ICCBased, Indexed,
Separation, DeviceN — are carried as `Colour::Unresolved` with their components
rather than guessed by component count. A one-component Separation is not a grey.

### 6.4 Resource resolution and recursion

| Item | State |
|---|---|
| Page attribute inheritance (`/Resources`, `/MediaBox`, `/CropBox`, `/Rotate`) | yes |
| `/Contents` array concatenated with a whitespace separator | yes |
| Logical offset → (stream index, offset) mapping | yes |
| Spans crossing a stream boundary reported per part | yes |
| Form XObject recursion with `/Matrix` composed | yes |
| Form resources with fallback to the invoker's | yes |
| Cycle guard: depth limit **and** visited set | yes |
| Tiling patterns and Type 3 procedures via the same machinery | `walk_stream` exists; not yet driven |

The cycle guard needs both halves. A depth limit alone permits an exponential
blowup where two forms invoke each other and each invocation doubles the work.

Page-tree traversal is defensive: nodes are classified by whether they have
`/Kids` rather than by `/Type`, because files exist whose interior nodes are
typed `/Page` and whose leaves carry no `/Type` at all. A tree that yields no
pages falls back to scanning for `/Type /Page` objects.

### 6.5 CMaps and font metrics

Positioning needs three things from a font, and all three come from the font
*dictionary* — no font program, so Phase 4 is not a prerequisite.

| Item | State |
|---|---|
| Codespace ranges, mixed byte lengths | yes |
| `cidrange` / `cidchar` → CID | yes |
| `bfchar` / `bfrange`, arrays, surrogate pairs | yes |
| `Identity-H` / `Identity-V` | yes, exact |
| Adobe collection CMaps (`UniJIS-UCS2-H` …) | **approximated** as two-byte identity, and flagged |
| `usecmap` | detected, not followed |
| Simple `/Widths` with `/FirstChar`, `/MissingWidth` | yes |
| CID `/W` (both forms) and `/DW` | yes |
| Type 3 widths through `/FontMatrix` | yes |
| Standard-14 metrics with no `/Widths` | **no** — reported as `missing_widths`, needs Phase 4 AFM data |

An approximated CMap positions correctly for the two-byte collections but does
not produce the right CIDs, so `approximate_cmap` is set rather than letting the
caller assume the mapping is exact.

### 6.6 Text extraction

Glyph runs carrying, per glyph: code, CID, advance, device-space origin, the
byte span within the showing operator, and whether the width was measured or
substituted. Runs carry the operator span, source object, `/MCID`, render mode
and form depth.

Unicode comes from `/ToUnicode` **only** — §7.2 strategy 1 of seven. `None`
means "this layer does not know", not "there is no text", and `TextReport`
counts unmapped glyphs so a caller can tell an empty page from an unreadable
one. The other six strategies are Phase 3.

### Phase 2 exit: measured against pdf.js

`harness/textdiff` extracts with pdf.js and with Rasura across the corpus
and compares. On 1088 pages:

| | |
|---|---:|
| Pages where both found text | 327 |
| — near-exact (≥0.98 character agreement) | 279 (85.3%) |
| — close (≥0.90) | 7 (2.1%) |
| — diverged | 41 (12.5%) |

Of the 41 divergences: 31 are pages with unmapped glyphs (the `/ToUnicode` gap,
Phase 3), 7 are ordering differences including Arabic in visual rather than
logical order (Phase 3 reading order and bidi), and 3 are pages where pdf.js
found *less* than Rasura.

**Zero divergences were pages where every glyph mapped and text still went
missing.** That is the gate: Phase 2 owes no lost text for any reason other than
the mapping chain it does not yet have.

### Not yet done in Phase 2

- Predefined Adobe CMaps beyond Identity, which need the collection data files.
  Positioning is approximated; CIDs are not correct for those fonts.
- Standard-14 AFM metrics (Phase 4). A font with no `/Widths` gets a half-em
  fallback and is flagged.
- Type 3 glyph procedures and tiling patterns are reachable through
  `walk_stream` but nothing drives them yet.

## §7 — `rasura-layout`, reconstruction

**Phase 3 complete.** §7.2–7.8 are done.

### 7.2 Unicode derivation

| Strategy | State |
|---|---|
| 1. `/ToUnicode` CMap, `bfchar`/`bfrange`, arrays, surrogate pairs | yes |
| 2. `/Encoding` `/Differences` → glyph name → Adobe Glyph List | yes, complete AGL (4,281 entries) |
| 2/3. Base encodings: Standard, WinAnsi, MacRoman | yes |
| 3. Built-in encoding for the standard 14 | yes, for non-symbolic fonts |
| 3. Symbol and ZapfDingbats built-in encodings | yes |
| 4. Composite `/Encoding` CMap → CID → registry-ordering Unicode | partial: Identity handled, Adobe collections need their data files |
| 5. Reverse lookup through the embedded font's `cmap` | yes, for fonts with no base encoding |
| 6. Glyph-name heuristics: `uniXXXX`, `uXXXXX`, `name.alt`, `f_i` ligatures | yes |
| 7. PUA sentinel and degraded confidence | yes |

`MacExpertEncoding` is deliberately **not** approximated with StandardEncoding:
it is a different glyph repertoire, and substituting would produce confidently
wrong text rather than an honest gap. A symbolic font with no `/Encoding` still
gets no base-encoding *guess* for the same reason — but it now gets step 5,
which reads the font's own `cmap` instead of guessing.

**Every strategy in §7.2 is now implemented** except step 6's shape matching,
which Q1's evidence does not justify: the glyph-name heuristics account for
0.03% of glyphs across the corpus.

Failures get a Private Use Area sentinel rather than being dropped. Dropping
would silently shorten the string and misalign every offset after it;
`text_lossy()` is available for callers who would rather have short text.

### Measured against pdf.js

Re-running the Phase 2 differential with the chain in place, over 1088 pages:

| | before (§7.2 strategy 1 only) | after |
|---|---:|---:|
| Glyphs with no mapping | 278,018 (75%) | **37,376 (10.1%)** |
| Pages where both found text | 327 | **622** |
| — near-exact agreement | 279 (85.3%) | **562 (90.4%)** |
| Mean similarity | 0.919 | **0.965** |
| Pages short purely for want of `/ToUnicode` | 405 | **80** |

Which strategy resolved each of the 369k glyphs:

| Strategy | Glyphs | Share |
|---|---:|---:|
| `/Differences` + AGL | 173,325 | **47.0%** |
| `/ToUnicode` | 86,931 | 23.6% |
| Base encoding | 52,751 | 14.3% |
| Failed | 37,376 | 10.1% |
| Built-in encoding | 18,504 | 5.0% |
| Glyph-name heuristic | 108 | **0.03%** |

This is Q1's prediction confirmed on the full corpus. The Adobe Glyph List
resolves nearly half of all glyphs — more than `/ToUnicode` does — while the
glyph-name heuristics the spec expected to be decisive account for one glyph in
three thousand. The 10.1% that still fail are dominated by symbolic fonts and
CID fonts needing the collection data, both of which need Phase 4.

### 7.3 Word segmentation

| Item | State |
|---|---|
| Explicit space glyphs (U+0020, U+00A0, U+2000–U+200A and friends) | yes |
| `TJ` negative adjustment beyond a threshold | yes, via the positional-gap rule |
| Positional gap beyond the expected advance (`0.25 × Tfs`) | yes |
| Non-monotonic pen movement | yes |
| No segmentation for CJK, Thai, Khmer, Lao, Myanmar | yes |
| Threshold calibrated against the font's own space advance | approximated as `0.25 × Tfs`, which is about one space |

Conditions 2 and 3 are the same measurement here. A `TJ` adjustment moves the
pen and the pen position is what this sees, so a gap test on device coordinates
catches both — and catches producers that move the pen with `Td` instead, which
an adjustment-only test would miss.

Each `Word` records *why* it began (`LineStart`, `SpaceGlyph`, `PositionalGap`,
`NonMonotonic`), which §7.6 will need for hyphenation detection and which makes
a badly-segmenting document diagnosable.

An unmapped glyph is never treated as a space: `None` means the chain could not
read it, not that it is blank, and treating it as whitespace would invent a word
boundary mid-word.

### 7.4 Line assembly

| Item | State |
|---|---|
| Cluster by baseline in **device space**, after the CTM | yes |
| Tolerance `0.3 × Tfs` | yes, scaled by the larger of the two glyph sizes |
| Rotated and skewed text forms lines | yes |
| Super/subscripts join the parent line | yes, by explicit `Ts` or by a >20% size drop within `0.6 × Tfs` |
| Interleaved runs sorted visually, operator order retained | yes — `PlacedGlyph` carries `run`/`index` |
| "One `Tj` per character" merged | yes, falls out of device-space clustering |
| Vertical writing mode lines | yes — Phase 8; see below |

Each glyph is reduced to two device-space scalars: `tangent` (position along its
baseline, which orders glyphs within a line) and `normal` (perpendicular offset,
which is constant along a baseline and is what lines cluster on). Baseline
*direction* buckets first — text at different angles cannot share a line however
close the glyphs are, and comparing normals across directions is meaningless.

Text-to-device advance ratios are computed from the rendering matrix, **not**
measured from consecutive glyph origins. Measuring is the obvious approach and is
wrong: a `TJ` adjustment inflates the distance between two glyphs, so a measured
ratio absorbs the adjustment and the gap that word segmentation exists to detect
then vanishes. This required carrying `Tz` on `GlyphRun`, since the rendering
matrix folds it in with the font size and both must come back out.

#### Vertical writing was a one-line bug behind a two-scalar design

The row above said "untested; the geometry is direction-agnostic". The geometry
was; the *input* to it was not. `/WMode 1` is a property of the font's CMap, not
of the text matrix, so a vertical run has an unrotated matrix and glyphs that
share an x while their y descends. Bucketing by `trm.rotation()` therefore put
them all at direction zero, where `normal` is y — and every character became its
own line. A CJK page came out as one line per glyph.

Rotating the basis a quarter turn for a vertical run fixes both halves at once,
and the second half is the part worth noticing:

- `tangent` becomes distance down the column, which is reading order within it.
- `normal` becomes `−x`, which is constant down a column and **ascends
  leftward** — so the existing "sort lines by ascending normal" already orders
  columns right to left, which is how vertical Japanese and Chinese are read.

No ordering code changed. That is what the two-scalar reduction bought, and it
would have been invisible without a case that used it.

`Tz` also had to stop applying: §9.4.4 excludes the horizontal scale from
vertical displacement, so the parameters divided back out of the rendering
matrix are `(size, scale_y)` rather than `(size × Tz, scale_x)`. Using the
horizontal pair scaled every advance by the one number the specification says to
ignore.

Three tests pin it — one column not three lines, columns right to left, glyphs
downward within a column — and all three were checked by removing the quarter
turn and watching them fail.

### 7.5 Block and column detection

| Item | State |
|---|---|
| Recursive XY-cut on glyph boxes | yes |
| Vertical threshold `1.5 × median line height` | yes |
| Horizontal threshold `0.8 × median char width` | yes |
| Reading order from the cut-tree traversal | yes |
| Ruling lines collected before cutting | yes |
| Horizontal rules as cut hints | yes |
| Docstrum fallback for undividable high-variance regions | yes, simplified to line-level nearest-neighbour |
| Rectangle grids as a table signal | collected; §7.7 will consume them |

**The cut operates on glyph boxes, not line boxes**, which the spec says
explicitly and which turns out to be load-bearing. Line assembly correctly joins
every glyph on a shared baseline, so on a two-column page the *lines span the
gutter* — left and right column at the same height are one line. Project those
and there is no valley to cut at, making columns undetectable in principle.
Cutting glyphs first and assembling lines inside each leaf is the order that
works.

A vertical cut wins a near-tie against a horizontal one, because separating
columns is structurally more significant than separating paragraphs. That
preference is safe precisely because a full-width heading *cannot* be cut
vertically — it spans the gutter, so the profile has no valley there — and the
horizontal cut therefore happens first, as it should.

Ruling lines are collected as segments, never as polylines: a curve's endpoints
say nothing about the path between them, and joining consecutive points turns a
Bézier arc into a horizontal rule. Both stroked segments and thin filled
rectangles count, because `re` + `f` is how Word, InDesign and most HTML-to-PDF
converters draw borders — a collector watching only strokes would miss most real
tables.

### 7.6 Paragraph and style reconstruction

| Item | State |
|---|---|
| Leading discontinuity, `> 1.3 ×` the block's modal gap | yes |
| First-line indent, where the previous line ended short | yes |
| Style discontinuity at a line boundary | yes, on each line's *dominant* style |
| `/MCID` boundary, preferred over heuristics | yes |
| Alignment from left/right edge variance | yes |
| Leading, first-line indent, left and right margin | yes |
| Style runs over font, size, colour, `Tr`, `Ts` | yes |
| Hyphenation detected and recorded | yes |

Every paragraph records **why it began** (`SplitReason`), and
`Paragraph::is_authoritative()` distinguishes a producer-declared `/MCID`
boundary from an inferred one. The four signals are applied in decreasing order
of trustworthiness, so no heuristic can override explicit tagging — which is
what spec 7.6 means by "authoritative".

The indent rule needs *both* conditions. An indent after a line that ran to the
right edge is a hanging indent inside one paragraph, not a new one; testing the
indent alone breaks every bibliography and numbered list.

Style changes are compared on each line's **dominant** style rather than its
first glyph. One italic word does not start a paragraph; a heading set in
another face does. The italic word still becomes its own *style run*, which is
the level at which spec 7.6 wants it recorded.

**Alignment excepts the first line as well as the last.** Spec 7.6 says to
except the last line, because a justified paragraph's last line is short by
design. It does not mention the first — but a first-line indent is exactly what
`first_line_indent` exists to record, so counting that same indent as left-edge
variance contradicts the rest of the section. It was not hypothetical: every
body paragraph of `freeculture.pdf`, a justified book with a 14 pt indent, came
out as **right-aligned**. Excluding the first line from the left-edge test (and
only where two lines remain to measure) fixed it; right-aligned paragraphs
across the corpus fell from 233 to 60 and justified rose from 418 to 591.

Alignment is `Unknown` rather than guessed when the evidence is thin. Spec §2
requires fidelity to be reported: re-justifying a paragraph that was never
justified is a visible change to the page.

### 7.7 Tables, headers, footers, footnotes

| Item | State |
|---|---|
| Tables from ruling-line grids | yes |
| Tables from ≥3 lines sharing ≥2 aligned column edges | yes, with two guards the spec does not state |
| `Table { rows, cols, cells }`, empty cells included | yes |
| Column edges kept for the explicit column-width operation | yes |
| Cell-level reflow | Phase 5 |
| Headers/footers in the top/bottom 12%, repeating across ≥3 pages | yes |
| One numeric field allowed to vary | yes |
| `RunningElement` with `isPageNumber` | yes |
| Footnotes: page bottom, short rule, smaller modal size | yes, plus a required marker |
| Link to in-text markers by superscript numeral | yes, matched on glyphs |

**Table detection is page-level, and cannot be otherwise.** §7.5's XY-cut treats
a table's column gutters as exactly the vertical valleys it exists to cut on, so
by the time blocks exist a 3×3 table is already nine of them. Asking "is this
block a table?" can only ever answer no. Detection re-flattens blocks to glyphs
— sound because §7.5 guarantees blocks partition them — and works from the page.
The first implementation here was block-level and found zero tables.

Cells are filled by assigning **glyphs**, not lines, for the same reason §7.5
had to: a table row is one line spanning every column, so assigning whole lines
puts each row entirely in its first cell.

**Spec 7.7's stated alignment rule is necessary but not sufficient**, and the
corpus shows both failure modes:

- *Two-column prose satisfies it exactly.* Every line has a word starting at the
  right column's left edge, preceded by a gutter, on every line of the page. The
  guard is that a table's cells are **short** — a prose column holds a full
  wrapped line — so mean words per cell is capped.
- *Bounding density only from above admits the opposite failure.* A large sparse
  grid trivially satisfies "few words per cell". `issue12810.pdf` produced an
  **89×80 grid with seven filled cells out of 7,120** — a chart's gridlines,
  which connect into one cluster and pass every structural test. Hence a minimum
  fill as well, looser for drawn grids than inferred ones, because the producer
  having drawn the lines is real evidence.

Footnotes required a third guard beyond spec 7.7's three. Position, size and
separation together still admitted 120 candidates, of which most were captions,
folios and datestamps — `Test-plusminus.pdf` is an engineering drawing whose
title block sets DRAWN / CHECKED / SCALE in small type between short rules, and
every cell of it qualified. What none of them has is a **marker**, which spec
7.7's own linking clause presupposes. Requiring one, plus a minimum length,
brought 120 candidates down to 4.

**Marker linking runs over glyphs, not words.** A superscript marker almost
always abuts the word it annotates with no space, so §7.4 segments `text` + `¹`
as the single word `text1`, which never equals `1`. Matching through words
scored **zero** links on the entire corpus.

Two defects in running-element templates, both found on `freeculture.pdf`:

- The prefix scan absorbed a digit when the field grew: "Page 9" and "Page 10"
  share the prefix "Page 1", so one running header split into two — `Page {}`
  for pages 1–9 and `Page 1{}` for 10–15. Neither boundary may fall inside a run
  of digits.
- `isPageNumber` read the first digits in the string rather than the field the
  template identified. The running head is a QuarkXPress slug —
  `14773_01_1-174_r9jm.qxd 2/10/04 3:51 PM Page 1` — whose leading digits are an
  unchanging job number, so nothing appeared to increment.

A field that varies but does not increment is deliberately **not** page
numbering: a running head quoting a section number varies too, and propagating
an edit across it would be wrong.

### 7.8 Document model

| Item | State |
|---|---|
| `DocumentModel { pages, structure, reading_order }` | yes |
| `PageModel { blocks, media_box, crop_box, rotate }` | yes |
| `Block::{Paragraph, Table, Image, Vector, Running, Unknown}` | yes |
| `StructTree` from `/StructTreeRoot` | yes, with `/RoleMap`, `/MCR`, `/OBJR`, `/Alt`, `/ActualText`, `/Lang` |
| `Block::Unknown` preserved verbatim, never reflowed | yes, with a typed `DeclineReason` |
| Reading order from the structure tree when tagged | yes, recorded in `OrderSource` |
| Cell-level reflow, column-width operations | Phase 5 |

§7.5's `Block` was renamed **`Region`**. The spec's `Block` is a classified
enum and is the type callers see; two types of that name in one crate is a trap.
A region is what the XY-cut produces; a block is what the model decides it is.

Classification runs strongest evidence first. A table claims a region because a
drawn grid or aligned columns is positive evidence; a running element claims one
because it repeats across pages, which no single page can fake; a paragraph
claims what is left; anything surviving all three is `Unknown`.

**Claiming is by membership, never by geometry.** Tables originally claimed
regions whose bounding box they overlapped while filling cells by glyph
membership — two different sets, so any glyph inside a table's box that no cell
took was claimed and then never stored. That dropped **2,216 glyphs** across the
corpus. A glyph's `(run, index)` is unique on a page, so the two sets can be
made to agree exactly, and a region only partly consumed by a table now keeps
its remainder rather than vanishing.

Tables are emitted at the position of the first region they consumed, not
hoisted to the front. §7.5's cut-tree traversal *is* the geometric reading
order, and reordering around it would both discard that and make the structure
tree's order incomparable with it — which would have destroyed the only external
check either one has.

`Block::Unknown` fires on two conditions, each recorded: `Unmapped`, where fewer
than half the glyphs resolved to Unicode, and `NonHorizontal`, where the
baseline is rotated so indent, alignment and margin would all be measured along
the wrong axis. Spec 7.8: "Guessing is worse than declining."

### Reading order finally has an oracle

Phase 3's exit criterion asks for reading order to be *correct*, and until §7.8
there was nothing to check it against: pdf.js's `getTextContent` is
content-ordered, so agreeing with it says nothing about ordering, and two
geometric heuristics agreeing says nothing at all.

A tagged document is different — the producer wrote the reading order down. The
corpus holds **87 tagged documents**, and against them the XY-cut's order scores
**89.8% of ordered pairs concordant (18,716 of 20,839), with 38 of 52 comparable
pages exactly right**.

That is a real number rather than an assumption, and it is not a good one: a
quarter of comparable pages have at least one block out of place. It is the
honest baseline for the ordering work, and it is now measured on every CI run.

### Measured over the corpus

The §7.8 model over the same 1088 pages: **4,380 paragraphs, 3,095 images, 847
unknown, 635 vector regions, 52 tables and 33 running elements**, with **zero
glyphs lost** between regions and blocks and **zero reading-order defects** —
every block listed exactly once, asserted per document and gated.

847 unknown blocks is 9.5% of the total, and that is the design working: those
are regions §7.2 could not map or whose baseline is rotated, preserved opaque
instead of being read as confident nonsense.

Running §7.2–7.7 across 1088 pages of the pdf.js corpus: **4,576 blocks, 12,800
lines, 6,839 paragraphs, 55,505 words, 20,112 style runs and 13,003 ruling
lines**, with **zero glyphs lost** by the cut, **zero lines lost** by the
paragraph split, and **zero glyphs lost** by cell assignment — all three
partitions asserted per page and gated in CI. Agreement against pdf.js is
unchanged at 565 near-exact, mean similarity 0.966.

§7.7 finds **52 tables** (28 ruled, 24 inferred, 3,682 cells; largest is
`prefilled_f1040.pdf`, a 46×30 tax form at 0.54 fill), **4 footnotes** of which
2 link to an in-text marker, and **9 running elements** across 6 documents.

### Non-text content is now measured, and asserted

Images and vector art reached the model from Phase 3 and **nothing checked that
they arrived**. One dropped from every page in the corpus would have moved a
number in an unasserted block histogram and failed nothing. `model::build`
pushes images and vectors one for one, so any discrepancy is a defect rather
than a judgement call — it is now a fourth partition assertion that fails the
run.

The first measurement:

| | Count |
|---|---|
| Images | **3,095** on 234 pages |
| — inline (`BI`/`ID`/`EI`) | 2,188 (71%) |
| — stencil masks (`/ImageMask`) | 2,214 |
| — **rotated or skewed** | **1,129 (36%)** |
| — missing `/Width` or `/Height` | 1 |
| Vector blocks | 635 on 290 pages, 120,802 painted paths |

The 36% decided `move_block`'s design, and it is exactly the kind of fact a
modelled-but-unmeasured feature hides.

A related repair. `a_clipped_path_is_not_artwork` passed because `W n` ends with
`n` — the same reason the neighbouring `a_discarded_path_is_not_artwork` passes.
It would have kept passing if clipping were removed from the tokenizer
entirely. It is renamed to what it actually checks, and joined by a test
asserting the real gap: **content drawn under a clip reports its unclipped
extent**, because `StateMachine` carries no clip at all. When clipping is
modelled that test should fail, and the fix is to intersect the box rather than
delete the test.

Those last two numbers are small, and the corpus is the reason rather than the
code. pdf.js's suite is bug-report files — mostly one to three pages, rarely
academic prose — so it contains almost no real footnotes, and the harness reads
only three pages per file against a three-page repeat threshold, which means a
running head must appear on all three of the *first* pages when page 1 is
usually a title page. Verified separately on a whole book: `freeculture.pdf`
over 30 pages yields exactly two running elements, the QuarkXPress slug (correctly
flagged as page numbering) and a constant "4TH PASS PAGES" footer. **§7.7 is the
least corpus-validated section of Phase 3**, and a prose corpus is what would
change that.

Of the 1,340 multi-line paragraphs, 44.1% are justified, 31.6% left, 8.3%
centred, 4.5% right, and 11.6% decline to say. The remaining 5,499 paragraphs
are single-line, where there is no alignment to infer — reported separately,
because folding them into "unknown" would hide how often the inference actually
fails. 525 paragraph boundaries came from `/MCID` and are authoritative rather
than guessed.

## §8 — `rasura-font`

**Phase 4 feature-complete.** Every section of §8 is implemented: parsing all
six font types, standard-14 metrics, shaping with the reshape boundary rule,
injection into TrueType and both CFF flavours with all PDF-level updates,
substitution matching, subsetting, and Type 3.

The **exit criterion is partly met**: "injection round-trips validate in all four
viewers", and two of the four now look at every build — pdf.js reads the text
back and pdfium renders both pages. Preview needs a `macos-latest` runner;
Acrobat has no headless mode. Both engines run against a synthesised font *and*
against Roboto, so the claim is not resting on a font this library wrote itself.
See [Injection, judged by something other than
us](#injection-judged-by-something-other-than-us).

### 8.2 Parsing

| Font type | Container | State |
|---|---|---|
| TrueType | `/FontFile2` | table directory, `head`, `maxp`, `hhea`, `loca`, `glyf`, `hmtx` |
| OpenType | `/FontFile3` `/OpenType` | same, either outline flavour |
| CFF / Type 1C | `/FontFile3` `/Type1C` | INDEXes, top dict, charstrings, charset, private dict, subrs |
| CID CFF | `/FontFile3` `/CIDFontType0C` | plus FDArray and FDSelect |
| Type 1 | `/FontFile` | PFB/PFA, eexec, charstrings, subrs, built-in `/Encoding` |
| `cmap` | sfnt | formats 0, 4, 6, 12; forward and reversed |
| `post` | sfnt | version 2.0 glyph names, with the 258 Macintosh standard names |
| Type 3 | `/CharProcs` | not started |
| Standard 14 metrics | — | yes, all 14 |

Font programs are parsed to the level **§8.4's injection** needs, not the level
a rasteriser needs. Charstrings are located as byte ranges rather than
interpreted, because "extract and re-encode the Type 2 charstring, resolving
local and global subroutines" is a copy with subroutine calls inlined — that
needs the bytes, not an outline. Likewise the sfnt is modelled as a table
directory over borrowed ranges: injection is byte surgery on tables that must
otherwise survive untouched, and §2 forbids rewriting what an edit did not need
to touch.

**The flavour is sniffed from the bytes, not taken from `/Subtype`.** 13
programs in the corpus (0.9%) embed something other than what they declare, and
a parser that trusted the declaration would lose every one. Where the two
disagree the bytes win.

A first version of that enum had separate `TrueType` and `OpenTypeGlyf`
variants, and reported **50.1% of the corpus as mislabelled** — because
`/FontFile2` and `/FontFile3 /OpenType` deliver byte-identical sfnt-with-`glyf`
programs, so the variants described the packaging rather than the font. The
enum now names the container and outline format, and `/OpenType` declares no
flavour at all, because spec 8.2 says it may hold "either" — an honest
non-statement that should not be scored as a claim.

`loca` short-format overflow is detectable via `loca_needs_long_format`. Spec
8.4 names it as "a common silent corruption": the short format halves offsets
into 16 bits, so a font crossing 128 KB of `glyf` mid-injection silently points
every later glyph at the wrong bytes.

### 8.2 Standard-14 metrics

All 14 faces, with per-glyph widths, ascent, descent, cap height and x-height.
The four Courier faces are fixed-pitch and answer for glyphs absent from the
table, which is the right answer in a monospaced font and would otherwise
return "unknown" for anything unusual.

Name resolution is deliberately generous — `Arial,Bold`, `TimesNewRomanPS-BoldMT`,
`CourierNewPS-ItalicMT` and subset prefixes all resolve — because ISO 32000-1
§9.6.2.2 tells a consumer to use "a reasonable substitute" and a refusal here
shows up as a page laid out at zero width. A name with no recognisable family
resolves to **nothing**; `resolve_or_default` is a separate call that falls back
to Helvetica and *tells the caller it did*.

The source is mozilla/pdf.js's `metrics.js`, Apache-2.0, already vendored by
`corpus/fetch.sh` and already the differential oracle — so a disagreement would
be measured against these numbers anyway. Its licence is vendored at
`crates/rasura-font/PDFJS-LICENSE`.

### Type 1: parsing is not the same as parsing correctly

A Type 1 font is decrypted twice — eexec (R=55665) over the private half, then
R=4330 over each charstring, each discarding leading random bytes governed by
`/lenIV`. Get any of that slightly wrong and **parsing still succeeds**: the
bytes come out shifted by a few positions and look entirely plausible.

So the corpus number is paired with a correctness check. Every Type 1 charstring
must set its side bearing before drawing anything, so the first operator has to
be `hsbw` or `sbw`. `Type1::soundness()` reports the fraction that do, and that
single check found three separate things a 100% parse rate had hidden:

1. **Computer Modern scored 0.000.** Its charstrings open
   `78 113889 100 div hsbw` — TeX computes fractional side bearings with the
   `div` escape, so the operand list is not simply a run of numbers. The check
   was wrong, not the parser.
2. **MinionPro-Bold scored 0.743.** Its `hyphen` is the entire charstring
   `2012 callsubr endchar`: Adobe's tools subroutinize so aggressively that the
   `hsbw` lives inside the subroutine. The check now follows `callsubr`, with a
   depth limit, and indexes subroutines directly — the bias is a Type 2
   invention and applying it here looks up the wrong subroutine every time.
3. **A latent scanning bug.** Resuming after each charstring was computed from
   the length alone, omitting the `RD` token and its space, so the next glyph
   search began a few bytes *inside* the previous charstring's binary. A `/`
   byte there would parse as a glyph name. It never fired on this corpus — the
   numbers did not move — but it is fixed and pinned by a test.

One tolerance was added from evidence: a PFB whose segment chain does not reach
the end marker now stops rather than failing, because one font in the corpus
pads after its last segment and renders fine everywhere else.

### Measured over the corpus

**1,388 of 1,388 embedded font programs parse — 100%** — reaching 1,854,545
glyphs:

| Flavour | Found | Parsed |
|---|---|---|
| sfnt/glyf | 686 | 686 |
| Type 1 | 345 | 345 |
| CFF/CID | 187 | 187 |
| CFF | 157 | 157 |
| sfnt/CFF | 13 | 13 |

Type 1 charstring soundness: **mean 0.994, 343 of 345 fonts at 1.000.** The two
exceptions are both in `issue7769.pdf`, from a converter that emits `/lenIV 0`
and a four-byte `.notdef` decoding to four operands and no operator — invalid
under any reading of `lenIV`.

Built-in encodings, which spec 8.2 requires and §7.2 has no other source for:
**344 of 345** — 232 explicit arrays, 112 `StandardEncoding`.

All three parsers are fuzzed against pseudo-random input in their own test
suites. A CFF is a series of offsets into itself, and a Type 1 is a stream that
decrypts to *something* whatever you feed it; both are easy ways to walk a
parser off a cliff.

### The metrics reach layout, through a hook

Spec 8.2's reason for shipping AFM metrics is "so that layout is correct even
without the outlines". Making that true meant crossing a layer boundary the
wrong way: advances are computed in `rasura-content`, which sits *below*
`rasura-font`, so content cannot call the metrics.

The content layer therefore states what it needs and a higher layer supplies it.
`rasura_content::font::WidthSource` is a one-method trait taking the font
*dictionary* — not just a name, because turning a character code into a width
needs the encoding, and that is more than a name conveys.
`rasura_layout::Standard14Widths` implements it, composing the font layer's
metrics with this layer's encoding tables. Neither ingredient alone is enough,
which is why the supplier lives in layout rather than in font.

Two rules the hook obeys:

- **It fires only where the file supplies nothing.** A font with its own
  `/Widths` keeps them even where they disagree with the standard metrics — the
  file is the authority on its own layout, and overriding it would move text
  that renders correctly today.
- **An unrecognisable face gets nothing.** `metrics::resolve` returns `None`
  rather than defaulting, because inventing Helvetica's metrics for an unknown
  display face lays it out confidently wrong. `LoadedFont::supplied_widths`
  records when metrics were substituted, since that is a fidelity claim.

**Measured: 31,395 glyphs across the corpus had no width from the file; with the
hook, 157 do.** That is 8.5% of every glyph extracted, previously advancing by
nothing.

### Symbol and ZapfDingbats, closed

The other half was encodings. Both faces are symbolic and name no `/Encoding`,
so §7.2 resolved them to nothing — correctly refusing to read them as
StandardEncoding, which would have turned a page of mathematics into Latin
letters. But refusing is not reading.

Their built-in encodings are now generated from pdf.js's `encodings.js`
(Apache-2.0), alongside code-to-glyph-name tables for Standard, WinAnsi and
MacRoman that the metrics need — the pre-existing tables map code to
*character*, which is the other half of the problem. The dingbat names `a1`
through `a191` come from Adobe's separate `zapfdingbats.txt` and are kept out of
the main AGL on purpose: merging them would let an ordinary font's `a1` resolve
to a dingbat, which is a wrong character rather than a missing one.

`Symbol` code 0x61 now reads as α rather than as nothing, and both faces have
metrics. Unmapped glyphs across the corpus fell from 37,376 to 37,317.

An explicit `/Encoding` still outranks the built-in one: a font may name
`/Symbol` and override with WinAnsi, and the file's own statement wins.

### §7.2 step 5 is implemented: the font's own `cmap`, reversed

The last missing strategy. A `cmap` says which glyph a character produces; §7.2
needs the opposite, so the font's Unicode subtable is inverted and the glyph a
code draws is looked up in it. Two chained lookups: code to glyph the way ISO
32000-1 §9.6.6.4 says a simple font's goes — the (3,0) symbol table with the
0xF000 offset, then (1,0) Macintosh, then Unicode — and glyph to character by
the reversed table.

**It runs only where the PDF offers no base encoding at all.** Everywhere else
the producer has said something, and a reversed cmap is inference: where a font
maps several characters to one glyph, running it backwards can only guess. The
gate is the absence of a base table rather than a threshold on how full the map
happens to be — which also keeps the cost honest, since step 5 parses the font
program and doing that for every font to fill a handful of gaps is a lot of work
for a rounding error.

A private-use answer is discarded. It means the font's "Unicode" table is itself
a symbol mapping, and reporting it would look like a successful mapping while
yielding characters nobody can read.

**Measured: 1,256 glyphs recovered, unmapped down from 37,317 to 36,061 (10.1%
→ 9.8%).** Comparable pages rose from 623 to 632 — nine pages that produced *no
text at all* now produce some.

That last point explains a number that looks like a regression: mean similarity
fell from 0.966 to 0.962. It is a denominator effect. The nine newly-comparable
pages average 0.69, so total agreement rose while the mean fell. Near-exact
pages went up, 565 to 570.

### 8.3 Shaping

| Item | State |
|---|---|
| `rustybuzz` | yes, MIT |
| Script, language and direction derived from the run | yes |
| Reshape boundary rule | yes |
| `kerningSource: font \| producer \| none` | yes |
| Complex scripts (Arabic, Indic, Hebrew, Thai, CJK vertical) | passed to `rustybuzz` with the right script and direction |
| Feature inference from `GSUB` coverage | yes — kerning and ligatures |

**The reshape boundary rule is the substance of §8.3, and it is not an
optimisation.** A PDF stores post-shaping glyph ids: the producer ran some
shaper, at some version, with some features, and recorded the result. Nothing in
the file says which. Reshaping a whole line would rewrite glyphs the user never
touched using a shaper that may legitimately disagree about ligatures, kerning
or mark placement — which is exactly what §2 forbids. So a reshape covers the
minimal span containing the edit, widened to word boundaries, never beyond the
line; everything else is copied byte for byte.

It lives in its own module with no font data in it, because it is the part that
decides what gets rewritten and it should be testable without a font. The
off-by-one that rule exists to prevent has a test of its own: a word boundary
falling exactly at the edit's end must not truncate the span at the edit's last
glyph.

**Kerning provenance decides opposite treatments**, which is why it is inferred
rather than assumed. `TJ` adjustments matching the font's own kern values mean
the producer used font kerning, and a reshape should regenerate it. Adjustments
the font does not explain are the producer's own tracking and must be carried
through untouched, or the text visibly respaces. A clear majority is required,
not merely some matches — a producer applying uniform tracking will coincide
with a font's kern value on the odd pair by chance.

Script detection takes the **dominant** script, not the first: an Arabic
sentence containing a Latin product name is Arabic, and shaping it as Latin
drops every joining form. Characters common to all scripts carry no vote, so a
run beginning with a space is not classified by the space. Vertical writing mode
comes from the PDF, not from the text — the same CJK characters are set both
ways and only the file knows which.

Getting any of this wrong is quiet rather than loud: hand Arabic to the shaper
as left-to-right Latin and it produces glyphs, in the wrong order, with no
joining forms, and nothing errors. That is why detection carries the tests and
the `rustybuzz` call barely does — though shaping *is* verified end to end
against a minimal TrueType built in the test, checking glyph ids, advances,
right-to-left reversal and `.notdef` for absent characters.

### 8.4 Glyph injection

| Item | State |
|---|---|
| TrueType: append to `glyf`, rebuild `loca` | yes |
| Widen `loca` to long format past the 32k limit | yes |
| Extend `hmtx`, bump `hhea.numberOfHMetrics` | yes |
| Update `maxp.numGlyphs` | yes |
| Composite components pulled transitively | yes, and renumbered |
| Never strip existing hinting tables | yes — untouched tables are copied verbatim |
| Simple fonts: `/Widths`, `/FirstChar`, `/LastChar`, `/Differences` | yes |
| `/FontBBox` widened; `/StemV`, `/Flags`, `/ItalicAngle` left alone | yes |
| Extend `/ToUnicode`, **always** | yes |
| CFF: charstring extraction with subroutine inlining | yes |
| CFF: append to CharStrings, extend the charset | yes, name-keyed |
| CFF: CID-keyed — place in the right FD, extend FDSelect | yes |
| Composite fonts: `/W`, `/CIDToGIDMap` | yes |

**Original glyph ids never move.** Spec 8.6's sparse-preserving default: new
glyphs are appended and nothing is renumbered, because renumbering would mean
rewriting every content stream that references the font — the non-local change
§2 forbids.

**Untouched tables are copied byte for byte.** Only `glyf`, `loca`, `hmtx`,
`hhea`, `maxp` and `head` change; `cmap`, `name`, `OS/2` and the hinting
programs come through unchanged. This is the payoff for modelling the sfnt as a
table directory over byte ranges rather than as parsed structures — a model that
re-serialised everything would rewrite every table on every save.

**Composite components are renumbered as they are copied.** Those glyph indices
live *inside* the glyph data. Injecting `Á` and copying it unchanged produces a
glyph referencing whatever happens to sit at ids 1 and 2 in the *target* font.
The component walker has to read every flag form correctly too — argument sizes
and the three transform flags all change the record length, and one wrong length
desynchronises the walk into nonsense ids.

`hmtx` is rebuilt with a full metric for every glyph rather than preserving the
format's compressed tail. The table stores full metrics for the first
`numberOfHMetrics` glyphs and bare side bearings after, so appending full
entries behind a compressed tail would be invalid; expanding is correct whatever
the target was doing, at the cost of two bytes per tail glyph.

**A `unitsPerEm` mismatch is refused, not silently drawn wrong.** A 2048-unit
outline in a 1000-unit font renders at twice the intended size, and correcting
it means re-encoding every coordinate — a different operation from copying one.

**`/ToUnicode` is always extended.** This is the strongest claim in §8.4 and the
easiest to skip, because nothing renders differently when it is missing. A font
injected into without it produces a document whose new text cannot be searched,
copied or extracted — this library would have made the file worse at the thing
it exists to do. Existing mappings are extended rather than replaced, and the
CMap is emitted as `bfchar` throughout: the saving from `bfrange` is a few
hundred bytes and a range with a miscomputed endpoint maps a whole span of codes
wrongly and silently.

The end-to-end test is that **`rustybuzz` loads the rebuilt font** and still
shapes through its original `cmap`. A wrong table offset, length or checksum
shows up there and nowhere else.

### 8.4, the CFF path

Spec 8.4 says to inline subroutines rather than merge subr indexes, and the
reason is the same one as everywhere else: two fonts' indexes are independent,
so merging renumbers every `callsubr` in the target. Inlining costs a few bytes
per glyph and touches nothing.

**`hintmask` is why this needs a real walker.** Almost every Type 2 operator has
a fixed length, so a naive scan gets most of a charstring right. `hintmask` and
`cntrmask` are followed by one bit per stem hint declared so far, rounded up to
bytes — which means having counted the arguments to every `hstem`, `vstem`,
`hstemhm` and `vstemhm`, *and* the implicit `vstem` that `hintmask` performs
when arguments are pending. Miscount by one and mask bytes are read as
operators; a `callsubr` a few bytes later then inlines the wrong subroutine and
the glyph draws something plausible and wrong.

A real bug of that exact class turned up in the first version: byte `28` is a
16-bit *operand* but sits inside the operator range, and the catch-all
`0..=31` arm shadowed it. Every token after a 16-bit number was off by two
bytes.

Glyph names travel as SIDs. Standard SIDs — below 391 — mean the same string in
every CFF and are copied unchanged; anything above is private to the source, so
its string is copied into the target's String INDEX under a fresh SID. That
avoids needing the 391-entry standard strings table to move a glyph.

The Top DICT's offsets are written in the fixed five-byte form whether or not
the values need it. A CFF's offsets live in a DICT whose encoded length depends
on how large they are, so writing it changes what it should contain; fixing the
width makes the length known in advance and one pass sufficient.

**CID-keyed targets get a new FD carrying the source's own private dict**, not
whichever existing FD looks closest. A private dict holds the hinting
parameters — blue zones, standard stem widths — the outline was drawn against;
putting a Times glyph under a Helvetica FD hints it against the wrong stems and
it renders subtly wrong at small sizes, with nothing to show for it in any
structural check. Two glyphs from the same source FD share one new FD rather
than duplicating it.

The new FD needs no `Subrs`: step 1 has already inlined them, which is the other
reason inlining beats merging. FDSelect is rewritten as format 0 — a byte per
glyph — because appending to format 3's ranges means recomputing every range for
a few kilobytes on a font that already carries hundreds. New glyphs get CIDs past
the highest already in use, so they cannot collide with one the document draws.

### 8.4, composite fonts at the PDF level

`/W` gains one `cid [w]` group per glyph rather than being merged into the
existing runs: a `cfirst clast w` run cannot absorb a new CID without changing
what it says about the ones already in it. `/DW` is untouched, as spec 8.4 says.

`/CIDToGIDMap` has three cases and the middle one is the trap. `/Identity` stays
`/Identity` **only while it is still true** — if an added CID differs from its
GID, the entry becomes a stream, because leaving `/Identity` in place would point
the CID at the wrong glyph silently, with a glyph still drawn and nothing to
error on. An existing stream is extended rather than replaced.

A composite font's `/ToUnicode` uses **two-byte codes**. A one-byte codespace in
a Type 0 font parses cleanly and maps the wrong things, which is why the emitter
takes the width rather than assuming it.

### 8.6 Subsetting on save

| Item | State |
|---|---|
| Sparse-preserving default | yes — it is what injection already does |
| `SubsetPolicy::Compact`, opt-in | yes, for TrueType |
| Old-to-new glyph mapping returned | yes |

The default needed no code: appending *is* sparse-preserving. `SubsetPolicy`
derives `Default` as `SparsePreserving`, so a caller who does not choose gets the
policy that cannot break a document — spec 8.6's "offer it; never default to it"
made structural rather than documentary.

Compaction returns the **old-to-new mapping**, and that is not a courtesy. Every
`Tj` and `TJ` in every content stream using the font refers to glyphs by numbers
this pass just changed; a caller who renumbers the font and forgets the streams
has produced a document that renders as gibberish. Handing the mapping back makes
that step impossible to overlook.

`cmap`, `post`, `GSUB`, `GPOS` and `kern` are **dropped rather than renumbered**.
All index glyph ids this pass invalidated, and a PDF reaches glyphs through
`/Encoding` and `/Widths`, never through the font's own character map. Leaving
them stale would be worse than dropping them: a reader that did consult one would
get the wrong glyph rather than none.

#### The renumbering does not reach the content streams after all

Spec 8.6 says compaction "would require rewriting every content stream that
references the font — exactly the non-local change §2 forbids", and the
paragraph above repeats it. Applied at the document level (`edit::compact`), it
turns out to be avoidable, and the reason is that the glyph ids are not where
that sentence looks for them.

A composite font's content stream contains **CIDs**, not glyph ids;
`/CIDToGIDMap` is what turns one into the other. So the renumbering can be
absorbed by rewriting that one map:

```text
before:  code → CID → (CIDToGIDMap) → old GID
after:   code → CID → (CIDToGIDMap) → new GID
```

Not one byte of any content stream changes, and the example asserts that against
the bytes rather than claiming it.

When `/CIDToGIDMap` is `/Identity`, CID *is* GID and there is no indirection to
absorb anything — the spec's warning holds in full. The answer is to **add the
indirection**: a map stream is written where the name was, and the streams stay
untouched again. One new object against every page that uses the font is not a
close decision.

Simple fonts and CFF programs decline by name. A simple font's code-to-glyph
path runs through `/Encoding`, glyph names, and the font's own `cmap` — the
table compaction deliberately drops — so compacting one means rebuilding that
`cmap`, and getting it wrong yields a page of blanks rather than a smaller file.
`/FontFile3` needs charset and FDSelect renumbering, which is other work in
another format. Both are listed in the report so a caller who saves 4% learns
why it was not 40%.

**Measured on a real font.** The whole of Roboto — 515,100 bytes, 3,387 glyphs —
embedded in a document drawing twelve of them, pruned to 13 glyphs and 12,856
bytes, the document from 517 KB to 15 KB. pdfium renders the result
**pixel-identical** and pdf.js reads the text back, which is the check that
matters: renumbering that loses track of which glyph is which produces a file
that opens, validates, extracts the right text through `/ToUnicode`, and draws
the wrong letters.

### 8.7 Type 3 fonts

| Item | State |
|---|---|
| Reading glyph procedures | yes — they are ordinary content streams |
| `/FontMatrix`, whose scale is arbitrary | yes |
| Report which codes have no procedure | yes |
| `Type3GlyphMissing` naming the codes | yes |
| Synthesising a procedure | refused, per spec 8.7 |

A Type 3 glyph is an arbitrary program: it can stroke paths, place images, set
colour. Nothing about "the letter é" says what that program should contain, and a
synthesised outline would not match the face in weight, width or style. Every
other format has an answer to "give me a glyph for this character"; Type 3 does
not.

`missing()` returns the **list** of codes, ordered and deduplicated, not a
boolean. A caller told only that an edit is impossible can do nothing; one told
which characters are missing can drop them, spell them differently, or change
fonts. Ordering matters because the message a user sees should not depend on the
order the text happened to be scanned.

A `/Differences` entry naming a glyph the font never defines does **not** read as
available — that is a real shape of broken font, and `can_draw` requires both the
name and the procedure.

### Measured over the corpus

The charstring walker and inliner run over every CFF in the corpus:
**59,498 charstrings across 344 fonts — 100% walked to exactly their length,
100% inlined, 100% with no `callsubr` remaining.** 16% genuinely call
subroutines, so the inliner is exercised on 9,547 of them; 16 charstrings
failed to inline, all referencing subroutines their font does not contain.

A first version of that measurement reported 31.2%, which was the *metric* being
wrong rather than the walker: a subset CFF keeps a zero-length CharStrings entry
for every glyph it dropped, and 131,046 of those were being counted as
charstrings the walker failed on.

### Injection, exercised on real fonts

Fixtures said 100%. Every embedded program in the corpus has its own last glyph
injected back into it — real tables, a real outline, and an answer known in
advance — and the first run said **61.2%**.

Two of the three causes were the check, one was the code:

- **Composite glyphs.** Self-injecting one legitimately renumbers its
  components, so the bytes differ by design. The check now maps them back
  through the inverse mapping, which also verifies the renumbering.
- **A real bug in `loca` handling.** `loca[first_new]` is *one slot* serving as
  both the end of the last original glyph and the start of the first new one.
  Writing a padded append position into it extended the last original glyph's
  data range — silently changing a glyph the edit never touched, on 270 of 681
  fonts. Appending exactly where the last glyph ends fixes it, and new glyphs
  are padded to an even length so short `loca` can still address them, with the
  padding falling inside their own range where a reader ignores it.

The 18 that did not round-trip were then diagnosed, and two of the three causes
were real bugs:

- **Padding an odd-length glyph in a long-`loca` font.** Only the *short* format
  needs even offsets, because it stores them halved. Padding a long-format font
  grew every odd glyph by a byte — and that byte is inside the glyph's declared
  range, so the outline coming out was not the one that went in. Five fonts.
- **Zero-filling to a `loca` that overruns `glyf`.** Growing `glyf` to match a
  table that claims more than the file holds *repairs* the font, turning glyphs
  a reader currently sees as truncated into zero-filled ones. That is a change
  to something the edit never asked about. The offsets are now clamped instead,
  which leaves every glyph's readable bytes exactly as they were.
- **Fonts whose `loca` contradicts itself** — fewer entries than `maxp` claims,
  or offsets that go backwards. No rebuild can reproduce a table that does not
  describe a layout. These are reported through
  `Injection::target_loca_inconsistent` and counted apart from defects that are
  ours.

The last three were then diagnosed too, and the diagnosis had to start with the
harness. Six of its failure paths returned a bare outcome with no reason
attached, and the survey printed reasons without naming the file they came from
— so "2 CID CFF failures" was a number with no thread to pull. Every path now
records why, and the report names the file. Both changes took minutes; the
failures had been undiagnosed for a phase for want of them.

**A CJK subset can be full.** `NotoSerifCJKjp` embedded in `issue9278.pdf` holds
**65,535 charstrings** — exactly the CFF ceiling, because a CID INDEX count is a
`Card16`. Appending a 65,536th wrapped that count to **zero**, producing a font
that declares no glyphs at all: it parses, it passes a structural check, and it
renders nothing. The refusal now happens before any work, as a typed
`FontError::Full` naming what filled up. There is a second such ceiling behind
it — CIDs are `u16`, so a font can have a free charstring slot and no CID to put
in it — and that one is reported the same way. Neither is a defect in this code
or in the file; §2's second property says fidelity is *reported*, and "this font
is full" is exactly the kind of report it means.

**A TrueType Collection is not a font.** `issue9262_reduced.pdf` embeds a 10 MB
`ttcf` as `/FontFile2`. Two things were wrong, and the smaller one hid the
larger. The rebuild copied the first four bytes of the input as its version tag,
so the output announced a collection and then supplied a plain table directory —
bytes no reader can open, which is how it surfaced. Underneath, `Sfnt::parse`
was **reslicing** the buffer to the sub-font's directory, while a collection's
table offsets are absolute from byte zero of the file. Every table read during
parsing therefore landed sixteen bytes late, and the bytes there still decoded:
MSMincho, a 19,398-glyph JIS face, reported **510 glyphs**, and nothing
complained, because 510 is a perfectly plausible number. The directory is now
*located* rather than sliced to, and `rebuild` takes its version tag from the
sub-font. Both are pinned by tests that wrap a fixture in a `ttcf` header and
require it to read identically to the bare font.

**Final: 1,019 of 1,019 attempts round-trip intact — 100.0%.** CFF 157/157,
CFF/CID 185/185, sfnt/glyf 676/676, with 5 fonts set aside as self-contradictory
input and 2 as full. Nothing is left undiagnosed.

The two categories are excluded from the rate rather than counted as failures,
and that is a claim worth stating plainly: a font whose own `loca` contradicts
itself cannot be reproduced by anyone, and a font at its format's ceiling has no
slot to append to. Averaging them in would make the number measure the corpus
instead of the library. They are printed on their own lines, with the files
named, so the exclusion is visible rather than assumed.

### Injection, judged by something other than us

Everything above is this library checking its own output — rasura's parsers
re-reading rasura's writers, with `rustybuzz` as the one outside opinion on
the font in isolation. Phase 4's exit criterion asks for more than that:
"injection round-trips validate in all four viewers".

The seed corpus now contains **`injected-truetype.pdf`**: a real document
drawing `ABC` through a font whose program has had a glyph injected, with
`/Widths`, `/FirstChar`/`/LastChar`, `/Differences` and `/ToUnicode` all updated
to match. The `C` did not exist in the font before injection. The font is
synthesised rather than vendored — shipping a real typeface means shipping its
licence, and the point is to exercise the machinery.

Two external checks run against it in CI:

- **`qpdf --check`**, which the seed corpus already goes through.
- **pdf.js**, which opens the document and builds the page's *operator list*.
  That is the part that matters: building it forces pdf.js to parse the embedded
  program and translate every glyph. Text extraction alone would pass straight
  from `/ToUnicode` without the font program being read at all, so passing that
  and nothing else would prove little.

pdf.js reads back `"ABC"` with no warnings, errors or silent repairs.

### Getting the other three viewers in

Spec §14.6 names them: a nightly job opening output in **Chrome, Firefox,
Acrobat (via a container), and Preview (via a macOS runner)**, checking for
repair prompts or render failures — plus §14.3's **pdfium pixel diff at 150 dpi**
catching glyph shifts above a quarter pixel.

| Viewer | Engine | Status |
|---|---|---|
| Firefox | pdf.js | **in CI** — opens, builds the operator list, reads the text |
| Chrome | pdfium | **in CI** — renders both pages; see §14.3 below |
| — | both, on Roboto | **in CI** — the same pair built from a real typeface |
| Preview | CoreGraphics | needs a `macos-latest` runner |
| Acrobat | — | no headless automation; spec's own "manual quarterly review" |

Two of the four now look at every build. Preview is ordinary work — a
`macos-latest` runner rendering through CoreGraphics. Acrobat has no scriptable
headless mode, which is presumably why the spec pairs its nightly job with a
manual quarterly review rather than pretending otherwise.

### 14.3 The pixel diff, and the bug only it could find

`harness/pixeldiff` renders before and after with **pdfium at 150 dpi** and asks
the question no structural test can: not "did anything change" — an injected
glyph is *meant* to appear — but **"did anything change where the old content
was"**. That is §2's first property expressed in pixels.

pdfium is downloaded, not vendored, pinned to a release (`chromium/7988`) rather
than `latest`: a new pdfium can legitimately change anti-aliasing, and the diff
would report that as a regression here. It is MIT around Google's BSD-3-Clause,
`cargo deny check licenses` passes with it, and §4.2 permits it explicitly as a
**test-only** renderer — so it lives in a harness, never in `crates/`, and the
library has no knowledge of it.

**It found a real bug on its first run.** The injected glyph drew *nothing*.
`qpdf --check` passed, pdf.js opened the file and extracted `"ABC"` — because
extraction reads `/ToUnicode`, which was correct — and every one of this
library's own checks was green. But ISO 32000-1 §9.6.6.4 resolves a simple
TrueType font's code to a glyph *through the font's own tables*: the
`/Differences` name becomes a character through the Adobe Glyph List, and the
character becomes a glyph id through the `cmap`. Injection had never extended
the `cmap`, so the name reached nothing.

Spec 8.4's PDF-level list is complete for **Type 1**, where a name addresses a
charstring directly. For TrueType it is not, and nothing short of rendering
says so. `cmap_write::add_mappings` now rebuilds the `cmap` as a format 4 table
carrying the font's existing mappings plus the new ones.

Two calibrations the harness needs, both documented in it:

- **A noise threshold**, because anti-aliasing is not bit-reproducible. Set well
  below the ~64/255 that a quarter-pixel edge shift produces, so §14.3's
  requirement still holds.
- **A two-pixel boundary margin.** New content set flush against old shares an
  edge, and a rasteriser's filter reaches about a pixel either side of it.
  Without the margin the harness cries non-locality on every ordinary
  injection. It does not weaken the test: text that has genuinely *moved*
  changes columns throughout the region it occupies, not one column at its edge.

The harness also checks its own baseline for ink and exits `2` if the render is
blank, because a rasteriser producing blank pages reports "0 changed pixels" for
every input it will ever be given — a green tick that can never go red. That
check was written after pdf.js under `node-canvas` did exactly that.

**Result on the injected fixture: 383 pixels changed, none inside the region the
original content occupied.**

### I2's pixel half, which needed Phase 5 to become checkable

> **I2 — Locality.** After editing page *N*, every other page renders
> pixel-identical and every object not on page *N* is byte-identical.

The object half has been green since Phase 1. The pixel half could not be
written, because until this phase there was no edit to make. It now runs on
every build: a two-page document, a word replaced on page one, and **page two
rendered pixel-identical**.

Two pages is the whole point. The failure that catches cannot be expressed by a
single-page fixture — an edit whose effect leaks through something the pages
share. A font dictionary rewritten in place, a `/Resources` entry renumbered, an
object stream repacked: each leaves the edited page correct and changes a page
nobody touched. Only rendering the other page says so.

Page one needed a **different question**. The harness's original model assumes
new content is appended to the right of the old, which is true of an injected
glyph and false of a replaced word: the change is legitimately inside the
original ink, and text after it moves because that is what changing a word's
width does. The claim that still holds is that nothing *upstream* of the edit
moved, and `--unchanged-before` checks it.

The boundary column is computed by the fixture from the layout layer's own
glyph coordinates rather than hard-coded, so the two cannot drift apart. It came
out at column 586, and pdfium's first changed column was **586** — the layout
layer and the renderer agreeing to the pixel, which is a stronger result than
either check was designed to produce.

#### Four questions, not one

"Did the page change" is never the right question, and which one *is* depends on
what the edit was meant to do. Asking the wrong one produces a failure that is
really the harness being confused — and a green tick that means nothing is worse
than a red one that means something.

| Mode | Asks | Fits |
|---|---|---|
| *(default)* | did anything change **left of** the original ink? | content appended — an injected glyph |
| `--identical` | did **anything at all** change? | a page the edit did not touch |
| `--unchanged-before N` | did anything change **before** column N? | an edit inside a line of text |
| `--changed-within X0 X1` | did anything change **outside** those columns? | content that moved |

The last three take their bound from the caller, because only the caller knows
where the edit was: a renderer can see that pixels differ, not which of them
were supposed to.

`--changed-within` exists for the move case, where neither of the others fits —
an image dragged across the middle of a page changes columns on both sides of
where it started. The fixture computes the band as the union of the image's old
and new bounds; the prediction was columns 145–852 and pdfium reported changes
in 146–851, one column of slack on each side. Deliberately narrowing the band to
300–500 fails with 68,628 stray pixels up to 349 columns out, which is the check
proving it can go red.

### 14.3b A typeface this library did not write

The fixture above has one weakness that no amount of validation removes: the
font is synthesised by the same crate that reads it. It therefore agrees with
every assumption this code makes about a well-formed sfnt, including any
assumption that is wrong. It proves the writer and the parser are consistent
with each other, not that either matches what the rest of the world produces.

So the same pipeline now runs against **Roboto** — 3,387 glyphs, 2,048 units per
em, hinting programs, a `post` table, `GSUB`, several `cmap` subtables.
Apache-2.0, already on §4.3's allowlist, and **downloaded rather than vendored**
(`corpus/fetch-font.sh`, pinned to a commit) so the repository carries no
third-party binary and the licence stays with its author.

The sequence is the real one rather than an approximation of it:

1. **Subset it the way a producer would** — `compact_truetype` over the seven
   glyphs of `Hamburg`, 3,387 down to 8.
2. **Type a character the subset threw away.** `É` is deliberate: in Roboto, as
   in every Latin face worth the name, it is a **composite** — a reference to
   `E` plus a reference to the acute, each with its own transform. Injecting it
   is only correct if the components come with it *and* their glyph ids are
   renumbered inside the composite's own body. Nothing in the synthesised
   fixture exercises that path at all. The run reports **2 components pulled**,
   and the test fails if that ever reads zero, because a run that silently tests
   less than it claims is the failure worth catching.
3. **Render both, and read both back.** pdfium: 487 pixels changed, columns
   360–383, with the original ink ending at 353 — the new glyph is entirely
   clear of the text that was already there. pdf.js: `"HamburgÉ"`, no warnings.

**It found a bug in the fixture on its first run**, which is the second time
rendering has caught something structural checks could not. The first attempt
reported 215 pixels changed *inside* the original text. The cause was `/Widths`:
it is indexed by character code from `/FirstChar`, and `Hamburg` occupies codes
72, 97, 98, 103, 109, 114 and 117 — seven letters spread across forty-six codes.
Writing seven widths starting at `/FirstChar 72` gave `a` the width of code 73,
and every letter after the first piled up. The tell was in the harness output
before the images were opened: the baseline's ink ended at column 225 where a
seven-letter word at 24 pt should reach 353.

A second test pins the arithmetic that would otherwise go unnoticed: Roboto is
2,048 units per em, so an advance copied straight out of `hmtx` into `/Widths`
is roughly twice the glyph's real width. The text would still draw, still
extract, and still pass every structural check — with growing gaps between the
letters. The synthesised fixture is 1,000 units per em and cannot catch it.

Both tests **decline rather than fail** when the font has not been fetched, so a
fresh clone runs green. CI fetches it, so the courtesy is not a hole in the gate.

### 8.5 Substitution matching

| Item | State |
|---|---|
| The §8.5 scoring formula, all six terms | yes |
| `avgWidthDelta` dominates | yes, by weight |
| Score returned so the caller can reject | yes |
| Never substitute without saying so | yes — no default-fallback entry point |

The weights encode spec 8.5's own claim that metrics matter most: a font that
looks right and measures wrong loses to one that measures right, because the
second reflows the page and the first moves every line after the substitution.
There is a test for exactly that — Liberation-for-Arial beats a
visually-similar sans with its own metrics.

The score is returned **broken out per term**, not reduced to a number: a
caller deciding whether to accept a substitution wants to know *what* is wrong,
and a score of 30 from a slant mismatch is a different problem from a score of
30 from metric drift. `metrics_compared()` says whether the dominant term could
be measured at all, so a total of zero from no shared glyphs is not mistaken for
a perfect match. `runner_up` says how close the decision was — two candidates a
point apart means the choice was arbitrary whatever the winner scored.

A metric neither side states contributes nothing rather than a penalty:
penalising absence would rank a font that declines to describe itself below one
that describes itself badly.

`best_match` returns `None` for an empty registry. There is deliberately no
fallback to a built-in default — spec 8.1 promotes "the developer must supply
fonts" into the public API, and quietly substituting something the caller never
registered is the silent behaviour this layer exists to avoid.

### The bundle budget survives shaping

`rustybuzz` was the largest outstanding risk to spec 12.3's 900 KB `core`
budget. Measured: the whole chunk — cos, content, layout, font, `rustybuzz`,
`ttf-parser`, a Unicode script table, and the generated AGL, metrics and
encoding tables — is **413.2 KB gzipped, 45.9% of budget**.

Q6 measured the object layer alone at 122.7 KB and concluded the module split in
§12.3 stood. It still does, now on a complete measurement rather than a floor.
The size harness gained a `core` variant gated on the real 900 KB figure; the
three cos-only variants keep their tighter ceilings, which exist to catch a
regression in the object layer that 900 KB would never notice.

Building it also re-proves that nothing in the stack needs a filesystem, a clock
or randomness: it compiles and links for `wasm32-unknown-unknown`.

### Licences

Shaping adds nine transitive crates. All permissive — MIT, Apache-2.0, Zlib —
and `cargo deny check licenses` passes. Three of them (`unicode-ccc`,
`unicode-properties`, `unicode-bidi-mirroring`) declare the deprecated
`MIT/Apache-2.0` slash form rather than SPDX `OR`; cargo-deny accepts it, which
was worth verifying rather than assuming given §4.3 makes this a hard build
gate.

## §9 — `rasura-edit`, mutation and commit

**Phases 5 to 7 complete.** The plumbing is built bottom-up and the operation
catalogue sits on top of it, so the order below is the order the bytes travel,
not the order the spec lists them.

| Item | State |
|---|---|
| 9.1 `EditSession`, operations accumulate | yes |
| 9.1 nothing touches the document until commit | yes |
| 9.1 op log with inverses, undo/redo | yes |
| 9.1 commit is atomic | yes |
| 9.1 `StaleSession` on a concurrent session | partial — see below |
| 9.2 `replace_text` / `insert_text` / `delete_range` | yes |
| 9.2 `set_style` | not started |
| 9.2 `set_alignment` / `set_leading` / `set_indent` | declines by name — Phase 6 geometry |
| 9.2 `split_paragraph` / `merge_paragraphs` | declines by name — Phase 6 geometry |
| 9.2 / 10.4 `move_image` | yes — Phase 6 |
| 9.2 / 10.4 `resize_image` (as scale factors) | yes — Phase 6 |
| 9.2 / 10.4 `delete_image` | yes — Phase 6 |
| 9.2 `move_block` for vectors | declines by name — no provenance to move |
| 10.4 `replace_image`, stretch and letterbox | yes — Phase 6 |
| 10.4 `resample_image` | not started — the only piece needing a pixel codec |
| 9.2 `insert_page` with a `PageSpec` | yes — Phase 6 |
| §17 draw-command emitter | yes — `edit::draw::Canvas` |
| 9.2 `insert_paragraph`, `set_z_order` | not started |
| 9.2 `set_cell` | yes — Phase 7 |
| 9.2 `insert_row` / `delete_row` / `insert_column` / `delete_column` / `set_column_width` | declines by name — needs a declared table structure |
| 9.3 greedy line breaking | yes, and still the default |
| 9.3 `KnuthPlass` | yes — Phase 8 |
| 9.3 justification mechanism detection | yes |
| 9.3 overflow `Refuse` / `Allow` | yes |
| 9.3 overflow `Grow` / `Shrink` | typed; the shape is reported, the caller applies it |
| 9.4 step 1, affected operators from glyph runs | yes |
| 9.4 step 2, generate replacement operators | yes |
| 9.4 step 3, splice with verbatim copy | yes |
| 9.4 step 4, re-encode with the original filter chain | yes |
| 9.4 step 5, mark the object dirty | yes |
| 9.4 number formatting matches the producer | yes |
| 9.5 commit and save | yes, through the §5.6 writer |
| Fidelity reported per operation | type exists; populated when operations do |

### The inverse is a byte image, not an instruction

The textbook op log records how to undo: delete what was inserted, re-insert
what was deleted. It is also how undo goes quietly wrong, because a replayed
inverse is exact only if it reproduces every *incidental* byte the original
touched — the `/Length`, the compression level, the operand spacing, the number
formatting. I5 does not say "undo restores the text":

> **I5 — Undo exactness.** Any operation followed by `undo()` restores the exact
> prior byte state.

So the log stores each touched object's prior value and undo puts it back. The
cost is memory proportional to what was edited, which buys a guarantee that
cannot drift as reflow and the operation catalogue are added on top.

**Restoring the value is not sufficient, and that was the first thing I5 caught.**
`Document::set` marks an object dirty, so a document whose objects have all been
restored still saves an appended revision rewriting them to exactly what they
already said. Every object reads back correctly and the file has changed. The
session now discards the staged set once its log is empty, which it can do
safely because it holds the only mutable borrow. No value-level assertion would
have found this; comparing bytes is the whole point of the invariant.

### Logical spans and the objects underneath

A page's `/Contents` may be an array. The content layer concatenates those
objects into one buffer so operators can be found across the join, and every
span this layer works in addresses *that* buffer — the objects on disk do not
exist in it. `stream::localise` maps each patch back through
`LogicalContent::locate_span` to the object that owns those bytes.

A span crossing the join is **refused, not split**. There is no correct way to
write half an operator into each of two streams, and an operator that appears to
cross the boundary is one the caller assembled from spans it should not have
merged.

### Number formatting, and why it is worth a module

Spec 9.4 asks for the producer's own precision and gives the reason: "this
matters because diffs are how users audit you." Every number involved is
numerically equivalent whichever way it is written, so nothing renders
differently — but a commit that rewrites `72.0` as `72` across a page produces a
diff full of meaningless changes and buries the one that matters.

The statistic is the **median** of the decimal counts a stream contains. The
mean is dragged upward by a handful of six-decimal `cm` entries; the maximum is
set by one of them outright; and an upper quantile is the same failure at a
smaller dose — a realistic pdfTeX page turns out to be a third matrix entries,
which is enough to carry the 75th percentile. Where the sampled precision cannot
represent a value exactly, it widens rather than rounding: fidelity to the number
outranks fidelity to the convention.

### Knuth–Plass is implemented and still not the default

That is the point of the greedy decision rather than a contradiction of it. The
two algorithms differ in exactly the way that matters to an editor: greedy
decides each line without looking ahead, so an edit late in a paragraph cannot
move an earlier break, while Knuth–Plass optimises the whole paragraph and a
single added character can shift every line in it. Optimal is right for a
paragraph being *set*; greedy is right for one being *corrected*, and correcting
is what this library does.

The implementation is the 1981 algorithm — adjustment ratios, badness `100|r|³`,
demerits `(ℓ + b)²`, four fitness classes with `\adjdemerits` between
non-adjacent ones — with **no hyphenation**. Discretionary breaks are where most
of Knuth–Plass's advantage lives, and they need a hyphenation dictionary per
language, which is a §12.3 bundle-size decision rather than a line-breaking one.
Without them this is optimal over the breakpoints that exist, which is the claim
being made and no more.

Two modelling decisions the specification does not make, both of which needed a
failing test to find:

**Interword elasticity.** TeX takes stretch and shrink from the font's
`\fontdimen`s. A PDF has none: `/Widths` gives the space's natural advance and
says nothing about how far it may give. The values used are Computer Modern's,
`w/2` and `w/3`, and without them nothing is justifiable at all — with no
stretch every line but a perfectly-filled one is infinitely bad.

**A stretch floor of one space per line.** TeX gives a line containing no glue —
one word, short of the measure — *infinite* badness, then relies on
`\emergencystretch` to make it finite again. Copying that meant every paragraph
containing a one-word line had no feasible breaks and fell back to greedy, which
is most paragraphs. One space of floor makes such a line merely bad, and bad *in
proportion to how short it is*, which is the comparison the algorithm exists to
make. The test that pins it takes the arrangement leaving a six-unit hole over
the one leaving seven; under the infinite-badness model both are equally
impossible and the optimiser has nothing to choose between.

The gain is checked against the textbook counterexample rather than asserted:
`aaa bb cc ddddd` at measure 6, where greedy fills line one to the margin and
strands `cc` alone with a four-unit hole, and moving one word back costs three
and saves four.

### Writing text means running §7.2 backwards

Everything below layer five reads in one direction: a code becomes a glyph, and
seven strategies turn that into Unicode. Writing needs the inverse, and the
inverse is **not a second implementation of the same table** — it is this
document's table, run backwards. An encoder built from the Adobe Glyph List
would be right about what `é` should be and wrong about what this file's font
draws at that code.

`encode::Encoder` inverts `layout::unicode::Decoder`, which buys the property
that matters: *text written back extracts, through the chain a reader uses, to
the text that was asked for*. That is asserted against the real reader rather
than against a copy of the table.

The code space bounds what is possible. A simple font's 256 codes are
enumerable, so any character in its encoding can be typed. A composite font's
`Identity-H` space is 65,536 codes of which a subset carries perhaps two
hundred, so its inverse is built from the codes the document is **observed** to
use: a character already on the page can be typed again, and one that is not is
reported `Unencodable` rather than guessed at. §8.4's glyph injection is what
moves that boundary, and the decision to spend a font edit on a character
belongs to the caller.

### The unit of replacement is the showing operator

An edit could rewrite only the bytes of the characters it touched. It does not,
because a code's byte length is a property of the font's codespace rather than
of the character: replacing `e` with `é` in a composite font changes the
string's length and invalidates every glyph span measured against it.
Regenerating the operator from the run's whole text keeps one source of truth,
and bounds the damage — everything outside that operator is copied verbatim, so
a bad encoding cannot move a different line.

A range spanning **two** showing operators is declined by name
(`TextError::Fragmented`). Regenerating both means deciding what happens to
whatever the producer put between them, and a producer that split a line across
operators usually did so *because* something sits in the gap: a colour change, a
font change, a rise.

### A paragraph that never wrapped has no measure

The first version of the fit check took the measure from the paragraph's widest
line. For a wrapped paragraph that is right — the producer showed us where it
breaks. For a **single-line** paragraph it is circular: the one line is exactly
as wide as its text, so every added character overflows, and a heading gaining
one letter reports itself re-broken into two lines. A fidelity report that cries
wolf stops being read.

`measure_of` now returns `None` for a single line, and the caller falls back to
the distance from the line's left edge to the crop box. That is a weaker bound
and a *true* one: glyphs past the visible edge are not visible, and no producer
intended that.

### Justification is a mechanism, not an effect

Two paragraphs justified to the same measure by different mechanisms are
identical in width and different in every glyph position between the first word
and the last. `reflow::justification` picks the mechanism that **varies from
line to line**, because a producer setting `Tw` for the gaps and `TJ` for
kerning carries both, and only one of them is doing the justifying. A constant
`Tw` on every line is a paragraph-wide setting, not justification.

### Moving an image: wrap, do not rewrite

Phase 6's first block operation, and the design is not the obvious one.

The obvious implementation finds the `cm` that positioned the content and edits
its operands. It does not survive real files. A CTM is *accumulated* — by the
time an image is drawn the transform may be the product of the page's base
matrix, a `cm` in an enclosing `q`, a form XObject's `/Matrix`, and one more
immediately before the `Do`. There is no single "the `cm`", and the last one is
not privileged: changing it moves everything else drawn under the same `q`.

So the drawing operator is **wrapped** instead:

```text
q  a b c d e f cm  <the original operator, byte for byte>  Q
```

`q`/`Q` bracket the change, so nothing outside is affected by construction
rather than by analysis. And the original bytes are carried through untouched,
which matters more than it looks: an inline image's pixel payload lives *inside*
its operator, and 71% of the corpus's images are inline. Re-emitting one would
mean re-encoding data this library deliberately never decodes.

**36% of the corpus's images are rotated or skewed** — 1,129 of 3,095. A move
that rewrote a bounding box would flatten every one, so the translation is
composed rather than assigned. The first implementation computed
`M = CTM × T × CTM⁻¹`, which is correct and round-trips the whole matrix through
an inversion; the linear part came back as identity plus floating-point litter,
printing `1 0 0 1 -0 0 cm` on an unmoved image. Since `M` is always a pure
translation, only its vector is unknown and it solves directly as
`v = (dx, dy) × linear(CTM)⁻¹`. The linear part is then exactly identity by
construction.

An image inside a form XObject is **declined**. A form may be invoked many
times; editing its stream moves every instance, which is not what "move this
image" means.

Vector artwork declines too, and for a reason worth stating plainly:
`VectorBlock` records a bounding box and a path count — no transform, no path
data, no operator spans. There is nothing to wrap. Fixing it means retaining
path provenance in the collector, not working around it in the edit layer.

**Resizing takes scale factors, not a target rectangle.** `resize_block(block,
rect)` is what the spec writes and it cannot express a rotated image: fitting a
parallelogram into an axis-aligned rectangle means discarding the rotation, on
36% of the corpus. Factors compose with the existing transform instead. The
anchor is the image's own local origin, so scale and move compose predictably —
growing about a centre is a scale followed by a move of half the difference.

A zero factor is **refused** rather than treated as "make it invisible". It
produces a singular transform, which cannot be scaled back and which
`move_image` would then reject; `delete_image` is the operation that means make
it go away, and it is reversible.

**Deleting removes the drawing operator and nothing else.** Any `q`/`cm`/`Q` the
producer wrapped it in stays: without a `Do` between them those operators paint
nothing, and removing them would mean deciding whether they also positioned
something else — which inside a `q` they often did. The image *object* is left
alone too, since it may be drawn on other pages; an unreferenced XObject costs
bytes rather than correctness, and reclaiming it belongs to a compacting save
where the whole document is in view.

### Object-level edits get the same undo guarantee

Page operations do not splice bytes into a content stream — a page removed from
a `/Kids` array changes the document's *shape* rather than the marks on a page,
and there is no span to patch. `EditSession::set_objects` is the sibling
primitive: it captures each object's prior value before writing any of them, so
I5 holds for structural edits exactly as it does for text.

### §10.9 — destinations, and what the corpus says to build

> Any operation that changes page count or order must fix up `/Outlines`,
> named destinations, `/Link` destinations, `/OpenAction`, `/Threads` and
> `/PageLabels`. **A dangling destination is a silent corruption; add an
> invariant check for it.**

Nothing in the workspace read any of those keys, so before writing page
operations the corpus was measured. Across the 960 pdf.js documents that open:

| | Documents | Destinations |
|---|---|---|
| `/Outlines` | 176 (18.3%) | 343 items, 306 with a destination |
| `/Link` annotations | 58 (6.0%) | 566 links, 180 with a destination |
| `/OpenAction` | 56 (5.8%) | 51 explicit |
| `/Names` → `/Dests` | 17 (1.8%) | 60 named |
| root `/Dests` (pre-1.2 form) | 5 (0.5%) | 34 named |
| `/PageLabels` | 22 (2.3%) | 33 ranges |
| `/Threads` | 1 (0.1%) | **0 beads** |

Two findings changed the design.

**The `/A` action form dominates, and the spec's sentence does not mention it.**
A destination is written either as `/Dest` directly or as an `/A` action of
subtype `/GoTo` carrying `/D`. The corpus has `/A` `/D` outnumbering bare
`/Dest` **3.6 : 1** on links and **4.5 : 1** on outline items. An implementation
following §10.9 literally would find about a quarter of the destinations in the
corpus and report the rest as clean.

**`/Threads` is theoretical.** One document has the key; its value is an empty
array. There is not one real article thread in 960 files. It is reported and
deliberately not traversed — a walk nothing can test is a walk that will be
wrong when it finally matters, and saying so beats shipping untested code that
looks like coverage.

Named destinations are only 23% of the total but 107 of the 180 *link*
destinations, so the name tree is unavoidable despite only 17 documents having
one. Both the name tree and the older root `/Dests` dictionary are read.

#### The check, and what it already found

`content::dest::collect` does the walk and the invariant suite uses the same
code, so the check and the future fix-up cannot disagree by construction.
Running it over the corpus: **199 documents have destinations and all of them
resolve; 8 documents already dangle.**

Those 8 are genuine, not false positives. `bug886717.pdf`, `issue6204.pdf` and
`pdfjs_wikipedia.pdf` contain no `/Names` and no `/Dests` anywhere in their
bytes and have no object streams — the names their links use are defined
nowhere. `pdfjs_wikipedia.pdf` resolves 31 of 58, which are its explicit
destinations, so the walk is finding what is there. The names themselves are
telling: `section*.2`, `subsection.10.2.1`, `cite_note-…` — LaTeX and MediaWiki
output left behind when pages were extracted. This is exactly the silent
corruption §10.9 describes, produced by tools that deleted pages without fixing
up, and it is a useful reminder that the fix-up is the hard part rather than the
deletion.

It is reported as an **input defect rather than a failure**, because these are
files this library did not write and the corpus is full of deliberately broken
input. On *output* it is a hard requirement: `delete_page` refuses rather than
producing one.

#### Delete is hard; reorder is not

The asymmetry is not obvious and it decides most of `edit::pages`. **A
destination names a page by object reference, not by index.** Reordering
`/Kids` therefore breaks nothing — every outline entry and every link still
points at the same page object, which is still in the document, and viewers
resolve it to its new position. The only thing a reorder invalidates is
`/PageLabels`, whose number tree *is* keyed by index, and that is reported as
`PageLabelsStale` rather than silently renumbered.

Deleting is the opposite: every destination naming the removed page now names an
object outside the page tree. The file opens, renders and extracts perfectly,
and a click goes nowhere.

`delete_page` therefore does four things: removes the one `/Kids` entry the page
tree recorded, decrements `/Count` up the whole `/Parent` chain, retargets every
destination that named the page, and refuses outright if any of them could not
be retargeted. That last clause is the point — a half-fixed document is exactly
the corruption being guarded against, so the operation is all-or-nothing.

**Retarget rather than remove.** A destination pointing at a deleted page could
be dropped instead. Retargeting sends it to the page that took its index — what
a reader scrolling to that position now finds — falling back to the previous
page when the deleted one was last. Dropping would silently lose an outline
entry the user can see in their sidebar. The view specification (`/XYZ left top
zoom`, `/Fit`, and the rest of §12.3.2.2) is carried through untouched, since it
describes where *on* a page to land and is as valid on the replacement.

`/Count` is fixed up the whole ancestry rather than just the immediate parent.
A viewer that trusts `/Count` over the actual kid list shows a blank page
otherwise — a rendering defect that `qpdf --check` does not catch.

A supporting change: the page tree now records each page's **parent node and its
slot in that node's `/Kids`**. Without it a page could be found and not removed —
`PageTree` knew where every page was and not where any of them was *listed*.

### The draw-command emitter, and why it stays small

`edit::draw::Canvas` is the one piece that produces content which never existed —
a new page, a caption under a figure. It is deliberately the smallest thing that
can: a builder that appends operators, with a `q`/`Q` nesting check and nothing
else.

The temptation is a drawing API — shapes, styles, a colour type per space. That
is a graphics library, and PDF already has one: **the operators are the API**,
and every abstraction over them is a place where the bytes this crate emits stop
resembling the bytes a producer would have written. §9.4's number-formatting
rule points the same way.

The one thing it enforces is balance. An unbalanced `q` leaks its graphics state
— a fill colour, a clip, a transform — into everything drawn afterwards, and the
symptom appears in unrelated content further down the page. `finish()` refuses
rather than emitting one, and refuses to auto-close: a caller who forgot a `Q`
has usually forgotten *where*, and closing at the end puts content inside a
state it was never meant to be in.

Text takes **codes, not characters**. The module never sees a `char`, so it
cannot write the wrong glyph for one; encoding is `Encoder`'s job and stays
there.

### Creating objects inside a transaction

`insert_page` exposed a real hole in the transaction model. It first called
`Document::add`, which allocates a number *and writes the object* — so the new
content stream landed in the dirty set before the session recorded that it had
not previously existed, and undo restored it instead of removing it. The
inserted page came back on undo; its content stream stayed.

`Document::reserve(n)` now separates the two halves. The operation claims object
numbers, the session creates the objects along with everything else, and undo
deletes them because their prior value was genuinely nothing. Numbers are never
reused, so an abandoned reservation costs a gap in the numbering and nothing
else.

The test that caught it asserts an insertion undoes to the exact original bytes —
the harder direction, because the operation *created* objects rather than
changing them.

### Replacing an image needs no pixel work

> `replace_image(image_block, bytes, format)` — swap an XObject's data. If the
> new image has different dimensions, either preserve the placement rectangle
> (default, stretch) or preserve the aspect ratio (opt-in, letterbox).

The caller supplies encoded bytes and the filter they are in, so nothing is
decoded here — which is what separates this from `resample_image`, the only
operation in §10.4 that genuinely needs a codec. The filter is *taken* rather
than sniffed: guessing a codec from magic bytes is how a mislabelled stream
becomes a corrupt file.

`Stretch` keeps the rectangle and reports `ImageDistorted` when the proportions
differ, because "the picture looks squashed" is what a caller wants told rather
than discovered. `Letterbox` returns a placement patch **only when one is
needed** — a same-shaped replacement rewrites the object and touches no page at
all. An inline image is declined by name: its bytes live in the content stream,
so the object path would silently do nothing.

## §9.2 — Tables, and why five of the six operations decline

§7.7 *detects* tables, from a drawn grid or from columns of text that line up,
and the detection is good enough to read one. It is not good enough to
restructure one, and the six operations spec 9.2 lists are not six variations on
a theme.

`set_cell` edits text that already exists, inside a region §7.7 identified. It
resolves the cell to a paragraph by position and hands the rest to
`replace_text` unchanged — nothing about a cell makes its text special, and a
separate path would be a second place where encoding and reflow could diverge.
If the detection was wrong the wrong text changes, and the caller sees that
immediately.

The other five *move* content. Inserting a row means shifting every cell below
it down, redrawing the rules that separate them, resizing the grid, and
reflowing any cell whose new width no longer fits — on a structure that was
**inferred** rather than declared. A misdetected column edge becomes a visibly
broken table, and unlike a wrong `set_cell` the damage is spread across the
whole figure with no single place to look.

So they decline **individually and by name**, and the error says what would make
them possible: `/StructTreeRoot` with `/Table`, `/TR` and `/TD` elements, where
the producer declared the structure instead of leaving it to be guessed. A
caller discovers the limit at the call site rather than in a changelog.

A related rule falls out of the same reasoning: a paragraph whose glyphs are not
*entirely* inside one cell is claimed by no cell at all. Editing it would change
text outside the region the caller addressed, which on an inferred grid is
exactly how a wrong detection becomes a wrong edit.

## §10.2 — Optional content, and what "hidden" is not

| Item | State |
|---|---|
| Preserve `/OCProperties`, `/OCGs`, `/OCMDs` | yes — nothing is rewritten |
| Layer names, visibility, `/Intent`, `/Locked`, `/Usage` | yes |
| `/D` default configuration, `/BaseState`, `/ON`, `/OFF` | yes |
| `/OCMD` policies `/AnyOn` `/AllOn` `/AnyOff` `/AllOff` | yes |
| `/VE` visibility expressions | yes, depth-bounded |
| `BDC /OC` … `EMC` content regions | yes |
| An XObject's own `/OC` on the `Do` that draws it | yes |
| Edits stay inside the block | yes, structurally — see below |
| Flattening layers | **refused**, per the spec's emphasis |

Measured over the 992 corpus documents that open:

| | Count |
|---|---|
| Documents with `/OCProperties` | 29 (2.9%) |
| Layers declared | 134 |
| **Layers off in the default configuration** | **89 (66%)** |
| Regions on a page | 1,091 |
| **Regions hidden** | **1,050 (96%)** |
| Regions from an XObject's own `/OC` | 13 |

### A hidden layer's text is in the document

Not a corner case: two thirds of declared layers are off, and 96% of the regions
on a page belong to one. Hidden is an instruction to a viewer, not a property of
the bytes — the text extracts, `strings` finds it, a reader that ignores
visibility copies it. Two consequences run in opposite directions and both are
implemented:

- **Redaction ignores visibility entirely.** Text in a layer that is off is
  removed like any other. Skipping it would be the cosmetic failure §10.6 exists
  to prevent, and worse than usual — the page renders identically either way, so
  nothing about the output would look wrong.
- **An edit to hidden content is reported.** `Compromise::EditedHiddenLayer`
  names the layer. The edit is real and the bytes change; the page does not,
  because no viewer draws that layer. A caller handed an exact result and an
  unchanged-looking document concludes the library is broken.

### An `/OC` names a group, so the reference must not be resolved

`ResourceStack::lookup` resolves what a name points at, which is right for
everything that wants an object's *contents* and wrong for anything that needs
its identity. An `/OC /L1 BDC` is a statement about *which group*, and resolving
the reference throws away the object number that is the whole answer — layers
came back with empty membership and every region read as visible. `lookup_raw`
exists for that distinction and is used only here.

### The XObject form is not negligible

Thirteen regions come from an XObject whose *own dictionary* carries `/OC`, with
no `BDC` anywhere near them. A walker looking only for marked content counts
every one as ordinary visible content — a hidden watermark reported as drawn.

### Do not flatten

The spec's emphasis, and the reason has the same shape as the redaction one.
Flattening means deciding which content survives, and the decision depends on a
configuration the *viewer* owns and the user can change after the file is saved.
A CAD drawing with its dimensions layer off is not a drawing without dimensions;
it is a drawing whose dimensions someone will turn back on.

## §10.1 — Tagged PDF, and I6

**I6: 50 passed, 0 failed.** 919 of the corpus's documents are untagged and are
skipped rather than passed — a pass would claim the tagging survived an edit on
a file that has none.

`validate_tags` compares what the structure tree **claims** against what the
pages actually **draw**, reading marked-content ids from the content streams
rather than from the model. Checking the tree against a model built by the same
walk would pass a tree that had been consistently detached from its content.

Spec 10.1's `taggedStatus` has three states, and the third is the point.
`Degraded` — a tree that no longer describes its content — is reported
separately from `Tagged` because assistive technology reads such a file *worse*
than an untagged one: it trusts a map that is wrong instead of falling back to
geometry. The two failure directions are named separately for the same reason:
a `DanglingMcid` makes a screen reader announce a heading with nothing under it,
while an `UnclaimedMcid` is text that is drawn and read aloud by nobody.

I6 runs the validation **before and after a real edit** and requires the element
count, the claimed and drawn marked-content counts, and the finding count all to
survive. A document that arrives degraded may stay degraded; it may not get
worse. The probe is an equal-length text replacement, which is the gentlest edit
available and therefore the one where a regression is most clearly ours —
marked-content operators sit outside a showing operator's span, so a correct
patch leaves every `BDC`/`EMC` pair exactly where it was.

## §10.7 — Annotations

| Item | State |
|---|---|
| Read all seventeen subtypes | yes |
| Delete, and unlink from the page's `/Annots` | yes |
| `set_contents` | yes |
| Create `/Square`, `/Circle`, `/Line`, `/Ink` | yes, with a generated `/AP` `/N` |
| Create `/Highlight`, `/Underline`, `/StrikeOut`, `/Squiggly` | yes, from `/QuadPoints` |
| Create `/Link` | yes, deliberately without an appearance |
| Create `/Text`, `/Stamp`, `/FileAttachment`, `/Popup`, `/FreeText` | declines by name |
| Create `/Widget` | declines — belongs to the form layer |

Spec 10.7 is easy to half-do, and the half that gets skipped is the one that
matters:

> Appearance streams (`/AP` `/N`, `/R`, `/D`) must be generated for any
> annotation Rasura creates or modifies — viewers that do not synthesise
> appearances (most of them, for most types) will otherwise show nothing.

Writing an annotation dictionary is trivial. Writing one that *appears* means
drawing it, and what a `/Stamp` or a note icon should look like is a design
decision no specification makes for you. So the split is not by difficulty but
by whether the appearance is **determined**: a `/Square` is its `/Rect` stroked
and filled per `/C` and `/IC`, a `/Line` is the two points in `/L`, an `/Ink` is
the paths in `/InkList`, a `/Highlight` is the quads in `/QuadPoints`. For those
there is one right answer and this module draws it, through the same `Canvas`
the §17 emitter uses — circles as four Bézier arcs with the usual
`K = 0.552 284 75`.

For `/Text`, `/Stamp`, `/FileAttachment` and `/Popup` there is not. The icon is
the viewer's to choose, every viewer chooses differently, and inventing one
produces a document that looks like no other reader would have drawn it. Those
return `NeedsDesignedAppearance` naming the type.

`/Link` is the interesting exception: it is created **without** an appearance on
purpose. A link is a rectangle a viewer makes clickable, and §12.5.6.5 gives it
no visible form of its own beyond an optional border. Generating one would add a
mark the format does not ask for.

Reading and deleting need no appearance, so both cover all seventeen. That
asymmetry is the point of keeping `Kind::has_derivable_appearance` separate from
`Kind::from_name`.

## §10.4 — AcroForm fields

| Item | State |
|---|---|
| Enumerate fields with fully-qualified names | yes |
| `/Btn`, `/Tx`, `/Ch`, `/Sig` classified | yes |
| Read values, widgets, `/DA`, flags | yes |
| `set_text_value`, regenerating `/AP` | yes |
| `/NeedAppearances` set alongside | yes |
| Setting a `/Sig` value | refused, per spec §3 |
| XFA | refused — the AcroForm only shadows the real content |

Names come from §12.7.3.2: a field's fully-qualified name is its ancestors'
`/T` values joined with periods, and two different fields can share a partial
name while differing in their parent. Reading `/T` alone would make
`address.city` and `billing.city` indistinguishable, and a caller setting one
would silently set the other.

`set_text_value` writes `/V` and then regenerates the appearance rather than
leaning on `/NeedAppearances` alone. Both are done because neither is
sufficient: a viewer that honours `/NeedAppearances` re-renders and is fine
without a stream, and one that ignores it shows whatever `/AP` says — which,
left alone, is the *old* value. A file that displays its previous contents in
half the readers on the market is the worst of the available outcomes.

The regenerated stream takes `/DA` **verbatim**, spliced through `Canvas::raw`
rather than re-parsed and re-emitted. `/DA` is a fragment of content-stream
syntax the producer wrote; re-emitting it means re-deciding its font name,
size and colour operators, and any of those decisions being different is a
field that changes appearance for no reason the caller asked for.

Two refusals. A `/Sig` field's value is not text — writing one would be creating
a signature, which spec §3 refuses permanently. An XFA form's real content is an
XML payload that the AcroForm merely shadows, so editing the AcroForm produces a
file whose two halves disagree; XFA is detected at `read` and every edit
declines.

`/AcroForm` written inline in the catalog rather than as an indirect object was
the practical trap here: `acroform_id` returns `None` for it, so
`/NeedAppearances` went nowhere and the tests still passed because the
regenerated `/AP` covered for it. The catalog is now rewritten in that case.

## §10.8 — Flattening

> Field flattening: convert widget appearances into page content and remove the
> fields. Common request; implement it.

The obvious implementation reads `/V`, picks the font from `/DA` and lays the
text out again. That reproduces the *data* and not the *appearance* — and the
appearance is what the person filling the form saw and approved. Alignment,
comb spacing, an `/MK` border, a chosen radio button's glyph, an ink signature's
path: none of that is in `/V`, and a re-render silently produces a document
differing from what was signed off.

So the existing `/AP` `/N` stream is invoked as a form XObject, mapped from its
transformed `/BBox` onto the annotation's `/Rect` per §12.5.5. The bytes a
viewer would have drawn are the bytes that get drawn — the same principle as
`blocks`'s wrap.

Three cases the tests pin:

- **A check box draws the state `/AS` names.** `/AP` `/N` is a dictionary of
  states for a button, and drawing the wrong one shows a box ticked that is not.
  That is the single most consequential thing to get wrong here.
- **Hidden annotations are not drawn.** A viewer does not draw them, so
  flattening would *add* marks the reader never saw.
- **An annotation with no appearance stream is left interactive and reported.**
  Inventing what a viewer might have shown is a different and much less safe
  operation than preserving what it did.

## §10.6 — Redaction, the one that must be correct

| Step | State |
|---|---|
| 1. Remove glyph-showing operators over the region | yes |
| 2. Remove intersecting image data | **no** — the only piece needing a pixel codec |
| 3. Annotations, form field values, link targets | yes |
| 4. Strip `/ActualText` and `/Alt` | yes |
| 5. Purge from `/Info` and XMP | yes |
| 6. Remove glyphs from the font subset | **no** — reported on every redaction |
| 7. Force `FullRewrite` | yes, enforced in `rasura-cos` |
| 8. Drop prior revisions | yes, a consequence of 7 |
| 9. Draw the redaction box | the caller's, via `Canvas` |

**I7: 329 passed, 0 failed** across the corpus.

Every failure mode here is silent and total. A redaction that left the text
behind looks identical to one that did not — the page renders the same, the file
opens, `qpdf --check` passes — and the difference only appears when someone
selects the text or runs `strings`. So the module does two things, and the
second matters as much as the first: it removes content, and it **re-reads the
saved output to prove the content is gone**.

### Enforced in code, as the spec demands

> **Forces `SaveMode::FullRewrite`.** This is non-negotiable and must be
> enforced in code, not documentation.

`Document::mark_redacted` sets a flag the writer checks *before* the caller's
own request, so asking for `Incremental` explicitly cannot defeat it, and there
is deliberately no way to unset it. A test demonstrates the failure it prevents:
the same edit saved incrementally leaves `Kowalski` in the file, and `verify`
catches it.

`verify` takes **bytes, not a `Document`** — verifying the in-memory document
would check the thing that was edited, while verifying the saved file checks the
thing that will be handed over, and those differ by exactly the save where an
incremental append leaves the original behind. It also searches the raw file, not
only the parsed objects: a string can survive somewhere no object model reaches.

### What the corpus taught, in order

Steps 3 and 4 grew considerably in the doing. Each of these was found by running
I7 over 1,030 files rather than by reading the spec:

1. **Page-scoped was wrong.** The first version redacted one page; `verify` is
   document-scoped and immediately failed on a word appearing on page two.
   `apply` is now document-wide, and `plan` is documented as the inspection
   entry point rather than the one to use.
2. **Form XObjects were a correctness bug, not a gap.** A run inside a form has
   `op_span` into the *form's* stream while the patch is applied against the
   *page's*. When the page stream is longer, the splice succeeds and rewrites
   unrelated bytes — leaving the redacted text exactly where it was. This
   affected `replace_text` too. Both now refuse, and a test builds a page long
   enough that an unguarded splice would land rather than fail a bounds check.
3. **Form field values, outline titles, signature dictionaries.** `/V`, `/DV`,
   `/Title`, `/Reason` and their neighbours all held redacted words in real
   files. `/T` is included **only for structure elements** — on a form field it
   is the partial name, and removing it detaches the field.
4. **Indirect strings.** An `/ActualText` whose value is a *reference* to a
   string object slips past a direct `as_string` check in complete silence. The
   value is now resolved, and the referenced object blanked rather than the key
   dropped — dropping the key leaves the string reachable from a full walk.

### The glyphs that stay do not move

Removing glyphs from the middle of a showing operator and writing the remainder
as one `Tj` lets the tail of the line **close up**. Every check above still
passes: the word is gone, the file is valid, `verify` is clean. It is still
wrong, and specifically wrong for redaction rather than as a matter of taste.

Step 9 is the caller's: they draw the black box, over a rectangle computed from
the layout as it was **before** the removal. If the tail slides left into the
gap, that rectangle now covers words nobody asked to hide, while the words that
moved out from under it are perfectly legible. A redaction that relocates text
into and out of its own censor bar is a worse outcome than one that fails.

So the operator becomes a `TJ` in which each removed stretch is replaced by a
position adjustment of exactly the advance it contributed — §9.4.3's
`tx = (−t/1000 × Tfs) × Th`, inverted, with `Th` omitted in vertical writing
mode per §9.4.4. Every surviving glyph keeps its device-space origin, and the
pen ends the operator where it would have, so anything positioned relative to it
is unaffected too. A trailing removal keeps its adjustment for that reason
alone.

The gap does leak the removed text's width. So does the black box, which has to
be that wide to cover the region — this is the same information, and it is not a
reason to move the rest of the line.

The test that pins it compares glyph origins before and after, and it was
checked by breaking it: with the closing-up behaviour restored, `reporting`
slides 47.9 units left and the test fails. Its side effect on the corpus was one
extra file passing I7 — encoding the remaining text as a single string had
failed there, where the surviving fragments encode individually.

### A panic in the object layer, found by redaction

Reading a `/Title` for step 5 crashed the suite. `pdf_doc_encoding_char`
indexed a 32-entry table at `c - 0x80 + 8`, so **any string containing a byte
from `0x98` upward panicked** — and the low range `0x18..=0x1F` was reading the
high range's characters, so PDFDocEncoding's accents decoded as punctuation.
Two tables now, and a test that walks all 256 byte values, because §14.4 says
the parser must never panic.

### What I7 measures, and what it does not

The check redacts a word the page genuinely draws **and that appears nowhere
else in the document**. That condition is what makes it measure redaction rather
than the corpus: some files name a font after a word on the page
(`/BaseFont /NuptialScript`), and one points `/Count` at a junk stream whose
bytes contain a word from the text. Removing those would break the font
reference or the page tree, so no correct redaction removes them, and asserting
their absence would be asserting that redaction should corrupt the file.

`verify` remains stricter than the check and reports them, because a caller
asking "does this string appear in the bytes I am about to hand over" wants the
strict answer. Its report names *what kind of object* held each trace — "an
embedded font program", "/Type /Sig" — since a full rewrite renumbers objects and
a bare object number does not even identify the same object the caller started
with.

### There is no document flow, and there cannot be one for free

The question "if I add a paragraph to page 3, does the rest of the document
reflow?" has a short answer: **no**, and the reason is the format rather than
this implementation.

A PDF is the *output* of a layout process, not an input to one. Every glyph
carries an absolute position; the block on page 4 is not positioned relative to
anything on page 3, and the information that would say it should be — the flow
the producer's word processor used — was discarded when the file was written.
Nothing "pushes down" because nothing was ever stacked. Spec §9.3 fixes the
scope accordingly:

> Scope is the paragraph. Never wider unless the caller opts into overflow
> propagation.

So what happens when a paragraph grows past its measure:

| Policy | Behaviour today |
|---|---|
| `Refuse` (default) | the edit fails with `Overflow { lines_over }` and nothing changes |
| `Allow` | the paragraph re-breaks into extra lines **which overlap whatever is below**, reported as `Overflowed { lines_over }` |
| `Grow` | typed, and behaves as `Allow`: the new shape is reported, nothing is pushed |
| `Shrink` | typed, and behaves as `Allow`: no size reduction is applied |

`Grow` is the spec's real answer — "extend the block downward, pushing
subsequent blocks on the page; cascade to following pages if needed" — and the
spec flags it as the hardest thing in the phase, because cascading changes page
count and therefore outlines, destinations, link annotations and the structure
tree. It needs Phase 6's page operations before it can be attempted honestly,
and until then it reports rather than pretends.

Adding a *new* block at all — §9.2's `insert_paragraph(page, rect, text, style)`
— is Phase 6 for the same reason. Placing content is not the hard part; deciding
what it displaces is.

#### The re-break is emitted, which it was not at first

The first version of this computed the new line breaks and threw them away: it
wrote one long `Tj` running off the right edge of the page while *reporting*
`LinesRebroken`. Every structural check passed, the text extracted correctly,
and the page was wrong — and the fidelity report named a compromise that had not
been made, which is worse than not reporting one.

Extra lines are now written as `(line) Tj  0 -leading Td` pairs inside the same
operator. `Td` is cumulative and moves the *line* matrix, so the last thing
written is a `Td` putting it back where it was found: without that, a paragraph
gaining a line would drag every following relatively-positioned operator in the
text object down with it. The net effect outside the rewritten operator is nil,
which is what keeps the edit local — and is also precisely why the new lines
overlap rather than displace.

### What `StaleSession` currently means

The spec's version is optimistic concurrency across sessions. Rust's borrow
checker already prevents two `EditSession`s over one `Document` — the session
takes `&mut Document` — so the case the spec worries about cannot be constructed
through this API. What remains is a session opened over a document someone else
had already staged changes into, which `opened_dirty()` reports. The error
variant exists for the WASM surface in Phase 6, where sessions are handles and
the borrow checker is not in the room.

---

## §9.6 — Signatures

| Item | State |
|---|---|
| Detect `/Sig` fields | yes |
| `SignatureImpact` reported before saving | yes |
| Full rewrite requires explicit acknowledgement | yes |
| Creating a signature | refused, per spec §3 |

Detection walks `/AcroForm` `/Fields` for `/FT /Sig`. It does not verify
`/ByteRange` coverage — the question Phase 7's form editing raised and then
sidestepped: `set_text_value` refuses a `/Sig` field outright, so no edit here
has yet needed to know whether an existing signature's byte range is honest.
Verification becomes necessary the moment something edits a signed document
without invalidating the signature, which nothing does and nothing is planned
to.

---

## §14 — Testing

Measured over 1,030 corpus files:

| Invariant | Passed | Failed | Skipped | State |
|---|---|---|---|---|
| I1 identity | 872 | 0 | 140 | implemented |
| I2 locality | 872 | 0 | 140 | object half; **the pixel half now runs in CI** |
| I3 validity | 1012 | 0 | 0 | structural; `qpdf --check` runs in CI |
| I4 round-trip stability | 872 | 0 | 140 | object level |
| I5 undo exactness | 763 | 0 | 249 | implemented, Phase 5 |
| I6 tag integrity | 50 | 0 | 962 | implemented, Phase 7 |
| I7 redaction completeness | 329 | 0 | 683 | implemented, Phase 7 |
| 10.9 destinations resolve | 199 | 0 | 813 | implemented, Phase 6; 8 skips are input defects |

Unimplemented invariants report `Skipped` with a reason. A suite that reports
green for checks it did not run is worse than no suite.

**And the reasons are now printed, grouped, per invariant.** A total skip count
says how much was not checked but never *what*, and an invariant that quietly
skips most of the corpus looks identical in that number to one that passes it.
The distinction matters most for a check that can decline for a reason which is
really a failure: I5 skips when its probe edit will not apply, and without the
breakdown that would be indistinguishable from a file having no content. It does
not currently happen — I5's 249 skips are 140 recovery-mode files (the same
exemption I1, I2 and I4 take, since recovery forces a full rewrite and byte
identity is invalid by design), 105 pages with no content stream, and 2 files
with no page tree.

I5's probe is a real content-stream edit through `rasura-edit`, not a
synthetic one: it localises a span to its object, splices, re-encodes through the
original filter chain and stages the object. A probe that only called
`Document::set` would pass without exercising any of that. The check also refuses
to pass if the probe edit produced no change, because an undo that follows a
no-op proves nothing.

I3 checks the **output** of a save, not the input. The invariant says "output
passes `qpdf --check`", and checking the input instead conflates a defect
Rasura introduced with one the file arrived carrying. Input defects are
reported separately as diagnostics that do not fail the run.

### Corpus

| Source | Files | Licence | Committed |
|---|---|---|---|
| Generated fixtures | 20 | ours | no — regenerated by the harness |
| mozilla/pdf.js `test/pdfs` | 974 | Apache-2.0 | no — `corpus/fetch.sh` |
| Chrome print-to-PDF | 3 | ours | no — `corpus/fetch.ps1` |

External corpora are fetched into the git-ignored `corpus/external/` rather than
vendored: they are other people's files under other people's licences.

| LaTeX (pdflatex/xelatex/lualatex) | 13 | ours | no — `corpus/latex/build.ps1` |

Spec §18 question Q1 is **answered**: `/ToUnicode` coverage is 53.0% across 1390
embedded fonts. See [q1-tounicode-coverage.md](q1-tounicode-coverage.md) for what
that changes about the Phase 3 plan. `corpus/manifest.toml` enumerates the
remaining gaps; the notable ones are Word, InDesign and LibreOffice.

Three corpus files are expected declines, listed with reasons in
`expected_decline`: two have no `/Type /Catalog` anywhere, and one has an
`/Encrypt` that does not resolve to a dictionary.

Fuzz targets exist for the lexer, document open, the filters and the
cross-reference parser. They are wired into CI as a 60-second smoke run per
target; no long campaign has been run yet.

---

## §11, §12 — The facade and the WASM surface

| Item | State |
|---|---|
| §11.7 the Rust facade, synchronous | yes — `rasura` |
| §11.5 one coded error type, never a bare error | yes |
| §11.2 `Document`, `Page`, `Paragraph`, `Block` | yes |
| §11.2 `documentKind` | yes — newly written; nothing below it classified scans |
| §11.3 `fontRequirements` and `registerFont` | yes |
| §10.3 metadata, both surfaces and their disagreements | yes |
| §11.4 the fidelity contract and `requireFidelity` | yes |
| §12 the wasm-bindgen surface | yes — `rasura-wasm` |
| §12.3 module built with the specified flags, measured in CI | yes |
| §12.1 threading | single-threaded, honestly reported |
| §12.2 Worker protocol, §12.4 npm package | yes — see the section below |
| §11.4 the edit catalogue beyond text | yes, bar `insert_page` and image pixels |

### The facade had to come first

§11.7 settles the order: "The facade crate exposes the same model
synchronously for native consumers. The WASM layer is a thin async adapter over
it. **Design the Rust API first; do not let WASM ergonomics distort the core.**"
There was no facade, so a WASM layer would have been an adapter over nothing —
and the five crates underneath speak cross-reference tables and glyph runs,
which is exactly what §11.1 says must not reach a caller.

`documentKind` is the piece that had to be invented rather than exposed. Nothing
below classified a scan, because nothing below needed to. The rule is that an
image covers most of the page **and** nothing visible is written on it, and both
halves earn their place: coverage alone calls a full-bleed magazine page a scan,
and absence of text alone calls a page of diagrams one.

The subtlety is invisible text. Every OCR tool lays its output over the scan in
rendering mode 3, so counting those glyphs classifies every OCR'd scan as
born-digital — backwards, since those are the scans most likely to reach an
editor and the ones where an edit changes an invisible layer and no pixels.

### Where the `async` actually is

§11.1 says "everything is `async` at the JS boundary because everything crosses
a Worker", and it is worth being exact, because the obvious reading builds the
wrong thing. Nothing in the WASM crate is async. The work is CPU-bound Rust with
nothing to await, so an `async fn` would resolve on the same tick having blocked
for exactly as long. The asynchrony comes from the **Worker** — the main thread
is free because the work is on another thread, not because a function returned a
promise. Synchronous inside the Worker, asynchronous outside it.

### Plain objects, not bound structs

A `#[wasm_bindgen]` struct gives JS a class whose every field is a getter, and
every getter is a boundary crossing: a paragraph's seven fields cost seven, and
a page of forty paragraphs costs two hundred and eighty. Building the object
once in Rust costs one.

### Measured

| | Size |
|---|---|
| Raw `.wasm`, after `wasm-opt -Oz` | 807 KB |
| **Gzipped** | **336 KB** |
| Brotli | 266 KB |
| §12.3's `core` budget | 900 KB — **37% used** |

The size probes stay: they measure the *floor*, which is what catches a
regression in a layer rather than in the surface. The shipped artefact is
measured separately because it is the number the budget is actually about.

### Driven from node, because a Rust test cannot

Everything on the WASM surface returns a `JsValue`, and constructing one traps
on a host target — so `cargo test` cannot reach any of it, and a binding that
compiles and cannot open a file would pass every test in the crate.
`harness/wasm-size/api.mjs` builds the real module, loads it in node, and opens
a corpus PDF, reads its paragraphs, edits one, saves, reopens the saved bytes
and checks the text changed — plus that an unedited save is byte-identical, that
errors arrive as `Error` objects carrying §11.5's `code`, and that a closed
handle stays closed.

It asserts its own preconditions, which is the point: the first run picked a
fixture with no text, skipped every edit check, and printed a clean result. It
now fails if the sample has nothing to edit.

## §12.2, §12.4 — The Worker and the package

| Item | State |
|---|---|
| §12.2 Worker by default, main thread never blocks | yes |
| §12.2 transfer `ArrayBuffer`s rather than copying | yes, both directions |
| §12.2 `{ worker: false }` for callers managing their own | yes |
| §12.4 ESM primary, CJS shim | yes |
| §12.4 hand-written declarations, no `any` | yes, `tsc --noEmit` gates it |
| §12.4 `.wasm` as an asset, `wasmUrl` override | yes |
| §12.4 no postinstall, no native build step | yes, checked by installing |
| §11.3 `registerFont`, with injection on demand | yes |
| §11.4 sessions spanning several operations, undo/redo/rollback | yes |
| §11.2 images, pages, annotations, forms, tables | yes |
| §9.6 redaction and its verification | yes |
| §5 encryption and password change | yes |

### The catalogue crosses whole, or it is not a surface

For a while the facade and the package exposed text editing and nothing else,
while `rasura-edit` carried the rest. That was not a design position — a
caller could reach everything through `Document::raw_mut` and `set_objects`,
which is how pages and annotations are applied underneath — but "the capability
exists one layer down" is not an answer to somebody who wants to move an image.

Everything now stages through one session and one undo stack, which is the part
worth checking rather than asserting: `js/test/catalogue.test.mjs` stages a text
edit and an image move together, undoes both, commits, and compares the result
byte-for-byte with the input. Two subsystems staging into two logs would make
"undo" ambiguous, and that test is what says they do not.

Two operations stop at the Rust facade on purpose. `insert_page` produces a
blank page and there is no way to draw on it from JavaScript, so exposing it
would ship the half that cannot be used. Arbitrary object writes stay in Rust
because §11.1's second principle is that no PDF concepts leak by default — a
`setObject(number, generation, dict)` on the JS surface would make every §11.5
error code a guess about what the caller meant.

### The module has no RNG, and that is the interface

Encryption needed one decision before it could cross. Key derivation needs 32
random bytes, and a WASM module can get them three ways: bundle a PRNG (seeded
with what?), call out to the host through a shim, or take them as an argument.

It takes them as an argument. `protect()` accepts `Entropy`, and the JS wrapper
fills it from `crypto.getRandomValues` — the platform's own CSPRNG, in the
layer that has one. There is deliberately no `Math.random()` fallback: an
environment without a CSPRNG must fail loudly rather than quietly produce a key
somebody can reproduce. The visible consequence is that the provenance of the
key material is at the call site instead of buried in whichever target the
crate was compiled for.

`Entropy::new` rejects 32 identical bytes and a counting sequence. That catches
the two mistakes that actually happen — a zeroed buffer and `0..32` typed while
testing — and cannot catch a caller with a poor RNG. It is a guard against
accident, not against determination, and it is documented as such.

### Structured clone drops an Error's own properties

The trap the Worker boundary sets, and it is silent. `postMessage` serialises an
`Error`'s name, message and stack — and **not** its own properties. Throw a
`PdfError` from inside the Worker and what arrives is an `Error` with the right
message and `code === undefined`, so every `if (e.code === '…')` a caller wrote
takes the wrong branch and nothing reports a failure.

Errors are therefore never thrown across the boundary. `toWire` converts them to
plain objects, they travel as ordinary data, and `fromWire` rebuilds a real
`PdfError` on the other side. A test asserts the code survives, on both
transports.

The Worker body never throws either. An exception escaping a Worker fires
`onerror`, which carries a message and no way to match it to the call that
caused it — so a caller awaiting request 7 would wait for ever while an
unrelated handler reported that something went wrong.

### Two bugs the test runner found by hanging

Neither showed up as a failed assertion.

**A failed `open` leaked its Worker.** `Pdf.open` starts a Worker and then
throws if the bytes are not a PDF, so the thread had no owner and no way to be
stopped. In node that is a script which never exits; in a browser it is a thread
per malformed file, for the life of the page. The regression test counts live
workers before and after three failed opens.

**`terminate()` was not awaited.** node's returns a promise and the thread
outlives it, so a caller who closed every document and expected their script to
end would find it hanging with nothing to point at.

Both were found because `node --test` stopped after printing fourteen passes —
which is a good argument for running a suite to completion rather than reading
its assertions.

### Transferring detaches the caller's buffer

§12.2 asks for transfer rather than copy, and the cost is real enough to be
documented rather than discovered: after `open`, the `ArrayBuffer` the caller
passed has zero length. That is the point — a 20 MB document is not duplicated —
and `{ transfer: false }` exists for when the same bytes are needed twice. A
`Blob` is never transferred, since its bytes are read out here and the caller
still owns it.

Results going the other way are always transferred: the module produced those
bytes and nobody else holds them, so there is no hazard to weigh.

### The declarations are compiled, not just written

§12.4 asks for "full TypeScript declarations, hand-checked, no `any` in the
public surface". `wasm-bindgen` generates declarations where every structured
return is `any` — which is not a defect in the generator, it is why the
specification asks for hand-written ones.

`js/types/index.d.ts` is written against the shapes `convert.rs` builds, and
`tsc --noEmit` under `strict` compiles a file that uses the whole API *and*
contains six deliberate mistakes behind `@ts-expect-error`. If a return type
ever widens to `any`, those mistakes stop being errors and the build fails —
which is what keeps the check from going vacuous.

### A session that outlives the call that made it

`EditSession` holds `&mut Document`, and a borrow cannot be parked — so a handle
table, an FFI boundary or a server holding documents in a map cannot store one,
and Rust has no way to express it without `unsafe` or a self-referential-struct
crate. Every edit therefore committed immediately, which is not a session.

The state, though, is all owned: the op log, the redo stack, the
already-dirty flag. Only the document is borrowed. So `SessionState` is that
half by itself, `suspend()` takes it out and `resume()` puts it back, and the
WASM layer takes-and-puts-back around every operation. The log survives
verbatim, so **I5 does not know a suspension happened** — a test asserts an undo
after resuming restores the same bytes an undo before suspending would have.

The state is parked again even when an operation *fails*, because a caller who
made four edits and then typed something unencodable must not lose the four.

Document and session live in one slot rather than two maps keyed by the same
handle: a `SessionState` is only meaningful against the document it came from —
an object id means nothing outside the document that issued it — so pairing them
makes resuming against the wrong file impossible rather than merely unlikely.

### The encoder cannot tell you a glyph is missing

`registerFont` was first wired to inject when `replace_text` returned
`Unencodable`, and on the case the feature exists for it **never fired**.

The encoder inverts the *decoder*: it knows which code produces which character,
because that is what `/Encoding` says. A simple font with `/WinAnsiEncoding`
therefore "can write" every character in Latin-1 — every one of them has a code.
Whether the embedded program holds an outline at that code is a different
question, and nothing was asking it.

So a document embedding seven letters of Roboto accepted `É` happily, wrote code
0xC9, and drew nothing. Exactly the silent failure §8.4 exists to prevent, found
by a test asserting the *injection* had happened rather than that the edit had
succeeded.

The question is now put to the font program — resolve the character through the
embedded `cmap` and see whether a glyph comes back — and the same mistake turned
out to be in `fontRequirements`, which measured coverage by asking whether a
width existed. It reported the seven-letter subset as covering all of Latin and
needing nothing supplied, which would have made the feature undiscoverable by
the very call §11.3 says to lead with.

### A missing glyph is a field, not a failure

§11.4's own example puts `missingGlyphs` beside `fidelity` rather than inside
it, so an unsupplied character does not fail the edit — the text is written, the
report names the character, and `requireFidelity` above the bottom rung refuses.
Being stricter by default would have broken every edit to a document with a
symbolic font whose `cmap` this layer reads differently from its producer, and
the corpus is the reason to care: 1,030 files still green.

### `npm i rasura`, done rather than intended

Everything else tests the working tree. An `exports` map missing an entry, a
`files` list omitting the `.wasm`, a path that only resolves inside the source
tree — all pass every test in the package and fail the first person to install
it. So CI packs the tarball, installs it into an empty directory **with
`--ignore-scripts`**, and runs a consumer that opens a PDF, reads its
paragraphs, edits one, reopens the saved bytes and checks the text changed.

Tarball: 360 KB, of which 827 KB unpacked is the `.wasm`.

---

## Not started

**Phase 8 is complete except for one item that cannot be started.** Its five
buildable pieces — encryption creation, Knuth–Plass, optional content, vertical
writing, compaction subsetting — are done. The sixth is `rasura/threaded`,
a *separate entry point to a WASM package*, and §12's packaging does not exist
yet; it is listed as blocked rather than pending, because there is nothing to
add it to.

Everything else below is unscheduled, and saying so is more useful than
assigning it a number the plan does not contain.

| Spec | What | Phase |
|---|---|---|
| §12.1 | Threaded build — a second artefact with `SharedArrayBuffer` | 8, named |
| §11.6 | The draw-command emitter's JS surface | next |
| §12.3 | Lazy chunk splitting — `fonts` loading on first shaping edit | next |
| §13 | Performance budgets, and the regression gate | unscheduled |
| §16 | Documentation deliverables | unscheduled |
| §9.2 | `set_style`, `insert_paragraph`, `set_z_order` | unscheduled |
| §9.2 | The five structural table operations | needs a declared structure; see §9.2 above |
| §10.4 | `resample_image` — the only piece needing a pixel codec | unscheduled |
| §10.5 | Vector path provenance, and `move_block` on it | unscheduled |
| §10.6 | Redaction steps 2 and 6 — image data, font subsetting | unscheduled, reported on every redaction |

§12.3's bundle budget is the exception: it is already gated in CI against the
compiled `wasm32` artefact, because a size ceiling discovered late is a rewrite.

## Refused, permanently

Per spec §3, these are rejected at the API boundary rather than half-built:

| Not doing | Behaviour |
|---|---|
| Scanned / image-only PDFs | `documentKind === 'scanned'`, `paragraphs()` empty |
| XFA forms | Detected, `hasXfa` exposed, form edits refused |
| Rendering as the primary product | Optional draw-command emitter only |
| Digital signature creation | Detect, preserve, report invalidation. Never create |
| Server-side rendering farm | Crates are runtime-agnostic; not our product |
