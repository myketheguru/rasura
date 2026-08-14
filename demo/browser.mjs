// Load the assembled demo in a real browser and check that it started.
//
//   node demo/build.mjs && node demo/browser.mjs
//
// This is the check whose absence let a page ship that had never once started.
// The module is built with `--omit-default-module-path`, the page called
// `init()` with no argument, and every browser-free check passed — because
// none of them ran the page. lint.mjs now decides that particular disagreement
// statically, but the general answer to "does it run" is to run it.
//
// It drives headless Chrome over the DevTools protocol: no puppeteer, no npm
// install, no download. `--dump-dom` would be shorter and does not work here —
// chrome.exe detaches from the console on Windows and its stdout arrives
// empty, which reads exactly like a page that rendered nothing.
//
// Skipped, loudly, when no Chromium is installed. A check that cannot run says
// so rather than passing.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, normalize } from 'node:path';

// Point it at a deployed site instead of the local build:
//
//   RASURA_DEMO_ORIGIN=https://myketheguru.github.io/rasura node demo/browser.mjs
//
// Same checks, real host, real headers — the only way to find out whether what
// was deployed is what was tested.
const ORIGIN = process.env.RASURA_DEMO_ORIGIN?.replace(/\/+$/, '');

// Which build to serve. `web/dist` is the React site; the variable exists
// because the same checker has to work against a local build and against a
// deployed origin, and hard-coding one directory made it a demo-only tool.
const DIST = process.env.RASURA_DIST ?? 'web/dist';
if (!ORIGIN && !existsSync(join(DIST, 'index.html'))) {
  console.error(`missing ${DIST}/index.html -- run npm --prefix web run build first`);
  process.exit(1);
}

// The pages to load. A hash router means the editor is a fragment of the same
// document, so both are checked from one server.
// Real paths now, not hash fragments. `editor` is also the deep-link case: the
// server has no file at that path and must hand the request to the application,
// which is exactly what Pages does with 404.html.
const PAGES = [
  ['docs', ''],
  ['editor', 'editor'],
];

/** Whether a URL is the editor route, which is the only one that loads the
 *  module. Matched on the path, because the routes stopped being hash
 *  fragments and the old `#/editor` test silently matched nothing. */
const isEditor = (url) => /\/editor\/?$/.test(new URL(url).pathname);

const CHROME = [
  process.env.CHROME_PATH,
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium-browser',
  '/usr/bin/chromium',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
].find((p) => p && existsSync(p));

if (!CHROME) {
  // A skip is a courtesy to someone without a browser installed, and a hole in
  // the gate anywhere else. CI is the one place this must not quietly pass:
  // silently skipping is how the page shipped broken in the first place.
  if (process.env.CI) {
    console.error('no Chromium found, and this is CI -- set CHROME_PATH or install one');
    process.exit(1);
  }
  console.log('no Chromium found -- skipping the browser check');
  console.log('(this is the only check that proves the page runs; a skip is not a pass)');
  process.exit(0);
}

// --- a static server, because file:// is not the deployment ------------------
//
// The MIME type matters more than usual: a `.wasm` served as anything else
// sends the glue down its non-streaming fallback, which is precisely the
// difference between this passing and a real host failing.

const TYPES = {
  '.html': 'text/html',
  '.mjs': 'text/javascript',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.pdf': 'application/pdf',
};

