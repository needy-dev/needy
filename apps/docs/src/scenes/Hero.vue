<script setup lang="ts">
import type { Step } from '../kit/player'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Logo from '../kit/Logo.vue'

/**
 * The first screen, framed as a product shot after the stageshow skill: the copy on the left; one window, the actor's
 * terminal, whole at a terminal's proportions; a corner chip on its top edge saying where the actor runs; one
 * satellite, the Mac executor, straddling the window's right edge over the terminal's empty margin. One turn of the harness: the model asks for a build, the actor catches `xcodebuild` and the Mac
 * runs it, the result comes back; then a push is denied. As in examples/macos, the actor runs Linux and the build goes
 * to a Mac. The poster is the frame where the result is back.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 576

interface HeroStep extends Step {
  /** Lines in the terminal so far. */
  lines: number
  /** What the Mac shows. */
  mac: 'idle' | 'running' | 'done'
  routed: boolean
  denied: boolean
}

const STEPS: HeroStep[] = [
  { ms: 2400, lines: 2, mac: 'idle', routed: false, denied: false },
  { ms: 2600, lines: 2, mac: 'running', routed: true, denied: false },
  { ms: 3200, lines: 3, mac: 'done', routed: true, denied: false },
  { ms: 3200, lines: 5, mac: 'done', routed: true, denied: true },
]
</script>

<template>
  <div class="wide">
  <Frame bare :width="W" :height="H" :steps="STEPS" :capture="capture" label="Needy: an isolated sandbox for agent harnesses. In the actor's terminal, the harness runs xcodebuild; needy routes it to a Mac executor, which builds and returns BUILD SUCCEEDED; a git push is denied by a rule.">
    <template #default="{ at }">
      <div class="intro">
        <h1 class="mark">
          Needy
        </h1>
        <p class="lead">
          An isolated sandbox for agent harnesses, with seamless exec routing and policy.
        </p>
      </div>

      <!-- The hero: the actor's terminal, whole. -->
      <div class="window">
        <div class="bar">
          <i /><i /><i />
          <span class="title">Actor <span class="driver mono">opencode</span></span>
        </div>
        <div class="term mono">
          <span class="line dim">opencode run "fix the build"</span>
          <span class="line dim">› the model asks to build the app</span>
          <span class="line cmd shown">
            <b>$</b> xcodebuild -scheme App
            <em class="tag to" :class="{ shown: STEPS[at]!.routed }">→ mac</em>
          </span>
          <span class="line ok" :class="{ shown: STEPS[at]!.lines >= 3 }">** BUILD SUCCEEDED **</span>
          <span class="line cmd" :class="{ shown: STEPS[at]!.lines >= 5 }">
            <b>$</b> git push origin main
            <em class="tag deny" :class="{ shown: STEPS[at]!.denied }">denied</em>
          </span>
          <span class="line err" :class="{ shown: STEPS[at]!.lines >= 5 }">needy: denied: no pushes from the agent</span>
          <span class="line filler"><i style="width: 46%" /></span>
          <span class="line filler"><i style="width: 31%" /></span>
        </div>
      </div>

      <!-- Where the actor runs: a chip on the window's top-right corner, half outside. -->
      <span class="where">
        <Logo name="firecracker" :size="16" /> Firecracker microVM
        <span class="dot" /> <Icon name="mingcute:linux-line" :size="15" /> Linux KVM host
      </span>

      <!-- The satellite: the Mac executor, half outside the window's right edge. -->
      <div class="mac" :class="STEPS[at]!.mac">
        <div class="mac-head">
          <span class="tile"><Icon name="mingcute:apple-line" :size="18" /></span>
          <span class="what">
            <strong class="display">mac</strong>
            <span class="sub">Executor · runner on a Mac</span>
          </span>
        </div>
        <div class="mac-body mono">
          <span class="state">{{ STEPS[at]!.mac === 'done' ? '** BUILD SUCCEEDED **' : STEPS[at]!.mac === 'running' ? 'xcodebuild -scheme App' : 'waiting for work' }}</span>
          <span class="progress"><i /></span>
        </div>
      </div>
    </template>
  </Frame>
  </div>

  <!-- The compact design, for a phone column: the same story as one still, stacked, at the poster's beat. -->
  <div v-if="!capture" class="compact">
    <h1 class="mark">
      Needy
    </h1>
    <p class="lead">
      An isolated sandbox for agent harnesses, with seamless exec routing and policy.
    </p>
    <span class="where c-where">
      <Logo name="firecracker" :size="16" /> Firecracker microVM <span class="dot" /> <Icon name="mingcute:linux-line" :size="15" /> Linux KVM host
    </span>
    <div class="c-window">
      <div class="bar">
        <i /><i /><i /><span class="title">Actor <span class="driver mono">opencode</span></span>
      </div>
      <div class="term mono">
        <span class="line dim">› the model asks to build the app</span>
        <span class="line shown"><b>$</b> xcodebuild -scheme App <em class="tag to shown">→ mac</em></span>
        <span class="line ok shown">** BUILD SUCCEEDED **</span>
      </div>
    </div>
    <div class="mac done c-mac">
      <div class="mac-head">
        <span class="tile"><Icon name="mingcute:apple-line" :size="18" /></span>
        <span class="what">
          <strong class="display">mac</strong>
          <span class="sub">Executor · runner on a Mac</span>
        </span>
      </div>
      <div class="mac-body mono">
        <span class="state">** BUILD SUCCEEDED **</span>
        <span class="progress"><i /></span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.intro {
  position: absolute;
  top: 132px;
  left: 56px;
  width: 340px;
}

