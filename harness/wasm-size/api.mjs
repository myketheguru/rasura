// Drive the shipped WASM artefact from node. Spec 12.
//
//   ./crates/rasura-wasm/build.sh          # builds, then runs this
//   node harness/wasm-size/api.mjs target/pkg/nodejs
//
// The check that a Rust test cannot make. Everything on the WASM surface
// returns a `JsValue`, so none of it can run on a host target — a binding that
// compiles, links, and cannot open a file passes every test in the crate and
// fails here. This opens a real PDF, reads its paragraphs, edits one, and reads
// the result back out of the saved bytes.
//
// It also exercises the two things only a real JS boundary can: that errors
// arrive as `Error` objects carrying spec 11.5's `code`, and that a closed
// handle stays closed.

import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const dir = process.argv[2] ?? "target/pkg/nodejs";
const entry = `${dir}/rasura_wasm.js`;
if (!existsSync(entry)) {
  console.error(`missing ${entry} -- run crates/rasura-wasm/build.sh first`);
  process.exit(1);
}

// `--target nodejs` emits CommonJS, so it is required rather than imported.
// Resolved against the working directory rather than this file, because the
// argument is a path a person typed at a shell.
const require = createRequire(import.meta.url);
const wasm = require(resolve(entry));

let failures = 0;
const check = (label, condition, detail = "") => {
  if (condition) {
    console.log(`  ok    ${label}`);
  } else {
    console.error(`  FAIL  ${label}${detail ? ` -- ${detail}` : ""}`);
    failures += 1;
  }
};

// A document with **text in it**, because the edit path is the half of this
// surface a compile cannot check. `minimal-classic.pdf` opens and has no
// paragraphs, so using it would run every check here and quietly skip the one
// that matters.
const candidates = [
  // First, because it is the only one the repository actually carries: the
  // corpus seeds are generated and gitignored, and corpus/external is other
  // people's files. A checkout with nothing else in it still runs this check.
  "demo/sample.pdf",
  "corpus/files/classic-flate-content.pdf",
  "target/textedit/before.pdf",
  "target/protect/plain.pdf",
  "corpus/files/minimal-classic.pdf",
];
const path = process.argv[3] ?? candidates.find((p) => existsSync(p));
if (!path || !existsSync(path)) {
  console.error(`no sample PDF found; looked for:\n  ${candidates.join("\n  ")}`);
  process.exit(1);
}
console.log(`sample: ${path}`);
console.log(`version: ${wasm.version()}, threaded: ${wasm.isThreaded()}`);

const bytes = new Uint8Array(readFileSync(path));

console.log("\nopening");
const handle = wasm.openDocument(bytes, undefined, undefined);
check("a handle came back", Number.isInteger(handle) && handle > 0, String(handle));

const info = wasm.documentInfo(handle);
check("pageCount is a number", typeof info.pageCount === "number", JSON.stringify(info));
check("documentKind is one of the three",
  ["born-digital", "scanned", "mixed"].includes(info.documentKind), info.documentKind);
check("taggedStatus is reported", typeof info.taggedStatus === "string", info.taggedStatus);
check("permissions crossed as an object", typeof info.permissions?.print === "boolean");
check("memoryUsage is reported", info.memoryUsage > 0, String(info.memoryUsage));
console.log(`  ${info.pageCount} page(s), ${info.documentKind}, ${info.taggedStatus}`);

console.log("\nreading page 1");
const page = wasm.pageContent(handle, 0);
check("paragraphs is an array", Array.isArray(page.paragraphs));
check("blocks is an array", Array.isArray(page.blocks));
check("mediaBox crossed as an object", typeof page.mediaBox?.x1 === "number");
if (page.paragraphs.length) {
  const p = page.paragraphs[0];
  console.log(`  first paragraph: ${JSON.stringify(p.text.slice(0, 60))}`);
  check("a paragraph carries its confidence",
    ["exact", "partial", "none"].includes(p.textConfidence), p.textConfidence);
  check("a paragraph carries its alignment", typeof p.alignment === "string", p.alignment);
}

console.log("\nfont requirements");
const fonts = wasm.fontRequirements(handle);
check("fonts is an array", Array.isArray(fonts));
for (const f of fonts.slice(0, 4)) {
  console.log(
    `  ${f.pdfFont}: embedded=${f.embedded} subset=${f.subset} ` +
      `coverage=${f.coverage} needsSupplying=${f.needsSupplying}`,
  );
}

console.log("\nmetadata");
const meta = wasm.documentMetadata(handle);
check("metadata has both surfaces", "info" in meta && "xmp" in meta);
check("disagreements is an array", Array.isArray(meta.disagreements));

console.log("\nsaving unchanged");
const saved = wasm.saveDocument(handle, undefined);
check("bytes came back as a Uint8Array", saved.bytes instanceof Uint8Array);
check(
  "an unedited save is byte-identical",
  saved.bytes.length === bytes.length && saved.bytes.every((b, i) => b === bytes[i]),
  `${saved.bytes.length} vs ${bytes.length}`,
);

