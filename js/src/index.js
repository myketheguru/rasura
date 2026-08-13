// The public API. Spec 11.2, 11.4, 12.2, 12.4.
//
// Everything is `async` because everything crosses a Worker (§11.1). Note the
// direction of that sentence: the promises are here because of the Worker, not
// the other way round — nothing inside the WASM module is asynchronous, and
// `{ worker: false }` resolves on the same tick.

import { Channel } from "./channel.js";
import { handle } from "./core.js";
import { PdfError, normalise } from "./errors.js";

export { PdfError, CODES } from "./errors.js";

/**
 * A transport. Two implementations: one that talks to a Worker, one that runs
 * the same code on the calling thread.
 */
class Inline {
  /** @param {{ wasmUrl?: string | URL }} opts */
  constructor(opts) {
    this.opts = opts;
  }
  async start() {
    await handle({ op: "init", args: [this.opts] });
  }
  /** @param {string} op @param {any[]} args */
  async request(op, args) {
    try {
      const { result } = await handle({ op, args });
      return result;
    } catch (e) {
      throw normalise(e);
    }
  }
  terminate() {}
}

/**
 * Open documents. Spec 11.2's `Pdf`.
 */
export class Pdf {
  /**
   * @param {ArrayBuffer | Uint8Array | Blob} src
   * @param {import("../types/index.d.ts").OpenOptions} [opts]
   * @returns {Promise<Document>}
   */
  static async open(src, opts = {}) {
    const transport =
      opts.worker === false ? new Inline(opts) : new Channel(opts);
    await transport.start();

    const bytes = await toBytes(src);
    if (bytes.byteLength === 0) {
      // Almost always a buffer that was already transferred away by an earlier
      // `open`, which detaches it and leaves a zero-length view behind. The
      // symptom otherwise is a `DataCloneError` from `postMessage` naming
      // nothing the caller wrote, so the guess is worth making out loud.
      await transport.terminate();
      throw new PdfError(
        "malformed",
        "the input has no bytes; if these came from a previous open, that call " +
          "transferred the buffer away — pass { transfer: false } to keep it usable",
      );
    }
    // Spec 12.2: transfer rather than copy. The buffer is **detached** by this,
    // so `src` becomes unusable in the caller — which is the point (a 20 MB
    // document is not copied) and is also surprising enough that
    // `{ transfer: false }` exists to opt out. A `Blob` is never transferred:
    // the bytes were read out of it here and the caller still owns the Blob.
    const transfer =
      opts.transfer === false || !(src instanceof ArrayBuffer || ArrayBuffer.isView(src))
        ? []
        : [bytes.buffer];

    // A failed open must not leak the Worker it just started. Without this a
    // caller who tries three files and finds two malformed is left with two
    // threads they have no handle to and no way to stop — and in node, a
    // script that never exits.
    let handleId;
    try {
      handleId = await transport.request(
        "open",
        [bytes, opts.password, opts.recovery],
        transfer,
      );
    } catch (e) {
      await transport.terminate();
      throw e;
    }
    return new Document(transport, handleId, opts);
  }

  /**
   * Compose a document. Spec 11's `create`.
   *
   * Returns the document **and** what composing had to approximate, because
   * the second half is not optional information: a typeface with no glyph for a
   * character drops it rather than substituting one, and a caller who never
   * looks at `missing` ships a document with holes in it.
   *
   * The typeface is required. A document set in a font nobody embedded looks
   * like whatever the reader happens to have installed, which is the one thing
   * a PDF exists to prevent.
   *
   * @param {import("../types/index.d.ts").Content[]} content
   * @param {ArrayBuffer | Uint8Array | Blob} font
   * @param {import("../types/index.d.ts").CreateOptions} [opts]
   * @returns {Promise<import("../types/index.d.ts").Composed>}
   */
  static async create(content, font, opts = {}) {
    const transport = opts.worker === false ? new Inline(opts) : new Channel(opts);
    await transport.start();

    const bytes = await toBytes(font);
    if (bytes.byteLength === 0) {
      await transport.terminate();
      throw new PdfError("font-unavailable", "composing needs a typeface; none was given");
    }

    let result;
    try {
      // The typeface is **not** transferred, unlike a document's bytes in
      // `open`. A document is the caller's own file and may be twenty megabytes;
      // a typeface is a few hundred kilobytes and is very often the same one for
      // the next document, so detaching it would break the obvious loop.
      result = await transport.request("create", [content, bytes, opts], []);
    } catch (e) {
      await transport.terminate();
      throw e;
    }

    const { handle, missing, ...rest } = result;
    return {
      document: new Document(transport, handle, opts),
      // A string across the boundary, an array of characters here: JS strings
      // index by UTF-16 code unit, and a missing astral character would come
      // back as two meaningless halves from `missing[0]`.
      report: { ...rest, missing: [...missing] },
    };
  }
}

