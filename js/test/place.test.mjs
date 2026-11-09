// Adding content, not editing it: text drawn onto a page, and a field made
// where there was none. Spec 9.2 and 10.4.
//
//   cd js && npm test
//
// The two together are the "prepare a flat PDF to be filled, then fill it" path
// the form tests could not reach: `placeText` writes a value beside a printed
// caption, and `createField` turns a bare rectangle into something
// `setFieldValue` can fill. Each is checked by committing, reopening the saved
// bytes, and reading the result back — never by trusting the in-memory session.

import { test } from "node:test";
import assert from "node:assert/strict";

import { Pdf } from "../src/index.js";
import { richer } from "./fixture.mjs";

const pageText = (page) => page.paragraphs().map((p) => p.text).join("\n");

test("placed text is drawn and survives a round trip", async () => {
  const doc = await Pdf.open(richer());
  let saved;
  try {
    const session = doc.edit();
    const outcome = await session.placeText("Placed here", { x: 72, y: 500 }, { size: 12 });
    assert.equal(outcome.fidelity, "exact");
    saved = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }

  const reopened = await Pdf.open(saved);
  try {
    const page = await reopened.page(0);
    assert.match(pageText(page), /Placed here/, "the placed text reads back from the saved file");
  } finally {
    await reopened.close();
  }
});

test("text is placed just past a printed caption", async () => {
  // The flat-form case: no field, just a caption and the space to its right.
  const doc = await Pdf.open(richer());
  let saved;
  try {
    const session = doc.edit();
    // Page 0's paragraph reads "Quarterly revenue summary".
    await session.placeText("4.2M", { label: "Quarterly", gap: 6 }, { size: 12 });
    saved = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }

  const reopened = await Pdf.open(saved);
  try {
    const page = await reopened.page(0);
    assert.match(pageText(page), /4\.2M/, "the label-anchored value reads back");
  } finally {
    await reopened.close();
  }
});

test("a missing label is refused with a code, not placed somewhere", async () => {
  const doc = await Pdf.open(richer());
  try {
    const session = doc.edit();
    await assert.rejects(
      session.placeText("x", { label: "no such caption on this page", gap: 6 }),
      (e) => e.code === "malformed",
    );
    assert.equal((await session.ops()).length, 0, "a refused placement staged nothing");
  } finally {
    await doc.close();
  }
});

test("a field is created where none was, then filled", async () => {
  const doc = await Pdf.open(richer());
  let before;
  let saved;
  try {
    before = (await doc.formFields()).length; // 1: "signatory"
    const session = doc.edit();
    // Page 1 ("Notes and assumptions") carries no field of its own.
    await session.createField("countersign", [72, 500, 300, 520], { page: 1 });
    saved = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }

  const reopened = await Pdf.open(saved);
  try {
    const names = (await reopened.formFields()).map((f) => f.name);
    assert.ok(names.includes("countersign"), `expected the new field, saw ${JSON.stringify(names)}`);
    assert.equal(names.length, before + 1);

    // The created field fills like any other, and the value reads back.
    const session = reopened.edit();
    await session.setFieldValue("countersign", "A. Ozdamar");
    const refilled = (await session.commit()).bytes;
    const check = await Pdf.open(refilled);
    try {
      const field = (await check.formFields()).find((f) => f.name === "countersign");
      assert.equal(field.value, "A. Ozdamar");
    } finally {
      await check.close();
    }
  } finally {
    await reopened.close();
  }
});

test("placing text undoes to byte-identical, like every other edit", async () => {
  // `richer()` is a pure builder, so a fresh call is the exact original bytes to
  // compare against — the input buffer itself is transferred into the document on
  // open and cannot be read back here.
  const doc = await Pdf.open(richer());
  let committed;
  try {
    const session = doc.edit();
    await session.placeText("X", { x: 72, y: 500 }, { size: 12, background: [1, 1, 1] });
    assert.equal((await session.ops()).length, 1);
    assert.equal(await session.undo(), true);
    committed = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }
  assert.deepEqual(committed, richer(), "undo restored the exact prior bytes");
});

test("place and create are captured as ops and replay byte-identically", async () => {
  // The op-log contract (spec 9.1) extends to the new operations: capture,
  // JSON round-trip, replay onto a fresh copy of the same original, match.
  const doc = await Pdf.open(richer());
  let ops;
  let committed;
  try {
    const session = doc.edit();
    await session.placeText("4.2M", { label: "Quarterly", gap: 6 }, { size: 12 });
    await session.createField("countersign", [72, 500, 300, 520], { page: 1 });
    ops = await session.ops();
    committed = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }

  assert.deepEqual(
    ops.map((o) => o.op),
    ["placeText", "createField"],
  );
  // A NearLabel anchor is recorded as the resolved coordinate, not a second
  // label search — so the stored op carries x/y.
  assert.equal(typeof ops[0].x, "number");
  assert.equal(typeof ops[0].y, "number");

  const stored = JSON.parse(JSON.stringify(ops));
  const doc2 = await Pdf.open(richer());
  let replayed;
  try {
    const session = doc2.edit();
    const status = await session.applyOps(stored);
    assert.equal(status.staged, 2, "both ops were staged");
    replayed = (await session.commit()).bytes;
  } finally {
    await doc2.close();
  }

  assert.deepEqual(replayed, committed, "the original plus the ops reconstruct the edit exactly");
});
