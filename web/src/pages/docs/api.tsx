import * as React from 'react'
import { ChevronRight } from 'lucide-react'
import { Badge } from '@/components/ui/primitives'
import { C, Code } from '@/components/code'
import { H2, PageHeader, useHeadings } from '@/components/docs-layout'
import { cn } from '@/lib/utils'

/* --- the reference data --------------------------------------------------- */

interface Param {
  name: string
  type: string
  optional?: boolean
  desc: string
}

interface Method {
  name: string
  signature: string
  summary: string
  params?: Param[]
  returns: string
  returnsDesc: string
  throws?: string[]
  example?: string
  notes?: string
}

const STATIC: Method[] = [
  {
    name: 'Pdf.open',
    signature: 'static open(src, opts?): Promise<Document>',
    summary: 'Parse a PDF and return a handle to it.',
    params: [
      { name: 'src', type: 'ArrayBuffer | Uint8Array | Blob', desc: 'The file. Transferred to the Worker rather than copied, which detaches it in the caller.' },
      { name: 'opts.password', type: 'string', optional: true, desc: 'Tried as both user and owner password. The empty password is always attempted first.' },
      { name: 'opts.recovery', type: '"auto" | "never"', optional: true, desc: 'Whether to rebuild the cross-reference table by scanning when it cannot be followed. Defaults to auto.' },
      { name: 'opts.worker', type: 'boolean', optional: true, desc: 'False runs on the calling thread. Same code, same answers, easier to debug.' },
      { name: 'opts.transfer', type: 'boolean', optional: true, desc: 'False copies the bytes instead of transferring them, leaving src usable.' },
      { name: 'opts.wasmUrl', type: 'string | URL', optional: true, desc: 'Where to fetch the module from, for CDN hosting or a strict content-security policy.' },
    ],
    returns: 'Promise<Document>',
    returnsDesc: 'An open document. Call close() when finished, or the Worker stays alive.',
    throws: ['malformed', 'encrypted-password-required', 'encrypted-unsupported', 'unsupported-filter'],
    example: `const doc = await Pdf.open(await file.arrayBuffer(), {
  password: 'hunter2',
})`,
    notes: 'A zero-length src almost always means the buffer was transferred away by an earlier open. Pass { transfer: false } to keep it usable.',
  },
  {
    name: 'Pdf.create',
    signature: 'static create(content, font, opts?): Promise<Composed>',
    summary: 'Compose a document that did not exist.',
    params: [
      { name: 'content', type: 'Content[]', desc: 'Blocks: heading, paragraph or list. Order is reading order.' },
      { name: 'font', type: 'ArrayBuffer | Uint8Array | Blob', desc: 'A TrueType or OpenType file. Required, and embedded subset to the characters used.' },
      { name: 'opts.pageSize', type: '"letter" | "a4"', optional: true, desc: 'Defaults to letter.' },
      { name: 'opts.margin', type: 'number', optional: true, desc: 'Every margin, in points. 72 is an inch.' },
      { name: 'opts.columns', type: 'number', optional: true, desc: 'Column count. Text flows down one and into the next.' },
      { name: 'opts.gutter', type: 'number', optional: true, desc: 'Space between columns, in points. Ignored for one column.' },
      { name: 'opts.bodySize', type: 'number', optional: true, desc: 'Body text size in points. Defaults to 11.' },
      { name: 'opts.headingSizes', type: 'number[]', optional: true, desc: 'Sizes for heading levels 1 to 6. Short arrays leave the rest at default.' },
      { name: 'opts.title', type: 'string', optional: true, desc: 'Written to /Info /Title, which is what a viewer shows in its window bar.' },
    ],
    returns: 'Promise<{ document: Document, report: Composition }>',
    returnsDesc: 'The document, and what composing had to approximate. Read report.missing before shipping the result.',
    throws: ['invalid-argument', 'font-unavailable'],
    example: `const { document, report } = await Pdf.create(
  [{ kind: 'heading', level: 1, text: 'Report' }],
  font,
  { pageSize: 'a4', columns: 2 },
)
if (report.missing.length) console.warn('no glyph for', report.missing)`,
  },
]

