import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createHashRouter, RouterProvider } from 'react-router-dom'
import { TooltipProvider } from '@/components/ui/primitives'
import { ThemeProvider } from '@/components/theme'
import { Shell } from '@/components/shell'
import { DocsLayout } from '@/components/docs-layout'
import { Introduction, Install, Quickstart } from '@/pages/docs/start'
import UseCases from '@/pages/docs/use-cases'
import Api from '@/pages/docs/api'
import {
  Composing,
  Editing,
  Encryption,
  Errors,
  Fidelity,
  Fonts,
  Reading,
  Redaction,
  Saving,
} from '@/pages/docs/guides'
import { Architecture, Rust, Types } from '@/pages/docs/reference'
import Editor from '@/pages/editor'
import './index.css'

// A hash router, not a browser one. GitHub Pages serves static files and has no
// rewrite rule, so a reload on /editor asks for a file that is not there and
// gets a 404. The alternative is a 404.html that redirects, which works and
// which nobody reading the deploy can see the reason for.
const router = createHashRouter([
  {
    element: <Shell />,
    children: [
      { path: '/editor', element: <Editor /> },
      {
        element: <DocsLayout />,
        children: [
          { index: true, element: <Introduction /> },
          { path: 'introduction', element: <Introduction /> },
          { path: 'install', element: <Install /> },
          { path: 'quickstart', element: <Quickstart /> },
          { path: 'use-cases', element: <UseCases /> },
          { path: 'reading', element: <Reading /> },
          { path: 'editing', element: <Editing /> },
          { path: 'fidelity', element: <Fidelity /> },
          { path: 'fonts', element: <Fonts /> },
          { path: 'composing', element: <Composing /> },
          { path: 'redaction', element: <Redaction /> },
          { path: 'encryption', element: <Encryption /> },
          { path: 'saving', element: <Saving /> },
          { path: 'errors', element: <Errors /> },
          { path: 'api', element: <Api /> },
          { path: 'types', element: <Types /> },
          { path: 'rust', element: <Rust /> },
          { path: 'architecture', element: <Architecture /> },
          // Anything unrecognised is the introduction rather than a dead end.
          { path: '*', element: <Introduction /> },
        ],
      },
    ],
  },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider>
      <TooltipProvider delayDuration={280}>
        <RouterProvider router={router} />
      </TooltipProvider>
    </ThemeProvider>
  </StrictMode>,
)
