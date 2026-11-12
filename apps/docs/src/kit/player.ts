import type { Ref } from 'vue'

import type { Text } from '../i18n'

import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

/** One step of a scene: how long it shows, and what the caption says meanwhile. */
export interface Step {
  ms: number
  caption?: string | Text
}

type Target = Window & { __captureFrames?: number[], __captureAt?: (i: number) => void }

/**
 * Steps through a scene. On the page it plays while `visible`, and loops. In a capture, the banner script drives it,
 * as in the injeca Docs: `__captureFrames` holds how long each step shows, and `__captureAt(i)` moves to step `i`.
 * Source: https://github.com/moeru-ai/injeca/blob/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs/src/components/CompareFigure.vue#L46-L60
 */
export function usePlayer(steps: readonly Step[], options: { capture: boolean, visible: Ref<boolean> }) {
  const index = ref(0)
  const paused = ref(false)
  let driven = false
  let timer: ReturnType<typeof setTimeout> | undefined

  const playing = computed(() => !driven && !paused.value && options.visible.value)

  function schedule(): void {
    clearTimeout(timer)
    if (!playing.value)
      return
    timer = setTimeout(() => {
      index.value = (index.value + 1) % steps.length
      schedule()
    }, steps[index.value]!.ms)
  }

  /** Moves to step `i` and plays on from there. */
  function go(i: number): void {
    index.value = (i + steps.length) % steps.length
    schedule()
  }

  watch(playing, schedule)

  onMounted(() => {
    if (options.capture) {
      const target = window as Target
      target.__captureFrames = steps.map(step => step.ms)
      target.__captureAt = (i: number) => {
        driven = true
        clearTimeout(timer)
        index.value = i
      }
    }
    schedule()
  })
  onBeforeUnmount(() => clearTimeout(timer))

  return {
    index,
    paused,
    go,
    next: () => go(index.value + 1),
    prev: () => go(index.value - 1),
    toggle: () => {
      paused.value = !paused.value
    },
  }
}
