/**
 * Turning Rasura's model into something drawable.
 *
 * The editor does not raster the PDF. There is a dependency reason — Rasura has
 * no renderer and §11.6 says it will not grow one — and a better reason for a
 * demonstration: **every pixel on screen should come from the library.** A
 * pdf.js raster with an overlay shows you pdf.js's rendering and Rasura's
 * annotations of it. This shows what Rasura itself knows.
 *
 * So it is a *model view* and the page says so. Text is placed from paragraph
 * boxes and line counts; images, tables and vector art are drawn as extents. A
 * reader recognises the page without mistaking it for a render.
 *
 * Pure: no DOM, no canvas. The whole data path is testable in node, which is the
 * only part of this that can be tested without a browser at all.
 */

export interface Box {
  x0: number
  y0: number
  x1: number
  y1: number
}

export interface Paragraph {
  id: { region: number; index: number }
  text: string
  box: Box
  lineCount: number
  leading: number
  alignment: string
  textConfidence: 'exact' | 'partial' | 'none'
}

export interface ImageBlock {
  id: { number: number; generation: number } | null
  box: Box
  editable: boolean
  pixels: { width: number; height: number } | null
}

export interface TableBlock {
  id: number
  box: Box
  rows: number
  columns: number
}

export interface PageModel {
  mediaBox: Box
  paragraphs: Paragraph[]
  blocks: { kind: string; box: Box }[]
  images: ImageBlock[]
  tables: TableBlock[]
}

export type Measure = (text: string, size: number) => number

/** Device space is what the library returns: y down, origin at the page's top left. */
export function pageBox(page: PageModel) {
  const b = page.mediaBox
  return { width: Math.abs(b.x1 - b.x0), height: Math.abs(b.y1 - b.y0) }
}

export interface ParagraphLayout {
  size: number
  leading: number
  lines: string[]
  top: number
  left: number
  width: number
  height: number
  alignment: string
}

/**
 * Lay a paragraph's text into lines that fit its own box.
 *
 * Greedy, matching §9.3's default and for the same reason: it is what the
 * document most likely did, so the result looks like the page rather than like
 * a re-typesetting.
 */
export function layoutParagraph(paragraph: Paragraph, measure: Measure): ParagraphLayout {
  const box = paragraph.box
  const width = Math.abs(box.x1 - box.x0)
  const height = Math.abs(box.y1 - box.y0)
  const lineCount = Math.max(1, paragraph.lineCount || 1)

  // The size the document used, recovered from the box rather than assumed: a
  // paragraph five lines tall in sixty points is a twelve-point paragraph, and
  // a fixed guess would make every heading the wrong size.
  const leading = paragraph.leading > 0 ? paragraph.leading : height / lineCount
  const size = Math.max(4, Math.min(leading * 0.82, (height / lineCount) * 0.95))

  const words = (paragraph.text || '').split(/\s+/).filter(Boolean)
  const lines: string[] = []
  let current = ''

  for (const word of words) {
    const candidate = current ? `${current} ${word}` : word
    if (current && measure(candidate, size) > width) {
      lines.push(current)
      current = word
    } else {
      current = candidate
    }
  }
  if (current) lines.push(current)
  if (lines.length === 0) lines.push('')

  return {
    size,
    leading: leading > 0 ? leading : size * 1.2,
    lines,
    top: Math.min(box.y0, box.y1),
    left: Math.min(box.x0, box.x1),
    width,
    height,
    alignment: paragraph.alignment,
  }
}

export type DrawItem =
  | { type: 'block'; kind: string; box: Box }
  | { type: 'table'; id: number; box: Box; rows: number; columns: number }
  | { type: 'image'; id: ImageBlock['id']; box: Box; editable: boolean }
  | {
      type: 'paragraph'
      id: Paragraph['id']
      box: Box
      confidence: string
      layout: ParagraphLayout
    }

/**
 * Everything to draw for one page, in paint order.
 *
 * Order matters: blocks that are not text go down first, so paragraph text sits
 * on top of a figure's extent rather than under it.
 */
export function drawList(page: PageModel, measure: Measure): DrawItem[] {
  const out: DrawItem[] = []
  for (const block of page.blocks) {
    if (block.kind === 'paragraph') continue
    out.push({ type: 'block', kind: block.kind, box: block.box })
  }
  for (const t of page.tables) {
    out.push({ type: 'table', id: t.id, box: t.box, rows: t.rows, columns: t.columns })
  }
  for (const i of page.images) {
    out.push({ type: 'image', id: i.id, box: i.box, editable: i.editable })
  }
  for (const p of page.paragraphs) {
    out.push({
      type: 'paragraph',
      id: p.id,
      box: p.box,
      confidence: p.textConfidence,
      layout: layoutParagraph(p, measure),
    })
  }
  return out
}

/** The paragraph under a point, smallest first — the library's own rule. */
export function paragraphAt(page: PageModel, x: number, y: number): Paragraph | null {
  const hits = page.paragraphs.filter(
    (p) => x >= p.box.x0 && x <= p.box.x1 && y >= p.box.y0 && y <= p.box.y1,
  )
  if (!hits.length) return null
  const area = (b: Box) => Math.abs((b.x1 - b.x0) * (b.y1 - b.y0))
  return hits.reduce((best, p) => (area(p.box) < area(best.box) ? p : best))
}

/** The image under a point. Topmost wins, matching paint order. */
export function imageAt(page: PageModel, x: number, y: number): ImageBlock | null {
  for (let i = page.images.length - 1; i >= 0; i -= 1) {
    const b = page.images[i].box
    if (x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1) return page.images[i]
  }
  return null
}

/**
 * A minimal edit range from an old string to a new one.
 *
 * A whole-paragraph replacement works and reports re-broken lines on every
 * keystroke; trimming the common prefix and suffix keeps most edits local, so
 * the log shows `exact` where the document allows it. The application's job,
 * not the library's — spec §5.3 says so.
 */
export function minimalRange(before: string, after: string) {
  let start = 0
  const max = Math.min(before.length, after.length)
  while (start < max && before[start] === after[start]) start += 1

  let endBefore = before.length
  let endAfter = after.length
  while (endBefore > start && endAfter > start && before[endBefore - 1] === after[endAfter - 1]) {
    endBefore -= 1
    endAfter -= 1
  }
  return { start, end: endBefore, text: after.slice(start, endAfter) }
}
