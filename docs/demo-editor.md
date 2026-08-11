# Rasura Studio: a demonstration PDF editor

**Status: specification.** Nothing here is built. It describes a single-page web
application that exercises the whole of `rasura`'s JavaScript surface, and it is
written to be implementable without further design work.

The purpose is narrow and worth stating plainly: **to make the library's
distinctive properties visible in ten minutes to someone who has not read a line
of its source.** Those properties are byte-exact incremental saving, the
fidelity contract, verified redaction, and the fact that all of it runs with the
network switched off. A demo that shows a text cursor and a save button
demonstrates none of them.

---

## 1. The constraint that dictates the architecture

**Rasura does not render.** There is no rasteriser, and §11.6 says there will not
be one — a draw-command emitter exists and is deliberately small, because "the
operators *are* the API". A PDF editor must show the user a page.

So the application pairs two libraries, which is what §11.6 anticipates:

| | Renders | Owns the document |
|---|---|---|
| **pdf.js** | yes | no |
| **rasura** | no | yes |

pdf.js paints pixels onto a canvas. Rasura holds the authoritative bytes, answers
every question about structure, and performs every edit. The user interacts with
an **overlay** of rasura's model drawn on top of pdf.js's raster.

This is not a workaround. It is the correct division: rendering a PDF faithfully
is an enormous, well-solved problem, and duplicating it to save a dependency
would be the worst trade in the project. What matters is that **pdf.js is never
the source of truth** — it renders bytes rasura produced, and is re-fed after
every commit.

### The consequence to design around

After each commit the displayed raster is stale. The application must re-render,
and pdf.js must re-parse the new bytes to do it. That is the dominant latency in
the product, and §3.4 below budgets it.

---

## 2. Architecture

```
┌─────────────────────────────────────────────────────────┐
│ main thread                                             │
│   UI (state store, panels, overlay, keyboard)           │
│   canvas ◀── pdf.js render ──┐                          │
│   overlay ◀── model geometry ─┼── coordinate mapper      │
└───────────────┬──────────────┴──────────────────────────┘
                │ postMessage (structured clone, transfer)
   ┌────────────┴─────────────┐   ┌────────────────────────┐
   │ rasura Worker            │   │ pdf.js Worker          │
   │   rasura_wasm            │   │   (its own)            │
   │   authoritative bytes    │   │   render only          │
   └──────────────────────────┘   └────────────────────────┘
```

Three threads. The rasura Worker is supplied by the package — `Pdf.open` starts
it — so the application does not manage it, but it must know it exists, because
**everything is `async` and the buffer it hands over is detached**.

### 2.1 State

One store, three regions:

```ts
type State = {
  doc: {                    // what rasura says
    info: DocumentInfo;
    pages: Page[];          // lazily fetched, cached by index
    fonts: FontRequirement[];
    fields: FormField[];
    metadata: Metadata;
  };
  session: {                // what is staged
    status: SessionStatus;  // staged, undone, canUndo, canRedo, closed
    fidelityFloor: Fidelity;
    lastResult: EditResult | null;
    history: EditResult[];  // for the fidelity log, §5.9
  };
  ui: {
    page: number;
    zoom: number;
    tool: Tool;             // 'select' | 'text' | 'image' | 'annotate' | 'redact'
    selection: Selection | null;
    dirty: boolean;         // raster is stale
  };
};
```

**`doc` is a cache and must be invalidated wholesale after every commit.**
Paragraph ids are documented as stable *for the life of a `Page` object* and
nothing more; holding one across a commit is the defect this rule exists to
prevent. The store exposes exactly one way to refresh, and it takes the whole
document.

### 2.2 Data flow for an edit

1. User acts on the overlay (types, drags, clicks).
2. UI resolves the target to a model id (paragraph index, image id, table id).
3. `session.replaceText(...)` → `EditResult`.
4. **The result is shown, not swallowed.** Fidelity, `missingGlyphs`,
   `reflowedLines` and warnings go to the fidelity log (§5.9).
5. The overlay updates optimistically from the result; the raster does not.
6. On commit: `session.commit()` → bytes → re-open in pdf.js → re-render → refetch
   the model → clear `dirty`.

Step 4 is the whole point of the product. §11.1's third principle is that
fidelity is a return value, not an exception, and an editor that discards it is
an editor that lies.

