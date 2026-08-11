// The work, with no Worker in sight.
//
// Both paths run this: the Worker body calls it after `postMessage` hands over
// a request, and `{ worker: false }` calls it directly. One implementation, so
// the two cannot drift — which matters because the inline path is the one
// people debug with and the Worker path is the one they ship.

import { PdfError, normalise } from "./errors.js";

/** @type {any} */
let wasm = null;

/**
 * Load and initialise the WASM module. Idempotent.
 *
 * `wasmUrl` is spec 12.4's override: "`.wasm` shipped as an asset with a
 * documented `wasmUrl` override for CSP-strict and CDN-hosted consumers." The
 * bytes are fetched here and handed to the glue as a `BufferSource` rather than
 * letting it fetch a URL itself — which is what makes the same code work in a
 * browser, in a Worker, and in node, where the glue's own `fetch` cannot read a
 * `file://` URL.
 *
 * @param {{ wasmUrl?: string | URL }} [opts]
 */
export async function init(opts = {}) {
  if (wasm) return wasm;

  const glue = await import("../wasm/rasura_wasm.js");
  const url = opts.wasmUrl ?? new URL("../wasm/rasura_wasm_bg.wasm", import.meta.url);
  await glue.default({ module_or_path: await readWasm(url) });
  wasm = glue;
  return wasm;
}

/**
 * The module's bytes, from wherever this is running.
 * @param {string | URL} url
 */
async function readWasm(url) {
  // node cannot `fetch` a file: URL, and a browser has no `fs`. Feature-detect
  // rather than branch on an environment flag: a bundler that shims one of
  // these should get the shimmed one.
  const href = String(url);
  if (href.startsWith("file:")) {
    const { readFile } = await import("node:fs/promises");
    const { fileURLToPath } = await import("node:url");
    return await readFile(fileURLToPath(href));
  }
  const response = await fetch(href);
  if (!response.ok) {
    throw new PdfError(
      "internal",
      `could not fetch the WASM module from ${href} (${response.status})`,
    );
  }
  return await response.arrayBuffer();
}

/**
 * Handle one request. The single switch both transports go through.
 *
 * Returns `{ result, transfer }` — `transfer` lists buffers the Worker path
 * hands over rather than copies (§12.2). The inline path ignores it, because
 * there is no boundary to hand anything across.
 *
 * @param {{ op: string, args: any[] }} request
 */
export async function handle(request) {
  const m = await init(request.op === "init" ? request.args[0] ?? {} : {});
  const { op, args } = request;

  try {
    switch (op) {
      case "init":
        return { result: { version: m.version(), threaded: m.isThreaded() }, transfer: [] };

      case "open":
        return { result: m.openDocument(args[0], args[1], args[2]), transfer: [] };

      case "close":
        return { result: m.closeDocument(args[0]), transfer: [] };

      case "info":
        return { result: m.documentInfo(args[0]), transfer: [] };

      case "page":
        return { result: m.pageContent(args[0], args[1]), transfer: [] };

      case "fonts":
        return { result: m.fontRequirements(args[0]), transfer: [] };

      case "registerFont":
        return { result: m.registerFont(args[0], args[1], args[2]), transfer: [] };

      case "metadata":
        return { result: m.documentMetadata(args[0]), transfer: [] };

      case "save": {
        const saved = m.saveDocument(args[0], args[1]);
        // Bytes this module produced and nobody else holds, so transferring
        // them is free of the detachment hazard that makes it a decision on
        // the way in. Spec 12.2's "transfer rather than copy", where it costs
        // nothing to obey.
        return { result: saved, transfer: [saved.bytes.buffer] };
      }

      case "configureSession":
        return { result: m.configureSession(args[0], args[1], args[2], args[3]), transfer: [] };

      // Staged, so nothing comes back but the fidelity report. The bytes
      // arrive once, at commit — which is also why no edit operation below has
      // anything to transfer.
      case "replaceText":
        return {
          result: m.replaceText(args[0], args[1], args[2], args[3], args[4], args[5]),
          transfer: [],
        };

      case "insertText":
        return {
          result: m.insertText(args[0], args[1], args[2], args[3], args[4]),
          transfer: [],
        };

      case "deleteRange":
        return {
          result: m.deleteRange(args[0], args[1], args[2], args[3], args[4]),
          transfer: [],
        };

      case "moveImage":
        return {
          result: m.moveImage(args[0], args[1], args[2], args[3], args[4]),
          transfer: [],
        };

      case "scaleImage":
        return {
          result: m.scaleImage(args[0], args[1], args[2], args[3], args[4]),
          transfer: [],
        };

      case "deleteImage":
        return { result: m.deleteImage(args[0], args[1], args[2]), transfer: [] };

      case "setCell":
        return {
          result: m.setCell(args[0], args[1], args[2], args[3], args[4], args[5]),
          transfer: [],
        };

      case "deletePage":
        return { result: m.deletePage(args[0], args[1]), transfer: [] };

      case "movePage":
        return { result: m.movePage(args[0], args[1], args[2]), transfer: [] };

      case "annotations":
        return { result: m.pageAnnotations(args[0], args[1]), transfer: [] };

      case "addAnnotation":
        return { result: m.addAnnotation(args[0], args[1], args[2]), transfer: [] };

      case "deleteAnnotation":
        return { result: m.deleteAnnotation(args[0], args[1], args[2]), transfer: [] };

      case "formFields":
        return { result: m.formFields(args[0]), transfer: [] };

      case "setFieldValue":
        return { result: m.setFieldValue(args[0], args[1], args[2]), transfer: [] };

      case "flattenForms":
        return { result: m.flattenForms(args[0], args[1]), transfer: [] };

      // Document-level, and outside the session on purpose: each one either
      // forces a full rewrite or re-keys every object, neither of which can be
      // undone by restoring bytes the way a staged edit can.
      case "redact":
        return { result: m.redactText(args[0], args[1]), transfer: [] };

      case "verifyRedaction":
        return { result: m.verifyRedaction(args[0], args[1]), transfer: [] };

      case "protect":
        return { result: m.protectDocument(args[0], args[1], args[2]), transfer: [] };

      case "unprotect":
        return { result: m.unprotectDocument(args[0]) ?? null, transfer: [] };

      case "compactFonts":
        return { result: m.compactFonts(args[0]), transfer: [] };

      case "undo":
        return { result: m.undo(args[0]), transfer: [] };

      case "redo":
        return { result: m.redo(args[0]), transfer: [] };

      case "sessionStatus":
        return { result: m.sessionStatus(args[0]), transfer: [] };

      case "rollbackSession":
        return { result: m.rollbackSession(args[0]) ?? null, transfer: [] };

      case "commitSession": {
        const saved = m.commitSession(args[0], args[1]);
        return { result: saved, transfer: [saved.bytes.buffer] };
      }

      default:
        throw new PdfError("internal", `unknown operation ${JSON.stringify(op)}`);
    }
  } catch (e) {
    throw normalise(e);
  }
}
