import type { Environment, Executor } from './guide'

import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

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
  // ROOT CAUSE:
  //
  // If an existing installation lacked fusermount3 in PATH, setup did not
  // detect it before the non-root daemon tried to detach a workspace mount.
  // The guide must locate installation and checks on the mounting host.
  it.each([
    ['apt', 'sudo apt-get install -y sshfs fuse3'],
    ['dnf', 'sudo dnf install -y fuse-sshfs fuse3'],
    ['pacman', 'sudo pacman -S --noconfirm sshfs fuse3'],
  ] as const)('prepares both mount helpers on the Linux container actor host with %s', (pm, install) => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, kvm: false, executor: 'runner', pm })
    const prepare = steps.find(step => step.id === 'prepare')!
    expect(prepare.machine).toBe('host')
    expect(prepare.code).toContain(install)
    expect(prepare.code).toContain('command -v sshfs && command -v fusermount3')
    expect(prepare.code).toContain('user_allow_other')
    expect(prepare.code).toContain('test -r /dev/fuse && test -w /dev/fuse')
    expect(prepare.text.en).toContain('daemon account')
    expect(prepare.text.en).toContain('actor host')
    expect(steps.indexOf(prepare)).toBeLessThan(steps.findIndex(step => step.id === 'daemon'))
    expect(steps.find(step => step.id === 'runner-install')?.code).not.toContain('sshfs')
  })

  it('keeps FUSE inside VM images and checks the Mac SFTP server as the executor account', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, os: 'mac', executor: 'runner', runnerOs: 'mac' })
    expect(steps.find(step => step.id === 'prepare')?.code).not.toMatch(/sshfs|fusermount3|fuse\.conf/)
    const install = steps.find(step => step.id === 'runner-install')!
    expect(install.text.en).toContain('macFUSE')
    const workspace = steps.find(step => step.id === 'workspace')!
    expect(workspace.machine).toBe('runner')
    expect(workspace.code).toContain('test -x /usr/libexec/sftp-server || exit 1')
    expect(workspace.text.en).toContain('same account')
  })

  it('checks Linux native SFTP on the executor without assuming the actor host package manager', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', runnerOs: 'linux', pm: 'dnf' })
    const install = steps.find(step => step.id === 'runner-install')!
    expect(install.text.en).toContain('openssh-sftp-server')
    expect(install.code).not.toContain('dnf')
    expect(steps.find(step => step.id === 'workspace')?.code).toContain('test -x /usr/lib/openssh/sftp-server || exit 1')
  })

  // ROOT CAUSE:
  //
  // If installation was reused, workspace preparation was easy to skip
  // because it was part of the install step; readiness checked only uname.
  // Before the fix, the guide offered no serving-account directory probe.
  // The fix verifies the source directory before the actor mounts it.
  it('verifies the remote workspace through the serving runner before mounting it', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner', workspace: '~/my project' })
    const workspace = steps.find(step => step.id === 'workspace')
    expect(workspace?.machine).toBe('runner')
    expect(workspace?.code).toContain('test -d')
    const probe = steps.find(step => step.id === 'check-runner')!
    expect(probe.code).toContain('test -d')
    expect(probe.code).toContain('id')
    expect(steps.indexOf(probe)).toBeLessThan(steps.findIndex(step => step.id === 'start'))
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

  // The two shells deliberately have different HOME values. Expanding the
  // runner path in the control shell would inspect the wrong machine's home.
  it('fails a missing-directory probe and expands quoted paths only on the executor', () => {
    const root = mkdtempSync(join(tmpdir(), 'needy-guide-'))
    try {
      const control = join(root, 'control')
      const executor = join(root, 'executor')
      mkdirSync(control)
      mkdirSync(executor)
      const path = "~/my 'project' $(touch unwanted)"
      const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', workspace: path })
      const probe = steps.find(step => step.id === 'check-runner')!.code!
      // Replace only the external execution boundary. The generated shell
      // quoting and the executor's sh -c body execute unchanged.
      const dispatch = `needy() {
        if [ "$1" = runner ]; then return 0; fi
        while [ "$1" != -- ]; do shift; done
        shift
        HOME="$EXECUTOR_HOME" "$@"
      }\n${probe}`
      const run = (script: string, home: string) => spawnSync('sh', ['-c', script], {
        cwd: root, env: { ...process.env, HOME: home, EXECUTOR_HOME: executor }, encoding: 'utf8',
      })
      expect(run(dispatch, control).status).not.toBe(0)
      expect(run(steps.find(step => step.id === 'workspace')!.code!, executor).status).toBe(0)
      const checked = run(dispatch, control)
      expect(checked.status).toBe(0)
      expect(checked.stdout).toContain(join(executor, path.slice(2)))
      expect(existsSync(join(control, path.slice(2)))).toBe(false)
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
        expect(saved.machine).toBe(executor === 'runner' ? 'runner' : 'host')
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
    expect(steps.find(step => step.id === 'runner')?.code).toContain('needy runner serve')
    expect(steps.find(step => step.id === 'runner')?.machine).toBe('runner')
    expect(steps.find(step => step.id === 'token')?.machine).toBe('host')
    expect(steps.filter(step => step.code?.includes('ssh -N'))).toHaveLength(1)
    const control = steps.find(step => step.id === 'daemon')!
    expect(control.terminal.zh).toContain('控制主机 · SSH 终端')
    expect(control.code).toContain('needy config set daemon.listen')
    expect(control.code).toContain('needy daemon service install --now')
    expect(control.code).toContain('needy config use local')
    expect(control.text.en).toContain('needy daemon service install --replace --now')
    expect(steps.find(step => step.id === 'token')?.text.en).toContain('client context')
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

  // Windows enrollment cannot yet persist private credentials. A complete
  // generated walkthrough incorrectly promised it could reach an online runner.
  it('explains the Windows enrollment limit without generating setup commands', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, runnerOs: 'windows' as const }
    expect(configurationIssue(env)?.en).toContain('Windows runner enrollment is not supported yet')
    expect(guide('bash', env)).toEqual([])
  })

  it('quotes user project paths without executing shell substitutions', () => {
    const workspace = "~/a 'project' $(touch unwanted)"
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'native', workspace })
    expect(steps.find(step => step.id === 'workspace')?.code?.split('\n')[0]).toBe('mkdir -p "$HOME"/\'a \'\\\'\'project\'\\\'\' $(touch unwanted)\'')
    expect(parse(steps.find(step => step.profile)!.profile!).actor.workspace.from).toBe(`executor:here:${workspace}`)
  })

  it('shows invalid answers instead of silently replacing them', () => {
    const env = { ...DEFAULT_ENVIRONMENT, executor: 'runner' as const, runnerName: 'bad name' }
    expect(configurationIssue(env)).toBeDefined()
    expect(guide('bash', env)).toEqual([])
    expect(configurationIssue({ ...env, runnerName: 'valid', workspace: '' })).toBeDefined()
    expect(configurationIssue({ ...env, runnerName: 'valid', workspace: '.' })).toBeDefined()
  })
})