const READ: Method[] = [
  {
    name: 'info',
    signature: 'info(): Promise<DocumentInfo>',
    summary: 'Everything readable about the document in one call.',
    returns: 'Promise<DocumentInfo>',
    returnsDesc:
      'pageCount, documentKind, taggedStatus, hasXfa, encrypted, revisionCount, permissions, memoryUsage and leniencies.',
    example: `const { pageCount, documentKind, leniencies } = await doc.info()`,
    notes: 'One call rather than eleven accessors, because every crossing of the Worker boundary costs.',
  },
  {
    name: 'page',
    signature: 'page(index): Promise<Page>',
    summary: 'The reconstructed model of one page.',
    params: [{ name: 'index', type: 'number', desc: 'Zero-based.' }],
    returns: 'Promise<Page>',
    returnsDesc: 'mediaBox, rotate, paragraphs, blocks, images and tables.',
    throws: ['overflow'],
    example: `const page = await doc.page(0)
for (const p of page.paragraphs) console.log(p.text)`,
  },
  {
    name: 'metadata',
    signature: 'metadata(): Promise<Metadata>',
    summary: 'The /Info dictionary and the XMP packet, separately.',
    returns: 'Promise<Metadata>',
    returnsDesc:
      'info, xmp, and disagreements: fields where the two surfaces say different things. Exposed rather than resolved, because which one is right depends on the producer.',
  },
  {
    name: 'fontRequirements',
    signature: 'fontRequirements(): Promise<FontInfo[]>',
    summary: 'What each font in the document can and cannot draw.',
    returns: 'Promise<FontInfo[]>',
    returnsDesc:
      'pdfFont, family, embedded, subset, coverage and needsSupplying. Measured against the embedded program, not against what the encoding claims.',
    example: `for (const f of await doc.fontRequirements()) {
  if (f.needsSupplying) console.log(f.pdfFont, 'cannot draw new text')
}`,
  },
  {
    name: 'formFields',
    signature: 'formFields(): Promise<Field[]>',
    summary: 'Every AcroForm field, by fully qualified name.',
    returns: 'Promise<Field[]>',
    returnsDesc: 'name, kind, value, readOnly and options for choice fields.',
    throws: ['xfa-unsupported'],
  },
]

const EDIT: Method[] = [
  {
    name: 'replaceText',
    signature: 'replaceText(range, text): Promise<Outcome>',
    summary: 'Replace a character range within one paragraph.',
    params: [
      { name: 'range.page', type: 'number', desc: 'Zero-based page index.' },
      { name: 'range.paragraph', type: 'number', desc: 'The paragraph id from page().' },
      { name: 'range.from', type: 'number', desc: 'Character offset, inclusive.' },
      { name: 'range.to', type: 'number', desc: 'Character offset, exclusive.' },
      { name: 'text', type: 'string', desc: 'The replacement.' },
    ],
    returns: 'Promise<Outcome>',
    returnsDesc: 'fidelity, and compromises listing anything that had to give.',
    throws: ['fidelity-below-required', 'type3-glyph-missing', 'font-unavailable'],
    example: `const out = await doc.replaceText(
  { page: 0, paragraph: 0, from: 0, to: 5 },
  'Hello',
)`,
    notes: 'Send the smallest range that differs. Replacing a whole paragraph works and re-breaks every line; trimming the common prefix and suffix usually keeps the edit exact.',
  },
  {
    name: 'redactText',
    signature: 'redactText(text): Promise<Outcome>',
    summary: 'Remove every occurrence of a string from the document.',
    params: [{ name: 'text', type: 'string', desc: 'Matched exactly, across the whole document.' }],
    returns: 'Promise<Outcome>',
    returnsDesc: 'fidelity, plus what the removal could not reach.',
    notes: 'Forces the next save to a full rewrite. That is enforced in code, not documentation: an incremental save would leave the original bytes in an earlier revision.',
  },
  {
    name: 'verifyRedaction',
    signature: 'verifyRedaction(text): Promise<Verdict>',
    summary: 'Search the saved bytes for a string that should be gone.',
    returns: 'Promise<{ clean: boolean, occurrences: number, notChecked: string[] }>',
    returnsDesc:
      'notChecked names the places the search does not reach, which matters more than the tick.',
  },
  {
    name: 'addAnnotation',
    signature: 'addAnnotation(page, spec): Promise<Outcome>',
    summary: 'Create an annotation with a generated appearance stream.',
    params: [
      { name: 'page', type: 'number', desc: 'Zero-based.' },
      { name: 'spec.kind', type: 'string', desc: 'Square, Circle, Highlight, StrikeOut, Underline, Line, Text and others.' },
      { name: 'spec.rect', type: 'Rect', desc: 'Where it goes, in PDF page space.' },
      { name: 'spec.colour', type: '[number, number, number]', optional: true, desc: 'RGB, each 0 to 1.' },
      { name: 'spec.contents', type: 'string', optional: true, desc: 'The note text.' },
    ],
    returns: 'Promise<Outcome>',
    returnsDesc: 'The rung achieved. Widget annotations are refused: those belong to form fields.',
  },
  {
    name: 'setFieldValue',
    signature: 'setFieldValue(name, value): Promise<Outcome>',
    summary: 'Fill a form field and regenerate its appearance.',
    returns: 'Promise<Outcome>',
    returnsDesc: 'The appearance is regenerated even when /NeedAppearances is set, because many viewers ignore the flag.',
  },
  {
    name: 'flattenForms',
    signature: 'flattenForms(): Promise<Outcome>',
    summary: 'Turn field values into page content so they cannot be edited back out.',
    returns: 'Promise<Outcome>',
    returnsDesc: 'Invokes the existing appearance stream rather than re-rendering the value, so what you see is what was there.',
  },
  {
    name: 'deletePage',
    signature: 'deletePage(index): Promise<Outcome>',
    summary: 'Remove a page and retarget everything that pointed at it.',
    returns: 'Promise<Outcome>',
    returnsDesc: 'retargeted counts the destinations that were fixed. Refuses if any cannot be.',
  },
  {
    name: 'compactFonts',
    signature: 'compactFonts(): Promise<number>',
    summary: 'Drop glyphs no page draws from every embedded font.',
    returns: 'Promise<number>',
    returnsDesc: 'How many fonts were touched.',
  },
]

