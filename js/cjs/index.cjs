// The CommonJS shim. Spec 12.4: "ESM primary, with a CJS shim."
//
// A shim rather than a build. The package is ESM all the way down — the Worker
// entry point has to be a module, and the WASM glue `wasm-bindgen` emits for
// `--target web` is one too — so there is no CJS build to produce, only a
// bridge for `require()` callers.
//
// The bridge has to be async, and that is not a shortcoming to apologise for:
// `require()` is synchronous and `import()` is not, so a CJS consumer gets a
// promise for the namespace rather than the namespace. Every entry point in
// this API is already async (§11.1), so one more `await` at the top costs a
// caller nothing:
//
//     const { Pdf } = await require('rasura').load();
//
// The alternative — a synchronous facade that queues calls until the real
// module arrives — hides the asynchrony and produces a `Pdf` that is not yet a
// `Pdf`, which fails in ways nobody can debug.

let cached = null;

/**
 * Load the ESM entry point.
 * @returns {Promise<typeof import("../src/index.js")>}
 */
function load() {
  cached ??= import("../src/index.js");
  return cached;
}

module.exports = { load };
