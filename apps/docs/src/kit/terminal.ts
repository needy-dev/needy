import type { Role } from './roles'

/** One line of a terminal: a command that types itself out, or output that appears. */
export interface Line {
  text: string
  kind?: 'cmd' | 'out' | 'err' | 'dim' | 'ok' | 'fault'
  /** The prompt of a command. The terminal's prompt when absent. */
  prompt?: string
  /** A note at the end of the line, such as where the command ran. */
  tag?: { text: string, role: Role }
}

/** The time a command types for, in ms: quick for a short one, never more than a second and a half. */
function typing(text: string): number {
  return Math.min(text.length * 34, 1500)
}

/**
 * When each new line shows, from the start of a step: the lines before `fresh` are already there, and the new ones
 * follow each other, a command once it has typed. `end` is when the last one is in.
 */
export function timeline(lines: readonly Line[], fresh: number): { at: number[], typed: number[], end: number } {
  const at: number[] = []
  const typed: number[] = []
  let t = 200
  for (const [i, line] of lines.entries()) {
    if (i < fresh) {
      at.push(0)
      typed.push(0)
      continue
    }
    at.push(t)
    if (line.kind === 'cmd') {
      const ms = typing(line.text)
      typed.push(ms)
      t += ms + 260
    }
    else {
      typed.push(0)
      t += 110
    }
  }
  return { at, typed, end: t }
}