const SESSION: Method[] = [
  {
    name: 'configureSession',
    signature: 'configureSession(opts): Promise<void>',
    summary: 'Set the fidelity floor and the reflow policy for subsequent edits.',
    params: [
      { name: 'opts.requireFidelity', type: '"exact" | "reembedded" | "substituted" | "overlaid"', optional: true, desc: 'Operations that cannot reach this rung are refused instead of degraded.' },
      { name: 'opts.overflow', type: '"refuse" | "allow" | "grow" | "shrink"', optional: true, desc: 'What to do when replacement text does not fit its box.' },
      { name: 'opts.lineBreaking', type: '"greedy" | "optimal"', optional: true, desc: 'Greedy matches what most producers did. Optimal is Knuth-Plass.' },
    ],
    returns: 'Promise<void>',
    returnsDesc: '',
  },
  {
    name: 'sessionStatus',
    signature: 'sessionStatus(): Promise<Status>',
    summary: 'How many operations are staged and whether undo is available.',
    returns: 'Promise<{ staged: number, canUndo: boolean, canRedo: boolean }>',
    returnsDesc: '',
  },
  { name: 'undo', signature: 'undo(): Promise<Outcome>', summary: 'Reverse the last staged operation exactly.', returns: 'Promise<Outcome>', returnsDesc: 'Restores the prior bytes, not an approximation of them.' },
  { name: 'redo', signature: 'redo(): Promise<Outcome>', summary: 'Reapply the last undone operation.', returns: 'Promise<Outcome>', returnsDesc: '' },
  { name: 'rollbackSession', signature: 'rollbackSession(): Promise<void>', summary: 'Discard everything staged and close the session.', returns: 'Promise<void>', returnsDesc: '' },
  {
    name: 'commit',
    signature: 'commit(opts?): Promise<Saved>',
    summary: 'Apply every staged operation and write the file.',
    returns: 'Promise<Saved>',
    returnsDesc: 'bytes, mode, bytesAppended and warnings.',
    example: `const { bytes, mode, bytesAppended } = await doc.commit()`,
  },
  {
    name: 'save',
    signature: 'save(opts?): Promise<Saved>',
    summary: 'Write the file without committing staged operations.',
    params: [
      { name: 'opts.mode', type: '"incremental" | "full-rewrite"', optional: true, desc: 'Incremental by default. Some states force a full rewrite whatever you ask for.' },
      { name: 'opts.acceptSignatureDestruction', type: 'boolean', optional: true, desc: 'Required when a full rewrite would invalidate an existing signature.' },
    ],
    returns: 'Promise<Saved>',
    returnsDesc: 'An unedited incremental save returns the input byte for byte.',
    throws: ['signature-would-be-destroyed'],
  },
]

