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
  <!--
    Rounder than a machine, to read as the capsule, but in the same family as the other surfaces. The focus and bad tints
    are important: they replace focus.css's `.f.focus` and `.f.bad`, which are the more specific rules.
  -->
  <div
    class="pod f flex flex-col gap-[12px] rounded-[22px] px-[18px] pb-[18px] pt-[16px] [backdrop-filter:blur(10px)]"
    :class="[focus, flow ? 'relative' : 'absolute', focus === 'focus' ? '!bg-actor/18' : focus === 'bad' ? '!bg-bad/12' : 'bg-actor/11']"
    :style="{ 'left': flow ? undefined : `${x}px`, 'top': flow ? undefined : `${y}px`, 'width': flow ? undefined : `${w}px`, 'height': flow ? undefined : `${h}px`, '--role': 'var(--actor)' }"
  >
    <header class="flex items-center gap-[9px] whitespace-nowrap" :class="{ 'flex-wrap': flow }">
      <Logo v-if="logo" :name="logo" :size="22" />
      <span class="display text-[21px] text-actor">{{ title }}</span>
      <span v-if="driver" class="mono text-[12.5px] text-muted">{{ driver }}</span>
      <span v-if="badge" class="ml-auto inline-flex items-center gap-[5px] rounded-[999px] bg-actor/14 px-[10px] py-[3px] text-[12.5px] text-actor"><Icon name="mingcute:refresh-2-line" />{{ badge }}</span>
    </header>
    <div class="relative flex min-h-0 flex-1 flex-col gap-[8px]">
      <slot />
    </div>
  </div>
</template>
