# The flow model: surgical and document modes

**Status: built, all five steps.** A document is reconstructed as flowing
content (`rasura-flow`), exported, or laid out into frames inferred from its own
geometry (`rasura_layout::frames`, `rasura_flow::layout`) and written back as a
PDF (`rasura_flow::emit`). I8 holds at every stage.

Document mode round-trips **698 of 820** corpus documents through a written,
re-opened PDF; of the rest, 100 lost characters WinAnsi cannot hold and 19 are
unexplained and named. It is behind `Options::accept_regeneration`, and that flag
is the boundary this document has been careful about throughout: on one side, §2's
first property holds absolutely and an edit changes no byte it did not need to;
on the other, page content is regenerated and it cannot. Nothing reaches the
second side without a caller writing the word.

Measured over the 958 pdf.js corpus documents that open:

| | |
|---|---|
| Tagged (`/StructTreeRoot` present) | 88 (9%) |
| Reading order taken from structure rather than geometry | 66 (7%) |
| Converted with no inference at all | 36 (4%) |
| Blocks in → out | 22,528 → 17,191 |
| Exported to nothing | 69 (7%), down from 154 before step 2 |

Every one of those 69 has an attributed cause: 62 are unclassified blocks whose
glyphs map to no characters, 3 are pages carrying nothing but rules, 2 are
paragraphs that resolved to no text, and 2 are mixed. None of them is a block
that vanished — `Report::accounts_for_everything` holds on all 958.

The headline number is the 9% tagged, and it is the one that should govern what
gets built next. This document already says to serve tagged documents first;
the corpus says that population is one document in eleven.

---

## The problem

A PDF is the *output* of a layout process, not an input to one. Every glyph
carries an absolute position; the block on page 4 is not positioned relative to
anything on page 3. The flow that a word processor used — this paragraph
follows that one, and if the first grows the second moves — was discarded when
the file was written.

So "add a paragraph and let the document reflow" is not a feature that was
switched off. There is nothing to switch on. The information is gone, and any
implementation has to *reconstruct* it and then be honest about the difference
between what it recovered and what was there.

That is why spec §9.3 scopes reflow to the paragraph, and why the current
engine's answer to a paragraph outgrowing its measure is to re-break it in place
and report `Overflowed` — the block below is positioned absolutely and does not
move out of the way.

---

## Two modes, one stack

The resolution is not to pick between fidelity and flow. It is to offer both,
under clearly different contracts, sharing everything below the edit layer.

| | **Surgical** (built) | **Document** (built, behind a flag) |
|---|---|---|
| Unit of change | byte spans in a content stream | the reconstructed model |
| Output | original bytes plus a patch | regenerated page content |
| §2 property 1 | **holds** — untouched objects are byte-identical | **cannot hold** for re-laid-out pages |
| Failure mode | refuses what it cannot do exactly | reports what it changed and how confident it is |
| Good for | correcting text, moving an image, redaction | restructuring, adding content, export |

`cos`, `content`, `layout` and `font` are common to both. Document mode adds
four components and changes no existing one:

1. a **flow model** — content as a sequence rather than as positions; built, in
   `rasura-flow`;
2. a **frame inferrer** — where content is allowed to go; built, in
   `rasura_layout::frames`;
3. a **layout engine** — model plus frames to pages; built, in
   `rasura_flow::layout`;
4. an **emitter** — placed pages back to a PDF; built, in `rasura_flow::emit`,
   and the only one of the four that changes what a save means.

That is the whole architectural claim: document mode is additive. If it turns
out to be a bad idea, nothing built for surgical mode was spent on it.

The claim held all the way through. `rasura-flow` depends on `cos`,
`content`, `layout`, `font` and `edit`, and the only changes any of them
needed were additive: a `model::analyse` convenience function, a clip on the
graphics state, path provenance on the vector collector, an annotation reader
moved down a layer, and `lookup_id` on the resource stack. Not one existing
behaviour was altered to make document mode possible.

---

## What already exists

Phase 3 reconstructs more of a word-processor document than is obvious:

- paragraphs, with alignment, leading, first-line indent, and left/right margins
- style runs — font, size, colour, render mode, rise
- tables, from drawn grids or from column alignment, with cells
- running headers and footers, and footnotes linked to their in-text markers
- images and vector blocks, with images retaining their full transform
- reading order, from `/StructTreeRoot` where the producer supplied one

