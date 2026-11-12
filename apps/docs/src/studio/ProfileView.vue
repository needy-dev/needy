<script setup lang="ts">
import type { Decision } from '../route/rules'
import type { ProfileView } from './model'

import { computed } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Machine from '../kit/Machine.vue'
import Pod from '../kit/Pod.vue'
import Sieve from '../kit/Sieve.vue'

import { focusOf } from '../kit/focus'
import { ACTOR_DRIVER_INFO } from './model'

/** A profile drawn as what it sets up: the actor, the rules between it and the executors, and the rest below. */
const props = defineProps<{ view: ProfileView, decision?: Decision }>()

const driver = computed(() => ACTOR_DRIVER_INFO[props.view.actor.driver])

/** Where the command being tried goes: an executor's name, `actor`, or `deny`. */
const target = computed(() => {
  const d = props.decision
  if (!d)
    return undefined
  return d.outcome.kind === 'deny' ? 'deny' : d.outcome.on
})
const lit = computed(() => {
  const d = props.decision
  if (!d)
    return undefined
  return 'rule' in d.outcome && d.outcome.rule !== undefined ? d.outcome.rule : -1
})

const MODE_ICON = { 'copy-in': 'mingcute:down-line', 'mount': 'mingcute:folder-open-line', 'mirror-out': 'mingcute:refresh-2-line' } as const
</script>

<template>
  <div class="view">
    <div class="top">
      <Pod flow :logo="driver?.logo" :driver="view.actor.driver || '—'" :focus="target === 'actor' ? 'focus' : target === 'deny' ? 'bad' : 'idle'">
        <dl class="facts">
          <template v-if="driver">
            <dt>{{ tr({ en: 'Isolation', zh: '隔离' }) }}</dt>
            <dd>{{ driver.isolation }}<template v-if="view.actor.runtime">, <code>{{ view.actor.runtime }}</code></template></dd>
          </template>
          <template v-if="view.actor.recipe">
            <dt>{{ tr({ en: 'Recipe', zh: 'Recipe' }) }}</dt>
            <dd><Icon name="mingcute:fingerprint-line" /> <code>{{ view.actor.recipe }}</code></dd>
          </template>
          <template v-if="view.actor.command">
            <dt>{{ tr({ en: 'Program', zh: '驱动程序' }) }}</dt>
            <dd><code class="clip">{{ view.actor.command }}</code></dd>
          </template>
          <dt>{{ tr({ en: 'Runs on', zh: '运行于' }) }}</dt>
          <dd>{{ view.actor.runner ? view.actor.runner : tr({ en: 'the control host', zh: '控制主机' }) }}</dd>
          <dt>{{ tr({ en: 'Network', zh: '网络' }) }}</dt>
          <dd class="chips">
            <span v-if="!view.actor.allow.length && !view.actor.services.length" class="muted">{{ tr({ en: 'nothing outside is allowed', zh: '不允许访问任何外部主机' }) }}</span>
            <code v-for="host in view.actor.allow" :key="host" class="chip">{{ host }}</code>
            <code v-for="u in view.actor.services" :key="u.host" class="chip">{{ u.host }} → {{ u.url }}</code>
          </dd>
        </dl>
      </Pod>

      <div class="right">
      <Sieve v-if="view.rules.length" flow :rules="view.rules" :lit="lit" :title="tr({ en: 'Rules', zh: '规则' })" class="sieve" />
      <div v-else class="norules">
        {{ tr({ en: 'No rules: every command runs in the actor.', zh: '没有规则：所有命令都在 actor 中运行。' }) }}
      </div>

      <div class="executors">
        <Machine
          v-for="e in view.executors" :key="e.name" flow :icon="e.icon" :title="e.title" :name="e.name"
          :sub="[e.driver, e.key && `key ${e.key}`, e.image].filter(Boolean).join(' · ')" :focus="target === undefined ? 'idle' : focusOf(e.name, [target])"
        />
        <p v-if="!view.executors.length" class="muted">
          {{ tr({ en: 'No executors.', zh: '没有 executor。' }) }}
        </p>
        <div v-if="target === 'deny'" class="denied">
          <Icon name="mingcute:forbid-circle-line" /> {{ tr({ en: 'Denied: exits 126', zh: '被拒绝：退出码 126' }) }}
        </div>
      </div>
      </div>
    </div>

    <div class="bottom">
      <section class="card">
        <h4><Icon name="mingcute:folder-open-line" /> {{ tr({ en: 'Files', zh: '文件' }) }}</h4>
        <p v-if="!view.files.length" class="muted">
          {{ tr({ en: 'No file mappings.', zh: '没有文件映射。' }) }}
        </p>
        <ul>
          <li v-for="f in view.files" :key="f.name">
            <span class="mode" :class="f.mode"><Icon :name="MODE_ICON[f.mode as keyof typeof MODE_ICON] ?? 'mingcute:file-code-line'" />{{ f.mode }}</span>
            <code class="clip">{{ f.from }} → {{ f.to }}</code>
            <span v-if="!f.run" class="muted small">{{ tr({ en: 'by rule', zh: '按规则' }) }}</span>
          </li>
        </ul>
      </section>
      <section class="card">
        <h4><Icon name="mingcute:fire-line" /> {{ tr({ en: 'Chaos', zh: '故障注入' }) }} <code v-if="view.chaos.seed !== undefined" class="seed">seed {{ view.chaos.seed }}</code></h4>
        <p v-if="!view.chaos.faults.length" class="muted">
          {{ tr({ en: 'No faults.', zh: '没有故障。' }) }}
        </p>
        <ul>
          <li v-for="f in view.chaos.faults" :key="f.name" :title="f.name">
            <span class="mode fault">{{ f.on }}</span><code class="clip">{{ f.text }}</code>
          </li>
        </ul>
      </section>
      <section class="card">
        <h4><Icon name="mingcute:git-branch-line" /> {{ tr({ en: 'Trace', zh: 'Trace' }) }}</h4>
        <ul>
          <li v-for="sink in view.trace" :key="sink">
            <code>{{ sink === 'sqlite' ? 'trace.db' : 'trace.jsonl' }}</code>
          </li>
        </ul>
      </section>
    </div>
  </div>
