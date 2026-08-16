// Check the JavaScript in the documentation against the JavaScript that exists.
//
//   node harness/docs-api/check.mjs
//
// This exists because the headline example in README.md never ran. It called
// `doc.replaceText(...)` and `doc.commit()`, which are `Session` methods and
// have never been on `Document`, and read `page.paragraphs[0]`, where
// `paragraphs` is a method — so the property access yielded the function and
// `.length` yielded its arity. Every mutation example in every guide had the
// same shape. It shipped in the npm tarball twice.
//
// Nothing caught it because nothing compared the two. `js/test/api.test.mjs`
// uses the API correctly and passes; the prose used a different API and no
// check knew the prose existed. Tests prove the library works. They cannot
// prove the documentation describes it.
//
// The surface is read out of `js/src/index.js` rather than listed here, so this
// cannot drift the way a hand-kept list would: move a method between classes
// and the docs are re-checked against where it actually lives.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = process.cwd();
const SOURCE = join(ROOT, 'js/src/index.js');

/* --- what actually exists ---------------------------------------------------- */

const src = readFileSync(SOURCE, 'utf8');

/** Method names declared in one exported class. */
function methodsOf(className) {
  const start = src.indexOf(`export class ${className}`);
  if (start === -1) throw new Error(`class ${className} not found in ${SOURCE}`);
  const after = src.slice(start + 1);
  const nextClass = after.indexOf('\nexport class ');
  const body = nextClass === -1 ? after : after.slice(0, nextClass);
  const names = new Set();
  // `if`, `for` and friends sit at the same indentation inside a method body
  // and match the same shape as a declaration. Excluded by name rather than by
  // a cleverer regex, because a parser here would be a second JavaScript
  // implementation and this only has to be right about one file.
  const KEYWORDS = new Set(['constructor', 'if', 'for', 'while', 'switch', 'catch', 'return']);
  for (const m of body.matchAll(/^ {2}(?:async\s+)?([a-zA-Z_][\w]*)\s*\(/gm)) {
    if (!KEYWORDS.has(m[1])) names.add(m[1]);
  }
  return names;
}

const DOCUMENT = methodsOf('Document');
const PAGE = methodsOf('Page');
const SESSION = methodsOf('Session');

// A sanity floor. If the parse silently returns nothing, every check below
// passes and the harness becomes decoration.
if (DOCUMENT.size < 8 || PAGE.size < 4 || SESSION.size < 10) {
  console.error(
    `parsed ${DOCUMENT.size} Document, ${PAGE.size} Page, ${SESSION.size} Session methods ` +
      `-- the shape of index.js changed and this check cannot be trusted`,
  );
  process.exit(1);
}

/** Session-only: calling these on a document is the mistake this exists for. */
const SESSION_ONLY = [...SESSION].filter((m) => !DOCUMENT.has(m) && !m.startsWith('_'));

/** Page methods, which read as properties if you forget the parentheses. */
const PAGE_CALLABLE = [...PAGE].filter((m) => !m.startsWith('_'));

/* --- where the documentation is ---------------------------------------------- */

const TARGETS = ['README.md', 'web/src/pages/docs', 'web/src/pages/landing.tsx'];

// `web/src/pages/editor.tsx` is deliberately out of scope, and the reason is
// the whole point of being careful here: the editor imports the *raw* module
// from `wasm/rasura_wasm.js`, not the npm package. At that layer a page is a
// plain object and `page.paragraphs` really is an array. Checking it against
// the wrapper's surface flags three correct lines, and "fixing" them would
// break a page that works. The same goes for `harness/` and `demo/`, which
// drive the module directly for the same reason.
function files(path, out = []) {
  const full = join(ROOT, path);
  if (statSync(full).isDirectory()) {
    for (const e of readdirSync(full)) files(join(path, e), out);
  } else if (/\.(md|tsx|ts)$/.test(path)) {
    out.push(path);
  }
  return out;
}

/* --- the checks --------------------------------------------------------------- */

const problems = [];

/**
 * Whether a line is inside a non-JavaScript code block.
 *
 * The Rust facade has `doc.page_count()`, which is correct Rust and not a
 * JavaScript method at all. Flagging it would be the check crying wolf, and a
 * checker that reports a line nobody should change is one people learn to run
 * with their eyes closed.
 *
 * Markdown fences carry the language; the site marks blocks with
 * `<Code lang="rust">`. Both are tracked with the same flag, reset when the
 * block ends.
 */
function nonJsRegions(lines, rel) {
  const isMarkdown = rel.endsWith('.md');
  const out = new Array(lines.length).fill(false);
  let inside = false;
  lines.forEach((line, i) => {
    if (isMarkdown) {
      const fence = line.match(/^```(\w*)/);
      if (fence) {
        // Opening a fence sets the state; closing one (empty tag) clears it.
        inside = fence[1] ? !['js', 'javascript', 'ts', 'typescript', ''].includes(fence[1]) : false;
        out[i] = true;
        return;
      }
    } else {
      if (/<Code\b[^>]*lang="(?!js|ts|tsx)/.test(line)) inside = true;
      else if (/<\/Code>|`}<\/Code>/.test(line)) { out[i] = inside; inside = false; return; }
    }
    out[i] = inside;
  });
  return out;
}

