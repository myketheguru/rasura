// Hand-written, per spec 12.4:
//
//   "Full TypeScript declarations, hand-checked, no `any` in the public
//    surface."
//
// Not generated. `wasm-bindgen` emits declarations that describe the ABI rather
// than the API — every structured return type comes out as `any`, which is
// precisely where a caller most needs a type. These are written against the
// same shapes `crates/rasura-wasm/src/convert.rs` builds, and the test
// suite checks the two agree at runtime.
//
// There is no `any` below. `unknown` appears once, on `PdfError.detail`, where
// it is the honest type: the detail is whatever the failing layer said.

/** Spec 11.5. Every failure carries one of these. */
export type PdfErrorCode =
  | "malformed"
  | "encrypted-password-required"
  | "encrypted-unsupported"
  | "scanned-no-text"
  | "xfa-unsupported"
  | "type3-glyph-missing"
  | "font-unavailable"
  | "overflow"
  | "stale-session"
  | "fidelity-below-required"
  | "signature-would-be-destroyed"
  | "unsupported-filter"
  | "invalid-argument"
  | "internal";

export const CODES: readonly PdfErrorCode[];

/** A block of a document to be composed. Spec 9.2. */
export type Content =
  | { kind: "heading"; level: 1 | 2 | 3 | 4 | 5 | 6; text: string }
  | { kind: "paragraph"; text: string }
  /** Drawn as its lines, without bullets; counted in `Composition.approximated`. */
  | { kind: "list"; items: readonly string[] };

export interface CreateOptions extends OpenOptions {
  /** Defaults to `"letter"`. */
  pageSize?: "letter" | "a4";
  /** Every margin, in points. 72 is an inch. */
  margin?: number;
  columns?: number;
  /** Space between columns, in points. Ignored for one column. */
  gutter?: number;
  bodySize?: number;
  /** Sizes for heading levels 1 to 6. Short arrays leave the rest at default. */
  headingSizes?: readonly number[];
  /** `/Info /Title`, which is what a viewer shows in its window bar. */
  title?: string;
}

/** What composing did, beyond producing the document. */
export interface Composition {
  pages: number;
  lines: number;
  /** Blocks drawn as plain text because their structure is not drawn. */
  approximated: number;
  /**
   * Characters the typeface has no glyph for. **Dropped, not substituted** —
   * spec 2's second property. An empty array is the only safe result to ignore.
   */
  missing: string[];
  /** `/BaseFont`, subset tag included. */
  baseFont: string;
  /** True when the text needed a Type0 font: anything outside WinAnsi. */
  composite: boolean;
  /** `/StemV` cannot be measured from a TrueType file, so it was estimated. */
  stemVEstimated: boolean;
}

export interface Composed {
  document: Document;
  report: Composition;
}

/** Spec 11.5: never a bare `Error`. */
export class PdfError extends Error {
  readonly name: "PdfError";
  readonly code: PdfErrorCode;
  /** What the failing layer said, when it said anything. */
  readonly detail: unknown;
  constructor(code: PdfErrorCode, message: string, detail?: unknown);
}

export interface OpenOptions {
  password?: string;
  /** Rebuild the cross-reference table when it cannot be followed. Default `'auto'`. */
  recovery?: "auto" | "never";
  /**
   * Run in a Worker. Default `true` (§12.2).
   *
   * `false` runs on the calling thread — for a caller who manages their own
   * Worker, or who is debugging and wants a stack that does not stop at a
   * message boundary.
   */
  worker?: boolean;
  /**
   * Transfer the input buffer rather than copying it. Default `true` (§12.2).
   *
   * **Transferring detaches the caller's buffer**: after `open`, the
   * `ArrayBuffer` or `Uint8Array` passed in has zero length and cannot be read.
   * That is the point — a 20 MB document is not copied — and it is surprising
   * enough to be worth turning off when the same bytes are needed twice.
   *
   * Ignored for a `Blob`, whose bytes are read out here; the caller keeps it.
   */
  transfer?: boolean;
  /**
   * Where to load the `.wasm` from. Spec 12.4's override for CSP-strict and
   * CDN-hosted consumers. Defaults to the asset beside the package.
   */
  wasmUrl?: string | URL;
}