/** One open document. Spec 11.2. */
export class Document {
  /** @param {any} transport @param {number} handle @param {any} opts */
  constructor(transport, handle, opts) {
    this._transport = transport;
    this._handle = handle;
    this._opts = opts;
    this._info = null;
    this._closed = false;
  }

  _live() {
    if (this._closed) {
      throw new PdfError("stale-session", "this document has been closed");
    }
  }

  /**
   * Everything §11.2 exposes as readable properties.
   *
   * One await rather than eleven getters. Properties cannot be async, and a
   * getter that returned a promise for `pageCount` would be a trap — so the
   * whole set arrives together and is cached, which is also cheaper than
   * eleven crossings.
   */
  async info() {
    this._live();
    if (!this._info) {
      this._info = await this._transport.request("info", [this._handle]);
    }
    return this._info;
  }

  /** @param {number} index */
  async page(index) {
    this._live();
    const data = await this._transport.request("page", [this._handle, index]);
    return new Page(this, data);
  }

  /**
   * What fonts this document needs. Spec 11.3.
   *
   * Worth calling immediately after `open`: it turns "the browser cannot see
   * system fonts" from a problem discovered on the user's first keystroke into
   * a list of files to fetch.
   */
  async fontRequirements() {
    this._live();
    return await this._transport.request("fonts", [this._handle]);
  }

  /**
   * Supply a font the document does not have. Spec 11.3.
   *
   * Held until an edit needs a character the document's own embedded font
   * cannot draw, at which point the outline is injected *into* that font — so
   * the page keeps one typeface and the edit reports `reembedded` rather than
   * a substitution a reader can see.
   *
   * @param {ArrayBuffer | Uint8Array} bytes
   * @param {{ matchFor?: string }} [opts]
   */
  async registerFont(bytes, opts = {}) {
    this._live();
    const data = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    return await this._transport.request("registerFont", [this._handle, data, opts.matchFor]);
  }

  /** `/Info` and XMP, with their disagreements. Spec 10.3. */
  async metadata() {
    this._live();
    return await this._transport.request("metadata", [this._handle]);
  }

  /**
   * The document's form fields, by fully-qualified name. Spec 10.5.
   *
   * The name is `parent.child`, joined from every ancestor's `/T` — ISO 32000-1
   * §12.7.3.2's definition of a field's identity, and what `setFieldValue`
   * expects. A partial name is only unique among siblings, so a form with two
   * `address` fields under different parents has two distinct fields here and
   * one ambiguous name if you go looking for the short one.
   */
  async formFields() {
    this._live();
    return await this._transport.request("formFields", [this._handle]);
  }

  /**
   * Remove every trace of a string from the document. Spec 9.6.
   *
   * **Not a session operation, and not undoable.** Redaction forces a full
   * rewrite — the point is that the bytes stop existing, and an undo stack that
   * kept them for you would defeat it. Returns the strings actually found, so
   * "removed three occurrences" is distinguishable from "found nothing".
   *
   * Any open session is invalidated: its recorded spans address a byte layout
   * that no longer exists.
   *
   * @param {string} text
   * @returns {Promise<string[]>}
   */
  async redact(text) {
    this._live();
    this._info = null;
    return await this._transport.request("redact", [this._handle, text]);
  }

  /**
   * Encrypt the document, or change its password. Spec 5.
   *
   * Entropy comes from `crypto.getRandomValues` here rather than from inside
   * the WASM module, which has no RNG and should not grow one — the platform's
   * source is better than anything a module could bundle, and this way the
   * provenance of the key material is visible at the call site. Pass your own
   * 32 bytes as `opts.entropy` if you have a better source.
   *
   * Returns the weaknesses of the policy that was applied: an empty user
   * password, an owner password equal to the user password, or AES-128's
   * legacy key derivation. None of them is an error and all of them are things
   * a caller should not learn about from a security audit.
   *
   * @param {import("../types/index.d.ts").ProtectOptions} [opts]
   * @returns {Promise<string[]>}
   */
  async protect(opts = {}) {
    this._live();
    const entropy = opts.entropy ?? (await randomBytes(32));
    if (entropy.length !== 32) {
      throw new PdfError("internal", "entropy must be exactly 32 bytes");
    }
    const { entropy: _ignored, ...policy } = opts;
    this._info = null;
    return await this._transport.request("protect", [this._handle, policy, entropy]);
  }