</template>

<style scoped>
.view {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.top {
  display: grid;
  align-items: start;
  gap: 16px;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
}

/* The rules, and the executors they send to, stacked beside the actor. */
.right {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 16px;
}

@media (max-width: 900px) {
  .top {
    grid-template-columns: minmax(0, 1fr);
  }
}

.facts {
  display: grid;
  gap: 6px 12px;
  margin: 0;
  font-size: 13.5px;
  grid-template-columns: auto minmax(0, 1fr);
}

dt {
  color: var(--muted);
}

dd {
  min-width: 0;
  margin: 0;
}

code {
  font-size: 12.5px;
}

.clip {
  display: inline-block;
  overflow: hidden;
  max-width: 100%;
  text-overflow: ellipsis;
  vertical-align: bottom;
  white-space: nowrap;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.chip {
  padding: 1px 7px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--actor) 12%, transparent);
}

.chip.key {
  background: color-mix(in srgb, var(--gateway) 14%, transparent);
}

.sieve :deep(.slots) {
  justify-content: flex-start;
}

.norules {
  padding: 16px;
  border-radius: 18px;
  background: var(--control);
  color: var(--muted);
  font-size: 14px;
}

.executors {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.denied {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 14px;
  border-radius: 14px;
  background: color-mix(in srgb, var(--bad) 14%, var(--surface));
  color: var(--bad);
}

.bottom {
  display: grid;
  gap: 16px;
  grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr) minmax(0, 0.6fr);
}

@media (max-width: 900px) {
  .bottom {
    grid-template-columns: minmax(0, 1fr);
  }
}

.card {
  padding: 14px 16px;
  border-radius: 16px;
  background: var(--surface);
}

h4 {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0 0 8px;
  font-family: var(--font-display);
  font-size: 16px;
  font-weight: 400;
}

.seed {
  margin-left: auto;
  color: var(--muted);
}

ul {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

li {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 8px;
}

.mode {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: 7px;
  background: color-mix(in srgb, var(--executor) 13%, transparent);
  color: var(--executor);
  font-family: var(--font-mono);
  font-size: 12px;
}

.mode.fault {
  background: color-mix(in srgb, var(--fault) 15%, transparent);
  color: var(--fault);
}

.muted {
  margin: 0;
  color: var(--muted);
  font-size: 13.5px;
}

.small {
  font-size: 12px;
}
</style>
