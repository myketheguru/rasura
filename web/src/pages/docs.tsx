import * as React from 'react'
import { Link } from 'react-router-dom'
import { ArrowRight } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/primitives'
import { Capability, Code, Note, Section } from '@/components/docs-parts'
import { cn } from '@/lib/utils'

const NAV = [
  { id: 'what', title: 'What Rasura is' },
  { id: 'install', title: 'Install' },
  { id: 'reading', title: 'Reading a document' },
  { id: 'editing', title: 'Editing text' },
  { id: 'fidelity', title: 'The fidelity contract' },
  { id: 'fonts', title: 'Supplying a typeface' },
  { id: 'composing', title: 'Composing a document' },
  { id: 'redaction', title: 'Redaction' },
  { id: 'protection', title: 'Encryption' },
  { id: 'saving', title: 'Saving' },
  { id: 'errors', title: 'Errors' },
  { id: 'capabilities', title: 'Capabilities' },
  { id: 'rust', title: 'The Rust API' },
]

/** Highlights the section currently under the header. */
function useActiveSection() {
  const [active, setActive] = React.useState(NAV[0].id)

  React.useEffect(() => {
    const observer = new IntersectionObserver(
      (entries) => {
        // The topmost section that is intersecting wins. Taking the *last*
        // entry instead makes the highlight jump to the bottom of the viewport
        // whenever two sections are on screen, which is most of the time.
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)
        if (visible[0]) setActive(visible[0].target.id)
      },
      { rootMargin: '-72px 0px -60% 0px', threshold: 0 },
    )
    for (const { id } of NAV) {
      const el = document.getElementById(id)
      if (el) observer.observe(el)
    }
    return () => observer.disconnect()
  }, [])

  return active
}

