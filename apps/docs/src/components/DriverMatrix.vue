<script setup lang="ts">
import type { DriverId, Group } from '../data/drivers'

import { ToggleGroupItem, ToggleGroupRoot, TooltipArrow, TooltipContent, TooltipPortal, TooltipProvider, TooltipRoot, TooltipTrigger } from 'reka-ui'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

import LevelMark from '../kit/LevelMark.vue'
import Logo from '../kit/Logo.vue'

import { DRIVERS, GROUPS, LEVELS } from '../data/drivers'

/**
 * The actor drivers side by side: pick the drivers to compare and a group of features; each cell says its level and
 * links the evidence the README cites. The data is src/data/drivers.ts, the README's "Actor drivers" table.
 */

const picked = ref<DriverId[]>(DRIVERS.map(d => d.id))
const group = ref<'all' | Group['id']>('all')

/**
 * A phone has room for one driver beside the features, so there the picker chooses one driver and the table shows
 * that one; wider, it compares any of them.
 */
const narrow = ref(false)
let query: MediaQueryList | undefined
const onQuery = () => (narrow.value = query?.matches ?? false)
const single = ref<DriverId>(DRIVERS[0]!.id)
onMounted(() => {
  query = matchMedia('(max-width: 720px)')
  onQuery()
  query.addEventListener('change', onQuery)
})
onBeforeUnmount(() => query?.removeEventListener('change', onQuery))

const columns = computed(() => DRIVERS.map((d, i) => ({ d, i })).filter(({ d }) => narrow.value ? d.id === single.value : picked.value.includes(d.id)))

function pickOne(value: unknown): void {
  if (value)
    single.value = value as DriverId
}
const groups = computed(() => GROUPS.filter(g => group.value === 'all' || g.id === group.value))

/** At least one driver stays picked, so the table never empties. */
function pick(value: unknown): void {
  const next = value as DriverId[]
  if (next.length > 0)
    picked.value = next
}

function pickGroup(value: unknown): void {
  if (value)
    group.value = value as typeof group.value
}
</script>

