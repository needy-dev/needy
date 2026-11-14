<script setup lang="ts">
import type { Focus } from '../kit/focus'
import type { Step } from '../kit/player'
import type { Line } from '../kit/terminal'

import Frame from '../kit/Frame.vue'
import Machine from '../kit/Machine.vue'
import Pod from '../kit/Pod.vue'
import Profile from '../kit/Profile.vue'
import Terminal from '../kit/Terminal.vue'

import { DEMO_PROFILE } from '../route/samples'

/**
 * The README's Getting started, typed out: install, start the control daemon, check a profile, run it, and read the
 * report. The outputs follow what the CLI prints: `needy config profile check` from crates/cli/src/commands/profile.rs, the
 * deny and fault lines from crates/cli/tests/cli.rs, and the report from crates/core/src/report/human.rs, with a run
 * ID, durations and the recipe lines made up for the scene.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 720

const PHASES = ['Install', 'Daemon', 'Profile', 'Run', 'Report'] as const

interface StartStep extends Step {
  phase: number
  /** Start a new screen, as after `clear`. */
  clear?: boolean
  lines: Line[]
  /** The lines of the profile to light, from 0. */
  lit: number[]
  /** What runs on the control host by now. */
  up: Array<'daemon' | 'actor' | 'build' | 'trace'>
  /** Where the newest command ran. */
  on?: 'actor' | 'build'
}

const ACTOR_PROMPT = 'agent@instance:~$'

const STEPS: StartStep[] = [
  {
    ms: 3800,
    phase: 0,
    lines: [
      // The README's Linux install line, its path elided to fit the terminal.
      { text: 'curl -fsSL https://raw.githubusercontent.com/…/install/install.sh | sh', kind: 'cmd' },
      { text: '# installs `needy` and the guest needy it brings into actors', kind: 'dim' },
      { text: 'needy image pull', kind: 'cmd' },
      { text: '# the base VM image and the executor image of this release', kind: 'dim' },
    ],
    lit: [],
    up: [],
    caption: 'Install needy, and pull the images of its release.',
  },
  {
    ms: 2600,
    phase: 1,
    lines: [{ text: 'needy daemon service install --now', kind: 'cmd' }, { text: '# publishes a private local management socket', kind: 'dim' }],
    lit: [],
    up: ['daemon'],
    caption: 'Start a control daemon on the machine that runs actors.',
  },
  {
    ms: 4600,
    phase: 2,
    lines: [
      { text: 'needy config profile check demo --mode control --cmd "python3 -V"', kind: 'cmd' },
      { text: 'user: agent (1000:1000)' },
      { text: '…', kind: 'dim' },
      { text: 'route names: python3' },
      { text: '…', kind: 'dim' },
      { text: '' },
      { text: 'COMMAND     TARGET          ARGV        RULES  FAULTS', kind: 'dim' },
      { text: 'python3 -V  executor build  python3 -V  0', kind: 'ok' },
    ],
    lit: [0, 1, 2, 3, 4, 5, 6, 7, 8],
    up: ['daemon'],
    caption: 'A profile names the actor driver, the executors, and which commands go where.',
  },
  {
    ms: 3800,
    phase: 3,
    clear: true,
    lines: [
      { text: 'needy run create -it --profile demo', kind: 'cmd' },
      { text: 'systemd-detect-virt', kind: 'cmd', prompt: ACTOR_PROMPT, tag: { text: 'actor', role: 'actor' } },
      { text: 'google', kind: 'out' },
    ],
    lit: [1, 2, 3, 4],
    up: ['daemon', 'actor', 'build', 'trace'],
    on: 'actor',
    caption: 'run create starts the actor: a microVM that looks like the recipe\'s Compute Engine VM.',
  },
  {
    ms: 3600,
    phase: 3,
    lines: [
      { text: 'python3 -c \'print(1 + 1)\'', kind: 'cmd', prompt: ACTOR_PROMPT, tag: { text: '→ build', role: 'executor' } },
      { text: '2', kind: 'out' },
      { text: 'exit', kind: 'cmd', prompt: ACTOR_PROMPT },
    ],
    lit: [5, 6, 7, 8],
    up: ['daemon', 'actor', 'build', 'trace'],
    on: 'build',
    caption: 'python3 matches the rule, so it runs in the build container, with ordinary output and exit codes.',
  },
  {
    ms: 5200,
    phase: 4,
    clear: true,
    lines: [
      { text: 'needy run report latest', kind: 'cmd' },
      { text: 'Run         3f9c0e2a71b4  (profile demo)' },
      { text: 'State       finished, driving program exit 0' },
      { text: 'Duration    21.4s' },
      { text: 'Actor       linux-firecracker' },
      { text: 'Started by  operator' },
      { text: '' },
      { text: 'Commands' },
      { text: '  routed to build  1', kind: 'ok' },
      { text: '  kept on actor    0' },
      { text: '  denied           0' },
      { text: '  faults           0' },
      { text: '  (kept on actor counts only the names the actor routes: python3)', kind: 'dim' },
      { text: '' },
      { text: 'Top commands' },
      { text: '  python3 on build  1  slowest 38 ms' },
      { text: '' },
      { text: 'No findings.' },
    ],
    lit: [],
    up: ['trace'],
    caption: 'needy run report reads the trace: what was routed, denied and faulted, and where.',
  },
]

