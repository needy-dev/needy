<script setup lang="ts">
import type { IconName } from '../icons'
import type { Text } from '../i18n'
import type { Environment, Os, Program } from '../setup/guide'

import { computed, reactive, ref } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Shell from '../kit/Shell.vue'
import Yaml from '../kit/Yaml.vue'
import { actorDriver, configurationIssue, DEFAULT_ENVIRONMENT, guide, needsDocker, PROGRAMS } from '../setup/guide'

const program = ref<Program>('bash')
const env = reactive<Environment>({ ...DEFAULT_ENVIRONMENT })
const driver = computed(() => actorDriver(env))
const dockerNeeded = computed(() => needsDocker(program.value, env))
const issue = computed(() => configurationIssue(env))
const steps = computed(() => guide(program.value, env))
const host = computed(() => env.where === 'server' ? tr({ en: 'the server', zh: '服务器' }) : tr({ en: 'this computer', zh: '这台电脑' }))
const copied = ref<string>()
const copyError = ref(false)

/** Copies one terminal context. Profiles copy as complete save commands. */
async function copy(id: string, text: string): Promise<void> {
  copyError.value = false
  try {
    await navigator.clipboard.writeText(text)
    copied.value = id
    setTimeout(() => { copied.value = undefined }, 1400)
  }
  catch {
    copyError.value = true
  }
}

const Q = 'flex flex-col gap-[10px]'
const ASK = 'font-display text-[18px]'
const CHOICES = 'flex flex-wrap gap-[6px]'
// Keep the original wizard's neutral chips and small selected accent.
const CHOICE = 'inline-flex cursor-pointer items-center gap-[8px] rounded-[12px] border-0 bg-control px-[14px] py-[9px] text-left text-[14.5px] text-ink-2 [transition:background_0.2s] [&:hover:not([data-state=on])]:bg-control-2 data-[state=on]:(bg-[color-mix(in_srgb,var(--actor)_16%,var(--control))] text-ink)'
const ASIDE = 'text-[13px] text-muted'
const INPUT = 'mono rounded-[9px] border-0 bg-control px-[12px] py-[9px] text-[13px] text-ink outline-offset-2'
type Where = Environment['where']
type Answer = 'yes' | 'no'
type Pm = Environment['pm']
type MacRuntime = Environment['macRuntime']
type Exec = Environment['executor']
interface Choice<T> { value: T, label: Text, hint?: Text, icon?: IconName }

