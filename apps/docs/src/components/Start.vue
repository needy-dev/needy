<script setup lang="ts">
import type { IconName } from '../icons'
import type { Text } from '../i18n'

import { ToggleGroupItem, ToggleGroupRoot } from 'reka-ui'
import { computed, ref } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Shell from '../kit/Shell.vue'
import Yaml from '../kit/Yaml.vue'

/**
 * Getting started as a few questions: where the harness runs, on what, and where its commands run. The answers pick
 * the actor driver and the executor, and the steps below show every command for that setup. The commands and the
 * per-host needs are the README's (Getting started, Install, Pick a profile, One command from your laptop, Add
 * machines); a step needy cannot do yet says so.
 */

type Where = 'here' | 'server'
type Os = 'linux' | 'mac'
type Answer = 'yes' | 'no'
type MacRuntime = 'vz' | 'apple'
type Exec = 'container' | 'native' | 'runner'
type Pm = 'apt' | 'dnf' | 'pacman'

const where = ref<Where>('here')
const os = ref<Os>('linux')
const kvm = ref<Answer>('yes')
const docker = ref<Answer>('yes')
const macRuntime = ref<MacRuntime>('vz')
const exec = ref<Exec>('container')
const pm = ref<Pm>('apt')

interface Choice<T> { value: T, label: Text, hint?: Text, icon?: IconName }

const WHERE: Choice<Where>[] = [
  { value: 'here', label: { en: 'This computer', zh: '这台电脑' }, icon: 'mingcute:laptop-line' },
  { value: 'server', label: { en: 'A server I reach over SSH', zh: '一台通过 SSH 访问的服务器' }, hint: { en: 'your laptop only runs the client', zh: '笔记本只运行客户端' }, icon: 'mingcute:server-2-line' },
]
const OS: Choice<Os>[] = [
  { value: 'linux', label: { en: 'Linux', zh: 'Linux' }, icon: 'mingcute:linux-line' },
  { value: 'mac', label: { en: 'macOS on Apple silicon', zh: 'Apple 芯片的 macOS' }, icon: 'mingcute:apple-line' },
]
const YES_NO: Choice<Answer>[] = [
  { value: 'yes', label: { en: 'Yes', zh: '有' } },
  { value: 'no', label: { en: 'No', zh: '没有' } },
]
const PM: Choice<Pm>[] = [
  { value: 'apt', label: { en: 'apt (Debian, Ubuntu)', zh: 'apt（Debian、Ubuntu）' } },
  { value: 'dnf', label: { en: 'dnf (Fedora, RHEL)', zh: 'dnf（Fedora、RHEL）' } },
  { value: 'pacman', label: { en: 'pacman (Arch)', zh: 'pacman（Arch）' } },
]

/** The install command of each Linux package manager. */
const INSTALL: Record<Pm, string> = { apt: 'sudo apt-get install -y', dnf: 'sudo dnf install -y', pacman: 'sudo pacman -S --noconfirm' }
/** sshfs is packaged as fuse-sshfs on Fedora. */
const SSHFS: Record<Pm, string> = { apt: 'sshfs', dnf: 'fuse-sshfs', pacman: 'sshfs' }

const MAC: Choice<MacRuntime>[] = [
  { value: 'vz', label: { en: 'Virtualization.framework (vfkit)', zh: 'Virtualization.framework（vfkit）' }, hint: { en: 'routes programs added at run time', zh: '能路由运行中新增的程序' } },
  { value: 'apple', label: { en: 'Apple container', zh: 'Apple container' }, hint: { en: 'Apple\'s runtime, its kernel', zh: 'Apple 的运行时和内核' } },
]
const EXEC: Choice<Exec>[] = [
  { value: 'container', label: { en: 'A container on the same machine', zh: '同一台机器上的容器' }, icon: 'mingcute:cube-line' },
  { value: 'native', label: { en: 'That machine itself', zh: '这台机器本身' }, icon: 'mingcute:computer-line' },
  { value: 'runner', label: { en: 'Another machine: a Mac, a Windows PC, a Linux box', zh: '另一台机器：Mac、Windows、Linux' }, icon: 'mingcute:server-2-line' },
]

const host = computed(() => where.value === 'server' ? tr({ en: 'the server', zh: '服务器' }) : tr({ en: 'this computer', zh: '这台电脑' }))

/** The actor driver the answers lead to, after the README's per-host table. */
const driver = computed(() => {
  if (os.value === 'mac')
    return macRuntime.value === 'vz' ? 'macos-vz' : 'macos-apple-container'
  return kvm.value === 'yes' ? 'linux-firecracker' : 'linux-container'
})

