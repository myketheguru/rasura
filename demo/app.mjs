// Rasura Studio — the demonstration editor.
//
// Talks to the WASM surface directly rather than through the npm wrapper. The
// wrapper starts a Worker and owns the transport, which is right for an
// application and unnecessary here: this page is static files on GitHub Pages,
// the module is same-origin, and the whole transport is a function call. What
// the wrapper otherwise provides — error normalisation — is thirty lines below.
//
// Everything else is the same library the npm package exposes.
//
// Deployment: plain static files, relative paths only, no COOP/COEP headers.
// The single-threaded build (§12.1) is what makes that last point true, and it
// is why this can live on Pages at all — cross-origin isolation is not
// something a Pages site can ask for.

import { drawList, imageAt, minimalRange, pageBox, paragraphAt } from './render.mjs';
import init, {
  addAnnotation, closeDocument, commitSession, compactFonts, configureSession,
  deleteImage, deletePage, documentInfo, documentMetadata, flattenForms,
  fontRequirements, formFields, moveImage, openDocument, pageContent,
  redactText, redo, registerFont, replaceText, rollbackSession, saveDocument,
  sessionStatus, setFieldValue, undo, verifyRedaction, version,
} from './rasura_wasm.js';

// --- state ------------------------------------------------------------------

const state = {
  handle: null,
  bytes: null, // the bytes currently open, for verification and re-opening
  info: null,
  page: null, // the page model for `pageIndex`
  pageIndex: 0,
  pageCount: 0,
  scale: 1,
  selection: null, // { kind: 'paragraph' | 'image', id }
  floor: 'overlaid',
  log: [],
  lastSave: null,
  tab: 'document',
};

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

/** The bundled document, fetched from beside the page. */
async function openSample() {
  const response = await fetch('./sample.pdf');
  if (!response.ok) {
    note('open', 'failed', `sample.pdf could not be fetched (${response.status})`);
    return;
  }
  await open(new Uint8Array(await response.arrayBuffer()), undefined);
}

