# rasura

**True PDF editing in the browser.** Read a document as paragraphs, change one,
and write it back with the untouched 99% of the file byte-identical.

```bash
npm i rasura
```

No postinstall script, no native build step, no `node-gyp`. The `.wasm` is in
the tarball.

```js
import { Pdf, PdfError } from "rasura";

const doc = await Pdf.open(await file.arrayBuffer());

const page = await doc.page(0);
const para = page.paragraphs()[0];

const session = doc.edit({ requireFidelity: "exact" });
const result = await session.replaceText(para.id, { start: 0, end: 5 }, "Q4 net revenue");
if (result.fidelity !== "exact") await session.undo();

const saved = await session.commit();   // nothing is written before this
download(saved.bytes);

await doc.close();
```

## Four things worth knowing before you start

**Everything is async, and the reason is the Worker.** Not because the parsing
is: nothing inside the WASM module is asynchronous, and `{ worker: false }`
resolves on the same tick. The promises are there because by default the work
happens on another thread and your main thread never blocks.

**`close()` is not optional.** WebAssembly memory is not garbage-collected
against the JS heap. A `Document` that goes out of scope leaves its bytes
allocated inside the module for the life of the page.

**`open()` detaches your buffer.** Transferring rather than copying is what
keeps a 20 MB document from being duplicated — and it empties the
`ArrayBuffer` you passed in. Pass `{ transfer: false }` when you need those
bytes again.

**Every failure is a `PdfError` with a `code`.** Never a bare `Error`, and never
an uncoded throw:

```js
try {
  await Pdf.open(bytes);
} catch (e) {
  if (e.code === "encrypted-password-required") return askForPassword();
  if (e.code === "malformed") return showRepairPrompt();
  throw e;
}
```

## Sessions

Operations accumulate; nothing is written until `commit()`. That is what makes
`undo()` able to restore the exact prior bytes, and what lets you make four
changes and keep or discard them together.

```js
const session = doc.edit();
await session.replaceText(a.id, { start: 0, end: 4 }, "Q4");
await session.replaceText(b.id, { start: 0, end: 6 }, "net revenue");

await session.status();     // { staged: 2, canUndo: true, canRedo: false, closed: false }
await session.undo();       // the second edit's exact prior bytes come back
await session.rollback();   // or discard everything and close
```

One session per document at a time. The state lives beside the document inside
the module rather than in the JS object, so a second `doc.edit()` returns a
handle onto the same session rather than a competing one — two independent undo
stacks over one document could not both be right.

## Everything else you can edit

Text is the hard part; it is not the only part. All of these stage into the
same session and come off the same undo stack, so an image move and a text edit
are undone in reverse order and a rollback leaves the file byte-identical.

```js
const page = await doc.page(0);
const session = doc.edit();

// Images. `page.images()` reports where each one sits and whether this layer
// can touch it — `editable: false` means it is drawn inside a form XObject
// that other pages share, so moving it would move it everywhere.
const [logo] = page.images();
await session.moveImage(logo.id, { dx: 12, dy: -4 });
await session.scaleImage(logo.id, { sx: 1.5, sy: 1.5 });
await session.deleteImage(logo.id);

// Pages.
await session.deletePage(3);
await session.movePage(0, 2);      // navigation is fixed up with them

// Annotations, with the appearance stream written rather than left to the
// viewer — two viewers synthesising one differently is why annotations look
// right in one reader and wrong in another.
await session.addAnnotation({
  kind: "Highlight",
  rect: { x0: 72, y0: 690, x1: 300, y1: 712 },
  colour: [1, 0.9, 0.2],
  contents: "check this figure",
});
const [note] = await session.annotations();
await session.deleteAnnotation(note.id);

// Form fields, by fully-qualified name (`parent.child`).
const fields = await doc.formFields();
await session.setFieldValue(fields[0].name, "A. Ozdamar");
await session.flattenForms();      // one-way: the form stops being a form

// Table cells, on a table the layout layer detected.
await session.setCell(page.tables()[0].id, { row: 1, column: 2 }, "48.2");

const saved = await session.commit();
```

### Redaction is not a session operation

