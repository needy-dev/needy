import type { Text } from '../i18n'

import { parse, stringify } from 'yaml'

import bash from '../../../../examples/linux-mac/bash.yaml?raw'

export type Program = 'bash' | 'opencode'
export type Os = 'linux' | 'mac'
export type Executor = 'container' | 'native' | 'runner'
export type Machine = 'host' | 'runner' | 'actor'
export type PackageManager = 'apt' | 'dnf' | 'pacman'

// NOTICE: release installs are generated here. Source builds remain in the
// README reference until they have a complete per-driver walkthrough.
// TODO: a Codex entrypoint waits for its own login and persistent-state walkthrough.
export interface Environment {
  where: 'here' | 'server'
  os: Os
  kvm: boolean
  macRuntime: 'vz' | 'apple'
  executor: Executor
  launchFrom: 'host' | 'runner'
  docker: boolean
  pm: PackageManager
  runnerOs: Os | 'windows'
  runnerName: string
  workspace: string
  routing: 'all' | 'shells'
}

export interface Step {
  id: string
  machine: Machine
  terminal: Text
  title: Text
  text: Text
  code?: string
  prompt?: Text
  profile?: string
  expected?: Text
}

export const PROGRAMS: Array<{ id: Program, name: string }> = [
  { id: 'bash', name: 'bash' },
  { id: 'opencode', name: 'OpenCode' },
]

export const DEFAULT_ENVIRONMENT: Environment = {
  where: 'here', os: 'linux', kvm: true, macRuntime: 'vz', executor: 'container',
  launchFrom: 'runner',
  docker: true, pm: 'apt', runnerOs: 'mac', runnerName: 'mac', workspace: '~/needy-demo', routing: 'all',
}

/** Machine roles shared by the route overview and each instruction's location badge. */
export function locations(env: Environment): Record<Machine, { name: Text, description: Text }> {
  const executorOs = env.runnerOs === 'mac' ? 'macOS' : env.runnerOs === 'linux' ? 'Linux' : 'Windows'
  const controlOs = env.os === 'mac' ? 'macOS' : 'Linux'
  return {
    host: {
      name: { en: `Control host · ${controlOs}`, zh: `控制主机 · ${controlOs}` },
      description: env.where === 'server'
        ? { en: 'Run host commands in an SSH shell on the server, as the daemon account. The daemon and actor live here.', zh: '在服务器的 SSH shell 中，以 daemon 的账号执行宿主机命令。daemon 和 actor 在这里运行。' }
        : { en: 'Run host commands in a terminal on this computer, as the daemon account. The daemon and actor live here.', zh: '在这台电脑的终端中，以 daemon 的账号执行宿主机命令。daemon 和 actor 在这里运行。' },
    },
    runner: {
      name: { en: `Executor · ${executorOs}`, zh: `执行机器 · ${executorOs}` },
      description: env.launchFrom === 'runner'
        ? { en: 'Run these commands on the executor machine as the invoking CLI account. Its automatic holder executes this invocation under that account. HOME and project paths belong here.', zh: '在执行机器上，以启动 CLI 的账号执行这些命令。自动启动的 holder 以该账号承接本次 run 的执行。HOME 和项目路径属于这里。' }
        : { en: 'Run these commands in a terminal on the executor machine, as the account running runner serve. Its home and project paths belong to this machine.', zh: '在执行机器的终端中，以运行 runner serve 的账号执行。这里的 HOME 和项目路径属于执行机器。' },
    },
    actor: {
      name: { en: 'Actor · Linux', zh: 'Actor · Linux' },
      description: { en: 'Use the program terminal opened by run create -it. The program lives in the Linux actor; captured child commands follow the routing rules.', zh: '使用 run create -it 打开的程序终端。程序运行在 Linux actor 中，捕获到的子命令按规则路由。' },
    },
  }
}

const PROFILES: Record<'bash', string> = { bash }
// Package source metadata and helper file lists:
// docs/ai/references/cli/2026-11-15-workspace-mount-package-sources-note.md
const INSTALL: Record<PackageManager, string> = {
  apt: 'sudo apt-get update\nsudo apt-get install -y sshfs fuse3',
  dnf: 'sudo dnf install -y fuse-sshfs fuse3',
  pacman: 'sudo pacman -S --noconfirm sshfs fuse3',
}
// The install script also installs and starts the control daemon as a user
// service; a machine that is only a runner opts out with NEEDY_NO_DAEMON=1.
function installNeedy(daemon: boolean): string {
  return `curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | ${daemon ? '' : 'NEEDY_NO_DAEMON=1 '}sh\nexport PATH="$HOME/.local/bin:$PATH"\nneedy --version`
}

