<script setup lang="ts">
import type { Section } from './content'

import { ToggleGroupItem, ToggleGroupRoot } from 'reka-ui'
import { onBeforeUnmount, onMounted, ref } from 'vue'

import DriverMatrix from './components/DriverMatrix.vue'
import HarnessLoop from './components/HarnessLoop.vue'
import Isolation from './components/Isolation.vue'
import Bento from './components/Bento.vue'
import Future from './components/Future.vue'
import Decisions from './components/Decisions.vue'
import Playground from './components/Playground.vue'
import Recipes from './components/Recipes.vue'
import Start from './components/Start.vue'
import { CAPABILITIES, COMPARISON, LIMITS, SECTIONS, STATUS_TEXT } from './content'
import { locale, tr } from './i18n'
import Icon from './kit/Icon.vue'
import { SCENES } from './scenes'
import Chaos from './scenes/Chaos.vue'
import Cluster from './scenes/Cluster.vue'
import Config from './scenes/Config.vue'
import Files from './scenes/Files.vue'
import Hero from './scenes/Hero.vue'
import Studio from './studio/Studio.vue'
import { theme, toggleTheme } from './theme'

/** `?capture=<scene>` renders one README banner at its size; `&theme=dark` picks the dark theme. */
const scene = new URLSearchParams(location.search).get('capture')
const captured = scene === null ? undefined : SCENES[scene]

if (scene !== null)
  document.documentElement.classList.add('capture')

// scripts/banners.ts waits for this flag: the fonts are in.
void document.fonts.ready.then(() => {
  ;(window as { __ready?: boolean }).__ready = true
})

const repo = 'https://github.com/needy-dev/needy'

/** Splits prose on backticks, so a command written as `cargo test` shows as inline code. */
function inline(text: string): Array<{ code: boolean, text: string }> {
  return text.split('`').map((part, i) => ({ code: i % 2 === 1, text: part })).filter(part => part.text)
}

/**
 * The page shows the guide at `/`, or the profile studio at `/studio`, with the History API. The host serves
 * index.html for `/studio` too (an SPA fallback; Vite's dev and preview servers do).
 */
const isStudio = () => location.pathname.replace(/\/+$/, '').endsWith('/studio')
const page = ref<'guide' | 'studio'>(isStudio() ? 'studio' : 'guide')
/** The base path the page is served under, so the links work from any directory. */
const base = location.pathname.replace(/\/?studio\/?$/, '').replace(/\/$/, '')
const startHref = `${base}/${location.search}#start`
function go(to: 'guide' | 'studio', event?: MouseEvent): void {
  if (event && (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0))
    return
  event?.preventDefault()
  if (page.value === to)
    return
  history.pushState(null, '', to === 'studio' ? `${base}/studio${location.search}` : `${base}/${location.search}`)
  page.value = to
  window.scrollTo(0, 0)
}
const onPop = () => (page.value = isStudio() ? 'studio' : 'guide')
onMounted(() => {
  window.addEventListener('popstate', onPop)
  // Fragment navigation can run before Vue creates the guide sections.
  // Wait for fonts too, so the hero's final height does not move the target.
  void document.fonts.ready.then(() => {
    document.getElementById(location.hash.slice(1))?.scrollIntoView()
  })
})
onBeforeUnmount(() => window.removeEventListener('popstate', onPop))

const ORDER = ['start', 'why', 'harness', 'separation', 'isolation', 'identity', 'decisions', 'files', 'chaos', 'config', 'cluster', 'future'] as const
const NAV: Section[] = ORDER.map(id => SECTIONS[id]!)


</script>

