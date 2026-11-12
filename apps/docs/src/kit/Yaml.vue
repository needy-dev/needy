<script setup lang="ts">
import type { Role } from './roles'

import { computed } from 'vue'

import { color } from './roles'

/**
 * A profile, in YAML, with the lines that matter now lit in the color of `role`.
 *
 * NOTICE: the colors come from a small tokenizer here, not from a highlighter. The scenes show a few short profiles
 * with keys, strings and comments only, and a highlighter would ship a grammar and a theme engine for that.
 */
const props = withDefaults(defineProps<{
  code: string
  lit?: readonly number[]
  role?: Role
  title?: string
}>(), { lit: () => [], role: 'actor' })

type Part = { text: string, kind: 'key' | 'string' | 'comment' | 'punct' | 'plain' }

function tokenize(line: string): Part[] {
  const parts: Part[] = []
  const pattern = /(#.*$)|("[^"]*")|([\w.-]+)(?=:\s|:$)|([{}[\],:-])/g
  let last = 0
  for (const m of line.matchAll(pattern)) {
    if (m.index > last)
      parts.push({ text: line.slice(last, m.index), kind: 'plain' })
    parts.push({ text: m[0], kind: m[1] ? 'comment' : m[2] ? 'string' : m[3] ? 'key' : 'punct' })
    last = m.index + m[0].length
  }
  if (last < line.length)
    parts.push({ text: line.slice(last), kind: 'plain' })
  return parts
}

const lines = computed(() => props.code.split('\n').map((line, i) => ({ parts: tokenize(line), lit: props.lit.includes(i) })))
</script>

<template>
  <div class="yaml mono" :style="{ '--role': color(role), '--role-soft': color(role, 'soft') }">
    <div v-if="title" class="title">
      {{ title }}
    </div>
    <pre><code><span v-for="(line, i) in lines" :key="i" class="line" :class="{ lit: line.lit, faded: lit.length > 0 && !line.lit }"><span v-for="(part, j) in line.parts" :key="j" :class="part.kind">{{ part.text }}</span>
</span></code></pre>
  </div>
</template>

<style scoped>
.yaml {
  overflow: hidden;
  border-radius: 14px;
  background: var(--surface);
  box-shadow: var(--shadow-card);
  font-size: 13.5px;
}

.title {
  padding: 10px 16px 0;
  color: var(--muted);
  font-size: 12px;
}

pre {
  margin: 0;
  padding: 8px 0 12px;
  line-height: 1.62;
}

.line {
  display: block;
  padding: 0 16px;
  transition:
    background 0.3s,
    opacity 0.3s;
}

.line.lit {
  background: var(--role-soft);
}

.line.faded {
  opacity: 0.55;
}

.key {
  color: var(--code-key);
}

.string {
  color: var(--code-string);
}

.comment {
  color: var(--code-comment);
}

.punct {
  color: var(--code-punct);
}
</style>
