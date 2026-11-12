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
  <div class="recipes">
    <article v-for="m in MACHINES" :key="m.ref" class="recipe">
      <code class="ref">{{ m.ref }}</code>
      <pre class="yaml mono">{{ m.yaml }}</pre>
      <span class="becomes"><Icon name="mingcute:down-line" /> {{ tr({ en: 'the program sees', zh: '程序看到的是' }) }}</span>
      <div class="machine">
        <header>
          <span class="tile"><Icon :name="m.icon" :size="22" /></span>
          <span class="what">
            <strong class="display">{{ m.name }}</strong>
            <span class="kind">{{ tr(m.kind) }}</span>
          </span>
        </header>
        <dl>
          <template v-for="[k, v] in m.sees" :key="k">
            <dt>{{ k }}</dt>
            <dd class="mono">
              {{ v }}
            </dd>
          </template>
        </dl>
      </div>
    </article>
  </div>
</template>

<style scoped>
.recipes {
  display: grid;
  gap: 32px;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
}

.recipe {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 10px;
}

.ref {
  color: var(--actor);
  font-size: 13.5px;
}

.yaml {
  margin: 0;
  color: var(--ink-2);
  font-size: 12.5px;
  line-height: 1.6;
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}

.becomes {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--muted);
  font-size: 13px;
}

/* The one surface of the card: the machine that comes out. */
.machine {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 16px;
  border-radius: 16px;
  background: var(--surface);
}

header {
  display: flex;
  align-items: center;
  gap: 12px;
}

.tile {
  display: grid;
  width: 42px;
  height: 42px;
  flex: none;
  place-items: center;
  border-radius: 10px;
  background: color-mix(in srgb, var(--actor) 13%, transparent);
  color: var(--actor);
}

.what {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

strong {
  font-size: 18px;
  font-weight: 400;
  white-space: nowrap;
}

.kind {
  color: var(--muted);
  font-size: 13px;
}

dl {
  display: grid;
  align-items: baseline;
  gap: 8px 14px;
  margin: 0;
  grid-template-columns: auto minmax(0, 1fr);
}

dt {
  color: var(--muted);
  font-size: 13px;
}

dd {
  min-width: 0;
  margin: 0;
  font-size: 13px;
  line-height: 1.45;
  overflow-wrap: anywhere;
}
</style>
