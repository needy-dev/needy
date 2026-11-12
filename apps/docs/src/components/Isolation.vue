<script setup lang="ts">
import type { Text } from '../i18n'

import { tr } from '../i18n'
import Logo from '../kit/Logo.vue'

import { DRIVERS } from '../data/drivers'

/**
 * The isolation needy supports for the actor, one line each: what it is, and the host it needs. The facts are the
 * README's "Actor drivers" table (src/data/drivers.ts); the matrix below the list has the details.
 */

const KIND: Record<string, Text> = {
  'linux-firecracker': { en: 'MicroVM', zh: 'MicroVM' },
  'linux-container': { en: 'Container', zh: '容器' },
  'macos-vz': { en: 'Virtual machine', zh: '虚拟机' },
  'macos-apple-container': { en: 'Container in a VM', zh: '虚拟机里的容器' },
}

const LINE: Record<string, Text> = {
  'linux-firecracker': { en: 'Its own kernel; presents the whole recipe.', zh: '有自己的内核，recipe 里写的都能装出来。' },
  'linux-container': { en: 'runc or gVisor; no KVM needed.', zh: 'runc 或 gVisor，没有 KVM 也能跑。' },
  'macos-vz': { en: 'needy\'s Linux on your Mac, through vfkit.', zh: '借 vfkit 在 Mac 上跑 needy 自己的 Linux。' },
  'macos-apple-container': { en: 'Apple\'s own container runtime.', zh: '直接用 Apple 自家的容器。' },
}
</script>

<template>
  <ul class="options">
    <li v-for="d in DRIVERS" :key="d.id" class="option">
      <span class="logos"><Logo v-for="l in d.logos" :key="l" :name="l" :size="28" /></span>
      <span class="kind">{{ tr(KIND[d.id]) }}</span>
      <strong class="display">{{ d.title }}</strong>
      <code class="id">{{ d.id }}</code>
      <span class="line">{{ tr(LINE[d.id]) }}</span>
      <span class="host">{{ d.host }}</span>
    </li>
  </ul>
</template>

<style scoped>
.options {
  display: grid;
  gap: 32px;
  margin: 0;
  padding: 0;
  list-style: none;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
}

.option {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.logos {
  display: flex;
  gap: 6px;
  margin-bottom: 6px;
}

.kind {
  color: var(--muted);
  font-size: 13px;
}

strong {
  font-size: 20px;
  font-weight: 400;
}

.id {
  color: var(--actor);
  font-size: 13px;
}

.line {
  color: var(--ink-2);
  font-size: 15px;
  line-height: 1.45;
}

.host {
  color: var(--muted);
  font-size: 13.5px;
}
</style>