/** The screen at step `at`: the lines since the last clear, with the lines of this step new. */
function screenAt(at: number): { lines: Line[], fresh: number } {
  let start = at
  while (start > 0 && !STEPS[start]!.clear)
    start--
  const before = STEPS.slice(start, at).flatMap(step => step.lines)
  return { lines: [...before, ...STEPS[at]!.lines], fresh: before.length }
}

type Part = StartStep['up'][number]

/** How a part of the control host stands at step `at`: not yet up, up, or what the newest command used. */
function partFocus(at: number, id: Part): Focus {
  const step = STEPS[at]!
  if (!step.up.includes(id))
    return 'gone'
  return step.on === id ? 'focus' : 'idle'
}

const ROWS = 22
/** The terminal's height for its lines: the bar, the padding and 22.4px a line (Terminal.vue), so it holds no blank. */
function screenHeight(at: number): number {
  const lines = Math.min(ROWS, screenAt(at).lines.length)
  return Math.max(150, 46 + lines * 22.4)
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Getting started: install needy, start the control daemon, check a profile, run it, and read the report.">
    <template #default="{ at }">
      <ol class="absolute top-[26px] left-[50%] m-0 flex list-none gap-[6px] rounded-[18px] p-[5px] bg-control [transform:translateX(-50%)]">
        <li
          v-for="(phase, i) in PHASES" :key="phase"
          class="flex items-center gap-[9px] rounded-[13px] px-[16px] py-[8px] text-[16px] [transition:background_0.3s,color_0.3s]"
          :class="STEPS[at]!.phase === i ? 'text-ink bg-surface' : STEPS[at]!.phase > i ? 'text-ink-2' : 'text-muted'"
        >
          <span class="mono grid h-[22px] w-[22px] place-items-center rounded-[50%] text-[12px]" :class="STEPS[at]!.phase === i ? 'text-[#fff] bg-actor-fill' : 'bg-control-2'">{{ i + 1 }}</span>{{ phase }}
        </li>
      </ol>

      <div class="absolute top-[96px] left-[48px] w-[560px]">
        <Profile :code="DEMO_PROFILE" :lit="STEPS[at]!.lit" :role="STEPS[at]!.on === 'build' ? 'executor' : 'actor'" title="~/.config/needy/profiles/demo.yaml" />
      </div>

      <div class="absolute top-[348px] left-[48px] w-[560px]">
        <span class="font-display text-[15px] text-muted">On the control host</span>
        <div class="mt-[10px] grid grid-cols-[1fr_1fr] gap-[12px] [&>*]:min-h-[112px]">
          <Machine flow role="host" icon="mingcute:server-2-line" title="Control Daemon" sub="needy daemon serve" :focus="partFocus(at, 'daemon')" />
          <Pod flow logo="firecracker" driver="linux-firecracker" badge="" :focus="partFocus(at, 'actor')" />
          <Machine flow icon="mingcute:cube-line" title="Container" name="build" sub="linux-container" :focus="partFocus(at, 'build')" />
          <Machine flow role="trace" icon="mingcute:git-branch-line" title="Trace" sub="trace.db" :focus="partFocus(at, 'trace')" />
        </div>
      </div>

      <div class="absolute top-[96px] left-[648px] w-[744px] [transition:height_0.45s_cubic-bezier(0.2,0,0,1)]" :style="{ height: `${screenHeight(at)}px` }">
        <Terminal v-bind="screenAt(at)" title="a shell on the control host" :rows="ROWS" :play="at" />
      </div>
    </template>
  </Frame>
</template>

