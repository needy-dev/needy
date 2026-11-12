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
</script>

<template>
  <div class="playground">
    <div class="pane">
      <div class="head">
        <span class="title display">{{ tr({ en: 'Rules, in order', zh: '规则（按顺序）' }) }}</span>
        <button type="button" class="button" @click="add">
          <Icon name="mingcute:add-line" /> {{ tr({ en: 'Rule', zh: '规则' }) }}
        </button>
      </div>
      <ol class="rules">
        <li v-for="(rule, i) in rules" :key="i" :class="{ decided: decidedRow === i, deny: rule.action === 'deny' }">
          <span class="n mono">{{ i }}</span>
          <input v-model="rule.match" class="field mono match" spellcheck="false" aria-label="match" placeholder="git **">
          <SelectRoot v-model="rule.action">
            <SelectTrigger class="field select mono" aria-label="action">
              <SelectValue />
              <Icon name="mingcute:down-line" />
            </SelectTrigger>
            <SelectPortal>
              <SelectContent class="nd-menu" position="popper" :side-offset="4">
                <SelectViewport>
                  <SelectItem v-for="a in ['to', 'deny']" :key="a" :value="a" class="nd-menu-item mono">
                    <SelectItemText>{{ a }}</SelectItemText>
                  </SelectItem>
                </SelectViewport>
              </SelectContent>
            </SelectPortal>
          </SelectRoot>
          <input v-model="rule.value" class="field mono value" spellcheck="false" :aria-label="rule.action === 'deny' ? 'message' : 'executor'">
          <Toggle v-model="rule.final" class="final mono" :aria-label="tr({ en: 'Stop at this rule', zh: '在这条规则停止' })">
            final
          </Toggle>
          <button type="button" class="remove" :aria-label="tr({ en: 'Remove rule', zh: '删除规则' })" @click="remove(i)">
            <Icon name="mingcute:close-line" />
          </button>
        </li>
      </ol>
      <dl class="legend">
        <div><dt><code>*</code></dt><dd>{{ tr({ en: 'one argument', zh: '一个参数' }) }}</dd></div>
        <div><dt><code>**</code></dt><dd>{{ tr({ en: 'any number of arguments', zh: '任意多个参数' }) }}</dd></div>
        <div><dt><code>{a,b}</code></dt><dd>{{ tr({ en: 'one of', zh: '其中之一' }) }}</dd></div>
        <div><dt><code>{name}</code></dt><dd>{{ tr({ en: 'capture an argument', zh: '捕获一个参数' }) }}</dd></div>
      </dl>
      <p class="hint">
        {{ tr({
          en: 'A first word without / matches the program\'s basename, so python3 matches /usr/bin/python3. Every matching rule applies: the first to and the first deny win, and a deny wins over a to. final stops at that rule.',
          zh: '第一个词不含 / 时匹配程序的 basename，所以 python3 能匹配 /usr/bin/python3。所有命中的规则都会生效：第一个 to 和第一个 deny 胜出，deny 优先于 to。final 会在该规则处停止。',
        }) }}
      </p>
    </div>

    <div class="pane">
      <label class="command">
        <span class="prompt mono">$</span>
        <input v-model="command" class="mono" spellcheck="false" :aria-label="tr({ en: 'Command line', zh: '命令行' })">
      </label>
      <div class="presets">
        <button v-for="p in PRESETS" :key="p" type="button" class="chip mono" @click="command = p">
          {{ p }}
        </button>
      </div>

      <div class="result" :class="result.tone" aria-live="polite">
        <span class="tile"><Icon :name="result.icon" :size="26" /></span>
        <div class="what">
          <span class="label">{{ tr({ en: 'Runs on', zh: '运行于' }) }}</span>
          <strong class="display">{{ result.title }}</strong>
          <span class="detail" :class="{ mono: result.tone === 'bad' }">{{ result.detail }}</span>
        </div>
      </div>

      <div class="block">
        <span class="block-title">{{ tr({ en: 'Why', zh: '原因' }) }}</span>
        <p v-if="!why.length" class="muted">
          {{ tr({ en: 'No rule matched.', zh: '没有规则命中。' }) }}
        </p>
        <ul class="why">
          <li v-for="w in why" :key="w.row" :class="{ decided: w.decided }">
            <span class="n mono">{{ w.row }}</span>
            <code>{{ w.rule.match }}</code>
            <span class="does mono" :class="{ deny: w.rule.deny !== undefined }">{{ w.rule.deny !== undefined ? 'deny' : `to ${w.rule.to}` }}</span>
            <span v-if="!w.decided" class="muted small">{{ tr({ en: 'matched, not first', zh: '命中，但不是第一个' }) }}</span>
            <span v-for="c in w.caught" :key="c" class="caught mono">{{ c }}</span>
          </li>
        </ul>
      </div>

      <div class="block">
        <span class="block-title mono">needy profile check demo --cmd "…"</span>
        <div class="table mono">
          <span class="th">COMMAND</span><span class="th">TARGET</span><span class="th">RULES</span>
          <span class="clip">{{ command }}</span><span>{{ result.target }}</span><span>{{ decision.matched.join(',') || '—' }}</span>
        </div>
      </div>

      <Yaml :code="yaml" :title="tr({ en: 'As a profile', zh: '写成 profile' })" />
    </div>
  </div>
