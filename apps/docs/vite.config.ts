import type { Plugin } from 'vite'

import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

import { createRequire } from 'node:module'

import { cjkFont } from './scripts/cjk-font.ts'
import { ICON_NAMES } from './src/icons.ts'
import { paletteCss } from './src/palette.ts'

const require = createRequire(import.meta.url)
const ICONS = 'virtual:icons'

interface IconSet {
  prefix: string
  width?: number
  height?: number
  icons: Record<string, { body: string, width?: number, height?: number }>
  aliases?: Record<string, { parent: string }>
}

/**
 * Puts the SVG bodies of ICON_NAMES and their view boxes into a module, after the injeca Docs.
 * Source: https://github.com/moeru-ai/injeca/blob/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs/vite.config.ts#L81-L100
 */
function icons(): Plugin {
  return {
    load(id) {
      if (id !== `\0${ICONS}`)
        return
      const bodies = Object.fromEntries(ICON_NAMES.map((full) => {
        const [prefix, name] = full.split(':') as [string, string]
        // lobe-icons (agent and model marks) is Project AIRI's Iconify port of @lobehub/icons.
        const set = require(prefix === 'lobe-icons' ? '@proj-airi/lobe-icons/icons.json' : `@iconify-json/${prefix}/icons.json`) as IconSet
        const found = set.icons[name] ?? set.icons[set.aliases?.[name]?.parent ?? '']
        if (!found)
          throw new Error(`${prefix} has no icon ${name}.`)
        return [full, { body: found.body, width: found.width ?? set.width ?? 24, height: found.height ?? set.height ?? 24 }]
      }))
      return `export default ${JSON.stringify(bodies)}`
    },
    name: 'icons',
    resolveId(id) {
      return id === ICONS ? `\0${ICONS}` : undefined
    },
  }
}

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
  plugins: [vue(), palette(), icons(), cjkFont()],
  // The profile studio reads the repository's examples/ at build time.
  server: { fs: { allow: ['../..'] } },
})
