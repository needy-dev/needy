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
  <!--
    On the page a scene sits on the page itself: no card and no grid around it. Only the banners keep their card. Without
    a card, the margin the design keeps inside it would indent the picture from the text around it, so the figure reaches
    out by that margin on both sides (about 40 of 1440 design pixels).
  -->
  <figure ref="outer" class="figure my-0" :class="capture ? 'mx-0 w-auto' : 'mx-[-2.8%] w-[calc(100%+5.6%)]'" :aria-label="label">
    <div
      class="relative" :class="[scrolls ? 'overflow-x-auto overflow-y-hidden [-webkit-overflow-scrolling:touch]' : 'overflow-hidden', capture ? 'rounded-[32px]' : 'rounded-[28px]']"
      :style="{ height: `${height * scale}px` }"
    >
      <div class="relative" :style="{ width: `${width * scale}px`, height: `${height * scale}px` }">
      <!--
        --ground is the ground of the scene: paper in a banner, a step off the page on the page. Wires.vue cuts its labels
        out of it.
        NOTICE: the old CSS made it transparent on the page and in a bare frame with `.figure > .box > .card`, which never
        matched once the track sat between box and card, so the card has always been paper. Kept as rendered: paper on
        the paper page looks the same, and a bare banner (the hero) keeps its paper ground.
      -->
      <div
        class="absolute left-0 top-0 flex flex-col rounded-[32px] bg-[var(--ground)] [--ground:var(--paper)] [transform-origin:0_0]"
        :style="{ width: `${width}px`, height: `${height}px`, transform: `scale(${scale})` }"
      >
        <!-- A faint grid, so the actor's glass reads as glass. -->
        <div
          class="relative flex-1 rounded-[32px_32px_0_0]"
          :class="{ '[background-image:radial-gradient(color-mix(in_srgb,var(--ink)_10%,transparent)_1px,transparent_1.3px)] [background-position:11px_11px] [background-size:22px_22px]': capture && !bare }"
        >
          <slot :at="index" />
        </div>
        <p v-if="captioned" class="m-0 flex h-[64px] items-center justify-center gap-[16px] px-[40px] pb-[18px] pt-0 text-[19px] leading-[1.4]">
          <span class="flex flex-none gap-[5px]" aria-hidden="true">
            <i
              v-for="(_, s) in steps" :key="s" class="h-[4px] w-[18px] rounded-[2px] [transition:background_0.3s]"
              :class="s < index ? 'bg-ink-2' : s === index ? 'bg-actor-fill' : 'bg-control-3'"
            />
          </span>
          <Transition
            mode="out-in"
            enter-active-class="[transition:opacity_0.25s,transform_0.25s_cubic-bezier(0.2,0,0,1)]"
            leave-active-class="[transition:opacity_0.25s,transform_0.25s_cubic-bezier(0.2,0,0,1)]"
            enter-from-class="opacity-0 [transform:translateY(6px)]"
            leave-to-class="opacity-0 [transform:translateY(-6px)]"
          >
            <span :key="index" class="text-ink" aria-live="polite">{{ caption }}</span>
          </Transition>
        </p>
      </div>
      </div>
    </div>
    <p v-if="scrolls" class="mx-0 mb-0 mt-[8px] text-center text-[13px] text-muted">
      ← {{ tr({ en: 'swipe to see the whole picture', zh: '左右滑动查看完整画面' }) }} →
    </p>
    <div v-if="!capture && !bare" class="mt-[12px] flex items-center gap-[6px]">
      <button type="button" class="grid h-[34px] w-[34px] cursor-pointer place-items-center rounded-[999px] border-0 bg-control text-[14px] hover:bg-control-2" aria-label="Previous step" @click="prev">
        <Icon name="mingcute:left-line" />
      </button>
      <button type="button" class="grid h-[34px] w-[34px] cursor-pointer place-items-center rounded-[999px] border-0 bg-control text-[14px] hover:bg-control-2" :aria-label="paused ? 'Play' : 'Pause'" @click="toggle">
        <Icon :name="paused ? 'mingcute:play-fill' : 'mingcute:pause-fill'" />
      </button>
      <button type="button" class="grid h-[34px] w-[34px] cursor-pointer place-items-center rounded-[999px] border-0 bg-control text-[14px] hover:bg-control-2" aria-label="Next step" @click="next">
        <Icon name="mingcute:right-line" />
      </button>
      <span class="ml-[8px] flex flex-wrap gap-[4px]">
        <button
          v-for="(_, s) in steps" :key="s" type="button" class="h-[8px] w-[22px] cursor-pointer rounded-[4px] border-0 p-0"
          :class="s === index ? 'bg-actor-fill' : 'bg-control-2'"
          :aria-label="`Step ${s + 1}`" @click="go(s)"
        />
      </span>
    </div>
  </figure>
</template>
