import { describe, expect, it } from 'vitest'

import { argvOf, decide } from './rules'
import { ROUTING_RULES } from './samples'

const where = (line: string) => decide(ROUTING_RULES, argvOf(line)).outcome

describe('rules', () => {
  it('places a command with the first rule that sets `to`', () => {
    expect(where('python3 -m pytest -q')).toEqual({ kind: 'run', on: 'build', rule: 0 })
    expect(where('swift build')).toEqual({ kind: 'run', on: 'mac', rule: 2 })
  })

  it('denies a command that any matching rule denies, even after a placement', () => {
    const outcome = decide([{ match: 'git **', to: 'build' }, { match: 'git push **', deny: 'no pushes' }], argvOf('git push origin main')).outcome
    expect(outcome).toEqual({ kind: 'deny', message: 'no pushes', rule: 1 })
  })

  it('matches the basename of an absolute path', () => {
    expect(where('/usr/bin/git push origin main').kind).toBe('deny')
  })

  it('stops at a final rule', () => {
    const rules = [{ match: 'git **', to: 'build', final: true }, { match: 'git push **', deny: 'no pushes' }]
    expect(decide(rules, argvOf('git push')).outcome).toEqual({ kind: 'run', on: 'build', rule: 0 })
  })

  it('keeps a command that no rule places in the actor', () => {
    expect(where('ls -la')).toEqual({ kind: 'run', on: 'actor' })
  })
})
