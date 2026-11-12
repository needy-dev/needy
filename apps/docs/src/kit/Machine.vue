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
    class="f flex flex-col gap-[8px] rounded-[14px] bg-surface p-[14px]" :class="[focus, flow ? 'relative' : 'absolute']"
    :style="{ 'left': flow ? undefined : `${x}px`, 'top': flow ? undefined : `${y}px`, 'width': flow ? undefined : `${w}px`, 'height': h ? `${h}px` : undefined, '--role': color(role) }"
  >
    <header class="flex items-center gap-[11px]">
      <span v-if="icon" class="grid h-[38px] w-[38px] flex-none place-items-center rounded-[10px] bg-role/15 text-role"><Icon :name="icon" :size="20" /></span>
      <Logo v-else-if="logo" :name="logo" :size="26" />
      <span class="flex min-w-0 flex-1 flex-col">
        <span class="display overflow-hidden text-ellipsis whitespace-nowrap text-[17px]">{{ title }}</span>
        <span v-if="sub" class="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] text-muted">{{ sub }}</span>
      </span>
      <code v-if="name" class="flex-none rounded-[7px] bg-role/13 px-[8px] py-[2px] text-[13px] text-role">{{ name }}</code>
    </header>
    <div v-if="$slots.default" class="flex min-h-0 flex-1 flex-col gap-[6px]">
      <slot />
    </div>
    <span v-if="badge" class="mt-auto inline-flex items-center gap-[4px] whitespace-nowrap text-[12px] text-muted"><Icon name="mingcute:box-3-line" />{{ badge }}</span>
  </div>
</template>
