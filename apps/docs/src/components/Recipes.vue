<script setup lang="ts">
import type { IconName } from '../icons'

import { tr } from '../i18n'
import Icon from '../kit/Icon.vue'

/**
 * Different recipes make different machines. Each card is one recipe from the repository and what a program inside
 * an actor built from it sees. Values are the recipes' own; a `{time}` or `{hash}` placeholder is filled in with an
 * example, as each run fills it in.
 *
 * Source: https://github.com/needy-dev/needy/tree/4a996b82f3d26665f24d5838d4415d18c64f881a/crates/config/src/builtin/recipes
 * and https://github.com/needy-dev/needy/blob/4a996b82f3d26665f24d5838d4415d18c64f881a/examples/opencode/gce.recipe.yaml
 */

interface Machine {
  ref: string
  icon: IconName
  name: string
  kind: { en: string, zh: string }
  yaml: string
  sees: Array<[string, string]>
}

const MACHINES: Machine[] = [
  {
    ref: 'builtin:gce-ubuntu-22.04',
    icon: 'mingcute:cloud-line',
    name: 'GCE n2-standard-2',
    kind: { en: 'A cloud VM', zh: '一台云上的虚拟机' },
    yaml: 'cpu: { brand: "Intel(R) Xeon(R) CPU @ 2.80GHz" }\nfirmware: { dmi: { sys_vendor: Google } }\nos: { hostname: "instance-{time}" }',
    sees: [
      ['hostname', 'instance-20261112-093000'],
      ['CPU', 'Intel(R) Xeon(R) CPU @ 2.80GHz'],
      ['cores', '1 core × 2 threads, 8 GiB'],
      ['kernel', '6.5.0-1020-gcp'],
      ['DMI', 'Google · Google Compute Engine'],
      ['PCI', '8086:1237 host bridge'],
      ['NIC', 'ens4 · 42:01:0a:80:00:…'],
      ['hypervisor', 'visible, as on a cloud VM'],
    ],
  },
  {
    ref: 'builtin:dell-optiplex-ubuntu-24.04',
    icon: 'mingcute:monitor-line',
    name: 'Dell OptiPlex 7000',
    kind: { en: 'A desktop on someone\'s desk', zh: '一台办公桌下的台式机' },
    yaml: 'cpu: { brand: "12th Gen Intel(R) Core(TM) i7-12700" }\nfirmware: { dmi: { sys_vendor: Dell Inc. } }\nos: { uptime: 2d7h }',
    sees: [
      ['hostname', 'optiplex-3f9c0e2a'],
      ['CPU', '12th Gen Intel(R) Core(TM) i7-12700'],
      ['cores', '4 cores × 2 threads, 16 GiB'],
      ['kernel', '6.8.0-49-generic'],
      ['DMI', 'Dell Inc. · OptiPlex 7000'],
      ['PCI', '8086:4668 host bridge'],
      ['sensors', 'coretemp per core, nvme'],
      ['hypervisor', 'hidden, up 2 days 7 hours'],
    ],
  },
  {
    ref: 'examples/opencode/gce.recipe.yaml',
    icon: 'mingcute:cloud-line',
    name: 'GCE, older Xeon',
    kind: { en: 'The built-in VM, extended', zh: '在内置那台的基础上改了改' },
    yaml: 'extends: builtin:gce-ubuntu-22.04\ncpu: { brand: "Intel(R) Xeon(R) CPU @ 2.20GHz" }\nmemory: 4GiB',
    sees: [
      ['hostname', 'instance-20261112-093000'],
      ['CPU', 'Intel(R) Xeon(R) CPU @ 2.20GHz'],
      ['cores', '2 cores × 1 thread, 4 GiB'],
      ['kernel', '6.5.0-1020-gcp'],
      ['DMI', 'Google · Google Compute Engine'],
      ['CPUID', 'family 6, model 79; AVX-512 hidden'],
      ['NIC', 'ens4 · 42:01:0a:80:00:…'],
      ['hypervisor', 'visible'],
    ],
  },
]
</script>

<template>
  <div class="grid gap-[32px] grid-cols-[repeat(auto-fill,minmax(320px,1fr))]">
    <article v-for="m in MACHINES" :key="m.ref" class="flex min-w-0 flex-col gap-[10px]">
      <code class="text-[13.5px] text-actor">{{ m.ref }}</code>
      <pre class="mono m-0 text-[12.5px] leading-[1.6] text-ink-2 whitespace-pre-wrap [overflow-wrap:anywhere]">{{ m.yaml }}</pre>
      <span class="flex items-center gap-[6px] text-[13px] text-muted"><Icon name="mingcute:down-line" /> {{ tr({ en: 'the program sees', zh: '程序看到的是' }) }}</span>
      <!-- The one surface of the card: the machine that comes out. -->
      <div class="flex flex-col gap-[14px] rounded-[16px] bg-surface p-[16px]">
        <header class="flex items-center gap-[12px]">
          <span class="grid h-[42px] w-[42px] flex-none place-items-center rounded-[10px] bg-actor/13 text-actor"><Icon :name="m.icon" :size="22" /></span>
          <span class="flex min-w-0 flex-col">
            <strong class="display text-[18px] font-normal whitespace-nowrap">{{ m.name }}</strong>
            <span class="text-[13px] text-muted">{{ tr(m.kind) }}</span>
          </span>
        </header>
        <dl class="m-0 grid items-baseline gap-x-[14px] gap-y-[8px] grid-cols-[auto_minmax(0,1fr)]">
          <template v-for="[k, v] in m.sees" :key="k">
            <dt class="text-[13px] text-muted">{{ k }}</dt>
            <dd class="mono m-0 min-w-0 text-[13px] leading-[1.45] [overflow-wrap:anywhere]">
              {{ v }}
            </dd>
          </template>
        </dl>
      </div>
    </article>
  </div>
</template>
