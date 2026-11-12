<script setup lang="ts">
import type { Role } from './roles'

import { color } from './roles'

/**
 * A place where work happens: the actor, an executor, the gateway, the control host. It sits at design coordinates on
 * the stage. `on` lights it in the color of its role, `bad` in red, `dim` steps it back, and `gone` fades it out.
 */
withDefaults(defineProps<{
  x: number
  y: number
  w: number
  h: number
  role: Role
  label: string
  sub?: string
  state?: 'idle' | 'on' | 'bad' | 'dim' | 'gone'
}>(), { state: 'idle' })

/** The ground and the ring of each state. */
const STATE = {
  idle: 'bg-surface [box-shadow:var(--shadow-card),0_0_0_0_transparent]',
  on: 'bg-[color-mix(in_srgb,var(--role-soft)_60%,var(--surface))] [box-shadow:var(--shadow-card),0_0_0_2px_var(--role)]',
  bad: 'bg-[color-mix(in_srgb,var(--bad-softer)_70%,var(--surface))] [box-shadow:var(--shadow-card),0_0_0_2px_var(--bad)]',
  dim: 'bg-surface [box-shadow:var(--shadow-card),0_0_0_0_transparent] opacity-45',
  gone: 'bg-surface [box-shadow:var(--shadow-card),0_0_0_0_transparent] opacity-12 [transform:scale(0.97)]',
} as const
</script>

<template>
  <div
    class="absolute flex flex-col gap-[10px] rounded-[20px] px-[16px] py-[14px] [transition:box-shadow_0.35s,background_0.35s,opacity_0.45s,transform_0.45s_cubic-bezier(0.2,0,0,1)]"
    :class="STATE[state]"
    :style="{ left: `${x}px`, top: `${y}px`, width: `${w}px`, height: `${h}px`, '--role': color(role), '--role-soft': color(role, 'softer'), '--role-fill': color(role, 'fill') }"
  >
    <header class="flex items-baseline justify-between gap-[10px] whitespace-nowrap">
      <span class="inline-flex items-center gap-[8px] font-display text-[19px]"><i class="h-[10px] w-[10px] rounded-[50%] bg-role-fill" />{{ label }}</span>
      <span v-if="sub" class="mono overflow-hidden text-ellipsis text-[12.5px] text-muted">{{ sub }}</span>
    </header>
    <div class="relative flex min-h-0 flex-1 flex-col gap-[6px]">
      <slot />
    </div>
  </div>
</template>
