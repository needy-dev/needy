<script setup lang="ts">
import type { Cell } from '../data/drivers'
import type { Step } from '../kit/player'

import LevelMark from '../kit/LevelMark.vue'
import Frame from '../kit/Frame.vue'
import Logo from '../kit/Logo.vue'
import Machine from '../kit/Machine.vue'
import Pod from '../kit/Pod.vue'
import Yaml from '../kit/Yaml.vue'

import { DRIVERS, feature, LEVELS } from '../data/drivers'

/**
 * The recipe as a costume over the actor: what the driving program sees is a Compute Engine VM, and underneath is a
 * Linux KVM host running Firecracker. Each step shows one field, the recipe line that sets it, and which actor
 * drivers can present it (the README's identity rows, through src/data/drivers.ts).
 *
 * The values are the recipe's own:
 * Source: https://github.com/needy-dev/needy/blob/1efa6c8d1ab6b2bde0db2a8cf808167758601aa7/crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml
 * `systemd-detect-virt` printing `google` is in docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md.
 * The hostname is `instance-{time}` for a run created at 2026-11-12 09:30:00 UTC.
 */
defineProps<{ capture?: boolean }>()

const W = 1440
const H = 576

/** An excerpt of builtin:gce-ubuntu-22.04, with its nested keys folded into flow mappings. */
const RECIPE = `# builtin:gce-ubuntu-22.04 (excerpt)
cpu:
  brand: "Intel(R) Xeon(R) CPU @ 2.80GHz"
  signature: { family: 6, model: 85, stepping: 7 }
memory: 8GiB
os:
  kernel: { release: 6.5.0-1020-gcp }
  hostname: "instance-{time}"
firmware:
  dmi:
    sys_vendor: Google
    product_name: Google Compute Engine
devices:
  pci: { host_bridge: "8086:1237:02" }
  sensors: []`

interface Field {
  id: string
  label: string
  probe: string
  value: string
  /** The recipe lines that set it. */
  lines: number[]
  /** The README's identity row, or none when the README does not list the field. */
  row?: string
  note: string
}

const FIELDS: Field[] = [
  { id: 'hostname', label: 'Hostname', probe: 'hostname', value: 'instance-20261112-093000', lines: [7], row: 'hostname', note: 'Every driver presents the hostname' },
  { id: 'cpu', label: 'CPU', probe: '/proc/cpuinfo', value: 'Intel(R) Xeon(R) CPU @ 2.80GHz', lines: [2], row: 'cpuinfo', note: 'A Linux driver rewrites /proc/cpuinfo' },
  { id: 'cpuid', label: 'CPUID', probe: 'leaf 1', value: 'family 6 · model 85 · stepping 7', lines: [3], row: 'cpuid', note: 'CPUID: only linux-firecracker' },
  { id: 'dmi', label: 'DMI', probe: '/sys/class/dmi/id', value: 'Google · Google Compute Engine', lines: [10, 11], row: 'dmi', note: 'SMBIOS strings from the recipe' },
  { id: 'pci', label: 'PCI', probe: 'lspci -n', value: '8086:1237 host bridge', lines: [13], row: 'pci', note: 'PCI IDs: only linux-firecracker' },
  { id: 'kernel', label: 'Kernel', probe: 'uname -r', value: '6.5.0-1020-gcp', lines: [6], row: 'kernel', note: 'The kernel version of a GCE image' },
  { id: 'hwmon', label: 'Sensors', probe: 'hwmon', value: 'none, as on Compute Engine', lines: [14], row: 'hwmon', note: 'No sensors, like the real VM' },
  { id: 'virt', label: 'Virt', probe: 'systemd-detect-virt', value: 'google', lines: [10, 11], note: 'It looks like a Compute Engine VM' },
]

interface IdentityStep extends Step {
  /** The field this step shows, or none for the last step, where the costume lifts. */
  field?: number
}

const STEPS: IdentityStep[] = [
  ...FIELDS.map((f, i) => ({ ms: 2400, field: i, caption: captionOf(f) })),
  { ms: 3400, caption: 'Underneath is the host: a Linux KVM host running Firecracker, which needy patches for CPUID, SMBIOS and PCI.' },
]

