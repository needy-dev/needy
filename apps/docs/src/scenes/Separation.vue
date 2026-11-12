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
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Separation: the actor is stateless, a read-only image with a throwaway layer; the workspace lives on the executor and the login on the control host. When a run ends the actor is thrown away and the next run mounts the same workspace.">
    <template #default="{ at }">
      <svg class="wires" :width="W" :height="H" aria-hidden="true">
        <path :d="UP" class="wire" :class="{ model: STEPS[at]!.near }" />
        <path :d="MOUNT" class="wire mount" :class="{ live: STEPS[at]!.mount }" />
        <path v-if="STEPS[at]!.mount" :d="MOUNT" class="flow" />
        <path v-if="STEPS[at]!.mount" :d="MOUNT" class="flow back" />
        <path :d="LOGIN" class="wire" :class="{ host: STEPS[at]!.login !== 'idle' }" />
        <text class="wire-label mono" :x="(POD.x + POD.w + EXEC.x) / 2" y="224" text-anchor="middle">mount</text>
        <text class="wire-label mono" :x="(POD.x + POD.w + HOST.x) / 2" y="486" text-anchor="middle">{{ STEPS[at]!.login === 'out' ? 'mirror-out' : 'copy-in' }}</text>
      </svg>

      <div class="runs">
        <span class="runs-title display">Runs</span>
        <div class="run" :class="{ live: STEPS[at]!.run === 'a' && !STEPS[at]!.ended, over: STEPS[at]!.run === 'b' || STEPS[at]!.ended }">
          <span class="run-name">Run A</span>
          <code>{{ RUN_A }}</code>
          <span class="state">{{ STEPS[at]!.run === 'a' && !STEPS[at]!.ended ? 'running' : 'ended' }}</span>
        </div>
        <div class="run" :class="{ live: STEPS[at]!.run === 'b', ghost: STEPS[at]!.run !== 'b' }">
          <span class="run-name">Run B</span>
          <code>{{ RUN_B }}</code>
          <span class="state">{{ STEPS[at]!.run === 'b' ? 'running' : 'not yet' }}</span>
        </div>
        <p class="runs-note">
          Each run gets a fresh actor. Nothing in it outlives the run.
        </p>
      </div>

      <Machine :x="MODEL.x" :y="MODEL.y" :w="MODEL.w" role="host" icon="mingcute:cloud-line" title="Model API" sub="the actor's main dependency" :focus="STEPS[at]!.near ? 'focus' : 'idle'" />

      <div :key="STEPS[at]!.run" class="form">
        <Pod v-bind="POD" driver="linux-firecracker" :focus="STEPS[at]!.ended ? 'gone' : STEPS[at]!.near ? 'focus' : 'idle'">
          <code class="run-id">run {{ STEPS[at]!.run === 'a' ? RUN_A : RUN_B }}</code>
          <div class="layer mount" :class="{ off: !STEPS[at]!.mount }">
            <span class="layer-name"><Icon name="mingcute:folder-open-line" />Workspace</span>
            <code>~/workspace/</code>
            <span class="layer-note">mounted from build</span>
          </div>
          <div class="layer login">
            <span class="layer-name"><Icon name="mingcute:key-2-line" />Login</span>
            <code>~/.local/share/opencode/auth.json</code>
            <span class="layer-note">copied in from the host</span>
          </div>
          <div class="layer scratch">
            <span class="layer-name"><Icon name="mingcute:refresh-2-line" />Throwaway layer</span>
            <span class="files">
              <code v-for="f in STEPS[at]!.scratch" :key="f" class="file">{{ f }}</code>
              <span v-if="STEPS[at]!.scratch.length === 0" class="layer-note">empty</span>
            </span>
          </div>
          <div class="layer root">
            <span class="layer-name"><Icon name="mingcute:lock-line" />Read-only root</span>
            <code>ubuntu:22.04</code>
            <span class="layer-note">the actor image</span>
          </div>
        </Pod>
      </div>

      <p v-if="STEPS[at]!.ended" class="gone-note">
        The actor is gone. A new one comes from the image.
      </p>

      <Machine v-bind="EXEC" title="Executor" name="build" icon="mingcute:cube-line" sub="linux-container · keeps the workspace" badge="Stateful" :focus="STEPS[at]!.ended || STEPS[at]!.near ? 'focus' : 'idle'">
        <ul class="tree mono">
          <li class="dir">
            ~/workspace/
          </li>
          <li v-for="f in STEPS[at]!.workspace" :key="f" :class="{ fresh: f === STEPS[at]!.fresh }">
            {{ f }}
          </li>
        </ul>
      </Machine>

      <Machine v-bind="HOST" role="host" icon="mingcute:server-2-line" title="Control Host" sub="keeps what a run copies out" :focus="STEPS[at]!.login === 'out' ? 'focus' : 'idle'">
        <code class="kept">~/needy-state/opencode/auth.json</code>
      </Machine>

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
  transition: stroke 0.4s;
}

