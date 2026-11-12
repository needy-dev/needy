// Command patterns, written like the commands they match: `git **`, `git commit *`, `{node,bun} {script} **`.
//
// Source: https://github.com/needy-dev/needy/blob/1efa6c8d1ab6b2bde0db2a8cf808167758601aa7/crates/core/src/pattern.rs
// NOTICE: this is a port of needy_core::pattern, so the playground and the scenes decide where a command goes the way
// the daemon does. pattern.test.ts repeats the cases of crates/core/src/pattern_test.rs; when pattern.rs changes,
// change both. The Rust side matches each word with the `glob_match` crate; picomatch with `dot: true` matches the
// same words for the globs these pages use (`*`, `?`, `[...]`, `{a,b}` inside a word).

import picomatch from 'picomatch'

type Token
  = | { kind: 'literal', word: string }
    | { kind: 'one' }
    | { kind: 'many' }
    | { kind: 'alternatives', words: string[] }
    | { kind: 'capture', name: string }

/** What a pattern matched: the arguments of `{name}` tokens, and the arguments of the last `**`. */
export interface Match {
  captures: Record<string, string>
  rest: string[]
}

export interface Pattern {
  text: string
  tokens: Token[]
}

export function compile(text: string): Pattern {
  const tokens = text.split(/\s+/).filter(Boolean).map((word): Token => {
    if (word === '*')
      return { kind: 'one' }
    if (word === '**')
      return { kind: 'many' }
    const inner = word.startsWith('{') && word.endsWith('}') ? word.slice(1, -1) : undefined
    if (inner !== undefined && inner.includes(','))
      return { kind: 'alternatives', words: inner.split(',') }
    if (inner && /^\w+$/.test(inner))
      return { kind: 'capture', name: inner }
    return { kind: 'literal', word }
  })
  return { text, tokens }
}

/** The program name of a command word: its last path component. */
export function basename(word: string): string {
  const parts = word.split('/').filter(Boolean)
  return parts.at(-1) ?? word
}

function wordMatches(pattern: string, arg: string, first: boolean): boolean {
  const subject = first && !pattern.includes('/') ? basename(arg) : arg
  return picomatch.isMatch(subject, pattern, { dot: true })
}

function matchTokens(tokens: Token[], argv: string[], first: boolean, m: Match): Match | undefined {
  const [token, ...others] = tokens
  if (!token)
    return argv.length === 0 ? m : undefined
  if (token.kind === 'many') {
    for (let take = 0; take <= argv.length; take++) {
      const found = matchTokens(others, argv.slice(take), false, { captures: { ...m.captures }, rest: argv.slice(0, take) })
      if (found)
        return found
    }
    return undefined
  }
  const [arg, ...after] = argv
  if (arg === undefined)
    return undefined
  let ok = true
  const next = { captures: { ...m.captures }, rest: m.rest }
  switch (token.kind) {
    case 'literal':
      ok = wordMatches(token.word, arg, first)
      break
    case 'alternatives':
      ok = token.words.some(word => wordMatches(word, arg, first))
      break
    case 'capture':
      next.captures[token.name] = arg
      break
    case 'one':
      break
  }
  return ok ? matchTokens(others, after, false, next) : undefined
}

/** Matches a whole argument vector. */
export function matches(pattern: Pattern, argv: string[]): Match | undefined {
  return matchTokens(pattern.tokens, argv, true, { captures: {}, rest: [] })
}