That is most of a `.docx` already. It is also the ceiling on quality: document
mode can be no better than the reconstruction it is given.

## What is missing

### 1. Frame geometry — **built**, `rasura_layout::frames`

Where text is *allowed* to go — column boxes, margins, text frames. Not in the
file at any level, so it must be inferred.

Measured over the 628 pdf.js corpus documents that have text to measure:

| | |
|---|---|
| Mean containment | 99.6% |
| Tightness, median / mean | 1.01 / 7.29 |
| Documents with more than one column | 52 (8%) |
| Documents with only one page of evidence | 568 (90%) |
| Page groups that fell back to the page box | 2 |

Containment and tightness pull in opposite directions on purpose: one frame the
size of the page contains everything and says nothing, and a frame drawn tightly
round one paragraph scores the reverse. A median tightness of 1.01 with 99.6%
containment means the frames are, typically, the content — which is what a text
frame should be.

The mean tightness of 7.29 against a median of 1.01 is not noise to be smoothed
away: it is a handful of documents with a page-sized frame around a few glyphs,
and reporting only the mean hid that. The first run of this measurement reported
a mean of 142 and no median at all.

**Two corrections the corpus forced**, both found by measurement rather than by
reading the code:

- The page box is *not* "an outer bound that is always true", as this document
  claimed above. `endchar.pdf` has a 15×34pt crop box with its text 260 points
  to the left of it. Clamping the histogram to the box put every block outside
  every frame, and eleven files scored 0% containment for that reason alone. The
  domain is now the page box **union** the content.
- A minimum column width, meant to stop a stray glyph becoming a column, was
  discarding real content: three of `issue925.pdf`'s four blocks landed in no
  frame. Folding a narrow run into its nearer neighbour instead took containment
  from 97.4% to 99.6%, cut mean tightness from 142 to 7.3, reduced the documents
  below 90% containment from 41 to 6 — and *raised* multi-column detection.
  The filter was costing accuracy rather than buying it.

Six documents remain below 90%. That is where the next work is, and they are
named by `--frames` rather than averaged away.

The useful observation is that **frames are a document-level property, not a
page-level one**. A single page gives one sample of where a column's text
happened to fall; twenty pages of the same column give twenty, and their union
converges on the frame the producer actually used. Inferring per page is the
obvious approach and the wrong one.

Signals available, in descending order of reliability:

1. `/StructTreeRoot` — where present, the producer named the containers.
2. Repeated block extents across pages, clustered — the union of a column's
   left and right edges over many pages.
3. `/MediaBox` and `/CropBox` — ~~an outer bound that is always true~~. It is
   not: see the corrections above. Used as the *starting* domain and as the
   fallback when a page group has no text, never as a clamp.
4. Ruling lines and table grids — hard boundaries content did not cross. Still
   unused; `rules::collect` produces them.

### 2. A layout engine — **built**, `rasura_flow::layout`

Model plus frames to pages. Well-understood work — it is what a browser or TeX
does — and the one component with no PDF-specific difficulty. Greedy breaking
already exists in `reflow`; what is missing is the box model above it: frames,
float placement for images, keep-with-next, widow and orphan control.

The assessment held. What is there: greedy breaking to a frame's measure,
filling frames left to right and then overflowing to a new page, paragraphs
split across column breaks with widow and orphan control, keep-with-next for
headings, and blocks that cannot be split placed whole or moved. Line breaking
is parameterised on a `Measurer`; the default uses the standard-14 metrics the
font crate already ships.

**It does not write a PDF**, and that separation is deliberate: the moment
content is regenerated §2's first property stops holding, and that is a contract
change a caller opts into rather than discovers. It also means the engine can be
checked without writing a file — which is how I8 closes:

```text
flow ──layout──▶ placed pages ──to_flow──▶ flow'
  └────────────── compare ────────────────┘
```

| | |
|---|---|
| Model survives layout | **820 / 820** |
| Median pagination after layout | 1.00× the original |
| Survives **document mode** — laid out, written, re-opened | **698 / 820** (85.4%) |
| Written files that would not re-open | **0** |