const WHERE: Choice<Where>[] = [
  { value: 'here', label: { en: 'This computer', zh: '这台电脑' }, icon: 'mingcute:laptop-line' },
  { value: 'server', label: { en: 'A server I reach over SSH', zh: '一台通过 SSH 访问的服务器' }, hint: { en: 'host commands run in its SSH shell', zh: '宿主机命令在它的 SSH shell 中执行' }, icon: 'mingcute:server-2-line' },
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

const MAC: Choice<MacRuntime>[] = [
  { value: 'vz', label: { en: 'Virtualization.framework (vfkit)', zh: 'Virtualization.framework（vfkit）' }, hint: { en: 'routes programs added at run time', zh: '能路由运行中新增的程序' } },
  { value: 'apple', label: { en: 'Apple container', zh: 'Apple container' }, hint: { en: 'Apple\'s runtime, its kernel', zh: 'Apple 的运行时和内核' } },
]
const EXEC: Choice<Exec>[] = [
  { value: 'container', label: { en: 'A container on the same machine', zh: '同一台机器上的容器' }, icon: 'mingcute:cube-line' },
  { value: 'native', label: { en: 'That machine itself', zh: '这台机器本身' }, icon: 'mingcute:computer-line' },
  { value: 'runner', label: { en: 'Another machine: a Mac, a Windows PC, a Linux box', zh: '另一台机器：Mac、Windows、Linux' }, icon: 'mingcute:server-2-line' },
]

</script>

<template>
  <div class="grid items-start gap-[56px] grid-cols-[minmax(0,0.85fr)_minmax(0,1.15fr)] max-[960px]:grid-cols-[minmax(0,1fr)]">
    <!-- Keep the environment questions next to their generated instructions. -->
    <div class="top-[88px] flex flex-col gap-[28px] min-[961px]:(sticky max-h-[calc(100dvh-112px)] overflow-y-auto pr-[12px])">
      <div :class="Q">
        <span :class="ASK">{{ tr({ en: 'What should start in the actor?', zh: 'actor 里启动什么？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Entrypoint', zh: '入口程序' })">
          <button v-for="p in PROGRAMS" :key="p.id" type="button" :class="CHOICE" :data-state="program === p.id ? 'on' : 'off'" :aria-pressed="program === p.id" @click="program = p.id">{{ p.name }}</button>
        </div>
      </div>
      <div :class="Q">
        <span :class="ASK">{{ tr({ en: 'Where should the actor run?', zh: 'actor 运行在哪里？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Actor location', zh: 'actor 位置' })">
          <button v-for="c in WHERE" :key="c.value" type="button" :class="CHOICE" :data-state="env.where === c.value ? 'on' : 'off'" :aria-pressed="env.where === c.value" @click="env.where = c.value"><Icon v-if="c.icon" :name="c.icon" />{{ tr(c.label) }}<span v-if="c.hint" :class="ASIDE">{{ tr(c.hint) }}</span></button>
        </div>
      </div>
      <div :class="Q">
        <span :class="ASK">{{ tr({ en: `What runs on ${host}?`, zh: `${host}上是什么系统？` }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Actor host OS', zh: 'actor 宿主机系统' })">
          <button v-for="c in OS" :key="c.value" type="button" :class="CHOICE" :data-state="env.os === c.value ? 'on' : 'off'" :aria-pressed="env.os === c.value" @click="env.os = c.value"><Icon v-if="c.icon" :name="c.icon" />{{ tr(c.label) }}</button>
        </div>
        <span :class="ASIDE">{{ tr({ en: 'Windows runner enrollment is planned. Use Linux or macOS for this guide.', zh: 'Windows runner 注册尚在计划中。此教程请使用 Linux 或 macOS。' }) }}</span>
      </div>
      <div v-if="env.os === 'linux'" :class="Q">
        <span :class="ASK">{{ tr({ en: 'Which package manager?', zh: '用哪个包管理器？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Package manager', zh: '包管理器' })">
          <button v-for="c in PM" :key="c.value" type="button" :class="CHOICE" :data-state="env.pm === c.value ? 'on' : 'off'" :aria-pressed="env.pm === c.value" @click="env.pm = c.value">{{ tr(c.label) }}</button>
        </div>
      </div>
      <div v-if="env.os === 'linux'" :class="Q">
        <span :class="ASK">{{ tr({ en: 'Can this user access KVM?', zh: '当前用户能访问 KVM 吗？' }) }}</span>
        <Shell code="test -r /dev/kvm && test -w /dev/kvm" class="m-0 overflow-x-auto rounded-[10px] bg-term px-[14px] py-[10px] text-[12.5px]" />
        <span :class="ASIDE">{{ tr({ en: 'Choose Yes only if the command succeeds. Device existence alone is not enough.', zh: '这条命令成功才选「有」。仅仅存在设备文件还不够。' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'KVM access', zh: 'KVM 访问' })">
          <button v-for="c in YES_NO" :key="c.value" type="button" :class="CHOICE" :data-state="env.kvm === (c.value === 'yes') ? 'on' : 'off'" :aria-pressed="env.kvm === (c.value === 'yes')" @click="env.kvm = c.value === 'yes'">{{ tr(c.label) }}</button>
        </div>
      </div>
      <div v-else :class="Q">
        <span :class="ASK">{{ tr({ en: 'Which Mac runtime?', zh: '用哪种 Mac 运行时？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Mac runtime', zh: 'Mac 运行时' })">
          <button v-for="c in MAC" :key="c.value" type="button" :class="CHOICE" :data-state="env.macRuntime === c.value ? 'on' : 'off'" :aria-pressed="env.macRuntime === c.value" @click="env.macRuntime = c.value">{{ tr(c.label) }}<span v-if="c.hint" :class="ASIDE">{{ tr(c.hint) }}</span></button>
        </div>
        <span :class="ASIDE">{{ tr({ en: 'Package commands use Homebrew.', zh: '包安装命令使用 Homebrew。' }) }}</span>
      </div>
      <div :class="Q">
        <span :class="ASK">{{ tr({ en: 'Where should commands run?', zh: '命令在哪里执行？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Executor location', zh: 'executor 位置' })">
          <button v-for="c in EXEC" :key="c.value" type="button" :class="CHOICE" :data-state="env.executor === c.value ? 'on' : 'off'" :aria-pressed="env.executor === c.value" @click="env.executor = c.value"><Icon v-if="c.icon" :name="c.icon" />{{ tr(c.label) }}</button>
        </div>
      </div>
      <div v-if="env.executor === 'runner'" :class="Q">
        <span :class="ASK">{{ tr({ en: 'What runs on that machine?', zh: '那台机器是什么系统？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Runner OS', zh: 'runner 系统' })">
          <button v-for="c in (['mac', 'linux', 'windows'] as const)" :key="c" type="button" :class="CHOICE" :data-state="env.runnerOs === c ? 'on' : 'off'" :aria-pressed="env.runnerOs === c" @click="env.runnerOs = c">{{ c === 'mac' ? 'macOS' : c === 'linux' ? 'Linux' : 'Windows' }}</button>
        </div>
        <label class="flex flex-col gap-[8px] text-[13px] text-muted">{{ tr({ en: 'Runner name (use the existing name when already enrolled)', zh: 'Runner 名称（已加入时使用已有名称）' }) }}<input v-model="env.runnerName" :class="INPUT" autocomplete="off" spellcheck="false"></label>
      </div>
      <div v-if="dockerNeeded" :class="Q">
        <span :class="ASK">{{ tr({ en: 'Is Docker or a compatible engine installed?', zh: '装了 Docker 或兼容引擎吗？' }) }}</span>
        <div :class="CHOICES" role="group" :aria-label="tr({ en: 'Docker installed', zh: '已安装 Docker' })">
          <button v-for="c in YES_NO" :key="c.value" type="button" :class="CHOICE" :data-state="env.docker === (c.value === 'yes') ? 'on' : 'off'" :aria-pressed="env.docker === (c.value === 'yes')" @click="env.docker = c.value === 'yes'">{{ tr(c.label) }}</button>
        </div>
      </div>
      <details class="text-[14px] text-ink-2">
        <summary class="cursor-pointer">{{ tr({ en: 'Project directory and routing', zh: '项目目录和路由范围' }) }}</summary>
        <div class="mt-[16px] flex flex-col gap-[20px]">
          <label v-if="env.executor !== 'container'" class="flex flex-col gap-[8px] text-[13px] text-muted">{{ tr({ en: 'Project path on the executor', zh: 'executor 上的项目路径' }) }}<input v-model="env.workspace" :class="INPUT" spellcheck="false"></label>
          <div :class="Q"><span>{{ tr({ en: 'What should be forwarded?', zh: '转发哪些进程？' }) }}</span><div :class="CHOICES" role="group" :aria-label="tr({ en: 'Routing scope', zh: '路由范围' })"><button type="button" :class="CHOICE" :data-state="env.routing === 'all' ? 'on' : 'off'" :aria-pressed="env.routing === 'all'" @click="env.routing = 'all'">{{ tr({ en: 'Captured child processes', zh: '捕获到的所有子进程' }) }}</button><button type="button" :class="CHOICE" :data-state="env.routing === 'shells' ? 'on' : 'off'" :aria-pressed="env.routing === 'shells'" @click="env.routing = 'shells'">{{ tr({ en: 'Child shells only', zh: '只转发子 shell' }) }}</button></div></div>
        </div>
      </details>
      <p class="m-0 text-[15px] text-ink-2">{{ tr({ en: 'Your actor driver:', zh: '你的 actor driver：' }) }} <code>{{ driver }}</code></p>
      <p class="m-0 text-[13px] text-muted leading-[1.7]">{{ tr({ en: 'These steps manage runs from the actor host. If it is remote, use its SSH shell. An optional client context saves a connection and client identity. Runner enrollment uses its own invitation and credential.', zh: '下方命令在 actor 宿主机上管理 run。宿主机在远端时，使用它的 SSH shell。可选的客户端 context 保存连接和客户端身份。runner 注册使用自己的邀请码和凭据。' }) }}</p>
      <a href="https://github.com/needy-dev/needy#one-command-from-your-laptop" class="text-[13px] text-ink-2">{{ tr({ en: 'Optional client pairing and source builds ↗', zh: '可选客户端配对与源码构建 ↗' }) }}</a>
    </div>

    <div class="min-w-0">
      <p v-if="issue" role="alert" class="m-0 text-[14px] text-fault">{{ tr(issue) }}</p>
      <ol v-else class="m-0 flex list-none flex-col gap-[36px] p-0">
        <li v-for="(s, i) in steps" :id="`setup-${s.id}`" :key="s.id" class="grid scroll-mt-[90px] grid-cols-[28px_minmax(0,1fr)] gap-[16px]">
          <span class="mono grid h-[28px] w-[28px] place-items-center rounded-[8px] bg-control text-[13px] text-muted">{{ i + 1 }}</span>
          <div class="flex min-w-0 flex-col gap-[8px]">
            <span class="display text-[19px]">{{ tr(s.title) }}</span>
            <span class="text-[13.5px] text-muted">{{ tr(s.terminal) }}</span>
            <p class="m-0 text-[14px] text-ink-2 leading-[1.7]">{{ tr(s.text) }}</p>
            <div v-if="s.profile">
              <div class="max-h-[400px] overflow-auto rounded-[14px]"><Yaml :code="s.profile" class="min-w-max" /></div>
              <button type="button" class="mt-[10px] inline-flex cursor-pointer items-center gap-[7px] rounded-[9px] border-0 bg-control px-[12px] py-[8px] text-[13px] text-ink-2" @click="copy(s.id, s.code!)"><Icon :name="copied === s.id ? 'mingcute:check-line' : 'mingcute:file-code-line'" :size="16" />{{ tr({ en: 'Copy the command that saves this profile', zh: '复制保存这份 profile 的命令' }) }}</button>
              <details class="mt-[10px] text-[13px] text-muted"><summary class="cursor-pointer">{{ tr({ en: 'Show save command', zh: '查看保存命令' }) }}</summary><Shell :code="s.code!" class="overflow-x-auto rounded-[12px] bg-term p-[14px] text-[12px]" /></details>
            </div>
            <div v-else-if="s.code || s.prompt" class="relative">
              <Shell v-if="s.code" :code="s.code" class="m-0 overflow-x-auto rounded-[12px] bg-term pb-[14px] pl-[16px] pr-[48px] pt-[14px] text-[13px] text-term-ink leading-[1.6]" />
              <p v-else class="m-0 rounded-[12px] bg-control py-[14px] pl-[16px] pr-[48px] text-[14px] text-ink-2 leading-[1.7]">{{ tr(s.prompt) }}</p>
              <button type="button" class="absolute right-[8px] top-[8px] grid h-[30px] w-[30px] cursor-pointer place-items-center rounded-[7px] border-0 bg-control text-ink-2" :aria-label="tr({ en: 'Copy', zh: '复制' })" @click="copy(s.id, s.code ?? tr(s.prompt))"><Icon :name="copied === s.id ? 'mingcute:check-line' : 'mingcute:file-code-line'" /></button>
            </div>
            <span v-if="s.expected" class="text-[13px] text-muted leading-[1.6]">{{ tr({ en: 'Expected: ', zh: '应看到：' }) }}{{ tr(s.expected) }}</span>
          </div>
        </li>
      </ol>
      <p v-if="copyError" role="alert" class="text-[14px] text-fault">{{ tr({ en: 'Clipboard access failed. Copy the visible command manually.', zh: '无法访问剪贴板，请手动复制可见命令。' }) }}</p>
      <details class="mt-[28px] text-[13px] text-muted leading-[1.8]">
        <summary class="cursor-pointer">{{ tr({ en: 'Capture limits and verification status', zh: '捕获范围与验证情况' }) }}</summary>
        <p>{{ tr({ en: 'Container capture covers image shims. New unshimmed programs can stay in the actor. Builtins stay in their shell.', zh: '容器捕获覆盖镜像内的 shims，新增且未经过 shim 的程序可能留在 actor；内建命令仍在各自 shell 内执行。' }) }}</p>
        <p>{{ tr({ en: 'The entrypoint stays in Linux even with a Mac actor host. HTTP/HTTPS hosts are allowed. Agent approval policies stay unchanged.', zh: 'actor 宿主机是 Mac 时，入口也仍在 Linux actor 中运行。示例允许 HTTP/HTTPS 域名，不改变 agent 自己的审批策略。' }) }}</p>
        <p>{{ tr({ en: 'The Linux-container to Mac path has live-probe evidence. Other combinations have configuration checks; fresh installation and authenticated tool calls remain unverified.', zh: 'Linux 容器到 Mac 的路径有现场探测记录。其他组合只做配置检查，全新安装和登录后的工具调用仍待验证。' }) }}</p>
        <a href="https://github.com/needy-dev/needy/blob/main/docs/ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md#onboarding-evidence" class="text-ink-2">{{ tr({ en: 'Verification record ↗', zh: '验证记录 ↗' }) }}</a>
      </details>
    </div>
  </div>
</template>
