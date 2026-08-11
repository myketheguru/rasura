// The rest of the edit catalogue, through the public API. Spec 9, 10, 11.
//
//   cd js && node --test test/catalogue.test.mjs
//
// Separate from api.test.mjs because it needs a different document: images,
// pages and form fields cannot be exercised on a page of prose. Same rule
// otherwise — the real WASM module, no mocks, and every claim read back out of
// the saved bytes rather than believed because a call returned.

import { test } from "node:test";
import assert from "node:assert/strict";

import { Pdf, PdfError } from "../src/index.js";
import { richer } from "./fixture.mjs";

/** Open, run, close — so a failing assertion cannot leak a worker. */
async function withDoc(fn, opts = {}) {
  const doc = await Pdf.open(richer(), opts);
  try {
    return await fn(doc);
  } finally {
    await doc.close();
  }
}

/** Open saved bytes, run, close. The only way to prove an edit landed. */
async function reopen(saved, fn) {
  const doc = await Pdf.open(saved.bytes, {});
  try {
    return await fn(doc);
  } finally {
    await doc.close();
  }
}

test("the fixture has the things the rest of this file needs", async () => {
  // Asserted rather than assumed. Every test below is conditional on the
  // fixture actually carrying an image, two pages and a field — and a fixture
  // that quietly lost one would make them all pass by skipping.
  await withDoc(async (doc) => {
    const info = await doc.info();
    assert.equal(info.pageCount, 2);

    const page = await doc.page(0);
    assert.ok(page.paragraphs().length > 0, "page 1 has text");
    assert.equal(page.images().length, 1, "page 1 has exactly one image");
    assert.deepEqual(page.images()[0].pixels, { width: 2, height: 2 });
    assert.equal(page.images()[0].editable, true, "and it is not inside a form XObject");

    const fields = await doc.formFields();
    assert.equal(fields.length, 1);
    assert.equal(fields[0].name, "signatory");
    assert.equal(fields[0].kind, "text");
  });
});

test("an image moves, and the text does not", async () => {
  const saved = await withDoc(async (doc) => {
    const page = await doc.page(0);
    const before = page.images()[0].box;

    const session = doc.edit();
    const result = await session.moveImage(page.images()[0].id, { dx: 40, dy: -25 });
    assert.ok(["exact", "reembedded", "substituted", "overlaid"].includes(result.fidelity));

    const out = await session.commit();
    return { out, before, text: page.textContent() };
  });

  await reopen(saved.out, async (doc) => {
    const page = await doc.page(0);
    const after = page.images()[0].box;
    assert.ok(Math.abs(after.x0 - (saved.before.x0 + 40)) < 0.01, `${saved.before.x0} -> ${after.x0}`);
    assert.ok(Math.abs(after.y0 - (saved.before.y0 - 25)) < 0.01, `${saved.before.y0} -> ${after.y0}`);
    assert.equal(page.textContent(), saved.text, "the text was not touched");
  });
});

test("an image scales about its own origin", async () => {
  const saved = await withDoc(async (doc) => {
    const page = await doc.page(0);
    const before = page.images()[0].box;
    const session = doc.edit();
    await session.scaleImage(page.images()[0].id, { sx: 2, sy: 0.5 });
    return { out: await session.commit(), before };
  });

  await reopen(saved.out, async (doc) => {
    const after = (await doc.page(0)).images()[0].box;
    const width = (b) => b.x1 - b.x0;
    const height = (b) => b.y1 - b.y0;
    assert.ok(
      Math.abs(width(after) - width(saved.before) * 2) < 0.01,
      `${width(saved.before)} -> ${width(after)}`,
    );
    assert.ok(
      Math.abs(height(after) - height(saved.before) * 0.5) < 0.01,
      `${height(saved.before)} -> ${height(after)}`,
    );
  });
});

test("deleting an image leaves the page's text alone", async () => {
  const saved = await withDoc(async (doc) => {
    const page = await doc.page(0);
    const session = doc.edit();
    await session.deleteImage(page.images()[0].id);
    return { out: await session.commit(), text: page.textContent() };
  });

  await reopen(saved.out, async (doc) => {
    const page = await doc.page(0);
    assert.equal(page.images().length, 0, "the image is no longer drawn");
    assert.equal(page.textContent(), saved.text);
  });
});

test("a page can be deleted and the other survives", async () => {
  const saved = await withDoc(async (doc) => {
    const second = (await doc.page(1)).textContent();
    const session = doc.edit();
    await session.deletePage(0);
    return { out: await session.commit(), second };
  });

  await reopen(saved.out, async (doc) => {
    assert.equal((await doc.info()).pageCount, 1);
    assert.equal((await doc.page(0)).textContent(), saved.second);
  });
});

