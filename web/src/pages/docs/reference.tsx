import { C, Code, Note } from '@/components/code'
import { H2, PageHeader, useHeadings } from '@/components/docs-layout'

export function Types() {
  useHeadings([
    { id: 'document', text: 'Document and page', level: 2 },
    { id: 'outcomes', text: 'Outcomes', level: 2 },
    { id: 'compose', text: 'Composition', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Types"
        summary="The shape of everything that crosses the boundary. Hand-written, with no any."
      />

      <H2 id="document">Document and page</H2>
      <Code lang="ts">{`interface DocumentInfo {
  pageCount: number
  documentKind: 'born-digital' | 'scanned' | 'mixed'
  taggedStatus: 'tagged' | 'untagged' | 'degraded'
  hasXfa: boolean
  encrypted: boolean
  revisionCount: number
  memoryUsage: number
  permissions: Permissions
  leniencies: Leniency[]
}

interface Page {
  index: number
  mediaBox: Rect
  rotate: number
  scanned: boolean
  paragraphs: Paragraph[]
  blocks: Block[]
  images: ImageBlock[]
  tables: TableBlock[]
}

interface Paragraph {
  id: number
  text: string
  box: Rect
  lineCount: number
  leading: number
  alignment: 'left' | 'right' | 'centre' | 'justified'
  textConfidence: 'exact' | 'partial' | 'none'
}

interface Rect { x0: number; y0: number; x1: number; y1: number }`}</Code>

      <H2 id="outcomes">Outcomes</H2>
      <Code lang="ts">{`type Fidelity = 'exact' | 'reembedded' | 'substituted' | 'overlaid'

interface Outcome {
  fidelity: Fidelity
  /** Anything that had to give: kerning dropped, a box grown, a line re-broken. */
  compromises: string[]
}

interface Saved {
  bytes: Uint8Array
  mode: 'incremental' | 'full-rewrite'
  /** Zero for a full rewrite, where the concept does not apply. */
  bytesAppended: number
  warnings: Warning[]
}

interface Leniency {
  kind: string
  offset: number
  detail: string
}`}</Code>

      <H2 id="compose">Composition</H2>
      <Code lang="ts">{`type Content =
  | { kind: 'heading'; level: 1 | 2 | 3 | 4 | 5 | 6; text: string }
  | { kind: 'paragraph'; text: string }
  | { kind: 'list'; items: readonly string[] }

interface CreateOptions {
  pageSize?: 'letter' | 'a4'
  margin?: number
  columns?: number
  gutter?: number
  bodySize?: number
  headingSizes?: readonly number[]
  title?: string
}

interface Composition {
  pages: number
  lines: number
  /** Blocks drawn as plain text because their structure is not drawn. */
  approximated: number
  /** Characters the typeface has no glyph for. Dropped, not substituted. */
  missing: string[]
  baseFont: string
  composite: boolean
  /** /StemV cannot be measured from a TrueType file. Always true today. */
  stemVEstimated: boolean
}`}</Code>
      <Note kind="info" title="Why hand-written">
        Generated declarations from wasm-bindgen describe the ABI rather than the API, and{' '}
        <C>any</C> turns up in exactly the places a caller most needs a type. These are
        written by hand and checked by compiling a file of deliberate mistakes on every
        build.
      </Note>
    </>
  )
}

export function Rust() {
  useHeadings([
    { id: 'facade', text: 'The facade', level: 2 },
    { id: 'layers', text: 'The layers', level: 2 },
    { id: 'sessions', text: 'Sessions', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Rust API"
        summary="The Rust surface was designed first. The JavaScript one is a thin wrapper over it."
      />

      <H2 id="facade">The facade</H2>
      <Code lang="rust">{`use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("in.pdf")?)?;
println!("{} pages, {}", doc.page_count(), doc.kind());

let page = doc.page(0)?;
for p in page.paragraphs() {
    println!("{}", p.text);
}

let saved = doc.save(&SaveOptions::default())?;
std::fs::write("out.pdf", saved.bytes)?;`}</Code>

      <p>Composition takes the same shape:</p>
      <Code lang="rust">{`use rasura::create::{Content, Options};

let font = std::fs::read("Roboto-Regular.ttf")?;
let (doc, report) = Document::create(
    &[
        Content::heading(1, "Quarterly report"),
        Content::paragraph("Revenue rose by eleven per cent."),
    ],
    &Options::with_font(font),
)?;

println!("{} pages, set in {}", report.pages, report.base_font);
if !report.missing.is_empty() {
    eprintln!("no glyph for {:?}", report.missing);
}`}</Code>

      <H2 id="layers">The layers</H2>
      <p>
        Each crate is publishable and usable alone. If you want the object model without
        the document model, depend on <C>rasura-cos</C> and stop there.
      </p>
      <Code lang="text">{`rasura-cos       objects, xref, filters, decryption, the writer
rasura-content   content streams, graphics and text state, layers
rasura-font      parsing, shaping, subsetting, injection, embedding
rasura-layout    glyph runs to lines to blocks to a document model
rasura-edit      edit operations, reflow, stream patching, sessions
rasura-flow      the flow model, the layout engine, composition
rasura           the facade, and the public API
rasura-wasm      the wasm-bindgen surface`}</Code>
      <p>
        The dependency graph only goes upward. Nothing in <C>rasura-cos</C> knows what a
        paragraph is, and nothing in <C>rasura-layout</C> knows how to write a file.
      </p>

      <H2 id="sessions">Sessions</H2>
      <Code lang="rust">{`let mut session = doc.session();
session.configure(Fidelity::Exact);

let outcome = session.replace_text(range, "Hello")?;
println!("{:?}", outcome.fidelity);

session.undo()?;
let saved = session.commit(&SaveOptions::default())?;`}</Code>
      <p>
        The Rust API is synchronous. Nothing in the library does IO, so there is nothing to
        await; the asynchrony on the JavaScript side is the Worker boundary, not the work.
      </p>
    </>
  )
}

export function Architecture() {
  useHeadings([
    { id: 'pipeline', text: 'The pipeline', level: 2 },
    { id: 'locality', text: 'How locality holds', level: 2 },
    { id: 'reconstruction', text: 'Reconstruction', level: 2 },
    { id: 'verification', text: 'How it is checked', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="How it works"
        summary="Bytes to a document model and back, and the one mechanism that keeps edits local."
      />

      <H2 id="pipeline">The pipeline</H2>
      <Code lang="text">{`bytes → objects → content streams → glyph runs → document model
                                                      ↓ mutate
bytes ← incremental append ← patched streams ← reflowed runs`}</Code>
      <p>
        Reading goes up the stack and writing comes back down the same path. The important
        property is that the descent is partial: a text edit produces a patched content
        stream and nothing else, so every object it did not touch is written back from the
        original bytes.
      </p>

      <H2 id="locality">How locality holds</H2>
      <p>
        The writer keeps the byte span each object occupied in the source file. On save,
        anything unmodified is copied out of the original buffer verbatim rather than
        re-serialised.
      </p>
      <p>
        That is the whole mechanism, and it is why the property survives as the layers
        above it grow. A re-serialising writer would drift: a dictionary that round-trips
        today gains a space, a different number format, or a reordered key the moment
        anything downstream changes, and non-locality returns without anyone editing the
        writer.
      </p>

      <H2 id="reconstruction">Reconstruction</H2>
      <p>
        A content stream says draw glyph 36 at this point. Getting from there to a
        paragraph takes six steps, and each one can fail in a way worth reporting.
      </p>
      <ol>
        <li>Extract glyph runs with their positions and the font in force.</li>
        <li>
          Resolve each glyph to a character. <C>/ToUnicode</C> covers about half of
          embedded fonts, so six other strategies exist behind it, and the glyph name list
          carries more glyphs than <C>/ToUnicode</C> does.
        </li>
        <li>Segment words, which frequently means inferring spaces the file does not contain.</li>
        <li>Assemble lines from runs that share a baseline.</li>
        <li>Cut the page into blocks and columns, recursively.</li>
        <li>Reconstruct paragraphs: alignment, leading, indents and hyphenation.</li>
      </ol>
      <p>
        Every step reports rather than assumes. Hyphenation that was joined is flagged,
        alignment that was inferred is distinguished from alignment that was measured, and
        a glyph resolved by heuristic lowers the paragraph’s confidence.
      </p>

      <H2 id="verification">How it is checked</H2>
      <ul>
        <li><strong>1,192 unit tests</strong> and 41 JavaScript tests.</li>
        <li>
          <strong>1,026 real PDFs</strong>, mostly the pdf.js regression suite, run through
          eight invariants on every build. Skips are itemised with reasons rather than
          counted as passes.
        </li>
        <li>
          <strong>Three independent judges.</strong> pdf.js extracts text and builds fonts,
          pdfium renders pages for pixel comparison, qpdf validates structure. None of them
          has a stake in agreeing with this library.
        </li>
        <li>
          <strong>Fuzzing</strong> on the lexer, document open, the filters and the
          cross-reference parser.
        </li>
        <li>
          <strong>A real browser</strong> loads the site before anything deploys, because a
          check that never executes the artifact reports green for ever.
        </li>
      </ul>
      <Note kind="info" title="The full account">
        <a href="https://github.com/myketheguru/rasura/blob/main/docs/report.md">
          docs/report.md
        </a>{' '}
        is the honest version: what exists, what does not, what was refused, and the
        mistakes that were found and fixed along the way.
      </Note>
    </>
  )
}
