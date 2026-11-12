// A probe of the page's contrast: every text color against every ground it is drawn on, in both themes, after WCAG 2
// (4.5 for body text). The grounds include the band behind every other chapter and the tooltip. Values that only
// style.css sets (the band, the tooltip) are repeated here; change both together.

import { describe, expect, it } from 'vitest'

import { accent, code, contrast, NEUTRALS } from './palette'

const ROLES = { actor: 255, executor: 160, gateway: 300, trace: 210, fault: 45, bad: 15 } as const
const BAND = { light: '#ffffff', dark: '#141414' } as const
const TIP = { light: { bg: '#171717', ink: '#ededed' }, dark: { bg: '#3b3b3b', ink: '#ededed' } } as const

for (const theme of ['light', 'dark'] as const) {
  const n = NEUTRALS[theme]
  const grounds = { paper: n.paper, surface: n.surface, control: n.control, band: BAND[theme] }
  const roles = Object.fromEntries(Object.entries(ROLES).map(([name, hue]) => [name, accent(name, hue, theme)]))

  describe(`contrast, ${theme}`, () => {
    for (const ink of ['ink', 'ink-2', 'muted', 'faint'] as const) {
      for (const [ground, color] of Object.entries(grounds)) {
        it(`${ink} on ${ground}`, () => expect(contrast(n[ink], color)).toBeGreaterThanOrEqual(4.5))
      }
    }
    for (const [name, tokens] of Object.entries(roles)) {
      for (const [ground, color] of Object.entries(grounds)) {
        it(`${name} on ${ground}`, () => expect(contrast(tokens[name]!, color)).toBeGreaterThanOrEqual(4.5))
      }
      it(`${name}-term on the terminal`, () => expect(contrast(tokens[`${name}-term`]!, n.term)).toBeGreaterThanOrEqual(4.5))
      it(`${name}-term on the tooltip`, () => expect(contrast(tokens[`${name}-term`]!, TIP[theme].bg)).toBeGreaterThanOrEqual(4.5))
      it(`white on ${name}-fill`, () => expect(contrast('#ffffff', tokens[`${name}-fill`]!)).toBeGreaterThanOrEqual(4.5))
    }
    for (const [token, color] of Object.entries(code(theme))) {
      for (const [ground, g] of Object.entries(grounds)) {
        it(`${token} on ${ground}`, () => expect(contrast(color, g)).toBeGreaterThanOrEqual(4.5))
      }
    }
    it('tooltip text', () => expect(contrast(TIP[theme].ink, TIP[theme].bg)).toBeGreaterThanOrEqual(4.5))
    it('terminal text', () => expect(contrast(n['term-ink'], n.term)).toBeGreaterThanOrEqual(4.5))
  })
}
