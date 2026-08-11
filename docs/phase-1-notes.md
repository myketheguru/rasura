# Phase 1 notes

Decisions taken while building `rasura-cos`, deviations from `spec.md`, and
what Phase 2 has to pick up.

---

## How invariant I1 is actually achieved

The spec asks that `open(bytes)` then `save()` with zero edits produce
byte-identical output. There are two ways to get there and only one of them is
honest.

The tempting way is to make the serialiser so faithful that re-emitting every
object reproduces its input. That fails on the first file that writes `72.0`
where you write `72`, or `/Type/Page` where you write `/Type /Page`.

The way taken here: **an incremental save with nothing dirty appends nothing.**
The output is the input. I1 holds by construction, not by care.

That makes I1 nearly vacuous as a *parser* test, which is why the invariant
suite carries a second check the spec does not name — `object round-trip`. It
walks every object, serialises it, re-parses it, and requires the two to agree
byte for byte. A parser that quietly normalised `/N#61me` to `/Name`, or dropped
a producer's `\101` escape, passes I1 and fails there. That check is the reason
`Name` and `PdfString` carry their raw bytes at all.

The third leg is the writer replaying source spans: an unmodified object in a
*full rewrite* is emitted from the bytes it originally occupied, so a rewrite
compacts the file without churning the formatting of objects nobody touched.

## Deviations from the spec

**RC4 is implemented inline rather than taken from the `rc4` crate.** Spec §4.3
lists `rc4` among the approved dependencies. PDF needs keys of every length from
5 to 16 bytes, and the RustCrypto `rc4` crate makes key length a compile-time
type parameter, which turns "decrypt with an n-byte key" into a twelve-arm
dispatch over typenum sizes. The algorithm is twenty lines, has no
constant-time requirement that matters for a cipher we support only to *read*
legacy files, and is pinned to published test vectors. The dependency was
dropped from `Cargo.toml` accordingly. **This is the one place the built
artefact disagrees with the spec's dependency table.**

**`Object::Real` does not preserve the producer's numeric spelling.** The spec's
byte-preservation requirement (§5.1) names `PdfString` and `Name` explicitly and
does not mention numbers. Preserving them would mean threading a raw-bytes
field through every arithmetic site. Instead, unmodified objects are re-emitted
from their source span, so a producer's `4.` survives a save untouched. See the
coverage matrix for when this could become observable.

**`/ID[1]` is derived, not random.** There is no RNG in this crate and none on
`wasm32-unknown-unknown` by default. `/ID[1]` is a SHA-256 over the revision's
own tail, which is unique per distinct output — what the identifier is actually
for — and has the useful side effect that saving the same edits twice produces
identical bytes, so the test suite can assert on output.

## What the pdf.js corpus found

Running the suite over mozilla/pdf.js's 974 committed test files — two decades
of cases kept precisely because they broke something — took the pass rate from
**949/990 to 989/992** across four rounds. Five defects, none of which any
hand-written fixture had reached.

**1. A reference to a missing object is not an error.** ISO 32000-1 §7.3.10:
"An indirect reference to an undefined object shall be considered a reference to
the null object." `resolve` was returning `MissingObject`. Because a full
rewrite legitimately drops unreachable objects, this also meant a rewrite could
produce a file this library then refused to open.

**2. A synthesised object must never be served from the input's bytes.** A full
rewrite drops unreachable objects, so the new cross-reference stream's freshly
allocated number can collide with an input object that is not being written. The
writer replayed *that* object's source span, emitting the old `/ObjStm` under the
xref stream's number. The file's `startxref` then pointed at an object stream,
and it reopened only through recovery. The verbatim-replay optimisation now
applies only to objects that came from the input; synthesised objects go through
a separate entry point that cannot reach it.

**3. The writer had no matching exemption for unencrypted streams.** The read
side already knew that a `/Type /XRef` stream is never encrypted — it has to be
readable before the file key exists. The write side did not. When the trailer
lives inside an xref stream, its `/ID` goes with it, and encrypting those strings
changes the input to the key derivation: the file rejected its own password on
reopen. Four real encrypted documents failed this way, and no fixture could have
caught it, because the fixtures were classic-xref. `encrypted_xref_stream` now
covers the combination.

**4. A wrong cross-reference offset is recoverable.** Files edited by tools that
did not fix up their offsets are common, and every viewer finds the object
anyway. The reader now builds a whole-file index of `N G obj` headers on the
first bad entry and repairs from it, recording `XrefOffsetMismatch`. One pass per
file, not one per bad entry.

**5. An object stream's index is a hint, not a contract.** A wrong or
out-of-range index is repaired by looking the object number up in the container's
own header.

Two harness bugs surfaced alongside them, and they mattered as much:

- **I3 was checking the input.** The invariant says "*output* passes `qpdf
  --check`". Checking the input conflates a defect Rasura introduced with one
  the file arrived carrying — and this corpus is full of the latter by design.
  Eighteen failures were the suite blaming the library for fuzzed input.
  Input defects are now reported as diagnostics that do not fail the run.
- **Password-protected files were counted as failures.** Seven of the thirteen
  encrypted failures were files whose passwords are declared in pdf.js's own test
  manifest (`'Hello'`, `'abc'`, and three SASLprep cases). Refusing a file whose
  password you were not given is correct behaviour. Counting it as a defect
  applies pressure towards opening things the library cannot actually decrypt.

Three files remain red, and all three are correct declines: two have no
`/Type /Catalog` anywhere in the file, and one has an `/Encrypt` that does not
resolve to a dictionary — so whether its content is protected cannot be
determined, and guessing "not encrypted" would hand back ciphertext dressed as
content. They are listed in `expected_decline` with reasons, so anything *new*
that starts failing is still loud.

## A bug the generated fixtures found

The `bytes-before-header` adversarial fixture (an HTTP preamble before `%PDF-`)
failed I3 on object 2. Offsets in a PDF are measured from the header, so with an
N-byte preamble every recorded offset is short by N. The original fallback tried
the raw offset, asked "is there an object here?", and used it if so.

With a 50-byte preamble, object 2's raw offset landed exactly on object 1's
header. The check said yes. The document then silently served object 1 whenever
object 2 was asked for — a corruption with no symptom the caller could detect.

The fix is to check the candidate against the object number *being looked for*,
not merely for the presence of some object. The same weakness existed in
`looks_like_xref_start`, which accepted any two integers as a possible
cross-reference stream header — and the body of a classic xref table is nothing
but pairs of integers. It now requires the `obj` keyword.

This is the argument for adversarial fixtures being generated code rather than a
wish-list in a document: neither bug was reachable from a well-formed file.

## Things deliberately left for later

- **Streams are decrypted on every `decoded_stream` call for a cache miss, but
  the decrypted-but-still-filtered form is not cached separately.** Only the
  fully decoded form is. That is the right trade for content streams and the
  wrong one for a large image the caller decodes twice; revisit when
  `rasura-edit` starts touching images.

- **`Document` is `Send` but not `Sync`.** The caches are `RefCell`. A document
  can move to a worker thread; it cannot be shared across threads without
  external synchronisation. Given the Worker-per-document model in spec §12.2
  this is the right shape, but a server-side consumer may want otherwise.

- **The LRU cap on the page-model cache (spec §12.5) is not implemented.** There
  is no page model yet. `memory_usage()` exists and reports honestly; the cap
  belongs with the thing it is capping.

- **No benchmarks.** Spec §13's budgets are all about operations that do not
  exist yet (`paragraphs()`, `replaceText`). The two that could be measured now
  — `open()` to first page metadata, and incremental `save()` — need a 500-page,
  20 MB corpus file to be meaningful, and the corpus has none.

## What Phase 2 needs from this layer

`rasura-content` will need, and these are already in place:

- `Document::decoded_stream(id)` for content streams, with the filter chain
  resolved and encryption removed.
- `Document::get_entry(dict, key)` for resolving inherited page attributes.
- `Stream::set_decoded` to hand back patched content, with the original filter
  chain re-applied on save.
- `Object` spans are *not* provided for content-stream operators — that is
  §6.2's job and belongs in the content crate, over the decoded buffer this
  layer hands it.

One thing it will need that does not exist: `/Contents` as an array of streams
has to concatenate into one logical buffer while retaining the mapping back to
(stream index, offset), per §6.4. That mapping is content-layer state, but it
depends on this layer keeping each member stream individually addressable —
which it does, since they are ordinary indirect objects.

## Open questions this phase did not answer

Spec §18 lists six.

- **Q1 (`/ToUnicode` coverage) is answered.** 53.0% across 1390 embedded fonts,
  decisively below the 85% threshold. Full write-up in
  [q1-tounicode-coverage.md](q1-tounicode-coverage.md); the short version is that
  the spec's threshold rule fires, but its diagnosis is wrong. The failure is not
  LaTeX subset fonts with `g34` glyph names — modern pdfTeX emits `/ToUnicode`
  for everything, and only **six fonts in 1390** carry opaque names at all. The
  load-bearing component is the **Adobe Glyph List** (300 of 653 failures resolve
  through it and nothing else), which §7.2 mentions only in passing. Step 5
  (reverse `cmap` lookup) is unavoidable Phase 3 work; step 6 is worth 0.4%; a
  shape-matching fallback is not justified by any evidence.

- **Q6 (bundle floor) is answered.** `rasura-cos` is 123 KB gzipped, 13.6%
  of the 900 KB `core` budget, leaving 777 KB for content and layout. The module
  split in §12.3 stands and the layout engine does not need to become a third
  lazy chunk. Write-up in [q6-bundle-floor.md](q6-bundle-floor.md). The probe
  also runs in node against the fixture corpus, which is the first evidence that
  the object layer works on `wasm32` at all — nothing in it needs a filesystem,
  a clock, or randomness.
- **Q5 (subsetter choice)** needs the font crate.
- **Q2, Q3, Q4** need the layout engine.

## On the value of the corpus

Worth recording plainly, because it changes how the remaining phases should be
sequenced: the generated fixtures — sixteen files written specifically to pin
down structural cases, including ten deliberately adversarial ones — found
**one** bug. Fetching someone else's corpus found **five**, in an afternoon, in
code that already passed 109 tests.

The fixtures are not wasted; they are what makes a failure legible, and two of
the five fixes are now pinned by fixtures written afterwards
(`encrypted_xref_stream`, and the synthesised-xref-stream regression test). But
the ratio argues for acquiring real files *before* writing each subsequent
layer, not after. Phase 2 should start by getting LaTeX and Word output.
