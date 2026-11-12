<script setup lang="ts">
import type { Role } from './roles'

import { color } from './roles'

/**
 * Something that travels along a wire once: an exec, a request, a file. It rides the path with CSS `offset-path`,
 * so it needs no script per frame, and it starts again whenever its key changes.
 */
export interface Packet {
  id: string
  d: string
  role: Role
  text?: string
  /** When it leaves, from the start of the step. */
  delay?: number
  ms?: number
  /** Ride the path from its end to its start. */
  back?: boolean
  /** Stay at the end instead of fading there. */
  keep?: boolean
}

defineProps<{ packets: readonly Packet[] }>()
</script>

<template>
  <div class="pointer-events-none absolute inset-0" aria-hidden="true">
    <span
      v-for="p in packets" :key="p.id"
      class="mono absolute left-0 top-0 z-5 whitespace-nowrap rounded-[999px] bg-[var(--fill)] text-[12.5px] text-[#fff] [animation-fill-mode:both] [animation-timing-function:cubic-bezier(0.45,0,0.25,1)] [box-shadow:0_4px_14px_rgb(0_0_0/18%)] [offset-anchor:center] [offset-rotate:0deg]"
      :class="[p.text ? 'px-[10px] py-[4px]' : 'h-[12px] w-[12px] p-0', p.keep ? '[animation-name:packet-travel-keep]' : p.back ? '[animation-name:packet-travel-back]' : '[animation-name:packet-travel]']"
      :style="{
        'offsetPath': `path('${p.d}')`,
        'animationDelay': `${p.delay ?? 0}ms`,
        'animationDuration': `${p.ms ?? 900}ms`,
        '--fill': color(p.role, 'fill'),
      }"
    >{{ p.text }}</span>
  </div>
</template>
