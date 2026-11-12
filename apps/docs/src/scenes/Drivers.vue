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
      <div class="intro">
        <h2 class="display">
          Actor Drivers
        </h2>
        <p class="lead">
          Where the actor runs. Pick one with <code>actor.driver</code>.
        </p>
        <ul class="legend">
          <li v-for="(l, level) in LEVELS" :key="level">
            <LevelMark :level="level" :size="15" />{{ l.label }}
          </li>
        </ul>
        <p v-if="STEPS[at]!.note" :key="`n${at}`" class="callout">
          {{ STEPS[at]!.note!.text }}
        </p>
      </div>

      <div
        v-for="(d, i) in DRIVERS" :key="d.id" class="head f" :class="colState(at, i)"
        :style="{ 'left': `${colX(i)}px`, 'top': `${HEAD.y}px`, 'width': `${COL_W}px`, 'height': `${HEAD.h}px`, '--role': 'var(--actor)' }"
      >
        <span class="logos">
          <Logo v-for="l in d.logos" :key="l" :name="l" :size="30" />
          <span v-if="d.ready" class="ready mono">{{ d.ready }}</span>
        </span>
        <span class="title display">{{ d.title }}</span>
        <code class="id">{{ d.id }}</code>
        <span class="fact">{{ d.host }}</span>
        <span class="fact muted">{{ d.isolation }}</span>
        <span class="fact capture">Catches execs with {{ d.capture }}</span>
      </div>

      <template v-for="row in ROWS" :key="`${row.kind}-${row.id}`">
        <div v-if="row.kind === 'group'" class="group f" :class="rowState(at, row)" :style="{ top: `${row.y}px` }">
          <span class="display">{{ row.label }}</span>
        </div>
        <div v-else class="row f" :class="rowState(at, row)" :style="{ 'top': `${row.y}px`, '--role': 'var(--actor)' }">
          <span class="label">{{ row.label }}</span>
          <span
            v-for="(cell, i) in row.cells" :key="i" class="cell" :class="{ lit: cellLit(at, row, i), off: colState(at, i) === 'dim' && rowState(at, row) === 'focus' }"
            :style="{ left: `${colX(i) - 40}px`, width: `${COL_W}px` }"
          >
            <LevelMark :level="cell.level" :size="16" />
          </span>
        </div>
      </template>

    </template>
  </Frame>
</template>

<style scoped>
.intro {
  position: absolute;
  top: 26px;
  left: 40px;
  width: 316px;
}

h2 {
  margin: 0;
  font-size: 30px;
}

.lead {
  margin: 8px 0 0;
  color: var(--ink-2);
  font-size: 15px;
  line-height: 1.45;
}

.lead code {
  font-size: 13px;
}

.legend {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 14px;
  margin: 14px 0 0;
  padding: 0;
  color: var(--ink-2);
  font-size: 13px;
  list-style: none;
}

.legend li {
  display: flex;
  align-items: center;
  gap: 6px;
}

.callout {
  margin: 12px 0 0;
  padding: 6px 12px;
  border-radius: 10px;
  background: color-mix(in srgb, var(--actor) 13%, transparent);
  color: var(--actor);
  font-size: 13.5px;
  animation: arrive 0.4s cubic-bezier(0.2, 0, 0, 1) both;
}

@keyframes arrive {
  from {
    opacity: 0;
    transform: translateY(4px);
  }
}



.head {
  position: absolute;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 14px 16px;
  border-radius: 14px;
  background: var(--surface);
}

.logos {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 4px;
}

.ready {
  margin-left: auto;
  padding: 2px 8px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--actor) 12%, transparent);
  color: var(--actor);
  font-size: 12px;
}

.title {
  font-size: 19px;
  white-space: nowrap;
}

.id {
  color: var(--actor);
  font-size: 13px;
}

.fact {
  color: var(--ink-2);
  font-size: 12.5px;
  line-height: 1.35;
}

.fact.muted {
  color: var(--muted);
}

.fact.capture {
  margin-top: auto;
  color: var(--muted);
}

.group {
  position: absolute;
  left: 40px;
  display: flex;
  width: 1378px;
  height: 30px;
  align-items: center;
  padding-left: 8px;
  color: var(--ink);
  font-size: 16px;
}

.row {
  position: absolute;
  left: 40px;
  width: 1378px;
  height: 22px;
}

.row.focus,
.group.focus {
  transform: none;
}

.group.focus {
  background: transparent;
}

.row.focus {
  background: color-mix(in srgb, var(--actor) 7%, transparent);
  border-radius: 6px;
}

.label {
  position: absolute;
  top: 0;
  left: 8px;
  color: var(--ink-2);
  font-size: 13.5px;
  line-height: 22px;
  white-space: nowrap;
}

.cell {
  position: absolute;
  top: 0;
  display: flex;
  height: 22px;
  align-items: center;
  justify-content: center;
  transition:
    transform 0.4s cubic-bezier(0.2, 0, 0, 1),
    opacity 0.4s;
}

.cell.lit {
  transform: scale(1.25);
}

.cell.off {
  opacity: 0.3;
}

</style>
