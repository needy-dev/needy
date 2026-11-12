<script setup lang="ts">
import type { Packet } from '../kit/Packets.vue'
import type { Step } from '../kit/player'
import type { Line } from '../kit/terminal'
import type { Decision, Rule } from '../route/rules'

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
import { argvOf, decide } from '../route/rules'

/**
 * Rules, as a profile writes them, for the usual shape of a run: one actor next to the harness and one executor for
 * compute and IO. The actor catches each exec (`binfmt_needy` in a VM), the daemon matches it
 * against `commands:`, and it runs on the executor a rule names, is denied, or stays in the actor. The decisions come
 * from src/route/rules.ts, the port of needy's rules.
 *
 * NOTICE: the port ignores `when:`, so the scene only sends commands that the `pytest **` rule does not match.
 * `npm install` prints npm's own summary, made up for the scene.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 480

const YAML = `executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "{python3,pip,npm,make} **", to: build }
  - { match: "git push **", deny: "no pushes from the agent" }
  - match: "pytest **"
    when: { tty: false }
    to: build
    timeout: 10m`

/** The rules of YAML as rules.ts reads them, and the profile lines of each. */
const RULES: Rule[] = [
  { match: '{python3,pip,npm,make} **', to: 'build' },
  { match: 'git push **', deny: 'no pushes from the agent' },
  { match: 'pytest **', to: 'build' },
]
const RULE_LINES: Array<[number, number]> = [[3, 3], [4, 4], [5, 8]]

const POD = { x: 36, y: 30, w: 410, h: 360 }
const CODE = { x: 488, y: 30, w: 536 }
const CODE_H = profileHeight(9)
const STATUS = { x: CODE.x, y: CODE.y + CODE_H + 18, w: CODE.w }
const MACHINE_X = 1064
const MACHINE_W = 340
const MACHINES = {
  build: { y: 30, title: 'Executor', sub: 'linux-container', icon: 'mingcute:cube-line' },
} as const
type MachineId = keyof typeof MACHINES
const MACHINE_H = 104
const LEGEND_Y = 30 + MACHINE_H + 20

/** The sections of a profile that hold rules, by the domain they decide. */
const LEGEND = [
  { key: 'commands:', what: 'to, deny, when, timeout, env' },
  { key: 'files:', what: 'copy-in, mount, mirror-out' },
  { key: 'actor.network:', what: 'allow, services' },
  { key: 'chaos:', what: 'faults, from a seed' },
  { key: 'presets: resolvers:', what: 'shared actions, launchers' },
]

interface RoutingStep extends Step {
  command: string
  output: Line
}

const STEPS: RoutingStep[] = [
  { ms: 3400, command: 'python3 -m pytest -q', output: { text: '4 passed in 0.31s', kind: 'ok' }, caption: 'binfmt_needy in the actor catches the exec. The heavy work goes to the executor build.' },
  { ms: 3400, command: 'npm install', output: { text: 'added 312 packages in 9s', kind: 'ok' }, caption: '{python3,pip,npm,make} picks between programs: installs and builds run on the executor too.' },
  { ms: 3400, command: '/usr/bin/git push origin main', output: { text: 'needy: denied: no pushes from the agent', kind: 'err' }, caption: 'A path matches by its basename. A rule that denies wins, and the command exits 126.' },
  { ms: 3600, command: 'git status -s', output: { text: ' M src/parse.py', kind: 'out' }, caption: 'No rule places git status, so it runs in the actor, next to the harness.' },
]

const decisions: Decision[] = STEPS.map(step => decide(RULES, argvOf(step.command)))

type Target = { kind: 'machine', id: MachineId, rule: number } | { kind: 'deny', rule: number } | { kind: 'actor' }

const TARGETS: Target[] = decisions.map((d) => {
  if (d.outcome.kind === 'deny')
    return { kind: 'deny', rule: d.outcome.rule }
  if (d.outcome.on === 'actor')
    return { kind: 'actor' }
  return { kind: 'machine', id: d.outcome.on as MachineId, rule: d.outcome.rule! }
})

function tagOf(t: Target): Line['tag'] {
  if (t.kind === 'deny')
    return { text: 'denied', role: 'bad' }
  if (t.kind === 'actor')
    return { text: 'actor', role: 'actor' }
  return { text: `→ ${t.id}`, role: 'executor' }
}

/** What the decision says, as `needy profile check` would name the target. */
function statusOf(t: Target): { text: string, role: 'executor' | 'bad' | 'actor' } {
  if (t.kind === 'deny')
    return { text: `rule ${t.rule} · deny · exit 126`, role: 'bad' }
  if (t.kind === 'actor')
    return { text: 'no rule places it · actor', role: 'actor' }
  return { text: `rule ${t.rule} · executor ${t.id}`, role: 'executor' }
}

function linesAt(at: number): { lines: Line[], fresh: number } {
  const lines: Line[] = STEPS.slice(0, at + 1).flatMap((step, i) => [{ text: step.command, kind: 'cmd' as const, tag: tagOf(TARGETS[i]!) }, step.output])
  return { lines, fresh: at * 2 }
}

/** When the command of step `at` has typed. */
function sent(at: number): number {
  const { lines, fresh } = linesAt(at)
  return timeline(lines, fresh).at[fresh + 1] ?? 0
}

