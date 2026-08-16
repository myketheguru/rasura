import * as React from 'react'
import {
  ChevronLeft,
  ChevronRight,
  FileText,
  Flag,
  Info,
  Layers,
  Redo2,
  Shrink,
  Trash2,
  Undo2,
  X,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Badge,
  Card,
  CardBody,
  CardHeader,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Separator,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Tooltip,
} from '@/components/ui/primitives'
import { bytes as fmtBytes, cn } from '@/lib/utils'
import * as R from '@/editor/rasura'
import {
  drawList,
  imageAt,
  minimalRange,
  pageBox,
  paragraphAt,
  type PageModel,
  type Paragraph,
} from '@/editor/model'

type Rung = 'exact' | 'reembedded' | 'substituted' | 'overlaid' | 'refused'

interface LogEntry {
  id: number
  what: string
  rung: Rung
  detail: string
  at: string
}

let logSeq = 0

export default function Editor() {
  const [wasm, setWasm] = React.useState<R.Wasm | null>(null)
  const [fatal, setFatal] = React.useState<string | null>(null)
  const scrollRef = React.useRef<HTMLDivElement>(null)
  /** Width available to the sheet, so it can be fitted rather than clipped. */
  const [avail, setAvail] = React.useState(0)
  /** The inspector, which is a bottom sheet on a phone and a rail above it. */
  const [details, setDetails] = React.useState(false)
  const pageRefs = React.useRef<(HTMLDivElement | null)[]>([])
  const canvasRefs = React.useRef<(HTMLCanvasElement | null)[]>([])
  const [handle, setHandle] = React.useState<number | null>(null)
  const [fileName, setFileName] = React.useState('sample.pdf')
  const [info, setInfo] = React.useState<R.DocumentInfo | null>(null)
  const [pages, setPages] = React.useState<(PageModel | null)[]>([])
  const [pageIndex, setPageIndex] = React.useState(0)
  const page = pages[pageIndex] ?? null
  const [selected, setSelected] = React.useState<{ page: number; paragraph: Paragraph } | null>(null)
  const [floor, setFloor] = React.useState('overlaid')
  const [session, setSession] = React.useState({ staged: 0, canUndo: false, canRedo: false })
  const [log, setLog] = React.useState<LogEntry[]>([])
  const [status, setStatus] = React.useState<{ text: string; state: string }>({
    text: 'ready',
    state: 'idle',
  })
  const [saved, setSaved] = React.useState<R.Saved | null>(null)
  const [editing, setEditing] = React.useState<{ paragraph: Paragraph; text: string } | null>(null)
  const [redacting, setRedacting] = React.useState<string | null>(null)
  const [notice, setNotice] = React.useState(true)

  const note = React.useCallback((what: string, rung: Rung, detail = '') => {
    setLog((l) => [
      ...l,
      {
        id: (logSeq += 1),
        what,
        rung,
        detail,
        at: new Date().toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' }),
      },
    ])
  }, [])

  const fail = React.useCallback(
    (e: unknown, what: string) => {
      const { code, message } = R.coded(e)
      note(what, 'refused', `${code}: ${message}`)
      setStatus({ text: `${what} refused: ${code}`, state: 'error' })
    },
    [note],
  )

  // --- boot ----------------------------------------------------------------

  React.useEffect(() => {
    let cancelled = false
    R.load()
      .then(async (m) => {
        if (cancelled) return
        setWasm(m)
        const base = import.meta.env.BASE_URL
        const res = await fetch(`${base}sample.pdf`)
        const buf = new Uint8Array(await res.arrayBuffer())
        const h = m.openDocument(buf, undefined, undefined)
        setHandle(h)
        setInfo(m.documentInfo(h))
        setPages(readPages(m, h))
        setStatus({ text: 'opened sample.pdf', state: 'ok' })
      })
      .catch((e) => {
        // Stated, not diagnosed. The cause is usually a MIME type, a
        // content-security policy without wasm-unsafe-eval, or the .wasm not
        // sitting beside its glue — and asserting one of the three when it is
        // another sends the reader somewhere the fault is not.
        setFatal(R.coded(e).message)
      })
    return () => {
      cancelled = true
    }
  }, [])

  /** Read every page's model. Cheap enough for a demo, and the stage shows them all. */
  const readPages = React.useCallback((m: R.Wasm, h: number) => {
    const count = m.documentInfo(h).pageCount
    const out: (PageModel | null)[] = []
    for (let i = 0; i < count; i += 1) {
      try {
        out.push(m.pageContent(h, i))
      } catch {
        // A page whose content stream will not decode still occupies a slot, or
        // every page after it would be numbered wrongly.
        out.push(null)
      }
    }
    return out
  }, [])

  const refresh = React.useCallback(
    (m: R.Wasm, h: number) => {
      setPages(readPages(m, h))
      setInfo(m.documentInfo(h))
      setSession(m.sessionStatus(h))
    },
    [readPages],
  )

  // --- drawing -------------------------------------------------------------

  const drawPage = React.useCallback((canvas: HTMLCanvasElement | null, page: PageModel | null, highlight: Paragraph | null, avail: number) => {
    if (!canvas || !page) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    const selected = highlight

    const { width, height } = pageBox(page)
    const dpr = window.devicePixelRatio || 1
    // Fit the height, and the *width* of whatever space there is.
    //
    // This fitted the height alone, which is fine on a desktop and clips the
    // page on a phone: an A4 sheet at 720px tall is about 509px wide, so on a
    // 390px screen a centred page lost roughly 60px off each edge and the
    // first characters of every line were simply not on screen. Nothing
    // reported it, because the canvas was drawn correctly — it was the sheet
    // that did not fit.
    const scale = Math.min(1, 720 / height, avail > 0 ? avail / width : Infinity)
    canvas.width = Math.round(width * scale * dpr)
    canvas.height = Math.round(height * scale * dpr)
    canvas.style.width = `${Math.round(width * scale)}px`
    canvas.style.height = `${Math.round(height * scale)}px`
    ctx.setTransform(dpr * scale, 0, 0, dpr * scale, 0, 0)

    // The sheet is white in both themes, because it is a sheet of paper — so
    // the ink on it is fixed too. Taking these from the theme drew near-white
    // text on white paper the moment the page was in dark mode, which made the
    // document unreadable while every control around it looked correct.
    const ink = '165 25% 12%'
    const muted = '160 6% 45%'
    const accent = getComputedStyle(document.documentElement)
      .getPropertyValue('--primary')
      .trim()

    ctx.fillStyle = '#fff'
    ctx.fillRect(0, 0, width, height)

    const measure = (text: string, size: number) => {
      ctx.font = `${size}px ui-sans-serif, system-ui, sans-serif`
      return ctx.measureText(text).width
    }

    for (const item of drawList(page, measure)) {
      const b = item.box
      const w = b.x1 - b.x0
      const h = b.y1 - b.y0

      if (item.type === 'block' || item.type === 'table' || item.type === 'image') {
        ctx.strokeStyle = `hsl(${muted} / 0.4)`
        ctx.fillStyle = `hsl(${muted} / 0.07)`
        ctx.lineWidth = 1
        ctx.setLineDash(item.type === 'image' ? [] : [4, 3])
        ctx.fillRect(b.x0, b.y0, w, h)
        ctx.strokeRect(b.x0 + 0.5, b.y0 + 0.5, w - 1, h - 1)
        ctx.setLineDash([])

        ctx.fillStyle = `hsl(${muted})`
        ctx.font = '9px ui-sans-serif, system-ui, sans-serif'
        const label =
          item.type === 'table'
            ? `table ${item.rows}×${item.columns}`
            : item.type === 'image'
              ? 'image'
              : item.kind
        ctx.fillText(label, b.x0 + 3, b.y0 + 11)
        continue
      }

      const l = item.layout
      ctx.fillStyle = item.confidence === 'exact' ? `hsl(${ink})` : `hsl(${muted})`
      ctx.font = `${l.size}px ui-sans-serif, system-ui, sans-serif`
      l.lines.forEach((line, i) => {
        const y = l.top + l.size + i * l.leading
        if (y > l.top + l.height + l.leading) return
        let x = l.left
        if (l.alignment === 'centre' || l.alignment === 'center') {
          x = l.left + (l.width - measure(line, l.size)) / 2
        } else if (l.alignment === 'right') {
          x = l.left + l.width - measure(line, l.size)
        }
        ctx.fillText(line, x, y)
      })
    }

    if (selected) {
      const b = selected.box
      ctx.strokeStyle = `hsl(${accent})`
      ctx.lineWidth = 1.5
      ctx.strokeRect(b.x0 - 1, b.y0 - 1, b.x1 - b.x0 + 2, b.y1 - b.y0 + 2)
      ctx.fillStyle = `hsl(${accent} / 0.08)`
      ctx.fillRect(b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0)
    }
  }, [])

  // Every page is drawn, not just the current one, because every page is on
  // screen: the stage scrolls continuously and page two is directly under page
  // one rather than behind a button.
  React.useEffect(() => {
    // Drawn on the page it was made on, not on whichever page is in view.
    // Keying it to pageIndex meant scrolling moved the selection box onto the
    // next page, over a paragraph nobody had chosen.
    pages.forEach((p, i) =>
      drawPage(canvasRefs.current[i], p, selected?.page === i ? selected.paragraph : null, avail),
    )
  }, [pages, selected, drawPage, avail])

  // How much width the sheet may use, watched rather than measured once.
  //
  // Nothing here reacted to size at all, so a page drawn at one width kept it
  // through a rotation or a window resize. The padding is subtracted here
  // because the canvas is sized in the same units the container pads in.
  React.useEffect(() => {
    const root = scrollRef.current
    if (!root) return
    const measure = () => {
      const pad = window.innerWidth < 640 ? 16 : 48 // p-2 on small, p-6 above
      setAvail(Math.max(0, root.clientWidth - pad))
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(root)
    return () => observer.disconnect()
  }, [])

  // Which page is being read, from what is actually in view. The one covering
  // the middle of the viewport wins; using the topmost visible page makes the
  // label flick to the next page as soon as a sliver of it appears.
  React.useEffect(() => {
    const root = scrollRef.current
    if (!root || pages.length < 2) return
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            const i = pageRefs.current.indexOf(e.target as HTMLDivElement)
            if (i >= 0) setPageIndex(i)
          }
        }
      },
      { root, rootMargin: '-45% 0px -45% 0px' },
    )
    for (const el of pageRefs.current) if (el) observer.observe(el)
    return () => observer.disconnect()
  }, [pages.length])

  const toModel = (e: React.MouseEvent<HTMLCanvasElement>, index: number) => {
    const model = pages[index]
    if (!model) return null
    const rect = e.currentTarget.getBoundingClientRect()
    const { width, height } = pageBox(model)
    return {
      x: ((e.clientX - rect.left) / rect.width) * width,
      y: ((e.clientY - rect.top) / rect.height) * height,
      model,
    }
  }

  // --- operations ----------------------------------------------------------

  const run = <T,>(what: string, fn: (m: R.Wasm, h: number) => T): T | undefined => {
    if (!wasm || handle === null) return undefined
    try {
      const out = fn(wasm, handle)
      refresh(wasm, handle)
      return out
    } catch (e) {
      fail(e, what)
      return undefined
    }
  }

  const applyEdit = () => {
    if (!editing || !wasm || handle === null) return
    const { paragraph, text } = editing
    setEditing(null)
    if (text === paragraph.text) return
    const range = minimalRange(paragraph.text, text)
    const out = run('Replace text', (m, h) =>
      m.replaceText(h, pageIndex, paragraph.id.region, paragraph.id.index, range.start, range.end, range.text),
    )
    if (out) {
      note('Replace text', out.fidelity, `${range.end - range.start} → ${range.text.length} characters`)
      setStatus({ text: `text replaced, ${out.fidelity}`, state: 'ok' })
      setSelected(null)
    }
  }

  const commit = () => {
    const out = run('Commit', (m, h) => m.commitSession(h, undefined))
    if (out) {
      setSaved(out)
      note('Commit', 'exact', `${out.mode}, ${fmtBytes(out.bytes.length)}`)
      setStatus({ text: `committed, ${out.mode}`, state: 'ok' })
    }
  }

  const save = () => {
    const out = run('Save', (m, h) => m.saveDocument(h, undefined))
    if (out) {
      setSaved(out)
      const blob = new Blob([out.bytes as BlobPart], { type: 'application/pdf' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = fileName
      a.click()
      URL.revokeObjectURL(url)
      note('Save', 'exact', `${out.mode}, ${fmtBytes(out.bytes.length)}`)
      setStatus({ text: `saved, ${out.mode}`, state: 'ok' })
    }
  }

  const openFile = async (file: File) => {
    if (!wasm) return
    try {
      const buf = new Uint8Array(await file.arrayBuffer())
      if (handle !== null) wasm.closeDocument(handle)
      const h = wasm.openDocument(buf, undefined, undefined)
      setHandle(h)
      setFileName(file.name)
      setPageIndex(0)
      setSelected(null)
      setSaved(null)
      setLog([])
      setPages(readPages(wasm, h))
      setInfo(wasm.documentInfo(h))
      setSession(wasm.sessionStatus(h))
      setStatus({ text: `opened ${file.name}`, state: 'ok' })
    } catch (e) {
      fail(e, 'Open')
    }
  }

  /** Scroll to a page rather than swapping which one is shown. */
  const goto = (index: number) => {
    const next = Math.max(0, Math.min(pages.length - 1, index))
    setSelected(null)
    pageRefs.current[next]?.scrollIntoView({ behavior: 'smooth', block: 'start' })
    setPageIndex(next)
  }

  if (fatal) {
    return (
      <main className="mx-auto max-w-2xl px-6 py-24">
        <h1 className="text-xl font-semibold">WebAssembly could not start</h1>
        <p className="mt-3 text-muted-foreground">
          The module did not compile. Usually one of: the host serves <code>.wasm</code> as
          something other than <code>application/wasm</code>, a content-security policy is
          missing <code>wasm-unsafe-eval</code>, or the module is not beside its glue. The
          error itself says which:
        </p>
        <pre className="mt-4 overflow-x-auto rounded-lg border border-border bg-muted p-3 text-xs">
          {fatal}
        </pre>
      </main>
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* --- tool bar ---
          On a phone this keeps the file name and the actions that commit, and
          everything else moves to the sheet behind "Tools". A row of twelve
          controls in a horizontal scroller is a desktop toolbar that has been
          made narrower, not something anyone would design for a thumb. */}
      <div className="flex h-12 shrink-0 items-center gap-1.5 border-b border-border bg-card px-2 sm:px-3 lg:overflow-x-auto">
        <div className="flex min-w-0 items-center gap-1.5 rounded-md border border-border px-2 py-1">
          <FileText className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="max-w-24 truncate text-[12.5px] sm:max-w-40">{fileName}</span>
        </div>

        <Separator orientation="vertical" className="hidden lg:block" />

        <div className="hidden items-center gap-1.5 lg:flex">
          <Button variant="ghost" size="icon-sm" onClick={() => goto(pageIndex - 1)} aria-label="Previous page">
            <ChevronLeft />
          </Button>
          <span
            data-testid="page-label"
            className="min-w-14 text-center text-[12.5px] tabular-nums text-muted-foreground"
          >
            {info ? `${pageIndex + 1} / ${info.pageCount}` : '–'}
          </span>
          <Button variant="ghost" size="icon-sm" onClick={() => goto(pageIndex + 1)} aria-label="Next page">
            <ChevronRight />
          </Button>
        </div>

        <Separator orientation="vertical" className="hidden lg:block" />

        <div className="hidden items-center gap-1.5 lg:flex">
        <span className="text-xs font-medium text-muted-foreground">Require</span>
        <Select
          value={floor}
          onValueChange={(v) => {
            setFloor(v)
            run('Configure', (m, h) => m.configureSession(h, { requireFidelity: v }))
          }}
        >
          <SelectTrigger className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="overlaid">any fidelity</SelectItem>
            <SelectItem value="substituted">substituted</SelectItem>
            <SelectItem value="reembedded">re-embedded</SelectItem>
            <SelectItem value="exact">exact</SelectItem>
          </SelectContent>
        </Select>

        <Separator orientation="vertical" />

        <Tooltip label="Add a Square annotation over the selection">
          <Button
            variant="ghost"
            size="sm"
            disabled={!selected}
            onClick={() => {
              if (!selected) return
              const out = run('Annotate', (m, h) =>
                m.addAnnotation(h, pageIndex, {
                  kind: 'Square',
                  rect: selected.paragraph.box,
                  colour: [0.85, 0.2, 0.2],
                  borderWidth: 1.5,
                  contents: 'Flagged in Rasura Studio',
                }),
              )
              if (out) note('Annotate', out.fidelity, 'Square over the selection')
            }}
          >
            <Flag /> Flag
          </Button>
        </Tooltip>

        <Tooltip label="Remove this page and retarget every link to it">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              const out = run('Delete page', (m, h) => m.deletePage(h, pageIndex))
              if (out) {
                note('Delete page', out.fidelity, `page ${pageIndex + 1}`)
                goto(Math.max(0, pageIndex - 1))
              }
            }}
          >
            <Trash2 /> Page
          </Button>
        </Tooltip>

        <Tooltip label="Drop glyphs no page uses from every embedded font">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              const n = run('Compact fonts', (m, h) => m.compactFonts(h))
              if (n !== undefined) note('Compact fonts', 'exact', `${n} font(s) touched`)
            }}
          >
            <Shrink /> Compact
          </Button>
        </Tooltip>

        <Button variant="ghost" size="sm" className="text-destructive" onClick={() => setRedacting('')}>
          Redact…
        </Button>
        </div>

        <div className="flex-1" />

        <label>
          <Button variant="outline" size="sm" asChild>
            <span className="whitespace-nowrap">Open…</span>
          </Button>
          <input
            type="file"
            accept="application/pdf"
            hidden
            onChange={(e) => e.target.files?.[0] && openFile(e.target.files[0])}
          />
        </label>
        <Button variant="outline" size="sm" className="hidden sm:inline-flex" onClick={save}>
          Save
        </Button>
        <Button size="sm" disabled={session.staged === 0} onClick={commit}>
          Commit
        </Button>
      </div>

      {/* --- notice --- */}
      {notice && (
        <div className="flex shrink-0 items-start gap-2 border-b border-border bg-info/6 px-3 py-2 text-[12.5px]">
          <Info className="mt-0.5 size-4 shrink-0 text-info" />
          <p className="flex-1 text-muted-foreground">
            <strong className="text-foreground">This is a model view, not a raster.</strong>{' '}
            Rasura has no renderer and will not grow one. Everything here is drawn from
            Rasura's own model, so every pixel came from the library rather than from a
            second one.
          </p>
          <Button variant="ghost" size="icon-sm" onClick={() => setNotice(false)} aria-label="Dismiss">
            <X />
          </Button>
        </div>
      )}

      {/* --- workspace --- */}
      <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[1fr_320px]">
        <div className="flex min-h-0 min-w-0 flex-col">
          <div
            ref={scrollRef}
            className="flex flex-1 flex-col items-center gap-5 overflow-auto bg-muted/50 p-2 sm:p-6"
          >
            {pages.map((_model, i) => (
              <div
                key={i}
                ref={(el) => {
                  pageRefs.current[i] = el
                }}
                className="relative h-fit rounded-[3px] bg-white shadow-md"
              >
                <canvas
                  ref={(el) => {
                    canvasRefs.current[i] = el
                  }}
                  className="block cursor-crosshair rounded-[3px]"
                  onClick={(e) => {
                    const hit = toModel(e, i)
                    if (!hit) return
                    setPageIndex(i)
                    const p = paragraphAt(hit.model, hit.x, hit.y)
                    setSelected(p ? { page: i, paragraph: p } : null)
                    if (!p && imageAt(hit.model, hit.x, hit.y)) {
                      setStatus({ text: 'image selected', state: 'idle' })
                    }
                  }}
                  onDoubleClick={(e) => {
                    const hit = toModel(e, i)
                    if (!hit) return
                    const p = paragraphAt(hit.model, hit.x, hit.y)
                    if (p) {
                      setPageIndex(i)
                      setEditing({ paragraph: p, text: p.text })
                    }
                  }}
                />
                <span className="absolute -bottom-4 right-0 text-[10px] tabular-nums text-muted-foreground">
                  {i + 1}
                </span>
              </div>
            ))}
          </div>
          {/* The same instruction in the language of the device it is on.
              "Double-click" is not an action anyone performs on a phone. */}
          <p className="hidden shrink-0 border-t border-border bg-card px-3 py-1.5 text-center text-[11.5px] text-muted-foreground lg:block">
            Scroll to move between pages · click to select a paragraph · double-click to edit
          </p>
          <p className="shrink-0 border-t border-border bg-card px-3 py-1.5 text-center text-[11.5px] text-muted-foreground lg:hidden">
            Scroll for pages · tap a paragraph · double-tap to edit
          </p>
        </div>

        <Inspector
          info={info}
          page={page}
          selected={selected?.paragraph ?? null}
          log={log}
          saved={saved}
          wasm={wasm}
          handle={handle}
        />
      </div>

      {/* --- the bar a thumb reaches ---
          Below the rail's breakpoint the inspector is simply absent, which
          left the page count, the fidelity floor, every operation and the
          whole document panel unreachable on a phone. This is where they go:
          page position, the actions, and a sheet for the rest. */}
      <div className="flex h-14 shrink-0 items-center gap-1 border-t border-border bg-card px-2 lg:hidden">
        <Button variant="ghost" size="icon" onClick={() => goto(pageIndex - 1)} aria-label="Previous page">
          <ChevronLeft />
        </Button>
        <span
          data-testid="page-label"
          className="min-w-12 text-center text-[13px] tabular-nums text-muted-foreground"
        >
          {info ? `${pageIndex + 1} / ${info.pageCount}` : '–'}
        </span>
        <Button variant="ghost" size="icon" onClick={() => goto(pageIndex + 1)} aria-label="Next page">
          <ChevronRight />
        </Button>

        <div className="flex-1" />

        <Button
          variant="ghost"
          size="icon"
          disabled={!session.canUndo}
          onClick={() => run('Undo', (m, h) => m.undo(h))}
          aria-label="Undo"
        >
          <Undo2 />
        </Button>
        <Button variant="outline" size="sm" onClick={save}>
          Save
        </Button>
        <Button variant="outline" size="sm" onClick={() => setDetails(true)}>
          Details
          {session.staged > 0 && (
            <span className="ml-1 rounded-full bg-primary px-1.5 text-[11px] tabular-nums text-primary-foreground">
              {session.staged}
            </span>
          )}
        </Button>
      </div>

      <Dialog open={details} onOpenChange={setDetails}>
        <DialogContent className="lg:hidden">
          <DialogHeader>
            <DialogTitle>This document</DialogTitle>
          </DialogHeader>
          <div className="max-h-[65dvh] overflow-y-auto">
            <MobileTools
              info={info}
              page={page}
              selected={selected?.paragraph ?? null}
              log={log}
              saved={saved}
              floor={floor}
              onFloor={(v) => {
                setFloor(v)
                run('Configure', (m, h) => m.configureSession(h, { requireFidelity: v }))
              }}
              onRedact={() => {
                setDetails(false)
                setRedacting('')
              }}
              onCompact={() => {
                const n = run('Compact fonts', (m, h) => m.compactFonts(h))
                if (n !== undefined) note('Compact fonts', 'exact', `${n} font(s) touched`)
              }}
            />
          </div>
        </DialogContent>
      </Dialog>

      {/* --- status --- */}
      <footer className="flex h-10 shrink-0 items-center gap-2 border-t border-border bg-card px-3 text-[12.5px]">
        <span
          className={cn(
            'size-1.5 rounded-full',
            status.state === 'ok' && 'bg-success',
            status.state === 'error' && 'bg-destructive',
            status.state === 'idle' && 'bg-muted-foreground/50',
          )}
        />
        <span data-testid="status" className="truncate">{status.text}</span>
        <div className="flex-1" />
        {/* The version comes from the module itself, so its presence is proof
            the library answered rather than merely that the page rendered. */}
        {wasm && (
          <span data-testid="version" className="tabular-nums text-muted-foreground">
            rasura {wasm.version()}
          </span>
        )}
        {info && (
          <span className="hidden tabular-nums text-muted-foreground sm:inline">
            {fmtBytes(info.memoryUsage)}
          </span>
        )}
        {/* Undo, redo and the staged count are on the bottom bar below the
            rail's breakpoint, where a thumb can reach them. Repeating them
            here put two undo buttons on a phone, one of them in a 10px-tall
            strip at the very bottom of the screen. */}
        <Separator orientation="vertical" className="hidden lg:block" />
        <Badge className="hidden lg:inline-flex">{session.staged} staged</Badge>
        <Button
          variant="ghost"
          size="sm"
          className="hidden lg:inline-flex"
          disabled={!session.canUndo}
          onClick={() => {
            run('Undo', (m, h) => m.undo(h))
            note('Undo', 'exact')
          }}
        >
          <Undo2 /> Undo
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="hidden lg:inline-flex"
          disabled={!session.canRedo}
          onClick={() => {
            run('Redo', (m, h) => m.redo(h))
            note('Redo', 'exact')
          }}
        >
          <Redo2 /> Redo
        </Button>
      </footer>

      {/* --- edit dialog --- */}
      <Dialog open={editing !== null} onOpenChange={(o) => !o && setEditing(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit paragraph</DialogTitle>
            <DialogDescription>
              Only the characters that actually differ are sent to the library, so most
              edits stay <code>exact</code>.
            </DialogDescription>
          </DialogHeader>
          <div className="px-5 pb-2">
            <Label>Text</Label>
            <textarea
              className="min-h-32 w-full rounded-md border border-input bg-card p-2.5 text-[13px] leading-relaxed"
              value={editing?.text ?? ''}
              onChange={(e) => setEditing((s) => (s ? { ...s, text: e.target.value } : s))}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditing(null)}>
              Cancel
            </Button>
            <Button onClick={applyEdit}>Replace</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* --- redact dialog --- */}
      <Dialog open={redacting !== null} onOpenChange={(o) => !o && setRedacting(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Redact text</DialogTitle>
            <DialogDescription>
              Removal is irreversible and forces a full rewrite. An incremental save would
              leave the original bytes in the file.
            </DialogDescription>
          </DialogHeader>
          <div className="px-5 pb-2">
            <Label>Exact text to remove</Label>
            <Input
              value={redacting ?? ''}
              onChange={(e) => setRedacting(e.target.value)}
              placeholder="Account 4417-9920"
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setRedacting(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() => {
                const what = redacting ?? ''
                setRedacting(null)
                if (!what) return
                const out = run('Redact', (m, h) => m.redactText(h, what))
                if (!out) return
                const verdict = run('Verify', (m, h) => m.verifyRedaction(h, what))
                note(
                  'Redact',
                  out.fidelity,
                  verdict
                    ? `${verdict.clean ? 'no trace found' : 'TRACE REMAINS'} · not checked: ${verdict.notChecked.join(', ') || 'nothing'}`
                    : '',
                )
                setStatus({
                  text: verdict?.clean ? 'redacted and verified' : 'redacted, but a trace remains',
                  state: verdict?.clean ? 'ok' : 'error',
                })
              }}
            >
              Redact
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

/* --- inspector ------------------------------------------------------------ */

function Pair({ k, v }: { k: string; v: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-3 border-b border-border/50 py-1 text-[12.5px] last:border-0">
      <span className="whitespace-nowrap text-muted-foreground">{k}</span>
      <span className="text-right tabular-nums break-words">{v}</span>
    </div>
  )
}

function Inspector({
  info,
  page,
  selected,
  log,
  saved,
  wasm,
  handle,
}: {
  info: R.DocumentInfo | null
  page: PageModel | null
  selected: Paragraph | null
  log: LogEntry[]
  saved: R.Saved | null
  wasm: R.Wasm | null
  handle: number | null
}) {
  const fonts = React.useMemo(
    () => (wasm && handle !== null ? safely(() => wasm.fontRequirements(handle), []) : []),
    [wasm, handle, info],
  )
  const fields = React.useMemo(
    () => (wasm && handle !== null ? safely(() => wasm.formFields(handle), []) : []),
    [wasm, handle, info],
  )

  return (
    <aside className="hidden min-h-0 flex-col border-l border-border bg-card lg:flex">
      <InspectorBody
        info={info}
        page={page}
        selected={selected}
        log={log}
        saved={saved}
        fonts={fonts}
        fields={fields}
      />
    </aside>
  )
}

/**
 * The panel's contents, without the panel.
 *
 * Split out so the phone's sheet and the desktop's rail show the same thing.
 * Rendering it twice from two copies is how they drift, and the version behind
 * the smaller breakpoint is the one nobody would notice going stale.
 */
function InspectorBody({
  info,
  page,
  selected,
  log,
  saved,
  fonts,
  fields,
}: {
  info: R.DocumentInfo | null
  page: PageModel | null
  selected: Paragraph | null
  log: LogEntry[]
  saved: R.Saved | null
  fonts: ReturnType<R.Wasm['fontRequirements']>
  fields: ReturnType<R.Wasm['formFields']>
}) {
  return (
      <Tabs defaultValue="document" className="flex min-h-0 flex-1 flex-col">
        <div className="border-b border-border p-2.5">
          <TabsList>
            <TabsTrigger value="document">Document</TabsTrigger>
            <TabsTrigger value="fonts">Fonts</TabsTrigger>
            <TabsTrigger value="fields">Fields</TabsTrigger>
            <TabsTrigger value="log">Log</TabsTrigger>
          </TabsList>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          <TabsContent value="document" className="flex flex-col gap-2.5">
            {saved && <SaveCard saved={saved} />}
            <Card>
              <CardHeader>
                <Layers className="size-3.5" /> Document
              </CardHeader>
              <CardBody>
                <Pair k="Pages" v={info?.pageCount ?? '–'} />
                <Pair k="Kind" v={info?.documentKind ?? '–'} />
                <Pair k="Tagged" v={info?.taggedStatus ?? '–'} />
                <Pair k="Revisions" v={info?.revisionCount ?? '–'} />
                <Pair k="Encrypted" v={info?.encrypted ? 'yes' : 'no'} />
                <Pair k="Memory" v={info ? fmtBytes(info.memoryUsage) : '–'} />
              </CardBody>
            </Card>

            {page && (
              <Card>
                <CardHeader>This page</CardHeader>
                <CardBody>
                  <Pair k="Paragraphs" v={page.paragraphs.length} />
                  <Pair k="Images" v={page.images.length} />
                  <Pair k="Tables" v={page.tables.length} />
                  {selected && (
                    <>
                      <Pair k="Selected" v={`${selected.lineCount} line(s)`} />
                      <Pair
                        k="Confidence"
                        v={
                          <Badge variant={selected.textConfidence === 'exact' ? 'exact' : 'reembedded'}>
                            {selected.textConfidence}
                          </Badge>
                        }
                      />
                    </>
                  )}
                </CardBody>
              </Card>
            )}

            {info && info.leniencies.length > 0 && (
              <Card>
                <CardHeader>Leniencies</CardHeader>
                <CardBody className="text-[12px] text-muted-foreground">
                  <p className="mb-2">
                    Specification deviations tolerated to open this file. No other viewer
                    will tell you these.
                  </p>
                  {info.leniencies.slice(0, 8).map((l, i) => (
                    <Pair key={i} k={l.kind} v={l.detail} />
                  ))}
                </CardBody>
              </Card>
            )}
          </TabsContent>

          <TabsContent value="fonts">
            {fonts.length === 0 ? (
              <Empty>No fonts reported.</Empty>
            ) : (
              <Card>
                <CardHeader>Fonts</CardHeader>
                <CardBody>
                  {fonts.map((f, i) => (
                    <div key={i} className="border-b border-border/50 py-2 last:border-0">
                      <div className="flex items-center justify-between gap-2">
                        <span className="truncate text-[12.5px] font-medium">{f.pdfFont}</span>
                        <Badge variant={f.needsSupplying ? 'reembedded' : 'exact'}>
                          {f.needsSupplying ? 'needs supplying' : f.embedded ? 'embedded' : 'standard'}
                        </Badge>
                      </div>
                      <p className="mt-0.5 text-[11.5px] text-muted-foreground">
                        {f.subset ? 'subset · ' : ''}
                        coverage {f.coverage}
                      </p>
                    </div>
                  ))}
                </CardBody>
              </Card>
            )}
          </TabsContent>

          <TabsContent value="fields">
            {fields.length === 0 ? (
              <Empty>This document has no form fields.</Empty>
            ) : (
              <Card>
                <CardHeader>Fields</CardHeader>
                <CardBody>
                  {fields.map((f, i) => (
                    <Pair key={i} k={f.name} v={f.value || <em className="opacity-60">empty</em>} />
                  ))}
                </CardBody>
              </Card>
            )}
          </TabsContent>

          <TabsContent value="log">
            {log.length === 0 ? (
              <Empty>
                Every operation is recorded here with the fidelity rung it achieved.
              </Empty>
            ) : (
              <div className="flex flex-col gap-2">
                {[...log].reverse().map((e) => (
                  <div
                    key={e.id}
                    className={cn(
                      'rounded-lg border px-2.5 py-2 text-[12.5px]',
                      e.rung === 'exact' && 'border-success/20 bg-success/5',
                      e.rung === 'refused' && 'border-destructive/20 bg-destructive/5',
                      e.rung !== 'exact' && e.rung !== 'refused' && 'border-warning/20 bg-warning/5',
                    )}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-medium">{e.what}</span>
                      <Badge
                        variant={
                          e.rung === 'exact' ? 'exact' : e.rung === 'refused' ? 'substituted' : 'reembedded'
                        }
                      >
                        {e.rung}
                      </Badge>
                    </div>
                    {e.detail && (
                      <p className="mt-1 break-words text-[11.5px] text-muted-foreground">{e.detail}</p>
                    )}
                    <p className="mt-1 text-[11px] tabular-nums text-muted-foreground/70">{e.at}</p>
                  </div>
                ))}
              </div>
            )}
          </TabsContent>
        </div>
      </Tabs>
  )
}

/**
 * The sheet behind "Details", which is where a phone reaches everything the
 * rail holds on a wider screen: the fidelity floor, the operations that are not
 * one-tap, and the whole inspector.
 */
function MobileTools({
  info,
  page,
  selected,
  log,
  saved,
  floor,
  onFloor,
  onRedact,
  onCompact,
}: {
  info: R.DocumentInfo | null
  page: PageModel | null
  selected: Paragraph | null
  log: LogEntry[]
  saved: R.Saved | null
  floor: string
  onFloor: (v: string) => void
  onRedact: () => void
  onCompact: () => void
}) {
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1.5">
        <Label>Require fidelity</Label>
        <Select value={floor} onValueChange={onFloor}>
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="overlaid">any fidelity</SelectItem>
            <SelectItem value="substituted">substituted</SelectItem>
            <SelectItem value="reembedded">re-embedded</SelectItem>
            <SelectItem value="exact">exact</SelectItem>
          </SelectContent>
        </Select>
      </div>

      <div className="flex gap-2">
        <Button variant="outline" size="sm" className="flex-1" onClick={onCompact}>
          <Shrink /> Compact fonts
        </Button>
        <Button variant="destructive" size="sm" className="flex-1" onClick={onRedact}>
          Redact…
        </Button>
      </div>

      <Separator />

      <div className="min-h-[18rem]">
        <InspectorBody
          info={info}
          page={page}
          selected={selected}
          log={log}
          saved={saved}
          fonts={[]}
          fields={[]}
        />
      </div>
    </div>
  )
}

/** The claim of incremental saving, as a bar: what was kept against what was added. */
function SaveCard({ saved }: { saved: R.Saved }) {
  const total = saved.bytes.length
  const added = saved.bytesAppended || 0
  const kept = Math.max(0, total - added)
  return (
    <Card>
      <CardHeader>Bytes written</CardHeader>
      <CardBody>
        <div className="my-2 flex h-2 overflow-hidden rounded-full bg-muted">
          <div className="bg-muted-foreground/35" style={{ width: `${(kept / total) * 100}%` }} />
          <div className="min-w-0.5 bg-primary" style={{ width: `${(added / total) * 100}%` }} />
        </div>
        <Pair k="Mode" v={saved.mode} />
        <Pair k="Untouched" v={fmtBytes(kept)} />
        <Pair k="Appended" v={fmtBytes(added)} />
      </CardBody>
    </Card>
  )
}

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="px-2 py-8 text-center text-[12.5px] text-muted-foreground">{children}</p>
}

function safely<T>(fn: () => T, fallback: T): T {
  try {
    return fn()
  } catch {
    return fallback
  }
}
