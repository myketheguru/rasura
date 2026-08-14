import * as React from 'react'
import { Highlight, type PrismTheme } from 'prism-react-renderer'
import { Check, Copy } from 'lucide-react'
import { useTheme } from '@/components/theme'
import { cn } from '@/lib/utils'

/**
 * The editor palette.
 *
 * Written against our own tokens rather than taken from a preset. Every preset
 * carries its own idea of a background and a foreground, and the result is a
 * code block that looks pasted in from another site. These two only colour the
 * syntax; the surface comes from the page.
 */
const light: PrismTheme = {
  plain: { color: 'hsl(165 25% 16%)' },
  styles: [
    { types: ['comment', 'prolog', 'cdata'], style: { color: 'hsl(160 8% 48%)', fontStyle: 'italic' } },
    { types: ['punctuation'], style: { color: 'hsl(160 8% 45%)' } },
    { types: ['keyword', 'builtin', 'important'], style: { color: 'hsl(275 55% 48%)' } },
    { types: ['function', 'function-variable', 'method'], style: { color: 'hsl(214 78% 42%)' } },
    { types: ['string', 'char', 'attr-value', 'regex'], style: { color: 'hsl(157 68% 28%)' } },
    { types: ['number', 'boolean', 'constant', 'symbol'], style: { color: 'hsl(24 78% 40%)' } },
    { types: ['class-name', 'maybe-class-name', 'namespace'], style: { color: 'hsl(196 68% 34%)' } },
    { types: ['operator', 'entity', 'url'], style: { color: 'hsl(340 55% 42%)' } },
    { types: ['property', 'attr-name', 'variable'], style: { color: 'hsl(214 60% 38%)' } },
    { types: ['deleted'], style: { color: 'hsl(0 65% 45%)' } },
    { types: ['inserted'], style: { color: 'hsl(157 65% 30%)' } },
  ],
}

const dark: PrismTheme = {
  plain: { color: 'hsl(150 14% 88%)' },
  styles: [
    { types: ['comment', 'prolog', 'cdata'], style: { color: 'hsl(155 8% 52%)', fontStyle: 'italic' } },
    { types: ['punctuation'], style: { color: 'hsl(155 8% 60%)' } },
    { types: ['keyword', 'builtin', 'important'], style: { color: 'hsl(280 65% 76%)' } },
    { types: ['function', 'function-variable', 'method'], style: { color: 'hsl(210 85% 72%)' } },
    { types: ['string', 'char', 'attr-value', 'regex'], style: { color: 'hsl(150 55% 62%)' } },
    { types: ['number', 'boolean', 'constant', 'symbol'], style: { color: 'hsl(30 80% 68%)' } },
    { types: ['class-name', 'maybe-class-name', 'namespace'], style: { color: 'hsl(190 60% 66%)' } },
    { types: ['operator', 'entity', 'url'], style: { color: 'hsl(340 70% 74%)' } },
    { types: ['property', 'attr-name', 'variable'], style: { color: 'hsl(205 70% 74%)' } },
    { types: ['deleted'], style: { color: 'hsl(0 65% 68%)' } },
    { types: ['inserted'], style: { color: 'hsl(150 55% 62%)' } },
  ],
}

const LABELS: Record<string, string> = {
  js: 'JavaScript',
  ts: 'TypeScript',
  tsx: 'TSX',
  rust: 'Rust',
  bash: 'Shell',
  toml: 'TOML',
  json: 'JSON',
  text: 'Text',
}

export function Code({
  children,
  lang = 'js',
  title,
  lines,
}: {
  children: string
  lang?: string
  /** Shown instead of the language, for a file name. */
  title?: string
  /** Line numbers, for anything long enough to point at. */
  lines?: boolean
}) {
  const [copied, setCopied] = React.useState(false)
  const { theme } = useTheme()
  const [dark_, setDark] = React.useState(false)

  // The syntax palette has to follow the theme, and "system" means asking the
  // browser rather than reading our own attribute.
  React.useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const resolve = () => setDark(theme === 'dark' || (theme === 'system' && media.matches))
    resolve()
    media.addEventListener('change', resolve)
    return () => media.removeEventListener('change', resolve)
  }, [theme])

  const code = children.trim()

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code)
      setCopied(true)
      setTimeout(() => setCopied(false), 1600)
    } catch {
      // Clipboard access can be refused. The text stays selectable.
    }
  }

  return (
    <div className="not-prose group relative my-5 overflow-hidden rounded-lg border border-border bg-card">
      <div className="flex items-center justify-between border-b border-border bg-muted/40 px-3 py-1.5">
        <span className="font-mono text-[11px] font-medium tracking-wide text-muted-foreground">
          {title ?? LABELS[lang] ?? lang}
        </span>
        <button
          onClick={copy}
          className="rounded-sm p-1 text-muted-foreground opacity-0 transition hover:bg-accent hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
          aria-label="Copy code"
        >
          {copied ? <Check className="size-3.5 text-primary" /> : <Copy className="size-3.5" />}
        </button>
      </div>

      <Highlight code={code} language={lang} theme={dark_ ? dark : light}>
        {({ tokens, getLineProps, getTokenProps }) => (
          <pre className="overflow-x-auto px-3 py-3 text-[12.5px] leading-[1.65]">
            <code className="font-mono">
              {tokens.map((line, i) => (
                <div key={i} {...getLineProps({ line })}>
                  {lines && (
                    <span className="mr-3 inline-block w-6 select-none text-right text-muted-foreground/50">
                      {i + 1}
                    </span>
                  )}
                  {line.map((token, k) => (
                    <span key={k} {...getTokenProps({ token })} />
                  ))}
                </div>
              ))}
            </code>
          </pre>
        )}
      </Highlight>
    </div>
  )
}

/** Inline code inside prose. */
export function C({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded-sm bg-muted px-1 py-0.5 font-mono text-[0.85em]">{children}</code>
  )
}

/** A callout. The colour carries meaning, so there are only three. */
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
    <div className={cn('not-prose my-5 rounded-lg border px-4 py-3 text-[13px] leading-relaxed', tone)}>
      {title && <p className="mb-1 font-semibold">{title}</p>}
      <div className="text-muted-foreground [&_a]:text-primary [&_a]:underline">{children}</div>
    </div>
  )
}
