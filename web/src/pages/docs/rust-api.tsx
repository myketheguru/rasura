import * as React from 'react'
import { ChevronRight } from 'lucide-react'
import { Badge } from '@/components/ui/primitives'
import { C, Code, Note } from '@/components/code'
import { H2, PageHeader, useHeadings } from '@/components/docs-layout'
import { cn } from '@/lib/utils'

interface Param {
  name: string
  type: string
  desc: string
}

interface Item {
  name: string
  signature: string
  summary: string
  params?: Param[]
  returns?: string
  returnsDesc?: string
  errors?: string[]
  example?: string
  notes?: string
}

/* --- rasura::Document ----------------------------------------------------- */

const CONSTRUCTORS: Item[] = [
  {
    name: 'open',
    signature: 'pub fn open(bytes: Vec<u8>) -> Result<Document>',
    summary: 'Parse a PDF with default options.',
    params: [{ name: 'bytes', type: 'Vec<u8>', desc: 'The whole file. Taken by value and kept for the life of the document, because the writer replays unmodified objects out of it.' }],
    returns: 'Result<Document>',
    returnsDesc: 'The document, or a coded error.',
    errors: ['Malformed', 'EncryptedPasswordRequired', 'EncryptedUnsupported'],
    example: `let mut doc = Document::open(std::fs::read("in.pdf")?)?;`,
  },
  {
    name: 'open_with',
    signature: 'pub fn open_with(bytes: Vec<u8>, opts: &OpenOptions) -> Result<Document>',
    summary: 'Parse a PDF with a password, or with recovery disabled.',
    params: [
      { name: 'bytes', type: 'Vec<u8>', desc: 'The whole file.' },
      { name: 'opts.password', type: 'String', desc: 'Tried as both user and owner password. The empty password is always attempted first.' },
      { name: 'opts.recovery', type: 'Recovery', desc: 'Auto rebuilds the cross-reference table by scanning when it cannot be followed. Never refuses instead.' },
    ],
    returns: 'Result<Document>',
    returnsDesc: 'A document opened by recovery reports it through leniencies() and is forced to a full rewrite on save.',
    example: `let doc = Document::open_with(bytes, &OpenOptions {
    password: "hunter2".into(),
    recovery: Recovery::Auto,
})?;`,
  },
  {
    name: 'create',
    signature: 'pub fn create(content: &[Content], opts: &create::Options) -> Result<(Document, Composition)>',
    summary: 'Compose a document that did not exist.',
    params: [
      { name: 'content', type: '&[Content]', desc: 'Heading, Paragraph or List blocks, in reading order.' },
      { name: 'opts.font', type: 'Vec<u8>', desc: 'A TrueType or OpenType file. Required, embedded, and subset to the characters drawn.' },
      { name: 'opts.geometry', type: 'PageGeometry', desc: 'Page size, margins, columns and gutter.' },
      { name: 'opts.body_size', type: 'f64', desc: 'Body text size in points.' },
      { name: 'opts.heading_sizes', type: '[f64; 6]', desc: 'Sizes for heading levels 1 to 6.' },
      { name: 'opts.title', type: 'Option<String>', desc: 'Written to /Info /Title.' },
    ],
    returns: 'Result<(Document, Composition)>',
    returnsDesc: 'The document, and what composing had to approximate. Read Composition::missing before using the result.',
    errors: ['InvalidArgument', 'FontUnavailable'],
    example: `use rasura::create::{Content, Options};

let font = std::fs::read("Roboto-Regular.ttf")?;
let (doc, report) = Document::create(
    &[
        Content::heading(1, "Quarterly report"),
        Content::paragraph("Revenue rose by eleven per cent."),
    ],
    &Options::with_font(font),
)?;
println!("{} pages, set in {}", report.pages, report.base_font);`,
  },
]