const PROTECT: Method[] = [
  {
    name: 'protect',
    signature: 'protect(policy): Promise<Weakness[]>',
    summary: 'Encrypt the document, or change its passwords.',
    params: [
      { name: 'policy.userPassword', type: 'string', desc: 'Opens the document.' },
      { name: 'policy.ownerPassword', type: 'string', desc: 'Grants full permissions.' },
      { name: 'policy.strength', type: '"aes-256"', desc: 'The only strength written. RC4 is read for legacy files and never produced.' },
      { name: 'policy.permissions', type: 'Partial<Permissions>', optional: true, desc: 'Reported to readers, and never enforced by this library.' },
    ],
    returns: 'Promise<Weakness[]>',
    returnsDesc: 'Anything about the request that is weaker than it looks: a short password, an empty user password, permissions that cannot be relied on.',
    notes: 'Entropy comes from crypto.getRandomValues in the wrapper. The module has no random number generator, which is what keeps it deterministic everywhere else.',
  },
  { name: 'unprotect', signature: 'unprotect(): Promise<void>', summary: 'Remove encryption. Forces a full rewrite.', returns: 'Promise<void>', returnsDesc: '' },
  { name: 'registerFont', signature: 'registerFont(bytes, opts?): Promise<FontHandle>', summary: 'Supply a typeface for glyphs the document cannot draw.', params: [{ name: 'bytes', type: 'ArrayBuffer | Uint8Array', desc: 'A TrueType file.' }, { name: 'opts.matchFor', type: 'string', optional: true, desc: 'The document font family this should stand in for.' }], returns: 'Promise<FontHandle>', returnsDesc: 'Later edits needing a missing glyph reach the reembedded rung instead of failing.' },
  { name: 'close', signature: 'close(): Promise<void>', summary: 'Release the document and shut down its Worker.', returns: 'Promise<void>', returnsDesc: 'Awaitable. A caller who closed every document and still cannot exit has found a bug.' },
]

const SECTIONS = [
  { id: 'static', title: 'Opening and creating', methods: STATIC },
  { id: 'reading', title: 'Reading', methods: READ },
  { id: 'editing', title: 'Editing', methods: EDIT },
  { id: 'session', title: 'Session and saving', methods: SESSION },
  { id: 'protection', title: 'Protection and fonts', methods: PROTECT },
]

/* --- the page ------------------------------------------------------------- */