Of the 119 that differ, **100 lost characters WinAnsi cannot hold** — 7,161 of
them across the corpus, in Arabic, CJK and symbol-font documents. That is the
single-font limitation doing exactly what it says, and it is counted rather than
discovered: `Report::unencodable` is the number, and a caller who sees it
non-zero knows the output is not the input.

**19 differ for another reason**, and those are the real defects. They are named
by `--i8` rather than averaged into the 85%, because a percentage that hides
nineteen unexplained files is a percentage doing the opposite of its job.

I8 earned its keep on the first corpus run here too: 81 documents failed, every
one of them carrying annotation text. The engine skipped notes — correctly, since
a note is drawn by the viewer from an annotation dictionary and has no place in
the page flow — and then never carried them through, so they vanished. Skipping
something and losing it are different, and only the round trip could tell.

Still to do: float placement for images (a figure occupies a whole block rather
than being flowed around), and per-run measurement — the engine measures a block
in one style rather than following its emphasis runs.

### 3. Style consolidation

Per-run font-and-size into named styles, so that "make every heading larger" is
one operation. Clustering on `(base_font, size, colour, weight)` across the
document recovers most of it; the structure tree names them outright where
tagging exists.

---

## The honest risks

**Reconstruction is heuristic, and regeneration compounds it.** Reading order
scores 89.8% concordant against the corpus's tagged documents. Re-laying-out
from a model that is a tenth wrong produces a document reordered in ways nobody
asked for — and unlike a bad extraction, a bad regeneration is what the user
now has.

Two consequences follow, and they are design constraints rather than caveats:

- **Tagged documents first.** Where `/StructTreeRoot` exists the producer wrote
  the logical structure down and the guessing largely disappears. 87 of the
  corpus's documents are tagged. That is the population document mode should
  serve before any other.
- **Refuse rather than scramble.** The `Unknown` block variant already exists
  precisely so content that cannot be confidently classified is preserved opaque
  and never reflowed. Document mode extends that from blocks to pages: a page
  whose reconstruction confidence is low is regenerated *not at all*, and passed
  through byte-for-byte instead. A partially-flowed document is a legitimate
  output; a confidently-wrong one is not.

~~**Non-text content is further behind than text.**~~ **Closed — step 2.** All
four holes named here are now filled:

- `VectorPath` carries subpaths, curves as curves, the paint operation, fill and
  stroke colours, line width, the CTM, and the operator's byte span and source
  stream. A layout engine can place the artwork and an edit can get back to the
  bytes.
- **Clipping** is graphics state on `StateMachine`, saved and restored by `q`/`Q`
  and narrowed at the painting operator rather than at `W`. A bounding box, not a
  path — never smaller than the true region, which is the only safe direction to
  be wrong. `ImageBlock` now reports both `bbox` (what shows) and
  `unclipped_bbox` (what the operators said).
- **Shading** (`sh`) paints the clip region, which is why it had to come second:
  before the clip was modelled there was nothing to give it an extent. Pattern
  fills are resolved against `/Resources` and distinguished as tiling or
  shading, so a gradient-filled path is no longer indistinguishable from a flat
  one.
- **Annotations** are read in `rasura_layout::annots`, with `/V` inherited up the
  field tree, and the reader moved down from `rasura-edit` rather than
  duplicated. `Annotation::visible_text` is the question the flow model asks:
  what does this put in front of a reader? Hidden annotations answer nothing.

The measured effect on the pdf.js corpus: documents that exported to **nothing**
fell from 154 to 69, and the 77 that were pages of vector art fell to 3. The
remaining 62 are files whose glyphs map to no characters, which is a font
problem rather than a non-text one.

---

## Validation: the model round trip

Document mode needs an invariant of its own, because none of I1–I5 apply — they
all assume the bytes are meant to survive.

> **I8 — Model stability.** Build the model, lay it out, extract the model from
> the result, and the two models agree.

This is a strong test and a cheap one: it needs no oracle, no reference
renderer, and no human. It fails loudly on exactly the things that matter —
content dropped in layout, reading order permuted, a style lost, a table
flattened. For tagged documents the comparison can be made against the structure
tree instead of against our own first pass, which removes the risk of a
self-consistent but wrong model passing.

A second, weaker check is worth having alongside it: **page-count and
ink-coverage drift**. If a re-laid-out document has 12% more pages than the
original, something is wrong even when the model round-trips.

