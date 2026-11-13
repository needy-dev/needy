<script setup lang="ts">
import type { DriverId, Level } from '../data/drivers'
import type { Step } from '../kit/player'

import Frame from '../kit/Frame.vue'
import LevelMark from '../kit/LevelMark.vue'
import Logo from '../kit/Logo.vue'

import { DRIVERS, GROUPS, LEVELS } from '../data/drivers'

/**
 * The four actor drivers and what each supports, as the README's "Actor drivers" table states it
 * (src/data/drivers.ts). A spotlight sweeps the rows: routing works on every driver, and the identity a recipe can
 * present depends on the driver.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 720

const COL_X = 384
const COL_W = 250
const COL_GAP = 10
const colX = (i: number) => COL_X + i * (COL_W + COL_GAP)
const HEAD = { y: 22, h: 200 }

const GROUP_H = 30
const ROW_H = 22
const GROUP_GAP = 8

/** Every row of the matrix with its place: group headers and features. */
const ROWS = (() => {
  const rows: Array<{ kind: 'group', id: string, label: string, y: number } | { kind: 'feature', id: string, group: string, label: string, cells: { level: Level, note?: string }[], y: number }> = []
  let y = HEAD.y + HEAD.h + 14
  for (const group of GROUPS) {
    rows.push({ kind: 'group', id: group.id, label: group.label, y })
    y += GROUP_H
    for (const f of group.features) {
      rows.push({ kind: 'feature', id: f.id, group: group.id, label: f.label, cells: f.cells, y })
      y += ROW_H
    }
    y += GROUP_GAP
  }
  return rows
})()


interface DriversStep extends Step {
  /** The features in the spotlight, as `group/id`, or a whole group as `group/*`. */
  rows: string[]
  /** The drivers in the spotlight; none means every column. */
  cols?: DriverId[]
  note?: { text: string, row: string, col: number }
  headers?: boolean
}

const STEPS: DriversStep[] = [
  { ms: 3000, rows: [], headers: true, caption: 'actor.driver picks where the actor runs; the prefix is the OS whose isolation it relies on.' },
  { ms: 2800, rows: ['routing/*'], caption: 'Routing works on every driver: by name, by path, through the gateway, with files and faults.' },
  { ms: 3000, rows: ['routing/runtime'], cols: ['linux-firecracker', 'macos-vz'], note: { text: 'Needs binfmt_needy in the kernel', row: 'routing/runtime', col: 2 }, caption: 'Only a kernel capture catches programs added at run time; shims cover the image\'s PATH.' },
  { ms: 3000, rows: ['routing/interactive'], cols: ['linux-container'], note: { text: 'Expected, not yet recorded', row: 'routing/interactive', col: 1 }, caption: 'An interactive container actor is expected to work, but no run has recorded it yet.' },
  { ms: 2800, rows: ['identity/*'], note: { text: '/proc/cpuinfo under macos-vz: not possible on arm64', row: 'identity/cpuinfo', col: 2 }, caption: 'The recipe\'s identity depends on what the driver can present.' },
  { ms: 3000, rows: ['identity/cpuid', 'identity/pci'], cols: ['linux-firecracker'], note: { text: 'CPUID and PCI IDs: only linux-firecracker', row: 'identity/cpuid', col: 0 }, caption: 'CPUID and PCI IDs need needy\'s patched Firecracker.' },
  { ms: 3000, rows: ['identity/dmi', 'identity/hwmon'], cols: ['linux-container'], note: { text: 'linux-container: where the host has DMI or hwmon; any host under runsc', row: 'identity/dmi', col: 1 }, caption: 'A container shows DMI and sensors where the host has them, and on any host under gVisor.' },
  { ms: 3200, rows: ['identity/kernel', 'identity/uptime'], cols: ['macos-vz'], note: { text: 'macos-vz: expected, not yet recorded. linux-container shows the host\'s or gVisor\'s kernel', row: 'identity/kernel', col: 2 }, caption: 'needy profile check lists every gap for your recipe and host.' },
]

function rowState(at: number, row: (typeof ROWS)[number]): 'idle' | 'focus' | 'dim' {
  const lit = STEPS[at]!.rows
  if (lit.length === 0)
    return STEPS[at]!.headers ? 'dim' : 'idle'
  const id = row.kind === 'group' ? `${row.id}/*` : `${row.group}/${row.id}`
  const on = lit.includes(id) || (row.kind === 'feature' && lit.includes(`${row.group}/*`)) || (row.kind === 'group' && lit.some(l => l.startsWith(`${row.id}/`)))
  return on ? 'focus' : 'dim'
}

