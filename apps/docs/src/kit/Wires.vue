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

/** The moving dashes of a flowing wire. */
const FLOW = 'fill-none stroke-role [stroke-width:4] [stroke-dasharray:0.1_21.9] [stroke-linecap:round] [animation:dash-flow_0.9s_linear_infinite]'

/** A wire's dotted line, drawn in its role's color once it is live, dashed red when it is denied. */
const BASE: Record<NonNullable<Wire['state']>, string> = {
  idle: 'stroke-line [stroke-dasharray:1.5_6]',
  live: 'stroke-role [stroke-dasharray:none] opacity-75',
  flow: 'stroke-role [stroke-dasharray:none] opacity-75',
  bad: 'stroke-bad [stroke-dasharray:6_6]',
  dim: 'stroke-line [stroke-dasharray:1.5_6]',
}
</script>

<template>
  <svg class="pointer-events-none absolute inset-0 overflow-visible" :width="width" :height="height" :viewBox="`0 0 ${width} ${height}`" aria-hidden="true">
    <!--
      NOTICE: a flowing wire's group takes the dashes' style too, as the old `.flow` rule matched the group and the
      dashes alike. Its label inherits the dash pattern and its animation through it.
    -->
    <g
      v-for="wire in wires" :key="wire.id" class="[transition:opacity_0.35s]"
      :class="wire.state === 'dim' ? 'opacity-35' : wire.state === 'flow' ? FLOW : ''"
      :style="{ '--role': color(wire.role ?? 'host') }"
    >
      <path class="fill-none [stroke-linecap:round] [stroke-width:1.6] [transition:stroke_0.35s]" :class="BASE[wire.state ?? 'idle']" :d="wire.d" />
      <path v-if="wire.state === 'flow'" :class="FLOW" :d="wire.d" />
      <path v-if="wire.state === 'flow' && wire.both" :class="FLOW" class="opacity-60 ![animation-direction:reverse] [stroke-dashoffset:11]" :d="wire.d" />
      <text
        v-if="wire.label" class="mono fill-muted stroke-[var(--ground)] text-[12.5px] [paint-order:stroke] [stroke-linejoin:round] [stroke-width:6px]"
        :x="wire.label.x" :y="wire.label.y" text-anchor="middle"
      >{{ wire.label.text }}</text>
    </g>
  </svg>
</template>
