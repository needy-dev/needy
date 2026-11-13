<script setup lang="ts">
import type { IconName } from '../icons'
import type { Rule } from '../route/rules'

import { SelectContent, SelectItem, SelectItemText, SelectPortal, SelectRoot, SelectTrigger, SelectValue, SelectViewport, Toggle } from 'reka-ui'
import { computed, ref } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Yaml from '../kit/Yaml.vue'

import { argvOf, decide } from '../route/rules'

/**
 * Route your own commands: edit a profile's `commands:` rules, type a command line, and see where needy would run it,
 * which rules matched and what they caught. The decision comes from src/route/rules.ts, the port of needy's rules;
 * `needy profile check PROFILE --cmd "..."` gives the same answer from the real daemon.
 */

interface EditableRule {
  match: string
  action: 'to' | 'deny'
  value: string
  final: boolean
}

// One actor and one executor, as a run has by default.
const rules = ref<EditableRule[]>([
  { match: '{bash,sh,zsh} **', action: 'to', value: 'build', final: false },
  { match: 'python3 **', action: 'to', value: 'build', final: false },
  { match: 'git push **', action: 'deny', value: 'no pushes from the agent', final: false },
  { match: '{uname,hostname} **', action: 'to', value: 'actor', final: false },
])

const command = ref('/usr/bin/python3 -m pytest -q')
const PRESETS = ['python3 -m pytest -q', 'bash -c "make test"', 'git push origin main', 'git status', 'hostname', 'ls -la']

const profileRules = computed<Rule[]>(() => rules.value.filter(rule => rule.match.trim()).map(rule => ({
  match: rule.match,
  ...(rule.action === 'deny' ? { deny: rule.value } : { to: rule.value || 'actor' }),
  ...(rule.final ? { final: true } : {}),
})))

/** Only the non-empty rows are rules, so a decision's indices map back to rows through this list. */
const indices = computed(() => rules.value.flatMap((rule, i) => (rule.match.trim() ? [i] : [])))
const argv = computed(() => argvOf(command.value))
const decision = computed(() => decide(profileRules.value, argv.value))
const decidedRow = computed(() => 'rule' in decision.value.outcome && decision.value.outcome.rule !== undefined ? indices.value[decision.value.outcome.rule] : undefined)

const result = computed<{ tone: 'executor' | 'actor' | 'bad', icon: IconName, title: string, detail: string, target: string }>(() => {
  const o = decision.value.outcome
  if (o.kind === 'deny') {
    return { tone: 'bad', icon: 'mingcute:forbid-circle-line', title: tr({ en: 'Denied', zh: '拒绝' }), detail: `needy: denied: ${o.message}`, target: `deny (${o.message})` }
  }
  if (o.on === 'actor') {
    return {
      tone: 'actor',
      icon: 'mingcute:terminal-box-ai-line',
      title: tr({ en: 'The actor', zh: 'Actor 本身' }),
      detail: o.rule === undefined ? tr({ en: 'No rule places it, so it runs where the harness runs.', zh: '没有规则放置它，所以它在 harness 所在的 actor 里运行。' }) : tr({ en: 'A rule keeps it in the actor.', zh: '规则把它留在 actor 里。' }),
      target: 'actor',
    }
  }
  return { tone: 'executor', icon: 'mingcute:cube-line', title: o.on, detail: tr({ en: 'Runs on this executor; the harness sees its output and exit code as usual.', zh: '在这个 executor 上运行；harness 照常拿到输出和退出码。' }), target: `executor ${o.on}` }
})

/** Why: each rule that matched, what it does, and what it caught. */
const why = computed(() => decision.value.matched.map((ruleIndex) => {
  const row = indices.value[ruleIndex]!
  const rule = profileRules.value[ruleIndex]!
  const m = decision.value.captures[ruleIndex]!
  const caught = [...Object.entries(m.captures).map(([name, value]) => `{${name}} = ${value}`), ...(m.rest.length ? [`** = ${m.rest.join(' ')}`] : [])]
  return { row, rule, caught, decided: row === decidedRow.value }
}))

const yaml = computed(() => ['commands:', ...profileRules.value.map((rule) => {
  const action = rule.deny !== undefined ? `deny: "${rule.deny}"` : `to: ${rule.to}`
  return `  - { match: "${rule.match}", ${action}${rule.final ? ', final: true' : ''} }`
})].join('\n'))

function add(): void {
  rules.value.push({ match: '', action: 'to', value: 'build', final: false })
}

function remove(i: number): void {
  rules.value.splice(i, 1)
}

/** An input or select of a rule row. */
const FIELD = 'h-[34px] min-w-0 rounded-[6px] border-0 bg-control px-[10px] py-0 text-[13.5px] text-ink'
/** A box of the right pane, on the surface. */
const BLOCK = 'flex flex-col gap-[8px] rounded-[12px] bg-surface px-[14px] py-[12px]'
</script>

