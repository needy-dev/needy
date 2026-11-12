<script setup lang="ts">
import type { IconName } from '../icons'

import { onBeforeUnmount, onMounted, ref } from 'vue'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'

/**
 * How an agent harness works, and where needy cuts it: the harness asks the model, the model answers with a tool
 * call, the harness runs the tool and hands back the result, and so on until the task is done. The loop itself is
 * light and waits on the model; the tools' commands are where the work, the state and the risk are.
 */

const HARNESSES: Array<{ icon: IconName, name: string }> = [
  { icon: 'lobe-icons:claudecode-color', name: 'Claude Code' },
  { icon: 'lobe-icons:codex-color', name: 'Codex' },
  { icon: 'lobe-icons:opencode', name: 'opencode' },
  { icon: 'lobe-icons:geminicli-color', name: 'Gemini CLI' },
]
const MODELS: Array<{ icon: IconName, name: string }> = [
  { icon: 'lobe-icons:claude-color', name: 'Claude' },
  { icon: 'lobe-icons:openai', name: 'OpenAI' },
  { icon: 'lobe-icons:deepseek-color', name: 'DeepSeek' },
  { icon: 'lobe-icons:qwen-color', name: 'Qwen' },
  { icon: 'lobe-icons:ollama', name: 'Ollama' },
]

const STAGES = [
  { id: 'ask', side: 'actor', title: { en: 'Ask the model', zh: '问模型' }, text: { en: 'The harness sends the conversation and the tools it offers.', zh: 'harness 把对话和能用的工具一起发给模型。' } },
  { id: 'call', side: 'model', title: { en: 'Tool call', zh: '工具调用' }, text: { en: 'The model answers with a call: run bash, edit a file, read a file.', zh: '模型回一句「去做这个」：跑个 bash、改个文件、读个文件。' } },
  { id: 'run', side: 'executor', title: { en: 'Run the tool', zh: '干活' }, text: { en: 'The harness starts a command: cargo test, npm install, pytest. This is the heavy part.', zh: 'harness 去跑命令，cargo test、npm install、pytest，重活都在这一步。' } },
  { id: 'result', side: 'actor', title: { en: 'Hand back the result', zh: '交结果' }, text: { en: 'Output and exit code go back to the model, and the loop goes on.', zh: '把输出和退出码交回模型，接着下一轮。' } },
] as const

const active = ref(0)
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  timer = setInterval(() => (active.value = (active.value + 1) % STAGES.length), 2200)
})
onBeforeUnmount(() => clearInterval(timer))
</script>

<template>
  <!-- Grouped by space and alignment, not by boxes: the two lists, then the four stages on one track. -->
  <div class="flex flex-col gap-[32px]">
    <dl class="m-0 flex flex-col gap-[10px]">
      <div class="flex items-center gap-[16px]">
        <dt class="w-[88px] flex-none text-[14px] text-muted">{{ tr({ en: 'Harnesses', zh: 'Harness' }) }}</dt>
        <dd class="m-0 flex flex-wrap gap-[18px]"><span v-for="h in HARNESSES" :key="h.name" class="inline-flex items-center gap-[7px] text-[15px]"><Icon :name="h.icon" :size="20" />{{ h.name }}</span></dd>
      </div>
      <div class="flex items-center gap-[16px]">
        <dt class="w-[88px] flex-none text-[14px] text-muted">{{ tr({ en: 'Models', zh: '模型' }) }}</dt>
        <dd class="m-0 flex flex-wrap gap-[18px]"><span v-for="m in MODELS" :key="m.name" class="inline-flex items-center gap-[7px] text-[15px]"><Icon :name="m.icon" :size="20" />{{ m.name }}</span></dd>
      </div>
    </dl>

    <ol class="m-0 grid grid-cols-[repeat(4,minmax(0,1fr))] list-none gap-[24px] p-0 max-[860px]:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <li
        v-for="(s, i) in STAGES" :key="s.id" class="flex flex-col cursor-default gap-[6px]"
        :class="s.side === 'model' ? '[--tone:var(--ink-2)]' : s.side === 'executor' ? '[--tone:var(--executor)]' : '[--tone:var(--actor)]'"
        @mouseenter="active = i"
      >
        <!-- One line through the four stages; the stage that runs now lights its stretch. -->
        <span class="relative mb-[10px] block h-[4px] rounded-[2px] [transition:background_0.35s]" :class="active === i ? 'bg-tone' : 'bg-control-2'"><i class="absolute left-0 top-[-4px] h-[12px] w-[12px] rounded-[50%] [transition:background_0.35s]" :class="active === i ? 'bg-tone' : 'bg-control-3'" /></span>
        <span class="text-[13px] text-tone">{{ s.side === 'executor' ? tr({ en: 'On the executor', zh: '在 executor 上' }) : s.side === 'model' ? tr({ en: 'At the model', zh: '在模型那里' }) : tr({ en: 'In the actor', zh: '在 actor 里' }) }}</span>
        <strong class="display text-[19px] font-400" :class="active === i ? '' : 'opacity-60 [transition:opacity_0.35s]'">{{ tr(s.title) }}</strong>
        <span class="text-[14.5px] text-ink-2 leading-[1.5]" :class="active === i ? '' : 'opacity-60 [transition:opacity_0.35s]'">{{ tr(s.text) }}</span>
      </li>
    </ol>
  </div>
</template>
