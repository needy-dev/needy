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
onMounted(() => window.addEventListener('popstate', onPop))
onBeforeUnmount(() => window.removeEventListener('popstate', onPop))

const ORDER = ['start', 'why', 'harness', 'separation', 'isolation', 'identity', 'decisions', 'files', 'chaos', 'config', 'cluster', 'future'] as const
const NAV: Section[] = ORDER.map(id => SECTIONS[id]!)


</script>

<template>
  <component :is="captured" v-if="captured" capture />
  <template v-else>
    <header class="bar">
      <a class="mark display" :href="`${base}/`" @click="go('guide', $event)">Needy</a>
      <nav class="pages">
        <a :href="`${base}/`" :class="{ on: page === 'guide' }" @click="go('guide', $event)">{{ tr({ en: 'Guide', zh: '指南' }) }}</a>
        <a :href="`${base}/studio`" :class="{ on: page === 'studio' }" @click="go('studio', $event)">{{ tr({ en: 'Profile Studio', zh: 'Profile 工作台' }) }}</a>
      </nav>
      <span class="spacer" />
      <ToggleGroupRoot v-model="locale" type="single" class="langs" :aria-label="tr({ en: 'Language', zh: '语言' })">
        <ToggleGroupItem value="en" class="lang">
          EN
        </ToggleGroupItem>
        <ToggleGroupItem value="zh-Hans" class="lang">
          中文
        </ToggleGroupItem>
      </ToggleGroupRoot>
      <button type="button" class="icon-button" :aria-label="tr({ en: 'Switch theme', zh: '切换主题' })" @click="toggleTheme">
        <Icon :name="theme === 'dark' ? 'mingcute:sun-line' : 'mingcute:moon-line'" :size="18" />
      </button>
      <a class="icon-button github" :href="repo" aria-label="GitHub"><Icon name="mingcute:github-line" :size="18" /></a>
    </header>

    <main v-if="page === 'studio'" class="studio-page">
      <h2>{{ tr({ en: 'Profile Studio', zh: 'Profile 工作台' }) }}</h2>
      <p class="prose">
        {{ tr({ en: 'Paste a profile or start from an example. Pick values from the ▾ menus, add parts, and try where a command goes.', zh: '粘贴一份 profile，或从示例开始。从 ▾ 菜单里选值、添加组成部分，试试一条命令会去哪里。' }) }}
      </p>
      <Studio />
      <h2 class="second">
        {{ tr({ en: 'Rule playground', zh: '规则试验场' }) }}
      </h2>
      <Playground />
    </main>

    <div v-if="page === 'guide'" class="hero">
      <Hero />
    </div>

    <div v-if="page === 'guide'" class="book">
      <main class="chapters">
        <section v-for="(s, n) in NAV" :id="s.id" :key="s.id" class="chapter" :class="{ band: n % 2 === 1 }">
          <div class="inner">
          <header class="chapter-head">
            <span class="eyebrow"><span class="num mono">{{ String(n + 1).padStart(2, '0') }}</span>{{ tr(s.nav) }}<span v-if="s.status" class="status" :class="s.status">{{ tr(STATUS_TEXT[s.status]) }}</span></span>
            <h2>{{ tr(s.title) }}</h2>
          </header>
          <p v-for="(p, i) in s.body" :key="i" class="prose">
            <template v-for="(part, j) in inline(tr(p))" :key="j">
              <code v-if="part.code">{{ part.text }}</code><template v-else>{{ part.text }}</template>
            </template>
          </p>

          <Bento v-if="s.id === 'why'" />
          <HarnessLoop v-else-if="s.id === 'harness'" />
          <template v-else-if="s.id === 'separation'">
            <h3 class="first">{{ tr({ en: 'How this differs from the usual agent sandbox', zh: '和常见的 agent 沙箱有什么不同' }) }}</h3>
            <div class="compare">
              <span class="compare-head" />
              <span class="compare-head">{{ tr({ en: 'The usual sandbox', zh: '常见的沙箱' }) }}</span>
              <span class="compare-head needy">Needy</span>
              <template v-for="row in COMPARISON" :key="row.aspect.en">
                <span class="aspect">{{ tr(row.aspect) }}</span>
                <span class="usual" :data-label="tr({ en: 'Usual', zh: '常见' })">{{ tr(row.usual) }}</span>
                <span class="ours" data-label="Needy">{{ tr(row.needy) }}</span>
              </template>
            </div>
            <h3>{{ tr({ en: 'What it does not do yet', zh: '目前的局限' }) }}</h3>
            <ul class="limits">
              <li v-for="l in LIMITS" :key="l.en">
                {{ tr(l) }}
              </li>
            </ul>
          </template>
          <template v-else-if="s.id === 'isolation'">
            <Isolation />
            <h3>{{ tr({ en: 'What each actor driver can do', zh: '每种 actor driver 能做什么' }) }}</h3>
            <DriverMatrix />
          </template>
          <Recipes v-else-if="s.id === 'identity'" />
          <Decisions v-else-if="s.id === 'decisions'" />
          <Files v-else-if="s.id === 'files'" />
          <Chaos v-else-if="s.id === 'chaos'" />
          <template v-else-if="s.id === 'config'">
            <Config />
            <a class="to-studio" :href="`${base}/studio`" @click="go('studio', $event)"><Icon name="mingcute:file-code-line" /> {{ tr({ en: 'Open the profile studio: paste a profile and see the run it sets up', zh: '打开 Profile 工作台：粘贴一份 profile，看它搭起怎样的运行' }) }} <Icon name="mingcute:right-line" /></a>
          </template>
          <Cluster v-else-if="s.id === 'cluster'" />
          <Future v-else-if="s.id === 'future'" />
          <Start v-else-if="s.id === 'start'" />
          </div>
        </section>
      </main>
    </div>

    <footer class="footer">
      <a :href="repo">needy-dev/needy</a> · {{ tr({ en: 'The figures here are the README banners, rendered by', zh: '这里的动图就是 README 里的横幅，由此命令渲染：' }) }}
      <code>pnpm -F @needy/docs banners</code>
    </footer>
  </template>