Deleting text moves glyphs off the page. Redaction makes the bytes stop
existing, which forces a full rewrite — and an undo stack that kept them for
you would defeat the point. So it happens on the document, immediately:

```js
const removed = await doc.redact("account 4021-8890");   // what it actually found
const saved = await doc.save();                          // mode: 'full-rewrite'

const report = await doc.verifyRedaction(saved.bytes, removed);
report.clean;        // no trace found in the places checked
report.notChecked;   // ...and the places it does not look. Read this one.
```

`verifyRedaction` takes the saved bytes rather than reading the open document
on purpose: asking the thing that performed the redaction whether it worked is
not a check.

### Encryption

```js
const weaknesses = await doc.protect({
  userPassword: "hunter2",
  ownerPassword: "s3kr1t",     // empty means *the same as the user password*
});
// [] — or 'empty-user-password', 'owner-password-equals-user',
//      'legacy-key-derivation' for aes-128
await doc.unprotect();
```

AES-256 by default; `strength: 'aes-128'` for readers older than Acrobat 9,
reported as weak because its key derivation is one MD5 plus fifty more. RC4 and
`/R` 5 are read for legacy files and never written.

The 32 bytes of key entropy come from `crypto.getRandomValues`, in JavaScript.
The WASM module has no random number generator and does not get one — your
platform's source is better than anything it could bundle, and this way you can
see where the key material came from. Pass `entropy` yourself if you have a
better source.

## The font problem, and what to do about it

A browser cannot see system fonts. Everything an editor draws it must already
have — and a PDF usually embeds only the letters it happened to use, so a
document that says "Hamburg" carries seven glyphs and cannot type an eighth.

That is the platform, not this library, and the useful response is to find out
early:

```js
const needs = await doc.fontRequirements();
// [{ pdfFont: 'ABCDEF+MinionPro-Regular', family: 'MinionPro-Regular',
//    embedded: true, subset: true, coverage: 'partial',
//    writableLatin: 54, needsSupplying: true }]
```

Call it straight after `open()` and fetch what the document needs before the
cursor appears, rather than discovering the problem on the user's first
keystroke. Then hand the font over:

```js
await doc.registerFont(minionBytes, { matchFor: "MinionPro-Regular" });
```

Nothing happens yet. When an edit needs a character the document's own embedded
font cannot draw, the outline is taken out of the font you supplied and injected
*into* the document's — so the page keeps one typeface and the edit reports
`reembedded` rather than a substitution a reader can see.

A character nobody can supply is not an error: the edit happens and the
character is named in `missingGlyphs`. Set `requireFidelity` above
`'overlaid'` to refuse instead.

## `requireFidelity`

The one setting worth reading about. An edit can succeed in four ways, and two
callers want opposite things from the same operation:

| Rung | Meaning |
|---|---|
| `exact` | the document's own glyphs, metrics and mechanism |
| `reembedded` | glyphs injected into the document's embedded font |
| `substituted` | a different typeface was used |
| `overlaid` | the original was masked and new text drawn over it |

A form-filler would rather have the field filled in a substituted typeface than
not filled. A contract-redlining tool would rather fail loudly than hand a
lawyer an amended clause set in a font the original never used — because that
outcome is invisible in a diff and visible in court.

Set the floor and every operation that cannot reach it fails instead of
degrading:

```js
const session = doc.edit({ requireFidelity: "exact" });
```

`reembedded` and above are reachable today. `substituted` and `overlaid` are in
the type and not yet produced, so a `requireFidelity: 'exact'` written now keeps
its meaning when they land.

## Loading the `.wasm` from somewhere else

The module ships beside the package and is found automatically. For a
CSP-strict page or a CDN:

```js
const doc = await Pdf.open(bytes, { wasmUrl: "https://cdn.example/rasura.wasm" });
```

## Running without a Worker

```js
const doc = await Pdf.open(bytes, { worker: false });
```

For a caller who manages their own Worker, or who is debugging and wants a
stack that does not stop at a message boundary. Same code, same answers — the
two transports share one implementation so they cannot drift.

## TypeScript

Declarations are hand-written and compiled under `strict`. There is no `any` in
the public surface; `unknown` appears once, on `PdfError.detail`, where it is
the honest type.

## Licence

MIT OR Apache-2.0.
