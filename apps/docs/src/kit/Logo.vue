<script setup lang="ts">
import type { IconName } from '../icons'

import Icon from './Icon.vue'

import firecracker from '../assets/logos/firecracker.svg'
import gvisor from '../assets/logos/gvisor.png'

/**
 * The mark of a technology needy runs on. Firecracker and gVisor are not in the Iconify sets, so they come from
 * their repositories (src/assets/logos/SOURCES.md); the others are Iconify logos.
 */
export type LogoName = 'firecracker' | 'gvisor' | 'docker' | 'linux' | 'apple' | 'windows' | 'opencode' | 'ollama'

const ICONIFY: Record<Exclude<LogoName, 'firecracker' | 'gvisor'>, IconName> = {
  docker: 'logos:docker-icon',
  linux: 'logos:linux-tux',
  apple: 'logos:apple',
  windows: 'logos:microsoft-windows-icon',
  opencode: 'logos:opencode-icon',
  ollama: 'logos:ollama',
}

const FILES = { firecracker, gvisor } as const

defineProps<{ name: LogoName, size?: number }>()
</script>

<template>
  <img v-if="name === 'firecracker' || name === 'gvisor'" class="logo" :class="name" :src="FILES[name]" :width="size ?? 24" :height="size ?? 24" :alt="name">
  <Icon v-else class="logo" :class="name" :name="ICONIFY[name]" :size="size ?? 24" />
</template>

<style scoped>
.logo {
  flex: none;
  object-fit: contain;
}

/* The Apple and opencode marks are black; on a dark ground they turn light. */
:root[data-theme='dark'] .logo.apple,
:root[data-theme='dark'] .logo.opencode {
  filter: invert(1);
}
</style>