export interface Rect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export interface Point {
  x: number;
  y: number;
}

/** Spec 5.5: advisory. Reported, never enforced. */
export interface Permissions {
  print: boolean;
  modify: boolean;
  copy: boolean;
  annotate: boolean;
  fillForms: boolean;
  extractForAccessibility: boolean;
  assemble: boolean;
  printHighQuality: boolean;
}

export type DocumentKind = "born-digital" | "scanned" | "mixed";
export type TaggedStatus = "untagged" | "tagged" | "tagged-degraded";
export type TextConfidence = "exact" | "partial" | "none";
export type Alignment = "left" | "right" | "centre" | "justified" | "unknown";
export type BlockKind = "paragraph" | "table" | "image" | "vector" | "running" | "unknown";
export type SaveMode = "incremental" | "full-rewrite";

/** Spec 11.4's ladder, worst to best. */
export type Fidelity = "overlaid" | "substituted" | "reembedded" | "exact";

export interface DocumentInfo {
  pageCount: number;
  documentKind: DocumentKind;
  taggedStatus: TaggedStatus;
  /** The real content is an XFA payload the AcroForm shadows. Edits are refused. */
  hasXfa: boolean;
  encrypted: boolean;
  /** Revisions already in the file. Each one preserves what came before it. */
  revisionCount: number;
  /** Bytes held, for spec 12.5's budget. */
  memoryUsage: number;
  permissions: Permissions;
  /** Every specification deviation tolerated while reading. Empty when well-formed. */
  leniencies: readonly string[];
}

export interface Paragraph {
  /** Stable for the life of the `Page` that produced it. */
  readonly id: number;
  readonly text: string;
  /** How far to trust `text`. `'none'` means glyphs resolved to nothing. */
  readonly textConfidence: TextConfidence;
  readonly box: Rect;
  readonly alignment: Alignment;
  readonly leading: number;
  readonly lineCount: number;
}

export interface Block {
  readonly kind: BlockKind;
  readonly box: Rect;
}

export interface Image {
  /** Stable for the life of the `Page` that produced it. */
  readonly id: number;
  /** Where it sits on the page, after every transform. */
  readonly box: Rect;
  /** Pixel dimensions, when the image declares them. */
  readonly pixels: { readonly width: number; readonly height: number } | null;
  /**
   * Whether this layer can move, scale or delete it.
   *
   * False for an image drawn inside a form XObject: the drawing lives in the
   * form's own stream, and a form may be invoked from several pages, so moving
   * it would move it everywhere. Check this before offering a drag handle.
   */
  readonly editable: boolean;
}

export interface Table {
  readonly id: number;
  readonly box: Rect;
  readonly rows: number;
  readonly columns: number;
}

/** ISO 32000-1 §12.5.6's annotation subtypes. Spec 10.4. */
export type AnnotationKind =
  | "Text"
  | "Link"
  | "FreeText"
  | "Line"
  | "Square"
  | "Circle"
  | "Polygon"
  | "PolyLine"
  | "Highlight"
  | "Underline"
  | "Squiggly"
  | "StrikeOut"
  | "Stamp"
  | "Ink"
  | "Popup"
  | "FileAttachment"
  | "Widget";

/** An object's identity in the file. Opaque; pass it back as you got it. */
export interface ObjectId {
  readonly number: number;
  readonly generation: number;
}

export interface Annotation {
  readonly id: ObjectId;
  /** `null` for a `/Subtype` this library does not model. */
  readonly kind: AnnotationKind | null;
  readonly rect: Rect | null;
  /** `/Contents`, the human-readable text. */
  readonly contents: string | null;
  /**
   * Whether it carries its own appearance stream.
   *
   * False means the viewer draws it however it likes, which is why annotations
   * this library writes always carry one.
   */
  readonly hasAppearance: boolean;
}

