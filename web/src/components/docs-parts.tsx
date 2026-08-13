import * as React from 'react'
import { Check, Copy } from 'lucide-react'
import { Badge } from '@/components/ui/primitives'
import { cn } from '@/lib/utils'

/** A section that the sidebar and the table of contents can both find. */
export function Section({
  id,
  title,
  children,
}: {
  id: string
  title: string
  children: React.ReactNode
}) {
  return (
    <section id={id} className="scroll-mt-20">
      <h2>{title}</h2>
      {children}
    </section>
  )
}

/**
 * A code block with a copy button.
 *
 * No syntax highlighter. One would add a parser and a theme for every language
 * shown here, and the thing a reader copies is the text — which is what this
 * gets right, including not copying the language label or a line number.
 */
export function Code({ children, lang }: { children: string; lang?: string }) {
  const [copied, setCopied] = React.useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(children.trim())
      setCopied(true)
      setTimeout(() => setCopied(false), 1600)
    } catch {
      // Clipboard access can be refused; the text is selectable either way.
    }
  }

  return (
    <div className="group relative my-4 overflow-hidden rounded-lg border border-border bg-muted/40">
      <div className="flex items-center justify-between border-b border-border px-3 py-1.5">
        <span className="font-mono text-[11px] uppercase tracking-wide text-muted-foreground">
          {lang ?? 'text'}
        </span>
        <button
          onClick={copy}
          className="rounded-sm p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-accent hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
          aria-label="Copy code"
        >
          {copied ? <Check className="size-3.5 text-primary" /> : <Copy className="size-3.5" />}
        </button>
      </div>
      <pre className="overflow-x-auto px-3 py-3 text-[12.5px] leading-relaxed">
        <code className="font-mono">{children.trim()}</code>
      </pre>
    </div>
  )
}

/** A callout. `kind` decides the colour, and the colour means something. */
export function Note({
  kind = 'info',
  title,
  children,
}: {
  kind?: 'info' | 'warning' | 'success'
  title?: string
  children: React.ReactNode
}) {
  const tone = {
    info: 'border-info/25 bg-info/6',
    warning: 'border-warning/30 bg-warning/8',
    success: 'border-success/25 bg-success/6',
  }[kind]

  return (
    <div className={cn('my-4 rounded-lg border px-4 py-3 text-[13px]', tone)}>
      {title && <p className="!mt-0 !mb-1 font-semibold">{title}</p>}
      <div className="[&>p:first-child]:!mt-0 [&>p:last-child]:!mb-0 text-muted-foreground">
        {children}
      </div>
    </div>
  )
}

/** A capability row: what it does, and how far it goes. */
export function Capability({
  name,
  status,
  children,
}: {
  name: string
  status: 'shipped' | 'partial' | 'refused'
  children: React.ReactNode
}) {
  const badge = {
    shipped: { variant: 'exact' as const, label: 'Shipped' },
    partial: { variant: 'reembedded' as const, label: 'Partial' },
    refused: { variant: 'substituted' as const, label: 'Refused by design' },
  }[status]

  return (
    <div className="flex flex-col gap-1.5 border-b border-border py-3 last:border-0 sm:flex-row sm:gap-4">
      <div className="flex min-w-52 items-start gap-2">
        <span className="text-[13px] font-medium">{name}</span>
      </div>
      <div className="flex-1 text-[13px] text-muted-foreground">{children}</div>
      <Badge variant={badge.variant} className="h-fit shrink-0">
        {badge.label}
      </Badge>
    </div>
  )
}
