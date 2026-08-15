import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router-dom'
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
import { Architecture, Types } from '@/pages/docs/reference'
import RustApi from '@/pages/docs/rust-api'
import Editor from '@/pages/editor'
import Landing from '@/pages/landing'
import { structuredData } from '@/seo'
import './index.css'

// Real paths, not hash fragments. Pages has no rewrite rule, so a reload on
// /editor asks the server for a file that is not there; public/404.html hands
// the request back to this router. The hash form worked and cost the site its
// search presence: every route shared one URL, so there was one page to index
// and no way to link to any part of the documentation.
const router = createBrowserRouter([
  {
    element: <Shell />,
    children: [
      // The root is the landing page, full width and without the docs
      // furniture. `/introduction` is the same argument at reading pace, and
      // is where the sidebar starts.
      { index: true, element: <Landing /> },
      { path: '/editor', element: <Editor /> },
      {
        element: <DocsLayout />,
        children: [
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
          { path: 'rust', element: <RustApi /> },
          { path: 'architecture', element: <Architecture /> },
          // Anything unrecognised is the introduction rather than a dead end.
          { path: '*', element: <Introduction /> },
        ],
      },
    ],
  },
], { basename: import.meta.env.BASE_URL })

// Injected once, and only if the prerendered HTML did not already carry it.
//
// Every route now ships as static HTML snapshotted from a real browser, so the
// block is in the document before this line runs. Appending unconditionally
// gave every page two identical copies, which is the kind of thing a structured
// data validator complains about and nobody notices.
if (!document.querySelector('script[type="application/ld+json"]')) {
  const ld = document.createElement('script')
  ld.type = 'application/ld+json'
  ld.textContent = JSON.stringify(structuredData())
  document.head.append(ld)
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider>
      <TooltipProvider delayDuration={280}>
        <RouterProvider router={router} />
      </TooltipProvider>
    </ThemeProvider>
  </StrictMode>,
)
