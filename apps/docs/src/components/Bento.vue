<script setup lang="ts">
import type { Text } from '../i18n'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Logo from '../kit/Logo.vue'
import MachineDeck from './MachineDeck.vue'

/**
 * What Needy does, as one picture made of tiles, after the bento rules of the stageshow skill: one hero tile with the
 * only window, a stat, then components, an object, a collection and a diagram, each saying one thing once. Copy sits
 * above each picture in page type. A tile whose claim is not fully built says so; the rest are verified end to end in
 * the closed-loop results.
 */
defineEmits<{ go: [id: string] }>()

const EVIDENCE = 'https://github.com/needy-dev/needy/blob/4a996b82f3d26665f24d5838d4415d18c64f881a/docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md'

interface Tile {
  id: string
  more: string
  lead: Text
  rest: Text
  status?: 'partly' | 'vision'
}

const TILES: Record<string, Tile> = {
  hero: { id: 'hero', more: 'decisions', lead: { en: 'Every decision, on record.', zh: '每一次判定都有据可查。' }, rest: { en: 'Routed, denied and faulted commands are typed events; needy report sums up a run.', zh: '命令被转到哪、被拦了几次、出过什么故障，全都记成事件，needy report 一眼看完。' } },
  stat: { id: 'stat', more: 'isolation', lead: { en: 'On a Linux KVM host,', zh: '在 Linux KVM 主机上，' }, rest: { en: 'Needy boots a Firecracker actor sandbox in', zh: 'Needy 拉起一个 Firecracker actor 沙箱，只要' } },
  rules: { id: 'rules', more: 'decisions', lead: { en: 'Policy per command.', zh: '每条命令都能单独管。' }, rest: { en: 'Route it, keep it, or deny it.', zh: '转出去、留下来，还是直接拦掉。' } },
  identity: { id: 'identity', more: 'identity', lead: { en: 'Any machine you describe.', zh: '想装成什么机器都行。' }, rest: { en: 'A recipe sets the CPU, firmware and devices the program sees.', zh: 'CPU、固件、设备，程序看到的都由 recipe 说了算。' } },
  isolation: { id: 'isolation', more: 'isolation', lead: { en: 'Four ways to isolate.', zh: '四种隔离方式可选。' }, rest: { en: 'A microVM, a container, gVisor, a VM on a Mac.', zh: 'microVM、容器、gVisor，还有 Mac 上的虚拟机。' } },
  chaos: { id: 'chaos', more: 'chaos', lead: { en: 'Chaos engineering for your agent.', zh: '给 agent 来点混沌工程。' }, rest: { en: 'Seeded faults, the same on every replay.', zh: '故障按 seed 注入，重放多少遍都一模一样。' }, status: 'partly' },
  laptop: { id: 'laptop', more: 'cluster', lead: { en: 'A decoupled control plane.', zh: '控制面是单独的。' }, rest: { en: 'The control plane runs on its own; a laptop, a board or a CI job only needs the client to start and manage runs.', zh: '控制面单独部署，笔记本、开发板、CI 只要装个客户端就能用。' } },
  teleport: { id: 'teleport', more: 'future', lead: { en: 'Teleport workloads.', zh: '哪里有机器就用哪里。' }, rest: { en: 'A macOS pool or a render farm, on demand.', zh: '要 Mac 集群就拉 Mac，要渲染农场就拉渲染农场。' }, status: 'vision' },
}

const STATUS: Record<'partly' | 'vision', Text> = {
  partly: { en: 'Partly there', zh: '部分可用' },
  vision: { en: 'Vision', zh: '设想' },
}
</script>