function captionOf(f: Field): string {
  switch (f.id) {
    case 'hostname': return 'A recipe fixes the machine the driving program sees, starting with its hostname.'
    case 'cpu': return 'The CPU brand in /proc/cpuinfo is the recipe\'s, not the host\'s.'
    case 'cpuid': return 'Under Firecracker, even the CPUID signature is Cascade Lake, as on an n2-standard-2.'
    case 'dmi': return 'Firmware strings say Google Compute Engine.'
    case 'pci': return 'The PCI host bridge is the 440FX that Compute Engine shows.'
    case 'kernel': return 'uname reports the GCE kernel build the recipe names.'
    case 'hwmon': return 'The recipe lists no sensors, because a Compute Engine VM has none.'
    default: return 'So systemd-detect-virt says google, as it does on a Compute Engine VM.'
  }
}

/** The support of field `i` by driver, from the README's identity rows. */
function support(i: number): Cell[] | undefined {
  const row = FIELDS[i]!.row
  return row ? feature('identity', row).cells : undefined
}

const peeled = (at: number) => STEPS[at]!.field === undefined
const fieldAt = (at: number) => STEPS[at]!.field

/** The rows revealed by step `at`: every field up to the current one; all of them once the costume lifts. */
const shown = (at: number) => peeled(at) ? FIELDS.length : (fieldAt(at)! + 1)

const POD = { x: 470, y: 30, w: 556, h: 410 }

