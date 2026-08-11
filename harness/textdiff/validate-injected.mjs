// Validate an injected-font PDF with an independent implementation.
//
// Spec 17, Phase 4's exit criterion: "injection round-trips validate in all
// four viewers". This is the first of them. Everything else in the font layer
// checks its own output -- rasura's parsers re-reading rasura's writers
// -- which catches a great deal and proves nothing to anyone else.
//
// pdf.js has no stake in agreeing with us. If it opens the document, finds the
// embedded font, builds glyphs from it and extracts the text the page draws,
// then the injection produced a file another implementation accepts.
//
//   node harness/textdiff/validate-injected.mjs target/seedcheck/injected-truetype.pdf
//   node harness/textdiff/validate-injected.mjs target/realfont/after.pdf HamburgÉ
//   node harness/textdiff/validate-injected.mjs target/redact/after.pdf --absent Wolfgang
//   node harness/textdiff/validate-injected.mjs target/protect/aes256.pdf \
//       --password hunter2 "Account balance: 4,200"
//
// `--absent` inverts the question for redaction, and is a separate mode rather
// than a negated exact match on purpose: "the page text is not exactly
// Wolfgang" is true of every PDF ever written, so a check phrased that way
// passes whether or not the redaction worked.

import { readFileSync } from "node:fs";
import { getDocument, VerbosityLevel } from "pdfjs-dist/legacy/build/pdf.mjs";

const argv = process.argv.slice(2);
// `--password X` may appear anywhere; the rest is positional as before.
const pwAt = argv.indexOf("--password");
const password = pwAt >= 0 ? argv[pwAt + 1] : undefined;
if (pwAt >= 0) argv.splice(pwAt, 2);
process.argv = [process.argv[0], process.argv[1], ...argv];

const path = process.argv[2];
// The synthesised fixture draws "ABC"; the real-font one draws something else.
// The expected text is an argument rather than a guess, because "extracted
// *something*" is not the claim -- the claim is that the character which did
// not exist in the font before comes back correctly.
const absent = process.argv[3] === "--absent" ? process.argv[4] : null;
const expected = absent === null ? (process.argv[3] ?? "ABC") : null;
if (!path || (process.argv[3] === "--absent" && !absent)) {
  console.error("usage: validate-injected.mjs <file.pdf> [expected-text | --absent text]");
  process.exit(2);
}

// Warnings are the interesting part: pdf.js repairs a lot silently, and a
// repaired file is not a valid one.
const problems = [];
const origWarn = console.warn;
const origError = console.error;
console.warn = (...a) => problems.push(`warn: ${a.join(" ")}`);
console.error = (...a) => problems.push(`error: ${a.join(" ")}`);

let status = 0;
try {
  const doc = await getDocument({
    data: new Uint8Array(readFileSync(path)),
    verbosity: VerbosityLevel.WARNINGS,
    ...(password === undefined ? {} : { password }),
  }).promise;

  const page = await doc.getPage(1);
  const content = await page.getTextContent();
  const text = content.items.map((i) => i.str).join("");

  // The operator list is the part that matters: building it forces pdf.js to
  // parse the embedded font program and translate every glyph. Text extraction
  // alone can succeed straight from /ToUnicode without the program being
  // touched at all, so passing that and nothing else would prove little.
  const ops = await page.getOperatorList();

  origWarn(`pages:     ${doc.numPages}`);
  origWarn(`text:      ${JSON.stringify(text)}`);
  origWarn(`operators: ${ops.fnArray.length}`);

  if (absent !== null) {
    if (text.includes(absent)) {
      origError(`FAIL: ${JSON.stringify(absent)} is still extractable: ${JSON.stringify(text)}`);
      status = 1;
    }
    // A page that extracted nothing would satisfy "does not contain" without
    // proving anything, so the document has to have said something first.
    if (text.length === 0) {
      origError("FAIL: no text extracted at all, so the absence proves nothing");
      status = 1;
    }
  } else if (text !== expected) {
    origError(`FAIL: expected ${JSON.stringify(expected)}, got ${JSON.stringify(text)}`);
    status = 1;
  }
  if (problems.length) {
    origError(`FAIL: pdf.js reported ${problems.length} problem(s):`);
    for (const p of problems.slice(0, 10)) origError(`  ${p}`);
    status = 1;
  }
  if (status === 0 && absent !== null) {
    origWarn(`OK: pdf.js read ${text.length} character(s) and none of them are the redacted text.`);
  } else if (status === 0) {
    origWarn("OK: pdf.js opened the document, built the injected font, and read the text.");
  }
} catch (e) {
  origError(`FAIL: ${e?.message ?? e}`);
  status = 1;
}

process.exit(status);