  /** Remove the document's encryption. Spec 5. */
  async unprotect() {
    this._live();
    this._info = null;
    await this._transport.request("unprotect", [this._handle]);
  }

  /**
   * Search saved bytes for text that should no longer be there. Spec 9.6.
   *
   * Takes bytes rather than reading this document, because it verifies the
   * *artefact*: asking the in-memory document whether the redaction worked
   * would be asking the thing that performed it. Pass what `commit()` or
   * `save()` returned.
   *
   * Read `notChecked` before trusting `clean`. It lists the places this check
   * does not look, and a clean report means "not found where we searched", not
   * "not present".
   *
   * @param {Uint8Array} bytes
   * @param {string[]} strings
   */
  async verifyRedaction(bytes, strings) {
    this._live();
    return await this._transport.request("verifyRedaction", [bytes, strings]);
  }

  /**
   * Drop unused glyphs from every embedded font. Spec 8.6.
   *
   * Returns how many fonts were compacted. Do it *after* editing, not before:
   * a font reduced to exactly the glyphs the page uses has nothing left over
   * for the next insertion, which turns the next edit from `exact` into one
   * that has to re-embed.
   *
   * @returns {Promise<number>}
   */
  async compactFonts() {
    this._live();
    return await this._transport.request("compactFonts", [this._handle]);
  }

  /** Begin an edit. Spec 11.4. */
  edit(opts = {}) {
    this._live();
    return new Session(this, opts);
  }

  /** @param {{ fullRewrite?: boolean }} [opts] */
  async save(opts = {}) {
    this._live();
    return await this._transport.request("save", [this._handle, opts.fullRewrite]);
  }

  /**
   * Release the document. Spec 12.5.
   *
   * Not optional. WASM linear memory is not garbage-collected against the JS
   * heap, so a `Document` that goes out of scope leaves its bytes allocated
   * inside the module for the life of the page.
   */
  async close() {
    if (this._closed) return false;
    this._closed = true;
    const closed = await this._transport.request("close", [this._handle]);
    // Awaited: node's worker thread outlives an unawaited `terminate()`, so a
    // script that closed every document would hang at exit with nothing to
    // point at.
    await this._transport.terminate();
    return closed;
  }
}

/** One page. Spec 11.2. */
export class Page {
  /** @param {Document} doc @param {any} data */
  constructor(doc, data) {
    this._doc = doc;
    this.index = data.index;
    this.mediaBox = data.mediaBox;
    this.rotate = data.rotate;
    this.scanned = data.scanned;
    this._paragraphs = data.paragraphs;
    this._blocks = data.blocks;
    this._images = data.images;
    this._tables = data.tables;
  }

  paragraphs() {
    return this._paragraphs;
  }

  blocks() {
    return this._blocks;
  }

  /**
   * The images on this page, with their ids. Spec 11.2.
   *
   * `editable` is false for an image drawn inside a form XObject: the drawing
   * lives in the form's own stream, and a form can be invoked from several
   * pages, so moving it would move it everywhere. Check it before offering a
   * drag handle rather than after the edit is refused.
   */
  images() {
    return this._images;
  }

  /** The tables detected on this page. Spec 7.7. */
  tables() {
    return this._tables;
  }

  /** The page's text, paragraphs joined by blank lines. */
  textContent() {
    return this._paragraphs.map((p) => p.text).join("\n\n");
  }

  /**
   * The paragraph under a point. Spec 11.2's `paragraphAt`.
   *
   * Smallest match rather than first, so clicking a footnote inside a column
   * selects the footnote.
   *
   * @param {{ x: number, y: number }} point
   */
  paragraphAt(point) {
    const hits = this._paragraphs.filter(
      (p) =>
        point.x >= p.box.x0 && point.x <= p.box.x1 && point.y >= p.box.y0 && point.y <= p.box.y1,
    );
    if (!hits.length) return null;
    const area = (b) => Math.abs((b.x1 - b.x0) * (b.y1 - b.y0));
    return hits.reduce((best, p) => (area(p.box) < area(best.box) ? p : best));
  }
}

