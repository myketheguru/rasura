/**
 * The rest of the component set: badge, card, separator, input, tabs, select,
 * switch, tooltip and dialog.
 *
 * shadcn's structure and Radix's primitives, in one file rather than nine.
 * Nine files is right for a project that adds components over years by
 * generator; this site has a fixed set and splitting them costs a reader eight
 * extra jumps to answer "what does a Badge look like".
 */
import * as React from 'react'
import * as SelectPrimitive from '@radix-ui/react-select'
import * as TabsPrimitive from '@radix-ui/react-tabs'
import * as SwitchPrimitive from '@radix-ui/react-switch'
import * as TooltipPrimitive from '@radix-ui/react-tooltip'
import * as DialogPrimitive from '@radix-ui/react-dialog'
import * as SeparatorPrimitive from '@radix-ui/react-separator'
import { cva, type VariantProps } from 'class-variance-authority'
import { Check, ChevronDown, X } from 'lucide-react'
import { cn } from '@/lib/utils'

/* --- badge ---------------------------------------------------------------- */

const badgeVariants = cva(
  'inline-flex items-center gap-1 rounded-full border px-2 py-px text-[11px] font-medium leading-relaxed whitespace-nowrap',
  {
    variants: {
      variant: {
        default: 'border-transparent bg-muted text-muted-foreground',
        outline: 'border-border text-muted-foreground',
        // The fidelity ladder, in colour. Each rung is a different judgement
        // and reads as one at a glance, which is the whole point of reporting
        // fidelity rather than throwing on it.
        exact: 'border-success/25 bg-success/12 text-success',
        reembedded: 'border-warning/25 bg-warning/12 text-warning',
        substituted: 'border-destructive/25 bg-destructive/10 text-destructive',
        overlaid: 'border-destructive/25 bg-destructive/10 text-destructive',
        info: 'border-info/25 bg-info/10 text-info',
      },
    },
    defaultVariants: { variant: 'default' },
  },
)

export function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<'span'> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />
}

/* --- card ----------------------------------------------------------------- */

export function Card({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('rounded-lg border border-border bg-card', className)} {...props} />
}

export function CardHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      className={cn(
        'flex items-center gap-2 border-b border-border bg-muted/50 px-3 py-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground',
        className,
      )}
      {...props}
    />
  )
}

export function CardBody({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('px-3 py-2.5', className)} {...props} />
}

/* --- separator ------------------------------------------------------------ */

export function Separator({
  className,
  orientation = 'horizontal',
  ...props
}: React.ComponentProps<typeof SeparatorPrimitive.Root>) {
  return (
    <SeparatorPrimitive.Root
      decorative
      orientation={orientation}
      className={cn(
        'shrink-0 bg-border',
        orientation === 'horizontal' ? 'h-px w-full' : 'h-5 w-px',
        className,
      )}
      {...props}
    />
  )
}

/* --- input ---------------------------------------------------------------- */

export function Input({ className, ...props }: React.ComponentProps<'input'>) {
  return (
    <input
      className={cn(
        'flex h-8 w-full rounded-md border border-input bg-card px-2.5 text-[13px] placeholder:text-muted-foreground focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 disabled:opacity-50',
        className,
      )}
      {...props}
    />
  )
}

export function Label({ className, ...props }: React.ComponentProps<'label'>) {
  return (
    <label
      className={cn('mb-1.5 block text-xs font-medium text-muted-foreground', className)}
      {...props}
    />
  )
}

/* --- tabs ----------------------------------------------------------------- */

export const Tabs = TabsPrimitive.Root

export function TabsList({ className, ...props }: React.ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      className={cn('inline-flex gap-0.5 rounded-md bg-muted p-0.5', className)}
      {...props}
    />
  )
}

export function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      className={cn(
        'rounded-sm px-2.5 py-1 text-xs font-medium text-muted-foreground transition-colors data-[state=active]:bg-card data-[state=active]:text-foreground data-[state=active]:shadow-sm',
        className,
      )}
      {...props}
    />
  )
}

export const TabsContent = TabsPrimitive.Content

/* --- select ---------------------------------------------------------------
 *
 * Radix rather than a native `<select>`: a native one draws its list with the
 * operating system's chrome, which is the wrong typeface, the wrong radius, and
 * on most engines a white popup in dark mode. No stylesheet fixes that.
 */

