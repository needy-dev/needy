<script setup lang="ts">
import type { Role } from './roles'

import { color } from './roles'

/** A speech bubble that says what is happening, anchored with its tail at (x, y). It pops in when its key changes. */
withDefaults(defineProps<{ x: number, y: number, role?: Role, delay?: number, side?: 'left' | 'right' }>(), { role: 'host', delay: 0, side: 'left' })
</script>

<template>
  <span
    class="absolute z-[6] whitespace-nowrap px-[12px] py-[6px] [background:var(--fill)] text-[14px] text-[#fff] [box-shadow:0_6px_18px_rgb(0_0_0/16%)] animate-[pop-up_0.45s_cubic-bezier(0.2,0,0,1)_both]"
    :class="side === 'right' ? 'rounded-[12px_12px_3px_12px] [transform:translate(-100%,-100%)]' : 'rounded-[12px_12px_12px_3px] [transform:translateY(-100%)]'"
    :style="{ 'left': `${x}px`, 'top': `${y}px`, '--fill': color(role, 'fill'), 'animationDelay': `${delay}ms` }"
  ><slot /></span>
</template>
