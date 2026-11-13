<script setup lang="ts">
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuPortal, DropdownMenuRoot, DropdownMenuTrigger, SelectContent, SelectItem, SelectItemText, SelectPortal, SelectRoot, SelectTrigger, SelectValue, SelectViewport } from 'reka-ui'
import { parseDocument } from 'yaml'
import { computed, onMounted, ref, watch } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import { argvOf, decide } from '../route/rules'
import { check } from './check'
import Editor from './Editor.vue'
import { viewOf } from './model'
import ProfileView from './ProfileView.vue'

/**
 * The profile studio: paste or write a profile, see what needy would reject, see the run it sets up, and try where a
 * command would go. The examples are the repository's own `examples/*\/profile.yaml` and `examples/agents/*.yaml`, read
 * at build time.
 */

const EXAMPLES = import.meta.glob(['../../../../examples/*/profile.yaml', '../../../../examples/agents/*.yaml', '../../../../examples/linux-mac/*.yaml'], { query: '?raw', import: 'default', eager: true }) as Record<string, string>

/** The README's "Pick a profile" blocks: one per actor driver. */
const DRIVER_PROFILES: Record<string, string> = {
  'kvm': `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-firecracker
  recipe: builtin:gce-ubuntu-22.04
  command: [bash]
executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "python3 **", to: build }
`,
  'container': `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-container
  container: { oci_runtime: runc }
  recipe: builtin:gce-ubuntu-22.04
  command: [bash]
executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "python3 **", to: build }
`,
  'macos-vz': `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: macos-vz
  command: [bash]
executors:
  mac: { driver: native, workspace: /tmp/needy-demo }
commands:
  - { match: "python3 **", to: mac }
`,
  'apple-container': `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: macos-apple-container
  command: [bash]
executors:
  mac: { driver: native, workspace: /tmp/needy-demo }
commands:
  - { match: "python3 **", to: mac }
`,
}

const PRESETS: Array<{ id: string, label: string, text: string }> = [
  { id: 'kvm', label: 'README · Linux with KVM', text: DRIVER_PROFILES.kvm! },
  { id: 'container', label: 'README · Linux without KVM', text: DRIVER_PROFILES.container! },
  { id: 'macos-vz', label: 'README · Mac (macos-vz)', text: DRIVER_PROFILES['macos-vz']! },
  { id: 'apple-container', label: 'README · Mac (Apple container)', text: DRIVER_PROFILES['apple-container']! },
  ...Object.entries(EXAMPLES).map(([path, text]) => {
    // `examples/NAME/profile.yaml` is `examples/NAME`; an agent profile keeps its file name.
    const name = path.endsWith('/profile.yaml') ? path.split('/').at(-2)! : path.split('/').slice(-2).join('/')
    return { id: `examples/${name}`, label: `examples/${name}`, text }
  }),
]

const preset = ref('examples/linux-mac/bash.yaml')
const text = ref(PRESETS.find(p => p.id === preset.value)?.text ?? DRIVER_PROFILES.kvm!)

watch(preset, (id) => {
  const found = PRESETS.find(p => p.id === id)
  if (found)
    text.value = found.text
})

const checked = computed(() => check(text.value))
const errors = computed(() => checked.value.findings)
// The last profile that parsed, so the drawing does not vanish while a line is half typed.
const lastView = ref(viewOf({ actor: {} }))
const view = computed(() => {
  if (checked.value.data)
    lastView.value = viewOf(checked.value.data)
  return lastView.value
})

const command = ref('python3 -m pytest -q')
const decision = computed(() => command.value.trim() ? decide(view.value.rules, argvOf(command.value)) : undefined)
const tries = computed(() => [...new Set(view.value.rules.map(r => r.match.replace(/\s*\*\*$/, '').replace(/^\{([^,}]+).*?\}/, '$1').replace(/\s\*$/, ' x')))].slice(0, 5).concat('ls -la'))

/** Line and column of an offset, for the findings list. */
function where(offset: number): string {
  const before = text.value.slice(0, offset).split('\n')
  return `${before.length}:${before.at(-1)!.length + 1}`
}

// Adding parts: each edit goes through the YAML document, so the rest of the text keeps its comments and layout.
function edit(change: (doc: ReturnType<typeof parseDocument>) => void): void {
  const doc = parseDocument(text.value)
  if (doc.errors.length)
    return
  change(doc)
  text.value = doc.toString({ flowCollectionPadding: true })
  preset.value = ''
}

