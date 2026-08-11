// The Worker body. Spec 12.2.
//
// > The JS wrapper spawns a Worker and runs everything in it. The main thread
// > never blocks.
//
// One file for both worlds. A browser `Worker` and node's `worker_threads`
// disagree about almost everything at the edges — `self.onmessage` against
// `parentPort.on('message')`, a bare value against `{ data }` — and agree about
// the middle, which is that a message goes in and a message comes out. The
// disagreement is absorbed in the first fifteen lines so that nothing below
// them knows which one it is running in.

import { handle } from "./core.js";
import { toWire } from "./errors.js";

/** @type {(message: any, transfer: any[]) => void} */
let reply;

if (typeof self !== "undefined" && typeof self.postMessage === "function") {
  reply = (message, transfer) => self.postMessage(message, transfer);
  self.onmessage = (event) => run(event.data);
} else {
  const { parentPort } = await import("node:worker_threads");
  if (!parentPort) {
    throw new Error("rasura worker started outside a worker context");
  }
  reply = (message, transfer) => parentPort.postMessage(message, transfer);
  parentPort.on("message", (message) => run(message));
}

/**
 * @param {{ id: number, op: string, args: any[] }} request
 */
async function run(request) {
  try {
    const { result, transfer } = await handle(request);
    reply({ id: request.id, ok: true, result }, transfer);
  } catch (e) {
    // Never `throw` here. An exception escaping a Worker fires `onerror` on the
    // other side, which carries a message and no way to match it to the call
    // that caused it — so a caller awaiting request 7 would hang for ever while
    // an unrelated handler reported something went wrong.
    //
    // And never post the error object itself: structured clone drops an Error's
    // own properties, so `code` would arrive `undefined`. See `errors.js`.
    reply({ id: request.id, ok: false, error: toWire(e) }, []);
  }
}
