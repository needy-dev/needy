<script setup lang="ts">
import type { Line } from './terminal'

import { computed } from 'vue'

import { color } from './roles'
import { timeline } from './terminal'

/**
 * A terminal. The lines before `fresh` stand still; the new ones play in order, a command typing itself out. It keeps
 * its last `rows` lines, so it scrolls the way a real one does.
 */
const props = withDefaults(defineProps<{
  lines: readonly Line[]
  fresh?: number
  title?: string
  prompt?: string
  rows?: number
  /** Changes when the step changes, so the new lines play again on a replay of the same step. */
  play?: number | string
  /** As tall as its lines, from the top, instead of filling its parent from the bottom. */
  fit?: boolean
}>(), { fresh: 0, prompt: '$', rows: 12 })

const shown = computed(() => {
  const { at, typed } = timeline(props.lines, props.fresh)
  const all = props.lines.map((line, i) => ({ line, i, at: at[i]!, typed: typed[i]!, fresh: i >= props.fresh }))
  return all.slice(-props.rows)
})

/** The color of an output line's text, by its kind. */
const TONE: Record<Exclude<Line['kind'], 'cmd' | undefined>, string> = {
  out: 'text-[rgb(255_255_255/78%)]',
  dim: 'text-[rgb(255_255_255/45%)]',
  err: 'text-bad-term',
  fault: 'text-fault-term',
  ok: 'text-executor-term',
}
</script>

<template>
  <div
    class="terminal mono flex min-h-0 flex-col overflow-hidden rounded-[14px] bg-term text-[14px] text-term-ink leading-[1.6]"
    :class="fit ? 'h-auto' : 'h-full'"
  >
    <div class="flex flex-none items-center gap-[6px] px-[12px] pb-0 pt-[9px]">
      <i class="h-[9px] w-[9px] rounded-[50%] bg-[rgb(255_255_255/16%)]" /><i class="h-[9px] w-[9px] rounded-[50%] bg-[rgb(255_255_255/16%)]" /><i class="h-[9px] w-[9px] rounded-[50%] bg-[rgb(255_255_255/16%)]" />
      <span v-if="title" class="ml-[8px] text-[12px] text-[rgb(255_255_255/45%)]">{{ title }}</span>
    </div>
    <div :key="play" class="flex min-h-0 flex-1 flex-col px-[14px] pb-[10px] pt-[6px]" :class="fit ? 'justify-start' : 'justify-end'">
      <div
        v-for="row in shown" :key="row.i" class="line flex min-h-[22.4px] items-center whitespace-pre"
        :class="{ '[animation:appear_0.01s_both]': row.fresh }"
        :style="{ animationDelay: `${row.at}ms` }"
      >
        <template v-if="row.line.kind === 'cmd'">
          <span class="text-actor-term">{{ row.line.prompt ?? prompt }}&nbsp;</span>
          <span
            class="inline-block overflow-hidden" :class="{ 'w-[calc(var(--n)*1ch)] [animation-fill-mode:both] [animation-name:type-in]': row.fresh }"
            :style="row.fresh ? { '--n': row.line.text.length, 'animationDelay': `${row.at}ms`, 'animationDuration': `${row.typed}ms`, 'animationTimingFunction': `steps(${row.line.text.length}, end)` } : {}"
          >{{ row.line.text }}</span>
        </template>
        <span v-else :class="TONE[row.line.kind ?? 'out']">{{ row.line.text }}</span>
        <span
          v-if="row.line.tag" class="ml-[12px] rounded-[999px] bg-[var(--fill)] px-[9px] text-[12px] text-[#fff] leading-[20px]"
          :class="{ '[animation:enter-left-6_0.3s_cubic-bezier(0.2,0,0,1)_both]': row.fresh }"
          :style="{ '--fill': color(row.line.tag.role, 'fill'), 'animationDelay': `${row.at + row.typed + 120}ms` }"
        >{{ row.line.tag.text }}</span>
      </div>
    </div>
  </div>
</template>
