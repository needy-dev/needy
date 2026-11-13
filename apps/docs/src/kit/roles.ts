/** What a box or a packet stands for. Each role has a hue in src/palette.ts; `host` stays ink. */
export type Role = 'actor' | 'executor' | 'gateway' | 'trace' | 'fault' | 'bad' | 'host'

/** The color of a role, as a CSS variable. */
export function color(role: Role, use: '' | 'fill' | 'soft' | 'softer' | 'term' = ''): string {
  if (role === 'host')
    return use === 'soft' || use === 'softer' ? 'var(--control)' : use === 'fill' ? 'var(--ink-2)' : 'var(--ink)'
  return `var(--${role}${use ? `-${use}` : ''})`
}
