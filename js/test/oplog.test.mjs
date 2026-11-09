// The op log: edits captured as data, and replayed. Spec 9.1, as data.
//
//   cd js && npm test
//
// The promise of the format is that the original bytes plus a few hundred bytes
// of JSON reconstruct the edited document exactly — so an editor can store the
// edit, not a second copy of the file. This test holds it to that: capture a
// session's ops, round-trip them through JSON (the thing a caller actually
// stores), replay them onto a fresh copy of the same original, and require the
// bytes to match the first commit to the byte.

import { test } from "node:test";
import assert from "node:assert/strict";

import { Pdf } from "../src/index.js";
import { richer } from "./fixture.mjs";

test("a session's ops are captured, serialised, and replay byte-identically", async () => {
  // First pass: edit, read the ops, commit — the bytes to reproduce.
  const doc = await Pdf.open(richer());
  let ops;
  let committed;
  try {
    const page = await doc.page(0);
    const session = doc.edit();

    await session.replaceText(page.paragraphs()[0].id, { start: 0, end: 5 }, "HELLO");
    await session.moveImage(page.images()[0].id, { dx: 10, dy: -10 });
    await session.setFieldValue("signatory", "A. Ozdamar");

    ops = await session.ops();
    committed = (await session.commit()).bytes;
  } finally {
    await doc.close();
  }

  // The log is the forward operations a caller named, in order — not the undo
  // stack's byte images.
  assert.equal(ops.length, 3);
  assert.deepEqual(
    ops.map((o) => o.op),
    ["replaceText", "moveImage", "setFieldValue"],
  );
  assert.equal(ops[0].text, "HELLO");
  assert.equal(ops[2].value, "A. Ozdamar");

  // The thing a caller stores: plain JSON, no live handles.
  const stored = JSON.parse(JSON.stringify(ops));

  // Second pass: replay the stored ops onto a fresh copy of the same original.
  const doc2 = await Pdf.open(richer());
  let replayed;
  try {
    const session = doc2.edit();
    const status = await session.applyOps(stored);
    assert.equal(status.staged, 3, "every op was staged");
    replayed = (await session.commit()).bytes;
  } finally {
    await doc2.close();
  }

  assert.deepEqual(replayed, committed, "the original plus the ops reconstruct the edit exactly");
});

test("replay refuses an op whose coordinate does not resolve", async () => {
  // Replay is exact against the document the ops were captured on. An op that
  // lands nowhere is refused with a code, not applied to whatever sits at that
  // index — the silent corruption the version stamp and this guard prevent.
  const doc = await Pdf.open(richer());
  try {
    const session = doc.edit();
    await assert.rejects(
      session.applyOps([{ op: "replaceText", page: 0, paragraph: 99, start: 0, end: 1, text: "x" }]),
      (e) => e.code === "malformed",
    );
    assert.equal((await session.ops()).length, 0, "a refused replay staged nothing");
  } finally {
    await doc.close();
  }
});