/** The drivers that boot needy's base VM image, and those that run a container image. */
const vmDriver = computed(() => driver.value === 'linux-firecracker' || driver.value === 'macos-vz')
const containerActor = computed(() => !vmDriver.value)

/** A daemon built from a checkout names the executor image `needy image build --executor` tagged. */
const executorImage = computed(() => exec.value === 'container' ? ' --executor-image needy-executor:latest' : '')

/** A Docker-compatible engine is needed for a container actor and for a container executor. */
const needsDocker = computed(() => driver.value === 'linux-container' || exec.value === 'container')

interface StepBlock {
  title: Text
  where?: Text
  note?: Text
  code?: string
  yaml?: string
  todo?: Text
}

const steps = computed<StepBlock[]>(() => {
  const list: StepBlock[] = []
  const on = { en: `On ${where.value === 'server' ? 'the server' : 'this computer'}`, zh: `在${where.value === 'server' ? '服务器' : '这台电脑'}上` }

  // 1. What the host needs, with the host's own package manager.
  const needs: string[] = []
  if (os.value === 'linux') {
    if (needsDocker.value && docker.value === 'no')
      needs.push('# Docker Engine', pm.value === 'pacman' ? `${INSTALL.pacman} docker && sudo systemctl enable --now docker` : 'curl -fsSL https://get.docker.com | sh')
    if (driver.value === 'linux-container')
      needs.push(...(needs.length ? [''] : []), '# workspace mounts and resource views', `${INSTALL[pm.value]} ${SSHFS[pm.value]} lxcfs`, 'echo user_allow_other | sudo tee -a /etc/fuse.conf')
  }
  else {
    needs.push(macRuntime.value === 'vz' ? '# vfkit runs the macos-vz actor' : '# Apple\'s container runs the actor', macRuntime.value === 'vz' ? 'brew install vfkit' : 'brew install container\ncontainer system start')
    if (exec.value === 'container' && docker.value === 'no')
      needs.push('', '# a Docker-compatible engine for the container executor', 'brew install --cask orbstack')
  }
  if (needs.length)
    list.push({ title: { en: 'Install what the host needs', zh: '安装宿主机需要的组件' }, where: on, code: needs.join('\n') })

  // 2. needy itself, the guest needy it brings into actors, and its images. No release has assets yet, so all three
  // come from a checkout (README, "Install" and "From source").
  const images: string[] = []
  if (vmDriver.value)
    images.push('needy image build                 # the base VM image')
  const flags = [containerActor.value ? `--${driver.value}` : '', exec.value === 'container' ? '--executor' : ''].filter(Boolean)
  if (flags.length)
    images.push(`needy image build ${flags.join(' ')}`)
  list.push({
    title: { en: 'Install needy and build its images', zh: '安装 needy 并构建镜像' },
    where: on,
    code: [
      'git clone https://github.com/needy-dev/needy && cd needy',
      'cargo install --path crates/cli',
      'make guest                         # the guest needy, built with Docker',
      'cp bin/needy-linux-* "$(dirname "$(command -v needy)")/"',
      ...images,
    ].join('\n'),
    note: {
      en: 'No release has binaries yet, so this builds from a checkout. From the next release on: brew install needy-dev/tap/needy on a Mac, or the install script on Linux, then needy image pull.',
      zh: '目前还没有发布二进制，所以先从源码构建。下一个版本起，Mac 上用 brew install needy-dev/tap/needy，Linux 上用安装脚本，再 needy image pull 就行。',
    },
  })

  // 3. The control daemon.
  list.push({
    title: { en: 'Start the control daemon', zh: '启动控制 daemon' },
    where: on,
    // A laptop or a runner reaches the daemon over TCP, through an SSH tunnel; otherwise its Unix socket is enough.
    code: where.value === 'server' || exec.value === 'runner'
      ? `export NEEDY_CONTROL_TOKEN=$(openssl rand -hex 32)\nneedy daemon --mode control${executorImage.value} --listen 127.0.0.1:7400 --token-env NEEDY_CONTROL_TOKEN &`
      : `needy daemon --mode control${executorImage.value} &`,
  })
  if (where.value === 'server') {
    list.push({
      title: { en: 'Pair your laptop once', zh: '让笔记本配对一次' },
      where: { en: 'On the server, then on your laptop', zh: '先在服务器上，再在笔记本上' },
      code: '# on the server\nNEEDY_TOKEN=$NEEDY_CONTROL_TOKEN needy pair create --name laptop\n\n# on the laptop: forward the port, then pair with the code\nssh -N -L 7400:127.0.0.1:7400 user@example.com &\nneedy --endpoint http://127.0.0.1:7400 pair CODE',
      note: { en: 'needy does not encrypt the connection itself; the SSH tunnel does.', zh: 'needy 自己不加密连接，由 SSH 隧道负责加密。' },
    })
  }

  // 4. Where commands run.
  let executor = ''
  let target = 'build'
  if (exec.value === 'container') {
    executor = 'executors:\n  build: { driver: linux-container, key: "{run}" }'
  }
  else if (exec.value === 'native') {
    executor = 'executors:\n  here: { driver: native, workspace: /tmp/needy-demo }'
    target = 'here'
  }
  else {
    executor = 'executors:\n  remote: { runner: other, workspace: ~/work }'
    target = 'remote'
    list.push({
      title: { en: 'Join the other machine as a runner', zh: '让另一台机器作为 runner 加入' },
      where: { en: 'On the control host, then on the other machine', zh: '先在控制主机上，再在另一台机器上' },
      code: '# on the control host: a join token\nneedy runner token create --ttl 1h --roles executor\n\n# on the other machine, with needy installed: reach the control host, then join\nssh -N -L 7400:127.0.0.1:7400 user@example.com &\nJOIN=... needy daemon --mode runner --name other --join http://127.0.0.1:7400 --join-token-env JOIN\n\n# back on the control host\nneedy runner ls',
      note: { en: 'The runner dials out through the tunnel, so it needs no open port. On Windows, start it outside the SSH session.', zh: 'runner 经隧道主动拨出，所以不需要开放端口。Windows 上请在 SSH 会话之外启动。' },
    })
  }
  const container = driver.value === 'linux-container' ? '\n  container: { oci_runtime: runc }' : ''
  // A container actor built from a checkout names the image `needy image build` tagged.
  const image = containerActor.value ? '\n  image: needy-actor:latest' : ''
  const recipe = os.value === 'linux' ? '\n  recipe: builtin:gce-ubuntu-22.04' : ''
  const yaml = `user: { name: agent, uid: 1000, gid: 1000 }\nactor:\n  driver: ${driver.value}${container}${image}${recipe}\n  command: [bash]\n${executor}\ncommands:\n  - { match: "python3 **", to: ${target} }`
  list.push({ title: { en: 'Write a profile', zh: '写一份 profile' }, where: { en: 'Save as ~/.config/needy/profiles/demo.yaml', zh: '保存为 ~/.config/needy/profiles/demo.yaml' }, yaml, note: exec.value === 'native' ? { en: 'Create /tmp/needy-demo first: the machine itself is the executor.', zh: '先创建 /tmp/needy-demo：这台机器本身就是 executor。' } : undefined })

  // 5. Run it.
  list.push({
    title: { en: 'Run bash in the actor', zh: '在 actor 里运行 bash' },
    where: where.value === 'server' ? { en: 'From your laptop', zh: '在笔记本上' } : on,
    code: 'needy profile check demo --cmd "python3 -V"\nneedy run create -it --profile demo\n#   agent@instance:~$ uname -a                      # the actor\n#   agent@instance:~$ python3 -c \'print(1 + 1)\'     # the executor\n#   agent@instance:~$ exit\nneedy report latest',
  })

  // 6. A coding agent, from examples/agents: one image with Claude Code, Codex, pi and opencode, and a profile each.
  const load = vmDriver.value
    ? ['needy image build --from needy-agents     # again after each rebuild']
    : driver.value === 'macos-apple-container'
      ? ['docker save needy-agents -o needy-agents.tar', 'container image load --input needy-agents.tar']
      : []
  list.push({
    title: { en: 'Then a coding agent', zh: '然后跑一个 coding agent' },
    where: on,
    code: [
      '# one image with Claude Code, Codex, pi and opencode',
      'docker build -t needy-agents examples/agents',
      ...load,
      'needy run create -it --profile examples/agents/claude.yaml   # or codex, pi, opencode',
    ].join('\n'),
    note: driver.value === 'linux-firecracker'
      ? undefined
      : { en: `Each profile uses linux-firecracker. Write a short profile that extends it and sets actor.driver: ${driver.value}; examples/agents/README.md has one for every driver.`, zh: `这些 profile 默认用 linux-firecracker。写一份短 profile，extends 它并把 actor.driver 改成 ${driver.value}；examples/agents/README.md 里每种 driver 都有例子。` },
  })
  return list
})

