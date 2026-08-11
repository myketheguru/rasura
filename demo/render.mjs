// Turning Rasura's model into something drawable.
//
// The demo does not raster the PDF. `docs/demo-editor.md` §1 explains why in
// terms of a dependency; here there is a second reason that matters more for a
// demonstration: **every pixel on screen should come from the library.** A
// pdf.js raster with an overlay shows you pdf.js's rendering and Rasura's
// annotations of it. This shows you what Rasura itself knows, which is the
// thing being demonstrated.
//
// So it is a *model view*, and the UI says so. Text is placed from paragraph
// boxes and line counts; images, tables and vector art are drawn as their
// extents. A reader will recognise the page without mistaking it for a render.
//
// This file is pure — no DOM, no canvas — so the whole data path can be tested
// in node, which is the only part of the demo that can be tested at all without
// a browser.

/** Device space is what the library returns: y down, origin at the page's top left. */
export function pageBox(page) {
  const b = page.mediaBox;
  return { width: Math.abs(b.x1 - b.x0), height: Math.abs(b.y1 - b.y0) };
}

/**
 * Lay a paragraph's text into lines that fit its own box.
 *
 * `measure` is a function from string to width in points — canvas's
 * `measureText` in the browser, an approximation in tests. Greedy, matching
 * §9.3's default, and for the same reason: it is what the document most likely
 * did, so the result looks like the page rather than like a re-typesetting.
 */
export function layoutParagraph(paragraph, measure) {
  const box = paragraph.box;
  const width = Math.abs(box.x1 - box.x0);
  const height = Math.abs(box.y1 - box.y0);
  const lineCount = Math.max(1, paragraph.lineCount || 1);

  // The size the document used, recovered from the box rather than assumed: a
  // paragraph five lines tall in sixty points is a twelve-point paragraph, and
  // guessing a fixed size would make every heading the wrong size.
  const leading = paragraph.leading > 0 ? paragraph.leading : height / lineCount;
  const size = Math.max(4, Math.min(leading * 0.82, height / lineCount * 0.95));

  const words = (paragraph.text || '').split(/\s+/).filter(Boolean);
  const lines = [];
  let current = '';

  for (const word of words) {
    const candidate = current ? `${current} ${word}` : word;
    if (current && measure(candidate, size) > width) {
      lines.push(current);
      current = word;
    } else {
      current = candidate;
    }
  }
  if (current) lines.push(current);
  if (lines.length === 0) lines.push('');

  return {
    size,
    leading: leading > 0 ? leading : size * 1.2,
    lines,
    // The first baseline sits one em below the top of the box, which is where
    // a leading of 1.2 puts it.
    top: Math.min(box.y0, box.y1),
    left: Math.min(box.x0, box.x1),
    width,
    height,
    alignment: paragraph.alignment,
  };
}

/**
 * Everything to draw for one page, in paint order.
 *
 * Order matters: blocks that are not text go down first so paragraph text sits
 * on top of a figure's extent rather than under it.
 */
export function drawList(page, measure) {
  const out = [];

  for (const block of page.blocks) {
    if (block.kind === 'paragraph') continue;
    out.push({ type: 'block', kind: block.kind, box: block.box });
  }
  for (const table of page.tables) {
    out.push({ type: 'table', id: table.id, box: table.box, rows: table.rows, columns: table.columns });
  }
  for (const image of page.images) {
    out.push({ type: 'image', id: image.id, box: image.box, editable: image.editable, pixels: image.pixels });
  }
  for (const paragraph of page.paragraphs) {
    out.push({
      type: 'paragraph',
      id: paragraph.id,
      box: paragraph.box,
      confidence: paragraph.textConfidence,
      layout: layoutParagraph(paragraph, measure),
    });
  }
  return out;
}

/**
 * The paragraph under a point, smallest first.
 *
 * The same rule as the library's own `paragraphAt`, reimplemented here because
 * the demo talks to the raw WASM surface rather than to the npm wrapper —
 * and stated in one place so a click and a hover cannot disagree.
 */
export function paragraphAt(page, x, y) {
  const hits = page.paragraphs.filter(
    (p) => x >= p.box.x0 && x <= p.box.x1 && y >= p.box.y0 && y <= p.box.y1,
  );
  if (!hits.length) return null;
  const area = (b) => Math.abs((b.x1 - b.x0) * (b.y1 - b.y0));
  return hits.reduce((best, p) => (area(p.box) < area(best.box) ? p : best));
}

/** The image under a point, if any. Topmost wins, matching paint order. */
export function imageAt(page, x, y) {
  for (let i = page.images.length - 1; i >= 0; i -= 1) {
    const b = page.images[i].box;
    if (x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1) return page.images[i];
  }
  return null;
}

/**
 * A minimal edit range from an old string to a new one.
 *
 * A whole-paragraph replacement works and reports re-broken lines on every
 * keystroke; trimming the common prefix and suffix keeps most edits local, so
 * the fidelity log shows `exact` where the document allows it. This is the
 * application's job, not the library's — §5.3 of the spec says so.
 */
export function minimalRange(before, after) {
  let start = 0;
  const max = Math.min(before.length, after.length);
  while (start < max && before[start] === after[start]) start += 1;

  let endBefore = before.length;
  let endAfter = after.length;
  while (endBefore > start && endAfter > start && before[endBefore - 1] === after[endAfter - 1]) {
    endBefore -= 1;
    endAfter -= 1;
  }
  return { start, end: endBefore, text: after.slice(start, endAfter) };
}
