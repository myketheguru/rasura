// Extract text and glyph positions with pdf.js, as the reference side of the
// Phase 2 differential.
//
//   node harness/textdiff/extract-pdfjs.mjs <dir> <out.jsonl> [maxPages]
//
// One JSON object per line: { file, page, text, items: [{ str, x, y }] }.
// JSONL rather than one big document so a crash on file 700 does not cost the
// first 699.
//
// pdf.js is already vendored by corpus/fetch.sh, so this uses that checkout
// rather than adding an npm dependency that could drift from the corpus.

import { readFileSync, readdirSync, writeFileSync, appendFileSync, existsSync } from "node:fs";
import { join, basename } from "node:path";
import { pathToFileURL } from "node:url";

const dir = process.argv[2] ?? "corpus/external/pdfjs/test/pdfs";
const out = process.argv[3] ?? "corpus/pdfjs-text.jsonl";
const maxPages = Number(process.argv[4] ?? 5);

// `pdfjs-dist` from npm rather than the corpus checkout: corpus/fetch.sh does a
// sparse checkout of test/pdfs only, which has no built library in it. The
// legacy build is the one that runs under Node without a DOM.
let pdfjs;
try {
  pdfjs = await import("pdfjs-dist/legacy/build/pdf.mjs");
} catch {
  const local = "harness/textdiff/node_modules/pdfjs-dist/legacy/build/pdf.mjs";
  if (existsSync(local)) {
    pdfjs = await import(pathToFileURL(local).href);
  }
}
if (!pdfjs) {
  console.error(
    "pdf.js not importable. Run `npm install` in harness/textdiff first.\n" +
      "Skipping the reference side.",
  );
  process.exitCode = 2;
}

if (pdfjs) {
  const { getDocument } = pdfjs;
  writeFileSync(out, "");

  const files = readdirSync(dir).filter((f) => f.endsWith(".pdf")).sort();
  let ok = 0;
  let failed = 0;

  for (const name of files) {
    let doc;
    try {
      doc = await getDocument({
        data: new Uint8Array(readFileSync(join(dir, name))),
        // Quiet, and comparable: no rendering, no fonts on disk.
        verbosity: 0,
        disableFontFace: true,
        useSystemFonts: false,
      }).promise;
    } catch {
      failed++;
      continue;
    }

    try {
      const pages = Math.min(doc.numPages, maxPages);
      for (let i = 1; i <= pages; i++) {
        const page = await doc.getPage(i);
        const content = await page.getTextContent();
        const items = content.items
          .filter((it) => typeof it.str === "string")
          .map((it) => ({
            str: it.str,
            // transform is [a b c d e f]; e,f is the origin in PDF user space
            // with y up. The Rust side reports device space with y down, so the
            // comparison normalises rather than assuming either convention.
            x: it.transform?.[4] ?? 0,
            y: it.transform?.[5] ?? 0,
          }));
        // The whole view box, not just its top edge. Rasura's device space
        // is relative to the crop box *origin*, so a page whose crop box does
        // not start at (0,0) needs both corners to convert correctly -- using
        // only the height silently biases every such page.
        const view = page.view ?? [0, 0, 0, 0];
        appendFileSync(
          out,
          JSON.stringify({
            file: basename(name),
            page: i - 1,
            x0: view[0],
            y0: view[1],
            x1: view[2],
            y1: view[3],
            text: items.map((it) => it.str).join(""),
            items,
          }) + "\n",
        );
      }
      ok++;
    } catch {
      failed++;
    } finally {
      await doc.destroy?.();
    }
  }

  console.log(`pdf.js: ${ok} file(s) extracted, ${failed} failed -> ${out}`);
}