export default function Api() {
  useHeadings([
    ...SECTIONS.map((s) => ({ id: s.id, text: s.title, level: 2 as const })),
    { id: 'codes', text: 'Error codes', level: 2 },
  ])

  return (
    <>
      <PageHeader
        title="JavaScript API"
        summary="Every method on the package surface, with its parameters, return type and the codes it can throw."
      />

      <p>
        Everything is asynchronous because everything crosses a Worker boundary by
        default. Pass <C>{`{ worker: false }`}</C> to <C>Pdf.open</C> to run inline; the
        methods keep the same shape.
      </p>

      {SECTIONS.map((section) => (
        <section key={section.id} className="mt-10">
          <H2 id={section.id}>{section.title}</H2>
          <div className="not-prose mt-4 flex flex-col gap-2">
            {section.methods.map((m) => (
              <MethodEntry key={m.name} method={m} />
            ))}
          </div>
        </section>
      ))}

      <H2 id="codes">Error codes</H2>
      <p>
        Every failure is a <C>PdfError</C> with a <C>code</C>. Fourteen of them, and each
        one implies a different response.
      </p>
      <div className="not-prose overflow-x-auto">
        <table className="w-full border-collapse text-[12.5px]">
          <thead>
            <tr className="border-b border-border text-left text-muted-foreground">
              <th className="py-2 pr-4 font-medium">Code</th>
              <th className="py-2 font-medium">What to do</th>
            </tr>
          </thead>
          <tbody>
            {[
              ['malformed', 'The bytes are not a usable PDF, even after recovery. Nothing to retry.'],
              ['encrypted-password-required', 'Ask for a password and open again.'],
              ['encrypted-unsupported', 'A handler this library does not implement. Rare outside DRM.'],
              ['scanned-no-text', 'The page is an image. Route it to OCR, or tell the user.'],
              ['xfa-unsupported', 'An XFA form. Form edits are refused; text edits still work.'],
              ['type3-glyph-missing', 'A Type 3 font with no procedure for the glyph. Substitute or decline.'],
              ['font-unavailable', 'No font can draw the text. Call registerFont and retry.'],
              ['overflow', 'An index past the end, or text that will not fit under the current policy.'],
              ['stale-session', 'The handle was closed. Open the document again.'],
              ['fidelity-below-required', 'The floor you set cannot be met. Lower it or supply a font.'],
              ['signature-would-be-destroyed', 'Pass acceptSignatureDestruction, or save incrementally.'],
              ['unsupported-filter', 'A stream filter this library does not decode.'],
              ['invalid-argument', 'The call itself is wrong: empty content, a level out of range.'],
              ['internal', 'A bug. The message and detail are worth reporting.'],
            ].map(([code, action]) => (
              <tr key={code} className="border-b border-border/60">
                <td className="py-2 pr-4 align-top">
                  <code className="font-mono text-[12px]">{code}</code>
                </td>
                <td className="py-2 align-top text-muted-foreground">{action}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </>
  )
}

function MethodEntry({ method }: { method: Method }) {
  const [open, setOpen] = React.useState(false)
  const detailed = Boolean(method.params?.length || method.example || method.notes || method.throws)

  return (
    <div className="rounded-lg border border-border bg-card">
      <button
        onClick={() => detailed && setOpen(!open)}
        className={cn(
          'flex w-full items-start gap-2 px-3.5 py-3 text-left',
          detailed && 'hover:bg-accent/50',
        )}
        aria-expanded={open}
      >
        {detailed && (
          <ChevronRight
            className={cn('mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform', open && 'rotate-90')}
          />
        )}
        <span className={cn('min-w-0 flex-1', !detailed && 'pl-6')}>
          <code className="font-mono text-[13px] font-medium">{method.signature}</code>
          <span className="mt-1 block text-[13px] text-muted-foreground">{method.summary}</span>
        </span>
      </button>

      {open && (
        <div className="border-t border-border px-3.5 py-3">
          {method.params && method.params.length > 0 && (
            <>
              <p className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Parameters
              </p>
              <div className="mb-4 flex flex-col gap-2">
                {method.params.map((p) => (
                  <div key={p.name} className="flex flex-col gap-0.5 sm:flex-row sm:gap-3">
                    <div className="flex min-w-56 shrink-0 items-baseline gap-1.5">
                      <code className="font-mono text-[12.5px]">{p.name}</code>
                      {p.optional && <span className="text-[11px] text-muted-foreground">optional</span>}
                    </div>
                    <div className="min-w-0 flex-1">
                      <code className="font-mono text-[11.5px] text-primary">{p.type}</code>
                      <p className="text-[12.5px] text-muted-foreground">{p.desc}</p>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}

          <p className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            Returns
          </p>
          <code className="font-mono text-[12px] text-primary">{method.returns}</code>
          {method.returnsDesc && (
            <p className="mt-0.5 text-[12.5px] text-muted-foreground">{method.returnsDesc}</p>
          )}

          {method.throws && (
            <>
              <p className="mb-1.5 mt-4 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Throws
              </p>
              <div className="flex flex-wrap gap-1.5">
                {method.throws.map((t) => (
                  <Badge key={t} variant="substituted" className="font-mono">
                    {t}
                  </Badge>
                ))}
              </div>
            </>
          )}

          {method.notes && (
            <p className="mt-4 border-l-2 border-info/40 pl-3 text-[12.5px] text-muted-foreground">
              {method.notes}
            </p>
          )}

          {method.example && <Code lang="js">{method.example}</Code>}
        </div>
      )}
    </div>
  )
}
