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
</script>

<template>
  <div class="terminal mono" :class="{ fit }">
    <div class="bar">
      <i /><i /><i />
      <span v-if="title" class="title">{{ title }}</span>
    </div>
    <div :key="play" class="lines">
      <div
        v-for="row in shown" :key="row.i" class="line" :class="[row.line.kind ?? 'out', { fresh: row.fresh }]"
        :style="{ animationDelay: `${row.at}ms` }"
      >
        <template v-if="row.line.kind === 'cmd'">
          <span class="prompt">{{ row.line.prompt ?? prompt }}&nbsp;</span>
          <span
            class="typed" :class="{ typing: row.fresh }"
            :style="row.fresh ? { '--n': row.line.text.length, 'animationDelay': `${row.at}ms`, 'animationDuration': `${row.typed}ms`, 'animationTimingFunction': `steps(${row.line.text.length}, end)` } : {}"
          >{{ row.line.text }}</span>
        </template>
        <span v-else class="text">{{ row.line.text }}</span>
        <span
          v-if="row.line.tag" class="tag" :class="{ fresh: row.fresh }"
          :style="{ '--fill': color(row.line.tag.role, 'fill'), 'animationDelay': `${row.at + row.typed + 120}ms` }"
        >{{ row.line.tag.text }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.terminal {
  display: flex;
  height: 100%;
  min-height: 0;
  flex-direction: column;
  overflow: hidden;
  border-radius: 14px;
  background: var(--term);
  color: var(--term-ink);
  font-size: 14px;
  line-height: 1.6;
}

.bar {
  display: flex;
  flex: none;
  align-items: center;
  gap: 6px;
  padding: 9px 12px 0;
}

.bar i {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: rgb(255 255 255 / 16%);
}

.title {
  margin-left: 8px;
  color: rgb(255 255 255 / 45%);
  font-size: 12px;
}

.lines {
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  justify-content: flex-end;
  padding: 6px 14px 10px;
}

.line {
  display: flex;
  align-items: center;
  min-height: 22.4px;
  white-space: pre;
}

.line.fresh {
  animation: appear 0.01s both;
}

@keyframes appear {
  from {
    visibility: hidden;
  }

  to {
    visibility: visible;
  }
}

.prompt {
  color: var(--actor-term);
}

.typed {
  display: inline-block;
  overflow: hidden;
}

.typed.typing {
  width: calc(var(--n) * 1ch);
  animation-name: type;
  animation-fill-mode: both;
}

@keyframes type {
  from {
    width: 0;
  }
}

.out .text {
  color: rgb(255 255 255 / 78%);
}

.dim .text {
  color: rgb(255 255 255 / 45%);
}

.err .text {
  color: var(--bad-term);
}

.fault .text {
  color: var(--fault-term);
}

.ok .text {
  color: var(--executor-term);
}

.tag {
  margin-left: 12px;
  padding: 0 9px;
  border-radius: 999px;
  background: var(--fill);
  color: #fff;
  font-size: 12px;
  line-height: 20px;
}

.tag.fresh {
  animation: tag 0.3s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes tag {
  from {
    opacity: 0;
    transform: translateX(-6px);
  }
}
</style>

<style scoped>
.terminal.fit {
  height: auto;
}

.terminal.fit .lines {
  justify-content: flex-start;
}
</style>
