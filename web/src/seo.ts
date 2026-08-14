/**
 * Per-page title, description and structured data.
 *
 * A single-page application serves one HTML file, so without this every route
 * shares one title and one description. Search engines that execute JavaScript
 * will read what this sets; those that do not fall back to what is in the HTML.
 * Both matter, which is why the shell carries sensible defaults as well.
 */
import { ALL } from '@/nav'

export const SITE = 'https://myketheguru.github.io/rasura'
export const NAME = 'Rasura'
export const TAGLINE = 'True PDF editing in the browser'

interface Meta {
  title: string
  description: string
}

const EXTRA: Record<string, Meta> = {
  '': {
    title: 'Rasura: true PDF editing in the browser',
    description:
      'A Rust and WebAssembly SDK that reads a PDF as paragraphs and blocks, edits them, and writes the file back with the untouched bytes byte-identical. Runs entirely client-side.',
  },
  editor: {
    title: 'Editor: edit a PDF in your browser',
    description:
      'A live demonstration of Rasura. Open a PDF, edit its text, redact, compact fonts and save, entirely in the tab with nothing uploaded.',
  },
}

export function metaFor(slug: string): Meta {
  if (EXTRA[slug]) return EXTRA[slug]
  const entry = ALL.find((e) => e.slug === slug)
  if (!entry) return EXTRA['']
  return {
    title: `${entry.title} | ${NAME} docs`,
    description: entry.summary,
  }
}

/** Set the document title, description and canonical link for a route. */
export function applyMeta(slug: string) {
  const { title, description } = metaFor(slug)
  document.title = title

  set('meta[name="description"]', 'content', description)
  set('meta[property="og:title"]', 'content', title)
  set('meta[property="og:description"]', 'content', description)
  set('meta[property="og:url"]', 'content', `${SITE}/${slug}`)
  set('meta[name="twitter:title"]', 'content', title)
  set('meta[name="twitter:description"]', 'content', description)

  let canonical = document.querySelector<HTMLLinkElement>('link[rel="canonical"]')
  if (!canonical) {
    canonical = document.createElement('link')
    canonical.rel = 'canonical'
    document.head.append(canonical)
  }
  canonical.href = `${SITE}/${slug}`
}

function set(selector: string, attr: string, value: string) {
  const el = document.querySelector(selector)
  if (el) el.setAttribute(attr, value)
}

/**
 * Structured data, injected once.
 *
 * SoftwareSourceCode rather than SoftwareApplication: this is a library, and
 * describing it as an application invites a rich result that promises something
 * to install and run.
 */
export function structuredData() {
  return {
    '@context': 'https://schema.org',
    '@graph': [
      {
        '@type': 'SoftwareSourceCode',
        name: NAME,
        description:
          'A browser-native SDK for true PDF editing. Reconstructs the document from the page description, edits it, and writes it back with the untouched bytes intact.',
        codeRepository: 'https://github.com/myketheguru/rasura',
        programmingLanguage: ['Rust', 'TypeScript', 'WebAssembly'],
        license: 'https://opensource.org/licenses/MIT',
        url: SITE,
      },
      {
        '@type': 'TechArticle',
        headline: 'Rasura documentation',
        description: 'Guides and API reference for editing and composing PDFs in the browser.',
        url: SITE,
        articleSection: ALL.map((e) => e.title),
      },
      {
        '@type': 'FAQPage',
        mainEntity: [
          [
            'Can you edit PDF text in a browser?',
            'Yes. Rasura reconstructs paragraphs from the positioned glyph runs a PDF actually contains, replaces the characters you name, re-breaks the lines and patches the content stream in place. It runs as WebAssembly in the tab with nothing uploaded.',
          ],
          [
            'How is this different from pdf-lib or jsPDF?',
            'Those draw new content on top of a page. They cannot change a word in an existing sentence, because doing so requires recovering the sentence from glyph positions first. Rasura does that recovery and edits the text itself.',
          ],
          [
            'Does editing a PDF change the whole file?',
            'No. An edit appends a revision and leaves every object it did not touch byte-identical. An unedited save returns the input byte for byte.',
          ],
          [
            'Can it create a PDF from scratch?',
            'Yes. Describe the content as headings, paragraphs and lists, supply a typeface, and the layout engine handles the measure, leading, pagination and columns. The font is embedded and subset to the characters used.',
          ],
          [
            'Does it redact securely?',
            'It removes the glyphs and then searches the saved bytes to confirm the string is gone, and it forces a full rewrite so the original bytes cannot survive in an earlier revision. It reports the places the check does not reach, such as image data and font subset glyphs.',
          ],
          [
            'Does it render PDFs?',
            'No, and it will not. pdf.js does that well and is Apache-2.0. The intended pairing is pdf.js for display and Rasura for editing.',
          ],
        ].map(([q, a]) => ({
          '@type': 'Question',
          name: q,
          acceptedAnswer: { '@type': 'Answer', text: a },
        })),
      },
    ],
  }
}
