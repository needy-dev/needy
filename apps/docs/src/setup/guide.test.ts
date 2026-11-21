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
    expect(steps.find(step => step.id === 'start')?.code).toContain('--mode static')
    expect(steps.find(step => step.id === 'start')?.code).toContain('-f ~/.config/needy/profiles/bash-demo.yaml')
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
    const sftp = steps.find(step => step.id === 'prepare-sftp')!
    expect(sftp.machine).toBe('runner')
    expect(sftp.text.en).toContain('macFUSE')
    expect(sftp.text.en).toContain('executor account')
    expect(sftp.code).toBe('id\ntest -x /usr/libexec/sftp-server')
    expect(steps.indexOf(sftp)).toBeLessThan(steps.findIndex(step => step.id === 'workspace'))
  })

  it('checks Linux native SFTP on the executor without assuming the actor host package manager', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', runnerOs: 'linux', pm: 'dnf' })
    const sftp = steps.find(step => step.id === 'prepare-sftp')!
    expect(sftp.machine).toBe('runner')
    expect(sftp.text.en).toContain('openssh-sftp-server')
    expect(sftp.code).not.toContain('dnf')
    expect(sftp.code).toBe('id\ntest -x /usr/lib/openssh/sftp-server')
  })

  it('checks a local native workspace source as the daemon account and leaves container sources to their image', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'native' })
    const sftp = steps.find(step => step.id === 'prepare-sftp')!
    expect(sftp.machine).toBe('host')
    expect(sftp.text.en).toContain('daemon account')
    expect(sftp.code).toBe('id\ntest -x /usr/lib/openssh/sftp-server')
    expect(guide('bash').some(step => step.id === 'prepare-sftp')).toBe(false)
  })

  // ROOT CAUSE:
  //
  // If installation was reused, workspace preparation was easy to skip
  // because it was part of the install step; readiness checked only uname.
  // Before the fix, the guide offered no serving-account directory probe.
  // The fix verifies the source directory before the actor mounts it.
  it('verifies the remote workspace through the serving runner before mounting it', () => {
    const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, where: 'server', executor: 'runner', launchFrom: 'host', workspace: '~/my project' })
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
      const steps = guide('bash', { ...DEFAULT_ENVIRONMENT, executor: 'runner', launchFrom: 'host', workspace: path })
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
          ? { runner: { client: true }, driver: 'native' }
          : { driver: executor === 'container' ? 'linux-container' : 'native' } })
        expect(profile.actor.workspace.from).toBe(`executor:${target}:${executor === 'container' ? '~/workspace' : '~/my project'}`)
        expect(profile.actor.workspace.writable).toBe(true)
        expect(profile.commands).toEqual([{ match: '**', to: target }])
        expect(profile.unknown).toBe(target)
        expect(saved.machine).toBe(executor === 'runner' ? 'runner' : 'host')
        expect(saved.code).toContain(`profiles/${id}-demo.yaml`)
        expect(steps.find(step => step.id === 'start')?.code).toContain(`-f ~/.config/needy/profiles/${id}-demo.yaml`)
        expect(steps.find(step => step.id === 'install')?.code).not.toContain('needy image pull')
        expect(check(saved.profile!).findings.filter(f => f.severity === 'error')).toEqual([])
        expect(steps.some(step => step.id === 'tunnel')).toBe(executor === 'runner')
        if (id === 'opencode') {
          expect(profile.extends).toBe('builtin:opencode')
          expect(steps.some(step => step.id === 'state')).toBe(false)
          expect(saved.profile).not.toContain('needy-state')
        }
      })
    }
  }

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
    expect(steps.find(step => step.id === 'workspace')?.code).toBe('mkdir -p "$HOME"/\'a \'\\\'\'project\'\\\'\' $(touch unwanted)\'')
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