function base64ToBytes(base64) {
  const binary = atob(base64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

// --- the one place errors are handled ---------------------------------------
//
// §11.5: never a bare error. Every failure carries a code, and the UI branches
// on the code rather than on a message — so this maps each of the thirteen to
// something a user can act on.

const REMEDY = {
  'encrypted-password-required': 'This document needs a password to open.',
  'encrypted-unsupported': 'The encryption in this file is not one the library reads.',
  malformed: 'The file could not be read. Cross-reference recovery is already on.',
  'scanned-no-text': 'This page is a picture of a page. There is no OCR, so there is no text to edit.',
  'xfa-unsupported': 'The real content is an XFA payload the form shadows. Editing is refused.',
  'type3-glyph-missing': 'A Type 3 glyph procedure is missing, so this text cannot be re-encoded.',
  'font-unavailable': 'The font cannot write that character. Supply the typeface in the Fonts panel.',
  overflow: 'The text no longer fits its block. Change the overflow policy and try again.',
  'stale-session': 'The session ended. The document has been refreshed.',
  'fidelity-below-required': 'Refused: this edit could not meet the fidelity floor you set.',
  'signature-would-be-destroyed': 'Saving would invalidate a digital signature, so it was refused.',
  'unsupported-filter': 'A stream uses a filter the library does not decode.',
  internal: 'Something went wrong inside the library.',
};

function report(e, context) {
  const code = e && typeof e.code === 'string' ? e.code : null;
  const message = code ? REMEDY[code] ?? e.message : String(e && e.message ? e.message : e);
  note(context, code ?? 'uncoded', message);
  if (!code) console.error('uncoded throw — this is a bug in the demo, not the document', e);
  return code;
}

// --- the fidelity log -------------------------------------------------------

function note(operation, rung, detail = '') {
  const time = new Date().toLocaleTimeString();
  state.log.unshift({ time, operation, rung, detail });
  if (state.tab === 'log') renderInspector();
  const bar = $('result');
  bar.textContent = `${operation}: ${rung}${detail ? ` — ${detail}` : ''}`;
  bar.dataset.rung = rung;
}

function noteOutcome(operation, outcome) {
  const detail = [
    outcome.missingGlyphs?.length ? `missing: ${outcome.missingGlyphs.join(' ')}` : '',
    outcome.reflowedLines != null ? `reflowed to ${outcome.reflowedLines} lines` : '',
    ...(outcome.warnings ?? []),
  ]
    .filter(Boolean)
    .join('; ');
  note(operation, outcome.fidelity, detail);
}

// --- opening ----------------------------------------------------------------

async function open(bytes, password) {
  if (state.handle != null) {
    closeDocument(state.handle);
    state.handle = null;
  }
  try {
    state.handle = openDocument(bytes, password, undefined);
  } catch (e) {
    const code = report(e, 'open');
    if (code === 'encrypted-password-required') {
      const entered = prompt('This document is encrypted. Password:');
      if (entered !== null) return open(bytes, entered);
    }
    return false;
  }

  state.bytes = bytes;
  state.info = documentInfo(state.handle);
  state.pageCount = state.info.pageCount;
  state.pageIndex = 0;
  state.selection = null;
  state.lastSave = null;
  configureSession(state.handle, state.floor, 'greedy', 'refuse');

  await loadPage(0);
  renderThumbnails();
  renderInspector();
  renderSession();
  note('open', 'ok', `${state.pageCount} page(s), ${state.info.documentKind}`);
  return true;
}

async function loadPage(index) {
  state.pageIndex = Math.max(0, Math.min(index, state.pageCount - 1));
  state.page = pageContent(state.handle, state.pageIndex);
  state.selection = null;
  draw();
  $('page-label').textContent = `${state.pageIndex + 1} / ${state.pageCount}`;
}

/** Everything cached is invalidated together — paragraph ids are page-scoped. */
async function refresh() {
  state.info = documentInfo(state.handle);
  state.pageCount = state.info.pageCount;
  await loadPage(state.pageIndex);
  renderThumbnails();
  renderInspector();
  renderSession();
}

// --- drawing ----------------------------------------------------------------

const canvas = () => $('page-canvas');

function fitScale() {
  const box = pageBox(state.page);
  const available = $('stage').clientWidth - 48;
  return Math.max(0.2, Math.min(2, available / box.width));
}

function draw() {
  if (!state.page) return;
  const c = canvas();
  const ctx = c.getContext('2d');
  const box = pageBox(state.page);
  state.scale = fitScale();

  const dpr = window.devicePixelRatio || 1;
  c.width = Math.round(box.width * state.scale * dpr);
  c.height = Math.round(box.height * state.scale * dpr);
  c.style.width = `${box.width * state.scale}px`;
  c.style.height = `${box.height * state.scale}px`;
  ctx.setTransform(dpr * state.scale, 0, 0, dpr * state.scale, 0, 0);

  ctx.fillStyle = '#ffffff';
  ctx.fillRect(0, 0, box.width, box.height);

  const measure = (text, size) => {
    ctx.font = `${size}px "Helvetica Neue", Helvetica, Arial, sans-serif`;
    return ctx.measureText(text).width;
  };

  for (const item of drawList(state.page, measure)) {
    if (item.type === 'block') {
      ctx.strokeStyle = item.kind === 'vector' ? '#cbd5e1' : '#e2e8f0';
      ctx.setLineDash([3, 3]);
      ctx.strokeRect(item.box.x0, item.box.y0, item.box.x1 - item.box.x0, item.box.y1 - item.box.y0);
      ctx.setLineDash([]);
    } else if (item.type === 'table') {
      ctx.strokeStyle = '#94a3b8';
      ctx.strokeRect(item.box.x0, item.box.y0, item.box.x1 - item.box.x0, item.box.y1 - item.box.y0);
    } else if (item.type === 'image') {
      const w = item.box.x1 - item.box.x0;
      const h = item.box.y1 - item.box.y0;
      ctx.fillStyle = '#eef2f7';
      ctx.fillRect(item.box.x0, item.box.y0, w, h);
      ctx.strokeStyle = item.editable ? '#64748b' : '#cbd5e1';
      ctx.strokeRect(item.box.x0, item.box.y0, w, h);
      ctx.fillStyle = '#94a3b8';
      ctx.font = '9px sans-serif';
      const label = item.pixels ? `image ${item.pixels.width}x${item.pixels.height}` : 'image';
      ctx.fillText(label, item.box.x0 + 4, item.box.y0 + 12);
    } else if (item.type === 'paragraph') {
      const { size, leading, lines, left, top } = item.layout;
      ctx.fillStyle = item.confidence === 'none' ? '#94a3b8' : '#0f172a';
      ctx.font = `${size}px "Helvetica Neue", Helvetica, Arial, sans-serif`;
      lines.forEach((line, i) => ctx.fillText(line, left, top + size + i * leading));
    }
  }

  drawSelection(ctx);
}

function drawSelection(ctx) {
  if (!state.selection) return;
  const box = selectionBox();
  if (!box) return;
  ctx.strokeStyle = '#2563eb';
  ctx.lineWidth = 1.5;
  ctx.strokeRect(box.x0 - 2, box.y0 - 2, box.x1 - box.x0 + 4, box.y1 - box.y0 + 4);
  ctx.lineWidth = 1;

  if (state.selection.kind === 'image') {
    const image = state.page.images.find((i) => i.id === state.selection.id);
    if (image?.editable) {
      ctx.fillStyle = '#2563eb';
      for (const [x, y] of [
        [box.x1, box.y1],
        [box.x0, box.y0],
      ]) {
        ctx.fillRect(x - 3, y - 3, 6, 6);
      }
    }
  }
}

function selectionBox() {
  if (!state.selection) return null;
  const list = state.selection.kind === 'paragraph' ? state.page.paragraphs : state.page.images;
  return list.find((item) => item.id === state.selection.id)?.box ?? null;
}

/** Canvas pixels to device space. The only conversion in the application. */
function toDevice(event) {
  const rect = canvas().getBoundingClientRect();
  return {
    x: (event.clientX - rect.left) / state.scale,
    y: (event.clientY - rect.top) / state.scale,
  };
}

// --- interaction ------------------------------------------------------------

let drag = null;

function onPointerDown(event) {
  if (!state.page) return;
  const { x, y } = toDevice(event);

  const image = imageAt(state.page, x, y);
  if (image) {
    state.selection = { kind: 'image', id: image.id };
    if (image.editable) drag = { id: image.id, from: { x, y }, moved: false };
    draw();
    renderInspector();
    return;
  }

  const paragraph = paragraphAt(state.page, x, y);
  state.selection = paragraph ? { kind: 'paragraph', id: paragraph.id } : null;
  draw();
  renderInspector();
}

function onPointerMove(event) {
  if (!drag) return;
  drag.moved = true;
  const { x, y } = toDevice(event);
  const image = state.page.images.find((i) => i.id === drag.id);
  if (!image) return;
  // Optimistic: the model is authoritative and is refetched after the edit.
  const dx = x - drag.from.x;
  const dy = y - drag.from.y;
  const box = image.box;
  image.box = { x0: box.x0 + dx, y0: box.y0 + dy, x1: box.x1 + dx, y1: box.y1 + dy };
  drag.from = { x, y };
  drag.total = { x: (drag.total?.x ?? 0) + dx, y: (drag.total?.y ?? 0) + dy };
  draw();
}

async function onPointerUp() {
  if (!drag) return;
  const finished = drag;
  drag = null;
  if (!finished.moved || !finished.total) return;
  try {
    const outcome = moveImage(
      state.handle,
      state.pageIndex,
      finished.id,
      finished.total.x,
      finished.total.y,
    );
    noteOutcome('moveImage', outcome);
  } catch (e) {
    report(e, 'moveImage');
  }
  await afterEdit();
}

async function onDoubleClick(event) {
  if (!state.page) return;
  const { x, y } = toDevice(event);
  const paragraph = paragraphAt(state.page, x, y);
  if (!paragraph) return;

  if (paragraph.textConfidence === 'none') {
    note('edit', 'refused', 'no glyph in this paragraph resolved to a character; editing it would be guessing');
    return;
  }
  const next = prompt('Edit paragraph', paragraph.text);
  if (next === null || next === paragraph.text) return;

  const range = minimalRange(paragraph.text, next);
  try {
    const outcome = replaceText(
      state.handle,
      state.pageIndex,
      paragraph.id,
      range.start,
      range.end,
      range.text,
    );
    noteOutcome('replaceText', outcome);
  } catch (e) {
    report(e, 'replaceText');
  }
  await afterEdit();
}

/** After any staged edit: the model is stale, the session status changed. */
async function afterEdit() {
  await loadPage(state.pageIndex);
  renderSession();
  renderInspector();
}

// --- session ----------------------------------------------------------------

function renderSession() {
  if (state.handle == null) return;
  const status = sessionStatus(state.handle);
  $('staged').textContent = `${status.staged} staged`;
  $('undo').disabled = !status.canUndo;
  $('redo').disabled = !status.canRedo;
  $('rollback').disabled = !status.canUndo;
  $('commit').disabled = status.staged === 0;
  $('memory').textContent = `${Math.round((state.info?.memoryUsage ?? 0) / 1024)} KB held`;
}

async function doCommit() {
  try {
    const saved = commitSession(state.handle, undefined);
    state.lastSave = saved;
    state.bytes = saved.bytes;
    note('commit', saved.mode, `${saved.bytesAppended} bytes appended`);
    // The document in the module has been written; reopen so the session and
    // the model are consistent with the bytes the user now has.
    await open(saved.bytes, undefined);
    showBytes(saved);
  } catch (e) {
    report(e, 'commit');
    await refresh();
  }
}

function showBytes(saved) {
  const panel = $('bytes');
  panel.innerHTML = '';
  panel.appendChild(el('h3', null, 'Saved bytes'));
  panel.appendChild(
    el(
      'p',
      'muted',
      saved.mode === 'incremental'
        ? `Incremental. The original ${(saved.bytes.length - saved.bytesAppended).toLocaleString()} bytes are untouched; ${saved.bytesAppended.toLocaleString()} were appended.`
        : `Full rewrite — ${saved.bytes.length.toLocaleString()} bytes. Redaction and protection changes force this.`,
    ),
  );
  const tail = saved.bytes.slice(Math.max(0, saved.bytes.length - 420));
  let text = '';
  for (const byte of tail) text += byte >= 32 && byte < 127 ? String.fromCharCode(byte) : byte === 10 ? '\n' : '·';
  panel.appendChild(el('pre', 'bytes', text));

  const download = el('button', 'primary', 'Download');
  download.onclick = () => {
    const url = URL.createObjectURL(new Blob([saved.bytes], { type: 'application/pdf' }));
    const a = el('a');
    a.href = url;
    a.download = 'edited.pdf';
    a.click();
    URL.revokeObjectURL(url);
  };
  panel.appendChild(download);
  panel.hidden = false;
}

// --- inspector --------------------------------------------------------------

function renderInspector() {
  const body = $('inspector-body');
  body.innerHTML = '';
  if (state.handle == null) return;

  if (state.tab === 'document') renderDocumentTab(body);
  else if (state.tab === 'fonts') renderFontsTab(body);
  else if (state.tab === 'fields') renderFieldsTab(body);
  else renderLogTab(body);
}

function row(parent, label, value, cls) {
  const line = el('div', `row ${cls ?? ''}`);
  line.appendChild(el('span', 'key', label));
  line.appendChild(el('span', 'value', value));
  parent.appendChild(line);
}

function renderDocumentTab(body) {
  const i = state.info;
  row(body, 'Pages', String(i.pageCount));
  row(body, 'Kind', i.documentKind);
  row(body, 'Tagged', i.taggedStatus);
  row(body, 'Encrypted', i.encrypted ? 'yes' : 'no');
  row(body, 'Revisions', String(i.revisionCount));
  if (i.hasXfa) row(body, 'XFA', 'present — editing refused', 'warn');

  body.appendChild(el('h3', null, 'Permissions'));
  body.appendChild(
    el('p', 'muted', 'Reported, never enforced. Whether to honour a bit that says "no printing" is the application\'s decision, not a parser\'s.'),
  );
  for (const [key, value] of Object.entries(i.permissions)) {
    row(body, key, value ? 'allowed' : 'denied');
  }

  body.appendChild(el('h3', null, 'Leniencies'));
  if (i.leniencies.length === 0) {
    body.appendChild(el('p', 'muted', 'None. Every structure in this file matched the specification.'));
  } else {
    body.appendChild(
      el('p', 'muted', 'Specification deviations tolerated while reading. No other viewer will tell you these.'),
    );
    for (const l of i.leniencies) body.appendChild(el('div', 'lenient', l));
  }

  const meta = documentMetadata(state.handle);
  body.appendChild(el('h3', null, 'Metadata'));
  for (const key of ['title', 'author', 'producer']) {
    if (meta.info[key]) row(body, key, meta.info[key]);
  }
  if (meta.disagreements.length) {
    body.appendChild(el('p', 'warn', `${meta.disagreements.length} field(s) where /Info and XMP disagree`));
    for (const d of meta.disagreements) row(body, d.field, `Info: ${d.info} · XMP: ${d.xmp}`, 'warn');
  }
}

function renderFontsTab(body) {
  const fonts = fontRequirements(state.handle);
  body.appendChild(
    el('p', 'muted', 'A browser cannot see system fonts. A PDF usually embeds only the letters it used, so a document saying "Hamburg" carries seven glyphs and cannot type an eighth.'),
  );
  if (!fonts.length) body.appendChild(el('p', 'muted', 'No fonts reported.'));

  for (const font of fonts) {
    const card = el('div', 'card');
    card.appendChild(el('div', 'card-title', font.family));
    row(card, 'PDF name', font.pdfFont);
    row(card, 'Embedded', font.embedded ? 'yes' : 'no');
    row(card, 'Subset', font.subset ? 'yes' : 'no');
    row(card, 'Latin coverage', `${font.coverage} (${font.writableLatin}/95 writable)`);

    if (font.needsSupplying) {
      const supply = el('button', null, 'Supply this typeface…');
      supply.onclick = () => {
        const input = el('input');
        input.type = 'file';
        input.accept = '.ttf,.otf,.woff';
        input.onchange = async () => {
          const file = input.files?.[0];
          if (!file) return;
          const bytes = new Uint8Array(await file.arrayBuffer());
          try {
            const count = registerFont(state.handle, bytes, font.family);
            note('registerFont', 'ok', `${count} font(s) registered — the outline is injected when an edit needs it`);
          } catch (e) {
            report(e, 'registerFont');
          }
          renderInspector();
        };
        input.click();
      };
      card.appendChild(supply);
    }
    body.appendChild(card);
  }
}

function renderFieldsTab(body) {
  const fields = formFields(state.handle);
  if (!fields.length) {
    body.appendChild(el('p', 'muted', 'This document has no form fields.'));
    return;
  }
  body.appendChild(el('p', 'muted', 'Addressed by fully-qualified name — every ancestor\'s /T joined with a dot, which is what ISO 32000-1 §12.7.3.2 makes a field\'s identity.'));

  for (const field of fields) {
    const card = el('div', 'card');
    card.appendChild(el('div', 'card-title', field.name));
    row(card, 'Kind', field.kind);
    const input = el('input');
    input.value = field.value ?? '';
    input.onchange = async () => {
      try {
        noteOutcome('setFieldValue', setFieldValue(state.handle, field.name, input.value));
      } catch (e) {
        report(e, 'setFieldValue');
      }
      await afterEdit();
    };
    card.appendChild(input);
    body.appendChild(card);
  }

  const flatten = el('button', null, 'Flatten this page\'s fields');
  flatten.onclick = async () => {
    if (!confirm('Flattening is one-way: the form stops being a form. Continue?')) return;
    try {
      noteOutcome('flattenForms', flattenForms(state.handle, state.pageIndex));
    } catch (e) {
      report(e, 'flattenForms');
    }
    await afterEdit();
  };
  body.appendChild(flatten);
}

function renderLogTab(body) {
  body.appendChild(
    el('p', 'muted', 'Every operation, with how faithfully it was performed. Fidelity is a return value, not an exception — an editor that discards it is an editor that lies.'),
  );
  if (!state.log.length) body.appendChild(el('p', 'muted', 'Nothing yet.'));
  for (const entry of state.log) {
    const line = el('div', 'log');
    line.appendChild(el('span', 'time', entry.time));
    line.appendChild(el('span', 'op', entry.operation));
    const rung = el('span', 'rung', entry.rung);
    rung.dataset.rung = entry.rung;
    line.appendChild(rung);
    if (entry.detail) line.appendChild(el('span', 'detail', entry.detail));
    body.appendChild(line);
  }
}

// --- thumbnails -------------------------------------------------------------

function renderThumbnails() {
  const list = $('thumbs');
  list.innerHTML = '';
  for (let i = 0; i < state.pageCount; i += 1) {
    const item = el('button', `thumb${i === state.pageIndex ? ' current' : ''}`);
    item.appendChild(el('span', 'n', String(i + 1)));
    item.onclick = () => loadPage(i).then(renderThumbnails);
    list.appendChild(item);
  }
}

// --- actions ----------------------------------------------------------------

function wire() {
  $('open-file').onchange = async () => {
    const file = $('open-file').files?.[0];
    if (!file) return;
    await open(new Uint8Array(await file.arrayBuffer()), undefined);
  };
  $('open-sample').onclick = () => openSample();

  $('prev').onclick = () => loadPage(state.pageIndex - 1).then(renderThumbnails);
  $('next').onclick = () => loadPage(state.pageIndex + 1).then(renderThumbnails);

  $('floor').onchange = () => {
    state.floor = $('floor').value;
    configureSession(state.handle, state.floor, 'greedy', 'refuse');
    note('requireFidelity', state.floor, 'operations that cannot reach this rung will now fail instead of degrading');
  };

  $('undo').onclick = async () => {
    note('undo', undo(state.handle) ? 'ok' : 'nothing to undo');
    await afterEdit();
  };
  $('redo').onclick = async () => {
    note('redo', redo(state.handle) ? 'ok' : 'nothing to redo');
    await afterEdit();
  };
  $('rollback').onclick = async () => {
    rollbackSession(state.handle);
    note('rollback', 'ok', 'every staged operation discarded');
    await open(state.bytes, undefined);
  };
  $('commit').onclick = doCommit;

  $('save').onclick = () => {
    try {
      const saved = saveDocument(state.handle, undefined);
      note('save', saved.mode, `${saved.bytesAppended} bytes appended`);
      showBytes(saved);
    } catch (e) {
      report(e, 'save');
    }
  };

  $('annotate').onclick = async () => {
    const box = selectionBox();
    if (!box) {
      note('addAnnotation', 'refused', 'select a paragraph or image first');
      return;
    }
    try {
      const outcome = addAnnotation(state.handle, state.pageIndex, {
        kind: 'Square',
        rect: box,
        colour: [0.85, 0.1, 0.1],
        borderWidth: 1.5,
        contents: 'flagged in Rasura Studio',
      });
      noteOutcome('addAnnotation', outcome);
    } catch (e) {
      report(e, 'addAnnotation');
    }
    await afterEdit();
  };

  $('delete-image').onclick = async () => {
    if (state.selection?.kind !== 'image') {
      note('deleteImage', 'refused', 'select an image first');
      return;
    }
    try {
      noteOutcome('deleteImage', deleteImage(state.handle, state.pageIndex, state.selection.id));
    } catch (e) {
      report(e, 'deleteImage');
    }
    await afterEdit();
  };

  $('delete-page').onclick = async () => {
    if (state.pageCount < 2) {
      note('deletePage', 'refused', 'a document needs a page');
      return;
    }
    try {
      noteOutcome('deletePage', deletePage(state.handle, state.pageIndex));
    } catch (e) {
      report(e, 'deletePage');
    }
    await refresh();
  };

  $('compact').onclick = () => {
    try {
      note('compactFonts', 'ok', `${compactFonts(state.handle)} font(s) pruned to the glyphs the document draws`);
    } catch (e) {
      report(e, 'compactFonts');
    }
  };

  $('redact').onclick = async () => {
    const text = prompt('Remove every trace of which string?');
    if (!text) return;
    try {
      const removed = redactText(state.handle, text);
      if (!removed.length) {
        note('redact', 'not found', `no occurrence of ${JSON.stringify(text)}`);
        return;
      }
      note('redact', 'removed', `${removed.length} occurrence(s) — this is not undoable`);

      const saved = saveDocument(state.handle, undefined);
      state.bytes = saved.bytes;
      const verdict = verifyRedaction(saved.bytes, removed);
      showVerification(verdict, saved);
      await open(saved.bytes, undefined);
    } catch (e) {
      report(e, 'redact');
    }
  };

  const c = canvas();
  c.addEventListener('pointerdown', onPointerDown);
  window.addEventListener('pointermove', onPointerMove);
  window.addEventListener('pointerup', onPointerUp);
  c.addEventListener('dblclick', onDoubleClick);
  window.addEventListener('resize', () => state.page && draw());

  for (const tab of document.querySelectorAll('[data-tab]')) {
    tab.onclick = () => {
      state.tab = tab.dataset.tab;
      for (const other of document.querySelectorAll('[data-tab]')) {
        other.classList.toggle('active', other === tab);
      }
      renderInspector();
    };
  }
}

function showVerification(verdict, saved) {
  const panel = $('bytes');
  panel.innerHTML = '';
  panel.appendChild(el('h3', null, 'Redaction verified'));
  panel.appendChild(
    el('p', verdict.clean ? 'ok' : 'warn',
      verdict.clean
        ? `Clean: no trace found in ${verdict.objectsChecked} objects and ${verdict.streamsChecked} streams.`
        : `${verdict.traces.length} trace(s) still present.`),
  );
  panel.appendChild(el('p', 'muted', `Save mode: ${saved.mode} — redaction forces this, in code rather than in documentation.`));
  panel.appendChild(el('h3', null, 'Where this check did not look'));
  panel.appendChild(el('p', 'muted', 'A clean report means "not found where we searched", not "not present". These are the exclusions:'));
  for (const place of verdict.notChecked) panel.appendChild(el('div', 'lenient', place));
  panel.hidden = false;
}

// --- boot -------------------------------------------------------------------

async function main() {
  try {
    // The path is passed explicitly because the module is built with
    // `--omit-default-module-path` (crates/rasura-wasm/build.sh), and that flag
    // does exactly one thing: it removes the `import.meta.url` fallback the
    // glue would otherwise use when called with no argument. Calling `init()`
    // bare therefore reached `WebAssembly.instantiate(undefined, …)` — which is
    // what shipped, and what never once started in a browser.
    //
    // Relative to this module, not the document: Pages serves the demo from a
    // subdirectory, and `./` against the page would resolve to the wrong place
    // the moment anyone hosts it under a path.
    await init({ module_or_path: new URL('./rasura_wasm_bg.wasm', import.meta.url) });
  } catch (e) {
    // State the error and the candidates. This used to assert a CSP without
    // wasm-unsafe-eval, confidently and wrongly, for every possible cause —
    // including the one that was actually shipping. A diagnosis nobody checked
    // is worse than none: it sends the reader somewhere the fault is not.
    document.body.innerHTML =
      '<div class="fatal"><h1>WebAssembly could not start</h1>' +
      '<p>The module did not compile. Usually one of: the host serves ' +
      '<code>.wasm</code> as something other than <code>application/wasm</code>, ' +
      'a content-security policy is missing <code>wasm-unsafe-eval</code>, or ' +
      '<code>rasura_wasm_bg.wasm</code> is not beside ' +
      '<code>rasura_wasm.js</code>. The error itself says which:</p>' +
      `<pre>${String(e)}</pre></div>`;
    return;
  }

  $('version').textContent = `rasura ${version()}`;
  wire();
  await openSample();
}

main();