</template>

<style scoped>
.bar {
  position: sticky;
  top: 0;
  z-index: 20;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 20px;
  background: color-mix(in srgb, var(--paper) 86%, transparent);
  backdrop-filter: blur(12px);
}

.mark {
  color: var(--ink);
  font-size: 22px;
  text-decoration: none;
}

.spacer {
  flex: 1;
}

.langs {
  display: flex;
  padding: 3px;
  border-radius: 12px;
  background: var(--control);
}

.lang {
  padding: 5px 10px;
  border: 0;
  border-radius: 9px;
  background: transparent;
  color: var(--ink-2);
  cursor: pointer;
  font-size: 13px;
}

.lang[data-state='on'] {
  background: var(--surface);
  color: var(--ink);
}

.icon-button {
  display: grid;
  width: 36px;
  height: 36px;
  place-items: center;
  border: 0;
  border-radius: 12px;
  background: var(--control);
  color: var(--ink);
  cursor: pointer;
}

.pages {
  display: flex;
  gap: 4px;
  margin-left: 16px;
}

.pages a {
  white-space: nowrap;
  padding: 6px 12px;
  border-radius: 10px;
  color: var(--ink-2);
  font-size: 14.5px;
  text-decoration: none;
}

.pages a.on {
  background: var(--control);
  color: var(--ink);
}

.studio-page {
  padding: 24px 32px 64px;
}

.studio-page .second {
  margin-top: 72px;
}

.to-studio {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  margin-top: 24px;
  padding: 10px 16px;
  border-radius: 12px;
  background: var(--control);
  color: var(--ink);
  font-size: 15px;
  text-decoration: none;
}

.to-studio:hover {
  background: var(--control-2);
}

.hero {
  max-width: 1440px;
  margin: 0 auto;
  padding: 8px 20px 24px;
}

.book {
  padding-bottom: 32px;
}

@media (max-width: 600px) {
  .bar {
    gap: 6px;
    padding: 10px 12px;
  }

  .mark {
    display: none;
  }

  .pages {
    margin-left: 0;
  }

  .pages a {
    padding: 6px 10px;
    font-size: 14px;
  }

  .icon-button.github {
    display: none;
  }

  .hero {
    padding: 0 16px;
  }
}

@media (max-width: 760px) {
  .studio-page {
    padding-right: 16px;
    padding-left: 16px;
  }
}