---

## 3. Coordinate mapping

The single most likely source of subtle bugs, so it gets its own section and its
own test suite.

### 3.1 The spaces

- **Rasura device space.** Origin at the top-left of the page box, `y` increasing
  **downward**, units of points, `/Rotate` already applied. Every rectangle the
  API returns — `mediaBox`, `Paragraph.box`, `Image.box`, `Table.box` — is in
  this space.
- **pdf.js viewport space.** `page.getViewport({ scale, rotation })` produces
  canvas pixels, also `y` down, also with rotation applied.

For a page whose box starts at the origin, the mapping is therefore just a scale:

```ts
const toCanvas = (r: Rect, scale: number) => ({
  x: r.x0 * scale, y: r.y0 * scale,
  w: (r.x1 - r.x0) * scale, h: (r.y1 - r.y0) * scale,
});
```

### 3.2 The two cases that break it

Both are real and both were found by measurement rather than by reading:

1. **A crop box with a non-zero origin.** `endchar.pdf` in the pdf.js corpus has
   a 15×34pt crop box positioned at (404, 727), with its text 260 points to the
   *left* of it. Rasura's device space is relative to the page box; pdf.js's
   viewport is relative to the crop box it was asked for. If the two disagree
   about which box, everything is offset by the difference.
2. **Content outside the page box.** 937 blocks in the corpus lie partly or
   wholly outside their page box. The overlay must not assume a hit region is on
   screen, and must not clamp it silently — a control the user cannot reach is
   better than a control in the wrong place.

**Requirement.** The mapper is a pure function with a golden test per case:
zero-origin, non-zero-origin, each of the four `/Rotate` values, and content
outside the box. It is the only place in the application allowed to convert
between spaces.

---

## 4. Panels

A three-column layout: navigator, canvas, inspector. Every panel is driven by an
API call that already exists; none requires anything the library does not do.

```
┌──────────┬─────────────────────────────┬──────────────────┐
│ pages    │  canvas + overlay           │ inspector        │
│ (thumbs) │                             │  ├ document      │
│          │  ┌───────────────────────┐  │  ├ fonts        │
│ [1]◀     │  │                       │  │  ├ fields       │
│ [2]      │  │   pdf.js raster       │  │  ├ metadata     │
│ [3]      │  │   + hit regions       │  │  └ fidelity log │
│          │  └───────────────────────┘  │                  │
├──────────┴─────────────────────────────┴──────────────────┤
│ toolbar: select | text | image | annotate | redact  ⋯     │
│ session: 3 staged · undo · redo · rollback · commit       │
└────────────────────────────────────────────────────────────┘
```

---

## 5. Features

Each is written as *what the user does* → *what the library call is*. Nothing
below needs an API that does not exist today.

### 5.1 Opening

- File picker, drag-and-drop, and a set of bundled samples.
- `Pdf.open(bytes, { password, recovery })`.
- On `encrypted-password-required`, show a password prompt and retry. This is
  the first place the §11.5 error codes earn their keep — the UI branches on
  `e.code`, never on a message string.
- On `malformed`, offer to retry with `{ recovery: 'auto' }` and say what
  recovery means.
- **Pass `{ transfer: false }`** when the same bytes are also handed to pdf.js.
  Transferring detaches the buffer, and the second consumer gets an empty one.
  The library now reports this as a coded error rather than a `DataCloneError`,
  but the demo should not rely on the diagnostic.

### 5.2 Reading

- `doc.info()` fills the document inspector: page count, kind, tagged status,
  encryption, revision count, memory usage.
- **Permissions are displayed as advisory and not enforced.** The inspector says
  so in words. §5.5: "Enforcement in a library whose source you can read is
  theatre." A demo that greys out its print button because a bit said so would
  be teaching the wrong lesson.
- **`leniencies` is shown.** Every specification deviation tolerated while
  reading, listed in full. For a well-formed file the list is empty, and that is
  worth seeing too — it tells a user that nothing was papered over. For a
  damaged one it is the most informative thing on screen, and no other viewer
  offers it.

### 5.3 Text editing

- Click → `page.paragraphAt({ x, y })` → select the smallest containing
  paragraph. The overlay draws its box.
