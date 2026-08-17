// Check the editor's hand-written view of the module against the module.
//
//   node harness/wasm-api/check.mjs
//
// `web/src/editor/rasura.ts` declares a `Wasm` interface covering "only what
// the editor calls", written by hand because wasm-bindgen's generated `.d.ts`
// describes the ABI rather than the API. That reason is sound and the file is
// worth keeping. What it lacks is anything checking the hand-written half
// against the generated one.
//
// It was wrong, and editing in the demo had never worked. The interface
// declared:
//
//     replaceText(handle, page, region, index, from, to, text): Outcome
//
// splitting a paragraph id that was never split into `region` and `index`. The
// module declares six parameters with a single `paragraph: number`. The call
// site obliged the declaration, passed two `undefined`s and one argument too
// many, and the replacement string arrived where a number was expected: the
// module faulted on every edit. TypeScript could not help. It checks callers
// against a declaration; it has no idea whether the declaration is true.
//
// Arity only, deliberately. The types either side describe different things --
// `unknown` against `any`, `Outcome` against `any` -- and comparing them would
// produce noise nobody would read. Every mismatch found so far has been a
// parameter count, because that is what a hand-written signature gets wrong.

import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = process.cwd();
const DECL = ['target/pkg/nodejs/rasura_wasm.d.ts', 'target/pkg/web/rasura_wasm.d.ts']
  .map((p) => join(ROOT, p))
  .find((p) => existsSync(p));

if (!DECL) {
  // Not a pass. The module is a build output, and a check that quietly
  // succeeds when the thing it compares against is missing is the shape of
  // check this project keeps having to fix.
  console.error('no rasura_wasm.d.ts under target/pkg -- run ./crates/rasura-wasm/build.sh first');
  process.exit(1);
}

const HAND = join(ROOT, 'web/src/editor/rasura.ts');

/* --- what the module declares -------------------------------------------- */

const generated = new Map();
for (const m of readFileSync(DECL, 'utf8').matchAll(
  /^export function (\w+)\(([^)]*)\)/gm,
)) {
  generated.set(m[1], params(m[2]));
}
if (generated.size < 20) {
  console.error(`only ${generated.size} functions parsed from ${DECL}; its shape changed`);
  process.exit(1);
}

/* --- what the editor declares -------------------------------------------- */

const source = readFileSync(HAND, 'utf8');
const start = source.indexOf('export interface Wasm {');
if (start === -1) {
  console.error('no `export interface Wasm` in web/src/editor/rasura.ts');
  process.exit(1);
}
const body = source.slice(start, source.indexOf('\n}', start));

const declared = new Map();
// A method line, possibly wrapping across several lines before the `)`.
for (const m of body.matchAll(/^ {2}(\w+)\(([\s\S]*?)\):/gm)) {
  declared.set(m[1], params(m[2]));
}
if (declared.size < 10) {
  console.error(`only ${declared.size} methods parsed from the Wasm interface; its shape changed`);
  process.exit(1);
}

/* --- compare -------------------------------------------------------------- */

const problems = [];

for (const [name, mine] of declared) {
  const theirs = generated.get(name);
  if (!theirs) {
    problems.push(`${name}: declared here, but the module exports no such function`);
    continue;
  }
  // Trailing optionals may legitimately be omitted, so a range is allowed.
  const required = theirs.filter((p) => !p.optional).length;
  const total = theirs.length;
  const count = mine.length;
  if (count < required || count > total) {
    problems.push(
      `${name}: declared ${count} parameter(s), the module takes ` +
        (required === total ? `${total}` : `${required} to ${total}`) +
        `\n      module: (${theirs.map((p) => p.text).join(', ')})` +
        `\n      editor: (${mine.map((p) => p.text).join(', ')})`,
    );
  }
}

if (problems.length) {
  console.error("the editor's declaration of the module disagrees with the module:\n");
  for (const p of problems) console.error(`  ${p}`);
  console.error(`\n${problems.length} mismatch(es).`);
  process.exit(1);
}

console.log(
  `the editor's ${declared.size} declared calls match the module (${generated.size} exported).`,
);

/** Split a parameter list, ignoring commas inside nested braces or brackets. */
function params(text) {
  const out = [];
  let depth = 0;
  let current = '';
  for (const ch of text) {
    if ('({[<'.includes(ch)) depth += 1;
    if (')}]>'.includes(ch)) depth -= 1;
    if (ch === ',' && depth === 0) {
      out.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  out.push(current);
  return out
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => ({ text: p.replace(/\s+/g, ' '), optional: /^\w+\?/.test(p) }));
}