export interface NewAnnotation {
  readonly kind: AnnotationKind;
  readonly rect: Rect;
  /** Stroke colour, RGB 0..1. Defaults to black. */
  readonly colour?: readonly [number, number, number];
  /** Alias of `colour`, for callers who spell it the other way. */
  readonly color?: readonly [number, number, number];
  /** Interior colour for `Square` and `Circle`; omitted leaves it unfilled. */
  readonly interior?: readonly [number, number, number];
  readonly borderWidth?: number;
  readonly contents?: string;
  /**
   * Flat `[x0, y0, x1, y1, ...]`. Two endpoints for `Line`; the quads for the
   * text-markup types.
   */
  readonly points?: readonly number[];
}

export type FieldKind = "button" | "text" | "choice" | "signature" | "unknown";

export interface FormField {
  readonly id: ObjectId;
  /**
   * The fully-qualified name: every ancestor's `/T` joined with `.`.
   *
   * ISO 32000-1 §12.7.3.2's definition of a field's identity, and what
   * `setFieldValue` expects. A partial name is only unique among siblings.
   */
  readonly name: string;
  readonly kind: FieldKind;
  readonly value: string | null;
  /** How many widget annotations draw it. */
  readonly widgets: number;
}

/** Spec 9.6's verification of a redaction that has already been saved. */
export interface RedactionReport {
  /**
   * No trace found **in the places checked**. Read `notChecked` before treating
   * this as "the text is not in this file".
   */
  readonly clean: boolean;
  readonly traces: readonly { readonly string: string; readonly whereFound: string }[];
  readonly objectsChecked: number;
  readonly streamsChecked: number;
  /** Places this check does not look. */
  readonly notChecked: readonly string[];
}

/** Reported by `protect()`. None is an error; all are worth knowing. */
export type Weakness =
  | "legacy-key-derivation"
  | "empty-user-password"
  | "owner-password-equals-user";

export interface ProtectOptions {
  /** Opens the document. Empty means it opens without one — legal, and reported. */
  userPassword?: string;
  /**
   * Lifts the advisory `/P` restrictions.
   *
   * **Empty means "the same as the user password", not "no owner password"** —
   * a reader is in if *either* entry is satisfied, so an empty owner password
   * would open a document that asks for one.
   */
  ownerPassword?: string;
  /** `/P`. Advisory (§5.5). Anything omitted is granted. */
  permissions?: Partial<Permissions>;
  /** `/EncryptMetadata`. `true` by default. */
  encryptMetadata?: boolean;
  /**
   * `'aes-256'` by default. `'aes-128'` exists for readers older than Acrobat 9
   * and reports `legacy-key-derivation`: the cipher is sound, the password
   * hashing is one MD5 plus fifty more.
   */
  strength?: "aes-256" | "aes-128";
  /**
   * 32 random bytes. Defaults to `crypto.getRandomValues` — supply your own
   * only if you have a better source.
   */
  entropy?: Uint8Array;
}

export interface FontRequirement {
  /** `/BaseFont`, subset prefix and all. */
  readonly pdfFont: string;
  /** The typeface name, with any six-letter subset prefix removed. */
  readonly family: string;
  readonly embedded: boolean;
  readonly subset: boolean;
  /** How much of Basic Latin this font can currently write. */
  readonly coverage: "full" | "partial" | "unknown";
  readonly writableLatin: number;
  /** Whether an application should offer to supply this font. Spec 11.3. */
  readonly needsSupplying: boolean;
}

export interface MetadataFields {
  readonly title: string | null;
  readonly author: string | null;
  readonly subject: string | null;
  readonly creator: string | null;
  readonly producer: string | null;
}

export interface MetadataDisagreement {
  readonly field: "title" | "author" | "subject" | "creator" | "producer";
  readonly info: string;
  readonly xmp: string;
}

/** Spec 10.3: both surfaces, and where they disagree. */
export interface Metadata {
  readonly info: MetadataFields;
  readonly xmp: MetadataFields;
  readonly hasXmp: boolean;
  readonly disagreements: readonly MetadataDisagreement[];
}