const READERS: Item[] = [
  { name: 'page_count', signature: 'pub fn page_count(&self) -> usize', summary: 'How many pages the document has.' },
  { name: 'page', signature: 'pub fn page(&self, index: usize) -> Result<Page>', summary: 'Reconstruct one page into the document model.', params: [{ name: 'index', type: 'usize', desc: 'Zero-based.' }], returns: 'Result<Page>', returnsDesc: 'Paragraphs in reading order, plus blocks, tables and images.', errors: ['Overflow'], example: `for p in doc.page(0)?.paragraphs() {
    println!("{}", p.text);
}` },
  { name: 'kind', signature: 'pub fn kind(&self) -> DocumentKind', summary: 'BornDigital, Scanned or Mixed.', returnsDesc: 'Classified from image geometry and glyph visibility, not from metadata.' },
  { name: 'tagged_status', signature: 'pub fn tagged_status(&self) -> TaggedStatus', summary: 'Whether a structure tree is present, absent, or degraded by editing.' },
  { name: 'leniencies', signature: 'pub fn leniencies(&self) -> Vec<Leniency>', summary: 'Every specification deviation tolerated to open this file.', returnsDesc: 'Each carries a kind, a byte offset and a detail. The list is complete only once everything in the file has been read.' },
  { name: 'permissions', signature: 'pub fn permissions(&self) -> Permissions', summary: 'What the /P bits say a reader should allow.', notes: 'Reported and never enforced. Enforcing them would claim a security property the format does not have.' },
  { name: 'has_xfa', signature: 'pub fn has_xfa(&self) -> bool', summary: 'Whether the AcroForm carries an XFA packet. Form edits are refused if so.' },
  { name: 'is_encrypted', signature: 'pub fn is_encrypted(&self) -> bool', summary: 'Whether the file was encrypted when it was opened.' },
  { name: 'revision_count', signature: 'pub fn revision_count(&self) -> usize', summary: 'How many incremental revisions the file contains.' },
  { name: 'fonts', signature: 'pub fn fonts(&self) -> Vec<FontInfo>', summary: 'What each font can and cannot draw.', returnsDesc: 'Coverage is measured against the embedded program, not against what the encoding claims.' },
  { name: 'metadata', signature: 'pub fn metadata(&self) -> Metadata', summary: 'The /Info dictionary and the XMP packet, separately.', returnsDesc: 'Fields where the two disagree are listed rather than resolved, because which is right depends on the producer.' },
  { name: 'form_fields', signature: 'pub fn form_fields(&self) -> Vec<Field>', summary: 'Every AcroForm field, by fully qualified name.' },
  { name: 'memory_usage', signature: 'pub fn memory_usage(&self) -> usize', summary: 'Bytes held: the source buffer, the object cache and decoded streams.' },
]

