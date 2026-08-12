// The package, exercised through its public API. Spec 11, 12.
//
//   cd js && npm test
//
// Every test runs the real WASM module. There are no mocks here on purpose:
// the whole point of this layer is that it crosses a boundary, and a mock of
// the boundary tests the mock.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { Pdf, PdfError, CODES } from "../src/index.js";

// A flate-compressed classic-xref document, chosen rather than defaulted to:
// the byte-identical save and the edit round-trip are only worth much over a
// file whose content streams are compressed and whose xref is the old kind.
//
// It is generated, not committed, so a fresh clone does not have it. Say so
// here — the bare ENOENT this used to throw named a path and not a remedy, and
// it threw sixteen times.
const SAMPLE = fileURLToPath(
  new URL("../../corpus/files/classic-flate-content.pdf", import.meta.url),
);
if (!existsSync(SAMPLE)) {
  console.error(
    `missing ${SAMPLE}\n` +
      "the seed fixtures are generated; run this from the repository root:\n" +
      "  cargo run -p rasura-invariants -- --write-seed corpus/files",
  );
  process.exit(1);
}
const bytes = () => new Uint8Array(readFileSync(SAMPLE));

/** Open, run, close — so a failing assertion cannot leak a worker. */
async function withDoc(opts, fn) {
  const doc = await Pdf.open(bytes(), opts);
  try {
    return await fn(doc);
  } finally {
    await doc.close();
  }
}

test("a document opens through the worker and reports itself", async () => {
  await withDoc({}, async (doc) => {
    const info = await doc.info();
    assert.equal(typeof info.pageCount, "number");
    assert.ok(info.pageCount > 0);
    assert.ok(["born-digital", "scanned", "mixed"].includes(info.documentKind));
    assert.equal(typeof info.permissions.print, "boolean");
    assert.ok(Array.isArray(info.leniencies));
  });
});

test("the same document opens inline, with the same answers", async () => {
  // The two transports run one implementation. If they ever disagree, the
  // inline path — the one people debug with — has stopped predicting the
  // worker path, which is the one they ship.
  const viaWorker = await Pdf.open(bytes(), {});
  const viaInline = await Pdf.open(bytes(), { worker: false });
  try {
    const a = await viaWorker.info();
    const b = await viaInline.info();
    assert.deepEqual(
      { ...a, memoryUsage: 0 },
      { ...b, memoryUsage: 0 },
      "the two transports disagree about the same file",
    );
  } finally {
    await viaWorker.close();
    await viaInline.close();
  }
});

test("a page comes back as paragraphs and blocks", async () => {
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    assert.equal(page.index, 0);
    assert.equal(typeof page.mediaBox.x1, "number");
    const paragraphs = page.paragraphs();
    assert.ok(paragraphs.length > 0, "the sample has text");
    assert.equal(typeof paragraphs[0].text, "string");
    assert.ok(["exact", "partial", "none"].includes(paragraphs[0].textConfidence));
    assert.ok(Array.isArray(page.blocks()));
    assert.ok(page.textContent().length > 0);
  });
});

test("paragraphAt takes the smallest match, not the first", async () => {
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const p = page.paragraphs()[0];
    const centre = { x: (p.box.x0 + p.box.x1) / 2, y: (p.box.y0 + p.box.y1) / 2 };
    assert.equal(page.paragraphAt(centre)?.id, p.id);
    assert.equal(page.paragraphAt({ x: -1e6, y: -1e6 }), null);
  });
});

test("an unedited save returns the input byte for byte", async () => {
  // Invariant I1, at the outermost surface there is.
  const original = bytes();
  await withDoc({ transfer: false }, async (doc) => {
    const saved = await doc.save();
    assert.ok(saved.bytes instanceof Uint8Array);
    assert.deepEqual(Array.from(saved.bytes), Array.from(original));
    assert.equal(saved.bytesAppended, 0);
  });
});

test("an edit round-trips and reports its fidelity", async () => {
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const before = page.paragraphs()[0].text;

    const session = doc.edit();
    const result = await session.replaceText(page.paragraphs()[0].id, { start: 0, end: 1 }, "Z");
    assert.ok(["exact", "reembedded", "substituted", "overlaid"].includes(result.fidelity));
    assert.ok(Array.isArray(result.missingGlyphs));

    const saved = await session.commit();
    assert.ok(saved.bytes instanceof Uint8Array);

    // Read it back through the same API, which is the only proof the edit
    // landed rather than merely returned.
    const after = await Pdf.open(saved.bytes, {});
    try {
      const reread = (await after.page(0)).paragraphs()[0].text;
      assert.notEqual(reread, before);
      assert.equal(reread.slice(1), before.slice(1), "only the first character changed");
    } finally {
      await after.close();
    }
  });
});

