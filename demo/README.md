# Rasura Studio

A demonstration editor for [Rasura](../README.md), built to the specification in
[docs/demo-editor.md](../docs/demo-editor.md). Static files, no server, no
network. It is the shortest honest answer to "what does this library actually
do".

```bash
./crates/rasura-wasm/build.sh   # compile the module
node demo/build.mjs             # assemble demo/dist
npx --yes serve demo/dist       # serve it
```

Deployed by [`.github/workflows/pages.yml`](../.github/workflows/pages.yml) on
every push that touches the demo or the library.

## What it shows

- **Byte-exact incremental saving.** Edit a word, commit, and the byte inspector
  reports how many bytes were appended and that the rest is untouched.
- **The fidelity contract.** The *Require* control is `requireFidelity`. Set it
  to `exact` and an edit that would need a glyph the document's font lacks is
  **refused** rather than quietly degraded. Supply the typeface in the Fonts
  panel and the same edit succeeds as `reembedded`.
- **Verified redaction.** Redact a string and the panel reports whether any
  trace remains — and lists the places the check does not look, which matters
  more than the tick.
- **The log.** Every operation with the rung it achieved. Fidelity is a return
  value, not an exception, and this is what that looks like in a product.
- **Leniencies.** Every specification deviation tolerated while reading the
  file. No other viewer will tell you these.

## Why it does not look like the PDF

Rasura has no renderer and §11.6 says it will not grow one; the intended pairing
is pdf.js for display. This demo does not pair with it, and the reason is not
the dependency:

> **Every pixel on screen should come from the library.**

A pdf.js raster with an overlay demonstrates pdf.js's rendering and Rasura's
annotations of it. Drawing from the model demonstrates what Rasura itself knows —
paragraph boxes, line counts, tables, image extents, text confidence — which is
the thing worth showing. The page carries a banner saying so, because a viewer
who thinks this is a render will conclude the library renders badly.

A product would use pdf.js. §1 of the spec describes that architecture.

## What is verified, and what is not

Two checks run in CI and neither needs a browser:

- `node demo/test.mjs` drives the **real WASM module** through the same call
  sequence the page makes — open, page model, hit test, edit, session, commit,
  reopen, redact, verify — and checks the pure render core against it.
- `node demo/lint.mjs` catches the two mistakes that would break the page on
  load and that `node --check` cannot see: an element id the script reaches for
  that the markup does not define, and a name imported from the WASM glue that
  the glue does not export.

**Not verified: anything requiring a browser.** The canvas drawing, the pointer
handling, the layout, and whether the compiled module starts under a given
host's content-security policy. Those need a real browser and this repository
has no browser-based test harness. Treat the first load on a new host as
unproven until someone has looked at it.

## Files

| | |
|---|---|
| `index.html` | markup; every id is checked by the lint |
| `style.css` | one stylesheet, light and dark |
| `app.mjs` | the editor — state, drawing, panels, all library calls |
| `render.mjs` | pure: model → draw list, hit testing, minimal edit ranges |
| `build.mjs` | copies sources and the module into `dist/` |
| `test.mjs` | the data path, against the real module |
| `lint.mjs` | the two browser-free correctness checks |

`app.mjs` calls the WASM surface directly rather than going through the npm
package. The package starts a Worker and owns the transport, which is right for
an application and unnecessary for a same-origin static page where the whole
transport is a function call.
