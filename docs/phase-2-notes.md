# Phase 2 notes

Decisions taken while building `rasura-content`, and what Phase 3 inherits.

---

## The exit criterion, and what it should not have been

Spec §17: *"Exit: text extraction with correct positions across the corpus."*

"Correct" needs an oracle. pdf.js is the best one available — two decades of work
against the same files, and already on disk from `corpus/fetch.sh` — so
`harness/textdiff` extracts with both and compares.

The obvious gate would be "95% of pages must agree on text". That gate is wrong,
and it took building it to see why: Phase 2 implements §7.2 **strategy 1 of
seven**. A page whose fonts have no `/ToUnicode` yields nothing here and readable
text from pdf.js, which implements the whole chain. Q1 already measured how much
of the corpus that is — 53% of embedded fonts have a usable `/ToUnicode` — so a
text-agreement gate would mostly be measuring Phase 3's absence, and "fixing" it
would mean guessing at Unicode, which spec §2 forbids.

The gate that means something:

> **Zero pages where every glyph mapped and text still went missing**, and glyph
> positions agreeing wherever both sides saw the same glyphs.

That isolates what Phase 2 actually owes. On the corpus: zero such pages, and
positions matching pdf.js to floating-point noise.

## What the differential found

Three rounds of the harness being wrong before it was right, which is worth
recording because each error would have produced a confident wrong number.

**The metric compared the wrong things.** The first version compared the *first*
glyph on each side. pdf.js groups by text item and this library groups by
showing operator, so "first" means different things — it was measuring the
grouping, not the geometry. A bounding box over glyph origins is invariant to
both.

**Then it compared the wrong corner.** Bounding-box *maxima* are not comparable
either: pdf.js reports one origin per text item, which is where the item
*starts*, so its maximum is the last item's start and ours is the last glyph's.
They differ by about the width of a run. Only the minima are the same quantity.
That correction took the mean from 113 pt to 5.2 pt.

**Then the reference conversion was wrong.** The remaining 5.2 pt was a real bug,
in the harness: pdf.js reports PDF user space with the origin at the media box,
while Rasura's device space has its origin at the **crop box** corner. Using
only the page height to flip `y` silently biased every page whose crop box does
not start at (0, 0). Converting through both corners took the mean to **0.000 pt
and zero pages off**.

The lesson is the one from Phase 1 restated: a harness that has not been checked
against a case whose answer you know independently is a source of confident
numbers, not of confidence. The 0.000 pt result is more trustworthy than the
5.2 pt one *because* it is implausibly good — it says the two implementations
agree exactly, which is what should happen when both are right.

## The bug at the bottom of the last category

The gate passed with one category left unexplained: five pages where pdf.js
found text, this library found none, and there were no unmapped glyphs to blame.
Chasing the one file the report named found a real defect.

`issue11922_reduced.pdf` maps its codes to **empty** `/ToUnicode` destinations —
`<>`. Ten glyphs extracted, ten "mapped", no text. An empty string counted as a
successful mapping is the worst of both answers: the page reports as fully
mapped *and* produces nothing, so the diagnostics say there is no problem while
there plainly is.

`CMap::unicode` now reports an empty destination as no mapping, which moves the
page into the `/ToUnicode` gap where it belongs and makes `unmapped_glyphs`
mean what it claims. Four pages remain in that category — 0.4% — and they
extract no glyphs at all; that is the next thing to look at, not something this
phase resolved.

## A bug found by walking the corpus

`harness/contentwalk` runs every page through the content layer. On the first
run it flagged two files, and chasing them found something much worse than a
page-tree bug — in code that had been green through all of Phase 1.

**`flate_decode` was accepting noise.** Arbitrary bytes frequently begin a valid
*raw* deflate block: `0x2b` is BFINAL with a fixed-Huffman block, and inflating
random data from there yields hundreds of bytes of plausible-looking garbage. A
stream that failed to *decrypt* came back as content rather than as an error.
Text extraction would have produced nonsense with nothing to indicate anything
was wrong — the silent degradation §2 forbids.

The first fix — require the stream to reach its end marker — was **not enough**,
and the test caught it: a fixed-Huffman block decoded from noise hits an
end-of-block symbol quickly and reports a clean finish having consumed a handful
of bytes. The rule that works needs both conditions: reach the end **and**
consume essentially all the input. A stream *with* a valid zlib header may still
be truncated and keep what it got, because that damage is real and common.

Two things worth being plain about. The only reason this surfaced is that a
"successful" decode returned 3 bytes for an object stream, which is absurd on its
face. And the invariant suite never caught it because I1 does not decode streams
— 1024 files green, every commit, past a bug that corrupted content.

## Layering decisions

**CMaps live in the content layer, not layout.** `/Encoding` CMaps define
codespace ranges, and without those you cannot split a string into character
codes at all — positioning depends on it. `/ToUnicode` shares the syntax, so it
is parsed here too. What stays in layout is §7.2's *chain*: parsing a CMap is
mechanical, deciding what to do when it is missing or wrong is reconstruction.

**Font metrics come from the font dictionary, never the font program.**
`/Widths`, `/W`, `/DW` and `/FontMatrix` are enough to position every glyph, so
Phase 4 is not a prerequisite for Phase 2. The one case that genuinely needs the
font engine — a non-embedded standard-14 font with no `/Widths` — reports
`missing_widths` and falls back to half an em, rather than returning zero and
stacking every glyph on the same spot.

**Matrices are `f64`, not `f32`.** Spec §7.1 uses `f32` for glyph positions, and
that is fine at the boundary. Internally, a page CTM composed with a form
`/Matrix` composed with `Tm` composed with the text-space scale drifts in `f32`
by more than the quarter-pixel bound §14.3 asks the pixel-diff harness to catch.
Conversion happens once, at the layout boundary.

## Things deliberately left

- **Adobe collection CMaps** (`UniJIS-UCS2-H` and the rest) are approximated as
  two-byte identity. That positions correctly, because those collections are
  two-byte in practice, but produces wrong CIDs. `approximate_cmap` is set so
  nothing downstream can mistake it for exact. Closing it means vendoring the
  collection data files.
- **`usecmap`** is detected and not followed.
- **Type 3 glyph procedures and tiling patterns** are reachable through
  `walk_stream`, which exists and is tested, but nothing drives it yet. They are
  content streams and go through the same machinery when Phase 3 needs them.
- **Bidi.** Arabic extracts in visual order; pdf.js produces logical order. Both
  are defensible for a *content-order* extractor, and reading order is §7.4.

## What Phase 3 inherits

Everything §7 needs from below it is in place:

- `GlyphRun`/`PositionedGlyph` with device-space origins, advances, per-glyph
  byte spans, `/MCID`, render mode and source object.
- `CMap`, ready to be strategy 1 of the seven-step chain.
- The `ContentVisitor` interface, so reconstruction is another visitor rather
  than a second traversal.
- `LogicalContent::locate_span`, which already reports every source object a
  span crosses — the thing the edit layer will need in Phase 5.

The highest-value item in §7.2 is not what the spec emphasises. Q1 established
that the **Adobe Glyph List** carries the load: 300 of 653 failing fonts resolve
through it and nothing else, while only six fonts in 1390 carry the opaque
`g34`-style names the spec built its remedy around. Build the AGL properly and
most of the gap this phase leaves closes with it.
