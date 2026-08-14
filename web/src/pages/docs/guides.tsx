import { Link } from 'react-router-dom'
import { Badge } from '@/components/ui/primitives'
import { C, Code, Note } from '@/components/code'
import { H2, PageHeader, useHeadings } from '@/components/docs-layout'

export function Reading() {
  useHeadings([
    { id: 'info', text: 'The document', level: 2 },
    { id: 'page', text: 'A page', level: 2 },
    { id: 'order', text: 'Reading order', level: 2 },
    { id: 'leniencies', text: 'Leniencies', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Reading a document"
        summary="What comes back when you open a file, and how much of it was recovered rather than read."
      />

      <H2 id="info">The document</H2>
      <Code lang="js">{`const info = await doc.info()`}</Code>
      <p>
        <C>documentKind</C> is the first thing to branch on. A scanned document has pages
        and no paragraphs, so code that assumes text will find none and needs to say so
        rather than appear to work.
      </p>
      <ul>
        <li><C>born-digital</C>: text was extracted. Everything works.</li>
        <li><C>scanned</C>: images only. Editing text is not possible.</li>
        <li><C>mixed</C>: some pages of each, common in scanned-and-appended files.</li>
      </ul>
      <p>
        <C>taggedStatus</C> reports whether a structure tree is present and whether edits
        have degraded it. Degraded is reported separately from absent, because a document
        that never had tags and one that lost them need different responses.
      </p>

      <H2 id="page">A page</H2>
      <Code lang="js">{`const page = await doc.page(0)

page.paragraphs   // reconstructed text, in reading order
page.blocks       // every block including figures and rules
page.tables       // detected tables, with row and column counts
page.images       // image extents, and whether each is editable
page.mediaBox     // the page box, in points`}</Code>
      <p>Each paragraph carries more than its text:</p>
      <Code lang="js">{`const p = page.paragraphs[0]

p.text            // the characters
p.box             // where it sits
p.lineCount       // how many lines the document broke it into
p.leading         // baseline to baseline, in points
p.alignment       // left, right, centre or justified
p.textConfidence  // exact, partial or none`}</Code>
      <p>
        <C>textConfidence</C> is the honest part. <C>exact</C> means every character came
        from a reliable mapping. <C>partial</C> means some glyphs were resolved by
        heuristic, usually a font with no <C>/ToUnicode</C> and a non-standard encoding.
        Text at <C>partial</C> is worth showing and worth flagging before anyone acts on
        it.
      </p>

      <H2 id="order">Reading order</H2>
      <p>
        Content order is not reading order. A two-column page usually stores the left
        column and the right column interleaved, because that is the order the producer
        drew them. Rasura uses the structure tree when the document has one and a
        recursive cut of the page geometry when it does not.
      </p>
      <p>
        Which one was used is reported. Against tagged documents in the test corpus,
        geometry agrees with the structure tree 89.8% of the time, measured over 50 tagged
        files.
      </p>

      <H2 id="leniencies">Leniencies</H2>
      <p>
        Real PDFs break the specification constantly, and viewers accept them silently.
        Rasura accepts them too, and tells you.
      </p>
      <Code lang="js">{`for (const l of info.leniencies) {
  console.log(l.kind, l.offset, l.detail)
}
// bad-startxref      0  cross-reference chain unusable; rebuilding
// length-recovered  912  /Length said 200, the stream runs 214`}</Code>
      <p>
        For most callers this is diagnostic noise. For anyone building validation,
        archival or forensic tooling it is the most useful thing the library produces, and
        no viewer exposes it.
      </p>
    </>
  )
}

export function Editing() {
  useHeadings([
    { id: 'ranges', text: 'Ranges', level: 2 },
    { id: 'reflow', text: 'Reflow and overflow', level: 2 },
    { id: 'sessions', text: 'Sessions and undo', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Editing text"
        summary="Replacing characters in a paragraph, and what happens to the lines around them."
      />

      <H2 id="ranges">Ranges</H2>
      <p>
        An edit names a paragraph and a character range within it. Offsets are into the
        reconstructed text, so they line up with what you showed the user.
      </p>
      <Code lang="js">{`await doc.replaceText(
  { page: 0, paragraph: p.id, from: 12, to: 19 },
  'replacement',
)`}</Code>
      <Note kind="success" title="Send the smallest range that differs">
        Replacing a whole paragraph works, and re-breaks every line in it. Trimming the
        common prefix and suffix first usually keeps the edit inside one text-showing
        operator, which is what makes the difference between the <C>exact</C> rung and a
        reflow. This is the caller's job, not the library's.
      </Note>

      <H2 id="reflow">Reflow and overflow</H2>
      <p>
        Longer text has to go somewhere. The policy decides where, and it is set on the
        session rather than per call.
      </p>
      <ul>
        <li><C>refuse</C>: the edit fails if the text no longer fits its box.</li>
        <li><C>allow</C>: the paragraph grows downward and may overlap what follows.</li>
        <li><C>grow</C>: the box is enlarged, and the shape of the change is reported.</li>
        <li><C>shrink</C>: the type size is reduced until it fits, and by how much is reported.</li>
      </ul>
      <p>
        Line breaking defaults to greedy, matching what most producers did. The optimal
        setting uses Knuth-Plass, which produces better paragraphs and different breaks
        from the original, so it changes more of the page than the edit strictly needed.
      </p>

      <H2 id="sessions">Sessions and undo</H2>
      <p>
        Operations stage until you commit. Everything in a session shares one undo stack,
        across every kind of operation: an image move and a text edit staged together come
        off in reverse order and leave the file byte-identical.
      </p>
      <Code lang="js">{`await doc.replaceText(range, 'first')
await doc.deletePage(4)

const { staged, canUndo } = await doc.sessionStatus() // 2, true
await doc.undo()                                       // the page comes back
const { bytes } = await doc.commit()                   // only the text edit lands`}</Code>
    </>
  )
}

export function Fidelity() {
  useHeadings([
    { id: 'rungs', text: 'The four rungs', level: 2 },
    { id: 'floor', text: 'Setting a floor', level: 2 },
    { id: 'why', text: 'Why it works this way', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="The fidelity contract"
        summary="Every operation reports how well it did. You can refuse anything below a rung you name."
      />

      <H2 id="rungs">The four rungs</H2>
      <div className="not-prose my-5 flex flex-col gap-2.5">
        {[
          ['exact', 'exact', 'The glyphs were already in the embedded font. Nothing was approximated and nothing about the page changed except the characters you asked to change.'],
          ['reembedded', 'reembedded', 'A glyph was injected into the document’s own font program from a typeface you supplied. The shapes are right and the file carries one font, not two.'],
          ['substituted', 'substituted', 'A different face was used. The text is correct; the letterforms are not the original. Metrics will differ, so lines may break elsewhere.'],
          ['overlaid', 'overlaid', 'The old content was covered and new content drawn on top. A last resort: the original text is still in the file underneath.'],
        ].map(([variant, name, desc]) => (
          <div key={name} className="rounded-lg border border-border p-3">
            <Badge variant={variant as 'exact'}>{name}</Badge>
            <p className="mt-1.5 text-[13px] leading-relaxed text-muted-foreground">{desc}</p>
          </div>
        ))}
      </div>

      <H2 id="floor">Setting a floor</H2>
      <Code lang="js">{`await doc.configureSession({ requireFidelity: 'exact' })

try {
  await doc.replaceText(range, 'Ω')
} catch (e) {
  if (e.code === 'fidelity-below-required') {
    // The font has no omega. Supply one, or accept a lower rung.
    await doc.registerFont(font, { matchFor: 'Helvetica' })
    await doc.replaceText(range, 'Ω') // now reembedded
  }
}`}</Code>

      <H2 id="why">Why it works this way</H2>
      <p>
        The alternative is what every other tool does: substitute a font, or draw a box
        over the old text, and return success. The document looks edited and is quietly
        wrong, and the caller finds out when a reader complains that one word is in a
        different typeface.
      </p>
      <p>
        Making degradation a value rather than an exception means the common case stays
        one line of code, and the caller who cares can branch on it. Making it refusable
        means the caller who must not degrade can say so once, at the top.
      </p>
    </>
  )
}

export function Fonts() {
  useHeadings([
    { id: 'coverage', text: 'What a font can draw', level: 2 },
    { id: 'supply', text: 'Supplying one', level: 2 },
    { id: 'injection', text: 'What injection does', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Fonts"
        summary="A browser cannot see installed fonts, so a character the document lacks has nowhere to come from unless you provide it."
      />

      <H2 id="coverage">What a font can draw</H2>
      <Code lang="js">{`for (const f of await doc.fontRequirements()) {
  console.log(f.pdfFont, f.embedded, f.subset, f.coverage, f.needsSupplying)
}`}</Code>
      <p>
        Coverage is measured against the embedded font program, not against what the
        encoding claims. A subset embedded for the word “Hamburg” contains seven glyphs
        whatever its <C>/Differences</C> array says, and asking it to draw a “z” fails.
      </p>

      <H2 id="supply">Supplying one</H2>
      <Code lang="js">{`const roboto = await fetch('/Roboto-Regular.ttf').then((r) => r.arrayBuffer())
await doc.registerFont(roboto, { matchFor: 'Helvetica' })`}</Code>
      <p>
        <C>matchFor</C> tells the matcher which document font this stands in for. Without
        it the font is a general fallback, used when nothing better is registered.
      </p>

      <H2 id="injection">What injection does</H2>
      <p>
        Rasura does not add a second font to the document. It takes the glyph outline from
        your file and writes it into the font the document already embeds, extends that
        font’s <C>cmap</C> so the new glyph is reachable, widens <C>/Widths</C> and
        <C>/FontBBox</C>, and merges <C>/ToUnicode</C> so the text stays copyable.
      </p>
      <p>
        The result is one font, slightly larger, and a page where the new character is set
        in the same typeface as its neighbours. That is the <C>reembedded</C> rung.
      </p>
      <Note kind="warning" title="What is declined">
        CFF outlines cannot be injected or embedded: they need a different stream and a
        different subsetter. Composite fonts cannot be injected into. Both are declined by
        name rather than half-done, because a font written into the wrong stream passes
        every structural check and renders nothing.
      </Note>
    </>
  )
}

export function Composing() {
  useHeadings([
    { id: 'blocks', text: 'Content blocks', level: 2 },
    { id: 'page', text: 'Page and columns', level: 2 },
    { id: 'font', text: 'The typeface', level: 2 },
    { id: 'report', text: 'Reading the report', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Composing documents"
        summary="Describe content as blocks and let the layout engine place it. No browser, no print pipeline, no font installed on the host."
      />

      <H2 id="blocks">Content blocks</H2>
      <Code lang="js">{`const { document, report } = await Pdf.create(
  [
    { kind: 'heading', level: 1, text: 'Quarterly report' },
    { kind: 'paragraph', text: 'Revenue rose by eleven per cent.' },
    { kind: 'list', items: ['Subscriptions grew', 'Hardware was flat'] },
  ],
  font,
)`}</Code>
      <p>
        Order is reading order. The engine decides the measure, the leading, where lines
        break, where pages break, and keeps a heading with the section under it.
      </p>

      <H2 id="page">Page and columns</H2>
      <Code lang="js">{`await Pdf.create(content, font, {
  pageSize: 'a4',
  margin: 56,
  columns: 2,
  gutter: 18,
  bodySize: 10,
  headingSizes: [22, 16, 13],
  title: 'Quarterly report',
})`}</Code>
      <p>
        Text fills the first column, then the next, then the first column of a new page.
        Widows and orphans are controlled, and a heading that cannot fit with the start of
        its section moves rather than being stranded at the foot of a column.
      </p>

      <H2 id="font">The typeface</H2>
      <p>
        The font is required and embedded. A document set in a font nobody embedded looks
        like whatever the reader has installed, which is the one thing a PDF exists to
        prevent.
      </p>
      <p>
        It is subset to the characters actually drawn. 515 KB of Roboto becomes a 14.5 KB
        subset for the two dozen glyphs a short document uses. Text that fits WinAnsi gets
        a simple font; text that does not gets a Type0 font with <C>/Identity-H</C>, chosen
        from the content rather than asked of you.
      </p>

      <H2 id="report">Reading the report</H2>
      <Code lang="js">{`console.log(report.pages)          // 2
console.log(report.lines)          // 44
console.log(report.baseFont)       // 'OEDTIL+Roboto-Regular'
console.log(report.composite)      // false
console.log(report.approximated)   // 1
console.log(report.missing)        // ['中', '文']`}</Code>
      <Note kind="warning" title="missing is not a warning to ignore">
        Characters the typeface cannot draw are dropped, not substituted. An empty{' '}
        <C>missing</C> array is the only result safe to skip checking.{' '}
        <C>approximated</C> counts blocks drawn as plain text because their structure is
        not drawn: lists without bullets, tables without rules.
      </Note>
      <p>
        One face per document today. Bold and italic need a second and third embedded
        font, which is the next piece of work here.
      </p>
    </>
  )
}

export function Redaction() {
  useHeadings([
    { id: 'removing', text: 'Removing', level: 2 },
    { id: 'verifying', text: 'Verifying', level: 2 },
    { id: 'limits', text: 'What it does not reach', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Redaction"
        summary="Removing content, and then proving it is gone."
      />

      <H2 id="removing">Removing</H2>
      <Code lang="js">{`await doc.redactText('Account 4417-9920')
const { bytes } = await doc.commit()`}</Code>
      <p>
        The glyphs are removed from the content stream and the surrounding text is left
        where it was, rather than closed up. That matters: text that slid left would move
        out from under whatever box was drawn over the original rectangle.
      </p>
      <Note kind="warning" title="Redaction forces a full rewrite">
        This is enforced in code, not documentation. An incremental save appends a
        revision and leaves the original bytes earlier in the file, so the removal would
        be cosmetic and recoverable with a text editor.
      </Note>

      <H2 id="verifying">Verifying</H2>
      <Code lang="js">{`const verdict = await doc.verifyRedaction('Account 4417-9920')

verdict.clean        // true
verdict.occurrences  // 0
verdict.notChecked   // ['image data', 'font subset glyphs']`}</Code>
      <p>
        The search runs over the saved bytes, decompressing streams, so it catches text
        that survived in a place the removal did not reach.
      </p>

      <H2 id="limits">What it does not reach</H2>
      <p>
        <C>notChecked</C> is the important half of the verdict. Two gaps are known and
        reported on every redaction:
      </p>
      <ul>
        <li>
          <strong>Image data.</strong> If the text is part of a scan, removing the text
          layer leaves the picture of it.
        </li>
        <li>
          <strong>Font subset glyphs.</strong> A subset embedded for a redacted name still
          contains that name’s letterforms, which has been used for real de-anonymisation.
        </li>
      </ul>
      <p>
        Treat a clean verdict as a bounded claim. If your threat model includes either of
        those, rasterise the region or rebuild the document.
      </p>
    </>
  )
}

export function Encryption() {
  useHeadings([
    { id: 'reading', text: 'Reading protected files', level: 2 },
    { id: 'writing', text: 'Writing protection', level: 2 },
    { id: 'permissions', text: 'Permissions', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Encryption"
        summary="Reads what exists, writes only what is worth writing."
      />

      <H2 id="reading">Reading protected files</H2>
      <Code lang="js">{`const doc = await Pdf.open(bytes, { password: 'hunter2' })`}</Code>
      <p>
        The empty password is always tried first, which opens the many documents that are
        encrypted with no user password purely to set permission bits. RC4 and every
        revision from 2 to 6 are read, because legacy files exist.
      </p>

      <H2 id="writing">Writing protection</H2>
      <Code lang="js">{`const weaknesses = await doc.protect({
  userPassword: 'open-me',
  ownerPassword: 'owner-only',
  strength: 'aes-256',
})`}</Code>
      <p>
        AES-256 is the only strength written. RC4 and revision 5 are read for
        compatibility and never produced, because writing them would be shipping a known
        weakness to somebody who asked for protection.
      </p>
      <p>
        <C>weaknesses</C> names anything about the request that is weaker than it looks: a
        short password, an empty user password, permissions that no reader is obliged to
        respect.
      </p>

      <H2 id="permissions">Permissions</H2>
      <Note kind="info" title="Reported, never enforced">
        The <C>/P</C> bits say what a conforming reader should allow. Rasura reports them
        and acts on none of them. Enforcing them would claim a security property the
        format does not have: anyone with the file and any library can ignore them.
      </Note>
    </>
  )
}

export function Saving() {
  useHeadings([
    { id: 'incremental', text: 'Incremental', level: 2 },
    { id: 'full', text: 'Full rewrite', level: 2 },
    { id: 'signatures', text: 'Signatures', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Saving"
        summary="Two modes. The default keeps every byte you did not touch."
      />

      <H2 id="incremental">Incremental</H2>
      <Code lang="js">{`const saved = await doc.save()
saved.mode           // 'incremental'
saved.bytesAppended  // 1204`}</Code>
      <p>
        The original file, unchanged, with a new revision appended. An unedited save
        returns the input byte for byte, which is checked across the whole test corpus on
        every run.
      </p>
      <p>
        This is what makes the library usable in workflows where something downstream
        hashes the file, or where prior revisions must remain recoverable.
      </p>

      <H2 id="full">Full rewrite</H2>
      <p>Four states force one, whatever you ask for:</p>
      <ul>
        <li>Redaction, so the removed bytes cannot survive in an earlier revision.</li>
        <li>A protection change, because a mixed-key file cannot be read.</li>
        <li>A document that only opened through recovery, whose original offsets are wrong.</li>
        <li>A composed document, which has no original bytes to append to.</li>
      </ul>
      <Code lang="js">{`const saved = await doc.save({ mode: 'full-rewrite' })`}</Code>

      <H2 id="signatures">Signatures</H2>
      <p>
        A full rewrite invalidates any existing digital signature. Rasura detects that
        before it happens and refuses unless you say otherwise:
      </p>
      <Code lang="js">{`await doc.save({ acceptSignatureDestruction: true })`}</Code>
      <p>
        Creating signatures is out of scope. Detecting, preserving and reporting damage to
        them is not.
      </p>
    </>
  )
}

export function Errors() {
  useHeadings([
    { id: 'shape', text: 'The shape', level: 2 },
    { id: 'recover', text: 'Recovering', level: 2 },
  ])
  return (
    <>
      <PageHeader
        title="Errors"
        summary="Never a bare Error. Every failure carries a code you can branch on."
      />

      <H2 id="shape">The shape</H2>
      <Code lang="js">{`import { PdfError, CODES } from 'rasura'

try {
  await doc.replaceText(range, text)
} catch (e) {
  if (e instanceof PdfError) {
    e.code     // one of CODES
    e.message  // safe to show a user
    e.detail   // what the failing layer said, for a bug report
  }
}`}</Code>
      <p>
        <C>detail</C> keeps the underlying cause rather than discarding it. The surface
        stays free of PDF vocabulary by default, and the escape hatch exists for anyone
        debugging somebody else’s file.
      </p>

      <H2 id="recover">Recovering</H2>
      <p>Three codes are worth handling specifically in most applications.</p>
      <Code lang="js">{`switch (e.code) {
  case 'encrypted-password-required':
    return retryWithPassword()

  case 'font-unavailable':
    await doc.registerFont(fallback)
    return retry()

  case 'fidelity-below-required':
    // The floor cannot be met. Lower it, or tell the user what would be lost.
    return offerLowerFidelity()
}`}</Code>
      <p>
        The full list of fourteen, with what each implies, is in the{' '}
        <Link to="/api">API reference</Link>.
      </p>
    </>
  )
}
