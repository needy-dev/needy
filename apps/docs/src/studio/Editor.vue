<script setup lang="ts">
import type { Completion, CompletionContext } from '@codemirror/autocomplete'
import type { DecorationSet } from '@codemirror/view'

import type { Choice } from './check'

import { autocompletion } from '@codemirror/autocomplete'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { yaml } from '@codemirror/lang-yaml'
import { HighlightStyle, indentOnInput, syntaxHighlighting } from '@codemirror/language'
import { linter, lintGutter } from '@codemirror/lint'
import { EditorState, StateField } from '@codemirror/state'
import { Decoration, EditorView, highlightActiveLine, keymap, lineNumbers, WidgetType } from '@codemirror/view'
import { tags } from '@lezer/highlight'
import { PopoverAnchor, PopoverContent, PopoverPortal, PopoverRoot } from 'reka-ui'
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

import Icon from '../kit/Icon.vue'

import { check } from './check'

/**
 * A YAML editor for a profile. It underlines what needy would reject, and after each value that takes one of a known
 * set (a driver, a file mode, a fault kind…) it puts a small ▾ that opens the choices in place.
 */
const model = defineModel<string>({ required: true })

const host = ref<HTMLElement>()
let view: EditorView | undefined

/** The ▾ after a value with choices. */
class ChoiceWidget extends WidgetType {
  constructor(readonly index: number) {
    super()
  }

  override eq(other: ChoiceWidget): boolean {
    return other.index === this.index
  }

  override toDOM(): HTMLElement {
    const button = document.createElement('button')
    button.className = 'cm-choice'
    button.type = 'button'
    button.dataset.index = String(this.index)
    button.setAttribute('aria-label', 'Choose a value')
    button.textContent = '▾'
    return button
  }

  override ignoreEvent(): boolean {
    return false
  }
}

function choicesOf(state: EditorState): Choice[] {
  return check(state.doc.toString()).choices
}

const choiceField = StateField.define<{ choices: Choice[], decorations: DecorationSet }>({
  create(state) {
    return build(state)
  },
  update(value, tr) {
    return tr.docChanged ? build(tr.state) : value
  },
  provide: field => EditorView.decorations.from(field, value => value.decorations),
})

function build(state: EditorState): { choices: Choice[], decorations: DecorationSet } {
  const choices = choicesOf(state)
  const decorations = Decoration.set(choices.map((c, i) => Decoration.widget({ widget: new ChoiceWidget(i), side: 1 }).range(c.to)), true)
  return { choices, decorations }
}

/** Offers a value's choices while typing it. */
function complete(context: CompletionContext) {
  const { choices } = context.state.field(choiceField)
  const here = choices.find(c => context.pos >= c.from && context.pos <= c.to)
  if (!here)
    return null
  const options: Completion[] = here.values.map(value => ({ label: value, type: 'enum' }))
  return { from: here.from, to: here.to, options }
}

const highlight = HighlightStyle.define([
  // Catppuccin: Latte on the light theme, Mocha on the dark one (the --code-* tokens of src/palette.ts).
  { tag: [tags.propertyName, tags.definition(tags.propertyName)], color: 'var(--code-key)' },
  { tag: [tags.string, tags.special(tags.string)], color: 'var(--code-string)' },
  { tag: [tags.number, tags.bool, tags.null], color: 'var(--code-number)' },
  { tag: tags.keyword, color: 'var(--code-keyword)' },
  { tag: tags.comment, color: 'var(--code-comment)', fontStyle: 'italic' },
  { tag: [tags.punctuation, tags.separator, tags.squareBracket, tags.brace], color: 'var(--code-punct)' },
])

