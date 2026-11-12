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
</script>

<template>
  <Frame :width="W" :height="H" :steps="STEPS" :capture="capture" label="Identity: a recipe dresses the actor as a Google Compute Engine VM. Hostname, CPU, CPUID, DMI, PCI, kernel and sensors come from the recipe, each presented by the drivers that can; underneath is a Linux KVM host running Firecracker.">
    <template #default="{ at }">
      <div class="recipe">
        <Yaml :code="RECIPE" :lit="fieldAt(at) === undefined ? [] : FIELDS[fieldAt(at)!]!.lines" role="actor" title="Recipe" />
      </div>

      <!-- The host, under the costume. It shows at the corner, and fully when the costume lifts. -->
      <Machine
        :x="POD.x + 34" :y="POD.y + 40" :w="POD.w" :h="POD.h" role="host" logo="firecracker"
        title="The host" sub="A Linux KVM host running Firecracker" :focus="peeled(at) ? 'focus' : 'idle'"
      >
        <ul class="host-rows" :class="{ shown: peeled(at) }">
          <li v-for="f in FIELDS" :key="f.id">
            <span class="label">{{ f.label }}</span>
            <span class="host-value">the host's own</span>
          </li>
        </ul>
      </Machine>

      <div class="costume" :class="{ peeled: peeled(at) }" :style="{ left: `${POD.x}px`, top: `${POD.y}px`, width: `${POD.w}px`, height: `${POD.h}px` }">
        <Pod :x="0" :y="0" :w="POD.w" :h="POD.h" title="What the program sees" driver="linux-firecracker" logo="firecracker">
          <ul class="rows">
            <li
              v-for="(f, i) in FIELDS" :key="f.id" class="row f"
              :class="[i >= shown(at) ? 'hidden' : fieldAt(at) === i ? 'focus' : fieldAt(at) === undefined ? 'idle' : 'dim']"
              style="--role: var(--actor)"
            >
              <span class="label">{{ f.label }}</span>
              <code class="probe">{{ f.probe }}</code>
              <code class="value">{{ f.value }}</code>
            </li>
          </ul>
        </Pod>
        <span class="fold" aria-hidden="true" />
      </div>

      <div class="who">
        <span class="who-title display">Who can present it</span>
        <span class="who-field">{{ fieldAt(at) === undefined ? 'Underneath' : FIELDS[fieldAt(at)!]!.label }}</span>
        <span :key="`t${at}`" class="who-tag">{{ fieldAt(at) === undefined ? 'Firecracker on a Linux KVM host' : FIELDS[fieldAt(at)!]!.note }}</span>
        <ul v-if="fieldAt(at) !== undefined && support(fieldAt(at)!)" :key="at" class="drivers">
          <li v-for="(d, i) in DRIVERS" :key="d.id" class="driver f" :class="support(fieldAt(at)!)![i]!.level === 'no' ? 'dim' : 'idle'">
            <span class="logos"><Logo v-for="l in d.logos.slice(0, 1)" :key="l" :name="l" :size="20" /></span>
            <code class="id">{{ d.id }}</code>
            <LevelMark :level="support(fieldAt(at)!)![i]!.level" />
            <span class="level">{{ LEVELS[support(fieldAt(at)!)![i]!.level].label }}<template v-if="support(fieldAt(at)!)![i]!.note"> · {{ support(fieldAt(at)!)![i]!.note }}</template></span>
          </li>
        </ul>
        <p v-else-if="fieldAt(at) !== undefined" class="who-note">
          The README's table does not list this field; the closed-loop results record it on <code>linux-firecracker</code>.
        </p>
        <p v-else class="who-note">
          The program never sees the host. <code>needy profile check</code> lists what of a recipe a driver cannot present, and a recipe's <code>required:</code> turns a gap into an error.
        </p>
      </div>

    </template>
  </Frame>
</template>

<style scoped>
.recipe {
  position: absolute;
  top: 30px;
  left: 32px;
  width: 404px;
}

