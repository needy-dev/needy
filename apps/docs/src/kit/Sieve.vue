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
  <div
    class="flex flex-col gap-[10px] rounded-[18px] bg-control px-[12px] py-[14px]" :class="flow ? 'relative' : 'absolute'"
    :style="{ left: (flow ? undefined : `${x}px`), top: (flow ? undefined : `${y}px`), width: (flow ? undefined : `${w}px`), height: flow ? undefined : `${h}px` }"
  >
    <span class="display flex items-center gap-[6px] pl-[4px] text-[16px] text-ink-2"><Icon name="mingcute:route-line" />{{ title }}</span>
    <ol class="slots m-0 flex flex-1 list-none flex-col justify-center gap-[8px] p-0">
      <!-- The lit slot takes its tint over focus.css's: important, since `.f.focus` is the more specific rule. -->
      <li
        v-for="(rule, i) in rules" :key="i" class="f flex flex-none flex-col justify-center gap-[2px] rounded-[10px] px-[11px] py-[8px]"
        :class="[
          lit === undefined ? 'idle' : lit === i ? (rule.deny !== undefined ? 'bad' : 'focus') : 'dim',
          lit === i ? '!bg-[color-mix(in_srgb,var(--role)_14%,var(--surface))]' : 'bg-surface',
          flow ? 'h-auto min-h-[52px]' : 'h-[52px]',
        ]"
        :style="{ '--role': rule.deny !== undefined ? 'var(--bad)' : 'var(--executor)' }"
      >
        <code class="text-[13px] text-ink" :class="flow ? '[overflow-wrap:anywhere] whitespace-normal' : 'whitespace-nowrap'">{{ rule.match }}</code>
        <span class="font-mono text-[12px]" :class="rule.deny !== undefined ? 'text-bad' : 'text-executor'">{{ rule.deny !== undefined ? 'deny' : `→ ${rule.to}` }}</span>
      </li>
    </ol>
  </div>
</template>
