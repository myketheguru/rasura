import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwind from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

// GitHub Pages serves this from a subdirectory (`/rasura/`), so every asset URL
// has to be relative to it. `base` is taken from the environment rather than
// written down, because the same build has to work from a local preview at `/`
// and from Pages at `/rasura/` — and a hard-coded base is the classic way to
// deploy a site whose every stylesheet 404s.
const base = process.env.RASURA_BASE ?? '/'

export default defineConfig({
  base,
  plugins: [react(), tailwind()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // The WASM module is over a megabyte and is loaded on demand by the editor
    // route, not by the docs. Warning about it on every build would train us to
    // ignore the warning that matters.
    chunkSizeWarningLimit: 2048,
  },
  // The module is fetched, not imported, so Vite must not try to pre-bundle it.
  assetsInclude: ['**/*.wasm'],
  worker: { format: 'es' },
})