/** The actor driver selected by the host's capabilities, not by the entrypoint. */
export function actorDriver(env: Environment): string {
  if (env.os === 'mac')
    return env.macRuntime === 'vz' ? 'macos-vz' : 'macos-apple-container'
  return env.kvm ? 'linux-firecracker' : 'linux-container'
}

export function needsDocker(_program: Program, env: Environment): boolean {
  return actorDriver(env) === 'linux-container' || env.executor === 'container'
}

/** Rejects values that cannot name a runner or a usable directory. No input is silently replaced. */
export function configurationIssue(env: Environment): Text | undefined {
  if (env.executor === 'runner' && env.launchFrom === 'runner' && env.runnerOs === 'windows')
    return { en: 'Windows automatic client execution requires private startup-file ACL support. Choose a Linux or macOS creating client for this walkthrough.', zh: 'Windows 自动客户端执行需要私有启动文件的 ACL 支持。此教程请选择 Linux 或 macOS 作为启动客户端。' }
  if (env.executor === 'runner' && env.runnerOs === 'windows')
    return { en: 'Windows runner enrollment is not supported yet. Choose a Linux or macOS executor for this walkthrough.', zh: '暂不支持 Windows runner 注册。此教程请选择 Linux 或 macOS executor。' }
  if (env.executor === 'runner' && env.launchFrom === 'host' && !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(env.runnerName))
    return { en: 'Use a runner name of 1–63 lowercase letters, digits, or hyphens.', zh: 'runner 名称需为 1–63 个小写字母、数字或连字符，且首尾不能是连字符。' }
  if (env.executor !== 'container' && (!env.workspace.trim() || /[\r\n\0]/.test(env.workspace)))
    return { en: 'Enter a project directory on the executor, without line breaks.', zh: '填写 executor 上的项目目录，不要包含换行。' }
  // NOTICE: explicit `actor.workspace.from` paths must be absolute or `~/...`;
  // omitting `actor.workspace` mounts the invoking client's current directory live.
  if (env.executor !== 'container' && !(env.workspace.startsWith('/') || env.workspace === '~' || env.workspace.startsWith('~/')))
    return { en: 'Use an absolute executor path or ~/path (or omit actor.workspace in your profile to mount the current directory).', zh: '请使用执行机器上的绝对路径或 ~/路径（或在 profile 中省略 actor.workspace 以自动挂载当前目录）。' }
  return undefined
}