<template>
  <TooltipProvider :delay-duration="120">
    <div class="matrix">
      <div class="bar">
        <ToggleGroupRoot v-if="narrow" type="single" :model-value="single" class="drivers" aria-label="Driver" @update:model-value="pickOne">
          <ToggleGroupItem v-for="d in DRIVERS" :key="d.id" :value="d.id" class="toggle">
            <Logo v-for="l in d.logos" :key="l" :name="l" :size="18" />
            <code>{{ d.id }}</code>
          </ToggleGroupItem>
        </ToggleGroupRoot>
        <ToggleGroupRoot v-else type="multiple" :model-value="picked" class="drivers" aria-label="Drivers to compare" @update:model-value="pick">
          <ToggleGroupItem v-for="d in DRIVERS" :key="d.id" :value="d.id" class="toggle">
            <Logo v-for="l in d.logos" :key="l" :name="l" :size="18" />
            <code>{{ d.id }}</code>
          </ToggleGroupItem>
        </ToggleGroupRoot>
        <ToggleGroupRoot type="single" :model-value="group" class="groups" aria-label="Features" @update:model-value="pickGroup">
          <ToggleGroupItem value="all" class="toggle">
            All
          </ToggleGroupItem>
          <ToggleGroupItem v-for="g in GROUPS" :key="g.id" :value="g.id" class="toggle">
            {{ g.id === 'routing' ? 'Routing' : 'Identity' }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>

      <div class="scroll">
        <table>
          <thead>
            <tr>
              <th class="feature" />
              <th v-for="{ d } in columns" :key="d.id" class="driver">
                <span class="logos"><Logo v-for="l in d.logos" :key="l" :name="l" :size="26" /></span>
                <span class="title display">{{ d.title }}</span>
                <code class="id">{{ d.id }}</code>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr class="row start">
              <th class="feature">
                Ready in
              </th>
              <td v-for="{ d } in columns" :key="d.id">
                <span class="cell ready mono">{{ d.ready ?? '–' }}</span>
              </td>
            </tr>
          </tbody>
          <tbody v-for="g in groups" :key="g.id">
            <tr class="group">
              <th :colspan="columns.length + 1" class="display">
                {{ g.label }}
              </th>
            </tr>
            <tr v-for="f in g.features" :key="f.id" class="row">
              <th class="feature">
                {{ f.label }}
              </th>
              <td v-for="{ d, i } in columns" :key="d.id">
                <TooltipRoot>
                  <TooltipTrigger as-child>
                    <button type="button" class="cell" :aria-label="`${d.id}, ${f.label}: ${LEVELS[f.cells[i]!.level].label}`">
                      <LevelMark :level="f.cells[i]!.level" />
                      <span v-if="f.cells[i]!.note" class="note">{{ f.cells[i]!.note }}</span>
                    </button>
                  </TooltipTrigger>
                  <TooltipPortal>
                    <TooltipContent class="nd-tip" side="top" :side-offset="6">
                      <strong>{{ LEVELS[f.cells[i]!.level].label }}</strong>: {{ f.label }} on <code>{{ d.id }}</code> is {{ LEVELS[f.cells[i]!.level].meaning }}<template v-if="f.cells[i]!.note"> ({{ f.cells[i]!.note }})</template>.
                      <a :href="d.evidence.url" target="_blank" rel="noopener">{{ d.evidence.label }}</a>
                      <TooltipArrow class="arrow" :width="10" :height="5" />
                    </TooltipContent>
                  </TooltipPortal>
                </TooltipRoot>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <p class="legend">
        <span v-for="(l, level) in LEVELS" :key="level"><LevelMark :level="level" :size="15" />{{ l.label }}: {{ l.meaning }}</span>
      </p>
    </div>
  </TooltipProvider>
</template>

<style scoped>
.matrix {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.bar {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  gap: 10px;
}

.drivers,
.groups {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  padding: 4px;
  border-radius: 14px;
  background: var(--control);
}

.toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border: 0;
  border-radius: 10px;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  font-size: 14px;
  transition:
    background 0.2s,
    color 0.2s;
}

.toggle code {
  font-size: 13px;
}

.toggle:hover {
  color: var(--ink);
}

.toggle[data-state='on'] {
  background: var(--surface);
  color: var(--ink);
}

.toggle[data-state='off'] :deep(.logo) {
  opacity: 0.45;
  filter: saturate(0);
}

.scroll {
  overflow-x: auto;
  border-radius: 20px;
  background: var(--surface);
}

table {
  width: 100%;
  min-width: 720px;
  /* Every driver column gets the same width, so the headers line up whatever the cells under them say. */
  table-layout: fixed;
}

thead .feature {
  width: 28%;
}

@media (max-width: 720px) {
  table {
    min-width: 0;
  }

  /* More specific than the wide rules below, which come later. */
  table thead .feature {
    width: 62%;
  }

  .driver .id {
    white-space: normal;
  }

  .feature {
    min-width: 0 !important;
    width: 62%;
  }

  .row .feature {
    /* Labels wrap to two lines here; the cell's own height no longer separates the rows. */
    padding-block: 7px;
    line-height: 1.35;
  }

  .driver {
    min-width: 0 !important;
  }
}

table {
  border-collapse: separate;
  border-spacing: 0;
}

th,
td {
  padding: 0;
  font-weight: 400;
  text-align: left;
}

.driver {
  min-width: 170px;
  padding: 18px 14px 14px;
  vertical-align: top;
}

.driver > span,
.driver > code {
  display: block;
}

.logos {
  display: flex !important;
  gap: 6px;
  margin-bottom: 6px;
}

.title {
  font-size: 17px;
}

.id {
  margin: 2px 0 6px;
  color: var(--actor);
  font-size: 13px;
  white-space: nowrap;
}

.fact {
  color: var(--ink-2);
  font-size: 13px;
  line-height: 1.4;
}

.fact.muted {
  margin-top: 4px;
  color: var(--muted);
}

.ready {
  color: var(--ink);
  cursor: default;
  font-size: 14px;
}

/* The start time is the table's first row, apart from the groups under it. */
.start th,
.start td {
  padding-top: 6px;
}

.group th {
  padding: 18px 16px 6px;
  font-size: 16px;
}

.feature {
  position: sticky;
  left: 0;
  min-width: 220px;
  padding: 0 16px;
  background: var(--surface);
  color: var(--ink-2);
  font-size: 14px;
}

.row:hover th,
.row:hover td {
  background: color-mix(in srgb, var(--actor) 7%, var(--surface));
}

.cell {
  display: flex;
  width: 100%;
  min-height: 34px;
  align-items: center;
  gap: 8px;
  padding: 0 14px;
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: help;
  font-size: 12px;
  text-align: left;
}

.note {
  line-height: 1.3;
}

.legend {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 20px;
  margin: 0;
  color: var(--muted);
  font-size: 13.5px;
}

.legend span {
  display: inline-flex;
  align-items: center;
  gap: 7px;
}
</style>

<style>
/* The tooltip renders in a portal, outside this component's scope. */
.nd-tip {
  z-index: 50;
  max-width: 320px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--tip-bg);
  box-shadow: 0 10px 28px rgb(0 0 0 / 22%);
  color: var(--tip-ink);
  font-size: 13.5px;
  line-height: 1.45;
}

.nd-tip code {
  font-size: 12.5px;
}

.nd-tip a {
  display: block;
  margin-top: 4px;
  color: inherit;
}

.nd-tip .arrow {
  fill: var(--tip-bg);
}
</style>
