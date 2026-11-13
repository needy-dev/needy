<script setup lang="ts">
import type { Packet } from '../kit/Packets.vue'
import type { Step } from '../kit/player'
import type { Wire } from '../kit/Wires.vue'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Machine from '../kit/Machine.vue'
import Packets from '../kit/Packets.vue'
import Pod from '../kit/Pod.vue'
import Wires from '../kit/Wires.vue'

import { curve } from '../kit/geometry'

/**
 * The file domain: named mappings between the control host (`host:`), the actor (`actor:`) and an executor
 * (`executor:NAME:`), as in the README's Configuration. A copy-in and mirror-out pair carries a login across
 * stateless actors; a mount serves the executor's workspace to the actor over sshfs.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 480

const HOST = { x: 36, y: 30, w: 380, h: 330 }
const ACTOR = { x: 530, y: 30, w: 380, h: 330 }
const EXEC = { x: 1024, y: 30, w: 380, h: 330 }

const COPY = curve(HOST.x + HOST.w, 150, ACTOR.x, 150)
const MOUNT = curve(ACTOR.x + ACTOR.w, 250, EXEC.x, 250)

interface File {
  path: string
  state?: 'new' | 'skip' | 'mount'
}

interface FilesStep extends Step {
  host: File[]
  actor: File[]
  exec: File[]
  copy: 'idle' | 'in' | 'out'
  mount: boolean
  /** The actor is gone. */
  gone?: boolean
  packets: Packet[]
  mapping: string
}

const LOGIN: File = { path: '~/needy-state/opencode/auth.json' }
const CACHE: File = { path: '~/needy-state/opencode/cache/…' }
const SOURCE: File = { path: '~/workspace/src/parse.py' }

const STEPS: FilesStep[] = [
  {
    ms: 3200,
    host: [LOGIN, { ...CACHE, state: 'skip' }],
    actor: [{ path: '~/.local/share/opencode/auth.json', state: 'new' }],
    exec: [SOURCE],
    copy: 'in',
    mount: false,
    packets: [{ id: 'in', d: COPY, role: 'host', text: 'auth.json', delay: 300, ms: 1100 }],
    mapping: 'login-in · copy-in · filter "- /cache/**"',
    caption: 'When the run starts, copy-in brings the login into the actor. The filter leaves the cache out.',
  },
  {
    ms: 3200,
    host: [LOGIN, CACHE],
    actor: [{ path: '~/.local/share/opencode/auth.json' }, { path: '~/workspace/src/parse.py', state: 'mount' }],
    exec: [SOURCE],
    copy: 'idle',
    mount: true,
    packets: [],
    mapping: 'workspace · mount · writable',
    caption: 'The executor serves its workspace, and the actor mounts it live over sshfs.',
  },
  {
    ms: 3200,
    host: [LOGIN, CACHE],
    actor: [{ path: '~/.local/share/opencode/auth.json' }, { path: '~/workspace/src/parse.py', state: 'mount' }, { path: '~/workspace/ok.txt', state: 'new' }],
    exec: [SOURCE, { path: '~/workspace/ok.txt', state: 'new' }],
    copy: 'idle',
    mount: true,
    packets: [{ id: 'w', d: MOUNT, role: 'executor', text: 'ok.txt', delay: 300, ms: 1000 }],
    mapping: 'workspace · mount · writable',
    caption: 'A file the actor writes in the workspace is on the executor, where routed commands see it.',
  },
  {
    ms: 3200,
    host: [{ ...LOGIN, state: 'new' }, CACHE],
    actor: [{ path: '~/.local/share/opencode/auth.json' }, { path: '~/workspace/src/parse.py', state: 'mount' }, { path: '~/workspace/ok.txt', state: 'mount' }],
    exec: [SOURCE, { path: '~/workspace/ok.txt' }],
    copy: 'out',
    mount: true,
    packets: [{ id: 'out', d: COPY, role: 'actor', text: 'auth.json', delay: 300, ms: 1100, back: true }],
    mapping: 'login-out · mirror-out',
    caption: 'When the run ends, mirror-out copies the refreshed login back to the control host.',
  },
  {
    ms: 3200,
    host: [LOGIN, CACHE],
    actor: [],
    exec: [SOURCE, { path: '~/workspace/ok.txt' }],
    copy: 'idle',
    mount: false,
    gone: true,
    packets: [],
    mapping: '',
    caption: 'The actor is thrown away. The login survives on the host, and the work in the workspace.',
  },
]

