// Generate robots.txt, sitemap.xml and llms.txt into dist after a build.
//
//   node scripts/seo.mjs
//
// Generated rather than written by hand, and from the same nav list the site
// routes from, so a page cannot be added without appearing in the sitemap. A
// hand-maintained sitemap is wrong within two commits of anyone forgetting.

import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const SITE = 'https://myketheguru.github.io/rasura'
const DIST = 'dist'

// The nav is TypeScript, and this is a plain node script, so the slugs are read
// out of the source rather than imported. Crude, and it fails loudly if the
// shape changes, which is better than silently emitting an empty sitemap.
const nav = readFileSync('src/nav.ts', 'utf8')
const entries = [...nav.matchAll(/slug: '([^']+)', title: '([^']+)', summary: '([^']+)'/g)].map(
  (m) => ({ slug: m[1], title: m[2], summary: m[3] }),
)
if (entries.length < 10) {
  console.error(`only ${entries.length} nav entries parsed; the shape of nav.ts changed`)
  process.exit(1)
}

const today = new Date().toISOString().slice(0, 10)

/* --- robots ---------------------------------------------------------------- */

writeFileSync(
  join(DIST, 'robots.txt'),
  `User-agent: *
Allow: /

Sitemap: ${SITE}/sitemap.xml
`,
)

/* --- sitemap --------------------------------------------------------------- */

const urls = [
  { loc: `${SITE}/`, priority: '1.0' },
  ...entries.map((e) => ({ loc: `${SITE}/${e.slug}`, priority: '0.8' })),
  { loc: `${SITE}/editor`, priority: '0.9' },
]

writeFileSync(
  join(DIST, 'sitemap.xml'),
  `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls
  .map(
    (u) =>
      `  <url>\n    <loc>${u.loc}</loc>\n    <lastmod>${today}</lastmod>\n    <priority>${u.priority}</priority>\n  </url>`,
  )
  .join('\n')}
</urlset>
`,
)

/* --- llms.txt --------------------------------------------------------------
 *
 * The convention at llmstxt.org: a short, linkable summary a language model can
 * read without executing the site. That matters more here than it would for
 * most sites, because this one is a single-page application and a model that
 * does not run JavaScript sees an empty div.
 */

writeFileSync(
  join(DIST, 'llms.txt'),
  `# Rasura

> A browser-native SDK for true PDF editing. It reconstructs the document from the
> page description, edits it, and writes it back with the untouched bytes
> byte-identical. Written in Rust, compiled to WebAssembly, runs entirely
> client-side with nothing uploaded.

Rasura is not a renderer and not an overlay tool. Every other browser PDF library
either renders the page (pdf.js) or draws new content on top of it (pdf-lib,
jsPDF). Neither can change a word in an existing sentence, because that requires
recovering the sentence from positioned glyph runs first. Rasura does that.

Three rules govern the design:

1. Non-locality is forbidden. An edit on page 40 must not change any other page
   by a pixel, nor alter the bytes of any object it did not need to touch.
2. Fidelity is reported, never assumed. Operations return the rung they achieved
   (exact, reembedded, substituted, overlaid) and can be refused below a floor.
3. The file stays a valid PDF, passing qpdf --check and opening in Acrobat,
   Preview, Chrome and Firefox without repair prompts.

It also composes documents that did not exist: describe content as blocks,
supply a typeface, and the layout engine handles measure, leading, pagination and
columns. The font is embedded and subset to the characters drawn.

Install: \`npm install rasura\` for JavaScript, \`rasura = "0.1"\` for Rust.

## Documentation

${entries.map((e) => `- [${e.title}](${SITE}/${e.slug}): ${e.summary}`).join('\n')}

## Also

- [Editor](${SITE}/editor): a live demonstration running the real library in the browser
- [Source](https://github.com/myketheguru/rasura): MIT or Apache-2.0
- [Build report](https://github.com/myketheguru/rasura/blob/main/docs/report.md): what exists, what does not, what was refused, and the mistakes found and fixed

## What it refuses, and why

- Rendering. pdf.js does it well and is Apache-2.0; pair with it.
- Scanned documents. Editing them needs OCR and raster inpainting.
- XFA forms. Deprecated by ISO and Adobe-proprietary.
- Creating digital signatures. Needs key custody and carries regulatory weight.
- CFF font embedding. A different stream and subsetter; declined by name rather
  than written into a key that claims TrueType.
`,
)

console.log(`seo: robots.txt, sitemap.xml (${urls.length} urls) and llms.txt`)
