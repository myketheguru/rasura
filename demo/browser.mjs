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

import { existsSync } from 'node:fs';
import { join } from 'node:path';
// Finding Chrome, launching it, driving one tab, and serving a built site the
// way Pages would. Shared with `web/scripts/prerender.mjs`, which has to serve
// the site under exactly the rules this reads it back with -- otherwise the
// prerendered files could work here and 404 in production, or the reverse.
import { CHROME, createDistServer, launchChrome, session } from './browser-kit.mjs';

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
// The site is built with a base path, because Pages serves it from a
// subdirectory. Serving it at `/` instead would 404 every asset — which is what
// happened, and is exactly the deploy this check exists to prevent, so the
// server mounts it where the build says it lives rather than papering over it.
//
// The server itself is in `browser-kit.mjs`, shared with the prerender step.
// MIME types matter more than usual there: a `.wasm` served as anything else
// sends the glue down its non-streaming fallback, which is precisely the
// difference between this passing and a real host failing.
const served = ORIGIN ? null : await createDistServer({ dist: DIST, base: process.env.RASURA_BASE ?? '/' });
const base = ORIGIN ?? served.origin;
console.log(`serving from ${base}/`);

// --- the browser -------------------------------------------------------------

let chromePort;
let killChrome;
try {
  ({ port: chromePort, kill: killChrome } = await launchChrome());
} catch (e) {
  served?.close();
  console.error(e.message);
  process.exit(1);
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
  return session(chromePort, url, async (send, events) => {
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

// --- what a client that never runs a script sees -----------------------------
//
// Every check above drives a browser, so every one of them passes whether or
// not the routes were prerendered: React fills the page in either case. The
// point of prerendering is the reader that does *not* do that -- a crawler on
// its first pass, a model fetching a URL, a preview card generator -- and the
// only way to test for it is to ask for the bytes and not execute them.
//
// Two routes rather than one, and one of them not the root, because the root is
// the file the shell already lives in: if the prerender wrote nothing at all,
// `/` would still look right and `/quickstart` would be the empty div.
console.log('\nwithout javascript');

for (const route of ['', 'quickstart']) {
  const label = route || '/';
  try {
    const res = await fetch(`${base}/${route}`);
    const html = await res.text();
    const text = html
      .replace(/<script[\s\S]*?<\/script>/g, '')
      .replace(/<style[\s\S]*?<\/style>/g, '')
      .replace(/<[^>]*>/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();

    check(`${label}: served`, res.ok, `HTTP ${res.status}`);

    // Prose, not markup. This catches the prerender having written nothing at
    // all, and only that: once the root is prerendered, the fallback document
    // is itself full of text, so a missing page still answers with plenty of
    // it. Deleting `quickstart.html` and rerunning leaves this check green,
    // which is why it is not the one relied on.
    check(`${label}: the text is in the HTML`, text.length > 1200, `${text.length} chars`);

    // This is the one that discriminates: the fallback carries the *root's*
    // canonical, so a route serving the shell fails here even though it looks
    // full. It is also how the first version of the prerender step failed --
    // every page written with the root's canonical, which would have told
    // search engines the whole site was duplicates of the homepage.
    const canonical = html.match(/rel="canonical" href="([^"]*)"/)?.[1] ?? '';
    const wanted = route === '' ? '/' : `/${route}`;
    check(
      `${label}: the canonical is its own`,
      canonical.endsWith(wanted),
      `canonical is ${JSON.stringify(canonical)}`,
    );
  } catch (e) {
    check(`${label}: served`, false, e.message);
  }
}

killChrome();
served?.close();
console.log(
  failures === 0
    ? '\nthe page starts, compiles the module and reads a document in a real browser.'
    : `\n${failures} problem(s)`,
);
process.exit(failures === 0 ? 0 : 1);
