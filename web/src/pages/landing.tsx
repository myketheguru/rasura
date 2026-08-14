import * as React from 'react'
import { Link } from 'react-router-dom'
import { ArrowRight, Check, FileText, Minus, ShieldCheck, Type as TypeIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/primitives'
import { Code } from '@/components/code'
import { applyMeta } from '@/seo'
import { cn } from '@/lib/utils'

/**
 * The front door.
 *
 * Everything here is a claim the library can be held to, and every number is
 * one the build measures. A landing page that oversells this particular
 * library would be undermining its own first rule.
 */
export default function Landing() {
  React.useEffect(() => {
    // The empty slug is the site root's own title and description, not the
    // introduction page's. They are different pages and want different ones.
    applyMeta('')
  }, [])

  return (
    <main className="flex-1">
      <Hero />
      <Figures />
      <Rules />
      <Paths />
      <Refusals />
      <Closing />
    </main>
  )
}

/* -------------------------------------------------------------------------- */

function Hero() {
  return (
    <section className="relative overflow-hidden border-b border-border">
      <div className="ruled pointer-events-none absolute inset-0" aria-hidden />
      <div
        className="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/40 to-transparent"
        aria-hidden
      />

      <div className="relative mx-auto grid w-full max-w-[80rem] gap-12 px-4 py-20 sm:px-6 lg:grid-cols-[1fr_1.05fr] lg:items-center lg:py-28">
        <div>
          <Eyebrow>Rust, compiled to WebAssembly</Eyebrow>

          <h1 className="mt-5 text-balance text-[2.6rem] font-semibold leading-[1.05] tracking-[-0.04em] sm:text-6xl">
            Edit the text in a PDF.
            <span className="block text-muted-foreground">Without regenerating the file.</span>
          </h1>

          <p className="mt-6 max-w-xl text-[15px] leading-relaxed text-muted-foreground">
            A PDF holds no paragraphs and no words. It holds instructions to draw glyph 36
            here and glyph 82 four units to the right. Rasura rebuilds the sentence from
            those positions, changes it, and patches the file in place. Every byte it did
            not need to touch is returned exactly as it arrived.
          </p>

          <div className="mt-8 flex flex-wrap items-center gap-3">
            <Button size="lg" asChild>
              <Link to="/quickstart" className="no-underline">
                Read the quickstart <ArrowRight />
              </Link>
            </Button>
            <Button size="lg" variant="outline" asChild>
              <Link to="/editor" className="no-underline">
                Open the live editor
              </Link>
            </Button>
          </div>

          <p className="mt-5 font-mono text-[12px] text-muted-foreground">
            npm install rasura
            <span className="mx-2 text-border">·</span>
            cargo add rasura
          </p>
        </div>

        <ScrapePanel />
      </div>
    </section>
  )
}

/**
 * The hero image, which is the mechanism rather than a picture of it.
 *
 * On the right, the content stream as it sits in the file. On the left, the
 * paragraph recovered from it. The line between them travels once on load.
 */
function ScrapePanel() {
  const OPERATORS = [
    ['BT', ''],
    ['/F1 11 Tf', ''],
    ['72 709.8 Td', ''],
    ['[(Prepar)18(ed f)-4(or the)] TJ', ''],
    ['13.2 TL T*', ''],
    ['[(boar)11(d, and f)-4(or any-)] TJ', ''],
    ['T*', ''],
    ['[(one curious about)] TJ', ''],
    ['ET', ''],
  ]

  return (
    <div className="relative" data-reveal>
      <div className="relative overflow-hidden rounded-xl border border-border bg-card shadow-[0_1px_2px_hsl(var(--foreground)/0.04),0_12px_32px_-12px_hsl(var(--foreground)/0.12)]">
        <div className="flex items-center gap-2 border-b border-border bg-muted/40 px-3 py-2">
          <FileText className="size-3.5 text-muted-foreground" />
          <span className="font-mono text-[11px] tracking-wide text-muted-foreground">
            board-report.pdf
          </span>
          <span className="ml-auto font-mono text-[10.5px] text-muted-foreground">page 1</span>
        </div>

        <div className="relative min-h-[19rem] p-5 sm:min-h-[21rem]">
          {/* What the file contains. */}
          <div className="font-mono text-[11.5px] leading-[1.9] text-muted-foreground">
            {OPERATORS.map(([op], i) => (
              <div key={i} className="whitespace-pre">
                {op}
              </div>
            ))}
          </div>

          {/* What Rasura reads it as, revealed over the top. */}
          <div
            className="scrape-reveal absolute inset-0 bg-card p-5"
            style={{ '--scrape-travel': '100%' } as React.CSSProperties}
          >
            <p className="text-[11px] font-medium uppercase tracking-wider text-primary">
              Paragraph 1
            </p>
            <p className="mt-3 text-[15px] leading-[1.75] text-foreground">
              Prepared for the board, and for anyone curious about what a PDF editor can
              actually see.
            </p>

            <dl className="mt-6 grid grid-cols-2 gap-x-4 gap-y-2.5 border-t border-border pt-4 font-mono text-[11.5px]">
              <Field label="font" value="Helvetica" />
              <Field label="size" value="11 pt" />
              <Field label="lines" value="3" />
              <Field label="coverage" value="complete" tone="primary" />
            </dl>

            <div className="mt-6 rounded-lg border border-primary/25 bg-primary/[0.06] px-3 py-2.5">
              <p className="font-mono text-[11.5px] leading-relaxed">
                <span className="text-muted-foreground">replaceText(0..8, </span>
                <span className="text-foreground">'Written'</span>
                <span className="text-muted-foreground">)</span>
              </p>
              <p className="mt-1.5 flex items-center gap-1.5 font-mono text-[11.5px] text-primary">
                <Check className="size-3.5" /> fidelity: exact
                <span className="text-muted-foreground">· +1,204 bytes</span>
              </p>
            </div>
          </div>

          {/* The scrape. */}
          <div
            className="scrape-line pointer-events-none absolute inset-y-0 left-0 w-px bg-primary"
            aria-hidden
          >
            <div className="absolute inset-y-0 -left-8 w-8 bg-gradient-to-r from-transparent to-primary/15" />
          </div>
        </div>
      </div>

      <p className="mt-3 text-center font-mono text-[11px] text-muted-foreground">
        the same page, before and after it is understood
      </p>
    </div>
  )
}

function Field({
  label,
  value,
  tone,
}: {
  label: string
  value: string
  tone?: 'primary'
}) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={cn('tabular-nums', tone === 'primary' ? 'text-primary' : 'text-foreground')}>
        {value}
      </dd>
    </div>
  )
}