function colState(at: number, i: number): 'idle' | 'focus' | 'dim' {
  const step = STEPS[at]!
  if (step.headers)
    return 'focus'
  if (!step.cols)
    return 'idle'
  return step.cols.includes(DRIVERS[i]!.id) ? 'focus' : 'dim'
}

function cellLit(at: number, row: (typeof ROWS)[number], i: number): boolean {
  return rowState(at, row) === 'focus' && colState(at, i) !== 'dim'
}

</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Actor drivers: linux-firecracker, linux-container, macos-vz and macos-apple-container, with what each supports for routing and for the recipe's identity, from the README's table.">
    <template #default="{ at }">
      <div class="absolute top-[26px] left-[40px] w-[316px]">
        <h2 class="display m-0 text-[30px]">
          Actor Drivers
        </h2>
        <p class="mx-0 mb-0 mt-[8px] text-[15px] text-ink-2 leading-[1.45] [&_code]:text-[13px]">
          Where the actor runs. Pick one with <code>actor.driver</code>.
        </p>
        <ul class="mx-0 mb-0 mt-[14px] flex list-none flex-wrap gap-x-[14px] gap-y-[6px] p-0 text-[13px] text-ink-2">
          <li v-for="(l, level) in LEVELS" :key="level" class="flex items-center gap-[6px]">
            <LevelMark :level="level" :size="15" />{{ l.label }}
          </li>
        </ul>
        <p v-if="STEPS[at]!.note" :key="`n${at}`" class="mx-0 mb-0 mt-[12px] rounded-[10px] px-[12px] py-[6px] text-[13.5px] text-actor bg-actor/13 animate-[rise-4_0.4s_cubic-bezier(0.2,0,0,1)_both]">
          {{ STEPS[at]!.note!.text }}
        </p>
      </div>

      <div
        v-for="(d, i) in DRIVERS" :key="d.id" class="f absolute flex flex-col gap-[4px] rounded-[14px] px-[16px] py-[14px] bg-surface" :class="colState(at, i)"
        :style="{ 'left': `${colX(i)}px`, 'top': `${HEAD.y}px`, 'width': `${COL_W}px`, 'height': `${HEAD.h}px`, '--role': 'var(--actor)' }"
      >
        <span class="mb-[4px] flex items-center gap-[8px]">
          <Logo v-for="l in d.logos" :key="l" :name="l" :size="30" />
          <span v-if="d.ready" class="mono ml-auto rounded-[999px] px-[8px] py-[2px] text-[12px] text-actor bg-actor/12">{{ d.ready }}</span>
        </span>
        <span class="display whitespace-nowrap text-[19px]">{{ d.title }}</span>
        <code class="text-[13px] text-actor">{{ d.id }}</code>
        <span class="text-[12.5px] text-ink-2 leading-[1.35]">{{ d.host }}</span>
        <span class="text-[12.5px] text-muted leading-[1.35]">{{ d.isolation }}</span>
        <span class="mt-auto text-[12.5px] text-muted leading-[1.35]">Catches execs with {{ d.capture }}</span>
      </div>

      <template v-for="row in ROWS" :key="`${row.kind}-${row.id}`">
        <div v-if="row.kind === 'group'" class="f absolute left-[40px] flex h-[30px] w-[1378px] items-center pl-[8px] text-[16px] text-ink [&.focus]:(!bg-transparent ![transform:none])" :class="rowState(at, row)" :style="{ top: `${row.y}px` }">
          <span class="display">{{ row.label }}</span>
        </div>
        <div v-else class="f absolute left-[40px] h-[22px] w-[1378px] [&.focus]:(rounded-[6px] !bg-actor/7 ![transform:none])" :class="rowState(at, row)" :style="{ 'top': `${row.y}px`, '--role': 'var(--actor)' }">
          <span class="absolute top-0 left-[8px] whitespace-nowrap text-[13.5px] text-ink-2 leading-[22px]">{{ row.label }}</span>
          <span
            v-for="(cell, i) in row.cells" :key="i" class="absolute top-0 flex h-[22px] items-center justify-center [transition:transform_0.4s_cubic-bezier(0.2,0,0,1),opacity_0.4s]"
            :class="[cellLit(at, row, i) && '[transform:scale(1.25)]', colState(at, i) === 'dim' && rowState(at, row) === 'focus' && 'opacity-30']"
            :style="{ left: `${colX(i) - 40}px`, width: `${COL_W}px` }"
          >
            <LevelMark :level="cell.level" :size="16" />
          </span>
        </div>
      </template>

    </template>
  </Frame>
</template>
