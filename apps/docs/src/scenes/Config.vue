<script setup lang="ts">
import type { Focus } from '../kit/focus'
import type { Step } from '../kit/player'
import type { Role } from '../kit/roles'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Logo from '../kit/Logo.vue'
import Profile from '../kit/Profile.vue'

import { focusOf } from '../kit/focus'
import { curve } from '../kit/geometry'
import { profileHeight, profileLineY } from '../kit/profile'
import { color } from '../kit/roles'

/**
 * The anatomy of a profile: one profile in the middle, made of the README's Configuration blocks, with a line from
 * each top-level section to the part of a run it controls. Each step lights one section. Every key is a field of
 * crates/config/src/profile (needy rejects unknown keys).
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 720

const PROFILE = `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-firecracker
  recipe: builtin:gce-ubuntu-22.04
  command: [opencode]
  workspace: { path: ~/work, from: "executor:build:~/work" }
  network:
    allow: [models.dev, "*.opencode.ai"]
    services:
      model: { url: "http://127.0.0.1:11434" }
executors:
  build: { driver: linux-container, key: "{run}", max_procs: 8 }
commands:
  - { match: "git push **", deny: "no pushes from the agent" }
  - match: "pytest **"
    when: { tty: false }
    to: build
    timeout: 10m
    env: { set: { CI: "1" } }
files:
  login:
    mode: copy-in
    from: "host:~/needy-state/opencode/"
    to: "actor:~/.local/share/opencode/"
run_files: [login]
chaos:
  seed: 1
  faults: { curl-down: { exec: "curl **", exit: 7 } }
trace: { sinks: [sqlite, jsonl] }`

const LINES = PROFILE.split('\n').length
const CODE = { x: 436, y: 18, w: 568 }
const CODE_H = profileHeight(LINES)

interface Section {
  id: string
  /** The profile's lines of the section, first and last. */
  from: number
  to: number
  side: 'left' | 'right'
  /** The card: its top and height. */
  y: number
  h: number
  role: Role
  title: string
}

const LEFT = { x: 36, w: 350 }
const RIGHT = { x: 1054, w: 350 }

const SECTIONS: Section[] = [
  { id: 'actor', from: 0, to: 5, side: 'left', y: 22, h: 150, role: 'actor', title: 'Actor' },
  { id: 'network', from: 6, to: 9, side: 'left', y: 190, h: 150, role: 'gateway', title: 'Network' },
  { id: 'executors', from: 10, to: 11, side: 'left', y: 358, h: 150, role: 'executor', title: 'Executors' },
  { id: 'commands', from: 12, to: 18, side: 'right', y: 22, h: 170, role: 'executor', title: 'Rules' },
  { id: 'files', from: 19, to: 24, side: 'right', y: 210, h: 140, role: 'host', title: 'Files' },
  { id: 'chaos', from: 25, to: 27, side: 'right', y: 368, h: 120, role: 'fault', title: 'Chaos' },
  { id: 'trace', from: 28, to: 28, side: 'right', y: 506, h: 110, role: 'trace', title: 'Trace' },
]

interface ConfigStep extends Step {
  section: string
}

const STEPS: ConfigStep[] = [
  { ms: 3000, section: 'actor', caption: 'Actor: the driver that isolates it, the recipe it looks like, the program it runs, and its workspace.' },
  { ms: 3000, section: 'network', caption: 'Network: the hosts the actor may reach. Everything else is blocked.' },
  { ms: 3000, section: 'executors', caption: 'Executors: where compute- and IO-heavy work runs. A key decides which commands share one.' },
  { ms: 3200, section: 'commands', caption: 'Commands: rules that send each command to an executor, deny it, or change how it runs.' },
  { ms: 3000, section: 'files', caption: 'Files: named mappings between the host, the actor and an executor; run_files picks them.' },
  { ms: 2800, section: 'chaos', caption: 'Chaos: faults on exec, files, the network and sensors, fired from a seed.' },
  { ms: 2800, section: 'trace', caption: 'Trace: where the events go, for needy report.' },
]

const sectionOf = (at: number) => SECTIONS.find(s => s.id === STEPS[at]!.section)!

