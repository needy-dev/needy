import type { Environment, Executor } from './guide'

import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

import { check } from '../studio/check'
import { actorDriver, configurationIssue, DEFAULT_ENVIRONMENT, guide, locations, PROGRAMS } from './guide'

const hosts: Array<Partial<Environment> & { driver: string }> = [
  { os: 'linux', kvm: true, driver: 'linux-firecracker' },
  { os: 'linux', kvm: false, driver: 'linux-container' },
  { os: 'mac', macRuntime: 'vz', driver: 'macos-vz' },
  { os: 'mac', macRuntime: 'apple', driver: 'macos-apple-container' },
]
const executors: Executor[] = ['container', 'native', 'runner']

describe('environment wizard', () => {
  it('uses a private creating-client binding without persistent runner setup', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, launchFrom: 'runner' as const, runnerName: 'unused invalid name' }
    expect(configurationIssue(env)).toBeUndefined()
    const steps = guide('bash', env)
    const profile = parse(steps.find(step => step.profile)!.profile!)
    expect(profile.executors.remote).toEqual({ driver: 'native', runner: { client: true } })
    expect(steps.some(step => ['token', 'paste-token', 'runner', 'check-runner'].includes(step.id))).toBe(false)
    expect(steps.find(step => step.id === 'client-pair')?.text.en).toContain('invoking CLI account')
    expect(steps.find(step => step.id === 'workspace')?.text.en).toContain('run create')
    expect(steps.find(step => step.id === 'start')?.code).toBe('needy run create -it -f ~/.config/needy/profiles/bash-demo.yaml')
    expect(steps.find(step => step.id === 'start')?.code).not.toContain('profile check')
    expect(locations(env).runner.description.en).toContain('invoking CLI account')
    expect(locations(env).runner.description.en).not.toContain('runner serve')
  })

  it('keeps explicit persistent runner selection when creation starts on the control host', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, launchFrom: 'host' as const, runnerName: 'workstation' }
    const steps = guide('bash', env)
    expect(parse(steps.find(step => step.profile)!.profile!).executors.remote.runner).toBe('workstation')
    expect(steps.find(step => step.id === 'runner')?.code).toContain('runner serve')
    expect(steps.some(step => step.id === 'client-pair')).toBe(false)
    expect(locations(env).runner.description.en).toContain('runner serve')
  })

  it('blocks automatic Windows client execution at the canonical private ACL limitation', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, launchFrom: 'runner' as const, runnerOs: 'windows' as const }
    expect(configurationIssue(env)?.en).toContain('Windows automatic client execution')
    expect(configurationIssue(env)?.en).toContain('ACL')
    expect(guide('bash', env)).toEqual([])
  })

  it.each([
    ['apt', 'sudo apt-get install -y sshfs fuse3'],
    ['dnf', 'sudo dnf install -y fuse-sshfs fuse3'],
    ['pacman', 'sudo pacman -S --noconfirm sshfs fuse3'],
  ] as const)('installs mount helpers without diagnostic boilerplate on Linux container host with %s', (pm, install) => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, kvm: false, executor: 'runner', pm })
    const prepare = steps.find(step => step.id === 'prepare')!
    expect(prepare.machine).toBe('host')
    expect(prepare.code).toContain(install)
    expect(prepare.code).toContain('user_allow_other')
    expect(prepare.code).not.toMatch(/docker info|command -v|test -r|\bid\b/)
    expect(prepare.text.en).toContain('actor host')
    expect(steps.indexOf(prepare)).toBeLessThan(steps.findIndex(step => step.id === 'daemon'))
    expect(steps.find(step => step.id === 'runner-install')?.code).not.toContain('sshfs')
  })

  it('skips the prepare step when Linux Firecracker already has Docker and omits diagnostic checks', () => {
    expect(guide('bash', DEFAULT_ENVIRONMENT).some(step => step.id === 'prepare')).toBe(false)
    const noDocker = guide('bash', { ...DEFAULT_ENVIRONMENT, docker: false }).find(step => step.id === 'prepare')!
    expect(noDocker.code).toContain('https://get.docker.com')
    expect(noDocker.code).not.toMatch(/docker info|test -r \/dev\/kvm/)
  })

  it('omits prepare-sftp across all executor modes because run create reports missing sftp-server', () => {
    expect(guide('bash', { ...DEFAULT_ENVIRONMENT, os: 'mac', executor: 'runner', runnerOs: 'mac' }).some(step => step.id === 'prepare-sftp')).toBe(false)
    expect(guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', runnerOs: 'linux', pm: 'dnf' }).some(step => step.id === 'prepare-sftp')).toBe(false)
    expect(guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'native' }).some(step => step.id === 'prepare-sftp')).toBe(false)
    expect(guide('bash').some(step => step.id === 'prepare-sftp')).toBe(false)
  })

  it('uses clean workspace setup and runner ls without diagnostic shell probes', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner', launchFrom: 'host', workspace: '~/my project' })
    const workspace = steps.find(step => step.id === 'workspace')!
    expect(workspace.machine).toBe('runner')
    expect(workspace.code).toBe('mkdir -p "$HOME"/\'my project\' && cd "$HOME"/\'my project\'')
    expect(workspace.code).not.toMatch(/test -d|uname -s|\bid\b|printf/)
    const checkRunner = steps.find(step => step.id === 'check-runner')!
    expect(checkRunner.code).toBe('needy runner ls')
    expect(steps.indexOf(checkRunner)).toBeLessThan(steps.findIndex(step => step.id === 'start'))
  })

  it('includes client authorization and launches from the executor when selected', () => {
    const env = { ...DEFAULT_ENVIRONMENT, where: 'server' as const, executor: 'runner' as const, launchFrom: 'runner' as const }
    const steps = guide('bash', env)
    expect(steps.find(step => step.id === 'client-invite')?.machine).toBe('host')
    expect(steps.find(step => step.id === 'client-pair')?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'client-pair')?.code).toContain('config context pair control')
    expect(steps.find(step => step.profile)?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'start')?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'client-invite')?.code).not.toContain('--admin')
  })

  it('keeps control-host launches available without adding a client invitation', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', launchFrom: 'host' })
    expect(steps.find(step => step.profile)?.machine).toBe('host')
    expect(steps.find(step => step.id === 'start')?.machine).toBe('host')
    expect(steps.some(step => step.id === 'client-pair')).toBe(false)
  })

  it('creates quoted workspace paths without executing shell substitutions', () => {
    const root = mkdtempSync(join(tmpdir(), 'needy-guide-'))
    try {
      const executor = join(root, 'executor')
      mkdirSync(executor)
      const path = "~/my 'project' $(touch unwanted)"
      const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', launchFrom: 'host', workspace: path })
      const script = steps.find(step => step.id === 'workspace')!.code!
      const result = spawnSync('sh', ['-c', script], {
        cwd: root, env: { ...process.env, HOME: executor }, encoding: 'utf8',
      })
      expect(result.status).toBe(0)
      expect(existsSync(join(executor, path.slice(2)))).toBe(true)
      expect(existsSync(join(root, 'unwanted'))).toBe(false)
    }
    finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  // The fixed walkthrough removed the environment answers. Previously the
  // agent appendix also kept Firecracker even after a different driver was chosen.
  for (const host of hosts) {
    for (const executor of executors) {
      it.each(PROGRAMS)(`${host.driver}, ${executor}: keeps $name and the selected environment throughout`, ({ id }) => {
        const env = { ...DEFAULT_ENVIRONMENT, ...host, executor, runnerName: 'workstation', workspace: '~/my project' }
        const steps = guide(id, env)
        const target = executor === 'runner' ? 'remote' : executor === 'native' ? 'here' : 'build'
        const useBuiltin = id === 'opencode' && host.driver === 'linux-firecracker' && executor === 'container'

        expect(actorDriver(env)).toBe(host.driver)
        expect(steps.find(step => step.id === 'install')?.code).not.toContain('needy image pull')
        expect(steps.find(step => step.id === 'start')?.code).not.toContain('profile check')
        expect(steps.some(step => step.id === 'tunnel')).toBe(executor === 'runner')

        if (useBuiltin) {
          expect(steps.some(step => step.id === 'profile')).toBe(false)
          expect(steps.find(step => step.id === 'start')?.code).toBe('needy run create -it --profile builtin:opencode')
          return
        }

        const saved = steps.find(step => step.profile)!
        const profile = parse(saved.profile!)
        expect(profile.actor.driver).toBe(host.driver)
        expect(profile.actor.container).toEqual(host.driver === 'linux-container' ? { oci_runtime: 'runc' } : undefined)
        expect(saved.machine).toBe(executor === 'runner' ? 'runner' : 'host')
        expect(saved.code).toContain(`profiles/${id}-demo.yaml`)
        expect(steps.find(step => step.id === 'start')?.code).toBe(`needy run create -it -f ~/.config/needy/profiles/${id}-demo.yaml`)
        expect(check(saved.profile!).findings.filter(f => f.severity === 'error')).toEqual([])

        if (id === 'opencode') {
          expect(profile.extends).toBe('builtin:opencode')
          expect(profile.actor.command).toBeUndefined()
          expect(steps.some(step => step.id === 'state')).toBe(false)
          expect(saved.profile).not.toContain('needy-state')
          if (executor === 'container') {
            expect(profile.executors).toBeUndefined()
            expect(profile.actor.workspace).toBeUndefined()
          }
          else {
            expect(profile.executors).toEqual({ [target]: executor === 'runner'
              ? { runner: { client: true }, driver: 'native' }
              : { driver: 'native' } })
            expect(profile.actor.workspace.from).toBe(`executor:${target}:~/my project`)
            expect(profile.actor.workspace.writable).toBe(true)
            expect(profile.commands).toEqual([{ match: '**', to: target }])
            expect(profile.unknown).toBe(target)
          }
        }
        else {
          expect(profile.actor.command).toEqual([id])
          expect(profile.executors).toEqual({ [target]: executor === 'runner'
            ? { runner: { client: true }, driver: 'native' }
            : { driver: executor === 'container' ? 'linux-container' : 'native' } })
          expect(profile.actor.workspace.from).toBe(`executor:${target}:${executor === 'container' ? '~/workspace' : '~/my project'}`)
          expect(profile.actor.workspace.writable).toBe(true)
          expect(profile.commands).toEqual([{ match: '**', to: target }])
          expect(profile.unknown).toBe(target)
        }
      })
    }
  }

  it('mentions Cloudflare Tunnel as a non-SSH option in the tunnel step', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner' })
    const tunnel = steps.find(step => step.id === 'tunnel')!
    expect(tunnel.code).toBe('ssh -N -o ExitOnForwardFailure=yes -L 7400:127.0.0.1:7400 user@example.com')
    expect(tunnel.text.en).toContain('cloudflared access tcp --hostname needy.example.com --url 127.0.0.1:7400')
    expect(tunnel.text.en).toContain('https://needy.example.com')
    expect(tunnel.text.en).toContain('http2Origin: true')
    expect(tunnel.text.zh).toContain('cloudflared access tcp --hostname needy.example.com --url 127.0.0.1:7400')
  })

  it('generalizes the login step instead of presenting /connect as the only login flow', () => {
    const steps = guide('opencode', DEFAULT_ENVIRONMENT)
    const login = steps.find(step => step.id === 'login')!
    expect(login.code).toBeUndefined()
    expect(login.text.en).toContain('interactive login console')
    expect(login.text.en).toContain('/connect')
    expect(login.text.zh).toContain('交互式登录控制台')
    expect(login.text.zh).toContain('/connect')
  })

  it('keeps local management, hidden enrollment, and the single tunnel in their terminal contexts', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner', launchFrom: 'host' })
    expect(steps.find(step => step.id === 'paste-token')?.code).toContain('needy --address tcp://127.0.0.1:7400 runner join')
    expect(steps.find(step => step.id === 'runner')?.code).toContain('needy runner serve')
    expect(steps.find(step => step.id === 'runner')?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'token')?.machine).toBe('host')
    expect(steps.filter(step => step.code?.includes('ssh -N'))).toHaveLength(1)
    const control = steps.find(step => step.id === 'daemon')!
    expect(control.terminal.zh).toContain('控制主机 · SSH 终端')
    expect(control.code).toContain('needy config set daemon.listen')
    // The install script already started the service; the listener needs a replacement.
    expect(control.code).toContain('needy daemon service install --replace --now')
    expect(control.code).toContain('needy config use local')
    expect(control.text.en).toContain('needy daemon service install --now')
    expect(steps.find(step => step.id === 'install')?.code).toContain('install.sh | sh')
    expect(steps.find(step => step.id === 'runner-install')?.code).toContain('install.sh | NEEDY_NO_DAEMON=1 sh')
    expect(steps.find(step => step.id === 'token')?.text.en).toContain('client context')
    expect(control.code).not.toContain('NEEDY_TOKEN')
    expect(control.code).not.toContain('NEEDY_ENDPOINT')
  })

  // A second plain install of the service the install script started would be
  // refused as already installed, so the step only checks it.
  it('checks the daemon the install script started without installing it again', () => {
    const control = guide('bash', DEFAULT_ENVIRONMENT).find(step => step.id === 'daemon')!
    expect(control.code).toContain('needy daemon service status')
    expect(control.code).not.toContain('needy daemon service install')
  })

  it('changes package and shell-only routing instructions with the answers', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, kvm: false, pm: 'dnf', routing: 'shells' })
    expect(steps.find(step => step.id === 'prepare')?.code).toContain('sudo dnf install -y fuse-sshfs')
    const profile = parse(steps.find(step => step.profile)!.profile!)
    expect(profile.commands).toEqual([{ match: '{bash,sh,dash,zsh} **', to: 'build' }])
    expect(profile.unknown).toBe('actor')
  })

  it('omits manual image pulls because local runs pull release base images automatically', () => {
    const steps = guide('bash')
    const install = steps.find(step => step.id === 'install')!
    expect(install.code).not.toContain('needy image pull')
    expect(install.code).not.toContain('docker pull')
    expect(install.text.en).toContain('automatically')
  })

  // Windows enrollment cannot yet persist private credentials. A complete
  // generated walkthrough incorrectly promised it could reach an online runner.
  it('explains the Windows enrollment limit without generating setup commands', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, launchFrom: 'host' as const, runnerOs: 'windows' as const }
    expect(configurationIssue(env)?.en).toContain('Windows runner enrollment is not supported yet')
    expect(guide('bash', env)).toEqual([])
  })

  it('quotes user project paths without executing shell substitutions', () => {
    const workspace = "~/a 'project' $(touch unwanted)"
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'native', workspace })
    expect(steps.find(step => step.id === 'workspace')?.code).toBe('mkdir -p "$HOME"/\'a \'\\\'\'project\'\\\'\' $(touch unwanted)\' && cd "$HOME"/\'a \'\\\'\'project\'\\\'\' $(touch unwanted)\'')
    expect(parse(steps.find(step => step.profile)!.profile!).actor.workspace.from).toBe(`executor:here:${workspace}`)
  })

  it('shows invalid answers instead of silently replacing them', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, launchFrom: 'host' as const, runnerName: 'bad name' }
    expect(configurationIssue(env)).toBeDefined()
    expect(guide('bash', env)).toEqual([])
    expect(configurationIssue({ ...env, runnerName: 'valid', workspace: '' })).toBeDefined()
    const relativeIssue = configurationIssue({ ...env, runnerName: 'valid', workspace: '.' })
    expect(relativeIssue).toBeDefined()
    expect(relativeIssue?.en).not.toContain('not bound automatically')
    expect(relativeIssue?.en).toContain('omit actor.workspace')
  })
})
