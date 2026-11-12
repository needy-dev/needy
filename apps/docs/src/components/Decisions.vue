<script setup lang="ts">
import type { IconName } from '../icons'
import type { Text } from '../i18n'
import type { NetPolicy } from '../route/network'
import type { Rule } from '../route/rules'

import { TooltipArrow, TooltipContent, TooltipPortal, TooltipProvider, TooltipRoot, TooltipTrigger } from 'reka-ui'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Yaml from '../kit/Yaml.vue'
import { allowedBy } from '../route/network'
import { basename, compile } from '../route/pattern'
import { argvOf, decide } from '../route/rules'

/**
 * How a decision is made, read off a real profile: the rules and the network policy of examples/opencode on the left,
 * what the harness does on the right. Hovering a line shows needy's decision for it; left alone, the lines take turns.
 * Exec decisions come from src/route/rules.ts, network ones from src/route/network.ts, both ports of needy's code.
 */

// examples/opencode/profile.yaml, cut to its executor, rules and network.
const PROFILE = `executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - match: "{bash,sh,dash,zsh} **"
    to: build
  - match: "{python3,python,pip,pip3,node,npm,npx,make,gcc,cc,cargo} **"
    to: build
  - match: "{uname,lscpu,hostname,nproc,free,uptime} **"
    to: actor
actor:
  network:
    allow: [models.dev, "*.models.dev", registry.npmjs.org, "*.npmjs.org", opencode.ai, "*.opencode.ai"]
    services:
      model: { url: "http://127.0.0.1:11434" }`

const RULES: Rule[] = [
  { match: '{bash,sh,dash,zsh} **', to: 'build' },
  { match: '{python3,python,pip,pip3,node,npm,npx,make,gcc,cc,cargo} **', to: 'build' },
  { match: '{uname,lscpu,hostname,nproc,free,uptime} **', to: 'actor' },
]
/** The YAML lines of rule `i`. */
const ruleLines = (i: number) => [3 + i * 2, 4 + i * 2]

const NETWORK: NetPolicy = {
  allow: ['models.dev', '*.models.dev', 'registry.npmjs.org', '*.npmjs.org', 'opencode.ai', '*.opencode.ai'],
  services: { model: 'http://127.0.0.1:11434' },
}

/**
 * The names the actor's kernel routes: the literal first words of the rules. An exec of any other program never
 * reaches needy and runs in the actor unrecorded (crates/core/src/rules/mod.rs, `route_names`).
 */
const ROUTE_NAMES = new Set(RULES.flatMap((rule) => {
  const first = compile(rule.match).tokens[0]
  if (first?.kind === 'literal')
    return [first.word]
  return first?.kind === 'alternatives' ? first.words : []
}))

interface Item {
  kind: 'exec' | 'net'
  line: string
  host?: string
}

const ITEMS: Item[] = [
  { kind: 'exec', line: 'bash -c "pytest -q"' },
  { kind: 'exec', line: 'npm install' },
  { kind: 'exec', line: 'uname -a' },
  { kind: 'exec', line: 'git status' },
  { kind: 'net', line: 'POST http://model.needy/v1/chat/completions', host: 'model.needy' },
  { kind: 'net', line: 'CONNECT registry.npmjs.org:443', host: 'registry.npmjs.org' },
  { kind: 'net', line: 'CONNECT example.com:443', host: 'example.com' },
]

interface Verdict {
  tone: 'executor' | 'actor' | 'bad' | 'muted'
  icon: IconName
  title: Text
  facts: Array<{ label: Text, value: string }>
  lit: number[]
}

function verdictOf(item: Item): Verdict {
  if (item.kind === 'net') {
    const by = allowedBy(NETWORK, item.host!)
    if (!by)
      return { tone: 'bad', icon: 'mingcute:forbid-circle-line', title: { en: 'Refused at the gateway', zh: '被 gateway 拒绝' }, facts: [{ label: { en: 'Why', zh: '原因' }, value: 'no allow pattern, no service' }, { label: { en: 'Answer', zh: '响应' }, value: '403' }, { label: { en: 'Event', zh: '事件' }, value: 'net.deny' }], lit: [11] }
    if (by.kind === 'service')
      return { tone: 'executor', icon: 'mingcute:route-line', title: { en: `Forwarded to the service ${by.name}`, zh: `转发到 service ${by.name}` }, facts: [{ label: { en: 'To', zh: '目标' }, value: by.url }, { label: { en: 'Event', zh: '事件' }, value: 'net.request' }], lit: [12, 13] }
    return { tone: 'executor', icon: 'mingcute:globe-lock-line', title: { en: 'Tunnelled', zh: '隧道放行' }, facts: [{ label: { en: 'Pattern', zh: '模式' }, value: by.pattern }, { label: { en: 'Inspected', zh: '解密' }, value: 'no: HTTPS stays end to end' }, { label: { en: 'Event', zh: '事件' }, value: 'net.connect' }], lit: [11] }
  }
  const argv = argvOf(item.line)
  if (!ROUTE_NAMES.has(basename(argv[0]!)))
    return { tone: 'muted', icon: 'mingcute:terminal-box-ai-line', title: { en: 'Not caught: runs in the actor', zh: '未被捕获：在 actor 里运行' }, facts: [{ label: { en: 'Why', zh: '原因' }, value: `no rule names ${argv[0]}` }, { label: { en: 'Event', zh: '事件' }, value: 'none' }], lit: [] }
  const d = decide(RULES, argv)
  const rule = d.outcome.kind === 'run' ? d.outcome.rule : d.outcome.rule
  const m = rule === undefined ? undefined : d.captures[rule]
  const facts: Verdict['facts'] = []
  if (rule !== undefined)
    facts.push({ label: { en: 'Rule', zh: '规则' }, value: `${rule} · ${RULES[rule]!.match}` })
  if (m?.rest.length)
    facts.push({ label: { en: 'Caught', zh: '捕获' }, value: `** = ${m.rest.join(' ')}` })
  facts.push({ label: { en: 'Event', zh: '事件' }, value: 'exec.decide' })
  if (d.outcome.kind === 'run' && d.outcome.on !== 'actor')
    return { tone: 'executor', icon: 'mingcute:cube-line', title: { en: `Runs on ${d.outcome.on}`, zh: `在 ${d.outcome.on} 上运行` }, facts, lit: rule === undefined ? [] : ruleLines(rule) }
  return { tone: 'actor', icon: 'mingcute:terminal-box-ai-line', title: { en: 'Kept in the actor', zh: '留在 actor 里' }, facts, lit: rule === undefined ? [] : ruleLines(rule) }
}