console.log("\nediting");
// Asserted rather than assumed. A sample with no text would skip every check
// below and still print a clean run, which is the failure mode this whole file
// exists to catch in the Rust tests.
check(
  "the sample has editable text for the checks below",
  page.paragraphs.length > 0 && page.paragraphs[0].text.length > 2,
  `${page.paragraphs.length} paragraph(s) in ${path}`,
);
if (page.paragraphs.length && page.paragraphs[0].text.length > 2) {
  const original = page.paragraphs[0].text;
  try {
    // Staged, not applied. Nothing is written until commitSession, which is
    // what makes undo able to restore the exact prior bytes.
    const outcome = wasm.replaceText(handle, 0, 0, 0, 1, "Z");
    check("an edit reports its fidelity",
      ["exact", "reembedded", "substituted", "overlaid"].includes(outcome.fidelity),
      outcome.fidelity);

    const staged = wasm.sessionStatus(handle);
    check("the operation is staged, not written", staged.staged === 1 && staged.canUndo,
      JSON.stringify(staged));

    const result = wasm.commitSession(handle, undefined);
    check("the commit produced bytes", result.bytes instanceof Uint8Array && result.bytes.length > 0);

    // Read the result back through the same surface, which is the only way to
    // know the edit landed rather than merely returning.
    const after = wasm.openDocument(result.bytes, undefined, undefined);
    const reread = wasm.pageContent(after, 0);
    const changed = reread.paragraphs[0]?.text ?? "";
    check("the text actually changed", changed !== original, `${original} -> ${changed}`);
    check("only the first character changed",
      changed.slice(1) === original.slice(1), `${original} -> ${changed}`);

    // The rest of the catalogue, over the committed document. Each one is
    // checked for *reaching* its layer and coming back with a fidelity report
    // -- what it does to the page is the Rust suite's job, but a binding that
    // was declared and never wired up fails only here.
    const shapes = wasm.pageContent(after, 0);
    check("a page reports its images", Array.isArray(shapes.images));
    check("a page reports its tables", Array.isArray(shapes.tables));

    const annot = wasm.addAnnotation(after, 0, {
      kind: "Square",
      rect: { x0: 40, y0: 40, x1: 160, y1: 100 },
      colour: [0.8, 0.1, 0.1],
      borderWidth: 2,
      contents: "checked by the harness",
    });
    check("an annotation reports its fidelity", typeof annot.fidelity === "string", annot.fidelity);
    const annots = wasm.pageAnnotations(after, 0);
    check("the annotation came back", annots.length === 1 && annots[0].kind === "Square",
      JSON.stringify(annots));
    check("it carries an object id", typeof annots[0].id?.number === "number");
    check("it carries its own appearance", annots[0].hasAppearance === true);

    const removed = wasm.deleteAnnotation(after, 0, annots[0].id);
    check("deleting it also reports fidelity", typeof removed.fidelity === "string");
    check("and it is gone", wasm.pageAnnotations(after, 0).length === 0);

    check("formFields is an array", Array.isArray(wasm.formFields(after)));
    check("compactFonts returns a count", typeof wasm.compactFonts(after) === "number");

    // Encryption, with entropy from the platform -- which is the whole point of
    // the module not having an RNG of its own.
    const { randomBytes } = await import("node:crypto");
    const weaknesses = wasm.protectDocument(
      after,
      { userPassword: "hunter2", ownerPassword: "s3kr1t", strength: "aes-256" },
      new Uint8Array(randomBytes(32)),
    );
    check("protect reports its weaknesses as an array", Array.isArray(weaknesses),
      JSON.stringify(weaknesses));
    const locked = wasm.saveDocument(after, undefined);
    check("a protected save is a full rewrite", locked.mode === "full-rewrite", locked.mode);
    let refused = null;
    try {
      wasm.openDocument(locked.bytes, undefined, undefined);
    } catch (e) {
      refused = e.code;
    }
    check("the saved file now needs the password",
      refused === "encrypted-password-required", String(refused));
    const reopened = wasm.openDocument(locked.bytes, "hunter2", undefined);
    check("and opens with it", wasm.documentInfo(reopened).encrypted === true);
    wasm.closeDocument(reopened);

    wasm.closeDocument(after);
  } catch (e) {
    // An edit can legitimately decline -- a font with no glyph for 'Z', text
    // inside a form XObject. What must not happen is an uncoded throw.
    check("a declined edit still carries a code", typeof e.code === "string", String(e));
    console.log(`  (declined: ${e.code} -- ${e.message})`);
  }
}

console.log("\nerrors");
try {
  wasm.openDocument(new Uint8Array([1, 2, 3]), undefined, undefined);
  check("garbage is refused", false, "it opened");
} catch (e) {
  check("a thrown value is an Error", e instanceof Error, typeof e);
  check("it carries spec 11.5's code", e.code === "malformed", String(e.code));
}

try {
  wasm.pageContent(handle, 9999);
  check("a page past the end is refused", false, "it returned");
} catch (e) {
  check("out-of-range page carries a code", typeof e.code === "string", String(e.code));
}

console.log("\nclosing");
check("close reports it closed something", wasm.closeDocument(handle) === true);
check("closing twice is false, not a throw", wasm.closeDocument(handle) === false);
try {
  wasm.documentInfo(handle);
  check("a closed handle is refused", false, "it returned");
} catch (e) {
  check("a stale handle says stale-session", e.code === "stale-session", String(e.code));
}

console.log();
if (failures) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log("the shipped artefact opens, reads, edits and saves a real PDF from node.");