</template>

<style scoped>
.playground {
  display: grid;
  align-items: start;
  gap: 16px;
  grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
}

@media (max-width: 900px) {
  .playground {
    grid-template-columns: minmax(0, 1fr);
  }
}

/* Corners nest: an inner box's radius is the outer radius minus the padding between them. */
.pane {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 8px;
}

.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 0 4px;
}

.title {
  font-size: 18px;
}

.button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 12px;
  border: 0;
  border-radius: 12px;
  background: var(--surface);
  color: var(--ink);
  cursor: pointer;
  font-size: 14px;
}

.rules {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.rules li {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px;
  border-radius: 12px;
  background: var(--surface);
  transition: background 0.2s;
}

.rules li.decided {
  background: color-mix(in srgb, var(--executor) 14%, var(--surface));
}

.rules li.decided.deny {
  background: color-mix(in srgb, var(--bad) 14%, var(--surface));
}

.n {
  width: 22px;
  flex: none;
  color: var(--muted);
  font-size: 12px;
  text-align: center;
}

.field {
  min-width: 0;
  height: 34px;
  padding: 0 10px;
  border: 0;
  border-radius: 6px;
  background: var(--control);
  color: var(--ink);
  font-size: 13.5px;
}

.match {
  flex: 2 1 0;
}

.value {
  flex: 1.3 1 0;
}

.select {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 6px;
  cursor: pointer;
}

.final {
  flex: none;
  height: 34px;
  padding: 0 10px;
  border: 0;
  border-radius: 6px;
  background: var(--control);
  color: var(--ink-2);
  cursor: pointer;
  font-size: 13px;
}

.final[data-state='on'] {
  background: var(--actor-fill);
  color: #fff;
}

.remove {
  display: grid;
  width: 34px;
  height: 34px;
  flex: none;
  place-items: center;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}

.remove:hover {
  background: var(--control);
  color: var(--ink);
}

.legend {
  display: grid;
  gap: 6px;
  margin: 4px 0 0;
  grid-template-columns: repeat(4, minmax(0, 1fr));
}

.legend div {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  border-radius: 12px;
  background: var(--surface);
}

.legend dt code {
  font-size: 14px;
}

.legend dd {
  margin: 0;
  color: var(--ink-2);
  font-size: 13px;
}

.hint {
  margin: 4px 0 0;
  color: var(--ink-2);
  font-size: 14px;
  line-height: 1.55;
}

.command {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 14px;
  border-radius: 12px;
  background: var(--term);
}

.command .prompt {
  color: var(--actor-term);
}

.command input {
  flex: 1;
  min-width: 0;
  height: 44px;
  border: 0;
  background: transparent;
  color: var(--term-ink);
  font-size: 15px;
  outline: none;
}

.presets {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.chip {
  padding: 5px 11px;
  border: 0;
  border-radius: 999px;
  background: var(--surface);
  color: var(--ink);
  cursor: pointer;
  font-size: 12.5px;
}

.chip:hover {
  background: var(--control-2);
}

.result {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 14px;
  border-radius: 12px;
  background: var(--surface);
  --tone: var(--executor);
}

.result.actor {
  --tone: var(--actor);
}

.result.bad {
  --tone: var(--bad);
}

.tile {
  display: grid;
  width: 52px;
  height: 52px;
  flex: none;
  place-items: center;
  border-radius: 12px;
  background: color-mix(in srgb, var(--tone) 15%, transparent);
  color: var(--tone);
}

.what {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 2px;
}

.label {
  color: var(--ink-2);
  font-size: 14px;
}

.result strong {
  color: var(--tone);
  font-size: 28px;
  font-weight: 400;
  line-height: 1.15;
}

.detail {
  color: var(--ink-2);
  font-size: 14px;
}

.detail.mono {
  font-size: 13px;
}

.block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px;
  border-radius: 12px;
  background: var(--surface);
}

.block-title {
  color: var(--ink-2);
  font-size: 13px;
}

.why {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.why li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--control);
  font-size: 13.5px;
}

.why li.decided {
  background: color-mix(in srgb, var(--executor) 12%, var(--control));
}

.does {
  color: var(--executor);
  font-size: 13px;
}

.does.deny {
  color: var(--bad);
}

.caught {
  padding: 1px 7px;
  border-radius: 6px;
  background: var(--surface);
  color: var(--ink-2);
  font-size: 12.5px;
}

.table {
  display: grid;
  gap: 4px 18px;
  overflow-x: auto;
  font-size: 13px;
  grid-template-columns: minmax(0, 1fr) auto auto;
}

.th {
  color: var(--muted);
  font-size: 11.5px;
  letter-spacing: 0.04em;
}

.clip {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.muted {
  margin: 0;
  color: var(--muted);
  font-size: 13.5px;
}

.small {
  font-size: 12.5px;
}
</style>