test("pages can be reordered", async () => {
  const saved = await withDoc(async (doc) => {
    const first = (await doc.page(0)).textContent();
    const second = (await doc.page(1)).textContent();
    const session = doc.edit();
    await session.movePage(0, 1);
    return { out: await session.commit(), first, second };
  });

  await reopen(saved.out, async (doc) => {
    assert.equal((await doc.page(0)).textContent(), saved.second);
    assert.equal((await doc.page(1)).textContent(), saved.first);
  });
});

test("a form field is filled by its fully-qualified name", async () => {
  const saved = await withDoc(async (doc) => {
    const session = doc.edit();
    const result = await session.setFieldValue("signatory", "A. Ozdamar");
    assert.equal(typeof result.fidelity, "string");
    return await session.commit();
  });

  await reopen(saved, async (doc) => {
    const fields = await doc.formFields();
    assert.equal(fields[0].value, "A. Ozdamar");
  });
});

test("a field that does not exist is a coded refusal, not a silent no-op", async () => {
  // The failure mode worth a test: a caller who mistypes a field name and gets
  // a document back that looks fine and is not filled in.
  await withDoc(async (doc) => {
    const session = doc.edit();
    await assert.rejects(
      () => session.setFieldValue("signatorie", "A. Ozdamar"),
      (e) => e instanceof PdfError && typeof e.code === "string",
    );
  });
});

test("an annotation is added with its own appearance, and removed again", async () => {
  const saved = await withDoc(async (doc) => {
    const session = doc.edit();
    const result = await session.addAnnotation({
      kind: "Square",
      rect: { x0: 100, y0: 100, x1: 260, y1: 180 },
      colour: [0.85, 0.1, 0.1],
      interior: [1, 0.95, 0.8],
      borderWidth: 2,
      contents: "needs review",
    });
    assert.equal(typeof result.fidelity, "string");
    return await session.commit();
  });

  const removed = await reopen(saved, async (doc) => {
    const session = doc.edit();
    const listed = await session.annotations();
    const square = listed.filter((a) => a.kind === "Square");
    assert.equal(square.length, 1, JSON.stringify(listed));
    assert.equal(square[0].contents, "needs review");
    // §12.5.5: written rather than left to the viewer, because two viewers
    // synthesising an appearance differently is why annotations look wrong in
    // one reader and right in another.
    assert.equal(square[0].hasAppearance, true);
    assert.equal(typeof square[0].id.number, "number");

    await session.deleteAnnotation(square[0].id);
    return await session.commit();
  });

  await reopen(removed, async (doc) => {
    const listed = await doc.edit().annotations();
    assert.equal(listed.filter((a) => a.kind === "Square").length, 0);
  });
});

test("an unknown annotation kind is refused before anything is staged", async () => {
  await withDoc(async (doc) => {
    const session = doc.edit();
    await assert.rejects(
      () => session.addAnnotation({ kind: "Sqaure", rect: { x0: 0, y0: 0, x1: 1, y1: 1 } }),
      (e) => e instanceof PdfError,
    );
    assert.equal((await session.status()).staged, 0, "the failed call staged nothing");
  });
});

test("an image edit and a text edit share one undo stack", async () => {
  // The session is per document, not per operation kind. Two subsystems
  // staging into two logs would make "undo" ambiguous, and this is the check
  // that they do not.
  const original = richer();
  const doc = await Pdf.open(original, { transfer: false });
  try {
    const page = await doc.page(0);
    const session = doc.edit();

    await session.replaceText(page.paragraphs()[0].id, { start: 0, end: 9 }, "Half-year");
    await session.moveImage(page.images()[0].id, { dx: 10, dy: 10 });
    assert.deepEqual(await session.status(), {
      staged: 2,
      undone: 0,
      canUndo: true,
      canRedo: false,
      closed: false,
    });

    assert.equal(await session.undo(), true, "the image move comes off first");
    assert.equal(await session.undo(), true, "then the text edit");
    assert.equal(await session.undo(), false);

    const saved = await session.commit();
    assert.deepEqual(Array.from(saved.bytes), Array.from(original), "byte-identical again");
  } finally {
    await doc.close();
  }
});

