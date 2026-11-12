<script setup lang="ts">
import type { Rule } from '../route/rules'

import Icon from './Icon.vue'

/**
 * The profile's rules as a membrane between the actor and the executors: one slot per rule, in order. A command
 * passes through the slot of the rule that decides it; that slot lights, red for a denial.
 */
withDefaults(defineProps<{
  x?: number
  y?: number
  w?: number
  h?: number
  /** In the page's flow instead of at stage coordinates. */
  flow?: boolean
  rules: readonly Rule[]
  /** The rule that decides now, or -1 for none (the command stays in the actor). */
  lit?: number
  title?: string
}>(), { lit: undefined, title: 'Rules' })
</script>

<template>
  <div class="sieve" :class="{ flow }" :style="{ left: (flow ? undefined : `${x}px`), top: (flow ? undefined : `${y}px`), width: (flow ? undefined : `${w}px`), height: flow ? undefined : `${h}px` }">
    <span class="title display"><Icon name="mingcute:route-line" />{{ title }}</span>
    <ol class="slots">
      <li
        v-for="(rule, i) in rules" :key="i" class="slot f"
        :class="[lit === undefined ? 'idle' : lit === i ? (rule.deny !== undefined ? 'bad' : 'focus') : 'dim', { deny: rule.deny !== undefined }]"
        :style="{ '--role': rule.deny !== undefined ? 'var(--bad)' : 'var(--executor)' }"
      >
        <code class="match">{{ rule.match }}</code>
        <span class="action">{{ rule.deny !== undefined ? 'deny' : `→ ${rule.to}` }}</span>
      </li>
    </ol>
  </div>
</template>

<style scoped>
.sieve {
  position: absolute;
}

.sieve.flow {
  position: relative;
}

.sieve.flow .slot {
  height: auto;
  min-height: 52px;
}

.sieve.flow .match {
  overflow-wrap: anywhere;
  white-space: normal;
}

.sieve {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 12px;
  border-radius: 18px;
  background: var(--control);
}

.title {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-left: 4px;
  color: var(--ink-2);
  font-size: 16px;
}

.slots {
  display: flex;
  flex: 1;
  flex-direction: column;
  justify-content: center;
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.slot {
  display: flex;
  height: 52px;
  flex: none;
  flex-direction: column;
  justify-content: center;
  gap: 2px;
  padding: 8px 11px;
  border-radius: 10px;
  background: var(--surface);
}

.slot.focus,
.slot.bad {
  background: color-mix(in srgb, var(--role) 14%, var(--surface));
}

.match {
  color: var(--ink);
  font-size: 13px;
  white-space: nowrap;
}

.action {
  color: var(--executor);
  font-family: var(--font-mono);
  font-size: 12px;
}

.deny .action {
  color: var(--bad);
}
</style>
