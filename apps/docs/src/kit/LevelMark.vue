<script setup lang="ts">
import type { Level } from '../data/drivers'

import { LEVELS } from '../data/drivers'

/**
 * A support level as a mark, so it reads without color: a filled disc with a check is verified end to end, a hollow
 * ring is expected but not yet recorded, a slashed ring is not possible with the driver. One SVG on a 20-unit grid,
 * so the check sits in the middle of its disc wherever the mark is placed.
 */
withDefaults(defineProps<{ level: Level, size?: number }>(), { size: 18 })
</script>

<template>
  <svg class="flex-none align-middle" :width="size" :height="size" viewBox="0 0 20 20" role="img" :aria-label="LEVELS[level].label">
    <template v-if="level === 'yes'">
      <circle cx="10" cy="10" r="10" class="fill-executor-fill" />
      <path d="M5.8 10.3 8.6 13.1 14.3 7.3" class="fill-none stroke-[#fff] [stroke-linecap:round] [stroke-linejoin:round] [stroke-width:2.2]" />
    </template>
    <circle v-else cx="10" cy="10" r="8.6" class="fill-none [stroke-width:1.8]" :class="level === 'no' ? 'stroke-faint/75' : 'stroke-faint'" />
    <path v-if="level === 'no'" d="M4.2 15.8 15.8 4.2" class="fill-none stroke-faint/75 [stroke-width:1.8]" />
  </svg>
</template>
