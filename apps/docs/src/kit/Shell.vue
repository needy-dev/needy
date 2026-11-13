<script setup lang="ts">
import { StreamLanguage } from '@codemirror/language'
import { shell } from '@codemirror/legacy-modes/mode/shell'
import { highlightCode, tagHighlighter, tags } from '@lezer/highlight'
import { computed } from 'vue'

/**
 * Shell commands with their syntax colored: CodeMirror's shell mode tokenizes them, and the tokens take Catppuccin
 * Mocha's colors, since a block of commands sits on a dark terminal in both themes.
 */
const props = defineProps<{ code: string }>()

const parser = StreamLanguage.define(shell).parser

const highlighter = tagHighlighter([
  { tag: tags.comment, class: 'c' },
  { tag: [tags.string, tags.special(tags.string)], class: 's' },
  { tag: tags.keyword, class: 'k' },
  { tag: [tags.variableName, tags.definition(tags.variableName)], class: 'v' },
  { tag: [tags.number, tags.atom], class: 'n' },
  { tag: tags.operator, class: 'o' },
])

type Token = { text: string, cls: string }

/**
 * Catppuccin Mocha's color for each kind of token: https://github.com/catppuccin/palette/blob/v1.8.0/palette.json
 * A plain word takes the text color, as the block does.
 */
const COLOR: Record<string, string> = {
  '': 'text-[#cdd6f4]',
  'c': 'text-[#9399b2] italic',
  's': 'text-[#a6e3a1]',
  'k': 'text-[#cba6f7]',
  'v': 'text-[#fab387]',
  'n': 'text-[#f9e2af]',
  'o': 'text-[#89dceb]',
  'cmd': 'text-[#89b4fa]',
  'f': 'text-[#94e2d5]',
}

/**
 * The code as lines of tokens. The shell mode colors comments, strings and variables but leaves plain words as one
 * run, so plain runs are split into words here: a line's first word, and the first word after `&&`, `|` or `;`, is
 * the command, and a word starting with `-` is a flag.
 */
const lines = computed(() => {
  const out: Token[][] = [[]]
  highlightCode(props.code, parser.parse(props.code), highlighter, (text, cls) => {
    if (cls) {
      out.at(-1)!.push({ text, cls })
      return
    }
    for (const word of text.split(/(\s+)/)) {
      if (word)
        out.at(-1)!.push({ text: word, cls: '' })
    }
  }, () => out.push([]))
  for (const line of out) {
    let expect = true
    for (const t of line) {
      if (/^\s+$/.test(t.text))
        continue
      if (t.cls === 'c')
        break
      if (expect && (!t.cls || t.cls === 'k' || t.cls === 'v')) {
        t.cls = 'cmd'
        expect = false
        continue
      }
      expect = false
      if (!t.cls && t.text.startsWith('-'))
        t.cls = 'f'
      if (t.text === '&&' || t.text === '|' || t.text === ';' || t.text.endsWith(';'))
        expect = true
    }
  }
  return out
})
</script>

<template>
  <pre class="mono text-[#cdd6f4]"><template v-for="(line, l) in lines" :key="l"><span v-for="(t, i) in line" :key="i" :class="COLOR[t.cls]">{{ t.text }}</span>{{ l < lines.length - 1 ? '\n' : '' }}</template></pre>
</template>
