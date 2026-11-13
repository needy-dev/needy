import type { Environment, Executor } from './guide'

import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

import { check } from '../studio/check'
import { actorDriver, configurationIssue, DEFAULT_ENVIRONMENT, guide, PROGRAMS } from './guide'

const hosts: Array<Partial<Environment> & { driver: string }> = [
  { os: 'linux', kvm: true, driver: 'linux-firecracker' },
  { os: 'linux', kvm: false, driver: 'linux-container' },
  { os: 'mac', macRuntime: 'vz', driver: 'macos-vz' },
  { os: 'mac', macRuntime: 'apple', driver: 'macos-apple-container' },
]
const executors: Executor[] = ['container', 'native', 'runner']

describe('environment wizard', () => {
  // The fixed walkthrough removed the environment answers. Previously the
  // agent appendix also kept Firecracker even after a different driver was chosen.
  for (const host of hosts) {
    for (const executor of executors) {
      it.each(PROGRAMS)(`${host.driver}, ${executor}: keeps $name and the selected environment throughout`, ({ id }) => {
        const env = { ...DEFAULT_ENVIRONMENT, ...host, executor, runnerName: 'workstation', workspace: '~/my project' }
        const steps = guide(id, env)
        const saved = steps.find(step => step.profile)!
        const profile = parse(saved.profile!)
        const target = executor === 'runner' ? 'remote' : executor === 'native' ? 'here' : 'build'

        expect(actorDriver(env)).toBe(host.driver)
        expect(profile.actor.command).toEqual([id])
        expect(profile.actor.driver).toBe(host.driver)
        expect(profile.actor.container).toEqual(host.driver === 'linux-container' ? { oci_runtime: 'runc' } : undefined)
        expect(profile.executors).toEqual({ [target]: executor === 'runner'
          ? { runner: 'workstation', driver: 'native' }
          : { driver: executor === 'container' ? 'linux-container' : 'native' } })
        expect(profile.actor.workspace.from).toBe(`executor:${target}:${executor === 'container' ? '~/workspace' : '~/my project'}`)
        expect(profile.actor.workspace.writable).toBe(true)
        expect(profile.commands).toEqual([{ match: '**', to: target }])
        expect(profile.unknown).toBe(target)
        expect(saved.machine).toBe('host')
        expect(saved.code).toContain(`profiles/${id}-demo.yaml`)
        expect(steps.find(step => step.id === 'start')?.code).toContain(`--profile ${id}-demo`)
        expect(steps.find(step => step.id === 'install')?.code).toContain(`needy image pull --driver ${host.driver}`)
        expect(check(saved.profile!).findings.filter(f => f.severity === 'error')).toEqual([])
        expect(steps.some(step => step.id === 'tunnel')).toBe(executor === 'runner')
        if (id !== 'bash') {
          expect(steps.find(step => step.id === 'state')?.code).toContain(`~/needy-state/${id}-demo`)
          expect(saved.profile).not.toContain(`${id}-mac`)
        }
      })
    }
  }

  it('keeps local management, hidden enrollment, and the single tunnel in their terminal contexts', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner' })
    expect(steps.find(step => step.id === 'paste-token')?.code).toContain('needy --address tcp://127.0.0.1:7400 runner join')
    expect(steps.find(step => step.id === 'runner')?.code).toBe('needy runner serve')
    expect(steps.find(step => step.id === 'runner')?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'token')?.machine).toBe('host')
    expect(steps.filter(step => step.code?.includes('ssh -N'))).toHaveLength(1)
    const control = steps.find(step => step.id === 'daemon')!
    expect(control.terminal.zh).toContain('同一个 SSH 配置终端')
    expect(control.code).toContain('needy config set daemon.listen')
    expect(control.code).toContain('needy daemon service install --now')
    expect(control.code).not.toContain('NEEDY_TOKEN')
    expect(control.code).not.toContain('NEEDY_ENDPOINT')
  })

  it('changes package and shell-only routing instructions with the answers', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, kvm: false, pm: 'dnf', routing: 'shells' })
    expect(steps.find(step => step.id === 'prepare')?.code).toContain('sudo dnf install -y fuse-sshfs')
    const profile = parse(steps.find(step => step.profile)!.profile!)
    expect(profile.commands).toEqual([{ match: '{bash,sh,dash,zsh} **', to: 'build' }])
    expect(profile.unknown).toBe('actor')
  })

  it('prepares the container executor image separately from a VM actor image', () => {
    const steps = guide('bash')
    expect(steps.find(step => step.id === 'install')?.code).toContain('ghcr.io/needy-dev/needy-executor:v${needy_release}')
  })

  it('uses PowerShell on a Windows runner and checks its Bash prerequisite', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', runnerOs: 'windows' })
    expect(steps.find(step => step.id === 'runner-install')?.code).toContain('bash --version')
    expect(steps.find(step => step.id === 'runner-install')?.code).toContain('New-Item')
    expect(steps.find(step => step.id === 'paste-token')?.code).toContain('runner join')
    expect(steps.find(step => step.id === 'paste-token')?.code).not.toContain('PASTE_JOIN_TOKEN')
    expect(steps.find(step => step.id === 'check-runner')?.code).toContain('cmd /c ver')
  })

  it('quotes user project paths without executing shell substitutions', () => {
    const workspace = "~/a 'project' $(touch unwanted)"
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'native', workspace })
    expect(steps.find(step => step.id === 'workspace')?.code).toBe('mkdir -p "$HOME"/\'a \'\\\'\'project\'\\\'\' $(touch unwanted)\'')
    expect(parse(steps.find(step => step.profile)!.profile!).actor.workspace.from).toBe(`executor:here:${workspace}`)
  })

  it('shows invalid answers instead of silently replacing them', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, runnerName: 'bad name' }
    expect(configurationIssue(env)).toBeDefined()
    expect(guide('bash', env)).toEqual([])
    expect(configurationIssue({ ...env, runnerName: 'valid', workspace: '' })).toBeDefined()
  })
})