- Double-click enters edit mode: a contenteditable overlay positioned over the
  paragraph, seeded with `paragraph.text`.
- On commit of the field, diff old against new to derive a minimal range, then
  `session.replaceText(id, range, text, { page })`. A pure insertion at a caret
  uses `insertText(id, at, text)` and a pure deletion `deleteRange(id, range)`;
  both are narrower operations than a replacement and report less reflow, which
  is visible in the fidelity log and worth having for that reason alone.
- The paragraph list comes from `page.paragraphs()`, and `page.textContent()`
  backs a **Copy page text** action and the search box (§5.14).
- `textConfidence` drives the affordance: `exact` edits freely, `partial` shows a
  warning strip, `none` is **read-only** with an explanation. Letting a user
  "correct" text the library could not read is how a document gets silently
  mangled.

### 5.4 The fidelity control — the centrepiece

A segmented control in the toolbar: **Exact · Re-embedded · Substituted · Any**,
mapped to `doc.edit({ requireFidelity })`.

- At `exact`, an edit needing an unavailable glyph **fails** with
  `fidelity-below-required`, and the UI says which characters and offers to
  supply a font.
- At `any`, the same edit succeeds and the fidelity log records `reembedded`.

This one control is the product's argument. §11.4: *"A contract-redlining tool
sets `'exact'`; a form-filler accepts `'substituted'`. That single knob is worth
more than any feature in this document."*

### 5.5 Supplying a font

- The inspector's font panel lists `doc.fontRequirements()`: `pdfFont`, `family`,
  `embedded`, `subset`, `coverage`, `writableLatin`, `needsSupplying`.
- Anything with `needsSupplying` gets a **Supply…** button → file picker →
  `doc.registerFont(bytes, { matchFor: family })`.
- Nothing visibly happens, and the UI says so: the outline is injected only when
  an edit needs a character the document's own font cannot draw, at which point
  the result comes back `reembedded` rather than as a visible substitution.

### 5.6 Images

- `page.images()` gives id, box, pixel size and `editable`.
- Drag to move → `session.moveImage(id, { dx, dy })`; corner handles to scale →
  `scaleImage(id, { sx, sy })`; Delete → `deleteImage(id)`.
- **`editable: false` greys the handles** and the tooltip explains why: the image
  is drawn inside a form XObject shared with other pages, so moving it would move
  every instance. Showing the refusal before the drag beats reporting it after.

### 5.7 Pages

- Thumbnail drag to reorder → `session.movePage(from, to)`.
- Delete → `session.deletePage(index)`, with a warning that later page indices
  shift and held references go stale.

### 5.8 Annotations, forms, tables

- **Annotations.** Tools for square, circle, highlight and note →
  `session.addAnnotation({ kind, rect, colour, contents })`. The list panel comes
  from `session.annotations()`; each row deletes via `deleteAnnotation(id)`.
  The panel notes that appearances are generated rather than left to the viewer,
  which is why they look the same in every reader.
- **Forms.** `doc.formFields()` lists fully-qualified names, kinds and values;
  editing one calls `setFieldValue(name, value)`. A **Flatten** button calls
  `flattenForms()` and warns that it is one-way.
- **Tables.** `page.tables()` draws a grid overlay; clicking a cell edits it via
  `setCell(tableId, { row, column }, text)`.

### 5.9 The fidelity log

A permanent panel, appended to on every operation:

```
14:02:11  replaceText p1 ¶0        exact
14:02:19  replaceText p1 ¶2        reembedded   glyphs injected: "é"
14:02:31  moveImage   p1 #0        exact
14:02:44  replaceText p2 ¶1        REFUSED      fidelity-below-required
```

This is the panel that distinguishes the product. Every other PDF editor reports
success or an error; this one reports *how* it succeeded.

### 5.10 Redaction and verification

Deliberately not an edit-session operation, and the UI must reflect that:

1. Select text → **Redact**. A dialog states plainly: this is not undoable, it
   forces a full rewrite, and it removes the string from the whole file.
2. `doc.redact(text)` → the strings actually found, shown as a count.
3. `doc.save()` → assert `mode === 'full-rewrite'` and display it.
4. `doc.verifyRedaction(bytes, removed)` → a report panel.

