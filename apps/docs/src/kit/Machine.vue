<script setup lang="ts">
import type { IconName } from '../icons'
import type { Focus } from './focus'
import type { LogoName } from './Logo.vue'
import type { Role } from './roles'

import Icon from './Icon.vue'
import Logo from './Logo.vue'

import { color } from './roles'

/**
 * A machine: an executor, a runner or the control plane. Opaque and squarer than the actor's capsule, with an icon
 * tile in the color of its role (a Mingcute icon), or a technology's mark (`logo`) where the technology is the point.
 * `name` is the profile's name for it, in mono; `title` says what it is.
 */
withDefaults(defineProps<{
  x?: number
  y?: number
  w?: number
  h?: number
  /** In the page's flow instead of at stage coordinates. */
  flow?: boolean
  role?: Role
  title: string
  name?: string
  sub?: string
  icon?: IconName
  logo?: LogoName
  focus?: Focus
  badge?: string
}>(), { role: 'executor', focus: 'idle' })
</script>

<template>
  <div
    class="machine f" :class="[focus, { flow }]"
    :style="{ 'left': flow ? undefined : `${x}px`, 'top': flow ? undefined : `${y}px`, 'width': flow ? undefined : `${w}px`, 'height': h ? `${h}px` : undefined, '--role': color(role) }"
  >
    <header>
      <span v-if="icon" class="tile"><Icon :name="icon" :size="20" /></span>
      <Logo v-else-if="logo" :name="logo" :size="26" />
      <span class="what">
        <span class="title display">{{ title }}</span>
        <span v-if="sub" class="sub">{{ sub }}</span>
      </span>
      <code v-if="name" class="name">{{ name }}</code>
    </header>
    <div v-if="$slots.default" class="body">
      <slot />
    </div>
    <span v-if="badge" class="badge"><Icon name="mingcute:box-3-line" />{{ badge }}</span>
  </div>
</template>

<style scoped>
.machine {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 14px;
  border-radius: 14px;
  background: var(--surface);
}

.machine.flow {
  position: relative;
}

header {
  display: flex;
  align-items: center;
  gap: 11px;
}

.tile {
  display: grid;
  width: 38px;
  height: 38px;
  flex: none;
  place-items: center;
  border-radius: 10px;
  background: color-mix(in srgb, var(--role) 15%, transparent);
  color: var(--role);
}

.what {
  display: flex;
  min-width: 0;
  flex: 1;
  flex-direction: column;
}

.title {
  overflow: hidden;
  font-size: 17px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.sub {
  overflow: hidden;
  color: var(--muted);
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.name {
  flex: none;
  padding: 2px 8px;
  border-radius: 7px;
  background: color-mix(in srgb, var(--role) 13%, transparent);
  color: var(--role);
  font-size: 13px;
}

.body {
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  gap: 6px;
}

.badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-top: auto;
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
}
</style>
