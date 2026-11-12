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
  <ul class="ideas">
    <li v-for="idea in IDEAS" :key="idea.title.en" class="idea">
      <span class="marks"><Icon v-for="m in idea.marks" :key="m" :name="m" :size="30" /></span>
      <strong class="display">{{ tr(idea.title) }}</strong>
      <span class="text">{{ tr(idea.text) }}</span>
    </li>
  </ul>
</template>

<style scoped>
.ideas {
  display: grid;
  gap: 16px;
  margin: 0;
  padding: 0;
  list-style: none;
  grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
}

.idea {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 22px;
  border-radius: 20px;
  background: var(--control);
}

.marks {
  display: flex;
  gap: 10px;
  height: 36px;
  align-items: center;
  margin-bottom: 8px;
  color: var(--ink-2);
}

strong {
  font-size: 19px;
  font-weight: 400;
}

.text {
  color: var(--ink-2);
  font-size: 15px;
  line-height: 1.5;
}
</style>