function lit(at: number): number[] {
  const s = sectionOf(at)
  return Array.from({ length: s.to - s.from + 1 }, (_, i) => s.from + i)
}

const focus = (at: number, id: string): Focus => focusOf(id, [STEPS[at]!.section])

/** A leader from the middle of a section's lines to the edge of its card. */
function leader(s: Section): string {
  const y = (profileLineY(CODE.y, s.from) + profileLineY(CODE.y, s.to)) / 2
  const cardY = s.y + s.h / 2
  return s.side === 'left'
    ? curve(CODE.x, y, LEFT.x + LEFT.w, cardY)
    : curve(CODE.x + CODE.w, y, RIGHT.x, cardY)
}

/** The bracket beside a section's lines, on the side of its card. */
function bracket(s: Section): { x: number, y: number, h: number } {
  const top = profileLineY(CODE.y, s.from) - 9
  const bottom = profileLineY(CODE.y, s.to) + 9
  return { x: s.side === 'left' ? CODE.x - 8 : CODE.x + CODE.w + 4, y: top, h: bottom - top }
}

const ROLE = Object.fromEntries(SECTIONS.flatMap(s => Array.from({ length: s.to - s.from + 1 }, (_, i) => [s.from + i, s.role]))) as Record<number, Role>
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="The anatomy of a profile: actor, network, executors, commands, files, chaos and trace, each linked to the part of a run it controls.">
    <template #default="{ at }">
      <svg class="leaders" :width="W" :height="H" aria-hidden="true">
        <path
          v-for="s in SECTIONS" :key="s.id" :d="leader(s)" class="leader" :class="{ on: s.id === STEPS[at]!.section }"
          :style="{ '--role': color(s.role) }"
        />
      </svg>

      <div class="code" :style="{ left: `${CODE.x}px`, top: `${CODE.y}px`, width: `${CODE.w}px`, height: `${CODE_H}px` }">
        <Profile :code="PROFILE" :lit="lit(at)" :role="sectionOf(at).role" :roles="ROLE" title="~/.config/needy/profiles/agent.yaml" />
      </div>

      <span
        v-for="s in SECTIONS" :key="`b${s.id}`" class="bracket" :class="{ on: s.id === STEPS[at]!.section }"
        :style="{ 'left': `${bracket(s).x}px`, 'top': `${bracket(s).y}px`, 'height': `${bracket(s).h}px`, '--role': color(s.role, 'fill') }"
      />

      <div
        v-for="s in SECTIONS" :key="s.id" class="card f" :class="[focus(at, s.id), s.id]"
        :style="{ 'left': `${(s.side === 'left' ? LEFT : RIGHT).x}px`, 'top': `${s.y}px`, 'width': `${(s.side === 'left' ? LEFT : RIGHT).w}px`, 'height': `${s.h}px`, '--role': color(s.role), '--role-fill': color(s.role, 'fill') }"
      >
        <header>
          <span class="title display">{{ s.title }}</span>
          <code class="key">{{ s.id === 'network' ? 'actor.network' : s.id === 'commands' ? 'commands' : s.id }}</code>
        </header>

        <!-- Each card is a small picture of what its section controls, in the shapes of the other scenes. -->
        <div v-if="s.id === 'actor'" class="pic">
          <div class="capsule">
            <Logo name="firecracker" :size="20" />
            <span class="mono">linux-firecracker</span>
            <span class="pill">Compute Engine</span>
          </div>
          <p>Runs the harness <code>opencode</code>, near the model. The recipe, a second file, sets the machine it looks like.</p>
        </div>

        <div v-else-if="s.id === 'network'" class="pic">
          <div class="door-row mono">
            <span class="host ok"><Icon name="mingcute:check-line" />models.dev</span>
            <span class="host no"><Icon name="mingcute:forbid-circle-line" />example.com</span>
          </div>
          <p>The actor reaches only the hosts it allows. A service is a name the gateway forwards to a real address.</p>
        </div>

        <div v-else-if="s.id === 'executors'" class="pic">
          <div class="boxes">
            <span class="box"><Icon name="mingcute:cube-line" /><code>build</code></span>
          </div>
          <p>The pool for compute and IO, next to the workspace. One by default; <code>runner:</code> puts one on another machine.</p>
        </div>

        <div v-else-if="s.id === 'commands'" class="pic">
          <ol class="slots mono">
            <li class="deny">
              git push ** <span>deny</span>
            </li>
            <li>pytest ** <span>→ build</span></li>
            <li class="rest">
              anything else <span>→ actor</span>
            </li>
          </ol>
          <p>A rule can also set a timeout, the environment, the directory or extra files.</p>
        </div>

        <div v-else-if="s.id === 'files'" class="pic">
          <div class="modes mono">
            <span class="on">copy-in</span><span>mount</span><span>mirror-out</span>
          </div>
          <p>A login copied in from the host when the run starts.</p>
        </div>

        <div v-else-if="s.id === 'chaos'" class="pic">
          <p class="fault mono">
            <Icon name="mingcute:fire-line" /> curl → exit 7, without running
          </p>
          <p>The same seed fires the same faults on a replay.</p>
        </div>

        <div v-else class="pic">
          <p class="events mono">
            <span>exec.decide</span><span>net.request</span><span>fault.injected</span>
          </p>
        </div>
      </div>
    </template>
  </Frame>