<template>
  <div class="bento">
    <a v-for="t in TILES" :key="t.id" class="tile" :class="`t-${t.id}`" :href="`#${t.more}`">
      <p class="copy">
        <strong>{{ tr(t.lead) }}</strong> {{ tr(t.rest) }}
        <span v-if="t.status" class="status" :class="t.status">{{ tr(STATUS[t.status]) }}</span>
      </p>

      <!-- The hero: `needy report`, the one window of the grid, running off the right and the bottom. The format is
           crates/core/src/report/human.rs; the run ID and the counts are made up for the tile. -->
      <div v-if="t.id === 'hero'" class="picture window">
        <div class="bar">
          <i /><i /><i /><span class="title mono">needy report latest</span>
        </div>
        <div class="term mono">
          <span><span class="k">Run</span>3f9c0e2a71b4  (profile opencode)</span>
          <span><span class="k">State</span>finished, driving program exit 0</span>
          <span class="gap" />
          <span class="dim">Commands</span>
          <span><span class="k in">routed to build</span><span class="ok">14</span></span>
          <span><span class="k in">kept on actor</span>3</span>
          <span><span class="k in">denied</span><span class="err">1</span></span>
          <span class="gap" />
          <span class="dim">Findings</span>
          <span class="in">network denied: example.com (actor network)</span>
        </div>
      </div>

      <div v-else-if="t.id === 'stat'" class="picture number">
        0.17<span class="unit">s</span>
      </div>

      <div v-else-if="t.id === 'rules'" class="picture rules">
        <div class="rule">
          <code>python3 **</code><span class="pill go">build</span>
        </div>
        <div class="rule on">
          <code>git push **</code><span class="pill no">deny</span>
        </div>
        <div class="rule">
          <code>uname **</code><span class="pill stay">actor</span>
        </div>
      </div>

      <MachineDeck v-else-if="t.id === 'identity'" class="picture" />

      <div v-else-if="t.id === 'isolation'" class="picture marks">
        <span class="mark"><Logo name="firecracker" :size="40" /></span>
        <span class="mark"><Logo name="docker" :size="40" /></span>
        <span class="mark"><Logo name="gvisor" :size="40" /></span>
        <span class="mark"><Logo name="apple" :size="40" /></span>
      </div>

      <div v-else-if="t.id === 'chaos'" class="picture faults">
        <span class="fault mono">EIO</span>
        <span class="fault mono">exit 7</span>
        <span class="fault mono">101.0°C</span>
      </div>

      <div v-else-if="t.id === 'laptop'" class="picture devices">
        <span class="targets">
          <span class="node"><Icon name="mingcute:laptop-line" :size="16" /> laptop</span>
          <span class="node"><Icon name="mingcute:chip-line" :size="16" /> board</span>
          <span class="node"><Icon name="mingcute:terminal-box-line" :size="16" /> CI job</span>
        </span>
        <span class="links" aria-hidden="true" />
        <span class="node plane"><Icon name="mingcute:server-2-line" :size="16" /> Control plane</span>
      </div>

      <div v-else-if="t.id === 'teleport'" class="picture pool">
        <span class="node actor">Actor</span>
        <span class="links" aria-hidden="true" />
        <span class="targets">
          <span class="node"><Icon name="mingcute:apple-line" :size="16" /> macOS</span>
          <span class="node"><Icon name="mingcute:apple-line" :size="16" /> macOS</span>
          <span class="node"><Icon name="mingcute:monitor-line" :size="16" /> GPU</span>
        </span>
      </div>
    </a>
  </div>
  <p class="foot">
    {{ tr({ en: 'Unmarked tiles are verified end to end in the', zh: '没有标注的都已经端到端跑通过，详见' }) }}
    <a :href="EVIDENCE">{{ tr({ en: 'closed-loop results', zh: '闭环验证记录' }) }}</a>.
  </p>
</template>

<style scoped>
/* Hero and strip, then two rows of three: rounded tiles on a gap that equals their padding's half. */
.bento {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

@media (max-width: 860px) {
  .bento {
    grid-template-columns: minmax(0, 1fr);
  }
}

.tile {
  position: relative;
  display: flex;
  min-height: 300px;
  flex-direction: column;
  overflow: hidden;
  padding: 28px 28px 0;
  border-radius: 24px;
  background: var(--control);
  color: var(--ink);
  text-decoration: none;
}

.tile.t-hero {
  grid-column: span 2;
}

@media (max-width: 860px) {
  .tile.t-hero {
    grid-column: auto;
  }

  .tile {
    min-height: 0;
    padding: 22px 22px 0;
  }

  .picture {
    margin-top: 24px;
  }

  .copy {
    font-size: 17px;
  }

  .number {
    font-size: 72px;
  }
}

.copy {
  max-width: 34ch;
  margin: 0;
  color: var(--muted);
  font-size: 19px;
  line-height: 1.4;
}

.t-hero .copy {
  max-width: 48ch;
}

.copy strong {
  color: var(--ink);
  font-weight: 600;
}

.status {
  display: inline-block;
  margin-left: 6px;
  padding: 2px 9px;
  border-radius: 999px;
  font-size: 12.5px;
  vertical-align: 2px;
}

.status.partly {
  background: color-mix(in srgb, var(--fault) 16%, transparent);
  color: var(--fault);
}

.status.vision {
  background: color-mix(in srgb, var(--gateway) 15%, transparent);
  color: var(--gateway);
}

.picture {
  margin-top: auto;
}

/* The hero's window: inset on the copy's gutter, off the right edge and the bottom. */
.window {
  width: calc(100% + 60px);
  margin-top: 28px;
  border-radius: 14px 0 0;
  background: var(--term);
  color: var(--term-ink);
}

.bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 12px 14px;
}