export const Select = SelectPrimitive.Root
export const SelectValue = SelectPrimitive.Value

export function SelectTrigger({
  className,
  children,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Trigger>) {
  return (
    <SelectPrimitive.Trigger
      className={cn(
        'inline-flex h-7 items-center justify-between gap-1.5 rounded-md border border-input bg-card px-2.5 text-[13px] font-medium whitespace-nowrap hover:bg-accent focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2',
        className,
      )}
      {...props}
    >
      {children}
      <SelectPrimitive.Icon asChild>
        <ChevronDown className="size-3.5 text-muted-foreground" />
      </SelectPrimitive.Icon>
    </SelectPrimitive.Trigger>
  )
}

export function SelectContent({
  className,
  children,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Content>) {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Content
        position="popper"
        sideOffset={5}
        className={cn(
          'z-50 min-w-[8rem] overflow-hidden rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-md data-[state=open]:animate-in data-[state=open]:fade-in-0',
          className,
        )}
        {...props}
      >
        <SelectPrimitive.Viewport>{children}</SelectPrimitive.Viewport>
      </SelectPrimitive.Content>
    </SelectPrimitive.Portal>
  )
}

export function SelectItem({
  className,
  children,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Item>) {
  return (
    <SelectPrimitive.Item
      className={cn(
        'relative flex cursor-pointer select-none items-center gap-2 rounded-sm py-1.5 pl-7 pr-2.5 text-[13px] outline-none data-[highlighted]:bg-accent',
        className,
      )}
      {...props}
    >
      <span className="absolute left-2 flex size-3.5 items-center justify-center">
        <SelectPrimitive.ItemIndicator>
          <Check className="size-3.5 text-primary" />
        </SelectPrimitive.ItemIndicator>
      </span>
      <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
    </SelectPrimitive.Item>
  )
}

/* --- switch --------------------------------------------------------------- */

export function Switch({ className, ...props }: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        'peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border-2 border-transparent transition-colors data-[state=checked]:bg-primary data-[state=unchecked]:bg-input',
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="pointer-events-none block size-4 rounded-full bg-card shadow-sm ring-0 transition-transform data-[state=checked]:translate-x-4 data-[state=unchecked]:translate-x-0" />
    </SwitchPrimitive.Root>
  )
}

/* --- tooltip -------------------------------------------------------------- */

export const TooltipProvider = TooltipPrimitive.Provider

export function Tooltip({ children, label }: { children: React.ReactNode; label: string }) {
  return (
    <TooltipPrimitive.Root>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          sideOffset={6}
          className="z-70 rounded-md bg-foreground px-2 py-1 text-[11px] font-medium text-background shadow-md"
        >
          {label}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  )
}

/* --- dialog --------------------------------------------------------------- */

export const Dialog = DialogPrimitive.Root
export const DialogTrigger = DialogPrimitive.Trigger
export const DialogClose = DialogPrimitive.Close

export function DialogContent({
  className,
  children,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content>) {
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Overlay className="fixed inset-0 z-100 bg-[hsl(165_25%_6%_/_0.45)] backdrop-blur-[2px] data-[state=open]:animate-in data-[state=open]:fade-in-0" />
      <DialogPrimitive.Content
        className={cn(
          'fixed left-1/2 top-1/2 z-100 w-[min(28rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-xl border border-border bg-card shadow-lg',
          className,
        )}
        {...props}
      >
        {children}
        <DialogPrimitive.Close className="absolute right-3 top-3 rounded-sm p-1 text-muted-foreground hover:bg-accent hover:text-foreground">
          <X className="size-4" />
          <span className="sr-only">Close</span>
        </DialogPrimitive.Close>
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  )
}

export function DialogHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('px-5 pb-1 pt-5', className)} {...props} />
}

export function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      className={cn('text-[15px] font-semibold tracking-tight', className)}
      {...props}
    />
  )
}

export function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      className={cn('mt-1 text-[13px] text-muted-foreground', className)}
      {...props}
    />
  )
}

export function DialogFooter({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('flex justify-end gap-2 px-5 pb-5 pt-2', className)} {...props} />
}