</template>

<style scoped>
.leaders {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.leader {
  fill: none;
  stroke: var(--line);
  stroke-width: 1.5;
  stroke-dasharray: 1.5 6;
  stroke-linecap: round;
  transition: stroke 0.35s;
}

.leader.on {
  stroke: var(--role);
  stroke-dasharray: none;
  stroke-width: 2;
}

.code {
  position: absolute;
}

.bracket {
  position: absolute;
  width: 4px;
  border-radius: 2px;
  background: var(--control-3);
  transition: background 0.35s;
}

.bracket.on {
  background: var(--role);
}

.card {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 13px 16px;
  border-radius: 14px;
  background: var(--surface);
}

header {
  display: flex;
  align-items: center;
  gap: 10px;
}

.title {
  color: var(--role);
  font-size: 19px;
}

.key {
  margin-left: auto;
  color: var(--muted);
  font-size: 12.5px;
}

.pic {
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  justify-content: space-between;
  gap: 8px;
}

.pic p {
  margin: 0;
  color: var(--ink-2);
  font-size: 13.5px;
  line-height: 1.4;
}

.pic code {
  font-size: 12.5px;
}

.capsule {
  display: flex;
  white-space: nowrap;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--actor) 10%, transparent);
  font-size: 12.5px;
}

.capsule .pill {
  margin-left: auto;
  padding: 2px 9px;
  border-radius: 999px;
  background: var(--surface);
  color: var(--ink-2);
  font-size: 12px;
}

.door-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.host {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 10px;
  border-radius: 999px;
  font-size: 12.5px;
}

.host.ok {
  background: color-mix(in srgb, var(--executor) 14%, transparent);
  color: var(--executor);
}

.host.no {
  background: color-mix(in srgb, var(--bad) 12%, transparent);
  color: var(--bad);
}

.boxes {
  display: flex;
  gap: 10px;
}

.box {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 7px 12px;
  border-radius: 9px;
  background: color-mix(in srgb, var(--executor) 12%, transparent);
  color: var(--executor);
}

.slots {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
  font-size: 12.5px;
}

.slots li {
  display: flex;
  justify-content: space-between;
  padding: 4px 10px;
  border-radius: 7px;
  background: var(--control);
}

.slots li span {
  color: var(--executor);
}

.slots li.deny span {
  color: var(--bad);
}

.slots li.rest {
  color: var(--muted);
}

.slots li.rest span {
  color: var(--actor);
}

.modes {
  display: flex;
  gap: 6px;
  font-size: 12.5px;
}

.modes span {
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--control);
  color: var(--muted);
}

.modes span.on {
  background: color-mix(in srgb, var(--executor) 16%, transparent);
  color: var(--executor);
}

.fault {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--fault) !important;
  font-size: 12.5px !important;
}

.events {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  font-size: 12px !important;
}

.events span {
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--trace-softer);
  color: var(--trace);
}
</style>