/** A file's row, by what the step does to it: new (it arrives), skip (a filter leaves it out) or mount (served live). */
function fileClass(state: File['state']): string {
  const row = 'flex items-center gap-[7px] whitespace-nowrap rounded-[9px] px-[10px] py-[7px]'
  if (state === 'new')
    return `${row} text-executor bg-executor/15 animate-[rise-4_0.4s_1.1s_cubic-bezier(0.2,0,0,1)_both]`
  if (state === 'skip')
    return `${row} text-muted line-through bg-ink/5`
  if (state === 'mount')
    return `${row} text-executor bg-executor/10`
  return `${row} bg-ink/5`
}

function wires(at: number): Wire[] {
  const step = STEPS[at]!
  return [
    { id: 'copy', d: COPY, role: step.copy === 'out' ? 'actor' : 'host', state: step.copy === 'idle' ? 'idle' : 'live', label: { x: (HOST.x + HOST.w + ACTOR.x) / 2, y: 134, text: step.copy === 'out' ? 'mirror-out' : 'copy-in' } },
    { id: 'mount', d: MOUNT, role: 'executor', state: step.mount ? 'flow' : 'idle', both: true, label: { x: (ACTOR.x + ACTOR.w + EXEC.x) / 2, y: 234, text: 'sshfs' } },
  ]
}
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Files: copy-in brings a login into the actor, the executor's workspace is mounted live, mirror-out copies the login back, and the stateless actor is thrown away.">
    <template #default="{ at }">
      <Wires :wires="wires(at)" :width="W" :height="H" />

      <Machine v-bind="HOST" role="host" icon="mingcute:server-2-line" title="Control Host" sub="host:" :focus="STEPS[at]!.copy !== 'idle' ? 'focus' : 'idle'">
        <ul class="mono m-0 mt-[4px] flex list-none flex-col gap-[6px] p-0 text-[12.5px]">
          <li v-for="f in STEPS[at]!.host" :key="f.path" :class="fileClass(f.state)">
            <Icon name="mingcute:file-code-line" />{{ f.path }}
          </li>
        </ul>
      </Machine>

      <Pod v-bind="ACTOR" logo="firecracker" driver="actor:" :focus="STEPS[at]!.gone ? 'gone' : 'idle'">
        <ul class="mono m-0 mt-[4px] flex list-none flex-col gap-[6px] p-0 text-[12.5px]">
          <li v-for="f in STEPS[at]!.actor" :key="f.path" :class="fileClass(f.state)">
            <Icon :name="f.state === 'mount' ? 'mingcute:folder-open-line' : 'mingcute:file-code-line'" />{{ f.path }}
          </li>
        </ul>
        <p v-if="STEPS[at]!.mapping" :key="at" class="mono mx-0 mb-0 mt-auto text-[12.5px] text-muted animate-[rise-4_0.35s_cubic-bezier(0.2,0,0,1)_both]">
          {{ STEPS[at]!.mapping }}
        </p>
      </Pod>

      <Machine v-bind="EXEC" icon="mingcute:cube-line" title="Container" name="build" sub="executor:build:" badge="Stateful" :focus="STEPS[at]!.mount ? 'focus' : 'idle'">
        <ul class="mono m-0 mt-[4px] flex list-none flex-col gap-[6px] p-0 text-[12.5px]">
          <li v-for="f in STEPS[at]!.exec" :key="f.path" :class="fileClass(f.state)">
            <Icon name="mingcute:file-code-line" />{{ f.path }}
          </li>
        </ul>
      </Machine>

      <p v-if="STEPS[at]!.gone" class="absolute top-[180px] m-0 text-center text-[15px] text-muted animate-[rise-4_0.4s_0.3s_both]" :style="{ left: `${ACTOR.x}px`, width: `${ACTOR.w}px` }">
        The next run starts from the image again.
      </p>

      <Packets :packets="STEPS[at]!.packets" />
    </template>
  </Frame>
</template>
