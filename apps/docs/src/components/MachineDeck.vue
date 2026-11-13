<script setup lang="ts">
import type { IconName } from '../icons'

import { animate } from 'animejs'
import { onBeforeUnmount, onMounted, ref } from 'vue'

import Icon from '../kit/Icon.vue'

/**
 * The machines that the repository's recipes describe, one card at a time: the front card leaves upward and the next
 * rises into its place. Only real recipes: the two built-in ones and examples/opencode's.
 * Source: https://github.com/needy-dev/needy/tree/4a996b82f3d26665f24d5838d4415d18c64f881a/crates/config/src/builtin/recipes
 * TODO(docs-recipes): AWS or Azure cards wait for recipes of those machines; there are none in the repository yet.
 */

interface Machine { icon: IconName, name: string, cpu: string, host: string, id: string, tone: string }

const MACHINES: Machine[] = [
  { icon: 'mingcute:cloud-line', name: 'Google Compute Engine', cpu: 'Intel(R) Xeon(R) CPU @ 2.80GHz', host: 'instance-20261112-093000', id: '8086:1237', tone: 'var(--actor-fill)' },
  { icon: 'mingcute:monitor-line', name: 'Dell OptiPlex 7000', cpu: '12th Gen Intel(R) Core(TM) i7-12700', host: 'optiplex-3f9c0e2a', id: '8086:4668', tone: 'var(--executor-fill)' },
  { icon: 'mingcute:cloud-line', name: 'Compute Engine, older Xeon', cpu: 'Intel(R) Xeon(R) CPU @ 2.20GHz', host: 'instance-20261112-093000', id: 'family 6 model 79', tone: 'var(--trace-fill)' },
]

const front = ref(0)
const cards = ref<HTMLElement[]>([])
const root = ref<HTMLElement>()
let timer: ReturnType<typeof setInterval> | undefined
let seen: IntersectionObserver | undefined
let visible = false

function next(): void {
  const out = cards.value[front.value]
  front.value = (front.value + 1) % MACHINES.length
  const into = cards.value[front.value]
  if (out)
    animate(out, { y: [0, -28], opacity: [1, 0], duration: 520, ease: 'inOutQuad' })
  if (into)
    animate(into, { y: [36, 0], opacity: [0, 1], duration: 760, delay: 140, ease: 'outCubic' })
}

onMounted(() => {
  cards.value.forEach((card, i) => {
    card.style.opacity = i === 0 ? '1' : '0'
  })
  if (matchMedia('(prefers-reduced-motion: reduce)').matches || !root.value)
    return
  seen = new IntersectionObserver(([entry]) => (visible = entry?.isIntersecting ?? false), { threshold: 0.4 })
  seen.observe(root.value)
  timer = setInterval(() => visible && next(), 2800)
})
onBeforeUnmount(() => {
  clearInterval(timer)
  seen?.disconnect()
})
</script>

<template>
  <div ref="root" class="relative mb-[-24px] h-[128px]" aria-live="polite">
    <div
      v-for="(m, i) in MACHINES" :key="m.name" :ref="el => (cards[i] = el as HTMLElement)"
      class="absolute inset-[0_0_auto] h-[152px] flex flex-col gap-[10px] rounded-[18px_18px_0_0] p-[20px] text-[#fff] [background:linear-gradient(140deg,var(--tone),color-mix(in_srgb,var(--tone)_55%,#000))]"
      :style="{ '--tone': m.tone }" :aria-hidden="front !== i"
    >
      <span class="flex items-center gap-[8px] whitespace-nowrap text-[18px] font-display"><Icon :name="m.icon" :size="18" /> {{ m.name }}</span>
      <span class="mono text-[13px] opacity-90">{{ m.cpu }}</span>
      <span class="mono flex justify-between gap-[12px] text-[12px] opacity-70"><span>{{ m.host }}</span><span>{{ m.id }}</span></span>
    </div>
  </div>
</template>