const copied = ref<number>()
async function copy(i: number, text: string): Promise<void> {
  await navigator.clipboard.writeText(text)
  copied.value = i
  setTimeout(() => (copied.value = undefined), 1400)
}
</script>

<template>
  <div class="start">
    <div class="questions">
      <div class="q">
        <span class="ask">{{ tr({ en: 'Where should the agent harness run?', zh: 'agent harness 运行在哪里？' }) }}</span>
        <ToggleGroupRoot v-model="where" type="single" class="choices">
          <ToggleGroupItem v-for="c in WHERE" :key="c.value" :value="c.value" class="choice">
            <Icon v-if="c.icon" :name="c.icon" /> {{ tr(c.label) }}
            <span v-if="c.hint" class="hint">{{ tr(c.hint) }}</span>
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>
      <div class="q">
        <span class="ask">{{ tr({ en: `What runs on ${host}?`, zh: `${host}上是什么系统？` }) }}</span>
        <ToggleGroupRoot v-model="os" type="single" class="choices">
          <ToggleGroupItem v-for="c in OS" :key="c.value" :value="c.value" class="choice">
            <Icon v-if="c.icon" :name="c.icon" /> {{ tr(c.label) }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
        <span class="aside">{{ tr({ en: 'Windows can be an executor, not an actor host.', zh: 'Windows 可以作为 executor，但不能运行 actor。' }) }}</span>
      </div>
      <div v-if="os === 'linux'" class="q">
        <span class="ask">{{ tr({ en: 'Which package manager?', zh: '用哪个包管理器？' }) }}</span>
        <ToggleGroupRoot v-model="pm" type="single" class="choices">
          <ToggleGroupItem v-for="c in PM" :key="c.value" :value="c.value" class="choice">
            {{ tr(c.label) }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>
      <div v-else class="q">
        <span class="ask">{{ tr({ en: 'Package manager', zh: '包管理器' }) }}</span>
        <span class="aside">{{ tr({ en: 'Homebrew, from brew.sh.', zh: 'Homebrew，见 brew.sh。' }) }}</span>
      </div>
      <div v-if="os === 'linux'" class="q">
        <span class="ask">{{ tr({ en: 'Does it have KVM?', zh: '它有 KVM 吗？' }) }}</span>
        <div class="check">
          <Shell code="ls -l /dev/kvm" class="check-code" />
          <button type="button" class="check-copy" :aria-label="tr({ en: 'Copy', zh: '复制' })" @click="copy(-1, 'ls -l /dev/kvm')">
            <Icon :name="copied === -1 ? 'mingcute:check-line' : 'mingcute:file-code-line'" />
          </button>
        </div>
        <span class="aside">{{ tr({ en: 'If it lists /dev/kvm, the answer is yes.', zh: '能列出 /dev/kvm 就是"有"。' }) }}</span>
        <ToggleGroupRoot v-model="kvm" type="single" class="choices">
          <ToggleGroupItem v-for="c in YES_NO" :key="c.value" :value="c.value" class="choice">
            {{ tr(c.label) }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>
      <div v-else class="q">
        <span class="ask">{{ tr({ en: 'Which Mac runtime?', zh: '用哪种 Mac 运行时？' }) }}</span>
        <ToggleGroupRoot v-model="macRuntime" type="single" class="choices">
          <ToggleGroupItem v-for="c in MAC" :key="c.value" :value="c.value" class="choice">
            {{ tr(c.label) }} <span v-if="c.hint" class="hint">{{ tr(c.hint) }}</span>
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>
      <div class="q">
        <span class="ask">{{ tr({ en: 'Where should its commands run?', zh: '它的命令在哪里执行？' }) }}</span>
        <ToggleGroupRoot v-model="exec" type="single" class="choices">
          <ToggleGroupItem v-for="c in EXEC" :key="c.value" :value="c.value" class="choice">
            <Icon v-if="c.icon" :name="c.icon" /> {{ tr(c.label) }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>
      <div v-if="needsDocker" class="q">
        <span class="ask">{{ tr({ en: 'Is Docker (or another Docker-compatible engine) installed?', zh: '装了 Docker（或其他兼容 Docker 的引擎）吗？' }) }}</span>
        <ToggleGroupRoot v-model="docker" type="single" class="choices">
          <ToggleGroupItem v-for="c in YES_NO" :key="c.value" :value="c.value" class="choice">
            {{ tr(c.label) }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>

      <p class="picked">
        {{ tr({ en: 'Your actor driver:', zh: '你的 actor driver：' }) }} <code>{{ driver }}</code>
      </p>
    </div>

    <ol class="steps">
      <li v-for="(s, i) in steps" :key="`${i}-${s.title.en}`" class="step" :class="{ todo: s.todo }">
        <span class="n mono">{{ s.todo ? '—' : i + 1 }}</span>
        <div class="body">
          <span class="title display">{{ tr(s.title) }}</span>
          <span v-if="s.where" class="where">{{ tr(s.where) }}</span>
          <div v-if="s.code" class="code">
            <Shell :code="s.code" class="block" />
            <button type="button" class="copy" :aria-label="tr({ en: 'Copy', zh: '复制' })" @click="copy(i, s.code)">
              <Icon :name="copied === i ? 'mingcute:check-line' : 'mingcute:file-code-line'" />
            </button>
          </div>
          <Yaml v-if="s.yaml" :code="s.yaml" />
          <span v-if="s.note" class="note">{{ tr(s.note) }}</span>
          <span v-if="s.todo" class="note">{{ tr(s.todo) }}</span>
        </div>
      </li>
    </ol>
  </div>
</template>

<style scoped>
.start {
  display: grid;
  align-items: start;
  gap: 56px;
  grid-template-columns: minmax(0, 0.85fr) minmax(0, 1.15fr);
}

@media (max-width: 960px) {
  .start {
    grid-template-columns: minmax(0, 1fr);
  }
}

/* Beside the steps the questions follow the reader down; stacked above them they must scroll away. */
@media (min-width: 961px) {
  .questions {
    position: sticky;
  }
}

.questions {
  top: 88px;
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.q {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.ask {
  font-family: var(--font-display);
  font-size: 18px;
}

.check {
  display: inline-flex;
  align-items: center;
  align-self: flex-start;
  gap: 10px;
  padding: 6px 6px 6px 14px;
  border-radius: 10px;
  background: var(--term);
  color: var(--term-ink);
}

.check-code {
  margin: 0;
  font-size: 13.5px;
}

.check-copy {
  display: grid;
  width: 28px;
  height: 28px;
  place-items: center;
  border: 0;
  border-radius: 7px;
  background: rgb(255 255 255 / 10%);
  color: var(--term-ink);
  cursor: pointer;
}

.choices {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.choice {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 9px 14px;
  border: 0;
  border-radius: 12px;
  background: var(--control);
  color: var(--ink-2);
  cursor: pointer;
  font-size: 14.5px;
  text-align: left;
  transition: background 0.2s;
}

.choice:hover {
  background: var(--control-2);
}

.choice[data-state='on'] {
  background: color-mix(in srgb, var(--actor) 16%, var(--control));
  color: var(--ink);
}

.hint {
  color: var(--muted);
  font-size: 13px;
}

.aside {
  color: var(--muted);
  font-size: 13px;
}

.picked {
  margin: 0;
  color: var(--ink-2);
  font-size: 15px;
}

.picked code {
  color: var(--actor);
}

.steps {
  display: flex;
  flex-direction: column;
  gap: 36px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.step {
  display: grid;
  gap: 16px;
  grid-template-columns: 28px minmax(0, 1fr);
}

.n {
  display: grid;
  width: 28px;
  height: 28px;
  place-items: center;
  border-radius: 8px;
  background: color-mix(in srgb, var(--actor) 14%, transparent);
  color: var(--actor);
  font-size: 13px;
}

.todo .n {
  background: var(--control);
  color: var(--muted);
}

.body {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 8px;
}

.title {
  font-size: 19px;
}

.todo .title {
  color: var(--muted);
}

.where {
  color: var(--muted);
  font-size: 13.5px;
}

.code {
  position: relative;
}

.block {
  overflow-x: auto;
  margin: 0;
  padding: 14px 48px 14px 16px;
  border-radius: 12px;
  background: var(--term);
  color: var(--term-ink);
  font-size: 13px;
  line-height: 1.6;
}

.copy {
  position: absolute;
  top: 8px;
  right: 8px;
  display: grid;
  width: 30px;
  height: 30px;
  place-items: center;
  border: 0;
  border-radius: 8px;
  /* Opaque, so a long line that scrolls under the button does not show through it. */
  background: color-mix(in srgb, var(--term-ink) 12%, var(--term));
  box-shadow: 0 0 0 6px var(--term);
  color: var(--term-ink);
  cursor: pointer;
}

.copy:hover {
  background: color-mix(in srgb, var(--term-ink) 20%, var(--term));
}

.note {
  color: var(--ink-2);
  font-size: 14px;
  line-height: 1.5;
}
</style>