const MUTATORS: Item[] = [
  { name: 'edit', signature: "pub fn edit(&mut self) -> Session<'_>", summary: 'Begin an edit session. Everything staged in it shares one undo stack.', example: `let mut session = doc.edit();
session.require(Fidelity::Exact);
session.replace_text(&page, id, 0..5, "Hello")?;
let saved = session.commit(&SaveOptions::default())?;` },
  { name: 'redact', signature: 'pub fn redact(&mut self, text: &str) -> Result<Vec<String>>', summary: 'Remove every occurrence of a string from the document.', returns: 'Result<Vec<String>>', returnsDesc: 'What the removal could not reach, which is the half worth reading.', notes: 'Forces the next save to a full rewrite, enforced in code. An incremental save would leave the original bytes in an earlier revision.' },
  { name: 'verify_redaction', signature: 'pub fn verify_redaction(bytes: &[u8], strings: &[String]) -> Report', summary: 'Search saved bytes for strings that should be gone.', params: [{ name: 'bytes', type: '&[u8]', desc: 'The saved file, not the in-memory document. Verification is about what was written.' }, { name: 'strings', type: '&[String]', desc: 'What must not appear.' }], returns: 'Report', returnsDesc: 'clean, occurrences, and not_checked naming the places the search does not reach.' },
  { name: 'protect', signature: 'pub fn protect(&mut self, policy: &Policy, entropy: &[u8; 32]) -> Result<Vec<Weakness>>', summary: 'Encrypt the document, or change its passwords.', params: [{ name: 'policy', type: '&Policy', desc: 'User and owner passwords, strength, and the permission bits to record.' }, { name: 'entropy', type: '&[u8; 32]', desc: 'Supplied by the caller. This crate has no random number generator, which is what keeps it deterministic everywhere else.' }], returns: 'Result<Vec<Weakness>>', returnsDesc: 'Anything about the request that is weaker than it looks.', notes: 'AES-256 is the only strength written. RC4 and revision 5 are read for legacy files and never produced.' },
  { name: 'unprotect', signature: 'pub fn unprotect(&mut self) -> Result<()>', summary: 'Remove encryption. Forces a full rewrite.' },
  { name: 'register_font', signature: 'pub fn register_font(&mut self, bytes: Vec<u8>, opts: &RegisterOptions) -> Result<FontHandle>', summary: 'Supply a typeface for glyphs the document cannot draw.', params: [{ name: 'bytes', type: 'Vec<u8>', desc: 'A TrueType file.' }, { name: 'opts.match_for', type: 'Option<String>', desc: 'The document font family this stands in for. Without it the font is a general fallback.' }], returnsDesc: 'Later edits needing a missing glyph reach the Reembedded rung instead of failing.' },
  { name: 'compact_fonts', signature: 'pub fn compact_fonts(&mut self) -> Result<usize>', summary: 'Drop glyphs no page draws from every embedded font.', returns: 'Result<usize>', returnsDesc: 'How many fonts were touched.' },
  { name: 'save', signature: 'pub fn save(&self, opts: &SaveOptions) -> Result<Saved>', summary: 'Write the file.', params: [{ name: 'opts.mode', type: 'Option<SaveMode>', desc: 'Incremental by default. Redaction, a protection change, recovery and composition each force a full rewrite whatever is asked for.' }, { name: 'opts.accept_signature_destruction', type: 'bool', desc: 'Required when a full rewrite would invalidate an existing signature.' }], returns: 'Result<Saved>', returnsDesc: 'bytes, mode, bytes_appended and warnings. An unedited incremental save returns the input byte for byte.', errors: ['SignatureWouldBeDestroyed'] },
  { name: 'raw / raw_mut', signature: 'pub fn raw(&self) -> &rasura_cos::Document', summary: 'The object layer underneath, for anything this surface does not cover.', notes: 'A deliberate cliff. Everything above keeps PDF vocabulary out of the API; this is where it comes back, and the type change is the warning.' },
]

