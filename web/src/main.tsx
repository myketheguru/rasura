import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createHashRouter, RouterProvider } from 'react-router-dom'
import { TooltipProvider } from '@/components/ui/primitives'
import { ThemeProvider } from '@/components/theme'
import { Shell } from '@/components/shell'
import Docs from '@/pages/docs'
import Editor from '@/pages/editor'
import './index.css'

// A hash router, not a browser one. GitHub Pages serves static files and has no
// rewrite rule, so a reload on /editor asks the server for a file that is not
// there and gets a 404. The alternative is a 404.html that redirects, which is
// a trick that works and that nobody reading the deploy can see the reason for.
const router = createHashRouter([
  {
    element: <Shell />,
    children: [
      { path: '/', element: <Docs /> },
      { path: '/editor', element: <Editor /> },
      // Anything else is the docs rather than a dead end.
      { path: '*', element: <Docs /> },
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
