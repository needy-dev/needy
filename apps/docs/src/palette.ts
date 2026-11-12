import type { Shade } from '@proj-airi/chromatic'

import { flavors } from '@catppuccin/palette'
import { chromaticColorFrom } from '@proj-airi/chromatic'

// The colors of the page, after the injeca Docs: @proj-airi/chromatic, the palette of Project AIRI, gives one hue for
// each role and a shade for each use. The neutrals are plain greys with no hue. The build turns this file into CSS
// custom properties.
// Source: https://github.com/moeru-ai/injeca/blob/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs/src/palette.ts

/**
 * The hue of each role. Blue is the actor, green an executor, violet the gateway, cyan the trace, orange a fault and
 * red a denial or a failure. The control host and the daemon stay ink.
 */
const ROLES = { actor: 255, executor: 160, gateway: 300, trace: 210, fault: 45, bad: 15 } as const

const hex = (hue: number, shade: Shade) => chromaticColorFrom(hue, { shade }).toHex()
const alpha = (hue: number, shade: Shade, value: number) => chromaticColorFrom(hue, { shade }).withAlpha(value).toCSS()

/**
 * Code and terminals use Catppuccin: Latte for code on the light theme, Mocha for code on the dark theme and for every
 * terminal, which is dark in both themes.
 * Source: @catppuccin/palette 1.8.0, https://github.com/catppuccin/palette/blob/v1.8.0/palette.json
 */
const LATTE = flavors.latte.colors
const MOCHA = flavors.mocha.colors

/** Each role's color in a terminal: the Catppuccin color closest to its hue. */
const TERM: Record<string, keyof typeof MOCHA> = { actor: 'blue', executor: 'green', gateway: 'mauve', trace: 'sky', fault: 'peach', bad: 'red' }

/** `a` mixed toward `b` by `t` (0 is `a`), in sRGB. */
function mix(a: string, b: string, t: number): string {
  const channel = (h: string, i: number) => Number.parseInt(h.slice(i, i + 2), 16)
  return `#${[1, 3, 5].map(i => Math.round(channel(a, i) * (1 - t) + channel(b, i) * t).toString(16).padStart(2, '0')).join('')}`
}

/**
 * A Catppuccin color that reads on `grounds`: as it is when it reaches 4.5, otherwise mixed toward the flavor's own
 * text color, a step at a time, until it does.
 * NOTICE: Latte's green, peach and overlay2 fall under 4.5 on white (3.3, 3.0 and 4.0), so on the light theme they
 * are darkened this way; the hue stays Catppuccin's. Mocha reaches it everywhere as it is.
 */
function legible(color: string, text: string, grounds: readonly string[]): string {
  for (let t = 0; t <= 1; t += 0.05) {
    const candidate = mix(color, text, t)
    if (grounds.every(g => contrast(candidate, g) >= 4.5))
      return candidate
  }
  return text
}

/** The tokens of highlighted code, by flavor, legible on the theme's code grounds. */
export function code(theme: 'light' | 'dark'): Record<string, string> {
  const c = theme === 'light' ? LATTE : MOCHA
  const n = NEUTRALS[theme]
  const grounds = [n.paper, n.surface, n.control]
  const read = (color: string) => legible(color, c.text.hex, grounds)
  return {
    'code-key': read(c.blue.hex),
    'code-string': read(c.green.hex),
    'code-number': read(c.peach.hex),
    'code-keyword': read(c.mauve.hex),
    'code-comment': read(c.overlay2.hex),
    'code-punct': read(c.overlay2.hex),
    'code-text': c.text.hex,
  }
}

/**
 * The neutrals, by role: `paper` is the page and the ground of a scene, `surface` a box on it, `control` a chip or a
 * row on a box, and `ink` down to `faint` the text.
 */
export const NEUTRALS = {
  light: { 'paper': '#f6f6f6', 'surface': '#ffffff', 'control': '#ececec', 'control-2': '#e2e2e2', 'control-3': '#d4d4d4', 'ink': '#171717', 'ink-2': '#3d3d3d', 'muted': '#5a5a5a', 'faint': '#636363', 'term': MOCHA.base.hex, 'term-ink': MOCHA.text.hex },
  dark: { 'paper': '#0d0d0d', 'surface': '#171717', 'control': '#232323', 'control-2': '#2d2d2d', 'control-3': '#3b3b3b', 'ink': '#ededed', 'ink-2': '#c8c8c8', 'muted': '#9e9e9e', 'faint': '#949494', 'term': MOCHA.crust.hex, 'term-ink': MOCHA.text.hex },
} as const

/** The relative luminance of a color, after WCAG 2. */
function luminance(color: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = Number.parseInt(color.slice(i, i + 2), 16) / 255
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}

/** The contrast ratio of two colors, after WCAG 2: from 1 to 21. Body text needs 4.5. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi! + 0.05) / (lo! + 0.05)
}

/** The first shade in `shades` whose color reads at 4.5 or more on each of `against`. */
function readable(hue: number, shades: readonly Shade[], against: readonly string[]): Shade {
  return shades.find(shade => against.every(ground => contrast(hex(hue, shade), ground) >= 4.5)) ?? shades.at(-1)!
}

/**
 * The tokens of one role: `<name>` for text and lines, `<name>-fill` for a solid fill, `<name>-soft` and
 * `<name>-softer` for grounds, and `<name>-term` for text on the terminal.
 */
export function accent(name: string, hue: number, theme: 'light' | 'dark'): Record<string, string> {
  const t = NEUTRALS[theme]
  const term = MOCHA[TERM[name] ?? 'text'].hex
  if (theme === 'light') {
    return {
      [name]: hex(hue, readable(hue, [600, 700, 800], [t.paper, t.surface, t.control])),
      [`${name}-fill`]: hex(hue, readable(hue, [500, 600, 700, 800], ['#ffffff'])),
      [`${name}-soft`]: hex(hue, 100),
      [`${name}-softer`]: hex(hue, 50),
      [`${name}-term`]: term,
    }
  }
  return {
    [name]: hex(hue, readable(hue, [300, 200, 100], [t.paper, t.surface, t.control])),
    // White text sits on a fill in both themes, so the fill is the first shade that carries it.
    [`${name}-fill`]: hex(hue, readable(hue, [600, 700, 800, 900], ['#ffffff'])),
    [`${name}-soft`]: alpha(hue, 800, 0.45),
    [`${name}-softer`]: alpha(hue, 900, 0.35),
    [`${name}-term`]: term,
  }
}

function block(selector: string, values: Record<string, string>): string {
  return `${selector} {\n${Object.entries(values).map(([k, v]) => `  --${k}: ${v};`).join('\n')}\n}`
}

export function paletteCss(): string {
  const roles = (theme: 'light' | 'dark') => Object.assign({}, ...Object.entries(ROLES).map(([name, hue]) => accent(name, hue, theme)))
  return [
    block(':root', { ...NEUTRALS.light, ...roles('light'), ...code('light') }),
    block(':root[data-theme=\'dark\']', { ...NEUTRALS.dark, ...roles('dark'), ...code('dark') }),
  ].join('\n\n')
}
