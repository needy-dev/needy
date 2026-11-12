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

/** One button of the pickers; an unpicked driver's logos go grey. */
const TOGGLE = 'inline-flex cursor-pointer items-center gap-[6px] rounded-[10px] border-0 bg-transparent px-[12px] py-[6px] text-[14px] text-muted [transition:background_0.2s,color_0.2s] [&_code]:text-[13px] hover:text-ink data-[state=on]:(bg-surface text-ink) [&[data-state=off]_.logo]:(opacity-45 [filter:saturate(0)])'
/** The picker groups: a strip of toggles on the control fill. */
const PICKER = 'flex flex-wrap gap-[4px] rounded-[14px] bg-control p-[4px]'
/**
 * The feature column stays put while the table scrolls sideways. On a phone its labels wrap to two lines, so the
 * cell's own height no longer separates the rows and the label takes padding instead.
 */
const FEATURE = 'sticky left-0 min-w-[220px] bg-surface px-[16px] text-left text-[14px] font-normal text-ink-2 max-[720px]:(!min-w-0 w-[62%])'
/** A row lights up under the pointer, its sticky label too. */
const HOVER = 'group-hover:bg-[color-mix(in_srgb,var(--actor)_7%,var(--surface))]'
/** A level cell; the start time takes the same look. */
const CELL = 'cell flex min-h-[34px] w-full cursor-help items-center gap-[8px] border-0 bg-transparent px-[14px] py-0 text-left text-[12px] text-muted'
</script>

<template>
  <TooltipProvider :delay-duration="120">
    <div class="matrix flex flex-col gap-[14px]">
      <div class="flex flex-wrap justify-between gap-[10px]">
        <ToggleGroupRoot v-if="narrow" type="single" :model-value="single" :class="PICKER" aria-label="Driver" @update:model-value="pickOne">
          <ToggleGroupItem v-for="d in DRIVERS" :key="d.id" :value="d.id" :class="TOGGLE">
            <Logo v-for="l in d.logos" :key="l" :name="l" :size="18" />
            <code>{{ d.id }}</code>
          </ToggleGroupItem>
        </ToggleGroupRoot>
        <ToggleGroupRoot v-else type="multiple" :model-value="picked" :class="PICKER" aria-label="Drivers to compare" @update:model-value="pick">
          <ToggleGroupItem v-for="d in DRIVERS" :key="d.id" :value="d.id" :class="TOGGLE">
            <Logo v-for="l in d.logos" :key="l" :name="l" :size="18" />
            <code>{{ d.id }}</code>
          </ToggleGroupItem>
        </ToggleGroupRoot>
        <ToggleGroupRoot type="single" :model-value="group" :class="PICKER" aria-label="Features" @update:model-value="pickGroup">
          <ToggleGroupItem value="all" :class="TOGGLE">
            All
          </ToggleGroupItem>
          <ToggleGroupItem v-for="g in GROUPS" :key="g.id" :value="g.id" :class="TOGGLE">
            {{ g.id === 'routing' ? 'Routing' : 'Identity' }}
          </ToggleGroupItem>
        </ToggleGroupRoot>
      </div>

      <div class="overflow-x-auto rounded-[20px] bg-surface">
        <!-- Every driver column gets the same width, so the headers line up whatever the cells under them say. -->
        <table class="w-full min-w-[720px] table-fixed border-separate border-spacing-0 max-[720px]:min-w-0">
          <thead>
            <tr>
              <th class="w-[28%] py-0" :class="FEATURE" />
              <th v-for="{ d } in columns" :key="d.id" class="min-w-[170px] px-[14px] pb-[14px] pt-[18px] text-left align-top font-normal max-[720px]:!min-w-0">
                <span class="mb-[6px] flex gap-[6px]"><Logo v-for="l in d.logos" :key="l" :name="l" :size="26" /></span>
                <span class="display block text-[17px]">{{ d.title }}</span>
                <code class="mx-0 mb-[6px] mt-[2px] block whitespace-nowrap text-[13px] text-actor max-[720px]:whitespace-normal">{{ d.id }}</code>
              </th>
            </tr>
          </thead>
          <tbody>
            <!-- The start time is the table's first row, apart from the groups under it. -->
            <tr class="group">
              <th class="pb-0 pt-[6px] max-[720px]:(py-[7px] leading-[1.35])" :class="[FEATURE, HOVER]">
                Ready in
              </th>
              <td v-for="{ d } in columns" :key="d.id" class="px-0 pb-0 pt-[6px] text-left font-normal" :class="HOVER">
                <span class="mono" :class="CELL">{{ d.ready ?? '–' }}</span>
              </td>
            </tr>
          </tbody>
          <tbody v-for="g in groups" :key="g.id">
            <tr>
              <th :colspan="columns.length + 1" class="display px-[16px] pb-[6px] pt-[18px] text-left text-[16px] font-normal">
                {{ g.label }}
              </th>
            </tr>
            <tr v-for="f in g.features" :key="f.id" class="group">
              <th class="py-0 max-[720px]:(py-[7px] leading-[1.35])" :class="[FEATURE, HOVER]">
                {{ f.label }}
              </th>
              <td v-for="{ d, i } in columns" :key="d.id" class="p-0 text-left font-normal" :class="HOVER">
                <TooltipRoot>
                  <TooltipTrigger as-child>
                    <button type="button" :class="CELL" :aria-label="`${d.id}, ${f.label}: ${LEVELS[f.cells[i]!.level].label}`">
                      <LevelMark :level="f.cells[i]!.level" />
                      <span v-if="f.cells[i]!.note" class="leading-[1.3]">{{ f.cells[i]!.note }}</span>
                    </button>
                  </TooltipTrigger>
                  <TooltipPortal>
                    <TooltipContent class="z-50 max-w-[320px] rounded-[12px] bg-tip-bg px-[12px] py-[10px] text-[13.5px] text-tip-ink leading-[1.45] [box-shadow:0_10px_28px_rgb(0_0_0/22%)] [&_code]:text-[12.5px] [&_a]:(mt-[4px] block text-inherit)" side="top" :side-offset="6">
                      <strong>{{ LEVELS[f.cells[i]!.level].label }}</strong>: {{ f.label }} on <code>{{ d.id }}</code> is {{ LEVELS[f.cells[i]!.level].meaning }}<template v-if="f.cells[i]!.note"> ({{ f.cells[i]!.note }})</template>.
                      <a :href="d.evidence.url" target="_blank" rel="noopener">{{ d.evidence.label }}</a>
                      <TooltipArrow class="fill-tip-bg" :width="10" :height="5" />
                    </TooltipContent>
                  </TooltipPortal>
                </TooltipRoot>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <p class="m-0 flex flex-wrap gap-x-[20px] gap-y-[8px] text-[13.5px] text-muted">
        <span v-for="(l, level) in LEVELS" :key="level" class="inline-flex items-center gap-[7px]"><LevelMark :level="level" :size="15" />{{ l.label }}: {{ l.meaning }}</span>
      </p>
    </div>
  </TooltipProvider>
</template>
