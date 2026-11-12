<script setup lang="ts">
import type { IconName } from '../icons'
import type { Packet } from '../kit/Packets.vue'
import type { Step } from '../kit/player'
import type { Line } from '../kit/terminal'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Machine from '../kit/Machine.vue'
import Packets from '../kit/Packets.vue'
import Pod from '../kit/Pod.vue'
import Profile from '../kit/Profile.vue'
import Terminal from '../kit/Terminal.vue'

import { focusOf } from '../kit/focus'
import { curve } from '../kit/geometry'
import { profileHeight, profileLineY } from '../kit/profile'
import { timeline } from '../kit/terminal'

/**
 * The actor's network: it has none of its own, so a host outside `allow:` is blocked. Plain HTTP gets a 403 whose
 * body is `needy: <host> is not allowed` (crates/core-actor/src/sidecar/gateway/mod_test.rs); HTTPS is a CONNECT
 * tunnel that the gateway refuses with 403, which curl reports as exit 56 (the gateway check of the closed-loop
 * results). A service's host in the actor, `NAME.needy`, is allowed and forwarded to its URL.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 480

const YAML = `actor:
  network:
    allow: [models.dev, "*.opencode.ai"]
    services:
      model: { url: "http://127.0.0.1:11434" }`

const POD = { x: 36, y: 30, w: 470, h: 360 }
const CODE = { x: 570, y: 30, w: 420 }
const CODE_H = profileHeight(5)
const STATUS = { x: CODE.x, y: CODE.y + CODE_H + 18, w: CODE.w }
const HOST_X = 1060
const HOST_W = 340
const HOST_H = 96
const HOSTS = {
  dev: { y: 30, title: 'models.dev', sub: 'allowed', icon: 'mingcute:globe-lock-line', role: 'executor' },
  model: { y: 142, title: 'model.needy', sub: 'service → 127.0.0.1:11434', icon: 'mingcute:server-line', role: 'executor' },
  other: { y: 254, title: 'example.com', sub: 'not on the allowlist', icon: 'mingcute:globe-off-line', role: 'bad' },
} as const satisfies Record<string, { y: number, title: string, sub: string, icon: IconName, role: 'executor' | 'bad' }>
type HostId = keyof typeof HOSTS

interface NetStep extends Step {
  command: string
  answer: Line
  to: HostId
  blocked: boolean
  /** The profile line that decides. */
  line: number
  event: string
}

const STEPS: NetStep[] = [
  { ms: 3200, command: 'curl -sI https://models.dev', answer: { text: 'HTTP/1.1 200 Connection established', kind: 'ok' }, to: 'dev', blocked: false, line: 2, event: 'net.connect · models.dev', caption: 'A host on the allowlist passes. HTTPS is a tunnel: needy sees the host, not the content.' },
  { ms: 3200, command: 'curl -s http://example.com', answer: { text: 'needy: example.com is not allowed', kind: 'err' }, to: 'other', blocked: true, line: 2, event: 'net.deny · example.com', caption: 'Any other host is blocked with 403, and the report lists it.' },
  { ms: 3200, command: 'curl -sI https://example.com', answer: { text: 'curl: (56) CONNECT tunnel failed, response 403', kind: 'err' }, to: 'other', blocked: true, line: 2, event: 'net.deny · example.com', caption: 'HTTPS too: the tunnel is refused before it opens.' },
  { ms: 3400, command: 'curl -s http://model.needy/api/tags', answer: { text: '{"models":[{"name":"qwen3:14b",…}]}', kind: 'ok' }, to: 'model', blocked: false, line: 4, event: 'net.request · model.needy', caption: 'A service is a name the actor uses; the gateway forwards it to the real address, such as a local model.' },
]

function linesAt(at: number): { lines: Line[], fresh: number } {
  const lines: Line[] = STEPS.slice(0, at + 1).flatMap(step => [{ text: step.command, kind: 'cmd' as const }, step.answer])
  return { lines, fresh: at * 2 }
}