for (const rel of TARGETS.flatMap((t) => files(t))) {
  const text = readFileSync(join(ROOT, rel), 'utf8');
  const lines = text.split(/\r?\n/);
  const skip = nonJsRegions(lines, rel);

  lines.forEach((line, i) => {
    if (skip[i]) return;
    const at = `${rel}:${i + 1}`;

    // Anything called on a document, checked against every name that exists.
    //
    // Two failures, and the second is the one a list of known-bad names would
    // have missed: `doc.replaceText()` is a real method on the wrong class,
    // and `doc.redactText()` / `doc.configureSession()` / `doc.sessionStatus()`
    // were names that never existed anywhere. Prose invents plausible methods,
    // so the check has to be "is this real" rather than "is this one of the
    // mistakes we already found".
    for (const m of line.matchAll(/\bdoc(?:ument)?\.([a-zA-Z_]\w*)\s*\(/g)) {
      const name = m[1];
      if (DOCUMENT.has(name)) continue;
      if (SESSION.has(name)) {
        problems.push(`${at}  doc.${name}() -- ${name} is on Session, from doc.edit()`);
      } else {
        problems.push(`${at}  doc.${name}() -- no such method on Document or Session`);
      }
    }

    for (const m of PAGE_CALLABLE) {
      // `page.paragraphs[0]` or `page.paragraphs.length` -- a method read as a
      // property. Silent: the first is undefined, the second is the arity.
      if (new RegExp(`\\bpage\\.${m}\\s*[[.]`).test(line) && !new RegExp(`\\bpage\\.${m}\\s*\\(`).test(line)) {
        problems.push(`${at}  page.${m} used as a property -- it is page.${m}()`);
      }
    }

    // The range shape that never existed. The real one is
    // `replaceText(id, { start, end }, text, { page })`.
    if (/\{\s*page:\s*\d+\s*,\s*paragraph:/.test(line)) {
      problems.push(`${at}  { page, paragraph, from, to } -- the range is { start, end }`);
    }
    if (/\bfrom:\s*\d+\s*,\s*to:\s*\d+/.test(line)) {
      problems.push(`${at}  { from, to } -- the range is { start, end }`);
    }
  });
}

/* --- report -------------------------------------------------------------------- */

if (problems.length) {
  console.error('the documentation calls an API that does not exist:\n');
  for (const p of problems) console.error(`  ${p}`);
  console.error(`\n${problems.length} problem(s).`);
  console.error('\nThe surface, read from js/src/index.js:');
  console.error(`  Document: ${[...DOCUMENT].filter((m) => !m.startsWith('_')).join(', ')}`);
  console.error(`  Page:     ${PAGE_CALLABLE.join(', ')}`);
  console.error(`  Session:  ${SESSION_ONLY.join(', ')}`);
  process.exit(1);
}

console.log(
  `the documentation matches the API ` +
    `(${DOCUMENT.size} Document, ${PAGE.size} Page, ${SESSION.size} Session methods).`,
);
