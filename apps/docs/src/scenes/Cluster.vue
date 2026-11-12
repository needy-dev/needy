<script setup lang="ts">
import type { Focus } from '../kit/focus'
import type { IconName } from '../icons'
import type { Packet } from '../kit/Packets.vue'
import type { Step } from '../kit/player'
import type { Line } from '../kit/terminal'

import Frame from '../kit/Frame.vue'
import Machine from '../kit/Machine.vue'
import Packets from '../kit/Packets.vue'
import Pod from '../kit/Pod.vue'
import Terminal from '../kit/Terminal.vue'
import Yaml from '../kit/Yaml.vue'

/**
 * One control plane, many machines. Runners join with a token and dial out: they keep a control stream open and send
 * heartbeats, and listen nowhere. A profile's `runner:` selectors place the actor and each executor when a run is
 * created, and a routed command reaches a runner over the connection it dialed.
 * Terms: docs/TERMS_AND_CONCEPTS.md (Runner, Capabilities, Runner selector); design:
 * docs/ai/references/architecture/2026-11-10-runner-and-run-design.md. The commands and the `runner ls` rows for kvm
 * and mac are the README's; the win row follows their format.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 720

const OPS = { x: 24, y: 24, w: 600, h: 214 }
const PLANE = { x: 652, y: 24, w: 340, h: 214 }
const PROFILE = { x: 1016, y: 24, w: 400 }

const RUNNER_Y = 312
const RUNNER_H = 290
interface Runner {
  id: 'kvm' | 'mac' | 'win'
  x: number
  w: number
  icon: IconName
  host: string
  capabilities: string
  labels: string[]
}
const RUNNERS: Runner[] = [
  { id: 'kvm', x: 24, w: 548, icon: 'mingcute:linux-line', host: 'linux/x86_64', capabilities: 'actor(linux-firecracker) executor(linux-container,native)', labels: ['kvm'] },
  { id: 'mac', x: 596, w: 400, icon: 'mingcute:apple-line', host: 'macos/aarch64', capabilities: 'actor(macos-vz) executor(native)', labels: ['os=darwin'] },
  { id: 'win', x: 1020, w: 396, icon: 'mingcute:windows-line', host: 'windows/x86_64', capabilities: 'executor(native)', labels: ['os=windows'] },
]

/** Where each runner's line meets the control plane, spread along its bottom edge. */
const PLANE_PORT: Record<Runner['id'], number> = { kvm: PLANE.x + 90, mac: PLANE.x + PLANE.w / 2, win: PLANE.x + PLANE.w - 90 }
const PLANE_BOTTOM = PLANE.y + PLANE.h

/** A runner's line: from the middle of its top edge up to the control plane. Runners dial out, so it points up. */
function dial(r: Runner): string {
  const x1 = r.x + r.w / 2
  const y1 = RUNNER_Y
  const x2 = PLANE_PORT[r.id]
  const bend = (y1 - PLANE_BOTTOM) / 2
  return `M${x1} ${y1} C${x1} ${y1 - bend} ${x2} ${PLANE_BOTTOM + bend} ${x2} ${PLANE_BOTTOM + 6}`
}

/** The reverse of `dial`: from the control plane down to a point on a runner. */
function down(id: Runner['id'], x: number, y: number): string {
  const x1 = PLANE_PORT[id]
  const bend = (y - PLANE_BOTTOM) / 2
  return `M${x1} ${PLANE_BOTTOM + 6} C${x1} ${PLANE_BOTTOM + bend} ${x} ${y - bend} ${x} ${y}`
}

// Where the run's instances land.
const ACTOR = { x: 44, y: 446, w: 250, h: 124 }
const BUILD = { x: 310, y: 446, w: 242 }
const MAC_EXEC = { x: 616, y: 446, w: 240 }

const PROFILE_YAML = `actor:
  driver: linux-firecracker
  runner: kvm
executors:
  build: { driver: linux-container, runner: kvm }
  mac: { driver: native, runner: mac }`

