import { Link } from 'react-router-dom'
import { ArrowRight } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/primitives'
import { C, Code, Note } from '@/components/code'
import { H2, PageHeader, useHeadings } from '@/components/docs-layout'

export function Introduction() {
  useHeadings([
    { id: 'problem', text: 'The problem', level: 2 },
    { id: 'rules', text: 'Three rules', level: 2 },
    { id: 'refuses', text: 'What it refuses', level: 2 },
    { id: 'next', text: 'Where to go next', level: 2 },
  ])

  return (
    <>
      <PageHeader
        title="Introduction"
        summary="Rasura reads a PDF as paragraphs and blocks, changes them, and writes the file back with the untouched bytes intact."
      />

      <H2 id="problem">The problem</H2>
      <p>
        A PDF is a page-description format. It contains no paragraphs, no words, and
        frequently no spaces. What it contains is positioned glyph runs: draw this glyph
        at this point, then that one 4.7 units to the right.
      </p>
      <p>
        Every browser PDF library does one of two things with that. It renders the page
        (pdf.js), or it draws new content on top of the old (pdf-lib, jsPDF). Neither
        edits. Changing a word in a sentence means recovering the sentence first, which
        means rebuilding the paragraph from glyph positions, resolving each glyph back to
        a character, re-breaking the lines at the width the document actually used, and
        patching the content stream in place.
      </p>
      <p>Rasura does that. It is Rust, compiled to WebAssembly, and it runs in the tab.</p>

      <H2 id="rules">Three rules</H2>
      <p>Everything in the library follows from these, in this order.</p>
      <ol>
        <li>
          <strong>Non-locality is forbidden.</strong> An edit on page 40 must not change
          the rendered output of any other page by a single pixel, and must not alter the
          bytes of any object it did not need to touch. An unedited save returns the input
          byte for byte.
        </li>
        <li>
          <strong>Fidelity is reported, never assumed.</strong> When the engine cannot
          make an exact edit it says so in the return value. It does not silently
          substitute a font, silently drop kerning, or silently overlay a text box.
        </li>
        <li>
          <strong>The file stays a valid PDF.</strong> Output passes <C>qpdf --check</C>{' '}
          and opens in Acrobat, Preview, Chrome and Firefox without repair prompts.
        </li>
      </ol>
      <p>
        The second rule is the one that shapes the API. Most operations return a fidelity
        rung rather than throwing, and you can set a floor below which an operation is
        refused instead of degraded.
      </p>

      <H2 id="refuses">What it refuses</H2>
      <p>Each of these is a decision with a reason, not a gap in the roadmap.</p>
      <ul>
        <li>
          <strong>Rendering.</strong> pdf.js does it well and is Apache-2.0. Pair with it
          for display and use Rasura for editing.
        </li>
        <li>
          <strong>Scanned documents.</strong> Editing them needs OCR and raster
          inpainting, which is a different product. <C>open()</C> succeeds and reports{' '}
          <C>documentKind: 'scanned'</C>.
        </li>
        <li>
          <strong>XFA forms.</strong> Deprecated by ISO and Adobe-proprietary. Detected,
          exposed as <C>hasXfa</C>, and form edits refused.
        </li>
        <li>
          <strong>Creating digital signatures.</strong> Needs key custody and carries
          regulatory weight. Existing signatures are detected, preserved, and their
          invalidation reported before you save.
        </li>
      </ul>
      <Note kind="info" title="Permissions are reported, not enforced">
        The <C>/P</C> bits say what a conforming reader should allow. Rasura tells you
        what they say and does not act on them. A library that enforced them would be
        claiming a security property the format does not have.
      </Note>

      <H2 id="next">Where to go next</H2>
      <div className="not-prose mt-4 grid gap-3 sm:grid-cols-2">
        <Link
          to="/quickstart"
          className="rounded-lg border border-border p-4 no-underline transition-colors hover:bg-accent"
        >
          <p className="text-[14px] font-medium text-foreground">Quickstart</p>
          <p className="mt-1 text-[13px] text-muted-foreground">
            Open a file, change a word, save it. Under twenty lines.
          </p>
        </Link>
        <Link
          to="/use-cases"
          className="rounded-lg border border-border p-4 no-underline transition-colors hover:bg-accent"
        >
          <p className="text-[14px] font-medium text-foreground">Use cases</p>
          <p className="mt-1 text-[13px] text-muted-foreground">
            Twenty things people build with this, with the calls each needs.
          </p>
        </Link>
      </div>
    </>
  )
}

