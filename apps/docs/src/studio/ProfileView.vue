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
  <div class="flex flex-col gap-[16px]">
    <div class="grid items-start gap-[16px] grid-cols-[minmax(0,1fr)_minmax(0,1fr)] max-[900px]:grid-cols-[minmax(0,1fr)]">
      <Pod flow :logo="driver?.logo" :driver="view.actor.driver || '—'" :focus="target === 'actor' ? 'focus' : target === 'deny' ? 'bad' : 'idle'">
        <dl class="m-0 grid gap-x-[12px] gap-y-[6px] text-[13.5px] grid-cols-[auto_minmax(0,1fr)]">
          <template v-if="driver">
            <dt class="text-muted">{{ tr({ en: 'Isolation', zh: '隔离' }) }}</dt>
            <dd class="m-0 min-w-0">{{ driver.isolation }}<template v-if="view.actor.runtime">, <code class="text-[12.5px]">{{ view.actor.runtime }}</code></template></dd>
          </template>
          <template v-if="view.actor.recipe">
            <dt class="text-muted">{{ tr({ en: 'Recipe', zh: 'Recipe' }) }}</dt>
            <dd class="m-0 min-w-0"><Icon name="mingcute:fingerprint-line" /> <code class="text-[12.5px]">{{ view.actor.recipe }}</code></dd>
          </template>
          <template v-if="view.actor.command">
            <dt class="text-muted">{{ tr({ en: 'Program', zh: '驱动程序' }) }}</dt>
            <dd class="m-0 min-w-0"><code class="inline-block max-w-full overflow-hidden text-ellipsis whitespace-nowrap align-bottom text-[12.5px]">{{ view.actor.command }}</code></dd>
          </template>
          <dt class="text-muted">{{ tr({ en: 'Runs on', zh: '运行于' }) }}</dt>
          <dd class="m-0 min-w-0">{{ view.actor.runner ? view.actor.runner : tr({ en: 'the control host', zh: '控制主机' }) }}</dd>
          <dt class="text-muted">{{ tr({ en: 'Network', zh: '网络' }) }}</dt>
          <dd class="m-0 flex min-w-0 flex-wrap gap-[4px]">
            <span v-if="!view.actor.allow.length && !view.actor.services.length" class="m-0 text-[13.5px] text-muted">{{ tr({ en: 'nothing outside is allowed', zh: '不允许访问任何外部主机' }) }}</span>
            <code v-for="host in view.actor.allow" :key="host" class="rounded-[6px] px-[7px] py-[1px] bg-actor/12 text-[12.5px]">{{ host }}</code>
            <code v-for="u in view.actor.services" :key="u.host" class="rounded-[6px] px-[7px] py-[1px] bg-actor/12 text-[12.5px]">{{ u.host }} → {{ u.url }}</code>
          </dd>
        </dl>
      </Pod>

      <!-- The rules, and the executors they send to, stacked beside the actor. -->
      <div class="flex min-w-0 flex-col gap-[16px]">
      <Sieve v-if="view.rules.length" flow :rules="view.rules" :lit="lit" :title="tr({ en: 'Rules', zh: '规则' })" class="[&_.slots]:justify-start" />
      <div v-else class="rounded-[18px] p-[16px] bg-control text-[14px] text-muted">
        {{ tr({ en: 'No rules: every command runs in the actor.', zh: '没有规则：所有命令都在 actor 中运行。' }) }}
      </div>

      <div class="flex flex-col gap-[10px]">
        <Machine
          v-for="e in view.executors" :key="e.name" flow :icon="e.icon" :title="e.title" :name="e.name"
          :sub="[e.driver, e.key && `key ${e.key}`, e.image].filter(Boolean).join(' · ')" :focus="target === undefined ? 'idle' : focusOf(e.name, [target])"
        />
        <p v-if="!view.executors.length" class="m-0 text-[13.5px] text-muted">
          {{ tr({ en: 'No executors.', zh: '没有 executor。' }) }}
        </p>
        <div v-if="target === 'deny'" class="flex items-center gap-[8px] rounded-[14px] px-[14px] py-[12px] bg-[color-mix(in_srgb,var(--bad)_14%,var(--surface))] text-bad">
          <Icon name="mingcute:forbid-circle-line" /> {{ tr({ en: 'Denied: exits 126', zh: '被拒绝：退出码 126' }) }}
        </div>
      </div>
      </div>
    </div>

    <div class="grid gap-[16px] grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)_minmax(0,0.6fr)] max-[900px]:grid-cols-[minmax(0,1fr)]">
      <section class="rounded-[16px] px-[16px] py-[14px] bg-surface">
        <h4 class="m-[0_0_8px] flex items-center gap-[6px] font-display text-[16px] font-normal"><Icon name="mingcute:folder-open-line" /> {{ tr({ en: 'Files', zh: '文件' }) }}</h4>
        <p v-if="!view.files.length" class="m-0 text-[13.5px] text-muted">
          {{ tr({ en: 'No file mappings.', zh: '没有文件映射。' }) }}
        </p>
        <ul class="m-0 flex list-none flex-col gap-[6px] p-0">
          <li v-for="f in view.files" :key="f.name" class="flex min-w-0 items-center gap-[8px]">
            <span class="inline-flex flex-none items-center gap-[4px] rounded-[7px] px-[8px] py-[2px] bg-executor/13 font-mono text-[12px] text-executor"><Icon :name="MODE_ICON[f.mode as keyof typeof MODE_ICON] ?? 'mingcute:file-code-line'" />{{ f.mode }}</span>
            <code class="inline-block max-w-full overflow-hidden text-ellipsis whitespace-nowrap align-bottom text-[12.5px]">{{ f.from }} → {{ f.to }}</code>
            <span v-if="!f.run" class="m-0 text-[12px] text-muted">{{ tr({ en: 'by rule', zh: '按规则' }) }}</span>
          </li>
        </ul>
      </section>
      <section class="rounded-[16px] px-[16px] py-[14px] bg-surface">
        <h4 class="m-[0_0_8px] flex items-center gap-[6px] font-display text-[16px] font-normal"><Icon name="mingcute:fire-line" /> {{ tr({ en: 'Chaos', zh: '故障注入' }) }} <code v-if="view.chaos.seed !== undefined" class="ml-auto text-[12.5px] text-muted">seed {{ view.chaos.seed }}</code></h4>
        <p v-if="!view.chaos.faults.length" class="m-0 text-[13.5px] text-muted">
          {{ tr({ en: 'No faults.', zh: '没有故障。' }) }}
        </p>
        <ul class="m-0 flex list-none flex-col gap-[6px] p-0">
          <li v-for="f in view.chaos.faults" :key="f.name" :title="f.name" class="flex min-w-0 items-center gap-[8px]">
            <span class="inline-flex flex-none items-center gap-[4px] rounded-[7px] px-[8px] py-[2px] bg-fault/15 font-mono text-[12px] text-fault">{{ f.on }}</span><code class="inline-block max-w-full overflow-hidden text-ellipsis whitespace-nowrap align-bottom text-[12.5px]">{{ f.text }}</code>
          </li>
        </ul>
      </section>
      <section class="rounded-[16px] px-[16px] py-[14px] bg-surface">
        <h4 class="m-[0_0_8px] flex items-center gap-[6px] font-display text-[16px] font-normal"><Icon name="mingcute:git-branch-line" /> {{ tr({ en: 'Trace', zh: 'Trace' }) }}</h4>
        <ul class="m-0 flex list-none flex-col gap-[6px] p-0">
          <li v-for="sink in view.trace" :key="sink" class="flex min-w-0 items-center gap-[8px]">
            <code class="text-[12.5px]">{{ sink === 'sqlite' ? 'trace.db' : 'trace.jsonl' }}</code>
          </li>
        </ul>
      </section>
    </div>
  </div>
</template>
