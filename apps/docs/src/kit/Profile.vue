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
</script>

<template>
  <div class="profile mono" :class="{ titled: title }">
    <div v-if="title" class="title">
      {{ title }}
    </div>
    <pre><code><span
      v-for="(line, i) in lines" :key="i" class="line" :class="{ lit: line.lit, faded: lit.length > 0 && !line.lit }"
      :style="{ '--role': color(line.role), '--role-fill': color(line.role, 'fill') }"
    ><i class="pill" /><span v-for="(part, j) in line.parts" :key="j" :class="part.kind">{{ part.text }}</span>
</span></code></pre>
  </div>
</template>

<style scoped>
/* Keep in step with profileLineY in profile.ts: 10px above the lines, 30px with a title, 20px a line. */
.profile {
  overflow: hidden;
  border-radius: 14px;
  background: var(--surface);
  font-size: 13px;
}

.title {
  height: 24px;
  padding: 8px 18px 0;
  color: var(--muted);
  font-family: var(--font-sans);
  font-size: 12.5px;
  line-height: 16px;
}

pre {
  margin: 0;
  padding: 10px 0 12px;
  line-height: 20px;
}

.titled pre {
  padding-top: 6px;
}

.line {
  position: relative;
  display: block;
  height: 20px;
  padding: 0 16px 0 22px;
  white-space: pre;
  transition:
    background 0.35s,
    opacity 0.35s;
}

.pill {
  position: absolute;
  top: 6px;
  left: 8px;
  width: 6px;
  height: 8px;
  border-radius: 3px;
  background: var(--role-fill);
  opacity: 0;
  transform: scale(0.4);
  transition:
    opacity 0.35s,
    transform 0.35s cubic-bezier(0.2, 0, 0, 1);
}

.line.lit {
  background: color-mix(in srgb, var(--role) 11%, transparent);
}

.line.lit .pill {
  opacity: 1;
  transform: none;
}

.line.faded {
  opacity: 0.45;
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
