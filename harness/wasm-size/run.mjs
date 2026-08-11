// Runs the size probe against a real PDF in a JS runtime.
//
// A size measurement of a module that does not execute is worthless: the
// linker may have stripped the very code the number was supposed to cover. This
// proves the wasm actually parses a document, and doubles as the first evidence
// that `rasura-cos` works on wasm32 at all.
//
//   node harness/wasm-size/run.mjs target/probe-full-oz.wasm corpus/files/*.pdf

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const wasmPath = process.argv[2] ?? "target/probe-full-oz.wasm";
const dir = process.argv[3] ?? "corpus/files";

const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
const { memory, alloc_input, probe_open, probe_read, probe_save } = instance.exports;

function load(bytes) {
  const ptr = alloc_input(bytes.length);
  new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);
  return ptr;
}

const files = readdirSync(dir).filter((f) => f.endsWith(".pdf")).sort();
let ok = 0;
let failed = 0;

for (const name of files) {
  const bytes = readFileSync(join(dir, name));
  try {
    const objects = probe_open(load(bytes), bytes.length);
    const decoded = probe_read ? probe_read(load(bytes), bytes.length) : -1;
    const saved = probe_save ? probe_save(load(bytes), bytes.length) : -1;

    // Every fixture in corpus/files is a document with a catalog, so a zero
    // object count means the wasm build behaves differently from the native
    // one -- which is the failure this script exists to catch.
    if (objects === 0) {
      console.log(`FAIL ${name}: opened to 0 objects`);
      failed++;
      continue;
    }
    console.log(
      `ok   ${name.padEnd(40)} objects=${String(objects).padStart(3)} ` +
        `decoded=${String(decoded).padStart(6)} saved=${String(saved).padStart(6)}`,
    );
    ok++;
  } catch (e) {
    console.log(`FAIL ${name}: ${e.message}`);
    failed++;
  }
}

console.log(`\n${ok} ok, ${failed} failed`);
// `process.exitCode` rather than `process.exit`: the latter tears down libuv
// mid-flight and trips an assertion on Windows after the result has printed.
process.exitCode = failed ? 1 : 0;
