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
  <div class="packets" aria-hidden="true">
    <span
      v-for="p in packets" :key="p.id" class="packet mono" :class="{ back: p.back, keep: p.keep, dot: !p.text }"
      :style="{
        'offsetPath': `path('${p.d}')`,
        'animationDelay': `${p.delay ?? 0}ms`,
        'animationDuration': `${p.ms ?? 900}ms`,
        '--fill': color(p.role, 'fill'),
      }"
    >{{ p.text }}</span>
  </div>
</template>

<style scoped>
.packets {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.packet {
  position: absolute;
  top: 0;
  left: 0;
  z-index: 5;
  padding: 4px 10px;
  border-radius: 999px;
  background: var(--fill);
  box-shadow: 0 4px 14px rgb(0 0 0 / 18%);
  color: #fff;
  font-size: 12.5px;
  white-space: nowrap;
  offset-anchor: center;
  offset-rotate: 0deg;
  animation-name: travel;
  animation-timing-function: cubic-bezier(0.45, 0, 0.25, 1);
  animation-fill-mode: both;
}

.packet.dot {
  width: 12px;
  height: 12px;
  padding: 0;
}

.packet.back {
  animation-name: travel-back;
}

.packet.keep {
  animation-name: travel-keep;
}

@keyframes travel {
  0% {
    offset-distance: 0%;
    opacity: 0;
  }

  10%,
  86% {
    opacity: 1;
  }

  100% {
    offset-distance: 100%;
    opacity: 0;
  }
}

@keyframes travel-back {
  0% {
    offset-distance: 100%;
    opacity: 0;
  }

  10%,
  86% {
    opacity: 1;
  }

  100% {
    offset-distance: 0%;
    opacity: 0;
  }
}

@keyframes travel-keep {
  0% {
    offset-distance: 0%;
    opacity: 0;
  }

  10%,
  100% {
    opacity: 1;
  }

  100% {
    offset-distance: 100%;
  }
}
</style>
