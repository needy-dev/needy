<script setup lang="ts">
import type { Text } from '../i18n'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'
import Logo from '../kit/Logo.vue'
import MachineDeck from './MachineDeck.vue'

/**
 * What Needy does, as one picture made of tiles, after the bento rules of the stageshow skill: one hero tile with the
 * only window, a stat, then components, an object, a collection and a diagram, each saying one thing once. Copy sits
 * above each picture in page type. A tile whose claim is not fully built says so; the rest are verified end to end in
 * the closed-loop results.
 */
defineEmits<{ go: [id: string] }>()

const EVIDENCE = 'https://github.com/needy-dev/needy/blob/4a996b82f3d26665f24d5838d4415d18c64f881a/docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md'

interface Tile {
  id: string
  more: string
  lead: Text
  rest: Text
  status?: 'partly' | 'vision'
}

const TILES: Record<string, Tile> = {
  hero: { id: 'hero', more: 'decisions', lead: { en: 'Every decision, on record.', zh: '每一次判定都有据可查。' }, rest: { en: 'Routed, denied and faulted commands are typed events; needy run report sums up a run.', zh: '命令被转到哪、被拦了几次、出过什么故障，全都记成事件，needy run report 一眼看完。' } },
  stat: { id: 'stat', more: 'isolation', lead: { en: 'On a Linux KVM host,', zh: '在 Linux KVM 主机上，' }, rest: { en: 'Needy boots a Firecracker actor sandbox in', zh: 'Needy 拉起一个 Firecracker actor 沙箱，只要' } },
  rules: { id: 'rules', more: 'decisions', lead: { en: 'Policy per command.', zh: '每条命令都能单独管。' }, rest: { en: 'Route it, keep it, or deny it.', zh: '转出去、留下来，还是直接拦掉。' } },
  identity: { id: 'identity', more: 'identity', lead: { en: 'Any machine you describe.', zh: '想装成什么机器都行。' }, rest: { en: 'A recipe sets the CPU, firmware and devices the program sees.', zh: 'CPU、固件、设备，程序看到的都由 recipe 说了算。' } },
  isolation: { id: 'isolation', more: 'isolation', lead: { en: 'Four ways to isolate.', zh: '四种隔离方式可选。' }, rest: { en: 'A microVM, a container, gVisor, a VM on a Mac.', zh: 'microVM、容器、gVisor，还有 Mac 上的虚拟机。' } },
  chaos: { id: 'chaos', more: 'chaos', lead: { en: 'Chaos engineering for your agent.', zh: '给 agent 来点混沌工程。' }, rest: { en: 'Seeded faults, the same on every replay.', zh: '故障按 seed 注入，重放多少遍都一模一样。' }, status: 'partly' },
  laptop: { id: 'laptop', more: 'cluster', lead: { en: 'A decoupled control plane.', zh: '控制面是单独的。' }, rest: { en: 'The control plane runs on its own; a laptop, a board or a CI job only needs the client to start and manage runs.', zh: '控制面单独部署，笔记本、开发板、CI 只要装个客户端就能用。' } },
  teleport: { id: 'teleport', more: 'future', lead: { en: 'Teleport workloads.', zh: '哪里有机器就用哪里。' }, rest: { en: 'A macOS pool or a render farm, on demand.', zh: '要 Mac 集群就拉 Mac，要渲染农场就拉渲染农场。' }, status: 'vision' },
}

const STATUS: Record<'partly' | 'vision', Text> = {
  partly: { en: 'Partly there', zh: '部分可用' },
  vision: { en: 'Vision', zh: '设想' },
}
</script>