.bar i {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: rgb(255 255 255 / 16%);
}

.bar .title {
  margin-left: 10px;
  color: rgb(255 255 255 / 45%);
  font-size: 12px;
}

.term {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 4px 18px 24px;
  font-size: 14px;
  line-height: 1.55;
}

.term b {
  color: var(--actor-term);
  font-weight: 400;
}

.term .dim {
  color: rgb(255 255 255 / 45%);
}

.term .ok {
  color: var(--executor-term);
}

.term .err {
  color: var(--bad-term);
}

.term .k {
  display: inline-block;
  width: 9ch;
  color: rgb(255 255 255 / 55%);
}

.term .k.in {
  width: 19ch;
  padding-left: 2ch;
}

.term .in {
  padding-left: 2ch;
}

.term .gap {
  height: 6px;
}

.term em {
  margin-left: 8px;
  padding: 1px 8px;
  border-radius: 999px;
  color: #fff;
  font-size: 12px;
  font-style: normal;
}

.term .to {
  background: var(--executor-fill);
}

.term .deny {
  background: var(--bad-fill);
}

/* The stat: the inverted tile, one number. */
.tile.t-stat {
  background: var(--ink);
}

.t-stat .copy {
  color: color-mix(in srgb, var(--paper) 62%, transparent);
}

.t-stat .copy strong {
  color: var(--paper);
}

.number {
  margin: auto 0 28px;
  color: var(--paper);
  font-family: var(--font-display);
  font-size: 96px;
  line-height: 1;
  white-space: nowrap;
}

.unit {
  margin-left: 6px;
  font-size: 48px;
}

/* One component: three rules, the middle one acted on. */
.rules {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: -12px;
  padding: 12px;
  border-radius: 16px 16px 0 0;
  background: var(--surface);
}

.rule {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 8px;
  font-size: 14px;
}

.rule.on {
  background: color-mix(in srgb, var(--bad) 10%, var(--surface));
}

.pill {
  padding: 2px 10px;
  border-radius: 999px;
  font-family: var(--font-mono);
  font-size: 12.5px;
}

.pill.go {
  background: color-mix(in srgb, var(--executor) 15%, transparent);
  color: var(--executor);
}

.pill.no {
  background: var(--bad-fill);
  color: #fff;
}

.pill.stay {
  background: color-mix(in srgb, var(--actor) 14%, transparent);
  color: var(--actor);
}

/* The object: a machine's spec card, rising from the bottom. */
.spec {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-bottom: -24px;
  padding: 20px 20px 40px;
  border-radius: 18px 18px 0 0;
  background: linear-gradient(140deg, var(--actor-fill), color-mix(in srgb, var(--actor-fill) 55%, #000));
  color: #fff;
}

.spec-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--font-display);
  font-size: 18px;
}

.spec-cpu {
  font-size: 13.5px;
  opacity: 0.9;
}

.spec-row {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  opacity: 0.7;
}

/* The collection: the marks, running off the right. */
.marks {
  display: flex;
  gap: 12px;
  width: calc(100% + 60px);
  margin-bottom: 28px;
}

.mark {
  display: grid;
  width: 76px;
  height: 76px;
  flex: none;
  place-items: center;
  border-radius: 18px;
  background: var(--surface);
}

/* A component: the faults, one row of chips. */
.faults {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 32px;
}

.fault {
  padding: 8px 14px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--fault) 16%, var(--surface));
  color: var(--fault);
  font-size: 15px;
}

/* A component: one command, from the laptop. */
.laptop {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 32px;
  padding: 16px 18px;
  border-radius: 14px;
  background: var(--term);
  color: var(--term-ink);
}

.laptop code {
  font-size: 12.5px;
  white-space: nowrap;
}

/* A diagram: small clients on the left, the control plane they talk to on the right. */
.devices {
  display: grid;
  align-items: center;
  margin-bottom: 28px;
  grid-template-columns: auto 40px auto;
}

.node.plane {
  justify-self: start;
  background: var(--ink);
  color: var(--paper);
}

/* A diagram: one actor, a pool of machines it could ask for. */
.pool {
  display: grid;
  align-items: center;
  margin-bottom: 28px;
  grid-template-columns: auto 40px auto;
}

.node {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  border-radius: 10px;
  background: var(--surface);
  font-size: 14px;
}

.node.actor {
  justify-self: start;
  background: color-mix(in srgb, var(--actor) 14%, var(--surface));
  color: var(--actor);
}

.links {
  height: 0;
  border-top: 1.5px dashed var(--line-strong);
}

.targets {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.foot {
  margin: 14px 0 0;
  color: var(--muted);
  font-size: 13.5px;
}
</style>