/** POSIX shell quoting at the clipboard boundary; project paths can contain spaces and quotes. */
function quote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`
}

function shellPath(path: string): string {
  return path === '~' ? '"$HOME"' : path.startsWith('~/') ? `"$HOME"/${quote(path.slice(2))}` : quote(path)
}

/** Derives one profile and its per-machine steps from all wizard answers. */
export function guide(program: Program, env: Environment = DEFAULT_ENVIRONMENT): Step[] {
  if (configurationIssue(env))
    return []
  const driver = actorDriver(env)
  const vm = driver === 'linux-firecracker' || driver === 'macos-vz'
  const host = env.where === 'server'
    ? { en: 'Control host · SSH terminal', zh: '控制主机 · SSH 终端' }
    : { en: 'Control host · local terminal', zh: '控制主机 · 本地终端' }
  const remote = { en: 'Executor · setup terminal', zh: '执行机器 · 配置终端' }
  const fromRunner = env.executor === 'runner' && env.launchFrom === 'runner'
  const launchMachine: Machine = fromRunner ? 'runner' : 'host'
  const launchTerminal = fromRunner ? remote : host
  const target = env.executor === 'runner' ? 'remote' : env.executor === 'native' ? 'here' : 'build'
  const name = `${program}-demo`
  const runner = quote(env.runnerName)
  const useBuiltin = program === 'opencode' && env.where === 'here' && driver === 'linux-firecracker' && env.executor === 'container' && env.routing === 'all'
  const steps: Step[] = []
  const prerequisites: string[] = []
  if (needsDocker(program, env) && !env.docker) {
    prerequisites.push(env.os === 'mac'
      ? 'brew install --cask orbstack'
      : env.pm === 'pacman'
        ? 'sudo pacman -S --noconfirm docker\nsudo systemctl enable --now docker'
        : 'curl -fsSL https://get.docker.com | sh')
  }
  if (driver === 'linux-container') {
    // Source: https://github.com/libfuse/libfuse/blob/fuse-3.16.2/util/fuse.conf#L3-L9
    // allow_other lets the container's user access the daemon's host mount.
    prerequisites.push(INSTALL[env.pm], 'sudo sh -c \'grep -qxF user_allow_other /etc/fuse.conf || echo user_allow_other >> /etc/fuse.conf\'')
  }
  if (driver === 'macos-vz')
    prerequisites.push('brew install vfkit')
  if (driver === 'macos-apple-container')
    prerequisites.push('brew install container', 'container system start')
  if (prerequisites.length > 0) {
    steps.push({
      id: 'prepare', machine: 'host', terminal: host,
      title: { en: 'Prepare the actor host', zh: '准备 actor 宿主机' },
      text: driver === 'linux-container'
        ? { en: 'Install SSHFS and FUSE 3 on this actor host. The daemon mounts the workspace here, even for a remote executor. user_allow_other permits the container user to access this mount.', zh: '在这台 actor 宿主机安装 SSHFS 和 FUSE 3。即使 executor 在远端，daemon 仍在这里挂载 workspace。user_allow_other 允许容器用户访问此挂载。' }
        : { en: `Install the required runtime on this actor host.${vm ? ' VM actor images contain their own SSHFS and FUSE helpers.' : ''}`, zh: `在这台 actor 宿主机安装所需运行时。${vm ? 'VM actor 镜像自带 SSHFS 和 FUSE 辅助程序。' : ''}` },
      code: prerequisites.join('\n'),
      expected: { en: `The host is ready for ${driver}. Start Docker first if its application is not running.`, zh: `宿主机具备 ${driver} 的前提；Docker 应用未运行时，先启动它。` },
    })
  }
  steps.push({
    id: 'install', machine: 'host', terminal: host,
    title: { en: 'Install needy', zh: '安装 needy' },
    text: { en: 'These steps use a released needy. Local runs pull the release’s base images automatically (use needy image pull for offline setups). The README reference covers source builds.', zh: '这里使用发布版 needy。本机运行会自动拉取发布版基础镜像（离线环境可使用 needy image pull）。源码构建仍可查阅 README 参考部分。' },
    code: installNeedy(true),
  })

  steps.push({ id: 'daemon', machine: 'host', terminal: host,
    title: { en: 'Reach the control daemon', zh: '确认控制 daemon 可以连接' },
    text: env.executor === 'runner'
      ? { en: 'The install script started a user service on the control host, with systemd or launchd available. Select local for the following management commands. Replacing the service applies the listener: it restarts control with the current configuration. If the script warned that the daemon did not start, use needy daemon service install --now instead. Use run stop to end an invocation.', zh: '安装脚本已在控制主机上安装并启动用户服务，需要可用的 systemd 用户管理器或 launchd。选择 local，让后续管理命令连接本机。替换服务会应用监听地址：它按当前配置重启控制服务。如果脚本警告 daemon 未能启动，改用 needy daemon service install --now。需要结束某个 run 时，使用 run stop。' }
      : { en: 'The install script started a user service on the control host, with systemd or launchd available. Select local for the following management commands. If the script warned that the daemon did not start, run needy daemon service install --now first. Use run stop to end an invocation.', zh: '安装脚本已在控制主机上安装并启动用户服务，需要可用的 systemd 用户管理器或 launchd。选择 local，让后续管理命令连接本机。如果脚本警告 daemon 未能启动，先执行 needy daemon service install --now。需要结束某个 run 时，使用 run stop。' },
    code: `needy config use local\n${env.executor === 'runner' ? "needy config set daemon.listen '[tcp://127.0.0.1:7400]'\nneedy daemon service install --replace --now\n" : ''}needy daemon service status\nneedy runner ls`,
    expected: { en: 'The service reports ready. The runner table appears without a client bearer or an address export.', zh: '服务状态为 ready，无需客户端令牌或地址环境变量即可列出 runner。' } })

  if (env.executor === 'runner') {
    const workspace = shellPath(env.workspace)
    steps.push({ id: 'runner-install', machine: 'runner', terminal: remote,
      title: { en: 'Prepare the executor machine', zh: '准备执行命令的机器' },
      text: { en: 'Install needy and your project tools here. Native execution needs no actor images on this machine.', zh: '在这里安装 needy 和项目工具。这台机器上的 native 执行不需要 actor 镜像。' },
      code: installNeedy(false),
      expected: { en: 'The executor has needy and its project tools. Use the account that will serve commands.', zh: '执行机器已安装 needy 和项目工具。使用将要执行命令的账号。' } },
    { id: 'workspace', machine: 'runner', terminal: remote,
      title: { en: 'Prepare the project on the executor', zh: '在执行机器上准备项目目录' },
      text: fromRunner
        ? { en: 'Use the same account for this directory and run create. The automatic holder inherits that account. Use an existing project, or create this empty demo directory here.', zh: '项目目录和 run create 使用同一账号，自动启动的 holder 继承该账号。可使用已有项目，或在这里创建这个空演示目录。' }
        : { en: 'Use the same account for this directory and runner serve. Use an existing project, or create this empty demo directory. Creating it on the control host will not create it here.', zh: '目录和 runner serve 使用同一账号。可填写已有项目，或创建这个空演示目录。在控制主机创建目录不会在这里创建它。' },
      code: `mkdir -p ${workspace} && cd ${workspace}` },
    { id: 'tunnel', machine: 'runner', terminal: { en: 'Executor machine · a separate tunnel terminal', zh: 'executor 机器 · 另开的隧道终端' },
      title: { en: 'Open one tunnel to the control host', zh: '建立一条到控制主机的隧道' },
      text: { en: 'Replace user@example.com with the actor host’s SSH destination and keep this tunnel open. No SSH is needed with Cloudflare Tunnel + Access: forward TCP with cloudflared access tcp --hostname needy.example.com --url 127.0.0.1:7400, or connect over HTTPS via https://needy.example.com with http2Origin: true.', zh: '把 user@example.com 换成 actor 宿主机的 SSH 地址并保持这个隧道开启。如果使用 Cloudflare Tunnel + Access 则无需 SSH：可用 cloudflared access tcp --hostname needy.example.com --url 127.0.0.1:7400 转发 TCP，或在开启 http2Origin: true 时直接通过 https://needy.example.com 连接。' },
      code: 'ssh -N -o ExitOnForwardFailure=yes -L 7400:127.0.0.1:7400 user@example.com',
      expected: { en: 'The connection stays open. Reuse it instead of opening another tunnel on port 7400.', zh: '连接保持开启。复用这条连接，不要再占用一次本地 7400 端口。' } })
    if (!fromRunner) {
      steps.push({ id: 'token', machine: 'host', terminal: host,
        title: { en: 'Create a runner invitation', zh: '创建 runner 邀请码' },
        text: { en: 'Use this code only to enroll the executor. A runner saves its own credential. An optional client context uses a separate client invitation to manage runs.', zh: '此邀请码只用于注册 executor。runner 保存自己的凭据。可选的客户端 context 使用单独的客户端邀请码来管理 run。' },
        code: 'needy runner invite --ttl 1h --roles executor' },
      { id: 'paste-token', machine: 'runner', terminal: remote,
        title: { en: 'Enroll the executor once', zh: '完成一次 executor 注册' },
        text: { en: 'Run this command. Paste the invitation code at its hidden prompt. Enrollment saves the runner credential and returns.', zh: '执行命令后，在隐藏输入提示中粘贴邀请码。注册会保存 runner 凭据并返回。' },
        code: `needy --address tcp://127.0.0.1:7400 runner join ${runner} --roles executor` },
      { id: 'runner', machine: 'runner', terminal: { en: 'Executor · separate runner terminal', zh: '执行机器 · 单独的 runner 终端' },
        title: { en: 'Start the enrolled runner', zh: '启动已注册的 runner' },
        text: { en: 'Keep this process running as the executor account, not with sudo. Native commands inherit its permissions. This named runner serves work when you create from the control host. Changing the client context does not change its OS account.', zh: '以执行账号持续运行，不要使用 sudo。native 命令继承它的权限。从控制主机创建 run 时，由此具名 runner 接收执行任务。切换客户端 context 不会改变它的系统账号。' },
        code: 'export PATH="$HOME/.local/bin:$PATH"\nneedy runner serve' },
      { id: 'check-runner', machine: 'host', terminal: host,
        title: { en: 'Check the enrolled runner', zh: '确认 runner 已上线' },
        text: { en: 'Confirm that the enrolled runner is online on the control host.', zh: '在控制主机确认已注册的 runner 处于 online 状态。' },
        code: 'needy runner ls',
        expected: { en: `The runner ${env.runnerName} appears as online.`, zh: `列表中的 runner ${env.runnerName} 显示为 online。` } })
    }
    if (fromRunner) {
      steps.push({ id: 'client-invite', machine: 'host', terminal: host,
        title: { en: 'Authorize a control client on the executor', zh: '授权执行机器上的控制客户端' },
        text: { en: 'Create a client invitation for the executor computer. This grants run/process access, not administrator access. This workflow needs no runner enrollment.', zh: '为执行机器创建客户端邀请码，授予 run/process 权限，不授予管理权限。此流程不需要注册 runner。' },
        code: 'needy client invite --name executor-cli',
      }, { id: 'client-pair', machine: 'runner', terminal: remote,
        title: { en: 'Pair and select the control connection', zh: '配对并选择控制连接' },
        text: { en: 'Reuse the open tunnel. Paste the client invitation at the hidden prompt. Pairing grants control access; execution uses the invoking CLI account. An automatic holder survives viewer detach. For an existing context, select and check it instead of pairing again.', zh: '复用已开启的隧道，在隐藏提示中粘贴客户端邀请码。配对授予控制访问权限，执行使用启动 CLI 的账号。自动启动的 holder 在终端分离后继续运行。已有 context 时选择并检查即可，不必再次配对。' },
        code: 'needy --address tcp://127.0.0.1:7400 config context pair control\nneedy config use control\nneedy config context check control\nneedy run ls',
      })
    }
  }
  else if (env.executor === 'native') {
    const workspace = shellPath(env.workspace)
    steps.push({ id: 'workspace', machine: 'host', terminal: host,
      title: { en: 'Create the native project directory', zh: '创建本机项目目录' },
      text: { en: 'Native means the actor host itself. The directory belongs to this machine.', zh: 'native 指 actor 所在宿主机本身，项目目录属于这台机器。' },
      code: `mkdir -p ${workspace} && cd ${workspace}` })
  }

  if (!useBuiltin) {
    const project = env.executor === 'container' ? '~/workspace' : env.workspace
    const profile: Record<string, any> = program === 'opencode'
      ? (() => {
          const actor: Record<string, any> = {
            driver,
            ...(driver === 'linux-container' ? { container: { oci_runtime: 'runc' } } : {}),
          }
          const out: Record<string, any> = {
            extends: 'builtin:opencode',
            actor,
          }
          if (env.executor !== 'container') {
            actor.workspace = {
              path: '~/workspace',
              from: `executor:${target}:${project}`,
              writable: true,
            }
            out.executors = {
              [target]: env.executor === 'runner'
                ? { runner: fromRunner ? { client: true } : env.runnerName, driver: 'native' }
                : { driver: 'native' },
            }
            out.commands = [{ match: env.routing === 'all' ? '**' : '{bash,sh,dash,zsh} **', to: target }]
            out.unknown = env.routing === 'all' ? target : 'actor'
          }
          else if (env.routing === 'shells') {
            out.unknown = 'actor'
          }
          return out
        })()
      : (() => {
          const parsed = parse(PROFILES[program])
          parsed.actor.driver = driver
          if (driver === 'linux-container')
            parsed.actor.container = { oci_runtime: 'runc' }
          else
            delete parsed.actor.container
          delete parsed.actor.image
          parsed.actor.workspace.from = `executor:${target}:${project}`
          parsed.executors = { [target]: env.executor === 'runner'
            ? { runner: fromRunner ? { client: true } : env.runnerName, driver: 'native' }
            : { driver: env.executor === 'container' ? 'linux-container' : 'native' } }
          parsed.commands = [{ match: env.routing === 'all' ? '**' : '{bash,sh,dash,zsh} **', to: target }]
          parsed.unknown = env.routing === 'all' ? target : 'actor'
          return parsed
        })()
    const header = program === 'opencode'
      ? '# Extends builtin:opencode (auto-builds the opencode feature and syncs ~/.local/share/opencode).\n# Container capture covers image shims, not new unshimmed programs.\n'
      : '# Generated from examples/linux-mac for the selected environment.\n# Container capture covers image shims, not new unshimmed programs.\n'
    const yaml = header + stringify(profile, { lineWidth: 0 })
    steps.push({ id: 'profile', machine: launchMachine, terminal: launchTerminal,
      title: { en: 'Save the selected profile', zh: '保存按选择生成的 profile' },
      text: { en: 'Save this profile where you will run needy run create. The CLI sends its resolved contents to the control daemon. Executor workspace paths still belong to the selected executor; host file paths belong to the control host.', zh: '保存在将要执行 needy run create 的机器上。CLI 会把解析后的内容发送给控制 daemon。executor 的 workspace 路径仍属于所选执行机器，host 文件路径仍属于控制主机。' },
      profile: yaml,
      code: `mkdir -p ~/.config/needy/profiles\ncat > ~/.config/needy/profiles/${name}.yaml <<'YAML'\n${yaml.trimEnd()}\nYAML`,
      expected: { en: `${name}: ${driver}, ${program}, executor ${target}.`, zh: `${name}：${driver}，入口 ${program}，executor 为 ${target}。` } })
  }

  steps.push({ id: 'start', machine: launchMachine, terminal: launchTerminal,
    title: { en: `Start ${PROGRAMS.find(p => p.id === program)!.name}`, zh: `启动 ${PROGRAMS.find(p => p.id === program)!.name}` },
    text: useBuiltin
      ? { en: 'Run opencode directly with the built-in profile. needy builds and caches the opencode feature layer on first run. The actor terminal appears here.', zh: '直接使用内置 profile 启动 opencode。首次运行时 needy 会自动构建并缓存 opencode feature 层，actor 终端会在这里打开。' }
      : fromRunner
        ? { en: 'Creation binds the automatic holder to this invoking account and validates the profile automatically. Keep the tunnel open. The actor terminal appears here.', zh: '创建时会自动校验 profile，并将自动启动的 holder 绑定到当前启动账号。保持隧道开启，actor 终端会在这里打开。' }
        : { en: 'run create validates the profile automatically. Keep any persistent runner and tunnel open. The actor terminal appears here.', zh: 'run create 会自动校验 profile。若使用持久 runner 和隧道，保持它们开启。actor 终端会在这里打开。' },
    code: useBuiltin
      ? 'needy run create -it --profile builtin:opencode'
      : `needy run create -it -f ~/.config/needy/profiles/${name}.yaml` })
  if (program === 'bash') {
    steps.push({ id: 'observe', machine: 'actor', terminal: { en: 'Inside the new bash', zh: '在刚进入的 bash 内' },
      title: { en: 'Compare the entrypoint and child shell', zh: '对比入口和子 shell 的执行位置' },
      text: { en: 'The entrypoint remains in the actor. Its child shell runs on the selected executor. Builtins stay in their shell.', zh: '入口留在 actor，子 shell 在所选 executor 上执行。内建命令仍由各自的 shell 执行。' },
      code: 'printf \'entrypoint=%s\\n\' "$OSTYPE"; bash -c \'uname -s; pwd\'' })
  }
  else {
    steps.push({ id: 'login', machine: 'actor', terminal: { en: 'Inside the program, not the host shell', zh: '在程序界面内，不是在宿主机 shell 内' },
      title: { en: 'Sign in to your model provider', zh: '登录模型服务' },
      text: { en: 'Sign in through the agent’s interactive login console — for example, run /connect in OpenCode, or follow its provider API-key or device-code prompt.', zh: '在 agent 的交互式登录控制台中完成登录——例如在 OpenCode 中输入 /connect，或按提示填写服务商 API key / 完成设备码登录。' } },
    { id: 'observe', machine: 'actor', terminal: { en: 'In the agent conversation', zh: '在 agent 对话内' },
      title: { en: 'Check a tool call and shared file', zh: '验证工具调用和共享文件' },
      text: { en: 'Check the actual tool output and the file on the selected executor.', zh: '查看实际工具输出，并在所选 executor 上查看同一份文件。' },
      prompt: { en: 'Create needy-check.txt with the text "shared workspace check" using your file editor. Then run uname -s and cat needy-check.txt with your shell tool. Show the exact output.', zh: '用文件编辑工具创建 needy-check.txt，内容为 shared workspace check。再用 shell 工具执行 uname -s 和 cat needy-check.txt，给我看实际输出。' } })
  }
  steps.push({ id: 'again', machine: launchMachine, terminal: launchTerminal,
    title: { en: 'Return to the setup shell', zh: '回到配置终端' },
    text: { en: 'Ctrl-] detaches without stopping the run. Replace RUN_ID with its listed ID; run each lifecycle command separately.', zh: 'Ctrl-] 只断开终端，不会停止 run。把 RUN_ID 换成列表中的编号，接回、停止命令分别执行。' },
    code: 'needy run ls',
    expected: { en: 'Use needy run attach RUN_ID to return, and needy run stop RUN_ID after detaching to save state.', zh: '用 needy run attach RUN_ID 接回；再次断开后，用 needy run stop RUN_ID 正常停止并保存状态。' } })
  return steps
}
