// `Pdf.create` over the shipped wasm: the multi-face surface the engine grew,
// reached through the public JS API. Bold and italic runs are drawn in real
// faces the caller supplies; a face the family omits is reported, not faked.
//
// Skips when the Roboto faces are not fetched — `./corpus/fetch-font.sh` — so a
// clean checkout without the corpus still passes the suite.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";

import { Pdf } from "../src/index.js";

const face = (name) => new URL(`../../corpus/fonts/${name}`, import.meta.url);
const have = (name) => existsSync(face(name));
const read = (name) => new Uint8Array(readFileSync(face(name)));

test("create composes emphasis in the faces the family supplies", async () => {
  if (!have("Roboto-Regular.ttf") || !have("Roboto-Bold.ttf") || !have("Roboto-Italic.ttf")) {
    console.log("  (skipped: run ./corpus/fetch-font.sh)");
    return;
  }

  const { document, report } = await Pdf.create(
    [
      {
        kind: "paragraph",
        spans: [
          { text: "A line with a " },
          { text: "bold", bold: true },
          { text: " word, an " },
          { text: "italic", italic: true },
          { text: " one, and a " },
          // No bold-italic face is passed: this run falls back to regular and
          // is reported, not synthesised.
          { text: "both", bold: true, italic: true },
          { text: " one." },
        ],
      },
    ],
    read("Roboto-Regular.ttf"),
    { worker: false, bold: read("Roboto-Bold.ttf"), italic: read("Roboto-Italic.ttf") },
  );

  try {
    // Regular, bold and italic were all embedded, and are named.
    assert.ok(Array.isArray(report.faces));
    assert.ok(report.faces.length >= 3, `faces: ${JSON.stringify(report.faces)}`);
    // The bold-italic run had no face, so it is reported as substituted.
    assert.ok(Array.isArray(report.substituted));
    assert.ok(report.substituted.length >= 1, "the missing bold-italic face is reported");
    assert.equal(report.missing.length, 0, "every glyph was drawable");

    const info = await document.info();
    assert.equal(info.pageCount, 1);
  } finally {
    await document.close();
  }
});

test("a table is composed as a grid and reads back", async () => {
  if (!have("Roboto-Regular.ttf")) {
    console.log("  (skipped: run ./corpus/fetch-font.sh)");
    return;
  }

  const { document, report } = await Pdf.create(
    [
      { kind: "heading", level: 1, text: "Hours" },
      {
        kind: "table",
        header: true,
        rows: [
          ["Task", "Hours"],
          ["Design", "12"],
          ["Build", "30"],
        ],
      },
    ],
    read("Roboto-Regular.ttf"),
    { worker: false },
  );

  try {
    // Drawn as a grid, not dropped as an approximation.
    assert.equal(report.approximated, 0, "a table is composed, not approximated");
    assert.equal(report.missing.length, 0);

    const page = await document.page(0);
    assert.ok(page.tables().length >= 1, "the grid reads back as a table");
    const text = page.paragraphs().map((p) => p.text).join(" ");
    assert.ok(text.includes("Design"), `cell text is present: ${text}`);
  } finally {
    await document.close();
  }
});

test("a one-face family substitutes nothing until a run asks for another", async () => {
  if (!have("Roboto-Regular.ttf")) {
    console.log("  (skipped: run ./corpus/fetch-font.sh)");
    return;
  }

  const { document, report } = await Pdf.create(
    [{ kind: "paragraph", text: "Plain text, one face." }],
    read("Roboto-Regular.ttf"),
    { worker: false },
  );

  try {
    assert.equal(report.faces.length, 1, `one face embedded: ${JSON.stringify(report.faces)}`);
    assert.equal(report.substituted.length, 0, "plain text asks for no other face");
  } finally {
    await document.close();
  }
});
