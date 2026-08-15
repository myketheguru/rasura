// Snapshot every route to static HTML, after a build and before the site ships.
//
//   RASURA_BASE=/rasura/ npx vite build && node scripts/prerender.mjs
//
// Why this exists: the site is one React application behind one `index.html` of
// about 2.4 KB. A crawler that runs JavaScript eventually sees the content; one
// that does not sees an empty div, and even Google renders JavaScript on a
// second, deferred pass. Every route shared the same empty shell, so the only
// thing in the HTML was the meta tags.
//
// **Rendered in a browser, not on a server.** The obvious alternative is
// `renderToString` behind a second entry point, and that is a second
// implementation of "render this application" which can disagree with the first
// -- silently, and only in production. Radix generates ids, the theme reads
// localStorage, and a hydration mismatch is exactly the kind of fault this
// project keeps finding in checks that never ran the real thing. Chrome is
// already on the runner and already driven over the DevTools protocol by
// `demo/browser.mjs`, so the snapshot is what a browser actually produced.
//
// The client still boots normally afterwards. `createRoot` replaces the
// container's contents rather than hydrating them, so there is no mismatch to
// warn about: the prerendered DOM is for the first paint and for readers that
// never run the script, and React takes over from there.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createDistServer, launchChrome, session, CHROME } from '../../demo/browser-kit.mjs';

const DIST = process.env.RASURA_DIST ?? 'dist';
const BASE = process.env.RASURA_BASE ?? '/';

// The same list the router and the sitemap are built from, read the same crude
// way `seo.mjs` reads it, so a page cannot be prerendered into existence that
// the application does not route to.
const nav = readFileSync('src/nav.ts', 'utf8');
const slugs = [...nav.matchAll(/slug: '([^']+)'/g)].map((m) => m[1]);
if (slugs.length < 10) {
  console.error(`only ${slugs.length} nav entries parsed; the shape of nav.ts changed`);
  process.exit(1);
}

// The root, then every documentation page. `/editor` is deliberately absent:
// its content is a PDF loaded at runtime, so a snapshot would freeze one
// document's DOM into the page for no crawler's benefit, and the module has to
// boot there regardless. It keeps the shell and its meta tags.
const ROUTES = ['', ...slugs];

if (!CHROME) {
  if (process.env.CI) {
    console.error('no Chromium found, and this is CI -- set CHROME_PATH or install one');
    process.exit(1);
  }
  console.log('no Chromium found -- skipping prerender');
  console.log('(the site still works; every route just ships the empty shell)');
  process.exit(0);
}

const { origin, close } = await createDistServer({ dist: DIST, base: BASE });
const { port, kill } = await launchChrome();

// Collected in memory and written only once every route has rendered.
//
// Writing as it went poisoned the run: `index.html` is both the root's output
// and the shell every other route is served from, so the moment the root was
// written, every later route was handed the *rendered landing page* as its
// starting document. It satisfied the readiness check instantly, before React
// had booted, and sixteen of eighteen files came out byte-identical copies of
// the landing page carrying the root's canonical URL. That is worse than not
// prerendering at all: it would have told Google every page was a duplicate of
// the homepage.
const output = new Map();
let failed = 0;

try {
  for (const route of ROUTES) {
    const url = `${origin}/${route}`;
    try {
      const html = await snapshot(port, url, route);
      output.set(route === '' ? 'index.html' : `${route}.html`, html);
      console.log(`  ${String(Math.round(html.length / 1024)).padStart(4)} KB  ${route || '/'}`);
    } catch (e) {
      console.error(`  FAIL  ${route || '/'} -- ${e.message}`);
      failed += 1;
    }
  }
} finally {
  kill();
  close();
}

if (failed) {
  // A route that did not render is a route whose HTML would have been the empty
  // shell. Failing here is the point: shipping it silently is the state this
  // script exists to leave behind.
  console.error(`\n${failed} route(s) did not render. Nothing was written.`);
  process.exit(1);
}

// Two pages with identical bytes means one of them rendered the other, which is
// the failure this script already shipped once. It cannot happen now that the
// canonical is checked per route, which is exactly why it is worth asserting:
// the check that would have caught it is cheap, and the one that replaced it
// should not be the only thing standing there.
const seen = new Map();
for (const [name, html] of output) {
  const twin = seen.get(html);
  if (twin) {
    console.error(`\n${name} and ${twin} are byte-identical. Nothing was written.`);
    process.exit(1);
  }
  seen.set(html, name);
}

for (const [name, html] of output) {
  const path = join(DIST, name);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, html);
}
console.log(`\nprerendered ${output.size} route(s) to static HTML.`);

/* -------------------------------------------------------------------------- */

async function snapshot(port, url, route) {
  // What only this route produces. `applyMeta` writes the canonical from the
  // slug in an effect after the first paint, so waiting for it to end in this
  // route's own path means the page has both rendered *and* rendered the right
  // thing.
  //
  // The first version waited for "a title, any title, and three headings". The
  // shell ships with a title and the fallback document had headings, so it was
  // satisfied by whatever happened to be on screen -- which was the previously
  // written page. A readiness check that any page can satisfy is not a
  // readiness check.
  const wanted = route === '' ? '/' : `/${route}`;

  return session(port, url, async (send) => {
    await send('Runtime.enable');
    await send('Page.enable');
    await send('Page.navigate', { url });

    const deadline = Date.now() + 30_000;
    let ready = false;
    let last = {};
    while (Date.now() < deadline) {
      const r = await send('Runtime.evaluate', {
        expression: `(() => {
          const root = document.getElementById('root');
          const link = document.querySelector('link[rel="canonical"]');
          return JSON.stringify({
            headings: document.querySelectorAll('h1, h2').length,
            body: root ? root.innerHTML.length : 0,
            canonical: link ? link.getAttribute('href') : '',
            title: document.title,
          });
        })()`,
        returnByValue: true,
      });
      last = JSON.parse(r.result?.value ?? '{}');
      const onRoute = new URL(last.canonical || 'http://x/none').pathname.endsWith(wanted);
      if (onRoute && last.headings >= 2 && last.body > 1000) {
        ready = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 120));
    }
    if (!ready) {
      throw new Error(
        `never settled on ${wanted} (canonical ${JSON.stringify(last.canonical ?? '')}, ` +
          `${last.headings ?? 0} headings, ${last.body ?? 0} bytes)`,
      );
    }

    const dump = await send('Runtime.evaluate', {
      expression: `'<!doctype html>\\n' + document.documentElement.outerHTML`,
      returnByValue: true,
    });
    const html = dump.result?.value;
    if (!html || html.length < 2000) throw new Error(`suspiciously small dump (${html?.length})`);
    return html;
  });
}
