<script setup lang="ts">
import type { Role } from './roles'

import { computed } from 'vue'

import { color } from './roles'

/** One trace event, by its kind in crates/core/src/event.rs, and what it says. */
export interface TraceEvent {
  kind: string
  text?: string
  role: Role
}

/** The trace of a run as a strip of events: the new ones slide in at the right, the old ones move left and fade. */
const props = withDefaults(defineProps<{ events: readonly TraceEvent[], fresh?: number, keep?: number, delay?: number }>(), { fresh: 0, keep: 4, delay: 0 })

const shown = computed(() => props.events.map((event, i) => ({ event, i })).slice(-props.keep))
</script>

<template>
  <!-- The oldest events leave at the left, so they fade there. -->
  <TransitionGroup
    tag="div" leave-active-class="absolute" leave-to-class="opacity-0"
    class="trace mono relative flex items-center justify-end gap-[8px] overflow-hidden text-[12.5px] [mask-image:linear-gradient(to_right,transparent,#000_64px)]"
  >
    <span
      v-for="{ event, i } in shown" :key="i"
      class="inline-flex flex-none items-center gap-[7px] whitespace-nowrap rounded-[999px] bg-[var(--soft)] px-[11px] py-[5px] [transition:transform_0.4s_cubic-bezier(0.2,0,0,1),opacity_0.4s]"
      :class="{ '[animation:enter-right-16_0.4s_cubic-bezier(0.2,0,0,1)_both]': i >= fresh }"
      :style="{ '--fill': color(event.role, 'fill'), '--soft': color(event.role, 'softer'), 'animationDelay': `${delay + (i - fresh) * 160}ms` }"
    >
      <i class="h-[7px] w-[7px] rounded-[50%] bg-[var(--fill)]" /><b class="font-medium">{{ event.kind }}</b><span v-if="event.text" class="text-muted">{{ event.text }}</span>
    </span>
  </TransitionGroup>
</template>
