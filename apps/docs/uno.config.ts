import { createRequire } from 'node:module'

import { defineConfig, presetIcons, presetWind3, transformerDirectives, transformerVariantGroup } from 'unocss'

import { ICON_NAMES } from './src/icons.ts'

const require = createRequire(import.meta.url)

/** The roles of src/palette.ts, each with the shades it writes as custom properties. */
const ROLES = ['actor', 'executor', 'gateway', 'trace', 'fault', 'bad'] as const

const shades = (name: string) => ({
  DEFAULT: `var(--${name})`,
  fill: `var(--${name}-fill)`,
  soft: `var(--${name}-soft)`,
  softer: `var(--${name}-softer)`,
  term: `var(--${name}-term)`,
})

/** Every color is a custom property: src/palette.ts and src/style.css set them for both themes. */
const colors: Record<string, string | Record<string, string>> = {
  ...Object.fromEntries(ROLES.map(role => [role, shades(role)])),
  'paper': 'var(--paper)',
  'surface': 'var(--surface)',
  'control': { DEFAULT: 'var(--control)', 2: 'var(--control-2)', 3: 'var(--control-3)' },
  'ink': { DEFAULT: 'var(--ink)', 2: 'var(--ink-2)' },
  'muted': 'var(--muted)',
  'faint': 'var(--faint)',
  'term': { DEFAULT: 'var(--term)', ink: 'var(--term-ink)', raised: 'var(--term-raised)' },
  'band': 'var(--band)',
  'line': { DEFAULT: 'var(--line)', strong: 'var(--line-strong)' },
  'tip': { bg: 'var(--tip-bg)', ink: 'var(--tip-ink)', muted: 'var(--tip-muted)' },
  'code': {
    key: 'var(--code-key)',
    string: 'var(--code-string)',
    number: 'var(--code-number)',
    keyword: 'var(--code-keyword)',
    comment: 'var(--code-comment)',
    punct: 'var(--code-punct)',
    text: 'var(--code-text)',
  },
  // Set per element in an inline style, for a part whose role its props decide.
  'role': { DEFAULT: 'var(--role)', soft: 'var(--role-soft)', fill: 'var(--role-fill)' },
  'tone': 'var(--tone)',
}

/** The theme color `name` (`actor`, `actor-soft`, `control-2`), or undefined. */
function color(name: string): string | undefined {
  const flat = colors[name]
  if (typeof flat === 'string')
    return flat
  if (flat)
    return flat.DEFAULT
  const cut = name.lastIndexOf('-')
  const group = colors[name.slice(0, cut)]
  return typeof group === 'object' ? group[name.slice(cut + 1)] : undefined
}

const PROPERTIES: Record<string, string> = {
  bg: 'background-color',
  text: 'color',
  border: 'border-color',
  outline: 'outline-color',
  decoration: 'text-decoration-color',
  fill: 'fill',
  stroke: 'stroke',
}

/**
 * The page's keyframes, by what they do; utilities run them with their own timing (`animate-[rise-4_0.4s_both]`).
 * They live here because a scoped `<style>` renames keyframes and a utility could not name them.
 */
const KEYFRAMES: Record<string, string> = {
  // An entrance: from transparent and a few pixels off.
  'rise-4': 'from{opacity:0;transform:translateY(4px)}',
  'rise-6': 'from{opacity:0;transform:translateY(6px)}',
  'enter-right-10': 'from{opacity:0;transform:translateX(10px)}',
  'enter-right-16': 'from{opacity:0;transform:translateX(16px)}',
  'enter-left-4': 'from{opacity:0;transform:translateX(-4px)}',
  'enter-left-6': 'from{opacity:0;transform:translateX(-6px)}',
  'pop-up': 'from{opacity:0;translate:0 8px}',
  'form-in': 'from{opacity:0;filter:blur(6px);transform:scale(0.97)}',
  // A dashed wire that flows: one dash and gap of `stroke-dasharray: 1.5 6`, three times over.
  'dash-flow': 'to{stroke-dashoffset:-22}',
  'beat': '50%{opacity:0.3}',
  // A terminal line shown at its moment, and one typed out.
  'appear': 'from{visibility:hidden}to{visibility:visible}',
  'type-in': 'from{width:0}',
  // A packet along its wire's path: out, back, or out and staying.
  'packet-travel': '0%{offset-distance:0%;opacity:0}10%,86%{opacity:1}100%{offset-distance:100%;opacity:0}',
  'packet-travel-back': '0%{offset-distance:100%;opacity:0}10%,86%{opacity:1}100%{offset-distance:0%;opacity:0}',
  'packet-travel-keep': '0%{offset-distance:0%;opacity:0}10%,100%{opacity:1}100%{offset-distance:100%}',
}

export default defineConfig({
  presets: [
    // NOTICE: presetWind3's preflight is no reset (that is @unocss/reset, which the page does not use; its base
    // styles are src/style.css). It only defines the `--un-*` variables that `shadow-*`, `translate-*`, filters and
    // rings build on, and 'on-demand' defines those the page uses.
    presetWind3({ preflight: 'on-demand' }),
    presetIcons({
      // Like the inline SVG the page used before: the text's size, on the text's baseline.
      extraProperties: { 'display': 'inline-block', 'flex': 'none', 'vertical-align': '-0.125em' },
      // A square box, as the page's inline SVG had: a wide logo (Docker's) keeps its aspect inside it instead of
      // widening the box and moving the text after it.
      customizations: {
        customize(props) {
          props.width = '1em'
          props.height = '1em'
          return props
        },
      },
      collections: {
        // Project AIRI's Iconify port of @lobehub/icons: agent and model marks.
        'lobe-icons': () => require('@proj-airi/lobe-icons/icons.json'),
      },
    }),
  ],
  theme: {
    colors,
    fontFamily: {
      display: 'var(--font-display)',
      sans: 'var(--font-sans)',
      mono: 'var(--font-mono)',
    },
  },
  rules: [
    // NOTICE: presetWind3 drops the opacity of a color that is a custom property (`bg-actor/12` gives the color
    // whole), and every color here is one. This mixes it with transparent instead, as the page's CSS did.
    [/^(bg|text|border|outline|decoration|fill|stroke)-([a-z0-9-]+)\/(\d+)$/, ([, property, name, alpha]) => {
      const value = color(name!)
      return value ? { [PROPERTIES[property!]!]: `color-mix(in srgb, ${value} ${alpha}%, transparent)` } : undefined
    }],
  ],
  preflights: [{ getCSS: () => Object.entries(KEYFRAMES).map(([name, body]) => `@keyframes ${name}{${body}}`).join('\n') }],
  // Icons named at run time (`<Icon :name="...">`) are not in any class attribute, so every icon of src/icons.ts is
  // generated.
  safelist: ICON_NAMES.map(name => `i-${name}`),
  transformers: [transformerDirectives(), transformerVariantGroup()],
})
