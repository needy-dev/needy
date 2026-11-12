<script setup lang="ts">
import type { Role } from './roles'

import { computed } from 'vue'

import { color } from './roles'

/**
 * A profile in YAML on a card, at a fixed line height so a scene can point at a line: line `i` has its middle at
 * `profileLineY(top, i)`. The lines that matter now get a tint in the color of `role` and a pill in the gutter; the
 * rest step back. `role` can differ per line with `roles`.
 *
 * NOTICE: the colors come from a small tokenizer here, as in Yaml.vue: the scenes show short profiles with keys,
 * strings and comments only, and a highlighter would ship a grammar and a theme engine for that.
 */
const props = withDefaults(defineProps<{
  code: string
  lit?: readonly number[]
  role?: Role
  /** A role per line, for lines lit in another color than `role`. */
  roles?: Readonly<Record<number, Role>>
  title?: string
}>(), { lit: () => [], role: 'actor', roles: () => ({}) })

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

const lines = computed(() => props.code.split('\n').map((line, i) => ({
  parts: tokenize(line),
  lit: props.lit.includes(i),
  role: props.roles[i] ?? props.role,
})))

const TOKEN: Record<Part['kind'], string> = {
  key: 'text-code-key',
  string: 'text-code-string',
  comment: 'text-code-comment',
  punct: 'text-code-punct',
  plain: '',
}
</script>

<template>
  <!-- Keep in step with profileLineY in profile.ts: 10px above the lines, 30px with a title, 20px a line. -->
  <div class="mono overflow-hidden rounded-[14px] bg-surface text-[13px]">
    <div v-if="title" class="h-[24px] px-[18px] pb-0 pt-[8px] font-sans text-[12.5px] text-muted leading-[16px]">
      {{ title }}
    </div>
    <pre class="m-0 px-0 pb-[12px] leading-[20px]" :class="title ? 'pt-[6px]' : 'pt-[10px]'"><code><span
      v-for="(line, i) in lines" :key="i" class="relative block h-[20px] py-0 pl-[22px] pr-[16px] whitespace-pre [transition:background_0.35s,opacity_0.35s]"
      :class="{ 'bg-role/11': line.lit, 'opacity-45': lit.length > 0 && !line.lit }"
      :style="{ '--role': color(line.role), '--role-fill': color(line.role, 'fill') }"
    ><i
      class="absolute left-[8px] top-[6px] h-[8px] w-[6px] rounded-[3px] bg-role-fill [transition:opacity_0.35s,transform_0.35s_cubic-bezier(0.2,0,0,1)]"
      :class="line.lit ? 'opacity-100 [transform:none]' : 'opacity-0 [transform:scale(0.4)]'"
    /><span v-for="(part, j) in line.parts" :key="j" :class="TOKEN[part.kind]">{{ part.text }}</span>
</span></code></pre>
  </div>
</template>
