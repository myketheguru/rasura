// Finding, launching and driving headless Chrome, and serving a built site to
// it the way GitHub Pages would.
//
// Extracted because two things need it: `browser.mjs`, which checks the site
// starts, and `web/scripts/prerender.mjs`, which snapshots every route. A second
// copy of "how do we resolve a URL to a file" is a second answer that can
// disagree with the first, and the whole point of the prerender step is that
// what it writes is served under the same rules the check reads it back with.
//
// No puppeteer: it downloads a browser, and the runner already has one.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, readFileSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, normalize } from 'node:path';

const TYPES = {
  '.html': 'text/html',
  '.mjs': 'text/javascript',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.pdf': 'application/pdf',
  '.xml': 'application/xml',
  '.txt': 'text/plain',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

/**
 * Resolve a request path to a file the way GitHub Pages does.
 *
 * Pages tries the path itself, then `<path>.html`, then `<path>/index.html`,
 * and only then 404s. That order matters here: the prerender step writes
 * `introduction.html`, and a server that skipped straight to the SPA fallback
 * would serve the empty shell and report the prerendered file working when it
 * had never been read.
 *
 * The fallback to `index.html` stays, because it is what `404.html` does on
 * Pages for a route with no file, which is how deep links survive.
 */
export function resolveFile(dist, rel) {
  if (rel === '') return join(dist, 'index.html');
  const direct = join(dist, rel);
  if (isFile(direct)) return direct;
  const html = join(dist, `${rel}.html`);
  if (isFile(html)) return html;
  const index = join(dist, rel, 'index.html');
  if (isFile(index)) return index;
  // Anything left with no extension is a route rather than a miss.
  if (!rel.includes('.')) return join(dist, 'index.html');
  return null;
}

function isFile(path) {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

/** A static server for a built site, mounted at `base`. */
export async function createDistServer({ dist, base }) {
  const mount = base.replace(/^\/*/, '/').replace(/\/*$/, '/');
  const server = createServer((req, res) => {
    let url = decodeURIComponent(req.url.split('?')[0]);
    if (mount !== '/' && url.startsWith(mount)) url = url.slice(mount.length - 1);
    const rel = normalize(url).replace(/^[/\\]+/, '').replace(/[/\\]+$/, '');
    const path = resolveFile(dist, rel);
    if (!path || !path.startsWith(normalize(dist)) || !existsSync(path)) {
      res.writeHead(404).end('not found');
      return;
    }
    res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' });
    res.end(readFileSync(path));
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const origin = `http://127.0.0.1:${server.address().port}${mount.slice(0, -1)}`;
  return { server, origin, close: () => server.close() };
}

export const CHROME = [
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

/** Launch headless Chrome and wait for it to publish a debugging port. */
export async function launchChrome() {
  if (!CHROME) throw new Error('no Chromium found');
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

  const portFile = join(profile, 'DevToolsActivePort');
  const started = Date.now();
  let port = null;
  while (!port && Date.now() - started < 30_000) {
    // The file exists before it is finished. On Windows, opening it while
    // Chrome still holds it fails with EBUSY, which crashed the run perhaps one
    // time in three -- a check that fails at random teaches its reader to rerun
    // it, and a rerun is how a real failure gets waved through. Existence is
    // the invitation to try, not the guarantee it will work.
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
    throw new Error('the browser never reported a debugging port');
  }
  return { port, kill: () => chrome.kill() };
}

let nextId = 1;

/** One CDP session against one fresh tab, closed however `run` ends. */
export async function session(port, url, run) {
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
