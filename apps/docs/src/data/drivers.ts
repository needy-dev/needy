import type { LogoName } from '../kit/Logo.vue'

// The actor drivers and what each one supports, as the README's "Actor drivers" table states it.
// Source: README.md, "Actor drivers" (https://github.com/needy-dev/needy/blob/1efa6c8d1ab6b2bde0db2a8cf808167758601aa7/README.md)
// Evidence the table cites:
// - docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md (linux-firecracker, routing)
// - docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md#identity (linux-container)
// - docs/ai/references/architecture/2026-11-12-macos-vz-actor-evidence.md (macos-vz)
// - docs/ai/references/architecture/2026-11-12-macos-apple-container-actor-evidence.md (macos-apple-container)
// NOTICE: when the README table changes, change this file with it.

const REPO = 'https://github.com/needy-dev/needy/blob/main'

/** ✅ verified end to end, ➖ expected to work but not yet recorded, ❌ not possible with this driver. */
export type Level = 'yes' | 'expected' | 'no'

export const LEVELS: Readonly<Record<Level, { symbol: string, label: string, meaning: string }>> = {
  yes: { symbol: '✅', label: 'Verified', meaning: 'verified end to end' },
  expected: { symbol: '➖', label: 'Expected', meaning: 'expected to work but not yet recorded' },
  no: { symbol: '❌', label: 'Not possible', meaning: 'not possible with this driver' },
}

export type DriverId = 'linux-firecracker' | 'linux-container' | 'macos-vz' | 'macos-apple-container'

export interface Driver {
  id: DriverId
  /** What the driver runs the actor in, in a few words. */
  title: string
  logos: LogoName[]
  host: string
  isolation: string
  capture: string
  ready?: string
  evidence: { label: string, url: string }
}

export const DRIVERS: readonly Driver[] = [
  {
    id: 'linux-firecracker',
    title: 'Firecracker microVM',
    logos: ['firecracker'],
    host: 'Linux x86_64 with KVM',
    isolation: 'Firecracker microVM, needy\'s kernel',
    capture: 'kernel module binfmt_needy',
    ready: '~0.17 s',
    evidence: { label: 'Closed-loop results', url: `${REPO}/docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md` },
  },
  {
    id: 'linux-container',
    title: 'Container',
    logos: ['docker', 'gvisor'],
    host: 'Linux with a Docker-compatible engine',
    isolation: 'runc (shared kernel) or runsc (gVisor)',
    capture: 'shims on every PATH program of the image',
    // Not in the README table at the commit above. Measured on 2026-11-13 on a Linux KVM host (Intel Core i5-13600KF,
    // 20 threads, Docker 29.7.1) at needy commit 90a8e8e: a release build, `actor.driver: linux-container` with runc,
    // `image: needy-actor:latest`, `recipe: builtin:gce-ubuntu-22.04`, no executors, `needy run create -t` then
    // `run stop`, 12 runs; the time from the trace's `run.create` to its `actor.ready`. Median 252 ms, 191 to 270 ms.
    // REVIEW: the README's other ready times do not say what they span; this one includes creating the container.
    ready: '~0.25 s',
    evidence: { label: 'Container actor', url: `${REPO}/docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md#identity` },
  },
  {
    id: 'macos-vz',
    title: 'Virtualization.framework',
    logos: ['apple'],
    host: 'Apple silicon macOS',
    isolation: 'Virtualization.framework VM through vfkit, needy\'s arm64 kernel',
    capture: 'kernel module binfmt_needy',
    ready: '~0.27 s',
    evidence: { label: 'macos-vz evidence', url: `${REPO}/docs/ai/references/architecture/2026-11-12-macos-vz-actor-evidence.md` },
  },
  {
    id: 'macos-apple-container',
    title: 'Apple container',
    logos: ['apple'],
    host: 'Apple silicon macOS',
    isolation: 'Apple container: one VM per container, Apple\'s kernel',
    capture: 'shims',
    ready: '~0.86 s',
    evidence: { label: 'macos-apple-container evidence', url: `${REPO}/docs/ai/references/architecture/2026-11-12-macos-apple-container-actor-evidence.md` },
  },
]

export interface Cell {
  level: Level
  /** The README's note on the cell, such as `arm64`. */
  note?: string
}

export interface Feature {
  id: string
  label: string
  /** By driver, in the order of DRIVERS. */
  cells: [Cell, Cell, Cell, Cell]
}

export interface Group {
  id: 'routing' | 'identity'
  label: string
  features: Feature[]
}

const y: Cell = { level: 'yes' }
const n: Cell = { level: 'no' }
const e: Cell = { level: 'expected' }

export const GROUPS: readonly Group[] = [
  {
    id: 'routing',
    label: 'Routing',
    features: [
      { id: 'name', label: 'Route a command by name', cells: [y, y, y, y] },
      { id: 'path', label: 'Route by absolute path', cells: [y, y, y, y] },
      { id: 'runtime', label: 'Route programs added at run time', cells: [y, n, y, n] },
      { id: 'gateway', label: 'Network only through the gateway', cells: [y, y, y, y] },
      { id: 'mount', label: 'Workspace mount from an executor', cells: [y, y, y, y] },
      { id: 'copy', label: 'Copy-in, mirror-out', cells: [y, y, y, y] },
      { id: 'faults', label: 'Exec and file faults', cells: [y, y, y, y] },
      { id: 'interactive', label: 'Interactive (-it, attach, detach)', cells: [y, e, y, y] },
    ],
  },
  {
    id: 'identity',
    label: 'Identity from the recipe',
    features: [
      { id: 'hostname', label: 'Hostname', cells: [y, y, y, y] },
      { id: 'cpuinfo', label: '/proc/cpuinfo', cells: [y, y, { level: 'no', note: 'arm64' }, n] },
      { id: 'cpuid', label: 'CPUID', cells: [y, n, n, n] },
      { id: 'dmi', label: 'DMI / SMBIOS', cells: [y, { level: 'yes', note: 'where the host has DMI; any host under runsc' }, n, n] },
      { id: 'pci', label: 'PCI IDs', cells: [y, n, n, n] },
      { id: 'hwmon', label: 'hwmon sensors and the overheat fault', cells: [y, { level: 'yes', note: 'where the host has hwmon; any host under runsc' }, y, n] },
      { id: 'kernel', label: 'Kernel version', cells: [y, { level: 'no', note: 'host or gVisor' }, e, n] },
      { id: 'uptime', label: 'Uptime offset', cells: [y, n, e, n] },
    ],
  },
]

/** The cells of feature `id` in group `group`. */
export function feature(group: Group['id'], id: string): Feature {
  return GROUPS.find(g => g.id === group)!.features.find(f => f.id === id)!
}