interface ClusterStep extends Step {
  lines: Line[]
  clear?: boolean
  joined: boolean
  beats: boolean
  scheduled: boolean
  routed?: boolean
  lit: number[]
  focus: Array<Runner['id'] | 'plane'>
}

const STEPS: ClusterStep[] = [
  {
    ms: 3200,
    lines: [{ text: 'needy daemon --mode control --listen 127.0.0.1:7400 --token-env NEEDY_CONTROL_TOKEN &', kind: 'cmd' }],
    joined: false,
    beats: false,
    scheduled: false,
    lit: [],
    focus: ['plane'],
    caption: 'One control plane: the daemon that schedules runs, routes their work and keeps the trace.',
  },
  {
    ms: 3800,
    lines: [
      { text: 'needy runner token create --ttl 1h --roles executor', kind: 'cmd' },
      { text: '# on each machine', kind: 'dim' },
      { text: 'JOIN=... needy daemon --mode runner --join http://127.0.0.1:7400 --join-token-env JOIN', kind: 'cmd' },
    ],
    joined: true,
    beats: false,
    scheduled: false,
    lit: [],
    focus: ['kvm', 'mac', 'win'],
    caption: 'Each machine joins as a runner with a token. It dials out and keeps that connection, so it needs no open port.',
  },
  {
    ms: 3400,
    lines: [],
    joined: true,
    beats: true,
    scheduled: false,
    lit: [],
    focus: ['kvm', 'mac', 'win'],
    caption: 'Every heartbeat reports what the runner can host. Three missed intervals and it shows offline.',
  },
  {
    ms: 4000,
    clear: true,
    lines: [
      { text: 'needy runner ls', kind: 'cmd' },
      { text: 'NAME  STATUS  CAPABILITIES                                              HOST', kind: 'dim' },
      { text: 'kvm   online  actor(linux-firecracker) executor(linux-container,native) linux/x86_64' },
      { text: 'mac   online  actor(macos-vz) executor(native)                          macos/aarch64' },
      { text: 'win   online  executor(native)                                          windows/x86_64' },
    ],
    joined: true,
    beats: true,
    scheduled: false,
    lit: [],
    focus: ['kvm', 'mac', 'win'],
    caption: 'needy runner ls lists each machine, what it can host, and its platform.',
  },
  {
    ms: 4200,
    lines: [{ text: 'needy run create --profile cluster', kind: 'cmd' }],
    joined: true,
    beats: true,
    scheduled: true,
    lit: [2, 4, 5],
    focus: ['kvm', 'mac'],
    caption: 'A profile names runners by name or label. The run\'s actor and executors land on the runners it names.',
  },
  {
    ms: 4400,
    lines: [],
    joined: true,
    beats: true,
    scheduled: true,
    routed: true,
    lit: [5],
    focus: ['kvm', 'mac'],
    caption: 'xcodebuild from the actor on kvm runs on the Mac: the control plane sends it down the connection mac dialed.',
  },
]

function screenAt(at: number): { lines: Line[], fresh: number } {
  let start = at
  while (start > 0 && !STEPS[start]!.clear)
    start--
  const before = STEPS.slice(start, at).flatMap(step => step.lines)
  return { lines: [...before, ...STEPS[at]!.lines], fresh: before.length }
}

function runnerFocus(at: number, id: Runner['id']): Focus {
  const step = STEPS[at]!
  if (!step.joined)
    return 'dim'
  return step.focus.includes(id) ? 'focus' : 'dim'
}

