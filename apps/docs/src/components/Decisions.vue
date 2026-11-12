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
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  timer = setInterval(() => {
    if (!hovering.value)
      active.value = (active.value + 1) % ITEMS.length
  }, 2600)
})
onBeforeUnmount(() => clearInterval(timer))

const lit = computed(() => VERDICTS[active.value]!.lit)
const litRole = computed(() => VERDICTS[active.value]!.tone === 'bad' ? 'bad' as const : VERDICTS[active.value]!.tone === 'actor' ? 'actor' as const : 'executor' as const)
</script>

<template>
  <div class="decisions">
    <Yaml :code="PROFILE" :lit="lit" :role="litRole" title="examples/opencode/profile.yaml" class="profile" />

    <TooltipProvider :delay-duration="0">
      <ul class="items" @mouseleave="hovering = false">
        <li class="group-label">
          {{ tr({ en: 'What the harness runs', zh: 'harness 执行的命令' }) }}
        </li>
        <template v-for="(item, i) in ITEMS" :key="item.line">
          <li v-if="i === 4" class="group-label">
            {{ tr({ en: 'What it connects to', zh: '它发起的连接' }) }}
          </li>
          <li>
            <TooltipRoot :open="active === i" disable-hoverable-content>
              <TooltipTrigger as-child>
                <button
                  type="button" class="item mono" :class="[VERDICTS[i]!.tone, { on: active === i }]"
                  @mouseenter="active = i; hovering = true" @focus="active = i; hovering = true"
                >
                  <span class="prompt">{{ item.kind === 'exec' ? '$' : '→' }}</span>{{ item.line }}
                </button>
              </TooltipTrigger>
              <TooltipPortal>
                <TooltipContent class="nd-verdict" :class="VERDICTS[i]!.tone" side="bottom" :side-offset="6" align="start" :avoid-collisions="true">
                  <span class="nd-verdict-title"><Icon :name="VERDICTS[i]!.icon" :size="18" />{{ tr(VERDICTS[i]!.title) }}</span>
                  <dl>
                    <template v-for="f in VERDICTS[i]!.facts" :key="f.label.en">
                      <dt>{{ tr(f.label) }}</dt>
                      <dd class="mono">
                        {{ f.value }}
                      </dd>
                    </template>
                  </dl>
                  <TooltipArrow class="nd-verdict-arrow" :width="12" :height="6" />
                </TooltipContent>
              </TooltipPortal>
            </TooltipRoot>
          </li>
        </template>
      </ul>
    </TooltipProvider>
  </div>
</template>

<style scoped>
.decisions {
  display: grid;
  align-items: start;
  gap: 32px;
  grid-template-columns: minmax(0, 1.25fr) minmax(0, 1fr);
}

@media (max-width: 900px) {
  .decisions {
    grid-template-columns: minmax(0, 1fr);
  }
}

.profile :deep(pre) {
  font-size: 13px;
}

.items {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.group-label {
  margin: 14px 0 6px;
  color: var(--muted);
  font-size: 13px;
}

.group-label:first-child {
  margin-top: 0;
}

.item {
  display: flex;
  width: calc(100% + 24px);
  align-items: center;
  gap: 10px;
  /* The fill of the line in focus reaches past the text, so the text lines up with the labels above it. */
  margin-left: -12px;
  padding: 9px 12px;
  border: 0;
  border-radius: 10px;
  background: transparent;
  color: var(--ink);
  cursor: default;
  font-size: 14px;
  text-align: left;
  transition: background 0.25s;
  --tone: var(--executor);
}

.item.actor {
  --tone: var(--actor);
}

.item.bad {
  --tone: var(--bad);
}

.item.muted {
  --tone: var(--muted);
}

.item.on {
  background: color-mix(in srgb, var(--tone) 13%, transparent);
}

.prompt {
  color: var(--muted);
}

.item.on .prompt {
  color: var(--tone);
}
</style>

<style>
/* The verdict is a tooltip, in a portal. */
.nd-verdict {
  z-index: 40;
  display: flex;
  max-width: 340px;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px;
  border-radius: 12px;
  background: var(--tip-bg);
  box-shadow: 0 10px 28px rgb(0 0 0 / 22%);
  color: var(--tip-ink);
  font-size: 13.5px;
  animation: verdict-in 0.2s cubic-bezier(0.2, 0, 0, 1);
}

@keyframes verdict-in {
  from {
    opacity: 0;
    transform: translateX(-4px);
  }
}

.nd-verdict-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--font-display);
  font-size: 16px;
}

.nd-verdict.executor .nd-verdict-title {
  color: var(--executor-term);
}

.nd-verdict.actor .nd-verdict-title {
  color: var(--actor-term);
}

.nd-verdict.bad .nd-verdict-title {
  color: var(--bad-term);
}

.nd-verdict dl {
  display: grid;
  gap: 4px 12px;
  margin: 0;
  grid-template-columns: auto minmax(0, 1fr);
}

.nd-verdict dt {
  color: var(--tip-muted);
}

.nd-verdict dd {
  overflow-wrap: anywhere;
  margin: 0;
  font-size: 12.5px;
}

.nd-verdict-arrow {
  fill: var(--tip-bg);
}
</style>