export interface SaveResult {
  readonly bytes: Uint8Array;
  readonly mode: SaveMode;
  /** Zero for a full rewrite, where the concept does not apply. */
  readonly bytesAppended: number;
  readonly warnings: readonly string[];
}

/** Spec 11.4. Degradation is normal and arrives here, not as an exception. */
export interface EditResult {
  readonly fidelity: Fidelity;
  /** Characters no font in the document could write. */
  readonly missingGlyphs: readonly string[];
  /** The paragraph's new line count, when re-breaking changed it. */
  readonly reflowedLines: number | null;
  readonly warnings: readonly string[];
}

/** What a session has staged. */
export interface SessionStatus {
  readonly staged: number;
  readonly undone: number;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  /** True once `commit()` or `rollback()` has run; further calls are refused. */
  readonly closed: boolean;
}

export interface SessionOptions {
  /**
   * Refuse any operation that cannot reach this rung. Spec 11.4.
   *
   * A contract-redlining tool sets `'exact'`; a form-filler accepts
   * `'substituted'`. Unset means accept whatever the operation achieves.
   */
  requireFidelity?: Fidelity;
  /** Spec 9.3. `'greedy'` by default, which is a fidelity decision. */
  lineBreaking?: "greedy" | "knuth-plass";
  /** What to do when reflowed text no longer fits its block. Spec 9.3. */
  overflow?: "refuse" | "allow" | "grow" | "shrink";
}

/**
 * An edit in progress. Spec 9.1, 11.4.
 *
 * Operations accumulate; nothing is written until `commit()`.
 */
export class Session {
  /** Stage a replacement. Nothing is written until `commit()`. */
  replaceText(
    paragraphId: number,
    range: { start: number; end: number },
    text: string,
    where?: { page?: number },
  ): Promise<EditResult>;
  insertText(
    paragraphId: number,
    at: number,
    text: string,
    where?: { page?: number },
  ): Promise<EditResult>;
  /**
   * Remove a character range from the page.
   *
   * Not redaction: the text may survive elsewhere in the file. Use
   * `Document.redact` when it has to be gone.
   */
  deleteRange(
    paragraphId: number,
    range: { start: number; end: number },
    where?: { page?: number },
  ): Promise<EditResult>;
  /** Move an image by a page-space offset. `imageId` comes from `page.images()`. */
  moveImage(
    imageId: number,
    by: { dx: number; dy: number },
    where?: { page?: number },
  ): Promise<EditResult>;
  scaleImage(
    imageId: number,
    by: { sx: number; sy: number },
    where?: { page?: number },
  ): Promise<EditResult>;
  /**
   * Remove an image from the page.
   *
   * The drawing operator goes; the XObject stays in the file. Save with
   * `{ fullRewrite: true }` when the pixels themselves must not survive.
   */
  deleteImage(imageId: number, where?: { page?: number }): Promise<EditResult>;
  /** Replace one cell's text. `tableId` comes from `page.tables()`. Spec 7.7. */
  setCell(
    tableId: number,
    cell: { row: number; column: number },
    text: string,
    where?: { page?: number },
  ): Promise<EditResult>;
  /** Remove a page. Indices of later pages shift; re-read them. */
  deletePage(index: number): Promise<EditResult>;
  movePage(from: number, to: number): Promise<EditResult>;
  annotations(where?: { page?: number }): Promise<readonly Annotation[]>;
  /** Add an annotation, with the appearance stream written rather than left to the viewer. */
  addAnnotation(spec: NewAnnotation, where?: { page?: number }): Promise<EditResult>;
  deleteAnnotation(id: ObjectId, where?: { page?: number }): Promise<EditResult>;
  /** Fill a field by its fully-qualified name, and regenerate its appearance. Spec 10.5. */
  setFieldValue(name: string, value: string): Promise<EditResult>;
  /** Burn widget appearances into the page and drop the fields. One-way. */
  flattenForms(where?: { page?: number }): Promise<EditResult>;
  /** Undo the last operation. Resolves to whether there was one. */
  undo(): Promise<boolean>;
  redo(): Promise<boolean>;
  status(): Promise<SessionStatus>;
  /** Undo everything and close the session. */
  rollback(): Promise<void>;
  /** Apply everything staged and write the document out. */
  commit(opts?: { fullRewrite?: boolean }): Promise<SaveResult>;
}