const SESSION: Item[] = [
  { name: 'require', signature: "pub fn require(&mut self, floor: Fidelity) -> &mut Self", summary: 'Refuse any operation that cannot reach this rung.', params: [{ name: 'floor', type: 'Fidelity', desc: 'Exact, Reembedded, Substituted or Overlaid.' }], errors: ['FidelityBelowRequired'] },
  { name: 'reflow', signature: 'pub fn reflow(&mut self, breaking: Breaking, overflow: Overflow) -> &mut Self', summary: 'How lines are re-broken, and what happens when text no longer fits.', params: [{ name: 'breaking', type: 'Breaking', desc: 'Greedy matches what most producers did. Optimal is Knuth-Plass and changes more of the page.' }, { name: 'overflow', type: 'Overflow', desc: 'Refuse, Allow, Grow or Shrink.' }] },
  { name: 'replace_text', signature: 'pub fn replace_text(&mut self, page: &Page, id: ParagraphId, range: Range<usize>, text: &str) -> Result<Outcome>', summary: 'Replace a character range within one paragraph.', params: [{ name: 'page', type: '&Page', desc: 'The page model the paragraph came from.' }, { name: 'id', type: 'ParagraphId', desc: 'From Page::paragraphs.' }, { name: 'range', type: 'Range<usize>', desc: 'Character offsets into the reconstructed text.' }, { name: 'text', type: '&str', desc: 'The replacement.' }], returns: 'Result<Outcome>', returnsDesc: 'The rung achieved, and what had to give.', errors: ['FidelityBelowRequired', 'FontUnavailable', 'Type3GlyphMissing'], notes: 'Send the smallest range that differs. Trimming the common prefix and suffix usually keeps the edit inside one text-showing operator, which is what keeps it Exact.' },
  { name: 'insert_text', signature: 'pub fn insert_text(&mut self, page: &Page, id: ParagraphId, at: usize, text: &str) -> Result<Outcome>', summary: 'Insert at a character offset.' },
  { name: 'delete_range', signature: 'pub fn delete_range(&mut self, page: &Page, id: ParagraphId, range: Range<usize>) -> Result<Outcome>', summary: 'Delete a character range.' },
  { name: 'move_image', signature: 'pub fn move_image(&mut self, page: &Page, id: ImageId, dx: f64, dy: f64) -> Result<Outcome>', summary: 'Translate an image by a delta in points.', notes: 'An image inside a form XObject is shared with every page that invokes the form. Those are refused rather than moved everywhere at once.' },
  { name: 'scale_image', signature: 'pub fn scale_image(&mut self, page: &Page, id: ImageId, sx: f64, sy: f64) -> Result<Outcome>', summary: 'Scale an image about its own origin.' },
  { name: 'delete_image', signature: 'pub fn delete_image(&mut self, page: &Page, id: ImageId) -> Result<Outcome>', summary: 'Remove the drawing operator, leaving the object for anything else that draws it.' },
  { name: 'set_cell', signature: 'pub fn set_cell(&mut self, page: &Page, table: usize, row: usize, col: usize, text: &str) -> Result<Outcome>', summary: 'Replace the text of one detected table cell.' },
  { name: 'delete_page', signature: 'pub fn delete_page(&mut self, index: usize) -> Result<Outcome>', summary: 'Remove a page and retarget everything pointing at it.', returnsDesc: 'Outcome::retargeted counts fixed destinations. Refuses if any cannot be fixed.' },
  { name: 'move_page', signature: 'pub fn move_page(&mut self, from: usize, to: usize) -> Result<Outcome>', summary: 'Reorder pages, fixing navigation.' },
  { name: 'add_annotation', signature: 'pub fn add_annotation(&mut self, page: &Page, new: NewAnnotation) -> Result<Outcome>', summary: 'Create an annotation with a generated appearance stream.', notes: 'Widget annotations are refused: those belong to form fields.' },
  { name: 'delete_annotation', signature: 'pub fn delete_annotation(&mut self, page: &Page, id: ObjId) -> Result<Outcome>', summary: 'Remove an annotation and its entry in /Annots.' },
  { name: 'set_field_value', signature: 'pub fn set_field_value(&mut self, name: &str, value: &str) -> Result<Outcome>', summary: 'Fill a form field by fully qualified name and regenerate its appearance.', notes: 'The appearance is regenerated even when /NeedAppearances is set, because many viewers ignore the flag.' },
  { name: 'flatten_forms', signature: 'pub fn flatten_forms(&mut self, page: &Page) -> Result<Outcome>', summary: 'Turn field values into page content.', notes: 'Invokes the existing appearance stream rather than re-rendering the value, so what you see is what was there.' },
  { name: 'undo', signature: 'pub fn undo(&mut self) -> Result<bool>', summary: 'Reverse the last staged operation exactly.', returns: 'Result<bool>', returnsDesc: 'False when there was nothing to undo. Restores the prior bytes, not an approximation.' },
  { name: 'redo', signature: 'pub fn redo(&mut self) -> Result<bool>', summary: 'Reapply the last undone operation.' },
  { name: 'rollback', signature: 'pub fn rollback(&mut self) -> Result<()>', summary: 'Discard everything staged and close the session.' },
  { name: 'fidelity', signature: 'pub fn fidelity(&self) -> Fidelity', summary: 'The lowest rung any staged operation reached.' },
  { name: 'commit', signature: 'pub fn commit(&mut self, opts: &SaveOptions) -> Result<Saved>', summary: 'Apply everything staged and write the file.' },
  { name: 'suspend / resume', signature: 'pub fn suspend(self) -> SessionState', summary: 'Detach a session so the document can be borrowed elsewhere, then continue it.', notes: 'The undo stack survives. This is what lets a WASM handle hold a session across calls without holding a borrow.' },
]

