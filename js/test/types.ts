// The declarations, compiled. Spec 12.4: "hand-checked, no `any` in the public
// surface."
//
// Not a runtime test — `tsc --noEmit` is the whole check. It exists because
// hand-written declarations that nothing compiles are a second source of truth
// nobody has read, and they drift from the implementation silently.
//
// `noImplicitAny` and `strict` are on in `tsconfig.json`, so a declaration that
// resolved to `any` would let the deliberate mistakes at the bottom of this
// file pass, and the file would stop being a check.

import { Pdf, PdfError } from "rasura";
import type {
  Alignment,
  Annotation,
  AnnotationKind,
  Block,
  DocumentInfo,
  EditResult,
  Fidelity,
  FieldKind,
  FontRequirement,
  FormField,
  Image,
  Metadata,
  ObjectId,
  Page,
  Paragraph,
  PdfErrorCode,
  RedactionReport,
  SaveResult,
  Table,
  Weakness,
} from "rasura";

export async function readAndEdit(bytes: Uint8Array): Promise<Uint8Array> {
  const doc = await Pdf.open(bytes, {
    password: "",
    recovery: "auto",
    worker: true,
    transfer: false,
    wasmUrl: new URL("https://cdn.example/rasura.wasm"),
  });

  const info: DocumentInfo = await doc.info();
  // Narrowing works, which is the point of a union over a string.
  if (info.documentKind === "scanned") {
    throw new PdfError("scanned-no-text", "nothing to edit here");
  }
  const canPrint: boolean = info.permissions.print;
  const notes: readonly string[] = info.leniencies;
  void canPrint;
  void notes;

  const fonts: readonly FontRequirement[] = await doc.fontRequirements();
  const missing: readonly string[] = fonts.filter((f) => f.needsSupplying).map((f) => f.family);
  void missing;

  const registered: number = await doc.registerFont(new Uint8Array(), { matchFor: "MinionPro" });
  void registered;

  const meta: Metadata = await doc.metadata();
  for (const clash of meta.disagreements) {
    // The field name is a union, not a bare string.
    const field: "title" | "author" | "subject" | "creator" | "producer" = clash.field;
    void field;
  }

  const page: Page = await doc.page(0);
  const paragraphs: readonly Paragraph[] = page.paragraphs();
  const blocks: readonly Block[] = page.blocks();
  void blocks;

  const hit: Paragraph | null = page.paragraphAt({ x: 100, y: 700 });
  const alignment: Alignment = paragraphs[0].alignment;
  void hit;
  void alignment;

  const floor: Fidelity = "exact";
  const session = doc.edit({ requireFidelity: floor });

  try {
    const result: EditResult = await session.replaceText(
      paragraphs[0].id,
      { start: 0, end: 5 },
      "Q4 net revenue",
    );
    if (result.fidelity !== "exact") {
      const lines: number | null = result.reflowedLines;
      const missing: readonly string[] = result.missingGlyphs;
      void lines;
      void missing;
    }
    // Staged, not written: the bytes arrive from the commit, not the edit.
    const out: SaveResult = await session.commit();
    return out.bytes;
  } catch (e) {
    // Spec 11.5: every failure is coded and actionable.
    if (e instanceof PdfError) {
      const code: PdfErrorCode = e.code;
      const detail: unknown = e.detail;
      void detail;
      if (code === "fidelity-below-required") {
        const saved: SaveResult = await doc.save({ fullRewrite: true });
        return saved.bytes;
      }
    }
    throw e;
  } finally {
    await doc.close();
  }
}

// --- The declarations must also *reject* these. -----------------------------
//
// Each line is a mistake a caller could make, and each `@ts-expect-error`
// fails the build if the mistake stops being one — which is what catches a
// return type quietly widening to `any`.

