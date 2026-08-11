// A consumer, using nothing but `npm i rasura`. Spec 12.4.
//
//   npm pack --prefix js
//   mkdir consumer && cd consumer
//   npm install --ignore-scripts ../rasura-0.1.0.tgz
//   cp ../corpus/files/classic-flate-content.pdf sample.pdf
//   node consumer.mjs
//
// Deliberately outside the repository's own resolution. Every other test in
// this package imports `../src/index.js`, which proves the code works and says
// nothing about whether the *package* does: an `exports` map that omits an
// entry point, a `files` list that leaves out the `.wasm`, or a path that only
// resolves relative to the source tree all pass every one of them and fail the
// first person to install it.
//
// Run with `--ignore-scripts`, so "no postinstall scripts, no native build
// step" is demonstrated rather than declared.

import { Pdf, PdfError } from "rasura";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const doc = await Pdf.open(new Uint8Array(readFileSync("sample.pdf")));

const info = await doc.info();
assert.ok(info.pageCount > 0);
console.log(`${info.pageCount} page(s), ${info.documentKind}, ${info.taggedStatus}`);

const page = await doc.page(0);
const before = page.textContent();
assert.ok(before.length > 0, "the sample has text");
console.log("text:", JSON.stringify(before.slice(0, 40)));

const fonts = await doc.fontRequirements();
console.log("fonts:", fonts.map((f) => `${f.pdfFont}(${f.coverage})`).join(", ") || "none");

const session = doc.edit();
const result = await session.replaceText(page.paragraphs()[0].id, { start: 0, end: 5 }, "GOODBYE");
assert.ok(["exact", "reembedded", "substituted", "overlaid"].includes(result.fidelity));

// Staged, not written. `status()` is the proof, and `commit()` is what produces
// bytes — a session that wrote on every call would have no undo to offer.
const staged = await session.status();
assert.equal(staged.staged, 1);
assert.equal(staged.canUndo, true);

const committed = await session.commit();
assert.ok(committed.bytes instanceof Uint8Array);
console.log("edit fidelity:", result.fidelity, "bytes:", committed.bytes.length);

// Read the edit back through a second open, which is the only proof it landed.
const after = await Pdf.open(committed.bytes);
const changed = (await after.page(0)).textContent();
assert.notEqual(changed, before);
assert.ok(changed.startsWith("GOODBYE"), changed);
console.log("re-read:", JSON.stringify(changed.slice(0, 40)));
await after.close();

// Spec 11.5 across the Worker boundary, from an installed package.
await assert.rejects(
  () => Pdf.open(new Uint8Array([1, 2, 3])),
  (e) => e instanceof PdfError && e.code === "malformed",
);

await doc.close();
console.log("OK: npm i rasura, and it works.");