test("redaction removes the text and reports what it found", async () => {
  const doc = await Pdf.open(richer(), {});
  try {
    const before = (await doc.page(0)).textContent();
    assert.ok(before.includes("revenue"));

    const removed = await doc.redact("revenue");
    assert.deepEqual(removed, ["revenue"]);

    // §9.6 forces a full rewrite, in code rather than in documentation: an
    // incremental save would leave the original bytes in the file.
    const saved = await doc.save();
    assert.equal(saved.mode, "full-rewrite");

    const report = await doc.verifyRedaction(saved.bytes, ["revenue"]);
    assert.equal(report.clean, true, JSON.stringify(report.traces));
    assert.ok(report.objectsChecked > 0);
    // The list of places the check does not look. A clean report without it
    // reads as a stronger claim than the check makes.
    assert.ok(Array.isArray(report.notChecked));

    await reopen(saved, async (after) => {
      assert.ok(!(await after.page(0)).textContent().includes("revenue"));
    });
  } finally {
    await doc.close();
  }
});

test("verifyRedaction finds a string that is still there", async () => {
  // The check has to be able to fail, or `clean: true` above proves nothing.
  await withDoc(async (doc) => {
    const saved = await doc.save();
    const report = await doc.verifyRedaction(saved.bytes, ["revenue"]);
    assert.equal(report.clean, false);
    assert.ok(report.traces.length > 0, JSON.stringify(report));
    assert.equal(report.traces[0].string, "revenue");
  });
});

test("protect encrypts, and the entropy comes from the platform", async () => {
  const saved = await withDoc(async (doc) => {
    // No `entropy` passed: the wrapper reaches for crypto.getRandomValues,
    // which is the decision this API exists to make visible — the module has
    // no RNG and will not grow one.
    const weaknesses = await doc.protect({
      userPassword: "hunter2",
      ownerPassword: "s3kr1t",
    });
    assert.deepEqual(weaknesses, []);
    return await doc.save();
  });

  assert.equal(saved.mode, "full-rewrite", "a protection change cannot be appended");

  // `transfer: false` because these bytes are opened twice: the default
  // transfers the buffer into the Worker and detaches it here.
  await assert.rejects(
    () => Pdf.open(saved.bytes, { transfer: false }),
    (e) => e.code === "encrypted-password-required",
  );

  const opened = await Pdf.open(saved.bytes, { password: "hunter2" });
  try {
    assert.equal((await opened.info()).encrypted, true);
    assert.ok((await opened.page(0)).textContent().includes("revenue"), "and it decrypts");
  } finally {
    await opened.close();
  }
});

test("protect reports a policy that protects nobody", async () => {
  await withDoc(async (doc) => {
    // Legal, common, and worth being told about: encrypted bytes that open for
    // anyone, with an owner password that grants what the user password does.
    const weaknesses = await doc.protect({});
    assert.deepEqual([...weaknesses].sort(), [
      "empty-user-password",
      "owner-password-equals-user",
    ]);
  });
});

test("aes-128 is offered and reported as weak", async () => {
  await withDoc(async (doc) => {
    const weaknesses = await doc.protect({ userPassword: "x", ownerPassword: "y", strength: "aes-128" });
    assert.ok(weaknesses.includes("legacy-key-derivation"), JSON.stringify(weaknesses));
  });
});

test("entropy that is not 32 bytes is refused before it reaches the key", async () => {
  await withDoc(async (doc) => {
    await assert.rejects(
      () => doc.protect({ userPassword: "x", entropy: new Uint8Array(16) }),
      (e) => e instanceof PdfError && e.code === "internal",
    );
  });
});

test("unprotect removes the encryption", async () => {
  const locked = await withDoc(async (doc) => {
    await doc.protect({ userPassword: "hunter2", ownerPassword: "s3kr1t" });
    return await doc.save();
  });

  const doc = await Pdf.open(locked.bytes, { password: "hunter2" });
  try {
    await doc.unprotect();
    const plain = await doc.save();
    const reopened = await Pdf.open(plain.bytes, {});
    try {
      assert.equal((await reopened.info()).encrypted, false);
      assert.ok((await reopened.page(0)).textContent().includes("revenue"));
    } finally {
      await reopened.close();
    }
  } finally {
    await doc.close();
  }
});

test("opening a buffer that was already transferred is a coded error", async () => {
  // Found by writing the encryption test above and getting a bare
  // `DataCloneError` out of `postMessage` — §11.5's one prohibition. The first
  // open transfers the buffer and detaches it; the second has nothing to send.
  const source = richer();
  const doc = await Pdf.open(source, {});
  await doc.close();
  assert.equal(source.byteLength, 0, "the first open transferred it away");

  await assert.rejects(
    () => Pdf.open(source, {}),
    (e) => e instanceof PdfError && e.code === "malformed" && /transfer: false/.test(e.message),
  );
});

test("compactFonts reports how many it touched", async () => {
  await withDoc(async (doc) => {
    const n = await doc.compactFonts();
    // The fixture's font is Helvetica, which is not embedded — so the honest
    // answer is zero, and a number greater than zero would mean this compacted
    // something that is not in the file.
    assert.equal(n, 0);
  });
});
