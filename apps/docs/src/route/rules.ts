// Where a command goes, from a profile's `commands:` rules.
//
// Source: https://github.com/needy-dev/needy/blob/1efa6c8d1ab6b2bde0db2a8cf808167758601aa7/crates/core/src/rules/mod.rs#L45-L93
// Every matching rule applies in order. A single-valued field (`to`, `deny`) takes the first rule that sets it, until a
// rule with `final: true`. A `deny` wins over a placement, and a command that no rule places runs in the actor.
//
// TODO(docs-playground): `when:` conditions, launcher resolvers and the subcommands of `sh -c` scripts are not ported:
// the pages show plain commands only. Port them from crates/core/src/rules/{when,launcher,shell}.rs when a scene or
// the playground needs one of them.

import type { Match } from './pattern'

import { compile, matches } from './pattern'

export interface Rule {
  match: string
  to?: string
  deny?: string
  final?: boolean
}

export type Outcome
  = | { kind: 'run', on: string, rule?: number }
    | { kind: 'deny', message: string, rule: number }

export interface Decision {
  outcome: Outcome
  /** The index of each rule that matched, in order. */
  matched: number[]
  /** What each matched rule caught, by index. */
  captures: Record<number, Match>
}

/** Splits a command line on whitespace. The pages type plain words; quoting is the shell's, and needy sees argv. */
export function argvOf(line: string): string[] {
  return line.split(/\s+/).filter(Boolean)
}

export function decide(rules: readonly Rule[], argv: readonly string[]): Decision {
  let deny: { message: string, rule: number } | undefined
  let placement: { on: string, rule: number } | undefined
  const matched: number[] = []
  const captures: Record<number, Match> = {}
  for (const [index, rule] of rules.entries()) {
    const m = matches(compile(rule.match), [...argv])
    if (!m)
      continue
    matched.push(index)
    captures[index] = m
    if (rule.deny !== undefined && !deny)
      deny = { message: rule.deny, rule: index }
    if (rule.to !== undefined && !placement)
      placement = { on: rule.to, rule: index }
    if (rule.final)
      break
  }
  const outcome: Outcome = deny
    ? { kind: 'deny', ...deny }
    : placement
      ? { kind: 'run', on: placement.on, rule: placement.rule }
      : { kind: 'run', on: 'actor' }
  return { outcome, matched, captures }
}
