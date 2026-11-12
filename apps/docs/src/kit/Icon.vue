<script setup lang="ts">
import type { IconName } from '../icons'

import icons from 'virtual:icons'

/** An icon from src/icons.ts, as inline SVG. Monochrome sets take the text color; logos keep their own colors. */
const props = defineProps<{ name: IconName, size?: number }>()
const icon = icons[props.name]!
</script>

<template>
  <svg
    class="icon" :width="size ?? '1em'" :height="size ?? '1em'" :viewBox="`0 0 ${icon.width} ${icon.height}`"
    aria-hidden="true" v-html="icon.body"
  />
</template>

<style scoped>
.icon {
  flex: none;
  vertical-align: -0.125em;
}
</style>
