import * as React from 'react'
import { Link, NavLink, Outlet, useLocation } from 'react-router-dom'
import { ArrowLeft, ArrowRight, Menu, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { GROUPS, neighbours } from '@/nav'
import { applyMeta } from '@/seo'
import { cn } from '@/lib/utils'

/** Headings a page registers so the right rail can list them. */
export interface Heading {
  id: string
  text: string
  level: 2 | 3
}

const TocContext = React.createContext<(h: Heading[]) => void>(() => {})

/** Called once per page with its headings. */
export function useHeadings(headings: Heading[]) {
  const set = React.useContext(TocContext)
  const key = headings.map((h) => h.id).join('|')
  React.useEffect(() => {
    set(headings)
    return () => set([])
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
}

export function DocsLayout() {
  const [headings, setHeadings] = React.useState<Heading[]>([])
  const [open, setOpen] = React.useState(false)
  const { pathname } = useLocation()
  const slug = pathname.replace(/^\/+|\/+$/g, '') || 'introduction'
  const { prev, next } = neighbours(slug)
  const active = useActiveHeading(headings)

  React.useEffect(() => {
    setOpen(false)
    window.scrollTo(0, 0)
    applyMeta(slug)
  }, [pathname, slug])

  return (
    <div className="mx-auto flex w-full max-w-[90rem] flex-1 gap-8 px-4 sm:px-6">
      <Sidebar open={open} onClose={() => setOpen(false)} />

      <main className="min-w-0 flex-1 py-8 lg:py-10">
        <Button
          variant="outline"
          size="sm"
          className="mb-6 lg:hidden"
          onClick={() => setOpen(true)}
        >
          <Menu /> Contents
        </Button>

        <TocContext.Provider value={setHeadings}>
          <article className="prose">
            <Outlet />
          </article>
        </TocContext.Provider>

        <nav className="mt-14 flex gap-3 border-t border-border pt-6">
          {prev && (
            <Link
              to={`/${prev.slug}`}
              className="group flex flex-1 flex-col gap-0.5 rounded-lg border border-border p-3 no-underline transition-colors hover:bg-accent"
            >
              <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                <ArrowLeft className="size-3" /> Previous
              </span>
              <span className="text-[13px] font-medium text-foreground">{prev.title}</span>
            </Link>
          )}
          {next && (
            <Link
              to={`/${next.slug}`}
              className="group flex flex-1 flex-col items-end gap-0.5 rounded-lg border border-border p-3 no-underline transition-colors hover:bg-accent"
            >
              <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                Next <ArrowRight className="size-3" />
              </span>
              <span className="text-[13px] font-medium text-foreground">{next.title}</span>
            </Link>
          )}
        </nav>
      </main>

      {headings.length > 1 && (
        <aside className="sticky top-14 hidden h-[calc(100dvh-3.5rem)] w-52 shrink-0 overflow-y-auto py-10 xl:block">
          <p className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            On this page
          </p>
          <nav className="flex flex-col gap-px border-l border-border">
            {headings.map((h) => (
              <a
                key={h.id}
                href={`#${h.id}`}
                className={cn(
                  '-ml-px border-l-2 py-1 text-[12.5px] no-underline transition-colors',
                  h.level === 3 ? 'pl-6 pr-3' : 'px-3',
                  active === h.id
                    ? 'border-primary font-medium text-foreground'
                    : 'border-transparent text-muted-foreground hover:text-foreground',
                )}
              >
                {h.text}
              </a>
            ))}
          </nav>
        </aside>
      )}
    </div>
  )
}

function Sidebar({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <>
      {open && (
        <div
          className="fixed inset-0 z-40 bg-black/40 lg:hidden"
          onClick={onClose}
          aria-hidden
        />
      )}
      <aside
        className={cn(
          'shrink-0 overflow-y-auto',
          open
            ? 'fixed inset-y-0 left-0 z-50 w-72 border-r border-border bg-card px-4 py-6 lg:hidden'
            : 'sticky top-14 hidden h-[calc(100dvh-3.5rem)] w-56 py-8 lg:block',
        )}
      >
        {open && (
          <Button variant="ghost" size="icon-sm" className="mb-4" onClick={onClose} aria-label="Close">
            <X />
          </Button>
        )}
        {GROUPS.map((group) => (
          <div key={group.title} className="mb-6">
            <p className="mb-1.5 px-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              {group.title}
            </p>
            <nav className="flex flex-col gap-px">
              {group.entries.map((entry) => (
                <NavLink
                  key={entry.slug}
                  to={`/${entry.slug}`}
                  className={({ isActive }) =>
                    cn(
                      'rounded-md px-2 py-1.5 text-[13px] no-underline transition-colors',
                      isActive
                        ? 'bg-accent font-medium text-foreground'
                        : 'text-muted-foreground hover:text-foreground',
                    )
                  }
                >
                  {entry.title}
                </NavLink>
              ))}
            </nav>
          </div>
        ))}
      </aside>
    </>
  )
}

function useActiveHeading(headings: Heading[]) {
  const [active, setActive] = React.useState('')
  const key = headings.map((h) => h.id).join('|')

  React.useEffect(() => {
    if (!headings.length) return
    const observer = new IntersectionObserver(
      (entries) => {
        // Topmost intersecting section wins. Taking the last entry makes the
        // highlight jump to the bottom of the viewport whenever two are on
        // screen, which is most of the time.
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)
        if (visible[0]) setActive(visible[0].target.id)
      },
      { rootMargin: '-72px 0px -65% 0px' },
    )
    for (const h of headings) {
      const el = document.getElementById(h.id)
      if (el) observer.observe(el)
    }
    return () => observer.disconnect()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])

  return active
}

/** A page heading that the rail can link to. */
export function H2({ id, children }: { id: string; children: React.ReactNode }) {
  return <h2 id={id}>{children}</h2>
}

export function H3({ id, children }: { id: string; children: React.ReactNode }) {
  return <h3 id={id}>{children}</h3>
}

/** The title block every page opens with. */
export function PageHeader({ title, summary }: { title: string; summary: string }) {
  return (
    <header className="mb-8 border-b border-border pb-6">
      <h1 className="text-3xl font-semibold tracking-[-0.025em]">{title}</h1>
      <p className="mt-2 text-[15px] leading-relaxed text-muted-foreground">{summary}</p>
    </header>
  )
}