<template>
  <!-- Hero and strip, then two rows of three: rounded tiles on a gap that equals their padding's half.
       NOTICE: the stylesheet this replaced also had phone sizes for the copy (17px), the number (72px) and the
       picture's top margin (24px), but later rules of the same weight overrode them, so the page never showed them.
       They are left out to keep the page as it was. -->
  <div class="grid grid-cols-[repeat(3,minmax(0,1fr))] gap-[16px] max-[860px]:grid-cols-[minmax(0,1fr)]">
    <a
      v-for="t in TILES" :key="t.id"
      class="relative min-h-[300px] flex flex-col overflow-hidden rounded-[24px] px-[28px] pb-0 pt-[28px] text-ink [text-decoration:none] max-[860px]:(min-h-0 px-[22px] pt-[22px])"
      :class="[t.id === 'hero' && '[grid-column:span_2] max-[860px]:[grid-column:auto]', t.id === 'stat' ? 'bg-ink' : 'bg-control']"
      :href="`#${t.more}`"
    >
      <p class="m-0 text-[19px] leading-[1.4]" :class="[t.id === 'hero' ? 'max-w-[48ch]' : 'max-w-[34ch]', t.id === 'stat' ? 'text-paper/62' : 'text-muted']">
        <strong class="font-600" :class="t.id === 'stat' ? 'text-paper' : 'text-ink'">{{ tr(t.lead) }}</strong> {{ tr(t.rest) }}
        <span
          v-if="t.status" class="ml-[6px] inline-block rounded-[999px] px-[9px] py-[2px] align-[2px] text-[12.5px]"
          :class="t.status === 'partly' ? 'bg-fault/16 text-fault' : 'bg-gateway/15 text-gateway'"
        >{{ tr(STATUS[t.status]) }}</span>
      </p>

      <!-- The hero: `needy run report`, the one window of the grid, running off the right and the bottom. The format is
           crates/core/src/report/human.rs; the run ID and the counts are made up for the tile. Its window is inset
           on the copy's gutter, off the right edge and the bottom. -->
      <div v-if="t.id === 'hero'" class="mt-[28px] w-[calc(100%+60px)] rounded-[14px_0_0] bg-term text-term-ink">
        <div class="flex items-center gap-[6px] px-[14px] py-[12px]">
          <i class="h-[10px] w-[10px] rounded-[50%] bg-[rgb(255_255_255/16%)]" /><i class="h-[10px] w-[10px] rounded-[50%] bg-[rgb(255_255_255/16%)]" /><i class="h-[10px] w-[10px] rounded-[50%] bg-[rgb(255_255_255/16%)]" /><span class="mono ml-[10px] text-[12px] text-[rgb(255_255_255/45%)]">needy run report latest</span>
        </div>
        <div class="mono flex flex-col gap-[4px] px-[18px] pb-[24px] pt-[4px] text-[14px] leading-[1.55]">
          <span><span class="inline-block w-[9ch] text-[rgb(255_255_255/55%)]">Run</span>3f9c0e2a71b4  (profile opencode)</span>
          <span><span class="inline-block w-[9ch] text-[rgb(255_255_255/55%)]">State</span>finished, driving program exit 0</span>
          <span class="h-[6px]" />
          <span class="text-[rgb(255_255_255/45%)]">Commands</span>
          <span><span class="inline-block w-[19ch] pl-[2ch] text-[rgb(255_255_255/55%)]">routed to build</span><span class="text-executor-term">14</span></span>
          <span><span class="inline-block w-[19ch] pl-[2ch] text-[rgb(255_255_255/55%)]">kept on actor</span>3</span>
          <span><span class="inline-block w-[19ch] pl-[2ch] text-[rgb(255_255_255/55%)]">denied</span><span class="text-bad-term">1</span></span>
          <span class="h-[6px]" />
          <span class="text-[rgb(255_255_255/45%)]">Findings</span>
          <span class="pl-[2ch]">network denied: example.com (actor network)</span>
        </div>
      </div>

      <!-- The stat: the inverted tile, one number. -->
      <div v-else-if="t.id === 'stat'" class="mx-0 mb-[28px] mt-auto whitespace-nowrap text-[96px] text-paper font-display leading-[1]">
        0.17<span class="ml-[6px] text-[48px]">s</span>
      </div>

      <!-- One component: three rules, the middle one acted on. -->
      <div v-else-if="t.id === 'rules'" class="mb-[-12px] mt-auto flex flex-col gap-[8px] rounded-[16px_16px_0_0] bg-surface p-[12px]">
        <div class="flex items-center justify-between gap-[12px] rounded-[8px] px-[12px] py-[10px] text-[14px]">
          <code>python3 **</code><span class="rounded-[999px] bg-executor/15 px-[10px] py-[2px] text-[12.5px] text-executor font-mono">build</span>
        </div>
        <div class="flex items-center justify-between gap-[12px] rounded-[8px] bg-[color-mix(in_srgb,var(--bad)_10%,var(--surface))] px-[12px] py-[10px] text-[14px]">
          <code>git push **</code><span class="rounded-[999px] bg-bad-fill px-[10px] py-[2px] text-[12.5px] text-[#fff] font-mono">deny</span>
        </div>
        <div class="flex items-center justify-between gap-[12px] rounded-[8px] px-[12px] py-[10px] text-[14px]">
          <code>uname **</code><span class="rounded-[999px] bg-actor/14 px-[10px] py-[2px] text-[12.5px] text-actor font-mono">actor</span>
        </div>
      </div>

      <MachineDeck v-else-if="t.id === 'identity'" class="mt-auto" />

      <!-- The collection: the marks, running off the right. -->
      <div v-else-if="t.id === 'isolation'" class="mb-[28px] mt-auto w-[calc(100%+60px)] flex gap-[12px]">
        <span class="grid h-[76px] w-[76px] flex-none place-items-center rounded-[18px] bg-surface"><Logo name="firecracker" :size="40" /></span>
        <span class="grid h-[76px] w-[76px] flex-none place-items-center rounded-[18px] bg-surface"><Logo name="docker" :size="40" /></span>
        <span class="grid h-[76px] w-[76px] flex-none place-items-center rounded-[18px] bg-surface"><Logo name="gvisor" :size="40" /></span>
        <span class="grid h-[76px] w-[76px] flex-none place-items-center rounded-[18px] bg-surface"><Logo name="apple" :size="40" /></span>
      </div>

      <!-- A component: the faults, one row of chips. -->
      <div v-else-if="t.id === 'chaos'" class="mb-[32px] mt-auto flex flex-wrap gap-[8px]">
        <span class="mono rounded-[12px] bg-[color-mix(in_srgb,var(--fault)_16%,var(--surface))] px-[14px] py-[8px] text-[15px] text-fault">EIO</span>
        <span class="mono rounded-[12px] bg-[color-mix(in_srgb,var(--fault)_16%,var(--surface))] px-[14px] py-[8px] text-[15px] text-fault">exit 7</span>
        <span class="mono rounded-[12px] bg-[color-mix(in_srgb,var(--fault)_16%,var(--surface))] px-[14px] py-[8px] text-[15px] text-fault">101.0°C</span>
      </div>

      <!-- A diagram: small clients on the left, the control plane they talk to on the right. -->
      <div v-else-if="t.id === 'laptop'" class="mb-[28px] mt-auto grid grid-cols-[auto_40px_auto] items-center">
        <span class="flex flex-col gap-[8px]">
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:laptop-line" :size="16" /> laptop</span>
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:chip-line" :size="16" /> board</span>
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:terminal-box-line" :size="16" /> CI job</span>
        </span>
        <span class="h-0 [border-top:1.5px_dashed_var(--line-strong)]" aria-hidden="true" />
        <span class="inline-flex items-center justify-self-start gap-[6px] rounded-[10px] bg-ink px-[12px] py-[8px] text-[14px] text-paper"><Icon name="mingcute:server-2-line" :size="16" /> Control plane</span>
      </div>

      <!-- A diagram: one actor, a pool of machines it could ask for. -->
      <div v-else-if="t.id === 'teleport'" class="mb-[28px] mt-auto grid grid-cols-[auto_40px_auto] items-center">
        <span class="inline-flex items-center justify-self-start gap-[6px] rounded-[10px] bg-[color-mix(in_srgb,var(--actor)_14%,var(--surface))] px-[12px] py-[8px] text-[14px] text-actor">Actor</span>
        <span class="h-0 [border-top:1.5px_dashed_var(--line-strong)]" aria-hidden="true" />
        <span class="flex flex-col gap-[8px]">
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:apple-line" :size="16" /> macOS</span>
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:apple-line" :size="16" /> macOS</span>
          <span class="inline-flex items-center gap-[6px] rounded-[10px] bg-surface px-[12px] py-[8px] text-[14px]"><Icon name="mingcute:monitor-line" :size="16" /> GPU</span>
        </span>
      </div>
    </a>
  </div>
  <p class="mx-0 mb-0 mt-[14px] text-[13.5px] text-muted">
    {{ tr({ en: 'Unmarked tiles are verified end to end in the', zh: '没有标注的都已经端到端跑通过，详见' }) }}
    <a :href="EVIDENCE">{{ tr({ en: 'closed-loop results', zh: '闭环验证记录' }) }}</a>.
  </p>
</template>