**Built** — `rasura_flow::compare`, with `Drift` for the second check.

The comparison is between two **flow** documents, not two placed ones, and that
is the whole reason the flow model came first. A placed model is coordinates:
laying a document out again moves every block by design, so a diff of placed
models reports every block as changed on a round trip that lost nothing. A flow
document has no coordinates, so it can be compared for the thing I8 is about.

The loop I8 will eventually close needs a layout engine, which is step 5. Three
round trips exist now and use the same function the engine will:

| Round trip | Result |
|---|---|
| Analysis against itself — same bytes, same model | 820 / 820 |
| Analysis across a save — write out, read back | 820 / 820 |
| A surgical edit is local — every other block unchanged | unit-tested |

**It found a bug on the first run.** One corpus file produced a *different model
from identical bytes*: `max_by_key` over a `HashMap` visits in a run-dependent
order, so two type sizes with equal glyph counts gave a different body size each
time, which moved the heading ladder, which changed whether a paragraph was
promoted. Ties now prefer the smaller size, and three consecutive sweeps of 820
files are clean. Nothing else in the codebase would have caught this: the output
was plausible every time, just not the same twice.

The design's suggestion of comparing against the structure tree for tagged
documents is not built. It is a different check — ours-versus-theirs rather than
ours-versus-ours — and it needs the tagged population to be worth the work,
which at 9% of this corpus it is not yet.

---

## Export is the cheaper half

Model to DOCX, HTML or Markdown needs **no frame inference and no layout
engine**. It is a direct mapping from what Phase 3 already produces, and nobody
expects byte fidelity from an export — the contract is honest by default.

It is also the fastest way to find out how good the reconstruction really is,
because an export is read by a human who will notice a scrambled paragraph
immediately. If document mode is worth building, export is how to learn that
cheaply first.

---

## Order of work, if this is pursued

1. ~~**Export to a flow format.** No new inference. Validates reconstruction
   quality against human judgement.~~ **Done** — `rasura-flow`, Markdown only.
   It validated the reconstruction immediately and unkindly: the first export
   of a real document read `Theefficientofficefinding`, because a PDF usually
   has no space characters and the builder was concatenating glyphs instead of
   running §7.3's word segmentation. Two more followed from reading the same
   page — `ex- traction` for every hyphenated line break, and `**1.1** **More**
   **text**` where one bold run had been split at every word boundary. All
   three are the kind of defect that is invisible in a unit test written by the
   person who wrote the code, and obvious in one paragraph of output. That is
   the argument for doing this step first, and it held.

   Still to do here: HTML and DOCX renderers, inline style inside table cells,
   footnotes (`running::footnotes` works on regions, which the model does not
   retain), and `/L`'s `/ListNumbering` so an ordered list is known to be
   ordered rather than assumed unordered.
2. ~~**Close the non-text holes** — vector provenance, clipping, shading,
   annotations.~~ **Done.** All four, in that order, because they depend on each
   other in it: shading has no extent without a clip, and a clip is graphics
   state that only the state machine can hold.

   The ordering claim held a second time. A test written earlier asserted the
   *gap* — that a clipped figure reports its unclipped extent — with a note
   saying it should fail once clipping was modelled and that the fix was to
   intersect rather than to delete the test. It failed on the first run after
   the state machine gained a clip.

   The three items left over at the end of the first pass are closed too:

   - **Pattern fills.** `VectorPath::pattern` resolves `scn` with a `/Pattern`
     colour space against `/Resources` and says whether it is a tiling pattern
     or a shading, with the object id so a consumer can go and read it. The
     state machine can only carry the *name* — resolving needs resources it does
     not have — so this belongs in the collector, which has the walker's scope
     stack.
   - **Clip exactness.** Still a bounding box, and now it says so:
     `GraphicsState::clip_exact` is true only while every clip path applied has
     been a single axis-aligned rectangle, which is what essentially every
     producer emits. Full path intersection is deliberately not built — two
     winding rules and curves is a polygon clipper, which is a rendering
     component — so the approximation is *reported* instead of hidden. It can
     only be lost, never regained, because intersecting a rectangle with a
     triangle does not give back a rectangle.
   - **`move_vector`.** Implemented, with the same wrap-do-not-rewrite technique
     as `move_image`. Two things had to exist first: `path_span`, covering the
     construction operators as well as the painting one — wrapping only the `f`
     would transform nothing, because the coordinates are in the `re` — and
     `self_contained`, which is false when anything else appears inside that
     range. That flag is what makes the operation safe rather than merely
     possible: a `W` inside a path would have its clip undone at the closing
     `Q`, silently changing every operator after it, which is precisely the
     non-locality §2 forbids. It declines instead.

   `move_vector` is reachable from `rasura-edit` and not yet from the facade or
   JavaScript; that is catalogue work rather than step 2.
