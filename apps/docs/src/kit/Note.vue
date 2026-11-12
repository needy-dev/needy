<script setup lang="ts">
import type { Role } from './roles'

import { color } from './roles'

/** A speech bubble that says what is happening, anchored with its tail at (x, y). It pops in when its key changes. */
withDefaults(defineProps<{ x: number, y: number, role?: Role, delay?: number, side?: 'left' | 'right' }>(), { role: 'host', delay: 0, side: 'left' })
</script>

<template>
  <span
    class="note" :class="side"
    :style="{ 'left': `${x}px`, 'top': `${y}px`, '--fill': color(role, 'fill'), 'animationDelay': `${delay}ms` }"
  ><slot /></span>
</template>

<style scoped>
.note {
  position: absolute;
  z-index: 6;
  padding: 6px 12px;
  border-radius: 12px 12px 12px 3px;
  background: var(--fill);
  box-shadow: 0 6px 18px rgb(0 0 0 / 16%);
  color: #fff;
  font-size: 14px;
  white-space: nowrap;
  transform: translateY(-100%);
  animation: pop 0.45s cubic-bezier(0.2, 0, 0, 1) both;
}

.note.right {
  border-radius: 12px 12px 3px 12px;
  transform: translate(-100%, -100%);
}

@keyframes pop {
  from {
    opacity: 0;
    translate: 0 8px;
  }
}
</style>