function packets(at: number): Packet[] {
  const step = STEPS[at]!
  const list: Packet[] = []
  if (at === 1) {
    RUNNERS.forEach((r, i) => list.push({ id: `join-${r.id}`, d: dial(r), role: 'host', text: 'join', delay: 1400 + i * 260, ms: 1100 }))
  }
  else if (step.beats && !step.scheduled) {
    RUNNERS.forEach((r, i) => {
      list.push({ id: `beat-${at}-${r.id}-a`, d: dial(r), role: 'trace', delay: 200 + i * 300, ms: 900 })
      list.push({ id: `beat-${at}-${r.id}-b`, d: dial(r), role: 'trace', delay: 1700 + i * 300, ms: 900 })
    })
  }
  if (at === 4) {
    list.push({ id: 'drop-actor', d: down('kvm', ACTOR.x + ACTOR.w / 2, ACTOR.y), role: 'actor', text: 'actor', delay: 300, ms: 1000 })
    list.push({ id: 'drop-build', d: down('kvm', BUILD.x + BUILD.w / 2, BUILD.y), role: 'executor', text: 'build', delay: 450, ms: 1000 })
    list.push({ id: 'drop-mac', d: down('mac', MAC_EXEC.x + MAC_EXEC.w / 2, MAC_EXEC.y), role: 'executor', text: 'mac', delay: 600, ms: 1000 })
  }
  if (step.routed) {
    const up = `M${ACTOR.x + ACTOR.w / 2} ${ACTOR.y} C${ACTOR.x + ACTOR.w / 2} ${ACTOR.y - 120} ${PLANE_PORT.kvm} ${PLANE_BOTTOM + 90} ${PLANE_PORT.kvm} ${PLANE_BOTTOM + 6}`
    const toMac = down('mac', MAC_EXEC.x + MAC_EXEC.w / 2, MAC_EXEC.y)
    list.push({ id: 'cmd-up', d: up, role: 'actor', text: 'xcodebuild', delay: 200, ms: 1000 })
    list.push({ id: 'cmd-down', d: toMac, role: 'executor', text: 'xcodebuild', delay: 1250, ms: 900 })
    list.push({ id: 'cmd-back', d: toMac, role: 'executor', text: 'exit 0', delay: 2400, ms: 800, back: true })
  }
  return list
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Cluster: Linux, Mac and Windows machines join one control plane as runners. They dial out and send heartbeats; a profile's runner selectors place the actor on kvm and executors on kvm and mac; a command from the actor runs on the Mac over the connection the Mac dialed.">
    <template #default="{ at }">
      <svg class="lines" :width="W" :height="H" aria-hidden="true">
        <defs>
          <marker id="cluster-arrow" viewBox="0 0 10 10" refX="5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0 1 L9 5 L0 9 z" class="head" />
          </marker>
        </defs>
        <path
          v-for="r in RUNNERS" :key="r.id" :d="dial(r)" class="dial"
          :class="{ up: STEPS[at]!.joined, dim: STEPS[at]!.joined && !STEPS[at]!.focus.includes(r.id) }"
          marker-end="url(#cluster-arrow)"
        />
      </svg>

      <div class="ops">
        <Terminal v-bind="screenAt(at)" title="Operator · control host" :rows="8" :play="at" />
      </div>

      <Machine v-bind="PLANE" role="host" icon="mingcute:server-2-line" title="Control Plane" sub="needy daemon --mode control" :focus="STEPS[at]!.focus.includes('plane') ? 'focus' : 'idle'">
        <ul class="parts">
          <li><b>Control daemon</b><span>schedules runs, routes their work</span></li>
          <li><b>Runner registry</b><span>names, roles, labels, heartbeats</span></li>
          <li><b>Trace</b><code>trace.db</code><code>state.db</code></li>
        </ul>
      </Machine>

      <div class="profile" :class="{ shown: STEPS[at]!.scheduled }">
        <Yaml :code="PROFILE_YAML" :lit="STEPS[at]!.lit" role="actor" title="profiles/cluster.yaml" />
      </div>

      <Machine
        v-for="r in RUNNERS" :key="r.id" :x="r.x" :y="RUNNER_Y" :w="r.w" :h="RUNNER_H" role="host"
        title="Runner" :name="r.id" :sub="r.host" :icon="r.icon" :focus="runnerFocus(at, r.id)"
      >
        <div class="meta">
          <span class="status" :class="{ online: STEPS[at]!.joined }"><i />{{ STEPS[at]!.joined ? 'online' : 'not joined' }}</span>
          <span v-for="label in r.labels" :key="label" class="label mono">{{ label }}</span>
        </div>
        <code class="caps">{{ r.capabilities }}</code>
        <p v-if="r.id === 'win'" class="empty" :class="{ shown: STEPS[at]!.scheduled }">
          This run places nothing here.
        </p>
      </Machine>

      <div class="land" :class="{ landed: STEPS[at]!.scheduled }">
        <Pod v-bind="ACTOR" driver="linux-firecracker" badge="" :focus="STEPS[at]!.routed ? 'focus' : 'idle'">
          <code class="inside">opencode</code>
        </Pod>
        <Machine v-bind="BUILD" title="Executor" name="build" icon="mingcute:cube-line" sub="linux-container" />
        <Machine v-bind="MAC_EXEC" title="Executor" name="mac" icon="mingcute:laptop-line" sub="native" :focus="STEPS[at]!.routed ? 'focus' : 'idle'" />
      </div>

      <Packets :packets="packets(at)" />
    </template>
  </Frame>
</template>

<style scoped>
.lines {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.dial {
  fill: none;
  stroke: var(--line);
  stroke-width: 1.6;
  stroke-dasharray: 1.5 6;
  stroke-linecap: round;
  transition:
    stroke 0.4s,
    opacity 0.4s;
}

.dial.up {
  stroke: color-mix(in srgb, var(--ink) 40%, transparent);
  stroke-dasharray: none;
}

.dial.dim {
  opacity: 0.35;
}

.head {
  fill: color-mix(in srgb, var(--ink) 40%, transparent);
}

.ops {
  position: absolute;
  top: 24px;
  left: 24px;
  width: 600px;
  height: 214px;
}

/* A narrower type, so `needy runner ls` keeps its columns. */
.ops :deep(.terminal) {
  font-size: 11px;
}

.ops :deep(.line) {
  min-height: 17.6px;
}

.parts {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 6px 0 0;
  padding: 0;
  list-style: none;
}

.parts li {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 8px;
  padding: 7px 10px;
  border-radius: 9px;
  background: var(--control);
  font-size: 13.5px;
}

.parts b {
  font-weight: 500;
}

.parts span {
  color: var(--muted);
  font-size: 12.5px;
}

.parts code {
  padding: 0 6px;
  border-radius: 5px;
  background: var(--trace-softer);
  color: var(--trace);
  font-size: 12px;
}

.profile {
  position: absolute;
  top: 24px;
  left: 1016px;
  width: 400px;
  opacity: 0.35;
  transition: opacity 0.4s;
}

.profile.shown {
  opacity: 1;
}

.profile :deep(.yaml) {
  font-size: 12.5px;
}

.meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 4px;
}

.status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--control);
  color: var(--muted);
  font-size: 12.5px;
}

.status i {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--faint);
}

.status.online {
  background: var(--executor-softer);
  color: var(--executor);
}

.status.online i {
  background: var(--executor-fill);
  animation: beat 1.6s ease-in-out infinite;
}

@keyframes beat {
  50% {
    opacity: 0.3;
  }
}

.label {
  padding: 3px 9px;
  border-radius: 999px;
  background: var(--control);
  color: var(--ink-2);
  font-size: 12px;
}

.caps {
  color: var(--muted);
  font-size: 12px;
  line-height: 1.5;
}

.empty {
  margin: 34px 0 0;
  color: var(--muted);
  font-size: 14px;
  text-align: center;
  opacity: 0;
  transition: opacity 0.4s 0.6s;
}

.empty.shown {
  opacity: 1;
}

/* The run's instances drop onto their runners when it is created. */
.land {
  position: absolute;
  inset: 0;
  opacity: 0;
  pointer-events: none;
  transform: translateY(-36px);
  transition:
    opacity 0.5s 0.9s,
    transform 0.6s 0.9s cubic-bezier(0.2, 0, 0, 1);
}

.land.landed {
  opacity: 1;
  transform: none;
}

.inside {
  color: var(--actor);
  font-size: 13px;
}
</style>