// The site is built with a base path, because Pages serves it from a
// subdirectory. Serving it at `/` instead would 404 every asset — which is what
// happened, and is exactly the deploy this check exists to prevent, so the
// server mounts it where the build says it lives rather than papering over it.
const BASE = (process.env.RASURA_BASE ?? '/').replace(/^\/*/, '/').replace(/\/*$/, '/');

const server = ORIGIN
  ? null
  : createServer((req, res) => {
      let url = decodeURIComponent(req.url.split('?')[0]);
      if (BASE !== '/' && url.startsWith(BASE)) url = url.slice(BASE.length - 1);
      const rel = normalize(url).replace(/^[/\\]+/, '');
      let path = join(DIST, rel === '' ? 'index.html' : rel);
      // A path with no file is a route, not a miss. Pages does this through
      // 404.html; serving index.html here is the same handoff and keeps the
      // check honest about deep links.
      if (!existsSync(path) && !rel.includes('.')) path = join(DIST, 'index.html');
      if (!path.startsWith(normalize(DIST)) || !existsSync(path)) {
        res.writeHead(404).end('not found');
        return;
      }
      res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' });
      res.end(readFileSync(path));
    });
if (server) await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = ORIGIN ?? `http://127.0.0.1:${server.address().port}${BASE.slice(0, -1)}`;
console.log(`serving from ${base}/`);

// --- the browser -------------------------------------------------------------

const profile = mkdtempSync(join(tmpdir(), 'rasura-browser-'));
const chrome = spawn(
  CHROME,
  [
    '--headless=new',
    '--disable-gpu',
    '--no-sandbox',
    '--disable-dev-shm-usage',
    // A fresh profile, or the launch is handed to the browser the developer
    // already has open and this process exits having done nothing.
    `--user-data-dir=${profile}`,
    '--remote-debugging-port=0',
    'about:blank',
  ],
  { stdio: ['ignore', 'ignore', 'pipe'] },
);

// Chrome writes the port it chose into the profile directory once it is up.
const portFile = join(profile, 'DevToolsActivePort');
const started = Date.now();
let port = null;
while (!port && Date.now() - started < 30_000) {
  // The file exists before it is finished. On Windows, opening it while Chrome
  // still holds it fails with EBUSY, which crashed the run perhaps one time in
  // three -- a check that fails at random teaches its reader to rerun it, and a
  // rerun is how a real failure gets waved through. Existence is the invitation
  // to try, not the guarantee it will work; the loop already handles waiting.
  if (existsSync(portFile)) {
    try {
      const first = readFileSync(portFile, 'utf8').split('\n')[0].trim();
      if (first) port = first;
    } catch {
      // Still being written. Fall through to the sleep and try again.
    }
  }
  if (!port) await new Promise((r) => setTimeout(r, 100));
}
if (!port) {
  chrome.kill();
  server.close();
  console.error('the browser never reported a debugging port');
  process.exit(1);
}

let nextId = 1;
/** One CDP session against one fresh tab. */
async function session(url, run) {
  const target = await (
    await fetch(`http://127.0.0.1:${port}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' })
  ).json();

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  const events = [];
  ws.addEventListener('message', (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method) {
      events.push(msg);
    }
  });
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', () => reject(new Error('devtools socket failed')), { once: true });
  });

  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });

  try {
    return await run(send, events);
  } finally {
    ws.close();
    await fetch(`http://127.0.0.1:${port}/json/close/${target.id}`);
  }
}

let failures = 0;
const check = (label, ok, detail = '') => {
  if (ok) console.log(`  ok    ${label}`);
  else {
    console.error(`  FAIL  ${label}${detail ? ` -- ${detail}` : ''}`);
    failures += 1;
  }
};

async function load(url) {
  return session(url, async (send, events) => {
    await send('Runtime.enable');
    await send('Log.enable');
    await send('Page.enable');
    await send('Page.navigate', { url });

    // Ask the DOM, rather than pattern-matching a dump of it. standalone.html
    // inlines every source file into a `<script>`, so `outerHTML` contains the
    // *text of the failure banner* whether or not the banner was ever shown —
    // which read as a failure on the one file that was working.
    // Asked of the DOM by test id, not pattern-matched out of a dump: the page
    // inlines its own sources in some builds, so `outerHTML` contains the text
    // of an error banner whether or not the banner was ever shown.
    const probe = `(() => {
      const t = (id) => { const el = document.querySelector('[data-testid="' + id + '"]'); return el ? el.textContent : ''; };
      const pre = document.querySelector('pre');
      const failed = /WebAssembly could not start/.test(document.body ? document.body.textContent : '');
      return JSON.stringify({
        fatal: failed && pre ? pre.textContent : null,
        version: t('version'),
        pageLabel: t('page-label'),
        status: t('status'),
        canvas: document.querySelectorAll('canvas').length,
        headings: document.querySelectorAll('h1, h2').length,
        body: document.body ? document.body.innerHTML.length : 0,
      });
    })()`;

    // The module has to compile *and* the sample has to open, and those are two
    // events. Waiting only for the version -- written between them -- passed
    // against a local server, where the fetch had already finished, and failed
    // against the deployed site, where it had not. Wait for the second one.
    const deadline = Date.now() + 45_000;
    let state = {};
    while (Date.now() < deadline) {
      const r = await send('Runtime.evaluate', { expression: probe, returnByValue: true });
      state = JSON.parse(r.result?.value ?? '{}');
      const started = /rasura \d+\.\d+\.\d+/.test(state.version ?? '');
      const opened = state.pageLabel && state.pageLabel.trim() !== '–';
      // The docs route never loads the module, so it settles as soon as its
      // content is on the page. Waiting for a version it will never show would
      // spend the whole timeout on a page that was ready immediately.
      //
      // This tested `url.includes('#/editor')` after the routes stopped being
      // hash fragments, so it was true for every page: the editor was allowed
      // to settle the moment it had five headings, which is before the module
      // has answered. It passed anyway while the editor rendered fewer than
      // five, and started failing when it rendered more, which made a harness
      // fault look like a regression in the page.
      const isDocs = !isEditor(url);
      if (state.fatal !== null || (isDocs && state.headings >= 5) || (started && opened)) break;
      await new Promise((r2) => setTimeout(r2, 250));
    }

    const problems = events
      .filter((e) => e.method === 'Runtime.exceptionThrown' || e.method === 'Log.entryAdded')
      .map((e) => {
        if (e.method === 'Runtime.exceptionThrown') {
          const d = e.params.exceptionDetails;
          return d.exception?.description ?? d.text;
        }
        const entry = e.params.entry;
        if (entry.level !== 'error') return null;
        // With the URL, because "failed to load resource" without one names
        // nothing and is the whole of what the browser puts in `text`.
        return entry.url ? `${entry.text} <${entry.url}>` : entry.text;
      })
      .filter(Boolean)
      // The page references no icon, so the browser's speculative request for
      // one is the browser's business and not a fault in the page.
      .filter((p) => !/favicon/i.test(p));

    return { state, problems };
  });
}

for (const [what, hash] of PAGES) {
  console.log(`\n${what}`);
  const { state, problems } = await load(`${base}/${hash}`);

  // First, because every check below is vacuous without it: a page that never
  // rendered matches no failure pattern either.
  check(`${what}: the page rendered`, state.body > 500, `${state.body ?? 0} bytes of body`);

  if (what === 'docs') {
    // The documentation is static: what has to be true is that React mounted
    // and the content is there, not that any WebAssembly ran.
    check(`${what}: the content is present`, state.headings >= 5, `${state.headings} heading(s)`);
  } else {
    check(`${what}: WebAssembly started`, state.body > 500 && !state.fatal, state.fatal ?? '');

    // Started is not ran. The version comes from the module itself, so this
    // separates "the module compiled" from "the library answered".
    check(
      `${what}: the module answered with its version`,
      /rasura \d+\.\d+\.\d+/.test(state.version ?? ''),
      state.version || 'no version rendered',
    );

    // And the sample was opened and modelled. It has two pages.
    check(
      `${what}: the sample opened and reported its pages`,
      state.pageLabel?.trim() === '1 / 2',
      state.pageLabel
        ? `page label is ${JSON.stringify(state.pageLabel.trim())}, status is ${JSON.stringify(state.status ?? '')}`
        : 'no page label rendered',
    );

    // The page is drawn from the model onto a canvas; no canvas means the model
    // never crossed the boundary even though the page count did.
    check(`${what}: the page was drawn`, state.canvas >= 1, `${state.canvas} canvas`);
  }

  check(`${what}: nothing threw and nothing logged an error`, problems.length === 0, problems.join(' | '));
}

chrome.kill();
server?.close();
console.log(
  failures === 0
    ? '\nthe page starts, compiles the module and reads a document in a real browser.'
    : `\n${failures} problem(s)`,
);
process.exit(failures === 0 ? 0 : 1);
