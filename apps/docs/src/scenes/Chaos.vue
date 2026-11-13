<script setup lang="ts">
import type { Step } from '../kit/player'
import type { Line } from '../kit/terminal'
import type { TraceEvent } from '../kit/Trace.vue'

import Frame from '../kit/Frame.vue'
import Machine from '../kit/Machine.vue'
import Pod from '../kit/Pod.vue'
import Profile from '../kit/Profile.vue'
import Terminal from '../kit/Terminal.vue'
import Trace from '../kit/Trace.vue'

import { CHAOS_PROFILE } from '../route/samples'

/**
 * examples/chaos: a file fault on the shared workspace, an exec fault and a scheduled overheat, from a seed. The
 * strings follow that example at https://github.com/needy-dev/needy/blob/90a8e8e/examples/chaos/profile.yaml:
 * `Input/output error` on broken.json, curl's own refusal from the fault's `stderr` with exit 7, and
 * `+101.0°C ALARM (CRIT)` as the closed-loop results record it. The report counts the faults as
 * crates/core/src/report/mod.rs at that commit writes them: `name (domain) (count)`, most first, then by name.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 576

interface ChaosStep extends Step {
  lines: Line[]
  lit: number[]
  event?: TraceEvent
  clear?: boolean
}

const STEPS: ChaosStep[] = [
  {
    ms: 3800,
    lines: [
      { text: 'cat ~/workspace/broken.json', kind: 'cmd', tag: { text: 'file.open', role: 'fault' } },
      { text: 'cat: /home/agent/workspace/broken.json: Input/output error', kind: 'fault' },
    ],
    lit: [5],
    event: { kind: 'fault.injected', text: 'broken-json (file)', role: 'fault' },
    caption: 'A file fault on the shared workspace: opening a .json file fails with EIO.',
  },
  {
    ms: 3600,
    lines: [
      { text: 'curl -s http://example.invalid', kind: 'cmd', tag: { text: 'exec', role: 'fault' } },
      { text: 'curl: (7) Failed to connect to example.invalid port 80: Connection refused', kind: 'fault' },
      { text: 'echo $?', kind: 'cmd' },
      { text: '7', kind: 'out' },
    ],
    lit: [1, 6],
    event: { kind: 'fault.injected', text: 'curl-down (exec)', role: 'fault' },
    caption: 'An exec fault answers for curl without running it: curl\'s own error, and exit 7.',
  },
  {
    ms: 3600,
    lines: [
      { text: 'sensors | grep -m1 Package', kind: 'cmd', tag: { text: 'sensor', role: 'fault' } },
      { text: 'Package id 0:  +101.0°C  ALARM (CRIT)', kind: 'fault' },
    ],
    lit: [7],
    event: { kind: 'fault.injected', text: 'overheat (sensor)', role: 'fault' },
    caption: 'Two seconds in, the actor\'s sensors overheat, as the recipe\'s hwmon reports them.',
  },
  {
    ms: 4600,
    clear: true,
    lines: [
      { text: 'needy run report latest', kind: 'cmd' },
      { text: '…', kind: 'dim' },
      { text: 'Findings' },
      { text: '  injected faults: 3 (broken-json (file) (1), curl-down (exec) (1), overheat (sensor) (1))', kind: 'fault' },
    ],
    lit: [3],
    caption: 'Faults fire from the seed, so a replayed run injects the same ones. The report counts them by kind.',
  },
]

function screenAt(at: number): { lines: Line[], fresh: number } {
  let start = at
  while (start > 0 && !STEPS[start]!.clear)
    start--
  const before = STEPS.slice(start, at).flatMap(step => step.lines)
  return { lines: [...before, ...STEPS[at]!.lines], fresh: before.length }
}

function eventsAt(at: number): { events: TraceEvent[], fresh: number } {
  const events = STEPS.slice(0, at + 1).flatMap(step => step.event ? [step.event] : [])
  const fresh = STEPS.slice(0, at).flatMap(step => step.event ? [step.event] : []).length
  return { events, fresh }
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Chaos: from a seed, a file fault, an exec fault and an overheating sensor fire in a run, and the report counts them.">
    <template #default="{ at }">
      <div class="absolute top-[24px] left-[36px] w-[540px]">
        <Profile :code="CHAOS_PROFILE" :lit="STEPS[at]!.lit" role="fault" title="examples/chaos/profile.yaml" />
      </div>

      <Machine :x="36" :y="262" :w="540" :h="218" role="trace" icon="mingcute:git-branch-line" title="Trace" sub="fault.injected events, in order">
        <!-- The kit's Trace is a row that fades its oldest events at the left; here it is a column, all shown. -->
        <div class="mt-[4px] [&_.trace]:(!flex-col !items-start ![mask-image:none])">
          <Trace v-bind="eventsAt(at)" :keep="3" :delay="900" />
        </div>
        <p class="mx-0 mb-0 mt-auto flex items-center gap-[10px] text-[13.5px] text-ink-2">
          <span class="mono rounded-[8px] px-[10px] py-[3px] text-[12.5px] text-fault bg-fault/15">seed: 1</span> The same seed fires the same faults on every replay.
        </p>
      </Machine>

      <Pod :x="626" :y="24" :w="778" :h="456" logo="firecracker" driver="linux-firecracker">
        <Terminal v-bind="screenAt(at)" title="examples/chaos" prompt="agent@instance:~$" :rows="15" :play="at" />
      </Pod>
    </template>
  </Frame>
</template>

