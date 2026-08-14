/** The documentation map. One list, used by the sidebar, the router and the
 *  previous/next links at the foot of every page. */
export interface Entry {
  slug: string
  title: string
  summary: string
}

export interface Group {
  title: string
  entries: Entry[]
}

export const GROUPS: Group[] = [
  {
    title: 'Start here',
    entries: [
      { slug: 'introduction', title: 'Introduction', summary: 'What Rasura does and what it refuses to do.' },
      { slug: 'install', title: 'Install', summary: 'npm and cargo, and what ships in each.' },
      { slug: 'quickstart', title: 'Quickstart', summary: 'Open a file, change a word, save it.' },
      { slug: 'use-cases', title: 'Use cases', summary: 'Twenty things people build with this.' },
    ],
  },
  {
    title: 'Guides',
    entries: [
      { slug: 'reading', title: 'Reading a document', summary: 'Pages, paragraphs, blocks, tables, images.' },
      { slug: 'editing', title: 'Editing text', summary: 'Replace, insert, delete, and how reflow behaves.' },
      { slug: 'fidelity', title: 'The fidelity contract', summary: 'Four rungs, and refusing rather than degrading.' },
      { slug: 'fonts', title: 'Fonts', summary: 'Coverage, supplying a typeface, glyph injection.' },
      { slug: 'composing', title: 'Composing documents', summary: 'Build a PDF that did not exist.' },
      { slug: 'redaction', title: 'Redaction', summary: 'Removal, verification, and what it cannot check.' },
      { slug: 'encryption', title: 'Encryption', summary: 'Reading protected files, writing AES-256.' },
      { slug: 'saving', title: 'Saving', summary: 'Incremental against full rewrite, and what forces which.' },
      { slug: 'errors', title: 'Errors', summary: 'Fourteen codes and what to do about each.' },
    ],
  },
  {
    title: 'Reference',
    entries: [
      { slug: 'api', title: 'JavaScript API', summary: 'Every class, method, option and return type.' },
      { slug: 'types', title: 'Types', summary: 'The shape of everything that crosses the boundary.' },
      { slug: 'rust', title: 'Rust API', summary: 'The crate layering and the facade.' },
      { slug: 'architecture', title: 'How it works', summary: 'Bytes to model to bytes, and why each layer exists.' },
    ],
  },
]

export const ALL: Entry[] = GROUPS.flatMap((g) => g.entries)

export function neighbours(slug: string): { prev?: Entry; next?: Entry } {
  const i = ALL.findIndex((e) => e.slug === slug)
  if (i === -1) return {}
  return { prev: ALL[i - 1], next: ALL[i + 1] }
}
