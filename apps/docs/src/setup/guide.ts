import type { Text } from '../i18n'

import { parse, stringify } from 'yaml'

import bash from '../../../../examples/linux-mac/bash.yaml?raw'
import claude from '../../../../examples/linux-mac/claude.yaml?raw'
import opencode from '../../../../examples/linux-mac/opencode.yaml?raw'

export type Program = 'bash' | 'claude' | 'opencode'
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
  { id: 'claude', name: 'Claude Code' },
  { id: 'opencode', name: 'OpenCode' },
]

export const DEFAULT_ENVIRONMENT: Environment = {
  where: 'here', os: 'linux', kvm: true, macRuntime: 'vz', executor: 'container',
  docker: true, pm: 'apt', runnerOs: 'mac', runnerName: 'mac', workspace: '~/needy-demo', routing: 'all',
}

const PROFILES: Record<Program, string> = { bash, claude, opencode }
const INSTALL: Record<PackageManager, string> = {
  apt: 'sudo apt-get install -y sshfs',
  dnf: 'sudo dnf install -y fuse-sshfs',
  pacman: 'sudo pacman -S --noconfirm sshfs',
}
const INSTALL_NEEDY = 'curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh\nexport PATH="$HOME/.local/bin:$PATH"\nneedy --version'

/** The actor driver selected by the host's capabilities, not by the entrypoint. */
export function actorDriver(env: Environment): string {
  if (env.os === 'mac')
    return env.macRuntime === 'vz' ? 'macos-vz' : 'macos-apple-container'
  return env.kvm ? 'linux-firecracker' : 'linux-container'
}

export function needsDocker(program: Program, env: Environment): boolean {
  return actorDriver(env) === 'linux-container' || env.executor === 'container' || program === 'claude'
}

/** Rejects values that cannot name a runner or a usable directory. No input is silently replaced. */
export function configurationIssue(env: Environment): Text | undefined {
  if (env.executor === 'runner' && env.runnerOs === 'windows')
    return { en: 'Windows runner enrollment is not supported yet. Choose a Linux or macOS executor for this walkthrough.', zh: '暂不支持 Windows runner 注册。此教程请选择 Linux 或 macOS executor。' }
  if (env.executor === 'runner' && !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(env.runnerName))
    return { en: 'Use a runner name of 1–63 lowercase letters, digits, or hyphens.', zh: 'runner 名称需为 1–63 个小写字母、数字或连字符，且首尾不能是连字符。' }
  if (env.executor !== 'container' && (!env.workspace.trim() || /[\r\n\0]/.test(env.workspace)))
    return { en: 'Enter a project directory on the executor, without line breaks.', zh: '填写 executor 上的项目目录，不要包含换行。' }
  return undefined
}

