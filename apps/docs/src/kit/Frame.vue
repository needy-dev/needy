<script setup lang="ts">
import type { Step } from './player'

import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

import Icon from './Icon.vue'

import { tr } from '../i18n'
import { usePlayer } from './player'

/**
 * One scene at its design size: a rounded card with the stage and a caption under it. On the page it scales to the
 * width of its parent, plays while it is on screen, and has controls under it. In a capture it draws at its design
 * size, and the banner script drives it.
 */
const props = defineProps<{
  width: number
  height: number
  steps: readonly Step[]
  capture?: boolean
  /** What the scene shows, for a screen reader. */
  label: string
  /** No card around the scene: it sits on the page (or on a transparent banner) as it is. */
  bare?: boolean
}>()

const outer = ref<HTMLElement>()
const measured = ref(props.width)
const visible = ref(props.capture === true)
let resize: ResizeObserver | undefined
let seen: IntersectionObserver | undefined

onMounted(() => {
  if (props.capture || !outer.value)
    return
  resize = new ResizeObserver(() => (measured.value = outer.value!.clientWidth))
  resize.observe(outer.value)
  measured.value = outer.value.clientWidth
  seen = new IntersectionObserver(([entry]) => (visible.value = entry?.isIntersecting ?? false), { threshold: 0.35 })
  seen.observe(outer.value)
})
onBeforeUnmount(() => {
  resize?.disconnect()
  seen?.disconnect()
})

const { index, paused, go, next, prev, toggle } = usePlayer(props.steps, { capture: props.capture === true, visible })
/** On a narrow screen a scene keeps a readable size and scrolls sideways instead of shrinking past it. */
const MIN_SCALE = 0.55
const scale = computed(() => props.capture ? 1 : Math.min(1, Math.max(MIN_SCALE, measured.value / props.width)))
const scrolls = computed(() => !props.capture && measured.value / props.width < MIN_SCALE)
const caption = computed(() => tr(props.steps[index.value]?.caption))
const captioned = computed(() => props.steps.some(step => step.caption))
</script>

<template>
  <figure ref="outer" class="figure" :class="{ capture, bare }" :aria-label="label">
    <div class="box" :class="{ scrolls }" :style="{ height: `${height * scale}px` }">
      <div class="track" :style="{ width: `${width * scale}px`, height: `${height * scale}px` }">
      <div class="card" :style="{ width: `${width}px`, height: `${height}px`, transform: `scale(${scale})` }">
        <div class="stage">
          <slot :at="index" />
        </div>
        <p v-if="captioned" class="caption">
          <span class="dots" aria-hidden="true">
            <i v-for="(_, s) in steps" :key="s" :class="{ done: s < index, on: s === index }" />
          </span>
          <Transition name="caption" mode="out-in">
            <span :key="index" class="text" aria-live="polite">{{ caption }}</span>
          </Transition>
        </p>
      </div>
      </div>
    </div>
    <p v-if="scrolls" class="swipe">
      ← {{ tr({ en: 'swipe to see the whole picture', zh: '左右滑动查看完整画面' }) }} →
    </p>
    <div v-if="!capture && !bare" class="controls">
      <button type="button" aria-label="Previous step" @click="prev">
        <Icon name="mingcute:left-line" />
      </button>
      <button type="button" :aria-label="paused ? 'Play' : 'Pause'" @click="toggle">
        <Icon :name="paused ? 'mingcute:play-fill' : 'mingcute:pause-fill'" />
      </button>
      <button type="button" aria-label="Next step" @click="next">
        <Icon name="mingcute:right-line" />
      </button>
      <span class="steps">
        <button
          v-for="(_, s) in steps" :key="s" type="button" class="step" :class="{ on: s === index }"
          :aria-label="`Step ${s + 1}`" @click="go(s)"
        />
      </span>
    </div>
  </figure>
</template>

<style scoped>
.figure {
  margin: 0;
  width: 100%;
}

.figure.capture {
  width: auto;
}

.box {
  position: relative;
  overflow: hidden;
  border-radius: 28px;
}

.box.scrolls {
  overflow-x: auto;
  overflow-y: hidden;
  -webkit-overflow-scrolling: touch;
}

.track {
  position: relative;
}

.swipe {
  margin: 8px 0 0;
  color: var(--muted);
  font-size: 13px;
  text-align: center;
}

.capture .box {
  border-radius: 32px;
}

.card {
  position: absolute;
  top: 0;
  left: 0;
  display: flex;
  flex-direction: column;
  border-radius: 32px;
  /* The ground of the scene: paper in a banner, a step off the page on the page. Wires.vue cuts its labels out of it. */
  --ground: var(--paper);
  background: var(--ground);
  transform-origin: 0 0;
}

/* On the page a scene sits on the page itself: no card and no grid around it. Only the banners keep their card. */
.figure:not(.capture) > .box > .card {
  --ground: transparent;
}

.figure:not(.capture) .stage {
  background-image: none;
}

/* Without a card, the margin the design keeps inside it would indent the picture from the text around it, so the
   figure reaches out by that margin on both sides (about 40 of 1440 design pixels). */
.figure:not(.capture) {
  width: calc(100% + 5.6%);
  margin-inline: -2.8%;
}

.figure.bare > .box,
.figure.bare > .box > .card {
  --ground: transparent;
  background: transparent;
}

.figure.bare .stage {
  background-image: none;
}

.stage {
  position: relative;
  flex: 1;
  /* A faint grid, so the actor's glass reads as glass. */
  background-image: radial-gradient(color-mix(in srgb, var(--ink) 10%, transparent) 1px, transparent 1.3px);
  background-position: 11px 11px;
  background-size: 22px 22px;
  border-radius: 32px 32px 0 0;
}

.caption {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 16px;
  height: 64px;
  margin: 0;
  padding: 0 40px 18px;
  font-size: 19px;
  line-height: 1.4;
}

.dots {
  display: flex;
  flex: none;
  gap: 5px;
}

.dots i {
  width: 18px;
  height: 4px;
  border-radius: 2px;
  background: var(--control-3);
  transition: background 0.3s;
}

.dots i.done {
  background: var(--ink-2);
}

.dots i.on {
  background: var(--actor-fill);
}

.text {
  color: var(--ink);
}

.caption-enter-active,
.caption-leave-active {
  transition:
    opacity 0.25s,
    transform 0.25s cubic-bezier(0.2, 0, 0, 1);
}

.caption-enter-from {
  opacity: 0;
  transform: translateY(6px);
}

.caption-leave-to {
  opacity: 0;
  transform: translateY(-6px);
}

.controls {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 12px;
}

.controls > button {
  display: grid;
  width: 34px;
  height: 34px;
  place-items: center;
  border: 0;
  border-radius: 999px;
  background: var(--control);
  cursor: pointer;
  font-size: 14px;
}

.controls > button:hover {
  background: var(--control-2);
}

.steps {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-left: 8px;
}

.step {
  width: 22px;
  height: 8px;
  padding: 0;
  border: 0;
  border-radius: 4px;
  background: var(--control-2);
  cursor: pointer;
}

.step.on {
  background: var(--actor-fill);
}
</style>
