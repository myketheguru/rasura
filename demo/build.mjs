// Assemble the demo into a directory GitHub Pages can serve.
//
//   ./crates/rasura-wasm/build.sh      # produces the module
//   node demo/build.mjs                # produces demo/dist/
//
// Nothing is bundled and nothing is minified. The output is the source files
// plus the two binaries, because a demo whose point is "look how little is
// happening here" should not arrive as a build artefact nobody can read.
//
// Pages constraints this respects:
//   * relative paths only — the site is served from /<repo>/, not from /
//   * no COOP/COEP — Pages cannot set headers, and the single-threaded build
//     (§12.1) is what makes that fine
//   * .nojekyll — otherwise Jekyll eats files and directories beginning with _

import { copyFileSync, mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const root = process.cwd();
const out = join(root, 'demo', 'dist');
const wasmDir = existsSync(join(root, 'target/pkg/web'))
  ? join(root, 'target/pkg/web')
  : join(root, 'js/wasm');

// Authored by cargo run -p rasura-flow --example sample and committed.
// Not a corpus file: everything under corpus/external is other people's work
// under other people's licences, and a public repository must not carry it.
const SAMPLE = 'demo/sample.pdf';

function need(path) {
  if (!existsSync(path)) {
    console.error(`missing ${path}\nrun ./crates/rasura-wasm/build.sh first`);
    process.exit(1);
  }
  return path;
}

mkdirSync(out, { recursive: true });

for (const file of ['index.html', 'style.css', 'app.mjs', 'render.mjs']) {
  copyFileSync(need(join(root, 'demo', file)), join(out, file));
}

// The `--target web` build: an ES module plus the binary beside it. The glue
// resolves the binary against its own module URL, so the two must stay
// together and neither may be renamed.
copyFileSync(need(join(wasmDir, 'rasura_wasm.js')), join(out, 'rasura_wasm.js'));
copyFileSync(need(join(wasmDir, 'rasura_wasm_bg.wasm')), join(out, 'rasura_wasm_bg.wasm'));

copyFileSync(need(join(root, SAMPLE)), join(out, 'sample.pdf'));
writeFileSync(join(out, '.nojekyll'), '');

// --- the single-file variant -------------------------------------------------
//
// One HTML file with the module, the sample and every source inlined. Not what
// Pages serves — it is nearly twice the size, because base64 costs a third and
// nothing can be cached separately — but it is what can be handed to someone as
// an attachment, or published somewhere that serves exactly one file.
//
// The transform is deliberately blunt: strip `export` so the declarations land
// in one module scope, drop the two import statements, and swap the two fetches
// for base64. Anything cleverer would be a bundler, and a demo whose point is
// how little is happening should not need one.

// `String.replace` with a pattern that matches nothing returns the input and
// says nothing about it. Every substitution below is load-bearing, so a missed
// one has to be an error: rewording a line in app.mjs would otherwise ship a
// standalone file that still fetched, still called the wrong initialiser, and
// looked fine until someone opened it.
function replaceOnce(text, pattern, replacement, what) {
  const out = text.replace(pattern, replacement);
  if (out === text) throw new Error(`standalone: nothing matched for ${what}`);
  return out;
}

function standalone() {
  const read = (p) => readFileSync(p, 'utf8');

  const glue = read(join(out, 'rasura_wasm.js'))
    .replace(/^export function /gm, 'function ')
    .replace(/^export \{[^}]*\};?\s*$/gm, '');

  const render = read(join(out, 'render.mjs')).replace(/^export function /gm, 'function ');

  let app = read(join(out, 'app.mjs'));
  app = replaceOnce(app, /^import \{[^}]*\} from '\.\/render\.mjs';\s*$/gm, '', 'the render import');
  app = replaceOnce(app, /^import init, \{[\s\S]*?\} from '\.\/rasura_wasm\.js';\s*$/gm, '', 'the glue import');
  // The whole init call, however it is spelled: the page fetches the module
  // beside it and this file has it inlined, so the argument differs too.
  app = replaceOnce(
    app,
    /await init\(\{[^}]*\}\);/,
    'await __wbg_init({ module_or_path: base64ToBytes(RASURA_WASM_BASE64) });',
    'the init call',
  );
  app = replaceOnce(
    app,
    /const response = await fetch\('\.\/sample\.pdf'\);[\s\S]*?await open\(new Uint8Array\(await response\.arrayBuffer\(\)\), undefined\);/,
    'await open(base64ToBytes(SAMPLE_PDF_BASE64), undefined);',
    'the sample fetch',
  );

  const b64 = (p) => readFileSync(p).toString('base64');
  const html = read(join(out, 'index.html'))
    .replace('<link rel="stylesheet" href="./style.css">', `<style>\n${read(join(out, 'style.css'))}\n</style>`)
    .replace(
      '<script type="module" src="./app.mjs"></script>',
      `<script type="module">
const RASURA_WASM_BASE64 = "${b64(join(out, 'rasura_wasm_bg.wasm'))}";
const SAMPLE_PDF_BASE64 = "${b64(join(out, 'sample.pdf'))}";
${glue}
${render}
${app}
</script>`,
    );

  writeFileSync(join(out, 'standalone.html'), html);
}

standalone();

const size = (name) => (readFileSync(join(out, name)).length / 1024).toFixed(1);
console.log(`demo/dist ready:
  index.html            ${size('index.html')} KB
  app.mjs               ${size('app.mjs')} KB
  render.mjs            ${size('render.mjs')} KB
  style.css             ${size('style.css')} KB
  rasura_wasm.js        ${size('rasura_wasm.js')} KB
  rasura_wasm_bg.wasm   ${size('rasura_wasm_bg.wasm')} KB
  sample.pdf            ${size('sample.pdf')} KB
  standalone.html       ${size('standalone.html')} KB  (single file, everything inlined)

serve it with any static server, e.g.
  npx --yes serve demo/dist
`);
