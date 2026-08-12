// What can be checked about a browser page without a browser.
//
//   node demo/lint.mjs
//
// Two classes of mistake would break the demo the instant it loads, and both
// are decidable from the source: an element id the script reaches for that the
// markup does not define, and a name imported from the WASM glue that the glue
// does not export. Neither is caught by `node --check`, and neither survives to
// a second run once it is caught here.
//
// It does not check the canvas, the layout, or whether anything looks right.
// That gap is real and is stated in demo/README.md rather than implied away.

import { existsSync, readFileSync } from 'node:fs';

const app = readFileSync('demo/app.mjs', 'utf8');
const html = readFileSync('demo/index.html', 'utf8');
// Wherever the module build left it. Not demo/dist: the workflow lints before
// it assembles, and reading the assembled copy made this pass locally and fail
// on a clean checkout -- which is the exact failure a lint is supposed to
// prevent rather than produce.
const gluePath = ['target/pkg/web/rasura_wasm.js', 'js/wasm/rasura_wasm.js', 'demo/dist/rasura_wasm.js'].find(existsSync);
if (!gluePath) {
  console.error('no rasura_wasm.js found -- run crates/rasura-wasm/build.sh first');
  process.exit(1);
}
const glue = readFileSync(gluePath, 'utf8');
const render = readFileSync('demo/render.mjs', 'utf8');

let failures = 0;
const check = (label, ok, detail = '') => {
  if (ok) console.log(`  ok    ${label}`);
  else {
    console.error(`  FAIL  ${label}${detail ? ` -- ${detail}` : ''}`);
    failures += 1;
  }
};

// --- element ids -------------------------------------------------------------

const wanted = [...app.matchAll(/\$\('([^']+)'\)/g)].map((m) => m[1]);
const defined = new Set([...html.matchAll(/\bid="([^"]+)"/g)].map((m) => m[1]));
const missingIds = [...new Set(wanted)].filter((id) => !defined.has(id));
check('every element the script reaches for exists in the markup', missingIds.length === 0, missingIds.join(', '));

// The reverse matters too, because an id defined and referenced by nothing is
// usually a rename that only got done on one side. An id the stylesheet selects
// is referenced — that is what `#inspector` is, and flagging it would train the
// reader to ignore this check, which is worse than not having it.
const css = readFileSync('demo/style.css', 'utf8');
const styled = new Set([...css.matchAll(/#([\w-]+)/g)].map((m) => m[1]));
const unused = [...defined].filter((id) => !wanted.includes(id) && !styled.has(id));
check('no element id is defined and referenced by nothing', unused.length === 0, unused.join(', '));

// --- tabs --------------------------------------------------------------------

const tabsInHtml = new Set([...html.matchAll(/data-tab="([^"]+)"/g)].map((m) => m[1]));
const tabsInApp = new Set(
  [...app.matchAll(/state\.tab === '([^']+)'/g)].map((m) => m[1]),
);
const missingTabs = [...tabsInApp].filter((t) => !tabsInHtml.has(t));
check('every tab the script renders has a button', missingTabs.length === 0, missingTabs.join(', '));

// --- imports -----------------------------------------------------------------

const importBlock = app.match(/import init, \{([\s\S]*?)\} from '\.\/rasura_wasm\.js';/);
check('the glue import is present and parseable', Boolean(importBlock));

if (importBlock) {
  const names = importBlock[1]
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
  const exported = new Set([...glue.matchAll(/^export function (\w+)/gm)].map((m) => m[1]));
  const missing = names.filter((n) => !exported.has(n));
  check(`all ${names.length} imported WASM functions are exported by the glue`, missing.length === 0, missing.join(', '));

  // A function called but never imported is the same bug seen from the other
  // side, and is the one a rename actually produces.
  const called = new Set([...app.matchAll(/(?<![.\w])(\w+)\(state\.handle/g)].map((m) => m[1]));
  const notImported = [...called].filter((n) => !names.includes(n) && exported.has(n));
  check('every WASM function called is imported', notImported.length === 0, notImported.join(', '));
}

// --- who supplies the module path --------------------------------------------
//
// The bug this exists for shipped, deployed green, and never started once in a
// browser. `crates/rasura-wasm/build.sh` passes `--omit-default-module-path`,
// whose entire effect is to delete the `import.meta.url` fallback from the
// glue; the page called `init()` with no argument on the strength of a comment
// saying the glue would resolve the path itself. It reached
// `WebAssembly.instantiate(undefined, …)`.
//
// Both halves are right here in the two files, so this is decidable: if the
// glue has no default, the caller must pass one.
const glueHasDefault = /module_or_path\s*=\s*new URL\(/.test(glue);
const appPassesPath = /\binit\(\s*\{[^}]*module_or_path/.test(app);
check(
  glueHasDefault
    ? 'the glue defaults the module path, so init() may be called bare'
    : 'the glue has no default module path, so the app passes one',
  glueHasDefault || appPassesPath,
  'built with --omit-default-module-path; call init({ module_or_path: new URL(...) })',
);

const renderImport = app.match(/import \{([^}]*)\} from '\.\/render\.mjs';/);
check('the render import is present', Boolean(renderImport));
if (renderImport) {
  const names = renderImport[1].split(',').map((s) => s.trim()).filter(Boolean);
  const exported = new Set([...render.matchAll(/^export function (\w+)/gm)].map((m) => m[1]));
  const missing = names.filter((n) => !exported.has(n));
  check('render.mjs exports everything the app imports', missing.length === 0, missing.join(', '));
}

// --- deployment constraints --------------------------------------------------

check('no absolute paths — Pages serves from a subdirectory', !/(src|href)="\//.test(html));
check(
  'the sample and the module are fetched relatively',
  app.includes("fetch('./sample.pdf')") && app.includes("from './rasura_wasm.js'"),
);
check('no external origins are referenced', !/https?:\/\//.test(html.replace(/<!--[\s\S]*?-->/g, '')));

console.log(failures === 0 ? '\ndemo lint clean.' : `\n${failures} problem(s)`);
process.exit(failures === 0 ? 0 : 1);