.recipe :deep(.yaml) {
  font-size: 12px;
}

.costume {
  position: absolute;
  z-index: 2;
  transform-origin: 0 0;
  transition:
    transform 0.7s cubic-bezier(0.2, 0, 0, 1),
    opacity 0.7s cubic-bezier(0.2, 0, 0, 1);
}

/* The costume lifts off its bottom-right corner and shows the host underneath. */
.costume.peeled {
  opacity: 0.07;
  transform: translate(-30px, -22px) rotate(-3deg);
}

.costume :deep(.pod) {
  background: color-mix(in srgb, var(--actor) 10%, var(--paper));
}

:root[data-theme='dark'] .costume :deep(.pod) {
  background: color-mix(in srgb, var(--actor) 16%, var(--paper));
}

/* A turned-up corner: the costume is something put on. */
.fold {
  position: absolute;
  right: 0;
  bottom: 0;
  width: 46px;
  height: 46px;
  border-radius: 0 0 34px;
  background: linear-gradient(135deg, transparent 50%, color-mix(in srgb, var(--actor) 22%, var(--surface)) 50%);
}

.rows {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 6px 0 0;
  padding: 0;
  list-style: none;
}

.row {
  display: grid;
  height: 36px;
  align-items: center;
  gap: 12px;
  padding: 0 12px;
  border-radius: 10px;
  grid-template-columns: 70px 150px 1fr;
}

.row.focus {
  background: color-mix(in srgb, var(--actor) 14%, var(--surface));
}

.row.hidden {
  opacity: 0;
  transform: translateY(6px);
}

.label {
  color: var(--ink-2);
  font-family: var(--font-display);
  font-size: 15px;
}

.probe {
  overflow: hidden;
  color: var(--muted);
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.value {
  overflow: hidden;
  color: var(--ink);
  font-size: 13.5px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.host-rows {
  opacity: 0;
  transition: opacity 0.5s 0.2s;
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 6px 0 0;
  padding: 0;
  list-style: none;
}

.host-rows.shown {
  opacity: 1;
}

.host-rows li {
  display: grid;
  height: 36px;
  align-items: center;
  gap: 12px;
  padding: 0 12px;
  grid-template-columns: 70px 1fr;
}

.host-value {
  color: var(--muted);
  font-size: 13.5px;
}

.who {
  position: absolute;
  top: 30px;
  left: 1080px;
  display: flex;
  width: 330px;
  flex-direction: column;
  gap: 8px;
}

.who-title {
  color: var(--muted);
  font-size: 15px;
}

.who-field {
  font-family: var(--font-display);
  font-size: 26px;
}

.drivers {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 6px 0 0;
  padding: 0;
  list-style: none;
}

.driver {
  display: grid;
  align-items: center;
  gap: 4px 10px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--surface);
  grid-template-columns: 22px 1fr 18px;
  animation: arrive 0.4s cubic-bezier(0.2, 0, 0, 1) both;
}

.driver:nth-child(2) {
  animation-delay: 60ms;
}

.driver:nth-child(3) {
  animation-delay: 120ms;
}

.driver:nth-child(4) {
  animation-delay: 180ms;
}

@keyframes arrive {
  from {
    opacity: 0;
    transform: translateX(10px);
  }
}

.logos {
  display: flex;
}

.id {
  font-size: 13px;
}

.level {
  color: var(--muted);
  font-size: 12.5px;
  grid-column: 2 / 4;
}

.who-tag {
  align-self: flex-start;
  padding: 4px 11px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--actor) 13%, transparent);
  color: var(--actor);
  font-size: 13.5px;
  animation: arrive 0.4s cubic-bezier(0.2, 0, 0, 1) both;
}

.who-note {
  margin: 6px 0 0;
  color: var(--ink-2);
  font-size: 15px;
  line-height: 1.5;
}

.who-note code {
  font-size: 13px;
}
</style>
