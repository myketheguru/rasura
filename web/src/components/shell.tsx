import { Link, NavLink, Outlet, useLocation } from 'react-router-dom'
import { ExternalLink } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/primitives'
import { ThemeToggle } from '@/components/theme'
import { cn } from '@/lib/utils'

const REPO = 'https://github.com/myketheguru/rasura'

/** The wordmark: a monogram, flat, because the loudest thing on a page about a
 * document should be the document. */
export function Mark({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        'grid size-6 place-items-center rounded-[7px] bg-primary text-[13px] font-bold tracking-tight text-primary-foreground',
        className,
      )}
      aria-hidden
    >
      R
    </span>
  )
}

export function Shell() {
  const { pathname } = useLocation()
  const isEditor = pathname.startsWith('/editor')

  return (
    <div className={cn('flex min-h-dvh flex-col', isEditor && 'h-dvh overflow-hidden')}>
      <header className="sticky top-0 z-50 flex h-14 shrink-0 items-center gap-3 border-b border-border bg-card/85 px-4 backdrop-blur">
        <Link to="/" className="flex items-center gap-2 no-underline">
          <Mark />
          <span className="text-[15px] font-semibold tracking-tight text-foreground">Rasura</span>
        </Link>

        <Separator orientation="vertical" className="mx-1 hidden sm:block" />

        <nav className="hidden items-center gap-1 sm:flex">
          {[
            { to: '/', label: 'Documentation' },
            { to: '/editor', label: 'Editor' },
          ].map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === '/'}
              className={({ isActive }) =>
                cn(
                  'rounded-md px-2.5 py-1.5 text-[13px] font-medium no-underline transition-colors',
                  isActive
                    ? 'bg-accent text-foreground'
                    : 'text-muted-foreground hover:text-foreground',
                )
              }
            >
              {item.label}
            </NavLink>
          ))}
        </nav>

        <div className="flex-1" />

        <Button variant="ghost" size="sm" asChild>
          <a href={REPO} target="_blank" rel="noreferrer">
            GitHub
            <ExternalLink />
          </a>
        </Button>
        <ThemeToggle />
      </header>

      <Outlet />
    </div>
  )
}