// What a row of the costume and of the host share, and a value cut to its cell.
const LABEL = 'font-display text-[15px] text-ink-2'
const CELL = 'overflow-hidden text-ellipsis whitespace-nowrap'
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Identity: a recipe dresses the actor as a Google Compute Engine VM. Hostname, CPU, CPUID, DMI, PCI, kernel and sensors come from the recipe, each presented by the drivers that can; underneath is a Linux KVM host running Firecracker.">
    <template #default="{ at }">
      <div class="absolute top-[30px] left-[32px] w-[404px] [&_.yaml]:text-[12px]">
        <Yaml :code="RECIPE" :lit="fieldAt(at) === undefined ? [] : FIELDS[fieldAt(at)!]!.lines" role="actor" title="Recipe" />
      </div>

      <!-- The host, under the costume. It shows at the corner, and fully when the costume lifts. -->
      <Machine
        :x="POD.x + 34" :y="POD.y + 40" :w="POD.w" :h="POD.h" role="host" logo="firecracker"
        title="The host" sub="A Linux KVM host running Firecracker" :focus="peeled(at) ? 'focus' : 'idle'"
      >
        <ul class="flex flex-col gap-[4px] m-[6px_0_0] p-0 list-none [transition:opacity_0.5s_0.2s]" :class="!peeled(at) && 'opacity-0'">
          <li v-for="f in FIELDS" :key="f.id" class="grid h-[36px] items-center gap-[12px] p-[0_12px] grid-cols-[70px_1fr]">
            <span :class="LABEL">{{ f.label }}</span>
            <span class="text-[13.5px] text-muted">the host's own</span>
          </li>
        </ul>
      </Machine>

      <!-- The costume lifts off its bottom-right corner and shows the host underneath. -->
      <div
        class="absolute z-2 [transform-origin:0_0] [transition:transform_0.7s_cubic-bezier(0.2,0,0,1),opacity_0.7s_cubic-bezier(0.2,0,0,1)] [&_.pod]:bg-[color-mix(in_srgb,var(--actor)_10%,var(--paper))] [[data-theme=dark]_&_.pod]:bg-[color-mix(in_srgb,var(--actor)_16%,var(--paper))]"
        :class="peeled(at) && 'opacity-[0.07] [transform:translate(-30px,-22px)_rotate(-3deg)]'"
        :style="{ left: `${POD.x}px`, top: `${POD.y}px`, width: `${POD.w}px`, height: `${POD.h}px` }"
      >
        <Pod :x="0" :y="0" :w="POD.w" :h="POD.h" title="What the program sees" driver="linux-firecracker" logo="firecracker">
          <ul class="flex flex-col gap-[4px] m-[6px_0_0] p-0 list-none">
            <li
              v-for="(f, i) in FIELDS" :key="f.id" class="f grid h-[36px] items-center gap-[12px] p-[0_12px] rounded-[10px] grid-cols-[70px_150px_1fr]"
              :class="[i >= shown(at) ? 'opacity-0 [transform:translateY(6px)]' : fieldAt(at) === i ? 'focus !bg-[color-mix(in_srgb,var(--actor)_14%,var(--surface))]' : fieldAt(at) === undefined ? 'idle' : 'dim']"
              style="--role: var(--actor)"
            >
              <span :class="LABEL">{{ f.label }}</span>
              <code :class="CELL" class="text-[12px] text-muted">{{ f.probe }}</code>
              <code :class="CELL" class="text-[13.5px] text-ink">{{ f.value }}</code>
            </li>
          </ul>
        </Pod>
        <!-- A turned-up corner: the costume is something put on. -->
        <span class="absolute right-0 bottom-0 w-[46px] h-[46px] rounded-[0_0_34px] [background:linear-gradient(135deg,transparent_50%,color-mix(in_srgb,var(--actor)_22%,var(--surface))_50%)]" aria-hidden="true" />
      </div>

      <div class="absolute top-[30px] left-[1080px] flex w-[330px] flex-col gap-[8px]">
        <span class="display text-[15px] text-muted">Who can present it</span>
        <span class="font-display text-[26px]">{{ fieldAt(at) === undefined ? 'Underneath' : FIELDS[fieldAt(at)!]!.label }}</span>
        <span :key="`t${at}`" class="self-start p-[4px_11px] rounded-[999px] bg-actor/13 text-[13.5px] text-actor animate-[enter-right-10_0.4s_cubic-bezier(0.2,0,0,1)_both]">{{ fieldAt(at) === undefined ? 'Firecracker on a Linux KVM host' : FIELDS[fieldAt(at)!]!.note }}</span>
        <ul v-if="fieldAt(at) !== undefined && support(fieldAt(at)!)" :key="at" class="flex flex-col gap-[8px] m-[6px_0_0] p-0 list-none">
          <li
            v-for="(d, i) in DRIVERS" :key="d.id"
            class="f grid items-center gap-x-[10px] gap-y-[4px] p-[10px_12px] rounded-[12px] bg-surface grid-cols-[22px_1fr_18px] animate-[enter-right-10_0.4s_cubic-bezier(0.2,0,0,1)_both] [&:nth-child(2)]:[animation-delay:60ms] [&:nth-child(3)]:[animation-delay:120ms] [&:nth-child(4)]:[animation-delay:180ms]"
            :class="support(fieldAt(at)!)![i]!.level === 'no' ? 'dim' : 'idle'"
          >
            <span class="flex"><Logo v-for="l in d.logos.slice(0, 1)" :key="l" :name="l" :size="20" /></span>
            <code class="text-[13px]">{{ d.id }}</code>
            <LevelMark :level="support(fieldAt(at)!)![i]!.level" />
            <span class="text-[12.5px] text-muted [grid-column:2/4]">{{ LEVELS[support(fieldAt(at)!)![i]!.level].label }}<template v-if="support(fieldAt(at)!)![i]!.note"> · {{ support(fieldAt(at)!)![i]!.note }}</template></span>
          </li>
        </ul>
        <p v-else-if="fieldAt(at) !== undefined" class="m-[6px_0_0] text-[15px] leading-[1.5] text-ink-2 [&_code]:text-[13px]">
          The README's table does not list this field; the closed-loop results record it on <code>linux-firecracker</code>.
        </p>
        <p v-else class="m-[6px_0_0] text-[15px] leading-[1.5] text-ink-2 [&_code]:text-[13px]">
          The program never sees the host. <code>needy config profile check</code> lists what of a recipe a driver cannot present, and a recipe's <code>required:</code> turns a gap into an error.
        </p>
      </div>
    </template>
  </Frame>
</template>