const SECTIONS = [
  { id: 'constructors', title: 'Opening and creating', items: CONSTRUCTORS, on: 'Document' },
  { id: 'readers', title: 'Reading', items: READERS, on: 'Document' },
  { id: 'mutators', title: 'Whole-document operations', items: MUTATORS, on: 'Document' },
  { id: 'session', title: 'Session', items: SESSION, on: "Session<'a>" },
]

export default function RustApi() {
  useHeadings([
    { id: 'layers', text: 'Crate layering', level: 2 },
    ...SECTIONS.map((s) => ({ id: s.id, text: s.title, level: 2 as const })),
    { id: 'types', text: 'Types', level: 2 },
    { id: 'errors', text: 'Errors', level: 2 },
  ])

  return (
    <>
      <PageHeader
        title="Rust API"
        summary="Every item on the facade, with its signature, parameters and errors. This surface was designed first; the JavaScript one wraps it."
      />

      <p>
        The API is synchronous. Nothing in the library does IO, so there is nothing to
        await. The asynchrony on the JavaScript side is the Worker boundary, not the work.
      </p>

      <H2 id="layers">Crate layering</H2>
      <p>
        Each crate is publishable and usable alone. Dependencies only go upward: nothing in{' '}
        <C>rasura-cos</C> knows what a paragraph is, and nothing in <C>rasura-layout</C>{' '}
        knows how to write a file.
      </p>
      <Code lang="text">{`rasura-cos       objects, xref, filters, decryption, the writer
rasura-content   content streams, graphics and text state, layers
rasura-font      parsing, shaping, subsetting, injection, embedding
rasura-layout    glyph runs to lines to blocks to a document model
rasura-edit      edit operations, reflow, stream patching, sessions
rasura-flow      the flow model, the layout engine, composition
rasura           the facade documented on this page
rasura-wasm      the wasm-bindgen surface`}</Code>
      <Code lang="toml">{`[dependencies]
rasura = "0.1"

# Or one layer, if that is all you need:
rasura-cos = "0.1"`}</Code>

      {SECTIONS.map((section) => (
        <section key={section.id} className="mt-10">
          <H2 id={section.id}>{section.title}</H2>
          <p className="text-[13px] text-muted-foreground">
            On <C>{section.on}</C>.
          </p>
          <div className="not-prose mt-4 flex flex-col gap-2">
            {section.items.map((item) => (
              <ItemEntry key={item.name} item={item} />
            ))}
          </div>
        </section>
      ))}

      <H2 id="types">Types</H2>
      <Code lang="rust">{`pub enum Fidelity { Exact, Reembedded, Substituted, Overlaid }

pub struct Outcome {
    pub fidelity: Fidelity,
    pub compromises: Vec<Compromise>,
    pub retargeted: usize,
}

pub struct Saved {
    pub bytes: Vec<u8>,
    pub mode: SaveMode,
    /// Zero for a full rewrite, where the concept does not apply.
    pub bytes_appended: usize,
    pub warnings: Vec<Warning>,
}

pub struct OpenOptions { pub password: String, pub recovery: Recovery }
pub enum Recovery { Auto, Never }

pub struct SaveOptions {
    pub mode: Option<SaveMode>,
    pub accept_signature_destruction: bool,
}
pub enum SaveMode { Incremental, FullRewrite }

pub enum DocumentKind { BornDigital, Scanned, Mixed }
pub enum TaggedStatus { Tagged, Untagged, Degraded }`}</Code>

      <p>Composition has its own small set:</p>
      <Code lang="rust">{`pub enum Content {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    List { items: Vec<String> },
}

pub struct PageGeometry {
    pub size: (f64, f64),
    /// Top, right, bottom, left, clockwise from the top.
    pub margins: (f64, f64, f64, f64),
    pub columns: usize,
    pub gutter: f64,
}

pub struct Composition {
    pub pages: usize,
    pub lines: usize,
    pub approximated: usize,
    /// Dropped, not substituted.
    pub missing: Vec<char>,
    pub base_font: String,
    pub composite: bool,
    /// /StemV cannot be measured from a TrueType file. Always true today.
    pub stem_v_estimated: bool,
}`}</Code>

      <H2 id="errors">Errors</H2>
      <p>
        One error type, always coded. The layers below have their own richer enums; those
        are kept in <C>Error::detail</C> rather than discarded, because throwing the cause
        away to keep the surface clean makes debugging somebody else&rsquo;s PDF
        impossible.
      </p>
      <Code lang="rust">{`pub struct Error { /* private */ }

impl Error {
    pub fn code(&self) -> Code;
    pub fn message(&self) -> &str;
    /// What the failing layer said, when it said anything.
    pub fn detail(&self) -> &str;
}

pub enum Code {
    Malformed,
    EncryptedPasswordRequired,
    EncryptedUnsupported,
    ScannedNoText,
    XfaUnsupported,
    Type3GlyphMissing,
    FontUnavailable,
    Overflow,
    StaleSession,
    FidelityBelowRequired,
    SignatureWouldBeDestroyed,
    UnsupportedFilter,
    InvalidArgument,
    Internal,
}`}</Code>
      <Note kind="info" title="Matching on codes">
        <C>Code</C> is <C>Copy</C> and <C>PartialEq</C>, so a <C>match</C> on{' '}
        <C>e.code()</C> is the intended way to branch. The string forms used by the
        TypeScript surface come from <C>Code::as_str</C> and are written out rather than
        derived, because a rename that silently changed one would break every caller.
      </Note>
    </>
  )
}

