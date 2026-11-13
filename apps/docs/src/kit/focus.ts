/**
 * How a thing on a stage stands at a step. The focus language: what the step is about lifts and casts a shadow in
 * the color of its role; the rest steps back (dim). A denial or a failure shakes once and casts red. Nothing gets a
 * colored outline.
 */
export type Focus = 'idle' | 'focus' | 'dim' | 'bad' | 'gone'

/** The focus of `id` at a step: `focus` when it is one of `on`, `dim` when something else is, else `idle`. */
export function focusOf(id: string, on: readonly string[], bad: readonly string[] = []): Focus {
  if (bad.includes(id))
    return 'bad'
  if (on.includes(id))
    return 'focus'
  return on.length + bad.length > 0 ? 'dim' : 'idle'
}