.mark {
  margin: 0;
  font-size: 92px;
  line-height: 1;
  letter-spacing: -0.02em;
}

.lead {
  margin: 24px 0 0;
  color: var(--ink);
  font-size: 22px;
  line-height: 1.35;
}

/* The window: whole, at a terminal's proportions; the page is its ground, so no plate. */
.window {
  position: absolute;
  top: 84px;
  left: 500px;
  width: 720px;
  height: 432px;
  overflow: hidden;
  border-radius: 14px;
  background: var(--term);
  box-shadow:
    0 0 0 1px rgb(255 255 255 / 6%),
    0 24px 60px rgb(0 0 0 / 18%);
  color: var(--term-ink);
}

/* On a dark page depth comes from a lighter surface, so the window steps up from the page instead of sinking into it. */
:root[data-theme='dark'] .window {
  background: var(--term-raised);
}

.bar {
  display: flex;
  align-items: center;
  gap: 7px;
  height: 40px;
  padding: 0 16px;
  background: rgb(255 255 255 / 4%);
}

.bar i {
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: rgb(255 255 255 / 16%);
}

.title {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  margin-left: 12px;
  color: rgb(255 255 255 / 80%);
  font-family: var(--font-display);
  font-size: 14px;
}

.driver {
  color: rgb(255 255 255 / 42%);
  font-size: 12px;
}

/* The lines keep to the left; the satellite covers only the empty right of the terminal. */
.term {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 28px 28px 0;
  font-size: 16px;
  line-height: 1.5;
}

.line {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 24px;
  white-space: pre;
  opacity: 0;
  transform: translateY(4px);
  transition:
    opacity 0.5s cubic-bezier(0.25, 0.8, 0.25, 1),
    transform 0.5s cubic-bezier(0.25, 0.8, 0.25, 1);
}

.line.dim,
.line.shown {
  opacity: 1;
  transform: none;
}

.line.dim {
  color: rgb(255 255 255 / 45%);
}

.line b {
  color: var(--actor-term);
  font-weight: 400;
}

.line.ok {
  color: var(--executor-term);
}

.line.err {
  color: var(--bad-term);
}

.line.filler {
  opacity: 1;
  transform: none;
}

.line.filler i {
  display: block;
  height: 8px;
  border-radius: 4px;
  background: rgb(255 255 255 / 8%);
}

