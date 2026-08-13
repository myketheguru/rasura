// One error class, always coded. Spec 11.5.
//
//   class PdfError extends Error { code: PdfErrorCode; detail: unknown; }
//
// > Never throw a bare `Error`. Every failure is coded and actionable.

/** Every code spec 11.5 defines. Frozen so a typo in a comparison is findable. */
export const CODES = Object.freeze([
  "malformed",
  "encrypted-password-required",
  "encrypted-unsupported",
  "scanned-no-text",
  "xfa-unsupported",
  "type3-glyph-missing",
  "font-unavailable",
  "overflow",
  "stale-session",
  "fidelity-below-required",
  "signature-would-be-destroyed",
  "unsupported-filter",
  "invalid-argument",
  "internal",
]);

export class PdfError extends Error {
  /**
   * @param {string} code
   * @param {string} message
   * @param {unknown} [detail]
   */
  constructor(code, message, detail) {
    super(message);
    this.name = "PdfError";
    this.code = code;
    this.detail = detail;
  }
}

// Structured clone does **not** carry an Error's own properties.
//
// This is the trap the Worker boundary sets, and it is silent: throw a
// `PdfError` from inside a Worker and what arrives on the other side is an
// `Error` with the right message, the right stack, and `code === undefined`.
// Every `if (e.code === 'encrypted-password-required')` a caller wrote then
// takes the wrong branch, and nothing anywhere reports a failure.
//
// So errors are never thrown across the boundary. They are converted to plain
// objects by `toWire`, sent as ordinary data, and rebuilt by `fromWire`.

/**
 * An error as plain data, safe to `postMessage`.
 * @param {unknown} e
 */
export function toWire(e) {
  if (e instanceof PdfError) {
    return { code: e.code, message: e.message, detail: e.detail ?? null };
  }
  // A `code` property on a plain Error: what the WASM surface throws, since
  // wasm-bindgen cannot construct a subclass without a JS shim.
  if (e && typeof e === "object" && typeof (/** @type {any} */ (e).code) === "string") {
    const any = /** @type {any} */ (e);
    return { code: any.code, message: any.message ?? String(e), detail: any.detail ?? null };
  }
  if (e instanceof Error) {
    // Something genuinely unexpected — a bug here, not a bad document. It still
    // gets a code, because a caller must never have to handle an uncoded throw.
    return { code: "internal", message: e.message, detail: e.stack ?? null };
  }
  return { code: "internal", message: String(e), detail: null };
}

/**
 * Rebuild an error from `toWire`'s output.
 * @param {{ code: string, message: string, detail: unknown }} wire
 */
export function fromWire(wire) {
  return new PdfError(wire.code, wire.message, wire.detail ?? undefined);
}

/**
 * Normalise anything thrown by the WASM surface into a `PdfError`.
 * @param {unknown} e
 */
export function normalise(e) {
  return e instanceof PdfError ? e : fromWire(toWire(e));
}
