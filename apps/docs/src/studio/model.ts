// What the studio draws from a profile: the actor, the executors, the rules, files, network, chaos and trace, read
// from the profile's plain data. Missing parts take needy's defaults where the studio shows them.

import type { IconName } from '../icons'
import type { LogoName } from '../kit/Logo.vue'
import type { Rule } from '../route/rules'

/**
 * What each actor driver is, after the README's "Actor drivers" table.
 * Source: https://github.com/needy-dev/needy/blob/4a996b82f3d26665f24d5838d4415d18c64f881a/README.md#actor-drivers
 */
export const ACTOR_DRIVER_INFO: Record<string, { logo: LogoName, isolation: string, host: string, capture: string }> = {
  'linux-firecracker': { logo: 'firecracker', isolation: 'Firecracker microVM, needy\'s kernel', host: 'Linux x86_64 with KVM', capture: 'binfmt_needy' },
  'linux-container': { logo: 'docker', isolation: 'runc (shared kernel) or runsc (gVisor)', host: 'Linux with a Docker-compatible engine', capture: 'shims' },
  'macos-vz': { logo: 'apple', isolation: 'Virtualization.framework VM through vfkit', host: 'Apple silicon macOS', capture: 'binfmt_needy' },
  'macos-apple-container': { logo: 'apple', isolation: 'Apple container: one VM per container', host: 'Apple silicon macOS', capture: 'shims' },
}

export interface ExecutorView {
  name: string
  driver: string
  runner?: string
  key?: string
  image?: string
  workspace?: string
  icon: IconName
  title: string
}

export interface FileView {
  name: string
  mode: string
  from: string
  to: string
  run: boolean
}

export interface ProfileView {
  actor: {
    driver: string
    runtime?: string
    recipe?: string
    runner?: string
    command?: string
    allow: string[]
    /** `actor.network.services`: `NAME.needy` and where the gateway forwards it. */
    services: Array<{ host: string, url: string }>
  }
  executors: ExecutorView[]
  rules: Rule[]
  files: FileView[]
  chaos: { seed?: number, faults: Array<{ name: string, on: string, text: string }> }
  trace: string[]
}

const words = (value: unknown): string | undefined => Array.isArray(value) ? value.map(String).join(' ') : typeof value === 'string' ? value : undefined

const runnerOf = (value: unknown): string | undefined => typeof value === 'string' ? value : value && typeof value === 'object' && 'labels' in value ? `labels ${(value as { labels: string[] }).labels.join(', ')}` : undefined

export function viewOf(data: Record<string, any>): ProfileView {
  const actor = data.actor ?? {}
  const network = actor.network ?? {}
  const executors: ExecutorView[] = Object.entries<any>(data.executors ?? {}).map(([name, e]) => {
    const driver = e?.driver ?? 'native'
    const runner = runnerOf(e?.runner)
    return {
      name,
      driver,
      runner,
      key: e?.key,
      image: e?.container?.image,
      workspace: e?.workspace,
      icon: driver === 'linux-container' ? 'mingcute:cube-line' : runner ? 'mingcute:laptop-line' : 'mingcute:server-2-line',
      title: driver === 'linux-container' ? 'Container' : runner ? `Runner ${runner}` : 'This machine',
    }
  })
  const rules: Rule[] = (data.commands ?? []).filter((r: any) => r && typeof r.match === 'string').map((r: any) => {
    // `to: executor` takes the rule's `executor:`, or the default (crates/core/src/rules/mod.rs, to_placement).
    const to = r.to === 'executor' ? (r.executor ?? data.default_executor) : r.to
    return { match: r.match, ...(r.deny !== undefined ? { deny: String(r.deny) } : {}), ...(to !== undefined ? { to: String(to) } : {}), ...(r.final ? { final: true } : {}) }
  })
  const runFiles: string[] = data.run_files ?? []
  const files: FileView[] = Object.entries<any>(data.files ?? {}).filter(([name]) => name !== 'ignore_file').map(([name, m]) => ({
    name,
    mode: m?.mode ?? '',
    from: String(m?.from ?? ''),
    to: String(m?.to ?? ''),
    run: runFiles.includes(name),
  }))
  // The mappings `actor.workspace` derives, which every run uses (crates/config/src/profile/workspace.rs, `derive`).
  const ws = actor.workspace
  if (ws && typeof ws.path === 'string' && typeof ws.from === 'string') {
    const from = String(ws.from)
    files.push({ name: 'workspace', mode: from.startsWith('host:') ? 'copy-in' : 'mount', from, to: `actor:${ws.path}`, run: true })
    if (from.startsWith('host:') && ws.writable)
      files.push({ name: 'workspace-out', mode: 'mirror-out', from: `actor:${ws.path}`, to: from, run: true })
  }
  const domains = ['exec', 'file', 'net', 'sensor'] as const
  const faults = Object.entries<any>(data.chaos?.faults ?? {}).map(([name, f]) => {
    const on = domains.find(d => f?.[d] !== undefined) ?? ''
    const subject = on === 'exec' ? f.exec : on === 'file' ? f.file?.open : on === 'net' ? (f.net?.connect ?? f.net?.request) : undefined
    return {
      name,
      on,
      text: [subject, f?.exit !== undefined && `exit ${f.exit}`, f?.signal, f?.errno, f?.refuse && 'refuse', f?.reset && 'reset', f?.status !== undefined && `status ${f.status}`, f?.hang && 'hang', f?.delay && `+${f.delay}`, f?.sensor?.temp_c !== undefined && `${f.sensor.temp_c} °C`, f?.at && `at ${f.at}`, f?.probability !== undefined && `p ${f.probability}`, f?.nth && `nth ${f.nth}`, f?.times && `×${f.times}`].filter(Boolean).join(' · '),
    }
  })
  return {
    actor: {
      driver: actor.driver ?? '',
      runtime: actor.container?.oci_runtime,
      recipe: actor.recipe,
      runner: runnerOf(actor.runner),
      command: words(actor.command),
      allow: (network.allow ?? []).map(String),
      services: Object.entries<any>(network.services ?? {}).map(([name, s]) => ({ host: `${name}.needy`, url: String(s?.url ?? '') })),
    },
    executors,
    rules,
    files,
    chaos: { seed: data.chaos?.seed, faults },
    // The default sinks: crates/config/src/profile/trace.rs, `Trace::default`.
    trace: data.trace?.sinks ?? ['sqlite', 'jsonl'],
  }
}