function ItemEntry({ item }: { item: Item }) {
  const [open, setOpen] = React.useState(false)
  const detailed = Boolean(item.params?.length || item.example || item.notes || item.errors || item.returnsDesc)

  return (
    <div className="rounded-lg border border-border bg-card">
      <button
        onClick={() => detailed && setOpen(!open)}
        className={cn('flex w-full items-start gap-2 px-3.5 py-3 text-left', detailed && 'hover:bg-accent/50')}
        aria-expanded={open}
      >
        {detailed && (
          <ChevronRight className={cn('mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform', open && 'rotate-90')} />
        )}
        <span className={cn('min-w-0 flex-1', !detailed && 'pl-6')}>
          <code className="font-mono text-[12.5px] font-medium break-words">{item.signature}</code>
          <span className="mt-1 block text-[13px] text-muted-foreground">{item.summary}</span>
        </span>
      </button>

      {open && (
        <div className="border-t border-border px-3.5 py-3">
          {item.params && item.params.length > 0 && (
            <>
              <p className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Parameters
              </p>
              <div className="mb-4 flex flex-col gap-2">
                {item.params.map((p) => (
                  <div key={p.name} className="flex flex-col gap-0.5 sm:flex-row sm:gap-3">
                    <code className="min-w-52 shrink-0 font-mono text-[12.5px]">{p.name}</code>
                    <div className="min-w-0 flex-1">
                      <code className="font-mono text-[11.5px] text-primary">{p.type}</code>
                      <p className="text-[12.5px] text-muted-foreground">{p.desc}</p>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}

          {(item.returns || item.returnsDesc) && (
            <>
              <p className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Returns
              </p>
              {item.returns && <code className="font-mono text-[12px] text-primary">{item.returns}</code>}
              {item.returnsDesc && (
                <p className="mt-0.5 text-[12.5px] text-muted-foreground">{item.returnsDesc}</p>
              )}
            </>
          )}

          {item.errors && (
            <>
              <p className="mb-1.5 mt-4 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Errors
              </p>
              <div className="flex flex-wrap gap-1.5">
                {item.errors.map((e) => (
                  <Badge key={e} variant="substituted" className="font-mono">
                    Code::{e}
                  </Badge>
                ))}
              </div>
            </>
          )}

          {item.notes && (
            <p className="mt-4 border-l-2 border-info/40 pl-3 text-[12.5px] text-muted-foreground">
              {item.notes}
            </p>
          )}

          {item.example && <Code lang="rust">{item.example}</Code>}
        </div>
      )}
    </div>
  )
}