/**
 * An edit in progress. Spec 9.1, 11.4.
 *
 * Operations accumulate and **nothing is written until `commit()`**. That is
 * what makes `undo()` able to restore the exact prior bytes, and what lets a
 * caller make four changes and keep or discard them together:
 *
 * ```js
 * const session = doc.edit({ requireFidelity: 'exact' });
 * const r = await session.replaceText(para.id, { start: 0, end: 10 }, 'Q4 net revenue');
 * if (r.fidelity !== 'exact') await session.undo();
 * const out = await session.commit();
 * ```
 *
 * One session per document at a time. The state lives beside the document in
 * the module rather than in this object, so a second `doc.edit()` returns a
 * handle onto the same session rather than a competing one — which is the
 * honest answer, since two independent undo stacks over one document could not
 * both be right.
 */
export class Session {
  /** @param {Document} doc @param {SessionOptions} opts */
  constructor(doc, opts) {
    this._doc = doc;
    this._configured = doc._transport.request("configureSession", [
      doc._handle,
      opts.requireFidelity,
      opts.lineBreaking,
      opts.overflow,
    ]);
  }

  /**
   * @param {number} paragraphId
   * @param {{ start: number, end: number }} range
   * @param {string} text
   * @param {{ page?: number }} [where]
   */
  async replaceText(paragraphId, range, text, where = {}) {
    return await this._op("replaceText", [
      where.page ?? 0,
      paragraphId,
      range.start,
      range.end,
      text,
    ]);
  }

  /**
   * Insert text at a character offset. Spec 9.2.
   *
   * @param {number} paragraphId
   * @param {number} at
   * @param {string} text
   * @param {{ page?: number }} [where]
   */
  async insertText(paragraphId, at, text, where = {}) {
    return await this._op("insertText", [where.page ?? 0, paragraphId, at, text]);
  }

  /**
   * Delete a character range. Spec 9.2.
   *
   * This removes the glyphs from the page. It is **not** redaction: the text
   * may survive elsewhere in the file, and `document.redact()` is what removes
   * every trace of it.
   *
   * @param {number} paragraphId
   * @param {{ start: number, end: number }} range
   * @param {{ page?: number }} [where]
   */
  async deleteRange(paragraphId, range, where = {}) {
    return await this._op("deleteRange", [where.page ?? 0, paragraphId, range.start, range.end]);
  }

  /**
   * Move an image by a page-space offset. Spec 11.2.
   *
   * @param {number} imageId  from `page.images()`
   * @param {{ dx: number, dy: number }} by
   * @param {{ page?: number }} [where]
   */
  async moveImage(imageId, by, where = {}) {
    return await this._op("moveImage", [where.page ?? 0, imageId, by.dx, by.dy]);
  }

  /**
   * Scale an image about its own origin. Spec 11.2.
   *
   * @param {number} imageId
   * @param {{ sx: number, sy: number }} by
   * @param {{ page?: number }} [where]
   */
  async scaleImage(imageId, by, where = {}) {
    return await this._op("scaleImage", [where.page ?? 0, imageId, by.sx, by.sy]);
  }

  /**
   * Remove an image from the page. Spec 11.2.
   *
   * The drawing operator goes; the XObject itself stays in the file until
   * something garbage-collects it. If the *pixels* have to be gone — a redaction
   * rather than a layout change — save with `{ fullRewrite: true }`.
   *
   * @param {number} imageId
   * @param {{ page?: number }} [where]
   */
  async deleteImage(imageId, where = {}) {
    return await this._op("deleteImage", [where.page ?? 0, imageId]);
  }

  /**
   * Replace the text of one table cell. Spec 7.7.
   *
   * @param {number} tableId  from `page.tables()`
   * @param {{ row: number, column: number }} cell
   * @param {string} text
   * @param {{ page?: number }} [where]
   */
  async setCell(tableId, cell, text, where = {}) {
    return await this._op("setCell", [where.page ?? 0, tableId, cell.row, cell.column, text]);
  }

  /**
   * Remove a page. Spec 11.2.
   *
   * Page indices shift, so anything held from `doc.page(n)` for a later page is
   * stale afterwards. Re-read rather than adjusting by hand.
   *
   * @param {number} index
   */
  async deletePage(index) {
    return await this._op("deletePage", [index]);
  }

  /**
   * Reorder a page. Spec 11.2.
   * @param {number} from @param {number} to
   */
  async movePage(from, to) {
    return await this._op("movePage", [from, to]);
  }

  /**
   * The annotations on a page. Spec 10.4.
   * @param {{ page?: number }} [where]
   */
  async annotations(where = {}) {
    return await this._op("annotations", [where.page ?? 0]);
  }

