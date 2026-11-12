<script setup lang="ts">
import type { Step } from '../kit/player'

import Frame from '../kit/Frame.vue'
import Icon from '../kit/Icon.vue'
import Logo from '../kit/Logo.vue'

/**
 * The first screen, framed as a product shot after the stageshow skill: the copy on the left; one window, the actor's
 * terminal, whole at a terminal's proportions; a corner chip on its top edge saying where the actor runs; one
 * satellite, the Mac executor, straddling the window's right edge over the terminal's empty margin. One turn of the harness: the model asks for a build, the actor catches `xcodebuild` and the Mac
 * runs it, the result comes back; then a push is denied. As in examples/macos, the actor runs Linux and the build goes
 * to a Mac. The poster is the frame where the result is back.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 576

interface HeroStep extends Step {
  /** Lines in the terminal so far. */
  lines: number
  /** What the Mac shows. */
  mac: 'idle' | 'running' | 'done'
  routed: boolean
  denied: boolean
}

const STEPS: HeroStep[] = [
  { ms: 2400, lines: 2, mac: 'idle', routed: false, denied: false },
  { ms: 2600, lines: 2, mac: 'running', routed: true, denied: false },
  { ms: 3200, lines: 3, mac: 'done', routed: true, denied: false },
  { ms: 3200, lines: 5, mac: 'done', routed: true, denied: true },
]

// The utilities a terminal line and a tag share; a hidden one fades in from a little below or to the left.
const LINE = 'flex items-center gap-[10px] min-h-[24px] whitespace-pre [transition:opacity_0.5s_cubic-bezier(0.25,0.8,0.25,1),transform_0.5s_cubic-bezier(0.25,0.8,0.25,1)]'
const LINE_HIDDEN = 'opacity-0 [transform:translateY(4px)]'
const TAG = 'p-[1px_10px] rounded-[999px] text-[13px] text-white not-italic [transition:opacity_0.35s_cubic-bezier(0.25,0.8,0.25,1),transform_0.35s_cubic-bezier(0.25,0.8,0.25,1)]'
const TAG_HIDDEN = 'opacity-0 [transform:translateX(-6px)]'
</script>



