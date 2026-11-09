// The server path: a document opened from a module handed in, not fetched.
//
//   cd js && npm test
//
// Spec 11.7 says the Rust core is runtime-agnostic and §12.4 ships the `.wasm`
// as an asset. Between those two sits a runtime the package did not reach:
// Cloudflare Workers (workerd), which can neither `fetch` a bundled asset by
// URL nor read a filesystem, but which hands an imported `.wasm` to the module
// already compiled to a `WebAssembly.Module`.
//
// `{ worker: false, wasmModule }` is the path for it. This test stands in for
// workerd using the one thing node shares with it — `WebAssembly.Module` built
// from bytes the test itself holds — and proves the loader never touches
// `fetch` or `fs` when the module is supplied. If this passes, the deploy-time
// import workerd does is the same shape, one layer up.
//
// It lives in its own file on purpose. `init()` caches the module the first
// time it runs in a process (it is idempotent by design), so a test that wants
// to prove *its* module was the one used must be the first `open` in the
// process — which node's per-file test isolation gives it.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { Pdf } from "../src/index.js";
import { richer } from "./fixture.mjs";

// The shipped artefact, read as bytes and compiled here — never fetched. This
// is the browser-target glue's own `.wasm`; workerd would get the compiled
// Module from an `import`, and the glue accepts either.
const WASM = fileURLToPath(new URL("../wasm/rasura_wasm_bg.wasm", import.meta.url));

test("a module handed in opens a document, with no fetch and no fs", async () => {
  // Make `fetch` throw for the duration: if the loader reaches for it, the test
  // fails with that reach named, rather than passing because node happened to
  // be able to fall back. The fs reader is never reached either — it is only
  // behind the `file:` URL branch, which a supplied module skips entirely.
  //
  // One test, not two, because `init()` caches the module process-wide the
  // first time it runs (idempotent by design): a second `open` in the same
  // process reuses this module whatever it is handed, so a bytes-specific test
  // here would prove nothing. The `??` in `init` passes a module and raw bytes
  // down the same line; the glue compiles bytes to a module. They are one path.
  const realFetch = globalThis.fetch;
  globalThis.fetch = () => {
    throw new Error("the loader fetched a URL; a module was supplied");
  };
  try {
    const wasmModule = await WebAssembly.compile(readFileSync(WASM));
    const doc = await Pdf.open(richer(), { worker: false, wasmModule });
    try {
      const info = await doc.info();
      assert.equal(info.pageCount, 2);

      const page = await doc.page(0);
      assert.match(page.textContent(), /Quarterly revenue summary/);
    } finally {
      await doc.close();
    }
  } finally {
    globalThis.fetch = realFetch;
  }
});