/* -------------------------------------------------------------------------- */

/** Numbers the build measures. Nothing here is an estimate. */
function Figures() {
  const items = [
    { n: '1,030', label: 'real PDFs checked on every build' },
    { n: '1,196', label: 'Rust tests, plus 41 in JavaScript' },
    { n: '8', label: 'invariants held across the corpus' },
    { n: '0', label: 'lines of unsafe code' },
  ]

  return (
    <Reveal>
      <section className="border-b border-border">
        <div className="mx-auto grid w-full max-w-[80rem] grid-cols-2 gap-px bg-border px-4 sm:px-6 lg:grid-cols-4">
          {items.map((item) => (
            <div key={item.label} className="bg-background px-4 py-8 text-center lg:px-6">
              <p className="font-mono text-3xl font-semibold tabular-nums tracking-tight text-foreground">
                {item.n}
              </p>
              <p className="mx-auto mt-2 max-w-[15rem] text-[13px] leading-snug text-muted-foreground">
                {item.label}
              </p>
            </div>
          ))}
        </div>
      </section>
    </Reveal>
  )
}

/* -------------------------------------------------------------------------- */

function Rules() {
  const rules = [
    {
      icon: Minus,
      title: 'Non-locality is forbidden',
      body: 'An edit on page 40 changes no other page by a pixel, and alters the bytes of no object it did not need to touch. An unedited save returns the input byte for byte.',
    },
    {
      icon: ShieldCheck,
      title: 'Fidelity is reported, never assumed',
      body: 'Operations return the rung they reached instead of throwing. Set a floor and anything below it is refused rather than quietly degraded.',
    },
    {
      icon: FileText,
      title: 'The file stays a valid PDF',
      body: 'Output passes qpdf --check and opens in Acrobat, Preview, Chrome and Firefox with no repair prompt.',
    },
  ]

  return (
    <Reveal>
      <section className="border-b border-border py-20">
        <div className="mx-auto w-full max-w-[80rem] px-4 sm:px-6">
          <Eyebrow>Three rules</Eyebrow>
          <h2 className="mt-4 max-w-2xl text-balance text-3xl font-semibold tracking-[-0.03em] sm:text-4xl">
            Everything in the library follows from these, in this order.
          </h2>

          <div className="mt-12 grid gap-px overflow-hidden rounded-xl border border-border bg-border md:grid-cols-3">
            {rules.map((rule, i) => (
              <div key={rule.title} className="flex flex-col gap-3 bg-card p-6">
                <div className="flex items-center gap-2.5">
                  <span className="grid size-7 place-items-center rounded-md bg-primary/10 text-primary">
                    <rule.icon className="size-3.5" />
                  </span>
                  <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                    {String(i + 1).padStart(2, '0')}
                  </span>
                </div>
                <h3 className="text-[15px] font-semibold tracking-tight">{rule.title}</h3>
                <p className="text-[13.5px] leading-relaxed text-muted-foreground">{rule.body}</p>
              </div>
            ))}
          </div>

          <div className="mt-6 overflow-x-auto rounded-xl border border-border">
            <table className="w-full min-w-[38rem] text-left text-[13px]">
              <thead>
                <tr className="border-b border-border bg-muted/40">
                  <th className="px-4 py-2.5 font-mono text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
                    Rung
                  </th>
                  <th className="px-4 py-2.5 font-mono text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
                    What it means
                  </th>
                </tr>
              </thead>
              <tbody>
                {[
                  ['exact', 'The glyphs were already in the embedded font. Nothing was approximated.'],
                  ['reembedded', "A glyph was injected into the document's own font from a typeface you supplied."],
                  ['substituted', 'A different face was used. The text is right, the letterforms are not.'],
                  ['overlaid', 'Old content covered, new content drawn on top. A last resort.'],
                ].map(([rung, meaning]) => (
                  <tr key={rung} className="border-b border-border last:border-0">
                    <td className="whitespace-nowrap px-4 py-2.5 font-mono text-primary">{rung}</td>
                    <td className="px-4 py-2.5 text-muted-foreground">{meaning}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </section>
    </Reveal>
  )
}

/* -------------------------------------------------------------------------- */

function Paths() {
  return (
    <Reveal>
      <section className="border-b border-border py-20">
        <div className="mx-auto w-full max-w-[80rem] px-4 sm:px-6">
          <div className="grid gap-10 lg:grid-cols-[0.85fr_1.15fr] lg:items-start">
            <div className="lg:sticky lg:top-24">
              <Eyebrow>Two things it does</Eyebrow>
              <h2 className="mt-4 text-balance text-3xl font-semibold tracking-[-0.03em] sm:text-4xl">
                Change a document, or make one.
              </h2>
              <p className="mt-5 text-[15px] leading-relaxed text-muted-foreground">
                Editing patches the file that exists. Composition builds one that does not,
                with a layout engine that decides the measure, the leading, where lines
                break and where pages break, and keeps a heading with the section under it.
              </p>
              <p className="mt-4 text-[15px] leading-relaxed text-muted-foreground">
                Both embed and subset the typeface to the characters actually drawn. 515 KB
                of Roboto becomes 14.5 KB for the two dozen glyphs a short document uses.
              </p>
              <Button variant="outline" className="mt-6" asChild>
                <Link to="/api" className="no-underline">
                  Full API reference <ArrowRight />
                </Link>
              </Button>
            </div>

            <Tabs defaultValue="edit">
              <TabsList>
                <TabsTrigger value="edit">Edit</TabsTrigger>
                <TabsTrigger value="create">Create</TabsTrigger>
                <TabsTrigger value="rust">Rust</TabsTrigger>
              </TabsList>

              <TabsContent value="edit">
                <Code lang="js" title="edit.js">{`import { Pdf } from 'rasura'

const doc = await Pdf.open(await file.arrayBuffer())
const page = await doc.page(0)

page.paragraphs[0].text
// 'Prepared for the board, and for anyone curious'

const outcome = await doc.replaceText(
  { page: 0, paragraph: page.paragraphs[0].id, from: 0, to: 8 },
  'Written',
)
outcome.fidelity  // 'exact'

const { bytes, bytesAppended } = await doc.commit()
// the original file, plus 1,204 bytes`}</Code>
              </TabsContent>

              <TabsContent value="create">
                <Code lang="js" title="create.js">{`import { Pdf } from 'rasura'

const font = await fetch('/Inter-Regular.ttf')
  .then((r) => r.arrayBuffer())

const { document, report } = await Pdf.create(
  [
    { kind: 'heading', level: 1, text: 'Invoice 0042' },
    { kind: 'paragraph', text: 'Due 30 days from receipt.' },
    { kind: 'list', items: ['Design, 12 hours', 'Build, 30 hours'] },
  ],
  font,
  { pageSize: 'a4' },
)

report.pages  // 1
report.lines  // 6`}</Code>
              </TabsContent>

              <TabsContent value="rust">
                <Code lang="rust" title="main.rs">{`use rasura::{Document, SaveOptions};

let mut doc = Document::open(std::fs::read("in.pdf")?)?;
let page = doc.page(0)?;

let mut session = doc.edit();
session.replace_text(
    &page,
    page.paragraphs()[0].id,
    0..8,
    "Written",
)?;

let saved = session.commit(&SaveOptions::default())?;
std::fs::write("out.pdf", saved.bytes)?;`}</Code>
              </TabsContent>
            </Tabs>
          </div>
        </div>
      </section>
    </Reveal>
  )
}

/* -------------------------------------------------------------------------- */

function Refusals() {
  const items = [
    ['Rendering', 'pdf.js does it well and is Apache-2.0. Pair with it for display.'],
    ['Scanned documents', 'Editing them needs OCR and raster inpainting, which is a different product. Reported as documentKind: scanned.'],
    ['XFA forms', 'Deprecated by ISO and Adobe-proprietary. Detected, exposed, and form edits refused.'],
    ['Creating signatures', 'Needs key custody and carries regulatory weight. Existing ones are preserved and their invalidation reported.'],
    ['Redacting under an image', 'Image data is not searched, so a scan of the same words would survive. It fails rather than half-succeeds.'],
    ['Bold and italic when composing', 'One embedded face per composed document, for now. Named here rather than left to be discovered.'],
  ]

  return (
    <Reveal>
      <section className="border-b border-border py-20">
        <div className="mx-auto w-full max-w-[80rem] px-4 sm:px-6">
          <Eyebrow>What it refuses</Eyebrow>
          <h2 className="mt-4 max-w-2xl text-balance text-3xl font-semibold tracking-[-0.03em] sm:text-4xl">
            Each of these is a decision with a reason, not a gap in a roadmap.
          </h2>

          <div className="mt-12 grid gap-x-10 gap-y-px sm:grid-cols-2">
            {items.map(([title, body]) => (
              <div key={title} className="border-t border-border py-5">
                <h3 className="flex items-center gap-2 text-[14px] font-semibold tracking-tight">
                  <Minus className="size-3.5 shrink-0 text-muted-foreground" />
                  {title}
                </h3>
                <p className="mt-1.5 pl-5 text-[13.5px] leading-relaxed text-muted-foreground">
                  {body}
                </p>
              </div>
            ))}
          </div>
        </div>
      </section>
    </Reveal>
  )
}

/* -------------------------------------------------------------------------- */

function Closing() {
  return (
    <Reveal>
      <section className="relative overflow-hidden py-24">
        <div className="ruled pointer-events-none absolute inset-0 rotate-180" aria-hidden />
        <div className="relative mx-auto w-full max-w-[80rem] px-4 text-center sm:px-6">
          <TypeIcon className="mx-auto size-6 text-primary" />
          <h2 className="mx-auto mt-6 max-w-2xl text-balance text-3xl font-semibold tracking-[-0.03em] sm:text-4xl">
            Open a file in the editor. Nothing you load is uploaded anywhere.
          </h2>
          <p className="mx-auto mt-4 max-w-xl text-[15px] leading-relaxed text-muted-foreground">
            It runs the real library in your tab, so there is nowhere to upload it to.
          </p>
          <div className="mt-8 flex flex-wrap justify-center gap-3">
            <Button size="lg" asChild>
              <Link to="/editor" className="no-underline">
                Open the editor <ArrowRight />
              </Link>
            </Button>
            <Button size="lg" variant="outline" asChild>
              <Link to="/introduction" className="no-underline">
                Read the documentation
              </Link>
            </Button>
          </div>
        </div>
      </section>
    </Reveal>
  )
}

/* -------------------------------------------------------------------------- */

function Eyebrow({ children }: { children: React.ReactNode }) {
  return (
    <p className="flex items-center gap-2 font-mono text-[11px] uppercase tracking-[0.16em] text-muted-foreground">
      <span className="h-px w-6 bg-primary" aria-hidden />
      {children}
    </p>
  )
}

/**
 * Reveals its children once, when they are first scrolled to.
 *
 * The content is in the DOM and visible from the start; the attribute only
 * adds the animation. A reveal that hides content until a script runs is a
 * blank page for anything that does not run scripts, search engines included.
 */
function Reveal({ children }: { children: React.ReactNode }) {
  const ref = React.useRef<HTMLDivElement>(null)
  const [shown, setShown] = React.useState(false)

  React.useEffect(() => {
    const el = ref.current
    if (!el || shown) return
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setShown(true)
          observer.disconnect()
        }
      },
      { rootMargin: '0px 0px -12% 0px' },
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [shown])

  return (
    <div ref={ref} data-reveal data-shown={shown}>
      {children}
    </div>
  )
}