<template>
  <!-- Wide: the stage. Narrow: the compact design instead, at the column's width with real-size type. -->
  <div class="max-[720px]:hidden">
  <Frame bare :width="W" :height="H" :steps="STEPS" :capture="capture" label="Needy: an isolated sandbox for agent harnesses. In the actor's terminal, the harness runs xcodebuild; needy routes it to a Mac executor, which builds and returns BUILD SUCCEEDED; a git push is denied by a rule.">
    <template #default="{ at }">
      <div class="absolute top-[132px] left-[56px] w-[340px]">
        <h1 class="m-0 text-[92px] leading-[1] tracking-[-0.02em]">
          Needy
        </h1>
        <p class="m-[24px_0_0] text-[22px] leading-[1.35] text-ink">
          An isolated sandbox for agent harnesses, with seamless exec routing and policy.
        </p>
      </div>

      <!-- The hero: the actor's terminal, whole. -->
      <!-- The window: whole, at a terminal's proportions; the page is its ground, so no plate. On a dark page depth
           comes from a lighter surface, so the window steps up from the page instead of sinking into it. -->
      <div class="absolute top-[84px] left-[500px] w-[720px] h-[432px] overflow-hidden rounded-[14px] bg-term [box-shadow:0_0_0_1px_rgb(255_255_255/6%),0_24px_60px_rgb(0_0_0/18%)] text-term-ink [[data-theme=dark]_&]:bg-term-raised">
        <div class="flex items-center gap-[7px] h-[40px] p-[0_16px] bg-white/4">
          <i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" /><i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" /><i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" />
          <span class="inline-flex items-center gap-[7px] ml-[12px] font-display text-[14px] text-white/80">Actor <span class="mono text-[12px] text-white/42">opencode</span></span>
        </div>
        <!-- The lines keep to the left; the satellite covers only the empty right of the terminal. -->
        <div class="mono flex flex-col gap-[8px] p-[28px_28px_0] text-[16px] leading-[1.5]">
          <span :class="LINE" class="text-white/45">opencode run "fix the build"</span>
          <span :class="LINE" class="text-white/45">› the model asks to build the app</span>
          <span :class="LINE">
            <b class="font-normal text-actor-term">$</b> xcodebuild -scheme App
            <em :class="[TAG, !STEPS[at]!.routed && TAG_HIDDEN]" class="bg-executor-fill">→ mac</em>
          </span>
          <span :class="[LINE, STEPS[at]!.lines < 3 && LINE_HIDDEN]" class="text-executor-term">** BUILD SUCCEEDED **</span>
          <span :class="[LINE, STEPS[at]!.lines < 5 && LINE_HIDDEN]">
            <b class="font-normal text-actor-term">$</b> git push origin main
            <em :class="[TAG, !STEPS[at]!.denied && TAG_HIDDEN]" class="bg-bad-fill">denied</em>
          </span>
          <span :class="[LINE, STEPS[at]!.lines < 5 && LINE_HIDDEN]" class="text-bad-term">needy: denied: no pushes from the agent</span>
          <span :class="LINE"><i class="block h-[8px] rounded-[4px] bg-white/8" style="width: 46%" /></span>
          <span :class="LINE"><i class="block h-[8px] rounded-[4px] bg-white/8" style="width: 31%" /></span>
        </div>
      </div>

      <!-- Where the actor runs: a chip on the window's top-right corner, half outside. -->
      <!-- The chip: one fact about the whole window, on its top-right corner over the empty title bar, half outside. -->
      <span class="absolute top-[66px] right-[156px] inline-flex items-center gap-[7px] p-[8px_14px] rounded-[999px] bg-surface [box-shadow:0_1px_2px_rgb(0_0_0/8%),0_8px_22px_rgb(0_0_0/14%)] text-[14px] text-ink whitespace-nowrap">
        <Logo name="firecracker" :size="16" /> Firecracker microVM
        <span class="w-[4px] h-[4px] m-[0_3px] rounded-[50%] bg-faint" /> <Icon name="mingcute:linux-line" :size="15" /> Linux KVM host
      </span>

      <!-- The satellite: the Mac executor, half outside the window's right edge. -->
      <!-- A light card on the dark window, straddling its right edge over the terminal's empty margin. -->
      <div
        class="absolute top-[262px] left-[1090px] flex w-[250px] flex-col gap-[14px] p-[16px] rounded-[16px] bg-surface [box-shadow:0_1px_2px_rgb(0_0_0/8%),0_12px_32px_rgb(0_0_0/18%)] [transition:transform_0.45s_cubic-bezier(0.2,0,0,1)]"
        :class="STEPS[at]!.mac !== 'idle' && '[transform:translateY(-4px)]'"
      >
        <div class="flex items-center gap-[12px]">
          <span class="grid w-[38px] h-[38px] flex-none place-items-center rounded-[10px] bg-executor/15 text-executor"><Icon name="mingcute:apple-line" :size="18" /></span>
          <span class="flex flex-col">
            <strong class="display text-[18px] font-normal">mac</strong>
            <span class="text-[13px] text-muted">Executor · runner on a Mac</span>
          </span>
        </div>
        <div class="mono flex flex-col gap-[10px]">
          <span
            class="text-[13px] [transition:color_0.3s]"
            :class="STEPS[at]!.mac === 'done' ? 'text-executor' : STEPS[at]!.mac === 'running' ? 'text-ink' : 'text-muted'"
          >{{ STEPS[at]!.mac === 'done' ? '** BUILD SUCCEEDED **' : STEPS[at]!.mac === 'running' ? 'xcodebuild -scheme App' : 'waiting for work' }}</span>
          <span class="overflow-hidden h-[6px] rounded-[3px] bg-control">
            <i
              class="block h-full rounded-[3px] bg-executor-fill"
              :class="STEPS[at]!.mac === 'done' ? 'w-full [transition:width_0.4s_cubic-bezier(0.25,0.8,0.25,1)]' : STEPS[at]!.mac === 'running' ? 'w-[82%] [transition:width_2.2s_cubic-bezier(0.45,0,0.1,1)]' : 'w-0'"
            />
          </span>
        </div>
      </div>
    </template>
  </Frame>
  </div>

  <!-- The compact design, for a phone column: the same story as one still, stacked, at the poster's beat. -->
  <div v-if="!capture" class="hidden max-[720px]:(flex flex-col gap-0 p-[24px_0_8px])">
    <h1 class="m-0 text-[64px] leading-[1] tracking-[-0.02em]">
      Needy
    </h1>
    <p class="m-[16px_0_0] text-[20px] leading-[1.35] text-ink">
      An isolated sandbox for agent harnesses, with seamless exec routing and policy.
    </p>
    <span class="relative z-2 self-start inline-flex items-center gap-[7px] m-[32px_0_12px] p-[8px_14px] rounded-[999px] bg-surface [box-shadow:0_1px_2px_rgb(0_0_0/8%),0_8px_22px_rgb(0_0_0/14%)] text-[12.5px] text-ink whitespace-nowrap">
      <Logo name="firecracker" :size="16" /> Firecracker microVM <span class="w-[4px] h-[4px] m-[0_3px] rounded-[50%] bg-faint" /> <Icon name="mingcute:linux-line" :size="15" /> Linux KVM host
    </span>
    <div class="overflow-hidden rounded-[14px] bg-term text-term-ink [[data-theme=dark]_&]:bg-term-raised">
      <div class="flex items-center gap-[7px] h-[40px] p-[0_16px] bg-white/4">
        <i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" /><i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" /><i class="w-[11px] h-[11px] rounded-[50%] bg-white/16" /><span class="inline-flex items-center gap-[7px] ml-[12px] font-display text-[14px] text-white/80">Actor <span class="mono text-[12px] text-white/42">opencode</span></span>
      </div>
      <div class="mono flex flex-col gap-[8px] p-[18px_16px_22px] text-[13.5px] leading-[1.5]">
        <span :class="LINE" class="!whitespace-normal flex-wrap text-white/45">› the model asks to build the app</span>
        <span :class="LINE" class="!whitespace-normal flex-wrap"><b class="font-normal text-actor-term">$</b> xcodebuild -scheme App <em :class="TAG" class="bg-executor-fill">→ mac</em></span>
        <span :class="LINE" class="!whitespace-normal flex-wrap text-executor-term">** BUILD SUCCEEDED **</span>
      </div>
    </div>
    <div class="relative flex flex-col gap-[14px] m-[-20px_12px_0_40px] p-[16px] rounded-[16px] bg-surface [box-shadow:0_1px_2px_rgb(0_0_0/8%),0_12px_32px_rgb(0_0_0/18%)]">
      <div class="flex items-center gap-[12px]">
        <span class="grid w-[38px] h-[38px] flex-none place-items-center rounded-[10px] bg-executor/15 text-executor"><Icon name="mingcute:apple-line" :size="18" /></span>
        <span class="flex flex-col">
          <strong class="display text-[18px] font-normal">mac</strong>
          <span class="text-[13px] text-muted">Executor · runner on a Mac</span>
        </span>
      </div>
      <div class="mono flex flex-col gap-[10px]">
        <span class="text-[13px] text-executor">** BUILD SUCCEEDED **</span>
        <span class="overflow-hidden h-[6px] rounded-[3px] bg-control"><i class="block w-full h-full rounded-[3px] bg-executor-fill" /></span>
      </div>
    </div>
  </div>
</template>
