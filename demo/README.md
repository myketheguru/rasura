# Rasura Studio

**Live: https://myketheguru.github.io/rasura/**

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

Three checks run in CI:

- `node demo/test.mjs` drives the **real WASM module** through the same call
  sequence the page makes — open, page model, hit test, edit, session, commit,
  reopen, redact, verify — and checks the pure render core against it.
- `node demo/lint.mjs` catches the mistakes that would break the page on load
  and that `node --check` cannot see: an element id the script reaches for that
  the markup does not define, a name imported from the WASM glue that the glue
  does not export, and a page that calls `init()` bare when the module was
  built without a default path to itself.
- `node demo/browser.mjs` **loads the page in headless Chrome** over the
  DevTools protocol — no puppeteer, no install — and checks that the module
  compiled, the library answered, and the sample document was opened and
  modelled. Both `index.html` and `standalone.html`, served with real MIME
  types. Nothing deploys that has not done this.

The third one exists because the first two passed for weeks on a page that had
never once started. The module is built with `--omit-default-module-path`,
whose entire effect is to remove the glue's `import.meta.url` fallback, and the
page called `init()` with no argument on the strength of a comment claiming the
opposite. It reached `WebAssembly.instantiate(undefined, …)` every time. The
lesson was not "add a lint" — the lint came second. It was that the only way to
know a page runs is to run it.

**Still not verified: anything a load does not exercise.** The canvas drawing
is checked for *happening*, not for looking right; pointer handling, dragging,
and the editing gestures are not driven at all; and no other engine is tested —
Chrome is not Safari. Treat those as unproven until someone has looked.

## Files

| | |
|---|---|
| `index.html` | markup; every id is checked by the lint |
| `style.css` | one stylesheet, light and dark |
| `app.mjs` | the editor — state, drawing, panels, all library calls |
| `render.mjs` | pure: model → draw list, hit testing, minimal edit ranges |
| `build.mjs` | copies sources and the module into `dist/` |
| `test.mjs` | the data path, against the real module |
| `lint.mjs` | the browser-free correctness checks |
| `browser.mjs` | loads the assembled page in headless Chrome and checks it started |

`app.mjs` calls the WASM surface directly rather than going through the npm
package. The package starts a Worker and owns the transport, which is right for
an application and unnecessary for a same-origin static page where the whole
transport is a function call.