export async function mistakes(bytes: Uint8Array): Promise<void> {
  const doc = await Pdf.open(bytes);

  // @ts-expect-error a typo'd code is not a PdfErrorCode
  const bad: PdfErrorCode = "encrypted-password-requird";
  void bad;

  // @ts-expect-error `documentKind` has three values and this is not one
  const kind: DocumentInfo["documentKind"] = "digital";
  void kind;

  // @ts-expect-error `page()` needs an index
  await doc.page();

  // @ts-expect-error paragraphs are read-only; a caller cannot push into them
  (await doc.page(0)).paragraphs().push({} as Paragraph);

  // @ts-expect-error `requireFidelity` takes a rung, not any string
  doc.edit({ requireFidelity: "perfect" });

  // @ts-expect-error a session stages; it has no `bytes()` accessor any more
  doc.edit().bytes();

  // @ts-expect-error `lineBreaking` is a union, not a free string
  doc.edit({ lineBreaking: "optimal" });

  // @ts-expect-error `info()` is async; there is no synchronous property
  const count: number = doc.pageCount;
  void count;

  // @ts-expect-error an annotation kind is a `/Subtype`, not any string
  doc.edit().addAnnotation({ kind: "Sqaure", rect: { x0: 0, y0: 0, x1: 1, y1: 1 } });

  // @ts-expect-error a rect is required; a kind alone does not place anything
  doc.edit().addAnnotation({ kind: "Square" });

  // @ts-expect-error `moveImage` takes a named offset, not two loose numbers
  doc.edit().moveImage(0, 10, 20);

  // @ts-expect-error `strength` is a union of the two this library writes
  doc.protect({ strength: "rc4-128" });

  // @ts-expect-error RC4 and /R 5 are read for legacy files and never written
  doc.protect({ strength: "rc4-40" });

  await doc.close();
}

/** The rest of the catalogue, typed. Spec 9, 10, 11.2. */
export async function editEverything(bytes: Uint8Array): Promise<Uint8Array> {
  const doc = await Pdf.open(bytes);
  const page = await doc.page(0);

  const images: readonly Image[] = page.images();
  const tables: readonly Table[] = page.tables();
  // `pixels` is nullable because a malformed image may not declare its size.
  const size: { readonly width: number; readonly height: number } | null = images[0].pixels;
  void size;

  const fields: readonly FormField[] = await doc.formFields();
  const kind: FieldKind = fields[0].kind;
  void kind;

  const session = doc.edit({ overflow: "shrink", lineBreaking: "knuth-plass" });

  const moved: EditResult = await session.moveImage(images[0].id, { dx: 12, dy: -4 });
  const scaled: EditResult = await session.scaleImage(images[0].id, { sx: 1.5, sy: 1.5 });
  const cell: EditResult = await session.setCell(tables[0].id, { row: 1, column: 2 }, "48.2");
  const filled: EditResult = await session.setFieldValue(fields[0].name, "A. Ozdamar");
  void moved;
  void scaled;
  void cell;
  void filled;

  const added: EditResult = await session.addAnnotation(
    {
      kind: "Highlight",
      rect: { x0: 72, y0: 690, x1: 300, y1: 712 },
      colour: [1, 0.9, 0.2],
      contents: "check this figure",
      points: [72, 690, 300, 690, 72, 712, 300, 712],
    },
    { page: 0 },
  );
  void added;

  const listed: readonly Annotation[] = await session.annotations({ page: 0 });
  // Nullable: a `/Subtype` this library does not model still has to be listed,
  // because an annotation it cannot name is one a caller must still be able to
  // see and delete.
  const subtype: AnnotationKind | null = listed[0].kind;
  const id: ObjectId = listed[0].id;
  void subtype;
  await session.deleteAnnotation(id);

  await session.movePage(1, 0);
  await session.deletePage(2);
  await session.flattenForms({ page: 0 });

  const out: SaveResult = await session.commit();

  const found: readonly string[] = await doc.redact("account 4021-8890");
  const report: RedactionReport = await doc.verifyRedaction(out.bytes, found);
  if (!report.clean) {
    const where: string = report.traces[0].whereFound;
    void where;
  }
  // Not the same claim as "clean": these are the places the check does not look.
  const blind: readonly string[] = report.notChecked;
  void blind;

  const weaknesses: readonly Weakness[] = await doc.protect({
    userPassword: "hunter2",
    ownerPassword: "s3kr1t",
    permissions: { print: true, copy: false },
    strength: "aes-256",
  });
  void weaknesses;
  await doc.unprotect();

  const compacted: number = await doc.compactFonts();
  void compacted;

  const saved: SaveResult = await doc.save({ fullRewrite: true });
  await doc.close();
  return saved.bytes;
}