**The report shows `notChecked` as prominently as `clean`.** A green tick with a
hidden list of exclusions is a worse outcome than no tick at all.

### 5.11 Encryption

- **Protect**: user and owner passwords, permission checkboxes, AES-256 or
  AES-128 → `doc.protect(opts)`.
- The returned `Weakness[]` is displayed immediately, in words: an empty user
  password means the document opens for anyone; an owner password equal to the
  user password grants nothing; AES-128 uses a legacy key derivation.
- **Remove protection** calls `doc.unprotect()`, and is available only once the
  document is open — which is to say, only to someone who already had the
  password. The dialog says that too, because "this button removes encryption"
  invites a question the UI should answer before it is asked.
- Entropy comes from `crypto.getRandomValues` inside the library. The dialog says
  so, because "where did the key come from" is a question a security-minded user
  should be able to answer from the UI.

### 5.12 Saving

Two buttons, and the difference is the demonstration:

- **Save** → `session.commit()` → `SaveResult { mode, bytesAppended, warnings }`.
- The result bar reads: `incremental · 412 bytes appended · original 2.1 MB
  unchanged`.
- **Save a copy (full rewrite)** → `commit({ fullRewrite: true })` for comparison.

A **byte inspector** shows the tail of the file: the original bytes, then the
appended revision. This is the clearest possible statement of what the library
does that others do not.

### 5.13 Optimising

An **Optimise** action in the document inspector calls `doc.compactFonts()` and
reports how many fonts were pruned to the glyphs the document actually draws.

Two things the UI must get right, because the ordering is counter-intuitive:

- It is only worth doing **after** editing. A font reduced to exactly the glyphs
  in use has nothing spare for the next insertion, so the next edit that would
  have been `exact` becomes one that has to re-embed. The button is disabled
  while a session has staged operations, with that as the tooltip.
- It forces a full rewrite, so the saving is real but the incremental-save
  demonstration of Act 1 no longer applies to that file. Run it last, or on a
  copy.

### 5.14 Search

`page.textContent()` per page, with matches highlighted on the overlay. It is
deliberately plain and it earns its place for one reason: **it is what makes
Act 3 checkable.** Searching for a redacted name before and after is how a
viewer sees that redaction did something; the byte inspector is how they see it
did the *right* thing.

---

## 6. The demo script

Five acts, ten minutes, in this order. Each act shows something no other
browser PDF tool can do.

**Act 1 — the incremental save (2 min).** Open a 40-page report. Change one word
on page 12. Save. The result bar reads *incremental · 412 bytes appended*. Open
the byte inspector: the original 2.1 MB is untouched and a small revision is
appended. Reopen the saved file to prove it is valid.

**Act 2 — the fidelity contract (3 min).** Set **Exact**. Type `café` into a
paragraph whose font is a subset without `é`. It is **refused**, naming the
character. Open the font panel — `MinionPro-Regular · subset · partial · needs
supplying`. Supply the file. Repeat the edit: it succeeds, and the log says
`reembedded`. Point out that the page still uses one typeface; the glyph was
injected into the document's own font rather than substituted.

**Act 3 — redaction you can check (2 min).** Redact a name. The save is forced to
a full rewrite and says so. The verification panel reports clean — and lists the
places it did not look. Then search the raw bytes in the byte inspector for the
name to show it is genuinely gone, not merely covered by a black rectangle.

**Act 4 — structure, not pixels (2 min).** Switch on the model overlay. Paragraph
boxes, table grids, image handles, and the inferred **text frames** appear over
the raster. Show a two-column page where the frames separate the columns. Export
to Markdown and show that the columns come out in reading order rather than
interleaved.

**Act 5 — it never left the building (1 min).** Open devtools, set the network to
**Offline**, and repeat Act 1. Everything works. For legal, medical and financial
documents this is not a feature, it is the procurement conversation.

---

## 7. Error handling

The application branches on `PdfError.code`, never on message text. Every one of
the thirteen codes gets a defined behaviour:

| Code | Behaviour |
|---|---|
| `encrypted-password-required` | Password prompt, retry |
| `encrypted-unsupported` | Explain; offer read-only if it opened at all |
| `malformed` | Offer recovery; explain what recovery does |
| `scanned-no-text` | Switch to annotate-only mode; explain that there is no OCR |
| `xfa-unsupported` | Read-only; explain that the AcroForm shadows an XFA payload |
| `type3-glyph-missing` | Mark the paragraph read-only |
| `font-unavailable` | Open the font panel, pre-filtered to the culprit |
| `overflow` | Offer the four overflow policies |
| `stale-session` | Refresh the model; never silently reissue |
| `fidelity-below-required` | Show the floor, the achieved rung, and the gap |
| `signature-would-be-destroyed` | Refuse; explain; offer save-as-copy |
| `unsupported-filter` | Name the filter |
| `internal` | Report with a copyable detail block |

An uncoded throw is a bug in the application, not in the user's document, and the
error boundary says so.

---

## 8. Performance

§13's budgets are **not built** in the library, so the application defines its
own and measures them itself:

| Operation | Budget | Notes |
|---|---|---|
| Open → first page metadata | 500 ms | `info()` on a 50-page file |
| Page model fetch | 150 ms | cached thereafter |
| Edit → `EditResult` | 100 ms | no re-render |
| Commit → re-rendered page | 1.5 s | dominated by pdf.js re-parsing |

The commit path is the one that will disappoint, and the mitigation is honesty
plus staging: **do not commit per keystroke.** Operations accumulate in the
session, the overlay updates optimistically, and the raster refreshes when the
user pauses or asks. The session was designed for exactly this — the undo stack
survives across calls precisely so an editor can batch.

`doc.info().memoryUsage` is displayed in a status bar, because §12.5 makes
`close()` non-optional and a demo that leaks documents should be visibly
leaking.

---

## 9. Testing

- **Coordinate mapping** — golden tests per §3.2, including `endchar.pdf`.
- **Every error code** — a fixture that provokes each of the thirteen.
- **Round trip** — for each bundled sample: open, edit, commit, reopen, assert the
  text changed and nothing else did.
- **The offline claim** — a Playwright run with the network blocked, asserting
  that Act 1 completes.
- **No uncoded throws** — a global handler that fails the test run on any error
  lacking a `code`.

---

## 10. Build and deploy

Static files. `npm i rasura` plus `pdfjs-dist`, a bundler, and nothing else — no
server, no API keys, no `SharedArrayBuffer`, so **no COOP/COEP headers**, which
is what makes it deployable to any static host and embeddable in any page. The
threaded build (§12.1) would change that and is not used.

The `.wasm` ships in the package. `wasmUrl` is set explicitly for CDN hosting,
per §12.4.

---

## 11. What this deliberately does not do

Stated in the UI, not just here, because a demo that quietly avoids its limits
teaches people to distrust it when they find them:

- **No rendering of its own.** pdf.js draws; rasura owns.
- **No OCR.** A scanned page offers annotation and nothing else.
- **No image pixel editing.** Images move, scale and delete; their contents are
  not touched. Re-encoding needs a codec, which §4.2's no-vendored-C++ rule makes
  a real decision.
- **No page insertion.** `insert_page` exists in Rust and is not on the JS
  surface, because a blank page you cannot draw on is not worth exposing.
- **No document mode.** The layout engine and PDF emitter exist in
  `rasura-flow` behind `accept_regeneration`, and are not reachable from
  JavaScript. Re-flowing a document is the one operation where §2's first
  property cannot hold, and it should stay hard to reach by accident. Act 4 shows
  the *model* and the Markdown export, not a re-laid-out PDF.
- **No signature creation.** Signatures are detected, preserved, and reported as
  invalidated.

---

## 12. Decisions still open

1. **Does Act 4 export via a server round trip?** Markdown export lives in
   `rasura-flow`, which is not in the WASM build. Either it is added to the JS
   surface (a `to_markdown` binding, cheap) or Act 4 shows the model overlay only.
   **Recommendation: add the binding.** It is one function and it makes the act.
2. **Text editing granularity.** Diffing an edited string into a minimal range is
   the application's job. A naive whole-paragraph replace is simpler and reports
   `reflowedLines` on every edit; a real diff keeps most edits local. Start naive,
   measure, then improve.
3. **Overlay for rotated pages.** `/Rotate 90` is common in scanned material.
   The mapper handles it; the *handles* (drag, resize) need their own rotation
   or they will feel wrong. Defer to after the first four acts work.