  /**
   * Add an annotation, generating its appearance stream. Spec 10.4.
   *
   * The appearance is written rather than left to the viewer, because §12.5.5
   * says a viewer may synthesise its own — and two viewers synthesising
   * differently is the whole reason annotations look wrong in one reader and
   * right in another.
   *
   * @param {import("../types/index.d.ts").NewAnnotation} spec
   * @param {{ page?: number }} [where]
   */
  async addAnnotation(spec, where = {}) {
    return await this._op("addAnnotation", [where.page ?? 0, spec]);
  }

  /**
   * Remove an annotation by the id `annotations()` reported.
   * @param {{ number: number, generation: number }} id
   * @param {{ page?: number }} [where]
   */
  async deleteAnnotation(id, where = {}) {
    return await this._op("deleteAnnotation", [where.page ?? 0, id]);
  }

  /**
   * Fill a form field and regenerate its appearance. Spec 10.5.
   *
   * Addressed by the fully-qualified name from `document.formFields()`.
   *
   * @param {string} name
   * @param {string} value
   */
  async setFieldValue(name, value) {
    return await this._op("setFieldValue", [name, value]);
  }

  /**
   * Burn a page's widget appearances into its content and drop the fields.
   *
   * One-way: the form stops being a form. What it is for is producing a
   * document whose filled values cannot be edited back out by a reader.
   *
   * @param {{ page?: number }} [where]
   */
  async flattenForms(where = {}) {
    return await this._op("flattenForms", [where.page ?? 0]);
  }

  /**
   * Every staged operation goes through here.
   *
   * The `await this._configured` is the reason it exists: `edit({
   * requireFidelity: 'exact' })` does not block, so an operation issued on the
   * next line would otherwise race the configuration it depends on and lose the
   * floor exactly when it mattered. Doing that once, in one place, is what stops
   * the twentieth operation added later from forgetting it.
   *
   * @param {string} op @param {any[]} args
   */
  async _op(op, args) {
    this._doc._live();
    await this._configured;
    return await this._doc._transport.request(op, [this._doc._handle, ...args]);
  }

  /** Undo the last operation. Returns whether there was one. Invariant I5. */
  async undo() {
    this._doc._live();
    return await this._doc._transport.request("undo", [this._doc._handle]);
  }

  /** Redo the most recently undone operation. */
  async redo() {
    this._doc._live();
    return await this._doc._transport.request("redo", [this._doc._handle]);
  }

  /** What is staged, without changing anything. */
  async status() {
    this._doc._live();
    return await this._doc._transport.request("sessionStatus", [this._doc._handle]);
  }

  /** Undo everything and close the session. */
  async rollback() {
    this._doc._live();
    await this._configured;
    await this._doc._transport.request("rollbackSession", [this._doc._handle]);
  }

  /** Apply everything staged and write the document out. Spec 9.5. */
  async commit(opts = {}) {
    this._doc._live();
    await this._configured;
    return await this._doc._transport.request("commitSession", [
      this._doc._handle,
      opts.fullRewrite,
    ]);
  }
}

/**
 * Cryptographically strong random bytes, from whichever platform this is.
 *
 * `globalThis.crypto` in a browser, a Worker, Deno, and node 19 and later; the
 * dynamic import is the fallback for older node. There is deliberately no
 * `Math.random()` branch — an environment without a CSPRNG must fail loudly
 * rather than silently produce an encryption key anyone can reproduce.
 *
 * @param {number} n
 * @returns {Promise<Uint8Array>}
 */
async function randomBytes(n) {
  const source = globalThis.crypto;
  if (source && typeof source.getRandomValues === "function") {
    return source.getRandomValues(new Uint8Array(n));
  }
  try {
    const node = await import("node:crypto");
    return new Uint8Array(node.randomBytes(n));
  } catch {
    throw new PdfError(
      "internal",
      "no cryptographic random source; pass 32 bytes as `entropy`",
    );
  }
}

/**
 * Normalise the three accepted inputs to bytes.
 * @param {ArrayBuffer | Uint8Array | Blob} src
 */
async function toBytes(src) {
  if (src instanceof Uint8Array) return src;
  if (src instanceof ArrayBuffer) return new Uint8Array(src);
  if (typeof Blob !== "undefined" && src instanceof Blob) {
    return new Uint8Array(await src.arrayBuffer());
  }
  if (ArrayBuffer.isView(src)) {
    return new Uint8Array(src.buffer, src.byteOffset, src.byteLength);
  }
  throw new PdfError(
    "internal",
    "open() takes an ArrayBuffer, a Uint8Array or a Blob",
  );
}
