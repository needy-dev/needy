<script setup lang="ts">
import type { Focus } from './focus'
import type { LogoName } from './Logo.vue'

import Icon from './Icon.vue'
import Logo from './Logo.vue'

/**
 * The actor: a rounded capsule of tinted glass. The grid of the stage shows through it, and it holds nothing that
 * outlives the run. Its driver's mark sits in the header. No outline: the tint and the round shape set it apart from
 * the machines.
 */
withDefaults(defineProps<{
  x?: number
  y?: number
  w?: number
  h?: number
  /** In the page's flow instead of at stage coordinates. */
  flow?: boolean
  title?: string
  driver?: string
  logo?: LogoName
  focus?: Focus
  badge?: string
}>(), { title: 'Actor', focus: 'idle', badge: 'Stateless' })
</script>

<template>
  <div
    class="pod f" :class="[focus, { flow }]"
    :style="{ 'left': flow ? undefined : `${x}px`, 'top': flow ? undefined : `${y}px`, 'width': flow ? undefined : `${w}px`, 'height': flow ? undefined : `${h}px`, '--role': 'var(--actor)' }"
  >
    <header>
      <Logo v-if="logo" :name="logo" :size="22" />
      <span class="title display">{{ title }}</span>
      <span v-if="driver" class="driver mono">{{ driver }}</span>
      <span v-if="badge" class="badge"><Icon name="mingcute:refresh-2-line" />{{ badge }}</span>
    </header>
    <div class="body">
      <slot />
    </div>
  </div>
</template>

<style scoped>
.pod {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px 18px 18px;
  /* Rounder than a machine, to read as the capsule, but in the same family as the other surfaces. */
  border-radius: 22px;
  background: color-mix(in srgb, var(--actor) 11%, transparent);
  backdrop-filter: blur(10px);
}

.pod.flow {
  position: relative;
}

.pod.flow header {
  flex-wrap: wrap;
}

.pod.focus {
  background: color-mix(in srgb, var(--actor) 18%, transparent);
}

.pod.bad {
  background: color-mix(in srgb, var(--bad) 12%, transparent);
}

header {
  display: flex;
  align-items: center;
  gap: 9px;
  white-space: nowrap;
}

.title {
  color: var(--actor);
  font-size: 21px;
}

.driver {
  color: var(--muted);
  font-size: 12.5px;
}

.badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-left: auto;
  padding: 3px 10px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--actor) 14%, transparent);
  color: var(--actor);
  font-size: 12.5px;
}

.body {
  position: relative;
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  gap: 8px;
}
</style>