<template>
  <component :is="captured" v-if="captured" capture />
  <template v-else>
    <header class="sticky top-0 z-20 flex items-center gap-[8px] px-[20px] py-[10px] bg-paper/86 [backdrop-filter:blur(12px)] max-[600px]:(flex-wrap gap-[6px] px-[12px])">
      <a class="display text-[22px] text-ink [text-decoration:none] max-[600px]:hidden" :href="`${base}/`" @click="go('guide', $event)">Needy</a>
      <nav class="ml-[16px] flex gap-[4px] max-[600px]:(order-3 ml-0 w-full)">
        <a :href="`${base}/`" class="whitespace-nowrap rounded-[10px] px-[12px] py-[6px] text-[14.5px] [text-decoration:none] max-[600px]:(px-[10px] text-[14px])" :class="page === 'guide' ? 'bg-control text-ink' : 'text-ink-2'" @click="go('guide', $event)">{{ tr({ en: 'Guide', zh: '指南' }) }}</a>
        <a :href="startHref" class="whitespace-nowrap rounded-[10px] px-[12px] py-[6px] text-[14.5px] text-ink-2 [text-decoration:none] max-[600px]:(px-[10px] text-[14px])">{{ tr({ en: 'Get started', zh: '开始使用' }) }}</a>
        <a :href="`${base}/studio`" class="whitespace-nowrap rounded-[10px] px-[12px] py-[6px] text-[14.5px] [text-decoration:none] max-[600px]:(px-[10px] text-[14px])" :class="page === 'studio' ? 'bg-control text-ink' : 'text-ink-2'" @click="go('studio', $event)">{{ tr({ en: 'Profile Studio', zh: 'Profile 工作台' }) }}</a>
      </nav>
      <span class="flex-1" />
      <ToggleGroupRoot v-model="locale" type="single" class="flex rounded-[12px] bg-control p-[3px]" :aria-label="tr({ en: 'Language', zh: '语言' })">
        <ToggleGroupItem value="en" class="cursor-pointer rounded-[9px] bg-transparent px-[10px] py-[5px] text-[13px] text-ink-2 [border:0] data-[state=on]:(bg-surface text-ink)">
          EN
        </ToggleGroupItem>
        <ToggleGroupItem value="zh-Hans" class="cursor-pointer rounded-[9px] bg-transparent px-[10px] py-[5px] text-[13px] text-ink-2 [border:0] data-[state=on]:(bg-surface text-ink)">
          中文
        </ToggleGroupItem>
      </ToggleGroupRoot>
      <button type="button" class="grid h-[36px] w-[36px] cursor-pointer place-items-center rounded-[12px] bg-control text-ink [border:0]" :aria-label="tr({ en: 'Switch theme', zh: '切换主题' })" @click="toggleTheme">
        <Icon :name="theme === 'dark' ? 'mingcute:sun-line' : 'mingcute:moon-line'" :size="18" />
      </button>
      <a class="grid h-[36px] w-[36px] cursor-pointer place-items-center rounded-[12px] bg-control text-ink [border:0] max-[600px]:hidden" :href="repo" aria-label="GitHub"><Icon name="mingcute:github-line" :size="18" /></a>
    </header>

    <main v-if="page === 'studio'" class="px-[32px] pb-[64px] pt-[24px] max-[760px]:px-[16px]">
      <h2 class="m-0 max-w-[24ch] text-[46px] leading-[1.1] tracking-[-0.01em] max-[760px]:text-[34px]">{{ tr({ en: 'Profile Studio', zh: 'Profile 工作台' }) }}</h2>
      <p class="mx-0 mb-[16px] mt-0 max-w-[640px] text-[19px] text-ink-2 leading-[1.7] last-of-type:mb-[44px]">
        {{ tr({ en: 'Paste a profile or start from an example. Pick values from the ▾ menus, add parts, and try where a command goes.', zh: '粘贴一份 profile，或从示例开始。从 ▾ 菜单里选值、添加组成部分，试试一条命令会去哪里。' }) }}
      </p>
      <Studio />
      <h2 class="mx-0 mb-0 mt-[72px] max-w-[24ch] text-[46px] leading-[1.1] tracking-[-0.01em] max-[760px]:text-[34px]">
        {{ tr({ en: 'Rule playground', zh: '规则试验场' }) }}
      </h2>
      <Playground />
    </main>

    <div v-if="page === 'guide'" class="mx-auto max-w-[1440px] px-[20px] pb-[24px] pt-[8px] max-[600px]:(px-[16px] py-0)">
      <Hero />
    </div>

    <div v-if="page === 'guide'" class="pb-[32px]">
      <main class="min-w-0 flex flex-col">
        <!-- Each chapter runs the full width; every other one sits on a band of the surface color, so the page reads
             in sections without lines between them. On the band, cards take the page's color, so they still stand
             apart from what they sit on. -->
        <section
          v-for="(s, n) in NAV" :id="s.id" :key="s.id" class="scroll-mt-[56px] px-[32px] py-[104px] max-[760px]:(px-[16px] py-[72px])"
          :class="n % 2 === 1 ? '[--surface:var(--paper)] bg-band' : ''"
        >
          <div class="mx-auto max-w-[1200px]">
          <header class="mb-[20px] flex flex-col gap-[14px]">
            <span class="flex items-center gap-[10px] text-[14.5px] text-actor tracking-[0.01em]"><span class="mono text-[13px] text-muted">{{ String(n + 1).padStart(2, '0') }}</span>{{ tr(s.nav) }}<span
              v-if="s.status" class="ml-[4px] whitespace-nowrap rounded-[999px] px-[10px] py-[3px] text-[13px]"
              :class="s.status === 'partly' ? 'bg-fault/15 text-fault' : s.status === 'vision' ? 'bg-gateway/14 text-gateway' : 'bg-executor/14 text-executor'"
            >{{ tr(STATUS_TEXT[s.status]) }}</span></span>
            <h2 class="m-0 max-w-[24ch] text-[46px] leading-[1.1] tracking-[-0.01em] max-[760px]:text-[34px]">{{ tr(s.title) }}</h2>
          </header>
          <p v-for="(p, i) in s.body" :key="i" class="mx-0 mb-[16px] mt-0 max-w-[640px] text-[19px] text-ink-2 leading-[1.7] last-of-type:mb-[44px]">
            <template v-for="(part, j) in inline(tr(p))" :key="j">
              <code v-if="part.code" class="rounded-[6px] bg-control px-[6px] py-[1px] text-[0.88em]">{{ part.text }}</code><template v-else>{{ part.text }}</template>
            </template>
          </p>

          <Bento v-if="s.id === 'why'" />
          <HarnessLoop v-else-if="s.id === 'harness'" />
          <template v-else-if="s.id === 'separation'">
            <h3 class="mx-0 mb-[18px] mt-[16px] text-[24px] font-display font-400">{{ tr({ en: 'How this differs from the usual agent sandbox', zh: '和常见的 agent 沙箱有什么不同' }) }}</h3>
            <div class="grid grid-cols-[200px_minmax(0,1fr)_minmax(0,1fr)] max-w-[920px] items-baseline gap-x-[28px] gap-y-[14px] max-[760px]:(grid-cols-[minmax(0,1fr)] gap-[4px])">
              <span class="text-[13px] text-muted max-[760px]:hidden" />
              <span class="text-[13px] text-muted max-[760px]:hidden">{{ tr({ en: 'The usual sandbox', zh: '常见的沙箱' }) }}</span>
              <span class="text-[13px] text-actor max-[760px]:hidden">Needy</span>
              <template v-for="row in COMPARISON" :key="row.aspect.en">
                <span class="text-[15px] text-ink max-[760px]:(mt-[16px] text-[17px] font-display)">{{ tr(row.aspect) }}</span>
                <span
                  class="text-[15px] text-muted max-[760px]:(grid grid-cols-[3.5em_minmax(0,1fr)] gap-[8px]) max-[760px]:before:(pt-[2px] text-[13px] text-muted content-[attr(data-label)])"
                  :data-label="tr({ en: 'Usual', zh: '常见' })"
                >{{ tr(row.usual) }}</span>
                <span
                  class="text-[15px] text-ink max-[760px]:(grid grid-cols-[3.5em_minmax(0,1fr)] gap-[8px]) max-[760px]:before:(pt-[2px] text-[13px] text-actor content-[attr(data-label)])"
                  data-label="Needy"
                >{{ tr(row.needy) }}</span>
              </template>
            </div>
            <h3 class="mx-0 mb-[18px] mt-[56px] text-[24px] font-display font-400">{{ tr({ en: 'What it does not do yet', zh: '目前的局限' }) }}</h3>
            <ul class="m-0 max-w-[760px] flex flex-col gap-[10px] py-0 pl-[20px] pr-0 text-[15px] text-ink-2 leading-[1.5]">
              <li v-for="l in LIMITS" :key="l.en">
                {{ tr(l) }}
              </li>
            </ul>
          </template>
          <template v-else-if="s.id === 'isolation'">
            <Isolation />
            <h3 class="mx-0 mb-[18px] mt-[56px] text-[24px] font-display font-400">{{ tr({ en: 'What each actor driver can do', zh: '每种 actor driver 能做什么' }) }}</h3>
            <DriverMatrix />
          </template>
          <Recipes v-else-if="s.id === 'identity'" />
          <Decisions v-else-if="s.id === 'decisions'" />
          <Files v-else-if="s.id === 'files'" />
          <Chaos v-else-if="s.id === 'chaos'" />
          <template v-else-if="s.id === 'config'">
            <Config />
            <a class="mt-[24px] inline-flex items-center gap-[8px] rounded-[12px] bg-control px-[16px] py-[10px] text-[15px] text-ink [text-decoration:none] hover:bg-control-2" :href="`${base}/studio`" @click="go('studio', $event)"><Icon name="mingcute:file-code-line" /> {{ tr({ en: 'Open the profile studio: paste a profile and see the run it sets up', zh: '打开 Profile 工作台：粘贴一份 profile，看它搭起怎样的运行' }) }} <Icon name="mingcute:right-line" /></a>
          </template>
          <Cluster v-else-if="s.id === 'cluster'" />
          <Future v-else-if="s.id === 'future'" />
          <Start v-else-if="s.id === 'start'" />
          </div>
        </section>
      </main>
    </div>

    <footer class="px-[16px] pb-[48px] pt-[32px] text-center text-[14px] text-muted">
      <a :href="repo">needy-dev/needy</a> · {{ tr({ en: 'The figures here are the README banners, rendered by', zh: '这里的动图就是 README 里的横幅，由此命令渲染：' }) }}
      <code class="rounded-[6px] bg-control px-[6px] py-[1px] text-[0.88em]">pnpm -F @needy/docs banners</code>
    </footer>
  </template>
</template>