/** POSIX shell quoting at the clipboard boundary; project paths can contain spaces and quotes. */
function quote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`
}

function shellPath(path: string): string {
  return path.startsWith('~/') ? `"$HOME"/${quote(path.slice(2))}` : quote(path)
}

/** Derives one profile and its per-machine steps from all wizard answers. */
export function guide(program: Program, env: Environment = DEFAULT_ENVIRONMENT): Step[] {
  if (configurationIssue(env))
    return []
  const driver = actorDriver(env)
  const vm = driver === 'linux-firecracker' || driver === 'macos-vz'
  const host = env.where === 'server'
    ? { en: 'Server · the same SSH setup terminal', zh: '服务器 · 同一个 SSH 配置终端' }
    : { en: 'This computer · the same setup terminal', zh: '本机 · 同一个配置终端' }
  const remote = { en: 'Executor machine · local terminal, outside SSH', zh: 'executor 机器 · 本地终端，不在服务器 SSH 会话里' }
  const target = env.executor === 'runner' ? 'remote' : env.executor === 'native' ? 'here' : 'build'
  const name = `${program}-demo`
  const runner = quote(env.runnerName)
  const steps: Step[] = []
  const prerequisites: string[] = []
  if (needsDocker(program, env) && !env.docker) {
    prerequisites.push(env.os === 'mac'
      ? 'brew install --cask orbstack'
      : env.pm === 'pacman'
        ? 'sudo pacman -S --noconfirm docker\nsudo systemctl enable --now docker'
        : 'curl -fsSL https://get.docker.com | sh')
  }
  if (needsDocker(program, env))
    prerequisites.push('docker info --format \'{{.ServerVersion}}\'')
  if (driver === 'linux-container')
    prerequisites.push(INSTALL[env.pm], 'sudo sh -c \'grep -qxF user_allow_other /etc/fuse.conf || echo user_allow_other >> /etc/fuse.conf\'', 'test -r /dev/fuse && test -w /dev/fuse')
  if (driver === 'linux-firecracker')
    prerequisites.push('test -r /dev/kvm && test -w /dev/kvm')
  if (driver === 'macos-vz')
    prerequisites.push('brew install vfkit')
  if (driver === 'macos-apple-container')
    prerequisites.push('brew install container', 'container system start')
  steps.push({
    id: 'prepare', machine: 'host', terminal: host,
    title: { en: 'Prepare the actor host', zh: '准备 actor 宿主机' },
    text: { en: 'Use the account that will run needy. Docker and device checks must succeed under this account.', zh: '使用将要运行 needy 的账号。Docker 与设备访问检查都必须在这个账号下通过。' },
    code: prerequisites.join('\n'),
    expected: { en: `The host is ready for ${driver}. Start Docker first if its application is not running.`, zh: `宿主机具备 ${driver} 的前提；Docker 应用未运行时，先启动它。` },
  }, {
    id: 'install', machine: 'host', terminal: host,
    title: { en: 'Install needy and the selected base image', zh: '安装 needy 和所选 driver 的基础镜像' },
    text: { en: 'These steps use a released needy. The README reference covers source builds.', zh: '这里使用发布版 needy。源码构建仍可查阅 README 参考部分。' },
    code: `${INSTALL_NEEDY}\nneedy image pull --driver ${driver}${env.executor === 'container' && driver !== 'linux-container'
      ? '\nneedy_release=$(needy --version | awk \'{print $2}\')\ndocker pull "ghcr.io/needy-dev/needy-executor:v${needy_release}"' : ''}`,
  })

  if (program === 'claude') {
    const build = [
      'git clone https://github.com/needy-dev/needy', 'cd needy',
      'needy_release=$(needy --version | awk \'{print $2}\')',
      'docker build --build-arg BASE="ghcr.io/needy-dev/needy-actor:v${needy_release}" -t needy-agents:latest examples/agents',
    ]
    if (vm)
      build.push('needy image build --from needy-agents:latest')
    if (driver === 'macos-apple-container')
      build.push('docker save needy-agents:latest -o needy-agents.tar', 'container image load --input needy-agents.tar')
    steps.push({ id: 'image', machine: 'host', terminal: host,
      title: { en: 'Build and prepare the Claude image', zh: '构建并准备 Claude 镜像' },
      text: { en: 'The selected driver uses this image. If you already have a checkout, enter it instead of cloning again.', zh: '所选 driver 会使用这份镜像。已有源码目录时，进入该目录，不必再次 clone。' }, code: build.join('\n') })
  }

  steps.push({ id: 'daemon', machine: 'host', terminal: host,
    title: { en: 'Start and reach the control daemon', zh: '启动控制 daemon 并确认连接' },
    text: { en: 'Install a user service on the control host, with systemd or launchd available. Select local for the following management commands. For an existing service, replace the install line with needy daemon service install --replace --now. Replacement applies the current configuration and stops active runs.', zh: '在控制主机上安装用户服务，需要可用的 systemd 用户管理器或 launchd。选择 local，让后续管理命令连接本机。已有服务时，把安装命令替换为 needy daemon service install --replace --now。替换会应用当前配置，并停止正在运行的 run。' },
    code: `needy config use local\n${env.executor === 'runner' ? "needy config set daemon.listen '[tcp://127.0.0.1:7400]'\n" : ''}needy daemon service install --now\nneedy daemon service status\nneedy runner ls`,
    expected: { en: 'The service reports ready. The runner table appears without a client bearer or an address export.', zh: '服务状态为 ready，无需客户端令牌或地址环境变量即可列出 runner。' } })

  if (env.executor === 'runner') {
    steps.push({ id: 'runner-install', machine: 'runner', terminal: remote,
      title: { en: 'Prepare the executor machine', zh: '准备执行命令的机器' },
      text: { en: 'Install needy and your project tools here. A native runner needs no actor images.', zh: '在这里安装 needy 和项目工具，native runner 不需要 actor 镜像。' },
      code: `${INSTALL_NEEDY}\nmkdir -p ${shellPath(env.workspace)}`,
      expected: { en: 'The project directory exists. The runner inherits this terminal’s PATH.', zh: '项目目录已创建。runner 会继承这个终端的 PATH。' } },
    { id: 'tunnel', machine: 'runner', terminal: { en: 'Executor machine · a separate tunnel terminal', zh: 'executor 机器 · 另开的隧道终端' },
      title: { en: 'Open one tunnel to the control host', zh: '建立一条到控制主机的隧道' },
      text: { en: 'Replace user@example.com with the actor host’s SSH destination. Keep this tunnel open.', zh: '把 user@example.com 换成 actor 宿主机的 SSH 地址，保持这个隧道开启。' },
      code: 'ssh -N -o ExitOnForwardFailure=yes -L 7400:127.0.0.1:7400 user@example.com',
      expected: { en: 'The connection stays open. Reuse it instead of opening another tunnel on port 7400.', zh: '连接保持开启。复用这条连接，不要再占用一次本地 7400 端口。' } },
    { id: 'token', machine: 'host', terminal: host,
      title: { en: 'Create a runner invitation', zh: '创建 runner 邀请码' },
      text: { en: 'Use this code only to enroll the executor. A runner saves its own credential. An optional client context uses a separate client invitation to manage runs.', zh: '此邀请码只用于注册 executor。runner 保存自己的凭据。可选的客户端 context 使用单独的客户端邀请码来管理 run。' },
      code: 'needy runner invite --ttl 1h --roles executor' },
    { id: 'paste-token', machine: 'runner', terminal: remote,
      title: { en: 'Enroll the executor once', zh: '完成一次 executor 注册' },
      text: { en: 'Run this command. Paste the invitation code at its hidden prompt. Enrollment saves the runner credential and returns.', zh: '执行命令后，在隐藏输入提示中粘贴邀请码。注册会保存 runner 凭据并返回。' },
      code: `needy --address tcp://127.0.0.1:7400 runner join ${runner} --roles executor` },
    { id: 'runner', machine: 'runner', terminal: remote,
      title: { en: 'Start the enrolled runner', zh: '启动已注册的 runner' },
      text: { en: 'Leave this process running outside the server SSH session. It reconnects with its saved credential and enrollment settings.', zh: '在服务器 SSH 会话之外持续运行此进程。它使用已保存的凭据和注册配置重连。' },
      code: 'needy runner serve' },
    { id: 'check-runner', machine: 'host', terminal: host,
      title: { en: 'Check which machine joined', zh: '确认加入的是哪台机器' },
      text: { en: 'Check the HOST column as well as the runner name.', zh: '既看 runner 名称，也看 HOST 列，确认操作系统与所选机器一致。' },
      code: `needy runner ls\nneedy proc exec -T -n --runner ${runner} -- uname -s`,
      expected: { en: `The runner is online. Its command reports ${env.runnerOs === 'mac' ? 'Darwin' : 'Linux'}.`, zh: `runner 为 online，命令返回 ${env.runnerOs === 'mac' ? 'Darwin' : 'Linux'}。` } })
  }
  else if (env.executor === 'native') {
    steps.push({ id: 'workspace', machine: 'host', terminal: host,
      title: { en: 'Create the native project directory', zh: '创建本机项目目录' },
      text: { en: 'Native means the actor host itself. The directory belongs to this machine.', zh: 'native 指 actor 所在宿主机本身，项目目录属于这台机器。' },
      code: `mkdir -p ${shellPath(env.workspace)}` })
  }

  if (program !== 'bash') {
    const dir = `~/needy-state/${name}`
    const paths = program === 'claude' ? dir : `${dir}/{data,config,state}`
    steps.push({ id: 'state', machine: 'host', terminal: host,
      title: { en: 'Create the program’s persistent directories', zh: '创建程序的持久目录' },
      text: { en: 'These host paths belong to the actor host, even when its executor is elsewhere.', zh: '这些 host 路径属于 actor 宿主机，即使命令在另一台机器上执行。' },
      code: `mkdir -p ${paths}\nchmod 700 ${dir}${program === 'opencode' ? ` ${paths}` : ''}` })
  }

  const profile = parse(PROFILES[program])
  for (const mapping of Object.values(profile.files ?? {}) as Array<{ from: string, to: string }>) {
    mapping.from = mapping.from.replace(`${program}-mac`, name)
    mapping.to = mapping.to.replace(`${program}-mac`, name)
  }
  profile.actor.driver = driver
  if (driver === 'linux-container')
    profile.actor.container = { oci_runtime: 'runc' }
  else
    delete profile.actor.container
  if (program === 'claude')
    profile.actor.image = 'needy-agents:latest'
  else
    delete profile.actor.image
  const project = env.executor === 'container' ? '~/workspace' : env.workspace
  profile.actor.workspace.from = `executor:${target}:${project}`
  profile.executors = { [target]: env.executor === 'runner'
    ? { runner: env.runnerName, driver: 'native' }
    : { driver: env.executor === 'container' ? 'linux-container' : 'native' } }
  profile.commands = [{ match: env.routing === 'all' ? '**' : '{bash,sh,dash,zsh} **', to: target }]
  profile.unknown = env.routing === 'all' ? target : 'actor'
  const yaml = '# Generated from examples/linux-mac for the selected environment.\n# Container capture covers image shims, not new unshimmed programs.\n' + stringify(profile, { lineWidth: 0 })
  steps.push({ id: 'profile', machine: 'host', terminal: host,
    title: { en: 'Save the selected profile', zh: '保存按选择生成的 profile' },
    text: { en: 'Save it on the actor host. The workspace derives both the mount and command path map.', zh: '保存在 actor 宿主机上。workspace 会同时生成共享挂载和命令路径映射。' },
    profile: yaml,
    code: `mkdir -p ~/.config/needy/profiles\ncat > ~/.config/needy/profiles/${name}.yaml <<'YAML'\n${yaml.trimEnd()}\nYAML`,
    expected: { en: `${name}: ${driver}, ${program}, executor ${target}.`, zh: `${name}：${driver}，入口 ${program}，executor 为 ${target}。` } },
  { id: 'start', machine: 'host', terminal: host,
    title: { en: `Start ${PROGRAMS.find(p => p.id === program)!.name}`, zh: `启动 ${PROGRAMS.find(p => p.id === program)!.name}` },
    text: { en: 'Check the profile, then start its entrypoint. Keep any runner and tunnel open.', zh: '先检查 profile，再启动入口程序。远端 runner 和隧道需要继续运行。' },
    code: `needy config profile check ${name} --mode control --cmd 'bash -c "pwd"'\nneedy run create -it --profile ${name}` })
  if (program === 'bash') {
    steps.push({ id: 'observe', machine: 'actor', terminal: { en: 'Inside the new bash', zh: '在刚进入的 bash 内' },
      title: { en: 'Compare the entrypoint and child shell', zh: '对比入口和子 shell 的执行位置' },
      text: { en: 'The entrypoint remains in the actor. Its child shell runs on the selected executor. Builtins stay in their shell.', zh: '入口留在 actor，子 shell 在所选 executor 上执行。内建命令仍由各自的 shell 执行。' },
      code: 'printf \'entrypoint=%s\\n\' "$OSTYPE"; bash -c \'uname -s; pwd\'' })
  }
  else {
    steps.push({ id: 'login', machine: 'actor', terminal: { en: 'Inside the program, not the host shell', zh: '在程序界面内，不是在宿主机 shell 内' },
      title: { en: 'Connect your model provider', zh: '登录模型服务' },
      text: program === 'claude'
        ? { en: 'Complete the first-start prompts. Open the printed login URL and return the code when requested.', zh: '完成首次启动向导，在浏览器打开输出的登录地址，按提示把验证码填回终端。' }
        : { en: 'Choose a provider, then complete its API-key or device-code login.', zh: '选择模型服务，按提示填写 API key 或完成设备码登录。' },
      code: program === 'claude' ? '/login' : '/connect' },
    { id: 'observe', machine: 'actor', terminal: { en: 'In the agent conversation', zh: '在 agent 对话内' },
      title: { en: 'Check a tool call and shared file', zh: '验证工具调用和共享文件' },
      text: { en: 'Check the actual tool output and the file on the selected executor.', zh: '查看实际工具输出，并在所选 executor 上查看同一份文件。' },
      prompt: { en: 'Create needy-check.txt with the text "shared workspace check" using your file editor. Then run uname -s and cat needy-check.txt with your shell tool. Show the exact output.', zh: '用文件编辑工具创建 needy-check.txt，内容为 shared workspace check。再用 shell 工具执行 uname -s 和 cat needy-check.txt，给我看实际输出。' } })
  }
  steps.push({ id: 'again', machine: 'host', terminal: host,
    title: { en: 'Return to the setup shell', zh: '回到配置终端' },
    text: { en: 'Ctrl-] detaches without stopping the run. Replace RUN_ID with its listed ID; run each lifecycle command separately.', zh: 'Ctrl-] 只断开终端，不会停止 run。把 RUN_ID 换成列表中的编号，接回、停止命令分别执行。' },
    code: 'needy run ls',
    expected: { en: 'Use needy run attach RUN_ID to return, and needy run stop RUN_ID after detaching to save state.', zh: '用 needy run attach RUN_ID 接回；再次断开后，用 needy run stop RUN_ID 正常停止并保存状态。' } })
  return steps
}