.tag {
  padding: 1px 10px;
  border-radius: 999px;
  color: #fff;
  font-size: 13px;
  font-style: normal;
  opacity: 0;
  transform: translateX(-6px);
  transition:
    opacity 0.35s cubic-bezier(0.25, 0.8, 0.25, 1),
    transform 0.35s cubic-bezier(0.25, 0.8, 0.25, 1);
}

.tag.shown {
  opacity: 1;
  transform: none;
}

.tag.to {
  background: var(--executor-fill);
}

.tag.deny {
  background: var(--bad-fill);
}

/* The chip: one fact about the whole window, on its top-right corner over the empty title bar, half outside. */
.where {
  position: absolute;
  top: 66px;
  right: 156px;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 8px 14px;
  border-radius: 999px;
  background: var(--surface);
  box-shadow:
    0 1px 2px rgb(0 0 0 / 8%),
    0 8px 22px rgb(0 0 0 / 14%);
  color: var(--ink);
  font-size: 14px;
  white-space: nowrap;
}

.where .dot {
  width: 4px;
  height: 4px;
  margin: 0 3px;
  border-radius: 50%;
  background: var(--faint);
}

/* The satellite: a light card on the dark window, straddling its right edge over the terminal's empty margin. */
.mac {
  position: absolute;
  top: 262px;
  left: 1090px;
  display: flex;
  width: 250px;
  flex-direction: column;
  gap: 14px;
  padding: 16px;
  border-radius: 16px;
  background: var(--surface);
  box-shadow:
    0 1px 2px rgb(0 0 0 / 8%),
    0 12px 32px rgb(0 0 0 / 18%);
  transition: transform 0.45s cubic-bezier(0.2, 0, 0, 1);
}

.mac.running,
.mac.done {
  transform: translateY(-4px);
}

.mac-head {
  display: flex;
  align-items: center;
  gap: 12px;
}

.tile {
  display: grid;
  width: 38px;
  height: 38px;
  flex: none;
  place-items: center;
  border-radius: 10px;
  background: color-mix(in srgb, var(--executor) 15%, transparent);
  color: var(--executor);
}

.what {
  display: flex;
  flex-direction: column;
}

strong {
  font-size: 18px;
  font-weight: 400;
}

.sub {
  color: var(--muted);
  font-size: 13px;
}

.mac-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.state {
  color: var(--muted);
  font-size: 13px;
  transition: color 0.3s;
}

.running .state {
  color: var(--ink);
}

.done .state {
  color: var(--executor);
}

.progress {
  overflow: hidden;
  height: 6px;
  border-radius: 3px;
  background: var(--control);
}

.progress i {
  display: block;
  width: 0;
  height: 100%;
  border-radius: 3px;
  background: var(--executor-fill);
}

.running .progress i {
  width: 82%;
  transition: width 2.2s cubic-bezier(0.45, 0, 0.1, 1);
}

.done .progress i {
  width: 100%;
  transition: width 0.4s cubic-bezier(0.25, 0.8, 0.25, 1);
}
</style>

<style scoped>
/* Wide: the stage. Narrow: the compact design instead, at the column's width with real-size type. */
.compact {
  display: none;
}

@media (max-width: 720px) {
  .wide {
    display: none;
  }

  .compact {
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 24px 0 8px;
  }

  .compact .mark {
    font-size: 64px;
  }

  .compact .lead {
    margin-top: 16px;
    font-size: 20px;
  }

  .compact .c-where {
    position: relative;
    top: auto;
    right: auto;
    z-index: 2;
    align-self: flex-start;
    margin: 32px 0 12px;
    font-size: 12.5px;
  }

  .c-window {
    overflow: hidden;
    border-radius: 14px;
    background: var(--term);
    color: var(--term-ink);
  }

  :root[data-theme='dark'] .c-window {
    background: var(--term-raised);
  }

  .compact .term {
    padding: 18px 16px 22px;
    font-size: 13.5px;
  }

  .compact .line {
    white-space: normal;
    flex-wrap: wrap;
  }

  .compact .c-mac {
    position: relative;
    top: auto;
    left: auto;
    width: auto;
    margin: -20px 12px 0 40px;
    transform: none;
  }
}
</style>
