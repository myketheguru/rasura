/**
 * Loading the WebAssembly module, and the shape of what it returns.
 *
 * The editor talks to the raw WASM surface rather than to the npm package. The
 * package starts a Worker and owns the transport, which is right for an
 * application and unnecessary here: this page is static files on the same
 * origin, and the whole transport would be a function call.
 *
 * The module is copied into `public/wasm/` by the build, so it is a plain fetch
 * from the site's own base — no CDN, no external origin, and nothing that a
 * content-security policy has to be widened for.
 */
import type { PageModel } from './model'

export interface DocumentInfo {
  pageCount: number
  documentKind: string
  taggedStatus: string
  hasXfa: boolean
  encrypted: boolean
  revisionCount: number
  memoryUsage: number
  permissions: Record<string, boolean>
  leniencies: { kind: string; detail: string }[]
}

export interface Outcome {
  fidelity: 'exact' | 'reembedded' | 'substituted' | 'overlaid'
  compromises?: string[]
}

export interface Saved {
  bytes: Uint8Array
  mode: string
  bytesAppended: number
}

export interface FontInfo {
  pdfFont: string
  embedded: boolean
  subset: boolean
  coverage: string
  needsSupplying: boolean
}

export interface FieldInfo {
  name: string
  kind: string
  value: string
  readOnly: boolean
}

/** Only what the editor calls. Hand-written, because a generated `.d.ts` from
 * wasm-bindgen describes the ABI rather than the API. */
export interface Wasm {
  version(): string
  isThreaded(): boolean
  openDocument(bytes: Uint8Array, password?: string, recovery?: string): number
  closeDocument(handle: number): boolean
  documentInfo(handle: number): DocumentInfo
  documentMetadata(handle: number): unknown
  pageContent(handle: number, page: number): PageModel
  fontRequirements(handle: number): FontInfo[]
  formFields(handle: number): FieldInfo[]
  replaceText(
    handle: number,
    page: number,
    region: number,
    index: number,
    from: number,
    to: number,
    text: string,
  ): Outcome
  saveDocument(handle: number, options?: unknown): Saved
  commitSession(handle: number, options?: unknown): Saved
  sessionStatus(handle: number): { staged: number; canUndo: boolean; canRedo: boolean }
  configureSession(handle: number, options: unknown): void
  undo(handle: number): unknown
  redo(handle: number): unknown
  rollbackSession(handle: number): unknown
  addAnnotation(handle: number, page: number, spec: unknown): Outcome
  deletePage(handle: number, page: number): Outcome
  deleteImage(handle: number, page: number, id: unknown): Outcome
  moveImage(handle: number, page: number, id: unknown, dx: number, dy: number): Outcome
  compactFonts(handle: number): number
  redactText(handle: number, text: string): Outcome
  verifyRedaction(handle: number, text: string): { clean: boolean; notChecked: string[] }
  registerFont(bytes: Uint8Array, options?: unknown): number
}

let cached: Promise<Wasm> | null = null

/**
 * The module, loaded once.
 *
 * `import.meta.env.BASE_URL` rather than an absolute path: Pages serves this
 * from a subdirectory, and `/wasm/...` would 404 there while working perfectly
 * in local preview — the classic way to ship a broken deploy.
 */
export function load(): Promise<Wasm> {
  cached ??= (async () => {
    const base = import.meta.env.BASE_URL
    const glue = await import(/* @vite-ignore */ `${base}wasm/rasura_wasm.js`)
    await glue.default({ module_or_path: `${base}wasm/rasura_wasm_bg.wasm` })
    return glue as unknown as Wasm
  })()
  return cached
}

/** Normalise anything the module throws into something with a code. */
export function coded(e: unknown): { code: string; message: string } {
  if (e && typeof e === 'object' && 'code' in e) {
    const withCode = e as { code: unknown; message?: unknown }
    return { code: String(withCode.code), message: String(withCode.message ?? e) }
  }
  return { code: 'internal', message: e instanceof Error ? e.message : String(e) }
}