export class Page {
  readonly index: number;
  readonly mediaBox: Rect;
  /** Clockwise degrees: 0, 90, 180 or 270. */
  readonly rotate: number;
  /** A picture of a page. Spec 3: no OCR is performed. */
  readonly scanned: boolean;
  paragraphs(): readonly Paragraph[];
  blocks(): readonly Block[];
  images(): readonly Image[];
  tables(): readonly Table[];
  textContent(): string;
  /** The smallest paragraph containing the point, so a footnote wins over its column. */
  paragraphAt(point: Point): Paragraph | null;
}

export class Document {
  /** Spec 11.2's readable properties, in one round trip. */
  info(): Promise<DocumentInfo>;
  page(index: number): Promise<Page>;
  /** Spec 11.3. Call it right after `open` and fetch what the document needs. */
  fontRequirements(): Promise<readonly FontRequirement[]>;
  /**
   * Supply a font the document does not have. Spec 11.3.
   *
   * Held until an edit needs a character the document's own embedded font
   * cannot draw, at which point the outline is injected *into* that font — so
   * the page keeps one typeface and the edit reports `reembedded` rather than
   * a substitution a reader can see. Resolves to how many fonts are registered.
   */
  registerFont(
    bytes: ArrayBuffer | Uint8Array,
    opts?: {
      /** Bind this font to one of the document's, by `/BaseFont` family. */
      matchFor?: string;
    },
  ): Promise<number>;
  metadata(): Promise<Metadata>;
  /** Every form field, by fully-qualified name. Spec 10.5. */
  formFields(): Promise<readonly FormField[]>;
  /**
   * Remove every trace of a string from the document. Spec 9.6.
   *
   * Not a session operation and not undoable: it forces a full rewrite, and an
   * undo stack that kept the bytes for you would defeat the purpose. Resolves
   * to the strings actually found.
   */
  redact(text: string): Promise<readonly string[]>;
  /**
   * Search saved bytes for text that should no longer be there. Spec 9.6.
   *
   * Takes the bytes commit() or save() returned rather than reading this
   * document: asking the thing that performed the redaction whether it worked
   * is not a check.
   */
  verifyRedaction(bytes: Uint8Array, strings: readonly string[]): Promise<RedactionReport>;
  /** Encrypt the document, or change its password. Resolves to the policy's weaknesses. Spec 5. */
  protect(opts?: ProtectOptions): Promise<readonly Weakness[]>;
  /** Remove the document's encryption. Spec 5. */
  unprotect(): Promise<void>;
  /**
   * Drop unused glyphs from every embedded font. Spec 8.6.
   *
   * After editing, not before: a font reduced to exactly the glyphs in use has
   * nothing spare for the next insertion.
   */
  compactFonts(): Promise<number>;
  edit(opts?: SessionOptions): Session;
  save(opts?: { fullRewrite?: boolean }): Promise<SaveResult>;
  /** Release the module's memory. Spec 12.5: not optional. */
  close(): Promise<boolean>;
}

export class Pdf {
  static open(src: ArrayBuffer | Uint8Array | Blob, opts?: OpenOptions): Promise<Document>;

  /**
   * Compose a document that did not exist. Spec 11's `create`.
   *
   * The typeface is required and is embedded, subset to the characters used.
   * Returns the document together with what composing approximated ‒ read
   * `report.missing` before shipping the result.
   */
  static create(
    content: readonly Content[],
    font: ArrayBuffer | Uint8Array | Blob,
    opts?: CreateOptions,
  ): Promise<Composed>;
}