/** The lines of the rule that decides step `at`. */
function litAt(at: number): number[] {
  const t = TARGETS[at]!
  if (t.kind === 'actor')
    return []
  const [from, to] = RULE_LINES[t.rule]!
  return Array.from({ length: to - from + 1 }, (_, i) => from + i)
}

const OUT = { x: POD.x + POD.w, y: POD.y + POD.h / 2 }
const ruleY = (rule: number) => profileLineY(CODE.y, RULE_LINES[rule]![0])
const machineY = (id: MachineId) => MACHINES[id].y + MACHINE_H / 2

function packets(at: number): Packet[] {
  const t = TARGETS[at]!
  const start = Math.max(0, sent(at) - 300)
  const name = argvOf(STEPS[at]!.command)[0]!.split('/').at(-1)!
  if (t.kind === 'actor') {
    const d = curve(OUT.x, OUT.y, STATUS.x, STATUS.y + 17)
    return [{ id: `a${at}`, d, role: 'actor', text: name, delay: start, ms: 700 }, { id: `a${at}b`, d, role: 'actor', text: 'runs here', delay: start + 760, ms: 700, back: true }]
  }
  const toRule = curve(OUT.x, OUT.y, CODE.x, ruleY(t.rule))
  if (t.kind === 'deny')
    return [{ id: `d${at}`, d: toRule, role: 'actor', text: name, delay: start, ms: 700 }, { id: `d${at}b`, d: toRule, role: 'bad', text: 'exit 126', delay: start + 760, ms: 700, back: true }]
  const toMachine = curve(CODE.x + CODE.w, ruleY(t.rule), MACHINE_X, machineY(t.id))
  return [
    { id: `r${at}`, d: toRule, role: 'actor', text: name, delay: start, ms: 650 },
    { id: `m${at}`, d: toMachine, role: 'executor', text: name, delay: start + 700, ms: 650 },
  ]
}

function machineFocus(at: number, id: MachineId) {
  const t = TARGETS[at]!
  return focusOf(id, t.kind === 'machine' ? [t.id] : [])
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Rules: the actor catches each exec, the profile's commands rules decide where it runs, and it runs on an executor, is denied, or stays in the actor.">
    <template #default="{ at }">
      <svg class="wires" :width="W" :height="H" aria-hidden="true">
        <path :d="curve(OUT.x, OUT.y, CODE.x, OUT.y)" class="wire" />
        <path v-for="(_, id) in MACHINES" :key="id" :d="curve(CODE.x + CODE.w, ruleY(0), MACHINE_X, machineY(id))" class="wire" />
      </svg>

      <Pod v-bind="POD" logo="firecracker" driver="linux-firecracker" :focus="TARGETS[at]!.kind === 'deny' ? 'bad' : TARGETS[at]!.kind === 'actor' ? 'focus' : 'idle'">
        <Terminal v-bind="linesAt(at)" :rows="10" :play="at" />
      </Pod>

      <div class="code" :style="{ left: `${CODE.x}px`, top: `${CODE.y}px`, width: `${CODE.w}px` }">
        <Profile :code="YAML" :lit="litAt(at)" :role="TARGETS[at]!.kind === 'deny' ? 'bad' : 'executor'" title="profile.yaml" />
      </div>

      <p :key="at" class="status mono" :style="{ 'left': `${STATUS.x}px`, 'top': `${STATUS.y}px`, 'width': `${STATUS.w}px`, '--role': `var(--${statusOf(TARGETS[at]!).role})`, 'animationDelay': `${sent(at) + 200}ms` }">
        <Icon name="mingcute:route-line" /> exec.decide · {{ statusOf(TARGETS[at]!).text }}
      </p>

      <Machine
        v-for="(m, id) in MACHINES" :key="id" :x="MACHINE_X" :y="m.y" :w="MACHINE_W" :h="MACHINE_H"
        :title="m.title" :name="id" :sub="m.sub" :icon="m.icon" badge="Stateful" :focus="machineFocus(at, id)"
      />

      <div class="legend" :style="{ left: `${MACHINE_X}px`, top: `${LEGEND_Y}px`, width: `${MACHINE_W}px` }">
        <span class="legend-title display">Kinds of rules</span>
        <dl>
          <template v-for="row in LEGEND" :key="row.key">
            <dt class="mono" :class="{ on: row.key === 'commands:' }">
              {{ row.key }}
            </dt>
            <dd>{{ row.what }}</dd>
          </template>
        </dl>
        <p class="files-kinds">
          A <b>profile</b> sets up one run. A <b>recipe</b> sets one machine's identity.
        </p>
      </div>

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

.label {
  fill: var(--muted);
  font-size: 12px;
}

.code {
  position: absolute;
}

.legend {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px;
  border-radius: 14px;
  background: color-mix(in srgb, var(--ink) 4%, transparent);
}

.legend-title {
  color: var(--ink-2);
  font-size: 15px;
}

dl {
  display: grid;
  gap: 5px 10px;
  margin: 0;
  grid-template-columns: auto 1fr;
  font-size: 12.5px;
}

dt {
  color: var(--muted);
  font-size: 12px;
}

dt.on {
  color: var(--executor);
}

dd {
  margin: 0;
  color: var(--ink-2);
}

.files-kinds {
  margin: 0;
  color: var(--muted);
  font-size: 12.5px;
  line-height: 1.4;
}

.files-kinds b {
  color: var(--ink-2);
  font-weight: 500;
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