function fresh(doc: ReturnType<typeof parseDocument>, base: string, path: string[]): string {
  let name = base
  for (let i = 2; doc.hasIn([...path, name]); i++)
    name = `${base}-${i}`
  return name
}

function flowNode(doc: ReturnType<typeof parseDocument>, value: unknown) {
  const node = doc.createNode(value)
  ;(node as { flow?: boolean }).flow = true
  return node
}

const firstExecutor = (doc: ReturnType<typeof parseDocument>): string => {
  const executors = doc.toJS()?.executors
  return executors ? Object.keys(executors)[0] ?? 'actor' : 'actor'
}

const ADD = [
  { label: { en: 'Container executor', zh: '容器 executor' }, run: () => edit(doc => doc.setIn(['executors', fresh(doc, 'build', ['executors'])], flowNode(doc, { driver: 'linux-container', key: '{run}' }))) },
  { label: { en: 'Executor on a runner', zh: 'Runner 上的 executor' }, run: () => edit(doc => doc.setIn(['executors', fresh(doc, 'mac', ['executors'])], flowNode(doc, { runner: 'mac', workspace: '~/work' }))) },
  { label: { en: 'Rule: route a command', zh: '规则：路由命令' }, run: () => edit(doc => doc.addIn(['commands'], flowNode(doc, { match: 'cargo **', to: firstExecutor(doc) }))) },
  { label: { en: 'Rule: deny a command', zh: '规则：拒绝命令' }, run: () => edit(doc => doc.addIn(['commands'], flowNode(doc, { match: 'git push **', deny: 'no pushes from the agent' }))) },
  { label: { en: 'Workspace mount', zh: '挂载工作区' }, run: () => edit((doc) => {
    const exec = firstExecutor(doc)
    doc.setIn(['files', fresh(doc, 'workspace', ['files'])], doc.createNode({ mode: 'mount', from: `executor:${exec}:~/workspace/`, to: 'actor:~/workspace/', writable: true }))
    doc.addIn(['run_files'], 'workspace')
  }) },
  { label: { en: 'Allow a host', zh: '放行一个主机' }, run: () => edit(doc => doc.addIn(['actor', 'network', 'allow'], 'registry.npmjs.org')) },
  { label: { en: 'Exec fault', zh: '命令故障' }, run: () => edit((doc) => {
    if (!doc.hasIn(['chaos', 'seed']))
      doc.setIn(['chaos', 'seed'], 1)
    doc.setIn(['chaos', 'faults', fresh(doc, 'curl-down', ['chaos', 'faults'])], flowNode(doc, { exec: 'curl **', exit: 7 }))
  }) },
]

const copied = ref(false)
async function copy(): Promise<void> {
  await navigator.clipboard.writeText(text.value)
  copied.value = true
  setTimeout(() => (copied.value = false), 1400)
}

/** A link that opens the studio with this profile, in the URL's hash. */
function share(): void {
  const bytes = new TextEncoder().encode(text.value)
  const encoded = btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
  history.replaceState(null, '', `#profile=${encoded}`)
  void navigator.clipboard.writeText(location.href)
}

onMounted(() => {
  const encoded = new URLSearchParams(location.hash.slice(1)).get('profile')
  if (!encoded)
    return
  try {
    const bytes = Uint8Array.from(atob(encoded.replace(/-/g, '+').replace(/_/g, '/')), c => c.charCodeAt(0))
    text.value = new TextDecoder().decode(bytes)
    preset.value = ''
  }
  catch {
    // A broken link leaves the example in place.
  }
})
</script>

