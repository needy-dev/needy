import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { check } from './check'

const examples = join(__dirname, '..', '..', '..', '..', 'examples')

describe('check', () => {
  // Every example profile loads in needy, so the studio must find nothing wrong with it either: each
  // `examples/*/profile.yaml`, and each agent profile in `examples/agents` (none of them `extends` another).
  const profiles = [
    ...readdirSync(examples).filter(dir => existsSync(join(examples, dir, 'profile.yaml'))).map(dir => `${dir}/profile.yaml`),
    ...readdirSync(join(examples, 'agents')).filter(file => file.endsWith('.yaml')).map(file => `agents/${file}`),
  ]
  for (const path of profiles) {
    it(`accepts examples/${path}`, () => {
      const text = readFileSync(join(examples, path), 'utf8')
      expect(check(text).findings).toEqual([])
    })
  }

  it('rejects an unknown key, as deny_unknown_fields does', () => {
    const { findings } = check('actor:\n  driver: macos-vz\n  drivr: x\n')
    expect(findings.map(f => f.message)).toEqual([expect.stringContaining('unknown key `actor.drivr`')])
  })

  it('rejects a driver that does not exist and offers the ones that do', () => {
    const { findings, choices } = check('actor:\n  driver: qemu\n')
    expect(findings[0]!.message).toContain('it takes linux-firecracker, linux-container, macos-vz, macos-apple-container')
    expect(choices[0]).toMatchObject({ path: 'actor.driver', value: 'qemu' })
  })

  it('checks that a rule sends to a known executor', () => {
    const { findings } = check('actor: { driver: macos-vz }\ncommands:\n  - { match: "python3 **", to: build }\n')
    expect(findings.map(f => f.message)).toEqual(['commands[0] (python3 **) sends to unknown executor "build"'])
  })

  it('asks a linux-container actor for its OCI runtime', () => {
    const { findings } = check('actor: { driver: linux-container }\n')
    expect(findings[0]!.message).toContain('needs `container: { oci_runtime: runc }`')
  })

  it('reads chaos.faults as named faults and refuses keys from another domain', () => {
    const { findings } = check('actor: { driver: macos-vz }\nchaos:\n  faults:\n    curl-down: { exec: "curl **", errno: EIO }\n')
    expect(findings.map(f => f.message)).toEqual([expect.stringContaining('chaos.faults.curl-down: errno is not for exec faults')])
  })

  it('refuses a fault that names two domains', () => {
    const { findings } = check('actor: { driver: macos-vz }\nchaos:\n  faults:\n    x: { exec: "**", net: { connect: example.com }, hang: true }\n')
    expect(findings.map(f => f.message)).toEqual(['chaos.faults.x: a fault names one domain, not exec and net'])
  })

  it('checks a service\'s name and URL', () => {
    const { findings } = check('actor:\n  driver: macos-vz\n  network:\n    services:\n      Models: { url: "http://192.0.2.1:8080" }\n      llm: { url: "ftp://192.0.2.1" }\n')
    expect(findings.map(f => f.message)).toEqual([
      'actor.network.services.Models: a service name is a DNS label: lowercase letters, digits and `-`, at most 63',
      'actor.network.services.llm: url "ftp://192.0.2.1" must be an absolute http:// or https:// URL',
    ])
  })

  it('checks that actor.workspace mounts from a known executor', () => {
    const { findings } = check('actor:\n  driver: macos-vz\n  workspace: { path: ~/workspace, from: "executor:build:~/workspace" }\n')
    expect(findings.map(f => f.message)).toEqual(['actor.workspace.from: names unknown executor "build"'])
  })

  it('refuses actor.cwd, which actor.workspace replaced', () => {
    const { findings } = check('actor: { driver: macos-vz, cwd: /tmp }\n')
    expect(findings.map(f => f.message)).toEqual([expect.stringContaining('unknown key `actor.cwd`')])
  })
})