const VERDICTS = ITEMS.map(verdictOf)

const active = ref(0)
const hovering = ref(false)
/**
 * Whether the list is on screen. Off screen the verdict stays closed and the autoplay waits: an open tooltip
 * whose item scrolled away would otherwise float over the page, pinned to the viewport's edge.
 */
const root = ref<HTMLElement>()
const visible = ref(false)
let timer: ReturnType<typeof setInterval> | undefined
let observer: IntersectionObserver | undefined
onMounted(() => {
  observer = new IntersectionObserver(([entry]) => (visible.value = entry?.isIntersecting ?? false))
  observer.observe(root.value!)
  timer = setInterval(() => {
    if (visible.value && !hovering.value)
      active.value = (active.value + 1) % ITEMS.length
  }, 2600)
})
onBeforeUnmount(() => {
  clearInterval(timer)
  observer?.disconnect()
})

/** The verdict title takes the light (`-term`) shade of its role, which reads on the tooltip in both themes. */
const TITLE_TONE: Record<Verdict['tone'], string> = { executor: 'text-executor-term', actor: 'text-actor-term', bad: 'text-bad-term', muted: '' }

const lit = computed(() => VERDICTS[active.value]!.lit)
const litRole = computed(() => VERDICTS[active.value]!.tone === 'bad' ? 'bad' as const : VERDICTS[active.value]!.tone === 'actor' ? 'actor' as const : 'executor' as const)
</script>

<template>
  <div class="grid items-start gap-[32px] grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)] max-[900px]:grid-cols-[minmax(0,1fr)]">
    <Yaml :code="PROFILE" :lit="lit" :role="litRole" title="examples/opencode/profile.yaml" class="[&_pre]:text-[13px]" />

    <TooltipProvider :delay-duration="0">
      <ul ref="root" class="m-0 flex list-none flex-col gap-[4px] p-0" @mouseleave="hovering = false">
        <li class="mx-0 mb-[6px] mt-[14px] text-[13px] text-muted first:mt-0">
          {{ tr({ en: 'What the harness runs', zh: 'harness 执行的命令' }) }}
        </li>
        <template v-for="(item, i) in ITEMS" :key="item.line">
          <li v-if="i === 4" class="mx-0 mb-[6px] mt-[14px] text-[13px] text-muted first:mt-0">
            {{ tr({ en: 'What it connects to', zh: '它发起的连接' }) }}
          </li>
          <li>
            <TooltipRoot :open="visible && active === i" disable-hoverable-content>
              <TooltipTrigger as-child>
                <!-- The fill of the line in focus reaches past the text, so the text lines up with the labels above it. -->
                <button
                  type="button"
                  class="mono ml-[-12px] w-[calc(100%+24px)] flex cursor-default items-center gap-[10px] rounded-[10px] border-0 px-[12px] py-[9px] text-left text-[14px] text-ink [transition:background_0.25s]"
                  :class="active === i ? 'bg-tone/13' : 'bg-transparent'" :style="{ '--tone': `var(--${VERDICTS[i]!.tone})` }"
                  @mouseenter="active = i; hovering = true" @focus="active = i; hovering = true"
                >
                  <span :class="active === i ? 'text-tone' : 'text-muted'">{{ item.kind === 'exec' ? '$' : '→' }}</span>{{ item.line }}
                </button>
              </TooltipTrigger>
              <TooltipPortal>
                <TooltipContent class="z-[40] max-w-[340px] flex flex-col gap-[8px] rounded-[12px] bg-tip-bg px-[14px] py-[12px] text-[13.5px] text-tip-ink [animation:enter-left-4_0.2s_cubic-bezier(0.2,0,0,1)] [box-shadow:0_10px_28px_rgb(0_0_0/22%)]" side="bottom" :side-offset="6" align="start" :avoid-collisions="true">
                  <span class="flex items-center gap-[8px] font-display text-[16px]" :class="TITLE_TONE[VERDICTS[i]!.tone]"><Icon :name="VERDICTS[i]!.icon" :size="18" />{{ tr(VERDICTS[i]!.title) }}</span>
                  <dl class="m-0 grid gap-x-[12px] gap-y-[4px] grid-cols-[auto_minmax(0,1fr)]">
                    <template v-for="f in VERDICTS[i]!.facts" :key="f.label.en">
                      <dt class="text-tip-muted">{{ tr(f.label) }}</dt>
                      <dd class="mono m-0 text-[12.5px] [overflow-wrap:anywhere]">
                        {{ f.value }}
                      </dd>
                    </template>
                  </dl>
                  <TooltipArrow class="fill-tip-bg" :width="12" :height="6" />
                </TooltipContent>
              </TooltipPortal>
            </TooltipRoot>
          </li>
        </template>
      </ul>
    </TooltipProvider>
  </div>
</template>