const theme = EditorView.theme({
  '&': { height: '100%', background: 'transparent', color: 'var(--ink)', fontSize: '13.5px' },
  '.cm-scroller': { fontFamily: 'var(--font-mono)', lineHeight: '1.65' },
  '.cm-content': { caretColor: 'var(--ink)', padding: '12px 0' },
  '.cm-gutters': { background: 'transparent', border: 'none', color: 'var(--faint)' },
  '.cm-activeLine': { background: 'color-mix(in srgb, var(--ink) 4%, transparent)' },
  '.cm-activeLineGutter': { background: 'transparent', color: 'var(--ink-2)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': { background: 'color-mix(in srgb, var(--actor) 22%, transparent) !important' },
  '.cm-tooltip': { background: 'var(--surface)', border: 'none', borderRadius: '10px', boxShadow: 'var(--shadow-card)', color: 'var(--ink)' },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': { background: 'var(--actor-soft)', color: 'var(--ink)' },
  '.cm-diagnostic': { borderLeft: 'none', padding: '6px 10px' },
  '.cm-lintRange-error': { backgroundImage: 'none', textDecoration: 'underline wavy var(--bad)', textUnderlineOffset: '3px' },
})

const open = ref(false)
const anchor = ref({ left: 0, top: 0 })
const active = ref<Choice>()

function choose(value: string): void {
  if (!view || !active.value)
    return
  view.dispatch({ changes: { from: active.value.from, to: active.value.to, insert: value } })
  open.value = false
  view.focus()
}

onMounted(() => {
  view = new EditorView({
    parent: host.value!,
    state: EditorState.create({
      doc: model.value,
      extensions: [
        lineNumbers(),
        history(),
        highlightActiveLine(),
        indentOnInput(),
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        yaml(),
        syntaxHighlighting(highlight),
        theme,
        choiceField,
        autocompletion({ override: [complete] }),
        lintGutter(),
        linter(v => check(v.state.doc.toString()).findings.map(f => ({ ...f, to: Math.max(f.to, f.from + 1) })), { delay: 250 }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged)
            model.value = update.state.doc.toString()
        }),
        EditorView.domEventHandlers({
          mousedown(event, v) {
            const target = (event.target as HTMLElement).closest<HTMLElement>('.cm-choice')
            if (!target)
              return false
            event.preventDefault()
            const choice = v.state.field(choiceField).choices[Number(target.dataset.index)]
            const box = target.getBoundingClientRect()
            const base = host.value!.getBoundingClientRect()
            anchor.value = { left: box.left - base.left, top: box.bottom - base.top }
            active.value = choice
            open.value = true
            return true
          },
        }),
      ],
    }),
  })
})

// A profile loaded from outside (a preset, a pasted link) replaces the text.
watch(model, (value) => {
  if (view && value !== view.state.doc.toString())
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } })
})

onBeforeUnmount(() => view?.destroy())
</script>

<template>
  <div class="editor">
    <div ref="host" class="host" />
    <PopoverRoot v-model:open="open">
      <PopoverAnchor as-child>
        <span class="anchor" :style="{ left: `${anchor.left}px`, top: `${anchor.top}px` }" />
      </PopoverAnchor>
      <PopoverPortal>
        <PopoverContent class="nd-choice-menu" side="bottom" align="start" :side-offset="4" @open-auto-focus.prevent>
          <p class="path mono">
            {{ active?.path }}
          </p>
          <button
            v-for="value in active?.values ?? []" :key="value" type="button" class="option mono"
            :class="{ on: value === active?.value }" @click="choose(value)"
          >
            <Icon v-if="value === active?.value" name="mingcute:check-line" />
            <span v-else class="space" />
            {{ value }}
          </button>
        </PopoverContent>
      </PopoverPortal>
    </PopoverRoot>
  </div>
</template>

<style scoped>
.editor {
  position: relative;
  height: 100%;
  min-height: 0;
}

.host {
  height: 100%;
}

.anchor {
  position: absolute;
  width: 1px;
  height: 1px;
}

.host :deep(.cm-choice) {
  display: inline-grid;
  width: 18px;
  height: 18px;
  margin-left: 4px;
  padding: 0;
  place-items: center;
  border: 0;
  border-radius: 6px;
  background: var(--actor-soft);
  color: var(--actor);
  cursor: pointer;
  font-size: 10px;
  line-height: 1;
  vertical-align: 1px;
}

.host :deep(.cm-choice:hover) {
  background: var(--actor-fill);
  color: #fff;
}
</style>

<style>
/* The popover lives in a portal, outside this component's scope. */
.nd-choice-menu {
  z-index: 50;
  display: flex;
  min-width: 200px;
  flex-direction: column;
  gap: 2px;
  padding: 6px;
  border-radius: 12px;
  background: var(--surface);
  box-shadow: var(--shadow-card);
}

.nd-choice-menu .path {
  margin: 2px 8px 6px;
  color: var(--muted);
  font-size: 12px;
}

.nd-choice-menu .option {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: var(--ink);
  cursor: pointer;
  font-size: 13.5px;
  text-align: left;
}

.nd-choice-menu .option:hover,
.nd-choice-menu .option.on {
  background: var(--control);
}

.nd-choice-menu .space {
  width: 1em;
}
</style>
