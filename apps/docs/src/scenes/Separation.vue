<script setup lang="ts">
import type { Packet } from '../kit/Packets.vue'
import type { Step } from '../kit/player'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Machine from '../kit/Machine.vue'
import Packets from '../kit/Packets.vue'
import Pod from '../kit/Pod.vue'

import { curve } from '../kit/geometry'

/**
 * Stateless actor, stateful executors. The actor runs from a read-only root with a throwaway writable layer; the
 * workspace lives on an executor and is mounted in; a copy-in and mirror-out pair carries a login across runs. When
 * the run ends the actor is gone and the executor keeps the work; the next run starts clean and mounts the same
 * workspace. The actor sits near its main dependency, the executors near what the work operates on (README, How it
 * works). Run IDs are made up for the scene.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 720

const RUNS = { x: 40, y: 120, w: 290 }
const MODEL = { x: 410, y: 12, w: 300 }
const POD = { x: 380, y: 118, w: 470, h: 456 }
const EXEC = { x: 930, y: 118, w: 470, h: 268 }
const HOST = { x: 930, y: 430, w: 470, h: 144 }

/** A light line from the actor up to the model it talks to; the gateway is another scene's subject. */
const UP = `M${POD.x + 120} ${POD.y} C${POD.x + 120} ${POD.y - 40} ${MODEL.x + 160} ${MODEL.y + 110} ${MODEL.x + 160} ${MODEL.y + 74}`
const MOUNT = curve(POD.x + POD.w, 236, EXEC.x, 236)
const LOGIN = curve(POD.x + POD.w, 498, HOST.x, 498)

const RUN_A = '3f9c0e2a71b4'
const RUN_B = '8d21b07c5e90'

interface SeparationStep extends Step {
  run: 'a' | 'b'
  /** The actor is gone: between runs. */
  ended?: boolean
  /** What the run wrote to its throwaway layer so far. */
  scratch: string[]
  /** The workspace's files, on the executor. */
  workspace: string[]
  fresh?: string
  login: 'in' | 'out' | 'idle'
  mount: boolean
  near?: boolean
}

const STEPS: SeparationStep[] = [
  {
    ms: 3800,
    run: 'a',
    scratch: [],
    workspace: ['src/parse.py', 'tests/test_parse.py'],
    login: 'in',
    mount: true,
    caption: 'A run\'s actor starts from a read-only image. The login is copied in, and the workspace is mounted from the executor.',
  },
  {
    ms: 3600,
    run: 'a',
    scratch: ['~/.cache/opencode/', '/tmp/pytest-of-agent/'],
    workspace: ['src/parse.py', 'tests/test_parse.py', 'notes.md'],
    fresh: 'notes.md',
    login: 'idle',
    mount: true,
    caption: 'What the program writes to the workspace lands on the executor. Everything else goes to a throwaway layer.',
  },
  {
    ms: 3800,
    run: 'a',
    ended: true,
    scratch: [],
    workspace: ['src/parse.py', 'tests/test_parse.py', 'notes.md'],
    login: 'out',
    mount: false,
    caption: 'When the run ends, mirror-out saves the login. The actor is thrown away with its writable layer; the workspace stays.',
  },
  {
    ms: 4000,
    run: 'b',
    scratch: [],
    workspace: ['src/parse.py', 'tests/test_parse.py', 'notes.md'],
    login: 'in',
    mount: true,
    caption: 'The next run gets a new actor from the same image, copies the login in, and mounts the same workspace.',
  },
  {
    ms: 4200,
    run: 'b',
    near: true,
    scratch: [],
    workspace: ['src/parse.py', 'tests/test_parse.py', 'notes.md'],
    login: 'idle',
    mount: true,
    caption: 'The actor sits near its main dependency, such as inference. Executors sit near what the work operates on.',
  },
]

function packets(at: number): Packet[] {
  const step = STEPS[at]!
  const list: Packet[] = []
  if (step.login === 'in')
    list.push({ id: `in${at}`, d: LOGIN, role: 'host', text: 'auth.json', delay: 900, ms: 1000, back: true })
  if (step.login === 'out')
    list.push({ id: `out${at}`, d: LOGIN, role: 'actor', text: 'auth.json', delay: 200, ms: 1000 })
  if (step.fresh)
    list.push({ id: `w${at}`, d: MOUNT, role: 'executor', text: step.fresh, delay: 400, ms: 1000 })
  return list
}

