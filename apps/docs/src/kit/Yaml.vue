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

const TOKEN: Record<Part['kind'], string> = {
  key: 'text-code-key',
  string: 'text-code-string',
  comment: 'text-code-comment',
  punct: 'text-code-punct',
  plain: '',
}
</script>

<template>
  <div class="yaml mono overflow-hidden rounded-[14px] bg-surface text-[13.5px] [box-shadow:var(--shadow-card)]" :style="{ '--role': color(role), '--role-soft': color(role, 'soft') }">
    <div v-if="title" class="px-[16px] pb-0 pt-[10px] text-[12px] text-muted">
      {{ title }}
    </div>
    <pre class="m-0 px-0 pb-[12px] pt-[8px] leading-[1.62]"><code><span v-for="(line, i) in lines" :key="i" class="block px-[16px] py-0 [transition:background_0.3s,opacity_0.3s]" :class="{ 'bg-role-soft': line.lit, 'opacity-55': lit.length > 0 && !line.lit }"><span v-for="(part, j) in line.parts" :key="j" :class="TOKEN[part.kind]">{{ part.text }}</span>
</span></code></pre>
  </div>
</template>
