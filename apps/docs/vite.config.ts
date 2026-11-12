import type { Plugin } from 'vite'

import vue from '@vitejs/plugin-vue'
import unocss from 'unocss/vite'
import { defineConfig } from 'vite'

import { cjkFont } from './scripts/cjk-font.ts'
import { paletteCss } from './src/palette.ts'

/** Puts the colors of src/palette.ts into the page head, as CSS custom properties for both themes. */
function palette(): Plugin {
  return {
    name: 'palette',
    transformIndexHtml: () => [{ children: paletteCss(), injectTo: 'head', tag: 'style' }],
  }
}

export default defineConfig({
  // Relative asset paths, so the build works from any directory.
  base: './',
  plugins: [vue(), unocss(), palette(), cjkFont()],
  // The profile studio reads the repository's examples/ at build time.
  server: { fs: { allow: ['../..'] } },
})