export function Install() {
  useHeadings([
    { id: 'npm', text: 'JavaScript', level: 2 },
    { id: 'cargo', text: 'Rust', level: 2 },
    { id: 'bundlers', text: 'Bundlers and hosts', level: 2 },
  ])

  return (
    <>
      <PageHeader
        title="Install"
        summary="One package for the browser and Node, one crate family for Rust. No native build step in either."
      />

      <H2 id="npm">JavaScript</H2>
      <Code lang="bash">{`npm install rasura`}</Code>
      <p>
        The package ships an ES module, a CommonJS shim, a WebAssembly module and
        hand-written TypeScript declarations. There is no postinstall script and no native
        build step. CI proves that by packing the tarball, installing it into an empty
        directory with <C>--ignore-scripts</C>, and editing a PDF with it.
      </p>
      <Code lang="js">{`import { Pdf } from 'rasura'

const doc = await Pdf.open(await file.arrayBuffer())`}</Code>
      <p>
        By default this starts a Worker and does the parsing off the main thread. Pass{' '}
        <C>{`{ worker: false }`}</C> to run inline, which is useful in tests and when
        debugging, and runs exactly the same code.
      </p>

      <H2 id="cargo">Rust</H2>
      <p>
        The facade crate re-exports everything most callers need. The layers below it are
        published separately and are usable on their own if you want the object model
        without the document model.
      </p>
      <Code lang="toml">{`[dependencies]
rasura = "0.1"`}</Code>
      <Code lang="rust">{`use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("in.pdf")?)?;
println!("{} pages", doc.page_count());`}</Code>

      <H2 id="bundlers">Bundlers and hosts</H2>
      <p>
        The <C>.wasm</C> file is a separate asset. Most bundlers resolve it from the
        package automatically. If yours does not, or if you serve it from a CDN, point at
        it explicitly:
      </p>
      <Code lang="js">{`const doc = await Pdf.open(bytes, {
  wasmUrl: '/assets/rasura_wasm_bg.wasm',
})`}</Code>
      <Note kind="warning" title="Two things a host can get wrong">
        Serve <C>.wasm</C> with the <C>application/wasm</C> content type, or streaming
        compilation falls back to a slower path and some engines refuse it outright. And
        if you set a content-security policy, it needs <C>wasm-unsafe-eval</C> in{' '}
        <C>script-src</C>. Both failures look like the module never loading.
      </Note>
      <p>
        Rasura does not need <C>SharedArrayBuffer</C>, so it does not need COOP and COEP
        headers. That is what lets it run on static hosts such as GitHub Pages.
      </p>
    </>
  )
}

export function Quickstart() {
  useHeadings([
    { id: 'edit', text: 'Change a word', level: 2 },
    { id: 'read', text: 'Read the structure', level: 2 },
    { id: 'make', text: 'Make a document', level: 2 },
    { id: 'errors', text: 'Handle failure', level: 2 },
  ])

  return (
    <>
      <PageHeader
        title="Quickstart"
        summary="Four short programs covering the paths most callers need first."
      />

      <H2 id="edit">Change a word</H2>
      <p>
        Edits are staged in a session and applied together. Nothing is written until{' '}
        <C>commit()</C>, which is what lets undo restore the exact prior bytes.
      </p>
      <Code lang="js" title="edit.js" lines>{`import { Pdf } from 'rasura'

const doc = await Pdf.open(await file.arrayBuffer())
const page = await doc.page(0)

// Paragraphs come back reconstructed, in reading order.
const first = page.paragraphs[0]
console.log(first.text)

// Replace characters 0 to 5 of that paragraph.
const outcome = await doc.replaceText(
  { page: 0, paragraph: first.id, from: 0, to: 5 },
  'Hello',
)
console.log(outcome.fidelity) // 'exact'

const { bytes } = await doc.commit()
await doc.close()`}</Code>
      <p>
        The saved bytes are the original file with one revision appended. Everything you
        did not touch is byte-identical.
      </p>

      <H2 id="read">Read the structure</H2>
      <Code lang="js" title="read.js">{`const info = await doc.info()
console.log(info.pageCount, info.documentKind, info.taggedStatus)

// Anything the file got wrong that Rasura tolerated to open it.
for (const l of info.leniencies) console.log(l.kind, l.detail)

const page = await doc.page(0)
console.log(page.paragraphs.length, 'paragraphs')
console.log(page.tables.length, 'tables')
console.log(page.images.length, 'images')`}</Code>
      <p>
        <C>leniencies</C> is worth reading even when nothing is wrong. It lists every
        specification deviation the parser accepted: a broken cross-reference table, a
        wrong <C>/Length</C>, a header that was not at byte zero. No viewer will tell you
        these.
      </p>

      <H2 id="make">Make a document</H2>
      <p>
        Describe the content as blocks and give it a typeface. The layout engine decides
        the measure, the leading, the pagination and the position of every line.
      </p>
      <Code lang="js" title="create.js">{`const font = await fetch('/Inter-Regular.ttf').then((r) => r.arrayBuffer())

const { document, report } = await Pdf.create(
  [
    { kind: 'heading', level: 1, text: 'Invoice 0042' },
    { kind: 'paragraph', text: 'Due 30 days from receipt.' },
    { kind: 'list', items: ['Design, 12 hours', 'Build, 30 hours'] },
  ],
  font,
  { pageSize: 'a4', title: 'Invoice 0042' },
)

console.log(report.pages, report.baseFont)
const { bytes } = await document.save()`}</Code>
      <p>
        The typeface is embedded and subset to the characters used. A three-line document
        set in Inter carries a few kilobytes of font, not the whole file.
      </p>

      <H2 id="errors">Handle failure</H2>
      <p>
        Every failure carries a code. There are fourteen, and each one tells you what to
        do next.
      </p>
      <Code lang="js" title="open.js">{`import { Pdf, PdfError } from 'rasura'

try {
  var doc = await Pdf.open(bytes)
} catch (e) {
  if (e instanceof PdfError && e.code === 'encrypted-password-required') {
    doc = await Pdf.open(bytes, { password: await askUser() })
  } else {
    throw e
  }
}`}</Code>
      <div className="not-prose mt-6">
        <Button asChild>
          <Link to="/use-cases">
            See what people build with this
            <ArrowRight />
          </Link>
        </Button>
      </div>
      <p className="mt-6">
        <Badge variant="outline">Note</Badge> Every example on this site runs in a browser
        with no server. The editor at <Link to="/editor">/editor</Link> is the same
        library doing all of it live.
      </p>
    </>
  )
}
