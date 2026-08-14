// Talking to the Worker. Spec 12.2.
//
// A request/response channel over `postMessage`, which is a one-way primitive:
// messages arrive with no relation to what was sent. So every request carries
// an id and a pending map turns the stream back into promises. Without that,
// two concurrent `page()` calls resolve in arrival order and the second caller
// gets the first caller's page — a bug that only appears under concurrency and
// looks like corruption when it does.

import { PdfError, fromWire, normalise } from "./errors.js";

export class Channel {
  /** @param {{ wasmUrl?: string | URL }} [opts] */
  constructor(opts = {}) {
    this.opts = opts;
    /** @type {Map<number, { resolve: (v: any) => void, reject: (e: any) => void }>} */
    this.pending = new Map();
    this.nextId = 0;
    /** @type {any} */
    this.worker = null;
    /** @type {Promise<void> | null} */
    this.ready = null;
  }

  async start() {
    if (this.ready) return this.ready;
    this.ready = (async () => {
      const url = new URL("./worker.js", import.meta.url);
      this.worker = await spawn(url);
      this.worker.onMessage((message) => this.receive(message));
      // A Worker that dies takes every request in flight with it, and nobody
      // was listening. The symptom was not an error but a **hang**: the module
      // failed to load on the Worker's thread, `init` was never answered, and
      // the promise stayed pending for ever. A hang is worse than a failure —
      // it reads as a slow parser, and in CI as an infrastructure problem.
      this.worker.onError?.((reason) => this.fail(reason));
      // The wasm module is loaded once, on the Worker's own thread, before any
      // request needs it. Doing it lazily inside the first `open` would make
      // that one call pay for an 800 KB compile and look like a slow parser.
      await this.request("init", [this.opts], []);
    })();
    return this.ready;
  }

  /**
   * Send one request and await its reply.
   *
   * @param {string} op
   * @param {any[]} args
   * @param {any[]} [transfer] buffers handed over rather than copied
   */
  request(op, args, transfer = []) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      try {
        this.worker.post({ id, op, args }, transfer);
      } catch (e) {
        // `postMessage` throws synchronously for anything it cannot clone or
        // transfer — a detached buffer being the one that actually happens,
        // when the same bytes are opened twice and the first open transferred
        // them away. Left alone it surfaces as a bare `DOMException`, which is
        // §11.5's one prohibition. Caught here rather than at each call site
        // because a request that never left is still a request that failed.
        this.pending.delete(id);
        reject(normalise(e));
      }
    });
  }

  /**
   * Settle everything in flight as failed, because the Worker is gone.
   *
   * @param {string} reason
   */
  fail(reason) {
    const error = new PdfError(
      "internal",
      `the worker stopped: ${reason}`,
      "requests in flight when a worker dies cannot be retried; open the document again",
    );
    for (const [id, waiting] of this.pending) {
      this.pending.delete(id);
      waiting.reject(error);
    }
  }

  /** @param {{ id: number, ok: boolean, result?: any, error?: any }} message */
  receive(message) {
    const waiting = this.pending.get(message.id);
    if (!waiting) return;
    this.pending.delete(message.id);
    if (message.ok) {
      waiting.resolve(message.result);
    } else {
      waiting.reject(fromWire(message.error));
    }
  }

  /**
   * Shut the Worker down and settle everything still in flight.
   *
   * Awaitable, and awaited by `close()`. node's `terminate()` returns a promise
   * and the thread stays alive until it resolves — so a caller who closed every
   * document and expected their script to exit would find it hanging, with
   * nothing to point at.
   */
  async terminate() {
    // Pending requests are rejected rather than left unsettled. A promise that
    // never settles is the worst way to end: the caller's `await` simply stops,
    // with no error to log and no timeout to fire.
    for (const [, waiting] of this.pending) {
      waiting.reject(fromWire({ code: "stale-session", message: "the worker was terminated" }));
    }
    this.pending.clear();
    const worker = this.worker;
    this.worker = null;
    this.ready = null;
    await worker?.terminate();
  }
}

/**
 * Start a Worker, in a browser or in node.
 *
 * Returned as a pair of closures rather than the Worker itself, so the two
 * APIs' differences stop here. `new Worker(new URL(..., import.meta.url))` is
 * also the form every bundler recognises — Vite, webpack 5 and Rollup all
 * rewrite it — which is what makes §12.4's "no build step" hold for consumers
 * who do have one.
 *
 * @param {URL} url
 */
async function spawn(url) {
  if (typeof Worker === "function") {
    const worker = new Worker(url, { type: "module" });
    return {
      post: (message, transfer) => worker.postMessage(message, transfer),
      terminate: () => worker.terminate(),
      onMessage: (fn) => {
        worker.onmessage = (event) => fn(event.data);
      },
      onError: (fn) => {
        worker.onerror = (event) => fn(event.message ?? String(event));
        worker.onmessageerror = () => fn("a message could not be deserialised");
      },
    };
  }

  const { Worker: NodeWorker } = await import("node:worker_threads");
  const worker = new NodeWorker(url);
  // node's worker keeps the process alive until it is terminated; `unref` lets
  // a script that forgot to `close()` still exit. Forgetting is a leak either
  // way (§12.5), and hanging the process is a worse way to report it.
  worker.unref();
  return {
    post: (message, transfer) => worker.postMessage(message, transfer),
    terminate: () => worker.terminate(),
    onMessage: (fn) => worker.on("message", fn),
    onError: (fn) => {
      worker.on("error", (e) => fn(e?.message ?? String(e)));
      // A non-zero exit with nothing in flight is a normal shutdown; with
      // requests waiting it means the thread died under them.
      worker.on("exit", (code) => code !== 0 && fn(`the worker exited with code ${code}`));
    },
  };
}
