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
</script>

<template>
  <div
    class="place" :class="state"
    :style="{ left: `${x}px`, top: `${y}px`, width: `${w}px`, height: `${h}px`, '--role': color(role), '--role-soft': color(role, 'softer'), '--role-fill': color(role, 'fill') }"
  >
    <header>
      <span class="name"><i class="dot" />{{ label }}</span>
      <span v-if="sub" class="sub mono">{{ sub }}</span>
    </header>
    <div class="body">
      <slot />
    </div>
  </div>
</template>

<style scoped>
.place {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 16px;
  border-radius: 20px;
  background: var(--surface);
  box-shadow:
    var(--shadow-card),
    0 0 0 0 transparent;
  transition:
    box-shadow 0.35s,
    background 0.35s,
    opacity 0.45s,
    transform 0.45s cubic-bezier(0.2, 0, 0, 1);
}

.place.on {
  background: color-mix(in srgb, var(--role-soft) 60%, var(--surface));
  box-shadow:
    var(--shadow-card),
    0 0 0 2px var(--role);
}

.place.bad {
  background: color-mix(in srgb, var(--bad-softer) 70%, var(--surface));
  box-shadow:
    var(--shadow-card),
    0 0 0 2px var(--bad);
}

.place.dim {
  opacity: 0.45;
}

.place.gone {
  opacity: 0.12;
  transform: scale(0.97);
}

header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
  white-space: nowrap;
}

.name {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-family: var(--font-display);
  font-size: 19px;
}

.dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: var(--role-fill);
}

.sub {
  overflow: hidden;
  color: var(--muted);
  font-size: 12.5px;
  text-overflow: ellipsis;
}

.body {
  position: relative;
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  gap: 6px;
}
</style>