<template>
  <div class="grid items-start gap-[16px] grid-cols-[minmax(0,0.9fr)_minmax(0,1.4fr)] max-[1100px]:grid-cols-[minmax(0,1fr)]">
    <div class="sticky top-[72px] flex min-w-0 flex-col gap-[8px] max-[1100px]:static">
      <div class="flex flex-wrap gap-[6px]">
        <SelectRoot v-model="preset">
          <SelectTrigger class="inline-flex max-w-[260px] items-center gap-[6px] border-0 rounded-[12px] px-[12px] py-[7px] bg-surface text-[14px] text-ink cursor-pointer hover:bg-control-2" :aria-label="tr({ en: 'Start from', zh: '从示例开始' })">
            <Icon name="mingcute:file-code-line" />
            <SelectValue :placeholder="tr({ en: 'Your profile', zh: '你的 profile' })" />
            <Icon name="mingcute:down-line" />
          </SelectTrigger>
          <SelectPortal>
            <!-- Menus live in a portal, so their utilities sit on the content and items reka-ui renders there. -->
            <SelectContent class="z-50 max-h-[360px] overflow-auto rounded-[12px] p-[4px] bg-surface [box-shadow:var(--shadow-card)]" position="popper" :side-offset="6">
              <SelectViewport>
                <SelectItem v-for="p in PRESETS" :key="p.id" :value="p.id" class="mono rounded-[8px] px-[12px] py-[7px] text-[13.5px] text-ink cursor-pointer data-[highlighted]:bg-control">
                  <SelectItemText>{{ p.label }}</SelectItemText>
                </SelectItem>
              </SelectViewport>
            </SelectContent>
          </SelectPortal>
        </SelectRoot>

        <DropdownMenuRoot>
          <DropdownMenuTrigger class="inline-flex items-center gap-[6px] border-0 rounded-[12px] px-[12px] py-[7px] bg-surface text-[14px] text-ink cursor-pointer hover:bg-control-2">
            <Icon name="mingcute:add-line" /> {{ tr({ en: 'Add', zh: '添加' }) }}
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent class="z-50 max-h-[360px] overflow-auto rounded-[12px] p-[4px] bg-surface [box-shadow:var(--shadow-card)]" :side-offset="6" align="start">
              <DropdownMenuItem v-for="a in ADD" :key="a.label.en" class="rounded-[8px] px-[12px] py-[7px] text-[13.5px] text-ink cursor-pointer data-[highlighted]:bg-control" @select="a.run">
                {{ tr(a.label) }}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>

        <span class="flex-1" />
        <button type="button" class="inline-flex items-center gap-[6px] border-0 rounded-[12px] px-[12px] py-[7px] bg-surface text-[14px] text-ink cursor-pointer hover:bg-control-2" @click="copy">
          <Icon :name="copied ? 'mingcute:check-line' : 'mingcute:file-code-line'" /> {{ copied ? tr({ en: 'Copied', zh: '已复制' }) : tr({ en: 'Copy', zh: '复制' }) }}
        </button>
        <button type="button" class="inline-flex items-center gap-[6px] border-0 rounded-[12px] px-[12px] py-[7px] bg-surface text-[14px] text-ink cursor-pointer hover:bg-control-2" @click="share">
          <Icon name="mingcute:git-branch-line" /> {{ tr({ en: 'Link', zh: '链接' }) }}
        </button>
      </div>

      <div class="h-[460px] overflow-hidden rounded-[12px] bg-surface">
        <Editor v-model="text" />
      </div>

      <div
        class="rounded-[12px] px-[12px] py-[10px] text-[13.5px]"
        :class="errors.length ? 'bg-[color-mix(in_srgb,var(--bad)_12%,var(--surface))] text-ink' : 'flex items-center gap-[6px] bg-[color-mix(in_srgb,var(--executor)_12%,var(--surface))] text-executor'"
      >
        <template v-if="!errors.length">
          <Icon name="mingcute:check-line" /> {{ tr({ en: 'needy would load this profile.', zh: 'needy 可以加载这个 profile。' }) }}
        </template>
        <ul v-else class="m-0 flex list-none flex-col gap-[4px] p-0">
          <li v-for="(f, i) in errors.slice(0, 5)" :key="i">
            <code class="text-[12px] text-bad">{{ where(f.from) }}</code> {{ f.message }}
          </li>
        </ul>
      </div>
    </div>

    <div class="flex min-w-0 flex-col gap-[8px]">
      <label class="flex items-center gap-[8px] rounded-[12px] py-[6px] pl-[14px] pr-[6px] bg-term">
        <span class="mono text-actor-term">$</span>
        <input v-model="command" class="mono min-w-0 flex-1 border-0 px-0 py-[6px] bg-transparent text-[14.5px] text-term-ink [outline:none]" spellcheck="false" :aria-label="tr({ en: 'Try a command', zh: '试一条命令' })">
      </label>
      <div class="flex flex-wrap items-center gap-[6px]">
        <span class="text-[13px] text-muted">{{ tr({ en: 'Try:', zh: '试试：' }) }}</span>
        <button v-for="c in tries" :key="c" type="button" class="mono border-0 rounded-[999px] px-[10px] py-[4px] bg-surface text-[12.5px] text-ink cursor-pointer hover:bg-control-2" @click="command = c">
          {{ c }}
        </button>
      </div>
      <ProfileView :view="view" :decision="decision" />
    </div>
  </div>
</template>