3. ~~**Frame inference**, measured on tagged documents where the answer is
   known.~~ **Done** — `rasura_layout::frames`, with the numbers above.

   One thing this step got wrong in the planning. "Measured on tagged documents
   where the answer is known" does not work: a structure tree carries no
   coordinates, so tagging cannot say where a frame is, only what belongs in
   one. There is no ground truth to compare against. What replaced it is a pair
   of measures that cannot both be gamed — containment and tightness — plus the
   named list of documents where they are worst.

   The other finding worth carrying into step 5: **90% of this corpus offers
   only one page of evidence**, so the document-level method that makes frame
   inference work barely gets to run on it. The corpus is a pile of one-page
   regression fixtures, not a library of documents. Frames inferred from one
   page are marked `Evidence::SinglePage` and should be trusted accordingly.

   Still to do here: ruling lines and table grids are signal 4 and unused —
   `rules::collect` already produces them, and a drawn box is a hard boundary
   content did not cross. Frames are also horizontal-only: a page whose columns
   start at different heights is described by one rectangle per column, which is
   correct but coarse.
4. ~~**I8, the model round trip**, before the layout engine rather than after —
   it is the test that makes the engine's development tractable.~~ **Done** —
   `rasura_flow::compare`, run by `--i8`.

   Doing it before the engine paid immediately and not in the way expected: with
   no engine to test, it was pointed at the round trips that already existed and
   found a non-determinism bug in the *reconstruction* — a `HashMap` tie-break
   that made the same bytes produce two different models. That bug had been
   there since heading inference was written and nothing else could see it.

   Two design decisions worth carrying forward. A permutation is tested before
   the block-by-block comparison, because a reordered document differs at almost
   every position and reporting it positionally buries the one fact that matters
   under a wall of text differences. And whitespace is normalised by default:
   text is reconstructed from glyph positions, so a difference of one space is a
   difference in segmentation rather than in content, and an invariant that
   failed on it would be useless on real files.
5. ~~**The layout engine**, and document mode behind an explicit flag.~~
   **Done** — `rasura_flow::layout` and `rasura_flow::emit`.

   The flag is a *field*, `Options::accept_regeneration`, not a function name.
   A caller cannot reach page regeneration by autocompleting through the API,
   and a reviewer can grep for it — the same technique as
   `SaveOptions::accept_signature_destruction`, for the same reason.

   Document mode replaces page content streams and grows or shrinks the page
   tree. It touches nothing else: metadata, annotations, form fields, the
   structure tree and embedded files are carried through by the writer because
   they were never read. Text is written as standard-14 Helvetica in WinAnsi,
   matching the metrics the engine measured with — and characters the encoding
   cannot hold are **counted**, never silently substituted, because "the text
   changed" is the one thing a caller must not learn from a reader.

   Two things the end-to-end round trip taught, neither of them predictable
   from the design:

   - **Block boundaries cannot survive re-pagination, and it is not a defect.**
     A paragraph the layout split across a page break *is* two paragraphs to
     anyone reading the result; the format has no mark that says otherwise, and
     the reconstruction is right to report two. So the through-PDF round trip
     is held to `compare_reading` — every word, in order — while the in-memory
     one keeps the stricter block-for-block `compare`. Holding the first to the
     second would be holding it to a standard PDF cannot express.
   - **`rasura_content::page_text` cannot read a standard-14 font.** The widths
     live in `Standard14Widths`, one layer up, and the content crate has no
     width source to give it. The emitted documents extract correctly through
     `rasura_layout::page_text`, which is what a consumer uses. Worth knowing
     before anyone reaches for the lower one and concludes the writer is
     broken.

Steps 1 and 2 have standalone value even if document mode is never built, which
is the main argument for doing them in that order.
