import * as React from 'react'
import { Monitor, Moon, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/primitives'

export type Theme = 'light' | 'dark' | 'system'

const KEY = 'rasura-theme'

function read(): Theme {
  try {
    const v = localStorage.getItem(KEY)
    return v === 'light' || v === 'dark' ? v : 'system'
  } catch {
    // A page opened from file:// or with storage blocked still works; it just
    // does not remember the choice.
    return 'system'
  }
}

function apply(theme: Theme) {
  const root = document.documentElement
  root.classList.remove('light', 'dark')
  if (theme !== 'system') root.classList.add(theme)
  try {
    localStorage.setItem(KEY, theme)
  } catch {
    /* applied, not remembered */
  }
}

const Ctx = React.createContext<{ theme: Theme; setTheme: (t: Theme) => void }>({
  theme: 'system',
  setTheme: () => {},
})

/**
 * Three states, not two.
 *
 * A toggle that flips light and dark has no way back to "follow the system",
 * which is what most people leave it on — so choosing once would opt them out
 * of their own machine's setting for ever.
 */
export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, set] = React.useState<Theme>(read)

  React.useEffect(() => {
    apply(theme)
  }, [theme])

  return <Ctx.Provider value={{ theme, setTheme: set }}>{children}</Ctx.Provider>
}

export const useTheme = () => React.useContext(Ctx)

/** The compact form, for an application bar. Cycles light → dark → system. */
export function ThemeToggle() {
  const { theme, setTheme } = useTheme()
  const next: Theme = theme === 'light' ? 'dark' : theme === 'dark' ? 'system' : 'light'
  const Icon = theme === 'light' ? Sun : theme === 'dark' ? Moon : Monitor

  return (
    <Button
      variant="ghost"
      size="icon"
      onClick={() => setTheme(next)}
      aria-label={`Theme: ${theme}. Switch to ${next}.`}
      title={`Theme: ${theme}`}
    >
      <Icon />
    </Button>
  )
}

/** The explicit form, for documentation, where the three states should be visible. */
export function ThemeSelect() {
  const { theme, setTheme } = useTheme()
  return (
    <Select value={theme} onValueChange={(v) => setTheme(v as Theme)}>
      <SelectTrigger className="w-28" aria-label="Theme">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="light">Light</SelectItem>
        <SelectItem value="dark">Dark</SelectItem>
        <SelectItem value="system">System</SelectItem>
      </SelectContent>
    </Select>
  )
}
