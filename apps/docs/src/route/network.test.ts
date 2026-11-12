import { describe, expect, it } from 'vitest'

import { allowedBy, matchesHost } from './network'

describe('network', () => {
  it('matches a wildcard pattern against the bare domain too, as Glob::matches_host does', () => {
    expect(matchesHost('*.models.dev', 'models.dev')).toBe(true)
    expect(matchesHost('*.models.dev', 'api.models.dev')).toBe(true)
    expect(matchesHost('*.models.dev', 'models.dev.example.com')).toBe(false)
    expect(matchesHost('Models.DEV', 'models.dev')).toBe(true)
  })

  it('allows a service by its host in the actor, in any case, and refuses the rest', () => {
    const policy = { allow: ['opencode.ai'], services: { model: 'http://127.0.0.1:11434' } }
    expect(allowedBy(policy, 'model.needy')).toEqual({ kind: 'service', name: 'model', url: 'http://127.0.0.1:11434' })
    expect(allowedBy(policy, 'Model.NEEDY')).toEqual({ kind: 'service', name: 'model', url: 'http://127.0.0.1:11434' })
    expect(allowedBy(policy, 'model')).toBeUndefined()
    expect(allowedBy(policy, 'example.com')).toBeUndefined()
  })
})
