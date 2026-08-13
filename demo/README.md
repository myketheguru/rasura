# The site, and the checks that keep it honest

**Live: https://myketheguru.github.io/rasura/**

The documentation and the demonstration editor are one React application in
[`web/`](../web), built by Vite in
[`.github/workflows/pages.yml`](../.github/workflows/pages.yml). Documentation
is at `/`; the editor is at `/#/editor`.

```bash
./crates/rasura-wasm/build.sh          # compile the module
cargo run -p rasura-flow --example sample   # write demo/sample.pdf
mkdir -p web/public/wasm && cp target/pkg/web/rasura_wasm.* web/public/wasm/
cp demo/sample.pdf web/public/
npm --prefix web ci && npm --prefix web run dev
```

This directory keeps the two things that are not part of the app: the sample
document the editor opens, and the checks.

## Why the editor does not look like the PDF

Rasura has no renderer and §11.6 says it will not grow one; the intended pairing
is pdf.js for display. The editor does not pair with it, and the reason is not
the dependency:

> **Every pixel on screen should come from the library.**

A pdf.js raster with an overlay demonstrates pdf.js's rendering and Rasura's
annotations of it. Drawing from the model demonstrates what Rasura itself knows —
paragraph boxes, line counts, tables, image extents, text confidence — which is
the thing worth showing. The page carries a banner saying so, because a viewer
who thinks it is a render will conclude the library renders badly.

## What is verified

- `node --experimental-strip-types demo/test.mjs` drives the **real WASM
  module** through the same call sequence the editor makes — open, page model,
  hit test, edit, session, commit, reopen, redact, verify — and checks the pure
  render core in `web/src/editor/model.ts` against it.
- `node demo/browser.mjs` **loads the built site in headless Chrome** over the
  DevTools protocol and checks both routes: that the documentation rendered, and
  that the editor compiled the module, got a version back from the library, and
  opened and modelled the sample. Nothing deploys that has not passed it.

  It also points at a deployed origin, which is the only way to learn whether
  what shipped is what was tested:

  ```bash
  RASURA_DEMO_ORIGIN=https://myketheguru.github.io/rasura node demo/browser.mjs
  ```

  That run has earned its keep: against a local server the sample fetch had
  always finished before the check looked, and against the real host it had not.

The browser check exists because for a long time nothing here loaded the page.
The module is built with `--omit-default-module-path`, whose entire effect is to
remove the glue's `import.meta.url` fallback, and the old demo called `init()`
with no argument on the strength of a comment claiming the opposite. It reached
`WebAssembly.instantiate(undefined, …)` every time and passed every check that
avoided a browser.

**Still not verified: anything a page load does not exercise.** The canvas is
checked for *happening*, not for looking right; pointer handling, dragging and
the editing gestures are not driven; and Chrome is not Safari. Treat those as
unproven until someone has looked.
