<script setup lang="ts">
import type { Role } from './roles'

import { color } from './roles'

/** A wire between two places. `live` draws it in ink, `flow` sends dots along it for as long as it shows. */
export interface Wire {
  id: string
  d: string
  state?: 'idle' | 'live' | 'flow' | 'bad' | 'dim'
  role?: Role
  /** Flow both ways, for a mount that serves reads and writes. */
  both?: boolean
  label?: { x: number, y: number, text: string }
}

/** The wires of a stage, in one SVG layer under the boxes, at design coordinates. */
defineProps<{ wires: readonly Wire[], width: number, height: number }>()
</script>

<template>
  <svg class="wires" :width="width" :height="height" :viewBox="`0 0 ${width} ${height}`" aria-hidden="true">
    <g v-for="wire in wires" :key="wire.id" class="wire" :class="wire.state ?? 'idle'" :style="{ '--role': color(wire.role ?? 'host') }">
      <path class="base" :d="wire.d" />
      <path v-if="wire.state === 'flow'" class="flow" :d="wire.d" />
      <path v-if="wire.state === 'flow' && wire.both" class="flow back" :d="wire.d" />
      <text v-if="wire.label" class="label mono" :x="wire.label.x" :y="wire.label.y" text-anchor="middle">{{ wire.label.text }}</text>
    </g>
  </svg>
</template>

<style scoped>
.wires {
  position: absolute;
  inset: 0;
  overflow: visible;
  pointer-events: none;
}

.wire {
  transition: opacity 0.35s;
}

.base {
  fill: none;
  stroke: var(--line);
  stroke-width: 1.6;
  stroke-dasharray: 1.5 6;
  stroke-linecap: round;
  transition: stroke 0.35s;
}

.live .base,
.flow .base {
  stroke: var(--role);
  stroke-dasharray: none;
  opacity: 0.75;
}

.bad .base {
  stroke: var(--bad);
  stroke-dasharray: 6 6;
}

.dim {
  opacity: 0.35;
}

.flow {
  fill: none;
  stroke: var(--role);
  stroke-width: 4;
  stroke-dasharray: 0.1 21.9;
  stroke-linecap: round;
  animation: flow 0.9s linear infinite;
}

.flow.back {
  stroke-dashoffset: 11;
  animation-direction: reverse;
  opacity: 0.6;
}

@keyframes flow {
  to {
    stroke-dashoffset: -22;
  }
}

.label {
  fill: var(--muted);
  stroke: var(--ground);
  stroke-width: 6px;
  stroke-linejoin: round;
  paint-order: stroke;
  font-size: 12.5px;
}
</style>
