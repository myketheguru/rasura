// Pixel-diff harness. Spec 14.3, applied to Phase 4's injection.
//
//   Render before and after with pdfium at 150 dpi. Compare with a perceptual
//   threshold that ignores anti-aliasing noise (per-pixel ΔE below a small
//   bound) but catches any glyph position shift above a quarter pixel.
//
// The renderer here is pdf.js rather than pdfium. That is a deliberate
// substitution and worth being plain about: pdfium is what §14.3 names and what
// CI should use, but this runs the *comparison* logic against a renderer that
// is already vendored and can be executed now. The two are not equivalent —
// pdfium is Chrome's engine and pdf.js is Firefox's — so this is one engine's
// opinion, not the pixel-diff harness the spec asks for.
//
// What it actually checks is §2's first property, which is the one that matters
// most and the one no structural test can reach:
//
//   An edit on page 40 must not change the rendered output of any other page by
//   a single pixel.
//
// Applied here: injecting a glyph must not move the text that was already
// there. The `AB` drawn before injection must land on exactly the same pixels
// after it, with the new `C` appearing beyond them and nothing else changing.
//
//   node harness/textdiff/pixeldiff.mjs before.pdf after.pdf

import { readFileSync } from "node:fs";
import { createCanvas } from "canvas";
import { getDocument, VerbosityLevel } from "pdfjs-dist/legacy/build/pdf.mjs";

const DPI = 150;
const SCALE = DPI / 72;

// Spec 14.3: ignore anti-aliasing noise, catch a quarter-pixel glyph shift.
// A quarter-pixel shift of a hard edge changes that edge's coverage by roughly
// a quarter, so a channel delta well below that is noise and anything above it
// is movement.
const NOISE = 24; // out of 255
const MAX_MOVED_FRACTION = 0.0;

async function render(path) {
  const doc = await getDocument({
    data: new Uint8Array(readFileSync(path)),
    verbosity: VerbosityLevel.WARNINGS,
  }).promise;
  const page = await doc.getPage(1);
  const viewport = page.getViewport({ scale: SCALE });
  const canvas = createCanvas(Math.ceil(viewport.width), Math.ceil(viewport.height));
  const context = canvas.getContext("2d");
  context.fillStyle = "white";
  context.fillRect(0, 0, canvas.width, canvas.height);
  await page.render({ canvasContext: context, viewport, canvas }).promise;
  return context.getImageData(0, 0, canvas.width, canvas.height);
}

const [beforePath, afterPath] = process.argv.slice(2);
if (!beforePath || !afterPath) {
  console.error("usage: pixeldiff.mjs <before.pdf> <after.pdf>");
  process.exit(2);
}

const before = await render(beforePath);
const after = await render(afterPath);

if (before.width !== after.width || before.height !== after.height) {
  console.error(
    `FAIL: page size changed, ${before.width}x${before.height} -> ${after.width}x${after.height}`,
  );
  process.exit(1);
}

// Columns are compared rather than raw pixel counts: the injected glyph is
// *expected* to appear, so the question is not "did anything change" but
// "did anything change to the left of where the new glyph starts".
const width = before.width;
const height = before.height;
let firstChangedColumn = width;
let lastChangedColumn = -1;
let changedPixels = 0;

for (let y = 0; y < height; y++) {
  for (let x = 0; x < width; x++) {
    const i = (y * width + x) * 4;
    const d = Math.max(
      Math.abs(before.data[i] - after.data[i]),
      Math.abs(before.data[i + 1] - after.data[i + 1]),
      Math.abs(before.data[i + 2] - after.data[i + 2]),
    );
    if (d > NOISE) {
      changedPixels++;
      if (x < firstChangedColumn) firstChangedColumn = x;
      if (x > lastChangedColumn) lastChangedColumn = x;
    }
  }
}

// Where does the pre-existing text end? The rightmost column that has any ink
// in the *before* render.
let lastInkedColumn = -1;
for (let x = width - 1; x >= 0 && lastInkedColumn < 0; x--) {
  for (let y = 0; y < height; y++) {
    if (before.data[(y * width + x) * 4] < 200) {
      lastInkedColumn = x;
      break;
    }
  }
}

console.log(`render:            ${width}x${height} at ${DPI} dpi`);
console.log(`changed pixels:    ${changedPixels}`);
console.log(`changed columns:   ${firstChangedColumn}..${lastChangedColumn}`);
console.log(`original text ends at column ${lastInkedColumn}`);

let status = 0;

// The renderer is checked before its output is trusted. A rasteriser that
// produces a blank page reports "nothing changed" for every input, which is a
// harness that passes for ever and notices nothing -- the most expensive kind
// of green tick. pdf.js under node-canvas does exactly this: it accepts the
// document, executes the operator list, and paints no glyphs.
if (lastInkedColumn < 0) {
  console.error(
    "ABORT: the baseline render is blank, so this comparison would pass whatever " +
      "the input. The renderer, not the document, is at fault -- pdf.js does not " +
      "rasterise text under node-canvas. Spec 14.3 names pdfium; use that.",
  );
  process.exit(2);
}

if (changedPixels === 0) {
  console.error("FAIL: nothing changed at all -- the injected glyph did not draw.");
  status = 1;
} else if (firstChangedColumn <= lastInkedColumn) {
  // Something moved inside the region the original text occupied. That is the
  // non-locality failure: text the edit did not touch has shifted.
  const moved = changedPixels;
  console.error(
    `FAIL: ${moved} pixel(s) changed at or before column ${lastInkedColumn}, ` +
      `where the original text already was. The edit moved text it did not touch.`,
  );
  status = MAX_MOVED_FRACTION > 0 ? status : 1;
} else {
  console.log(
    `OK: every change is beyond column ${lastInkedColumn}. ` +
      `The text that was already there did not move by a pixel.`,
  );
}
process.exit(status);