export default function Docs() {
  const active = useActiveSection()

  return (
    <div className="mx-auto flex w-full max-w-[90rem] flex-1 gap-8 px-4 sm:px-6">
      {/* --- sidebar --- */}
      <aside className="sticky top-14 hidden h-[calc(100dvh-3.5rem)] w-56 shrink-0 overflow-y-auto py-8 lg:block">
        <p className="mb-2 px-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
          Guide
        </p>
        <nav className="flex flex-col gap-px">
          {NAV.map((item) => (
            <a
              key={item.id}
              href={`#${item.id}`}
              className={cn(
                'rounded-md px-2 py-1.5 text-[13px] no-underline transition-colors',
                active === item.id
                  ? 'bg-accent font-medium text-foreground'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              {item.title}
            </a>
          ))}
        </nav>
      </aside>

      {/* --- content --- */}
      <main className="min-w-0 flex-1 py-10">
        <Hero />
        <div className="prose">
          <Sections />
        </div>
        <footer className="mt-16 border-t border-border pt-6 text-[13px] text-muted-foreground">
          MIT or Apache-2.0. Built in Rust, compiled to WebAssembly, and run entirely in
          your browser — nothing on this page uploads a document anywhere.
        </footer>
      </main>

      {/* --- on this page --- */}
      <aside className="sticky top-14 hidden h-[calc(100dvh-3.5rem)] w-52 shrink-0 overflow-y-auto py-10 xl:block">
        <p className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
          On this page
        </p>
        <nav className="flex flex-col gap-px border-l border-border">
          {NAV.map((item) => (
            <a
              key={item.id}
              href={`#${item.id}`}
              className={cn(
                '-ml-px border-l-2 px-3 py-1 text-[12.5px] no-underline transition-colors',
                active === item.id
                  ? 'border-primary font-medium text-foreground'
                  : 'border-transparent text-muted-foreground hover:text-foreground',
              )}
            >
              {item.title}
            </a>
          ))}
        </nav>
      </aside>
    </div>
  )
}

function Hero() {
  return (
    <div className="mb-10 border-b border-border pb-10">
      <Badge variant="outline" className="mb-4">
        Rust · WebAssembly · MIT / Apache-2.0
      </Badge>
      <h1 className="max-w-3xl text-4xl font-semibold tracking-[-0.03em] text-balance sm:text-5xl">
        Every other browser PDF library renders or overlays.{' '}
        <span className="text-primary">Rasura edits.</span>
      </h1>
      <p className="mt-5 max-w-2xl text-[15px] leading-relaxed text-muted-foreground">
        A PDF has no paragraphs, no words and frequently no spaces — only positioned glyph
        runs. Rasura reconstructs the document from the page description, changes it, and
        writes it back with the untouched 99% of the file byte-identical.
      </p>
      <div className="mt-7 flex flex-wrap gap-3">
        <Button asChild>
          <Link to="/editor">
            Try the editor
            <ArrowRight />
          </Link>
        </Button>
        <Button variant="outline" asChild>
          <a href="#install">Install</a>
        </Button>
      </div>
    </div>
  )
}

function Sections() {
  return (
    <>
      <Section id="what" title="What Rasura is">
        <p>
          Three properties define correctness, in priority order. Everything in the library
          follows from them.
        </p>
        <ol>
          <li>
            <strong>Non-locality is forbidden.</strong> An edit on page 40 must not change
            the rendered output of any other page by a single pixel, nor alter the bytes of
            any object it did not need to touch.
          </li>
          <li>
            <strong>Fidelity is reported, never assumed.</strong> When the engine cannot
            make an exact edit it says so in a typed result. It never silently substitutes a
            font, silently drops kerning, or silently overlays a text box.
          </li>
          <li>
            <strong>The file remains a valid PDF.</strong> Output passes{' '}
            <code>qpdf --check</code>, and opens in Acrobat, Preview, Chrome and Firefox
            without repair prompts.
          </li>
        </ol>
        <Note kind="info" title="What it is not">
          Rasura does not render. pdf.js does that well and is Apache-2.0; the intended
          pairing is pdf.js for display and Rasura for editing. It also declines scanned
          documents, XFA forms, and creating digital signatures — each for a stated reason
          rather than because they were not got to.
        </Note>
      </Section>

      <Section id="install" title="Install">
        <Code lang="bash">{`npm install rasura`}</Code>
        <p>
          No postinstall script, no native build step. The package ships a WebAssembly
          module and hand-written TypeScript declarations; CI proves the claim by packing
          the tarball, installing it into an empty directory with{' '}
          <code>--ignore-scripts</code>, and editing a PDF with it.
        </p>
        <p>For Rust, the workspace crates are published under the same names:</p>
        <Code lang="toml">{`[dependencies]
rasura = "0.1"`}</Code>
      </Section>

      <Section id="reading" title="Reading a document">
        <p>
          <code>Pdf.open</code> starts a Worker and returns a handle. The document's bytes
          are transferred rather than copied, so a 20&nbsp;MB file does not become 40.
        </p>
        <Code lang="js">{`import { Pdf } from 'rasura'

const doc = await Pdf.open(await file.arrayBuffer())

const info = await doc.info()
// { pageCount, documentKind: 'born-digital' | 'scanned' | 'mixed',
//   taggedStatus, encrypted, permissions, leniencies, memoryUsage }

const page = await doc.page(0)
for (const p of page.paragraphs) {
  console.log(p.text, p.alignment, p.textConfidence)
}`}</Code>
        <p>
          <code>leniencies</code> is worth reading. It lists every specification deviation
          Rasura tolerated to open the file — a broken cross-reference table, a wrong{' '}
          <code>/Length</code>, a header that was not at byte zero. No other viewer will
          tell you these.
        </p>
      </Section>

      <Section id="editing" title="Editing text">
        <p>
          Edits are staged in a session and applied together. Nothing is written until{' '}
          <code>commit</code>, which is what makes undo able to restore the exact prior
          bytes.
        </p>
        <Code lang="js">{`const outcome = await doc.replaceText(
  { page: 0, paragraph: 0, from: 0, to: 5 },
  'Hello',
)
console.log(outcome.fidelity) // 'exact' | 'reembedded' | 'substituted' | 'overlaid'

const { staged, canUndo } = await doc.sessionStatus()
const { bytes } = await doc.commit()`}</Code>
      </Section>

      <Section id="fidelity" title="The fidelity contract">
        <p>
          Every operation returns the rung it achieved. This is the second correctness
          property in practice: degradation is a return value, not an exception and not a
          silent event.
        </p>
        <ul>
          <li>
            <Badge variant="exact">exact</Badge> — the glyphs were already in the embedded
            font. Nothing was approximated.
          </li>
          <li>
            <Badge variant="reembedded">reembedded</Badge> — a glyph was injected into the
            document's own font program from a typeface you supplied.
          </li>
          <li>
            <Badge variant="substituted">substituted</Badge> — a different face was used.
            The text is right; the shapes are not the original.
          </li>
          <li>
            <Badge variant="overlaid">overlaid</Badge> — the old content was covered and new
            content drawn on top. A last resort.
          </li>
        </ul>
        <p>
          Set a floor and an operation that cannot reach it is <em>refused</em> rather than
          quietly degraded:
        </p>
        <Code lang="js">{`await doc.configureSession({ requireFidelity: 'exact' })

try {
  await doc.replaceText(range, 'Ω')
} catch (e) {
  if (e.code === 'fidelity-below-required') {
    // The font has no omega. Supply a typeface that does, or accept a lower rung.
  }
}`}</Code>
      </Section>

      <Section id="fonts" title="Supplying a typeface">
        <p>
          A browser cannot see the fonts installed on the machine, so a character the
          document's own font lacks has nowhere to come from unless you provide it. Register
          one and the same edit succeeds a rung lower.
        </p>
        <Code lang="js">{`const roboto = await fetch('/Roboto-Regular.ttf').then((r) => r.arrayBuffer())
await doc.registerFont(roboto, { matchFor: 'Helvetica' })

const outcome = await doc.replaceText(range, 'Ω')
console.log(outcome.fidelity) // 'reembedded'`}</Code>
        <p>
          Rasura injects the glyph outline into the document's existing font program,
          extends its <code>cmap</code> and <code>/Widths</code>, and merges{' '}
          <code>/ToUnicode</code> — so the text stays copyable and the file stays one font
          heavier rather than two.
        </p>
      </Section>

      <Section id="composing" title="Composing a document">
        <p>
          Rasura also makes documents that did not exist. Describe the content as blocks,
          give it a typeface, and the layout engine decides the measure, the leading, the
          pagination and the position of every line.
        </p>
        <Code lang="js">{`const font = await fetch('/Roboto-Regular.ttf').then((r) => r.arrayBuffer())

const { document, report } = await Pdf.create(
  [
    { kind: 'heading', level: 1, text: 'Quarterly report' },
    { kind: 'paragraph', text: 'Revenue rose by eleven per cent over the period…' },
    { kind: 'list', items: ['Subscriptions grew', 'Hardware was flat'] },
  ],
  font,
  { pageSize: 'a4', columns: 2, title: 'Quarterly report' },
)

console.log(report.pages, report.baseFont) // 2  'OEDTIL+Roboto-Regular'
const bytes = (await document.save()).bytes`}</Code>
        <p>
          The typeface is embedded and subset to exactly the characters used — 515&nbsp;KB
          of Roboto becomes a 14&nbsp;KB subset for two dozen glyphs. Text outside WinAnsi
          gets a Type0 font with <code>/Identity-H</code> automatically, so Greek and Latin
          can share one document.
        </p>
        <Note kind="warning" title="Read report.missing">
          A typeface with no glyph for a character <strong>drops it</strong> rather than
          substituting a different one — the same rule as everywhere else in the library. An
          empty <code>missing</code> array is the only result safe to ignore.
        </Note>
      </Section>

      <Section id="redaction" title="Redaction">
        <p>
          Redaction removes content and then proves it. A redacted document is forced to a
          full rewrite — enforced in code, not documentation — because an incremental save
          would leave the original bytes in the file and the removal would be cosmetic.
        </p>
        <Code lang="js">{`await doc.redactText('Account 4417-9920')
const verdict = await doc.verifyRedaction('Account 4417-9920')

console.log(verdict.clean)      // true
console.log(verdict.notChecked) // where the check does not look`}</Code>
        <p>
          <code>notChecked</code> matters more than the tick. It names the places the
          verification cannot reach, so a clean result is a bounded claim rather than a
          promise.
        </p>
      </Section>

      <Section id="protection" title="Encryption">
        <p>
          Rasura reads RC4 and AES documents and writes AES-256 only. Entropy comes from the
          platform — the module has no random number generator of its own, which is what
          keeps it deterministic everywhere else.
        </p>
        <Code lang="js">{`const weaknesses = await doc.protect({
  userPassword: 'open-me',
  ownerPassword: 'owner',
  strength: 'aes-256',
})
// weaknesses names anything about the request that is weaker than it looks`}</Code>
        <Note kind="info" title="Permissions are reported, never enforced">
          The <code>/P</code> bits say what a conforming reader should allow. Rasura tells
          you what they say and does not act on them: a library that enforced them would be
          claiming a security property that the format does not have.
        </Note>
      </Section>

      <Section id="saving" title="Saving">
        <p>
          The default is an incremental save: the original bytes, unchanged, with a new
          revision appended. An unedited save returns the input byte for byte.
        </p>
        <Code lang="js">{`const saved = await doc.save()
console.log(saved.mode)           // 'incremental'
console.log(saved.bytesAppended)  // 1,204`}</Code>
        <p>
          Some operations force a full rewrite and say so: redaction, a protection change,
          and a document that only opened through recovery. Composition does too, there
          being no original bytes to append to.
        </p>
      </Section>

      <Section id="errors" title="Errors">
        <p>
          Never a bare <code>Error</code>. Every failure carries a code you can branch on
          and a message you can show.
        </p>
        <Code lang="js">{`import { PdfError, CODES } from 'rasura'

try {
  await Pdf.open(bytes)
} catch (e) {
  if (e instanceof PdfError && e.code === 'encrypted-password-required') {
    // ask for a password and try again
  }
}`}</Code>
        <p>
          The codes are <code>malformed</code>, <code>encrypted-password-required</code>,{' '}
          <code>encrypted-unsupported</code>, <code>scanned-no-text</code>,{' '}
          <code>xfa-unsupported</code>, <code>type3-glyph-missing</code>,{' '}
          <code>font-unavailable</code>, <code>overflow</code>, <code>stale-session</code>,{' '}
          <code>fidelity-below-required</code>, <code>signature-would-be-destroyed</code>,{' '}
          <code>unsupported-filter</code>, <code>invalid-argument</code> and{' '}
          <code>internal</code>.
        </p>
      </Section>

      <Section id="capabilities" title="Capabilities">
        <p>What the library does, and how far each part goes.</p>
        <div className="not-prose mt-5 rounded-lg border border-border px-4">
          <Capability name="Text editing" status="shipped">
            Replace, insert and delete across runs, with reflow and a reported fidelity rung.
          </Capability>
          <Capability name="Incremental saving" status="shipped">
            Byte-exact. An unedited save returns the input unchanged; an edit appends a
            revision and touches nothing else.
          </Capability>
          <Capability name="Composition" status="shipped">
            Documents from nothing: layout, pagination, columns, and a typeface embedded and
            subset.
          </Capability>
          <Capability name="Font embedding" status="shipped">
            TrueType outlines, simple or Type0/Identity-H, chosen from the text. CFF outlines
            are declined by name.
          </Capability>
          <Capability name="Redaction" status="shipped">
            Removal with verification, forced to a full rewrite in code.
          </Capability>
          <Capability name="Annotations, forms, pages" status="shipped">
            Create and delete annotations, read and fill form fields, insert, move and delete
            pages with link retargeting.
          </Capability>
          <Capability name="Images" status="partial">
            Add, move, delete and replace image XObjects. No pixel work: bytes go in encoded
            and come out encoded.
          </Capability>
          <Capability name="Tables" status="partial">
            Detected and readable, editable cell by cell. Composition draws them as text
            without rules.
          </Capability>
          <Capability name="Rendering" status="refused">
            §11.6. pdf.js does this well; pair with it rather than growing a second renderer.
          </Capability>
          <Capability name="Scanned documents" status="refused">
            Needs OCR and raster inpainting — a different product. Reported as{' '}
            <code>documentKind: 'scanned'</code>.
          </Capability>
          <Capability name="XFA forms" status="refused">
            Deprecated by ISO and Adobe-proprietary. Detected, exposed as{' '}
            <code>hasXfa</code>, and form edits refused.
          </Capability>
          <Capability name="Signature creation" status="refused">
            Needs key custody and carries a regulatory surface. Existing signatures are
            detected, preserved, and their invalidation reported.
          </Capability>
        </div>
      </Section>

      <Section id="rust" title="The Rust API">
        <p>
          The JavaScript surface is a thin wrapper over the Rust one, which was designed
          first. Everything above is available natively.
        </p>
        <Code lang="rust">{`use rasura::{Document, SaveOptions};
use rasura::create::{Content, Options};

// Read and edit.
let mut doc = Document::open(std::fs::read("in.pdf")?)?;
let page = doc.page(0)?;
println!("{}", page.paragraphs()[0].text);

// Or compose one that did not exist.
let font = std::fs::read("Roboto-Regular.ttf")?;
let (doc, report) = Document::create(
    &[
        Content::heading(1, "Quarterly report"),
        Content::paragraph("Revenue rose by eleven per cent…"),
    ],
    &Options::with_font(font),
)?;
println!("{} page(s), set in {}", report.pages, report.base_font);
std::fs::write("out.pdf", doc.save(&SaveOptions::default())?.bytes)?;`}</Code>
        <p>
          The workspace is layered: <code>cos</code> for objects and the writer,{' '}
          <code>content</code> for content streams, <code>font</code> for typefaces,{' '}
          <code>layout</code> for the document model, <code>edit</code> for operations,{' '}
          <code>flow</code> for the layout engine, and <code>rasura</code> as the facade.
          Each is usable on its own.
        </p>
      </Section>
    </>
  )
}
