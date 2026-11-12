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
  <TransitionGroup tag="div" name="event" class="trace mono">
    <span
      v-for="{ event, i } in shown" :key="i" class="event" :class="{ fresh: i >= fresh }"
      :style="{ '--fill': color(event.role, 'fill'), '--soft': color(event.role, 'softer'), 'animationDelay': `${delay + (i - fresh) * 160}ms` }"
    >
      <i class="dot" /><b>{{ event.kind }}</b><span v-if="event.text" class="text">{{ event.text }}</span>
    </span>
  </TransitionGroup>
</template>

<style scoped>
.trace {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  overflow: hidden;
  font-size: 12.5px;
  /* The oldest events leave at the left, so they fade there. */
  mask-image: linear-gradient(to right, transparent, #000 64px);
}

.event {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 7px;
  padding: 5px 11px;
  border-radius: 999px;
  background: var(--soft);
  white-space: nowrap;
  transition:
    transform 0.4s cubic-bezier(0.2, 0, 0, 1),
    opacity 0.4s;
}

.event.fresh {
  animation: arrive 0.4s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes arrive {
  from {
    opacity: 0;
    transform: translateX(16px);
  }
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--fill);
}

b {
  font-weight: 500;
}

.text {
  color: var(--muted);
}

.event-leave-active {
  position: absolute;
}

.event-leave-to {
  opacity: 0;
}
</style>
