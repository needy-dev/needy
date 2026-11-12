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

// The picture in a card: its text and code; a file mode that is not picked; a trace event.
const PIC = 'flex min-h-0 flex-1 flex-col justify-between gap-[8px] [&_p]:m-0 [&_p]:text-[13.5px] [&_p]:leading-[1.4] [&_p]:text-ink-2 [&_code]:text-[12.5px]'
const MODE = 'p-[3px_10px] rounded-[999px] bg-control text-muted'
const EVENT = 'p-[3px_10px] rounded-[999px] bg-trace-softer text-trace'
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="The anatomy of a profile: actor, network, executors, commands, files, chaos and trace, each linked to the part of a run it controls.">
    <template #default="{ at }">
      <svg class="absolute inset-0 pointer-events-none" :width="W" :height="H" aria-hidden="true">
        <path
          v-for="s in SECTIONS" :key="s.id" :d="leader(s)" class="fill-none [stroke-linecap:round] [transition:stroke_0.35s]"
          :class="s.id === STEPS[at]!.section ? 'stroke-role [stroke-width:2]' : 'stroke-line [stroke-width:1.5] [stroke-dasharray:1.5_6]'"
          :style="{ '--role': color(s.role) }"
        />
      </svg>

      <div class="absolute" :style="{ left: `${CODE.x}px`, top: `${CODE.y}px`, width: `${CODE.w}px`, height: `${CODE_H}px` }">
        <Profile :code="PROFILE" :lit="lit(at)" :role="sectionOf(at).role" :roles="ROLE" title="~/.config/needy/profiles/agent.yaml" />
      </div>

      <span
        v-for="s in SECTIONS" :key="`b${s.id}`" class="absolute w-[4px] rounded-[2px] [transition:background_0.35s]" :class="s.id === STEPS[at]!.section ? 'bg-role' : 'bg-control-3'"
        :style="{ 'left': `${bracket(s).x}px`, 'top': `${bracket(s).y}px`, 'height': `${bracket(s).h}px`, '--role': color(s.role, 'fill') }"
      />

      <div
        v-for="s in SECTIONS" :key="s.id" class="f absolute flex flex-col gap-[8px] p-[13px_16px] rounded-[14px] bg-surface" :class="focus(at, s.id)"
        :style="{ 'left': `${(s.side === 'left' ? LEFT : RIGHT).x}px`, 'top': `${s.y}px`, 'width': `${(s.side === 'left' ? LEFT : RIGHT).w}px`, 'height': `${s.h}px`, '--role': color(s.role), '--role-fill': color(s.role, 'fill') }"
      >
        <header class="flex items-center gap-[10px]">
          <span class="display text-[19px] text-role">{{ s.title }}</span>
          <code class="ml-auto text-[12.5px] text-muted">{{ s.id === 'network' ? 'actor.network' : s.id === 'commands' ? 'commands' : s.id }}</code>
        </header>

        <!-- Each card is a small picture of what its section controls, in the shapes of the other scenes. -->
        <div v-if="s.id === 'actor'" :class="PIC">
          <div class="flex whitespace-nowrap items-center gap-[8px] p-[8px_12px] rounded-[999px] bg-actor/10 text-[12.5px]">
            <Logo name="firecracker" :size="20" />
            <span class="mono">linux-firecracker</span>
            <span class="ml-auto p-[2px_9px] rounded-[999px] bg-surface text-[12px] text-ink-2">Compute Engine</span>
          </div>
          <p>Runs the harness <code>opencode</code>, near the model. The recipe, a second file, sets the machine it looks like.</p>
        </div>

        <div v-else-if="s.id === 'network'" :class="PIC">
          <div class="mono flex items-center gap-[10px]">
            <span class="inline-flex items-center gap-[5px] p-[3px_10px] rounded-[999px] bg-executor/14 text-[12.5px] text-executor"><Icon name="mingcute:check-line" />models.dev</span>
            <span class="inline-flex items-center gap-[5px] p-[3px_10px] rounded-[999px] bg-bad/12 text-[12.5px] text-bad"><Icon name="mingcute:forbid-circle-line" />example.com</span>
          </div>
          <p>The actor reaches only the hosts it allows. A service is a name the gateway forwards to a real address.</p>
        </div>

        <div v-else-if="s.id === 'executors'" :class="PIC">
          <div class="flex gap-[10px]">
            <span class="inline-flex items-center gap-[8px] p-[7px_12px] rounded-[9px] bg-executor/12 text-executor"><Icon name="mingcute:cube-line" /><code>build</code></span>
          </div>
          <p>The pool for compute and IO, next to the workspace. One by default; <code>runner:</code> puts one on another machine.</p>
        </div>

        <div v-else-if="s.id === 'commands'" :class="PIC">
          <ol class="mono flex flex-col gap-[4px] m-0 p-0 list-none text-[12.5px]">
            <li class="flex justify-between p-[4px_10px] rounded-[7px] bg-control">
              git push ** <span class="text-bad">deny</span>
            </li>
            <li class="flex justify-between p-[4px_10px] rounded-[7px] bg-control">
              pytest ** <span class="text-executor">→ build</span>
            </li>
            <li class="flex justify-between p-[4px_10px] rounded-[7px] bg-control text-muted">
              anything else <span class="text-actor">→ actor</span>
            </li>
          </ol>
          <p>A rule can also set a timeout, the environment, the directory or extra files.</p>
        </div>

        <div v-else-if="s.id === 'files'" :class="PIC">
          <div class="mono flex gap-[6px] text-[12.5px]">
            <span class="p-[3px_10px] rounded-[999px] bg-executor/16 text-executor">copy-in</span><span :class="MODE">mount</span><span :class="MODE">mirror-out</span>
          </div>
          <p>A login copied in from the host when the run starts.</p>
        </div>

        <div v-else-if="s.id === 'chaos'" :class="PIC">
          <p class="mono flex items-center gap-[6px] !text-[12.5px] !text-fault">
            <Icon name="mingcute:fire-line" /> curl → exit 7, without running
          </p>
          <p>The same seed fires the same faults on a replay.</p>
        </div>

        <div v-else :class="PIC">
          <p class="mono flex flex-wrap gap-[6px] !text-[12px]">
            <span :class="EVENT">exec.decide</span><span :class="EVENT">net.request</span><span :class="EVENT">fault.injected</span>
          </p>
        </div>
      </div>
    </template>
  </Frame>
</template>
