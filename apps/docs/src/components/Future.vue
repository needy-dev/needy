<script setup lang="ts">
import type { IconName } from '../icons'
import type { Text } from '../i18n'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'

/**
 * Where Needy goes, as the kinds of executors a harness could ask for. None of this is built: each card is marked as
 * a vision, as content.ts marks the section. DSec is the sandbox infrastructure the README's acknowledgements cite.
 */

interface Idea { marks: IconName[], title: Text, text: Text }

const IDEAS: Idea[] = [
  { marks: ['logos:apple', 'logos:xcode'], title: { en: 'A macOS pool', zh: 'macOS 集群' }, text: { en: 'Builds and simulators on Macs that join for one run.', zh: '需要时拉几台 Mac 进来，构建、跑模拟器，用完就撤。' } },
  { marks: ['logos:blender'], title: { en: 'A render farm', zh: '渲染农场' }, text: { en: 'Blender jobs fanned out to machines with GPUs.', zh: 'Blender 任务分到带显卡的机器上去渲。' } },
  { marks: ['logos:nvidia', 'mingcute:gpu-line'], title: { en: 'GPU boxes', zh: 'GPU 机器' }, text: { en: 'Training and evals where the accelerators are.', zh: '训练和评测，放到有加速卡的地方去做。' } },
  { marks: ['mingcute:grid-line'], title: { en: 'Executor groups', zh: 'Executor 分组' }, text: { en: 'Many harnesses scheduled at once, as DSec does for agent RL.', zh: '一次调度一大批 harness，像 DSec 给 agent RL 做的那样。' } },
]
</script>

<template>
  <ul class="m-0 grid grid-cols-[repeat(auto-fill,minmax(230px,1fr))] list-none gap-[16px] p-0">
    <li v-for="idea in IDEAS" :key="idea.title.en" class="flex flex-col gap-[8px] rounded-[20px] bg-control p-[22px]">
      <span class="mb-[8px] h-[36px] flex items-center gap-[10px] text-ink-2"><Icon v-for="m in idea.marks" :key="m" :name="m" :size="30" /></span>
      <strong class="display text-[19px] font-400">{{ tr(idea.title) }}</strong>
      <span class="text-[15px] text-ink-2 leading-[1.5]">{{ tr(idea.text) }}</span>
    </li>
  </ul>
</template>