function sent(at: number): number {
  const { lines, fresh } = linesAt(at)
  return timeline(lines, fresh).at[fresh + 1] ?? 0
}

const OUT = { x: POD.x + POD.w, y: POD.y + POD.h / 2 }
const lineY = (line: number) => profileLineY(CODE.y, line)
const hostY = (id: HostId) => HOSTS[id].y + HOST_H / 2

function packets(at: number): Packet[] {
  const step = STEPS[at]!
  const start = Math.max(0, sent(at) - 300)
  const host = step.command.split('//')[1]!.split('/')[0]!
  const toPolicy = curve(OUT.x, OUT.y, CODE.x, lineY(step.line))
  if (step.blocked)
    return [{ id: `a${at}`, d: toPolicy, role: 'actor', text: host, delay: start, ms: 700 }, { id: `b${at}`, d: toPolicy, role: 'bad', text: '403', delay: start + 760, ms: 700, back: true }]
  return [
    { id: `a${at}`, d: toPolicy, role: 'actor', text: host, delay: start, ms: 650 },
    { id: `h${at}`, d: curve(CODE.x + CODE.w, lineY(step.line), HOST_X, hostY(step.to)), role: 'executor', text: host, delay: start + 700, ms: 650 },
  ]
}

function hostFocus(at: number, id: HostId) {
  const step = STEPS[at]!
  return focusOf(id, step.blocked ? [] : [step.to], step.blocked ? [step.to] : [])
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Network: the actor reaches only the hosts on its allowlist and its services; every other host is blocked with 403.">
    <template #default="{ at }">
      <svg class="wires" :width="W" :height="H" aria-hidden="true">
        <path :d="curve(OUT.x, OUT.y, CODE.x, lineY(2))" class="wire" />
        <path v-for="(_, id) in HOSTS" :key="id" :d="curve(CODE.x + CODE.w, lineY(2), HOST_X, hostY(id))" class="wire" :class="{ blocked: id === 'other' }" />
      </svg>

      <Pod v-bind="POD" logo="firecracker" driver="linux-firecracker" badge="No network of its own">
        <Terminal v-bind="linesAt(at)" :rows="10" :play="at" />
      </Pod>

      <div class="code" :style="{ left: `${CODE.x}px`, top: `${CODE.y}px`, width: `${CODE.w}px` }">
        <Profile :code="YAML" :lit="[STEPS[at]!.line]" :role="STEPS[at]!.blocked ? 'bad' : 'executor'" title="profile.yaml" />
      </div>

      <p :key="at" class="status mono" :style="{ 'left': `${STATUS.x}px`, 'top': `${STATUS.y}px`, 'width': `${STATUS.w}px`, '--role': STEPS[at]!.blocked ? 'var(--bad)' : 'var(--executor)', 'animationDelay': `${sent(at) + 200}ms` }">
        <Icon :name="STEPS[at]!.blocked ? 'mingcute:forbid-circle-line' : 'mingcute:check-line'" /> {{ STEPS[at]!.event }}
      </p>

      <Machine
        v-for="(h, id) in HOSTS" :key="id" :x="HOST_X" :y="h.y" :w="HOST_W" :h="HOST_H"
        :role="h.role" :title="h.title" :sub="h.sub" :icon="h.icon" :focus="hostFocus(at, id)"
      />

      <Packets :packets="packets(at)" />
    </template>
  </Frame>
</template>

<style scoped>
.wires {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.wire {
  fill: none;
  stroke: var(--line);
  stroke-width: 1.6;
  stroke-dasharray: 1.5 6;
  stroke-linecap: round;
}

.code {
  position: absolute;
}

.status {
  position: absolute;
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0;
  padding: 8px 14px;
  border-radius: 10px;
  background: color-mix(in srgb, var(--role) 12%, transparent);
  color: var(--role);
  font-size: 13.5px;
  animation: arrive 0.4s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes arrive {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
}
</style>