test("several edits accumulate and commit together", async () => {
  // The point of a session. Each call stages; one call writes.
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    const session = doc.edit();
    await session.replaceText(id, { start: 0, end: 1 }, "X");
    await session.replaceText(id, { start: 1, end: 2 }, "Y");
    assert.deepEqual(await session.status(), {
      staged: 2,
      undone: 0,
      canUndo: true,
      canRedo: false,
      closed: false,
    });

    const saved = await session.commit();
    const after = await Pdf.open(saved.bytes, {});
    try {
      const text = (await after.page(0)).paragraphs()[0].text;
      assert.ok(text.startsWith("XY"), text);
    } finally {
      await after.close();
    }
  });
});

test("undo across the boundary restores the exact prior bytes", async () => {
  // Invariant I5, through a Worker. The session's log is parked in the module
  // between calls, so an undo three messages later has to restore what an undo
  // in the same call would have.
  const original = bytes();
  await withDoc({ transfer: false }, async (doc) => {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    const session = doc.edit();
    await session.replaceText(id, { start: 0, end: 5 }, "Howdy");
    assert.equal(await session.undo(), true);
    assert.equal(await session.undo(), false, "nothing left to undo");

    const saved = await session.commit();
    assert.deepEqual(Array.from(saved.bytes), Array.from(original), "byte-identical again");
  });
});

test("redo survives the round trip too", async () => {
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    const session = doc.edit();
    await session.replaceText(id, { start: 0, end: 1 }, "Z");
    await session.undo();
    let status = await session.status();
    assert.equal(status.canRedo, true);
    assert.equal(status.staged, 0);

    assert.equal(await session.redo(), true);
    status = await session.status();
    assert.equal(status.staged, 1);
    assert.equal(status.canRedo, false);
  });
});

test("rollback discards everything and closes the session", async () => {
  const original = bytes();
  await withDoc({ transfer: false }, async (doc) => {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    const session = doc.edit();
    await session.replaceText(id, { start: 0, end: 1 }, "A");
    await session.replaceText(id, { start: 1, end: 2 }, "B");
    await session.rollback();

    const status = await session.status();
    assert.equal(status.staged, 0);
    assert.equal(status.closed, true);

    // A closed session refuses further work rather than silently reopening.
    await assert.rejects(
      () => session.replaceText(id, { start: 0, end: 1 }, "C"),
      (e) => e instanceof PdfError && e.code === "stale-session",
    );

    const saved = await doc.save();
    assert.deepEqual(Array.from(saved.bytes), Array.from(original));
  });
});

test("a failed operation does not discard the ones before it", async () => {
  // The state has to be parked back even when the operation throws, or one
  // unencodable character would silently drop four good edits.
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    const session = doc.edit({ requireFidelity: "exact" });
    await session.replaceText(id, { start: 0, end: 1 }, "X");
    await assert.rejects(() => session.replaceText(id, { start: 1, end: 2 }, "一"));

    const status = await session.status();
    assert.equal(status.staged, 1, "the successful edit survived the failed one");
  });
});

test("requireFidelity refuses rather than degrading", async () => {
  await withDoc({}, async (doc) => {
    const page = await doc.page(0);
    const session = doc.edit({ requireFidelity: "exact" });
    await assert.rejects(
      () => session.replaceText(page.paragraphs()[0].id, { start: 0, end: 1 }, "一"),
      (e) => {
        assert.ok(e instanceof PdfError, `expected a PdfError, got ${e?.constructor?.name}`);
        assert.equal(e.code, "font-unavailable");
        return true;
      },
    );
  });
});

test("an error keeps its code across the worker boundary", async () => {
  // The trap this whole layer has to avoid. Structured clone does not carry an
  // Error's own properties, so an error *thrown* across a Worker arrives with
  // `code === undefined` — and every `if (e.code === ...)` a caller wrote takes
  // the wrong branch, silently. Errors are serialised explicitly instead.
  await assert.rejects(
    () => Pdf.open(new Uint8Array([1, 2, 3]), {}),
    (e) => {
      assert.ok(e instanceof PdfError, `expected a PdfError, got ${e?.constructor?.name}`);
      assert.equal(e.code, "malformed");
      assert.ok(CODES.includes(e.code));
      return true;
    },
  );
});

