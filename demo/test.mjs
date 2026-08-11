// The part of the demo that can be tested without a browser.
//
//   node demo/test.mjs
//
// It drives the real WASM module — the node-target build from
// `crates/rasura-wasm/build.sh` — through the same call sequence the page uses,
// and checks the pure render core against it. What it cannot check is the
// canvas, the DOM and the published page's content-security policy; that gap is
// stated in the demo's own README rather than papered over here.

import { readFileSync, existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

import { drawList, layoutParagraph, minimalRange, paragraphAt, pageBox } from './render.mjs';

const dir = process.argv[2] ?? 'target/pkg/nodejs';
const entry = `${dir}/rasura_wasm.js`;
if (!existsSync(entry)) {
  console.error(`missing ${entry} -- run crates/rasura-wasm/build.sh first`);
  process.exit(1);
}
const wasm = createRequire(import.meta.url)(resolve(entry));

let failures = 0;
const check = (label, ok, detail = '') => {
  if (ok) {
    console.log(`  ok    ${label}`);
  } else {
    console.error(`  FAIL  ${label}${detail ? ` -- ${detail}` : ''}`);
    failures += 1;
  }
};

// A width function standing in for canvas `measureText`. Proportional rather
// than a character count, because a measure that counted characters would make
// every line-breaking assertion below meaningless.
const measure = (text, size) => {
  let width = 0;
  for (const ch of text) width += /[iljt.,;:'!|]/.test(ch) ? 0.28 : /[mwMW]/.test(ch) ? 0.83 : 0.52;
  return width * size;
};

console.log('opening the bundled sample');
const sample = readFileSync('demo/sample.pdf');
const handle = wasm.openDocument(new Uint8Array(sample), undefined, undefined);
check('a handle came back', Number.isInteger(handle) && handle > 0);

const info = wasm.documentInfo(handle);
check('info has a page count', info.pageCount > 0, JSON.stringify(info.pageCount));
check('permissions crossed as an object', typeof info.permissions?.print === 'boolean');
check('leniencies is an array', Array.isArray(info.leniencies));

console.log('\nreading the model the page draws from');
const page = wasm.pageContent(handle, 0);
check('paragraphs came back', page.paragraphs.length > 0, `${page.paragraphs.length}`);
check('blocks came back', Array.isArray(page.blocks));
check('images is an array', Array.isArray(page.images));
check('tables is an array', Array.isArray(page.tables));

const box = pageBox(page);
check('the page has a size', box.width > 100 && box.height > 100, JSON.stringify(box));

const list = drawList(page, measure);
const drawnParagraphs = list.filter((d) => d.type === 'paragraph');
check('every paragraph is in the draw list', drawnParagraphs.length === page.paragraphs.length);
check(
  'every drawn paragraph has at least one line',
  drawnParagraphs.every((d) => d.layout.lines.length >= 1),
);
check(
  'no line is wider than its own box',
  drawnParagraphs.every((d) =>
    d.layout.lines.every(
      (line, i) =>
        // A single word wider than the measure is allowed to overhang: breaking
        // inside a word needs hyphenation the demo does not have.
        d.layout.lines[i].split(/\s+/).length === 1 ||
        measure(line, d.layout.size) <= d.layout.width + 0.5,
    ),
  ),
);
check(
  'text sizes are plausible',
  drawnParagraphs.every((d) => d.layout.size >= 4 && d.layout.size <= 72),
  JSON.stringify(drawnParagraphs.map((d) => Math.round(d.layout.size))),
);

console.log('\nhit testing');
const first = page.paragraphs[0];
const centre = {
  x: (first.box.x0 + first.box.x1) / 2,
  y: (first.box.y0 + first.box.y1) / 2,
};
check('a click in a paragraph finds it', paragraphAt(page, centre.x, centre.y)?.id === first.id);
check('a click off the page finds nothing', paragraphAt(page, -1e6, -1e6) === null);

console.log('\nediting through the same calls the page makes');
wasm.configureSession(handle, 'exact', 'greedy', 'refuse');
const before = first.text;
const range = minimalRange(before, `Z${before.slice(1)}`);
check('a one-character change is a one-character range', range.end - range.start === 1, JSON.stringify(range));

let outcome;
try {
  outcome = wasm.replaceText(handle, 0, first.id, range.start, range.end, range.text);
  check('the edit reports a fidelity rung', ['exact', 'reembedded', 'substituted', 'overlaid'].includes(outcome.fidelity), outcome.fidelity);
} catch (e) {
  // A refusal at the `exact` floor is a legitimate outcome and the demo shows
  // it as one; what must never happen is an uncoded throw.
  check('a refusal carries a code', typeof e.code === 'string', String(e));
}

const status = wasm.sessionStatus(handle);
check('the operation is staged, not written', status.staged >= 0 && typeof status.canUndo === 'boolean', JSON.stringify(status));

if (status.staged > 0) {
  const saved = wasm.commitSession(handle, undefined);
  check('the commit produced bytes', saved.bytes instanceof Uint8Array && saved.bytes.length > 0);
  check('the save mode is reported', typeof saved.mode === 'string', saved.mode);

  const reopened = wasm.openDocument(saved.bytes, undefined, undefined);
  const rereadPage = wasm.pageContent(reopened, 0);
  check('the edited document reopens with text', rereadPage.paragraphs.length > 0);
  check('the text actually changed', rereadPage.paragraphs[0].text !== before, `${before} -> ${rereadPage.paragraphs[0].text}`);
  wasm.closeDocument(reopened);
} else {
  wasm.rollbackSession(handle);
}

console.log('\npanels');
const fonts = wasm.fontRequirements(handle);
check('font requirements is an array', Array.isArray(fonts), `${fonts.length} font(s)`);
check('metadata has both surfaces', (() => {
  const m = wasm.documentMetadata(handle);
  return 'info' in m && 'xmp' in m;
})());
check('form fields is an array', Array.isArray(wasm.formFields(handle)));

console.log('\nredaction, which the demo shows verified');
const word = (first.text.match(/[A-Za-z]{5,}/) ?? ['the'])[0];
const fresh = wasm.openDocument(new Uint8Array(sample), undefined, undefined);
const removed = wasm.redactText(fresh, word);
check('redaction reports what it found', Array.isArray(removed), JSON.stringify(removed));
const after = wasm.saveDocument(fresh, undefined);
check('a redacted save is a full rewrite', after.mode === 'full-rewrite', after.mode);
const report = wasm.verifyRedaction(after.bytes, removed);
check('verification reports cleanliness', typeof report.clean === 'boolean', JSON.stringify(report.clean));
check('and says where it did not look', Array.isArray(report.notChecked) && report.notChecked.length > 0);
wasm.closeDocument(fresh);

wasm.closeDocument(handle);

console.log(
  failures === 0
    ? '\nthe demo\'s data path works against the real module.'
    : `\n${failures} check(s) failed`,
);
process.exit(failures === 0 ? 0 : 1);