// The utilities a run's card, its state and a layer of the actor share.
const RUN = 'grid items-center gap-x-[10px] gap-y-[2px] p-[12px_14px] rounded-[14px] grid-cols-[1fr_auto] [transition:opacity_0.45s,background_0.45s,transform_0.45s_cubic-bezier(0.2,0,0,1)]'
const RUN_LIVE = 'bg-[color-mix(in_srgb,var(--actor)_10%,var(--surface))] [transform:translateX(6px)]'
const STATE = 'p-[2px_9px] rounded-[999px] text-[12px] [grid-column:2] [grid-row:1/span_2]'
const LAYER = 'flex flex-col gap-[3px] p-[10px_13px] rounded-[14px] [transition:opacity_0.4s]'
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Separation: the actor is stateless, a read-only image with a throwaway layer; the workspace lives on the executor and the login on the control host. When a run ends the actor is thrown away and the next run mounts the same workspace.">
    <template #default="{ at }">
      <svg class="absolute inset-0 pointer-events-none" :width="W" :height="H" aria-hidden="true">
        <path :d="UP" class="fill-none [stroke-width:1.6] [stroke-linecap:round] [transition:stroke_0.4s]" :class="STEPS[at]!.near ? 'stroke-actor/50' : 'stroke-line [stroke-dasharray:1.5_6]'" />
        <path :d="MOUNT" class="fill-none [stroke-width:1.6] [stroke-linecap:round] [transition:stroke_0.4s]" :class="STEPS[at]!.mount ? 'stroke-executor/60' : 'stroke-line [stroke-dasharray:1.5_6]'" />
        <path v-if="STEPS[at]!.mount" :d="MOUNT" class="fill-none stroke-executor [stroke-width:4] [stroke-dasharray:0.1_21.9] [stroke-linecap:round] animate-[dash-flow_0.9s_linear_infinite]" />
        <path v-if="STEPS[at]!.mount" :d="MOUNT" class="fill-none stroke-executor [stroke-width:4] [stroke-dasharray:0.1_21.9] [stroke-dashoffset:11] [stroke-linecap:round] opacity-60 animate-[dash-flow_0.9s_linear_infinite_reverse]" />
        <path :d="LOGIN" class="fill-none [stroke-width:1.6] [stroke-linecap:round] [transition:stroke_0.4s]" :class="STEPS[at]!.login !== 'idle' ? 'stroke-ink/40' : 'stroke-line [stroke-dasharray:1.5_6]'" />
        <text class="mono fill-muted text-[12.5px]" :x="(POD.x + POD.w + EXEC.x) / 2" y="224" text-anchor="middle">mount</text>
        <text class="mono fill-muted text-[12.5px]" :x="(POD.x + POD.w + HOST.x) / 2" y="486" text-anchor="middle">{{ STEPS[at]!.login === 'out' ? 'mirror-out' : 'copy-in' }}</text>
      </svg>

      <div class="absolute top-[120px] left-[40px] flex w-[290px] flex-col gap-[10px]">
        <span class="display text-[19px] text-ink-2">Runs</span>
        <div
          :class="[RUN, STEPS[at]!.run === 'a' && !STEPS[at]!.ended ? RUN_LIVE : 'bg-surface', (STEPS[at]!.run === 'b' || STEPS[at]!.ended) && 'opacity-55']"
        >
          <span class="font-display text-[16px]" :class="(STEPS[at]!.run === 'b' || STEPS[at]!.ended) && 'line-through decoration-line-strong'">Run A</span>
          <code class="text-[12.5px] text-muted [grid-column:1]" :class="(STEPS[at]!.run === 'b' || STEPS[at]!.ended) && 'line-through decoration-line-strong'">{{ RUN_A }}</code>
          <span :class="[STATE, STEPS[at]!.run === 'a' && !STEPS[at]!.ended ? 'bg-actor-fill text-white' : 'bg-control text-muted']">{{ STEPS[at]!.run === 'a' && !STEPS[at]!.ended ? 'running' : 'ended' }}</span>
        </div>
        <div :class="[RUN, STEPS[at]!.run === 'b' ? RUN_LIVE : 'bg-surface opacity-30']">
          <span class="font-display text-[16px]">Run B</span>
          <code class="text-[12.5px] text-muted [grid-column:1]">{{ RUN_B }}</code>
          <span :class="[STATE, STEPS[at]!.run === 'b' ? 'bg-actor-fill text-white' : 'bg-control text-muted']">{{ STEPS[at]!.run === 'b' ? 'running' : 'not yet' }}</span>
        </div>
        <p class="m-[6px_0_0] text-[14px] leading-[1.5] text-muted">
          Each run gets a fresh actor. Nothing in it outlives the run.
        </p>
      </div>

      <Machine :x="MODEL.x" :y="MODEL.y" :w="MODEL.w" role="host" icon="mingcute:cloud-line" title="Model API" sub="the actor's main dependency" :focus="STEPS[at]!.near ? 'focus' : 'idle'" />

      <!-- A new actor forms when the run changes. -->
      <div :key="STEPS[at]!.run" class="absolute inset-0 animate-[form-in_0.7s_cubic-bezier(0.2,0,0,1)_both]">
        <Pod v-bind="POD" driver="linux-firecracker" :focus="STEPS[at]!.ended ? 'gone' : STEPS[at]!.near ? 'focus' : 'idle'">
          <code class="self-start p-[2px_9px] rounded-[6px] bg-actor/12 text-[12.5px] text-actor">run {{ STEPS[at]!.run === 'a' ? RUN_A : RUN_B }}</code>
          <div :class="[LAYER, 'bg-[color-mix(in_srgb,var(--executor)_12%,var(--surface))]', !STEPS[at]!.mount && 'opacity-40']">
            <span class="flex items-center gap-[6px] font-display text-[15px] text-executor"><Icon name="mingcute:folder-open-line" />Workspace</span>
            <code class="text-[12.5px] text-ink-2">~/workspace/</code>
            <span class="text-[12.5px] text-muted">mounted from build</span>
          </div>
          <div :class="LAYER" class="bg-surface/72">
            <span class="flex items-center gap-[6px] font-display text-[15px]"><Icon name="mingcute:key-2-line" />Login</span>
            <code class="text-[12.5px] text-ink-2">~/.local/share/opencode/auth.json</code>
            <span class="text-[12.5px] text-muted">copied in from the host</span>
          </div>
          <div :class="LAYER" class="bg-actor/7">
            <span class="flex items-center gap-[6px] font-display text-[15px] text-actor"><Icon name="mingcute:refresh-2-line" />Throwaway layer</span>
            <span class="flex flex-wrap gap-[6px] min-h-[22px]">
              <code v-for="f in STEPS[at]!.scratch" :key="f" class="p-[1px_8px] rounded-[6px] bg-surface text-[12.5px] text-ink-2 animate-[rise-4_0.4s_0.8s_cubic-bezier(0.2,0,0,1)_both]">{{ f }}</code>
              <span v-if="STEPS[at]!.scratch.length === 0" class="text-[12.5px] text-muted">empty</span>
            </span>
          </div>
          <div :class="LAYER" class="bg-control-3/55">
            <span class="flex items-center gap-[6px] font-display text-[15px]"><Icon name="mingcute:lock-line" />Read-only root</span>
            <code class="text-[12.5px] text-ink-2">ubuntu:22.04</code>
            <span class="text-[12.5px] text-muted">the actor image</span>
          </div>
        </Pod>
      </div>

      <p v-if="STEPS[at]!.ended" class="absolute top-[320px] left-[380px] w-[470px] m-0 font-display text-[18px] text-center text-muted animate-[rise-4_0.5s_0.6s_both]">
        The actor is gone. A new one comes from the image.
      </p>

      <Machine v-bind="EXEC" title="Executor" name="build" icon="mingcute:cube-line" sub="linux-container · keeps the workspace" badge="Stateful" :focus="STEPS[at]!.ended || STEPS[at]!.near ? 'focus' : 'idle'">
        <ul class="mono flex flex-col gap-[5px] m-[8px_0_0] p-0 list-none text-[13.5px]">
          <li class="p-[6px_11px] rounded-[8px] bg-control text-executor">
            ~/workspace/
          </li>
          <li
            v-for="f in STEPS[at]!.workspace" :key="f" class="p-[6px_11px_6px_26px] rounded-[8px]"
            :class="f === STEPS[at]!.fresh ? 'bg-executor-softer animate-[rise-4_0.45s_1.2s_cubic-bezier(0.2,0,0,1)_both]' : 'bg-control'"
          >
            {{ f }}
          </li>
        </ul>
      </Machine>

      <Machine v-bind="HOST" role="host" icon="mingcute:server-2-line" title="Control Host" sub="keeps what a run copies out" :focus="STEPS[at]!.login === 'out' ? 'focus' : 'idle'">
        <code class="mt-[6px] p-[7px_11px] rounded-[8px] bg-control text-[13px] text-ink-2">~/needy-state/opencode/auth.json</code>
      </Machine>

      <Packets :packets="packets(at)" />
    </template>
  </Frame>
</template>