test("a failed open does not leak its worker", async () => {
  // Found by the test runner hanging rather than by an assertion: `Pdf.open`
  // starts a Worker and then throws, so the thread it started had no owner and
  // no way to be stopped. In node that is a script which never exits; in a
  // browser it is a thread per malformed file, for the life of the page.
  const before = process.getActiveResourcesInfo().filter((r) => r === "Worker").length;
  for (let i = 0; i < 3; i++) {
    await assert.rejects(() => Pdf.open(new Uint8Array([1, 2, 3]), {}));
  }
  // Termination is asynchronous; give the loop a turn to collect them.
  await new Promise((r) => setTimeout(r, 100));
  const after = process.getActiveResourcesInfo().filter((r) => r === "Worker").length;
  assert.equal(after, before, "three failed opens left workers behind");
});

test("the inline path codes its errors identically", async () => {
  await assert.rejects(
    () => Pdf.open(new Uint8Array([1, 2, 3]), { worker: false }),
    (e) => e instanceof PdfError && e.code === "malformed",
  );
});

test("a closed document refuses further use", async () => {
  const doc = await Pdf.open(bytes(), {});
  assert.equal(await doc.close(), true);
  assert.equal(await doc.close(), false, "closing twice is false, not a throw");
  await assert.rejects(
    () => doc.info(),
    (e) => e instanceof PdfError && e.code === "stale-session",
  );
});

test("transferring detaches the caller's buffer, and can be turned off", async () => {
  // Spec 12.2 asks for transfer rather than copy, and the cost is real enough
  // to be documented rather than discovered: the caller's buffer is emptied.
  const input = bytes();
  const doc = await Pdf.open(input, {});
  await doc.close();
  assert.equal(input.byteLength, 0, "the buffer was transferred, so it detached");

  const kept = bytes();
  const doc2 = await Pdf.open(kept, { transfer: false });
  await doc2.close();
  assert.ok(kept.byteLength > 0, "opting out kept the caller's bytes");
});

test("concurrent requests resolve to their own answers", async () => {
  // postMessage has no notion of a reply, so every request carries an id. Get
  // that wrong and two concurrent calls swap results — a bug that only appears
  // under concurrency and looks like corruption when it does.
  await withDoc({}, async (doc) => {
    const [info, page, fonts, meta] = await Promise.all([
      doc.info(),
      doc.page(0),
      doc.fontRequirements(),
      doc.metadata(),
    ]);
    assert.equal(typeof info.pageCount, "number");
    assert.equal(page.index, 0);
    assert.ok(Array.isArray(fonts));
    assert.ok("disagreements" in meta);
  });
});

test("a registered font supplies a glyph the document's subset lost", async () => {
  // Spec 11.3, end to end through the Worker. The producer embedded the
  // letters "Hi"; the user types É; the outline comes out of the font the
  // caller supplied and goes *into* the document's own, so the page keeps one
  // typeface and the edit reports `reembedded`.
  const roboto = fileURLToPath(
    new URL("../../corpus/fonts/Roboto-Regular.ttf", import.meta.url),
  );
  if (!existsSync(roboto)) {
    console.log("  (skipped: run ./corpus/fetch-font.sh)");
    return;
  }
  const sample = fileURLToPath(new URL("./subset.pdf", import.meta.url));
  if (!existsSync(sample)) {
    console.log("  (skipped: no subset fixture)");
    return;
  }

  const doc = await Pdf.open(new Uint8Array(readFileSync(sample)), {});
  try {
    const page = await doc.page(0);
    const id = page.paragraphs()[0].id;

    // Without it, the missing glyph is reported rather than hidden.
    const before = await doc.edit().replaceText(id, { start: 0, end: 1 }, "É");
    assert.deepEqual(before.missingGlyphs, ["É"], JSON.stringify(before));

    const count = await doc.registerFont(new Uint8Array(readFileSync(roboto)), {
      matchFor: "Roboto-Regular",
    });
    assert.equal(count, 1);

    const fresh = await doc.page(0);
    const after = await doc
      .edit()
      .replaceText(fresh.paragraphs()[0].id, { start: 0, end: 1 }, "É");
    assert.equal(after.fidelity, "reembedded", JSON.stringify(after));
    assert.deepEqual(after.missingGlyphs, []);
  } finally {
    await doc.close();
  }
});

test("font requirements answer the question spec 11.3 leads with", async () => {
  await withDoc({}, async (doc) => {
    const fonts = await doc.fontRequirements();
    assert.ok(Array.isArray(fonts));
    for (const f of fonts) {
      assert.equal(typeof f.pdfFont, "string");
      assert.ok(["full", "partial", "unknown"].includes(f.coverage));
      assert.equal(typeof f.needsSupplying, "boolean");
    }
  });
});

test("the CJS shim reaches the same module", async () => {
  const { createRequire } = await import("node:module");
  const require = createRequire(import.meta.url);
  const shim = require("../cjs/index.cjs");
  const mod = await shim.load();
  assert.equal(typeof mod.Pdf.open, "function");
  assert.equal(mod.PdfError, PdfError);
});
