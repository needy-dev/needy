<script setup lang="ts">
import type { Text } from '../i18n'
import type { Environment, Machine } from '../setup/guide'

import { TooltipArrow, TooltipContent, TooltipPortal, TooltipRoot, TooltipTrigger } from 'reka-ui'
import { computed } from 'vue'

import { tr } from '../i18n'
import { locations } from '../setup/guide'

const props = defineProps<{ machine: Machine, env: Environment, terminal?: Text }>()
const location = computed(() => locations(props.env)[props.machine])
const COLORS: Record<Machine, string> = { host: 'bg-gateway', runner: 'bg-executor', actor: 'bg-actor' }
</script>

<template>
  <TooltipRoot>
    <TooltipTrigger as-child>
      <button type="button" class="inline-flex w-fit cursor-help items-center gap-[7px] rounded-[6px] border-0 bg-transparent p-[2px] text-left text-[13px] text-ink-2" :data-machine="machine">
        <span aria-hidden="true" class="h-[8px] w-[8px] shrink-0 rounded-full" :class="COLORS[machine]" />
        <span>{{ tr(terminal ?? location.name) }}</span>
      </button>
    </TooltipTrigger>
    <TooltipPortal>
      <TooltipContent class="z-50 max-w-[320px] rounded-[12px] bg-tip-bg px-[12px] py-[10px] text-[13px] text-tip-ink leading-[1.6] [box-shadow:0_10px_28px_rgb(0_0_0/22%)]" side="top" :side-offset="6">
        {{ tr(location.description) }}
        <TooltipArrow class="fill-tip-bg" :width="10" :height="5" />
      </TooltipContent>
    </TooltipPortal>
  </TooltipRoot>
</template>