.wire.model {
  stroke: color-mix(in srgb, var(--actor) 50%, transparent);
  stroke-dasharray: none;
}

.wire.mount.live {
  stroke: color-mix(in srgb, var(--executor) 60%, transparent);
  stroke-dasharray: none;
}

.wire.host {
  stroke: color-mix(in srgb, var(--ink) 40%, transparent);
  stroke-dasharray: none;
}

.flow {
  fill: none;
  stroke: var(--executor);
  stroke-width: 4;
  stroke-dasharray: 0.1 21.9;
  stroke-linecap: round;
  animation: flow 0.9s linear infinite;
}

.flow.back {
  stroke-dashoffset: 11;
  animation-direction: reverse;
  opacity: 0.6;
}

@keyframes flow {
  to {
    stroke-dashoffset: -22;
  }
}

.wire-label {
  fill: var(--muted);
  font-size: 12.5px;
}

.runs {
  position: absolute;
  top: 120px;
  left: 40px;
  display: flex;
  width: 290px;
  flex-direction: column;
  gap: 10px;
}

.runs-title {
  color: var(--ink-2);
  font-size: 19px;
}

.run {
  display: grid;
  align-items: center;
  gap: 2px 10px;
  padding: 12px 14px;
  border-radius: 14px;
  background: var(--surface);
  grid-template-columns: 1fr auto;
  transition:
    opacity 0.45s,
    background 0.45s,
    transform 0.45s cubic-bezier(0.2, 0, 0, 1);
}

.run code {
  color: var(--muted);
  font-size: 12.5px;
  grid-column: 1;
}

.run-name {
  font-family: var(--font-display);
  font-size: 16px;
}

.state {
  padding: 2px 9px;
  border-radius: 999px;
  background: var(--control);
  color: var(--muted);
  font-size: 12px;
  grid-column: 2;
  grid-row: 1 / span 2;
}

.run.live {
  background: color-mix(in srgb, var(--actor) 10%, var(--surface));
  transform: translateX(6px);
}

.run.live .state {
  background: var(--actor-fill);
  color: #fff;
}

.run.over {
  opacity: 0.55;
}

.run.over .run-name,
.run.over code {
  text-decoration: line-through;
  text-decoration-color: var(--line-strong);
}

.run.ghost {
  opacity: 0.3;
}

.runs-note {
  margin: 6px 0 0;
  color: var(--muted);
  font-size: 14px;
  line-height: 1.5;
}

/* A new actor forms when the run changes. */
.form {
  position: absolute;
  inset: 0;
  animation: form 0.7s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes form {
  from {
    opacity: 0;
    filter: blur(6px);
    transform: scale(0.97);
  }
}

.run-id {
  align-self: flex-start;
  padding: 2px 9px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--actor) 12%, transparent);
  color: var(--actor);
  font-size: 12.5px;
}

.layer {
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 10px 13px;
  border-radius: 14px;
  background: color-mix(in srgb, var(--surface) 72%, transparent);
  transition: opacity 0.4s;
}

.layer-name {
  display: flex;
  align-items: center;
  gap: 6px;
  font-family: var(--font-display);
  font-size: 15px;
}

.layer code {
  color: var(--ink-2);
  font-size: 12.5px;
}

.layer-note {
  color: var(--muted);
  font-size: 12.5px;
}

.layer.mount {
  background: color-mix(in srgb, var(--executor) 12%, var(--surface));
}

.layer.mount .layer-name {
  color: var(--executor);
}

.layer.mount.off {
  opacity: 0.4;
}

.layer.scratch {
  background: color-mix(in srgb, var(--actor) 7%, transparent);
}

.layer.scratch .layer-name {
  color: var(--actor);
}

.layer.root {
  background: color-mix(in srgb, var(--control-3) 55%, transparent);
}

.files {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  min-height: 22px;
}

.file {
  padding: 1px 8px;
  border-radius: 6px;
  background: var(--surface);
  animation: arrive 0.4s 0.8s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes arrive {
  from {
    opacity: 0;
    transform: translateY(4px);
  }
}

.gone-note {
  position: absolute;
  top: 320px;
  left: 380px;
  width: 470px;
  margin: 0;
  color: var(--muted);
  font-family: var(--font-display);
  font-size: 18px;
  text-align: center;
  animation: arrive 0.5s 0.6s both;
}

.tree {
  display: flex;
  flex-direction: column;
  gap: 5px;
  margin: 8px 0 0;
  padding: 0;
  list-style: none;
  font-size: 13.5px;
}

.tree li {
  padding: 6px 11px 6px 26px;
  border-radius: 8px;
  background: var(--control);
}

.tree li.dir {
  padding-left: 11px;
  color: var(--executor);
}

.tree li.fresh {
  background: var(--executor-softer);
  animation: arrive 0.45s 1.2s cubic-bezier(0.2, 0, 0, 1) both;
}

.kept {
  margin-top: 6px;
  padding: 7px 11px;
  border-radius: 8px;
  background: var(--control);
  color: var(--ink-2);
  font-size: 13px;
}
</style>
