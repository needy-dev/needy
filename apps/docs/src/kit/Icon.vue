<script setup lang="ts">
import type { IconName } from '../icons'

/**
 * An icon from src/icons.ts, as UnoCSS's icon preset draws it (uno.config.ts). Monochrome sets take the text color;
 * logos keep their own colors.
 */
defineProps<{ name: IconName, size?: number }>()
</script>

<template>
  <span :class="`i-${name}`" :style="size ? { fontSize: `${size}px` } : undefined" aria-hidden="true" />
</template>