.chapters {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

/* Each chapter runs the full width; every other one sits on a band of the surface color, so the page reads in
   sections without lines between them. */
.chapter {
  padding: 104px 32px;
  scroll-margin-top: 56px;
}

.chapter.band {
  /* On the band, cards take the page's color, so they still stand apart from what they sit on. */
  --surface: var(--paper);
  background: var(--band);
}

.inner {
  max-width: 1200px;
  margin: 0 auto;
}

@media (max-width: 760px) {
  .chapter {
    padding: 72px 16px;
  }
}

.chapter-head {
  display: flex;
  flex-direction: column;
  gap: 14px;
  margin-bottom: 20px;
}

.eyebrow {
  display: flex;
  align-items: center;
  gap: 10px;
  color: var(--actor);
  font-size: 14.5px;
  letter-spacing: 0.01em;
}

.eyebrow .num {
  color: var(--muted);
  font-size: 13px;
}

.eyebrow .status {
  margin-left: 4px;
}

h2 {
  max-width: 24ch;
  margin: 0;
  font-size: 46px;
  line-height: 1.1;
  letter-spacing: -0.01em;
}

@media (max-width: 760px) {
  h2 {
    font-size: 34px;
  }
}

h3 {
  margin: 56px 0 18px;
  font-family: var(--font-display);
  font-size: 24px;
  font-weight: 400;
}

h3.first {
  margin-top: 16px;
}

.prose {
  max-width: 640px;
  margin: 0 0 16px;
  color: var(--ink-2);
  font-size: 19px;
  line-height: 1.7;
}

.prose:last-of-type {
  margin-bottom: 44px;
}

.status {
  padding: 3px 10px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--executor) 14%, transparent);
  color: var(--executor);
  font-size: 13px;
  white-space: nowrap;
}

.status.partly {
  background: color-mix(in srgb, var(--fault) 15%, transparent);
  color: var(--fault);
}

.status.vision {
  background: color-mix(in srgb, var(--gateway) 14%, transparent);
  color: var(--gateway);
}

.capabilities {
  display: grid;
  gap: 36px 32px;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
}

/* No card around each capability: space and alignment group them, and a hover gives a surface to the one under the pointer. */
.capability {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: -12px;
  padding: 12px;
  border-radius: 16px;
  color: var(--ink);
  text-decoration: none;
  transition: background 0.25s;
}

.capability:hover {
  background: var(--control);
}

.compare {
  display: grid;
  max-width: 920px;
  gap: 14px 28px;
  align-items: baseline;
  grid-template-columns: 200px minmax(0, 1fr) minmax(0, 1fr);
}

.compare-head {
  color: var(--muted);
  font-size: 13px;
}

.compare-head.needy {
  color: var(--actor);
}

.aspect {
  color: var(--ink);
  font-size: 15px;
}

.usual {
  color: var(--muted);
  font-size: 15px;
}

.ours {
  color: var(--ink);
  font-size: 15px;
}

@media (max-width: 760px) {
  .compare {
    grid-template-columns: minmax(0, 1fr);
    gap: 4px;
  }

  .compare-head {
    display: none;
  }

  .aspect {
    margin-top: 16px;
    color: var(--ink);
    font-family: var(--font-display);
    font-size: 17px;
  }

  .usual,
  .ours {
    display: grid;
    gap: 8px;
    grid-template-columns: 3.5em minmax(0, 1fr);
  }

  .usual::before,
  .ours::before {
    padding-top: 2px;
    color: var(--muted);
    content: attr(data-label);
    font-size: 13px;
  }

  .ours::before {
    color: var(--actor);
  }
}

.limits {
  display: flex;
  max-width: 760px;
  flex-direction: column;
  gap: 10px;
  margin: 0;
  padding: 0 0 0 20px;
  color: var(--ink-2);
  font-size: 15px;
  line-height: 1.5;
}

.tile {
  display: grid;
  width: 40px;
  height: 40px;
  place-items: center;
  border-radius: 10px;
  background: color-mix(in srgb, var(--actor) 13%, transparent);
  color: var(--actor);
}

.capability strong {
  font-size: 18px;
  font-weight: 400;
  line-height: 1.25;
}

.capability .text {
  flex: 1;
  color: var(--ink-2);
  font-size: 14.5px;
  line-height: 1.5;
}

.capability .status {
  align-self: flex-start;
}

code {
  padding: 1px 6px;
  border-radius: 6px;
  background: var(--control);
  font-size: 0.88em;
}

.footer {
  padding: 32px 16px 48px;
  color: var(--muted);
  font-size: 14px;
  text-align: center;
}
</style>