<template>
  <div class="grid items-start gap-[16px] grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)] max-[900px]:grid-cols-[minmax(0,1fr)]">
    <!-- Corners nest: an inner box's radius is the outer radius minus the padding between them. -->
    <div class="flex min-w-0 flex-col gap-[8px]">
      <div class="flex items-center justify-between pb-[4px]">
        <span class="display text-[18px]">{{ tr({ en: 'Rules, in order', zh: '规则（按顺序）' }) }}</span>
        <button type="button" class="inline-flex cursor-pointer items-center gap-[6px] rounded-[12px] border-0 bg-surface px-[12px] py-[7px] text-[14px] text-ink" @click="add">
          <Icon name="mingcute:add-line" /> {{ tr({ en: 'Rule', zh: '规则' }) }}
        </button>
      </div>
      <ol class="m-0 flex list-none flex-col gap-[6px] p-0">
        <li
          v-for="(rule, i) in rules" :key="i" class="flex items-center gap-[6px] rounded-[12px] p-[6px] [transition:background_0.2s]"
          :class="decidedRow !== i ? 'bg-surface' : rule.action === 'deny' ? 'bg-[color-mix(in_srgb,var(--bad)_14%,var(--surface))]' : 'bg-[color-mix(in_srgb,var(--executor)_14%,var(--surface))]'"
        >
          <span class="mono w-[22px] flex-none text-center text-[12px] text-muted">{{ i }}</span>
          <input v-model="rule.match" class="mono flex-[2_1_0]" :class="FIELD" spellcheck="false" aria-label="match" placeholder="git **">
          <SelectRoot v-model="rule.action">
            <SelectTrigger class="mono inline-flex flex-none cursor-pointer items-center gap-[6px]" :class="FIELD" aria-label="action">
              <SelectValue />
              <Icon name="mingcute:down-line" />
            </SelectTrigger>
            <SelectPortal>
              <SelectContent class="z-50 max-h-[360px] overflow-auto rounded-[12px] bg-surface p-[4px] [box-shadow:var(--shadow-card)]" position="popper" :side-offset="4">
                <SelectViewport>
                  <!-- NOTICE: the old `.nd-menu-item { outline: none }` lost to style.css's `:focus-visible`, which loads after
                       the component styles, so the focused item showed the focus ring; it keeps it here. -->
                  <SelectItem v-for="a in ['to', 'deny']" :key="a" :value="a" class="mono cursor-pointer rounded-[8px] px-[12px] py-[7px] text-[13.5px] text-ink data-[highlighted]:bg-control">
                    <SelectItemText>{{ a }}</SelectItemText>
                  </SelectItem>
                </SelectViewport>
              </SelectContent>
            </SelectPortal>
          </SelectRoot>
          <input v-model="rule.value" class="mono flex-[1.3_1_0]" :class="FIELD" spellcheck="false" :aria-label="rule.action === 'deny' ? 'message' : 'executor'">
          <Toggle v-model="rule.final" class="mono h-[34px] flex-none cursor-pointer rounded-[6px] border-0 bg-control px-[10px] py-0 text-[13px] text-ink-2 data-[state=on]:(bg-actor-fill text-[#fff])" :aria-label="tr({ en: 'Stop at this rule', zh: '在这条规则停止' })">
            final
          </Toggle>
          <button type="button" class="grid h-[34px] w-[34px] flex-none cursor-pointer place-items-center rounded-[6px] border-0 bg-transparent text-muted hover:(bg-control text-ink)" :aria-label="tr({ en: 'Remove rule', zh: '删除规则' })" @click="remove(i)">
            <Icon name="mingcute:close-line" />
          </button>
        </li>
      </ol>
      <dl class="mx-0 mb-0 mt-[4px] grid gap-[6px] grid-cols-[repeat(4,minmax(0,1fr))] [&_div]:(flex flex-col gap-[2px] rounded-[12px] bg-surface px-[10px] py-[8px]) [&_dd]:(m-0 text-[13px] text-ink-2) [&_dt_code]:text-[14px]">
        <div><dt><code>*</code></dt><dd>{{ tr({ en: 'one argument', zh: '一个参数' }) }}</dd></div>
        <div><dt><code>**</code></dt><dd>{{ tr({ en: 'any number of arguments', zh: '任意多个参数' }) }}</dd></div>
        <div><dt><code>{a,b}</code></dt><dd>{{ tr({ en: 'one of', zh: '其中之一' }) }}</dd></div>
        <div><dt><code>{name}</code></dt><dd>{{ tr({ en: 'capture an argument', zh: '捕获一个参数' }) }}</dd></div>
      </dl>
      <p class="mx-0 mb-0 mt-[4px] text-[14px] leading-[1.55] text-ink-2">
        {{ tr({
          en: 'A first word without / matches the program\'s basename, so python3 matches /usr/bin/python3. Every matching rule applies: the first to and the first deny win, and a deny wins over a to. final stops at that rule.',
          zh: '第一个词不含 / 时匹配程序的 basename，所以 python3 能匹配 /usr/bin/python3。所有命中的规则都会生效：第一个 to 和第一个 deny 胜出，deny 优先于 to。final 会在该规则处停止。',
        }) }}
      </p>
    </div>

    <div class="flex min-w-0 flex-col gap-[8px]">
      <label class="flex items-center gap-[8px] rounded-[12px] bg-term px-[14px]">
        <span class="mono text-actor-term">$</span>
        <input v-model="command" class="mono h-[44px] min-w-0 flex-1 border-0 bg-transparent text-[15px] text-term-ink [outline:none]" spellcheck="false" :aria-label="tr({ en: 'Command line', zh: '命令行' })">
      </label>
      <div class="flex flex-wrap gap-[6px]">
        <button v-for="p in PRESETS" :key="p" type="button" class="mono cursor-pointer rounded-[999px] border-0 bg-surface px-[11px] py-[5px] text-[12.5px] text-ink hover:bg-control-2" @click="command = p">
          {{ p }}
        </button>
      </div>

      <div class="flex items-center gap-[14px] rounded-[12px] bg-surface p-[14px]" :style="{ '--tone': `var(--${result.tone})` }" aria-live="polite">
        <span class="grid h-[52px] w-[52px] flex-none place-items-center rounded-[12px] bg-tone/15 text-tone"><Icon :name="result.icon" :size="26" /></span>
        <div class="flex min-w-0 flex-col gap-[2px]">
          <span class="text-[14px] text-ink-2">{{ tr({ en: 'Runs on', zh: '运行于' }) }}</span>
          <strong class="display text-[28px] font-normal leading-[1.15] text-tone">{{ result.title }}</strong>
          <span class="text-ink-2" :class="result.tone === 'bad' ? 'mono text-[13px]' : 'text-[14px]'">{{ result.detail }}</span>
        </div>
      </div>

      <div :class="BLOCK">
        <span class="text-[13px] text-ink-2">{{ tr({ en: 'Why', zh: '原因' }) }}</span>
        <p v-if="!why.length" class="m-0 text-[13.5px] text-muted">
          {{ tr({ en: 'No rule matched.', zh: '没有规则命中。' }) }}
        </p>
        <ul class="m-0 flex list-none flex-col gap-[6px] p-0">
          <li v-for="w in why" :key="w.row" class="flex flex-wrap items-center gap-[8px] rounded-[6px] px-[8px] py-[6px] text-[13.5px]" :class="w.decided ? 'bg-[color-mix(in_srgb,var(--executor)_12%,var(--control))]' : 'bg-control'">
            <span class="mono w-[22px] flex-none text-center text-[12px] text-muted">{{ w.row }}</span>
            <code>{{ w.rule.match }}</code>
            <span class="mono text-[13px]" :class="w.rule.deny !== undefined ? 'text-bad' : 'text-executor'">{{ w.rule.deny !== undefined ? 'deny' : `to ${w.rule.to}` }}</span>
            <span v-if="!w.decided" class="m-0 text-[12.5px] text-muted">{{ tr({ en: 'matched, not first', zh: '命中，但不是第一个' }) }}</span>
            <span v-for="c in w.caught" :key="c" class="mono rounded-[6px] bg-surface px-[7px] py-[1px] text-[12.5px] text-ink-2">{{ c }}</span>
          </li>
        </ul>
      </div>

      <div :class="BLOCK">
        <span class="mono text-[13px] text-ink-2">needy profile check demo --cmd "…"</span>
        <div class="mono grid gap-x-[18px] gap-y-[4px] overflow-x-auto text-[13px] grid-cols-[minmax(0,1fr)_auto_auto]">
          <span class="text-[11.5px] tracking-[0.04em] text-muted">COMMAND</span><span class="text-[11.5px] tracking-[0.04em] text-muted">TARGET</span><span class="text-[11.5px] tracking-[0.04em] text-muted">RULES</span>
          <span class="overflow-hidden text-ellipsis whitespace-nowrap">{{ command }}</span><span>{{ result.target }}</span><span>{{ decision.matched.join(',') || '—' }}</span>
        </div>
      </div>

      <Yaml :code="yaml" :title="tr({ en: 'As a profile', zh: '写成 profile' })" />
    </div>
  </div>
</template>
