// Reads a profile as the studio shows it: findings with their place in the text, and the places where a value is one
// of a known set, so the editor can offer a dropdown there.

import type { Node, Pair } from 'yaml'

import type { Field } from './schema'

import { isMap, isScalar, isSeq, parseDocument } from 'yaml'

import { PROFILE, SIGNALS } from './schema'

export interface Finding {
  from: number
  to: number
  severity: 'error' | 'warning'
  message: string
}

/** A value the editor can offer choices for: an enum value, or a scalar with suggestions. */
export interface Choice {
  from: number
  to: number
  value: string
  values: readonly string[]
  path: string
}

export interface Checked {
  /** The profile as plain data, or undefined when the text is not YAML. */
  data: Record<string, unknown> | undefined
  findings: Finding[]
  choices: Choice[]
}

function range(node: Node | null | undefined): [number, number] {
  const r = node?.range
  return r ? [r[0], r[1]] : [0, 0]
}

function walk(node: Node | null | undefined, field: Field, path: string, out: Checked): void {
  if (!node || field.kind === 'any')
    return
  const [from, to] = range(node)
  switch (field.kind) {
    case 'map':
    case 'named': {
      if (!isMap(node)) {
        out.findings.push({ from, to, severity: 'error', message: `${path || 'the profile'} should be a mapping` })
        return
      }
      const keys = new Set<string>()
      for (const pair of node.items as Pair<Node, Node>[]) {
        const key = isScalar(pair.key) ? String(pair.key.value) : ''
        keys.add(key)
        const [kf, kt] = range(pair.key)
        const at = path ? `${path}.${key}` : key
        if (field.kind === 'map') {
          const inner = field.fields[key]
          if (!inner) {
            const known = Object.keys(field.fields).join(', ')
            out.findings.push({ from: kf, to: kt, severity: 'error', message: `unknown key \`${at}\`; ${path || 'a profile'} takes ${known}` })
            continue
          }
          walk(pair.value, inner, at, out)
        }
        else {
          walk(pair.value, field.extra?.[key] ?? field.of, at, out)
        }
      }
      if (field.kind === 'map') {
        for (const key of field.required ?? []) {
          if (!keys.has(key))
            out.findings.push({ from, to: Math.min(to, from + 1), severity: 'error', message: `${path || 'the profile'} needs \`${key}\`` })
        }
      }
      return
    }
    case 'list':
      if (!isSeq(node)) {
        out.findings.push({ from, to, severity: 'error', message: `${path} should be a list` })
        return
      }
      node.items.forEach((item, i) => walk(item as Node, field.of, `${path}[${i}]`, out))
      return
    case 'enum':
    case 'scalar': {
      if (!isScalar(node))
        return
      const value = String(node.value)
      const values = field.kind === 'enum' ? field.values : field.suggest
      if (values)
        out.choices.push({ from, to, value, values, path })
      if (field.kind === 'enum' && !field.values.includes(value))
        out.findings.push({ from, to, severity: 'error', message: `${path} is \`${value}\`; it takes ${field.values.join(', ')}` })
    }
  }
}

/** The place of the value at `path` (keys and list indices), for a cross-reference finding. */
function placeOf(doc: ReturnType<typeof parseDocument>, path: (string | number)[]): [number, number] {
  const node = doc.getIn(path, true) as Node | undefined
  return node ? range(node) : [0, 0]
}

type Report = (path: (string | number)[], message: string) => void

const EXEC_KEYS = ['exit', 'signal', 'stderr', 'hang', 'delay', 'probability', 'nth', 'times', 'after', 'until']
const FILE_KEYS = ['errno', 'delay', 'probability', 'nth', 'times', 'after', 'until']
const NET_KEYS = ['refuse', 'reset', 'status', 'hang', 'delay', 'probability', 'nth', 'times', 'after', 'until']
const OFFERS: Record<string, string> = {
  exec: 'an exec fault takes exit, signal, hang or delay (and stderr), with probability, nth, times, after or until',
  file: 'a file fault takes errno or delay, with probability, nth, times, after or until',
  net: 'a net fault takes refuse, reset, hang, status or delay, with probability, nth, times, after or until',
  sensor: 'a sensor fault takes only `at`',
}

/**
 * What needy refuses in one fault as it loads it: exactly one domain, only that domain's effects and triggers, and
 * effects that go together. The messages are needy's.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/chaos.rs#L242-L431
 */
// NOTICE: needy also spells a signal without `SIG` as `SIGNAME`; the studio accepts both spellings, as needy does.
// TODO(docs-studio): `delay`, `after`, `until` and `at` are durations and `until` must be later than `after`; that
// comparison needs the duration parser the value-type deferral in schema.ts leaves out.
function faultProblem(f: Record<string, any>): string | undefined {
  const domains = (['exec', 'file', 'net', 'sensor'] as const).filter(d => f[d] !== undefined && f[d] !== null)
  if (domains.length === 0)
    return 'a fault names one domain: `exec`, `file`, `net` or `sensor`'
  if (domains.length > 1)
    return `a fault names one domain, not ${domains.join(' and ')}`
  const domain = domains[0]!
  const set = (key: string) => key in f && f[key] !== null && f[key] !== false
  const allowed = { exec: EXEC_KEYS, file: FILE_KEYS, net: NET_KEYS, sensor: ['at'] }[domain]
  const stray = [...EXEC_KEYS, ...FILE_KEYS, ...NET_KEYS, 'at'].find(key => set(key) && !allowed.includes(key))
  if (stray)
    return `${stray} is not for ${domain} faults; ${OFFERS[domain]}`
  if (domain === 'exec') {
    const ends = ['exit', 'signal', 'hang'].filter(set).length
    if (ends > 1)
      return 'an exec fault ends the command one way: exit, signal or hang'
    if (set('stderr') && ends === 0)
      return 'stderr is written before exit, signal or hang; add one'
    if (ends === 0 && !set('delay'))
      return `an exec fault needs an effect; ${OFFERS.exec}`
    if (typeof f.exit === 'number' && (f.exit < 0 || f.exit > 255))
      return `exit ${f.exit} is not an exit status (0 to 255)`
    if (set('signal')) {
      const name = String(f.signal).startsWith('SIG') ? String(f.signal) : `SIG${f.signal}`
      if (!(SIGNALS as readonly string[]).includes(name))
        return `signal ${f.signal} does not end a process; one of ${SIGNALS.join(', ')}`
    }
    if (typeof f.exec === 'string' && !f.exec.trim())
      return 'exec needs a command pattern, such as `curl **` (`**` for every command)'
  }
  if (domain === 'file' && !set('errno') && !set('delay'))
    return `a file fault needs an effect; ${OFFERS.file}`
  if (domain === 'net') {
    const connect = f.net?.connect !== undefined
    const request = f.net?.request !== undefined
    if (connect && request)
      return 'net matches `connect` or `request`, not both'
    if (!connect && !request)
      return 'net needs `connect: HOST` or `request: HOST`'
    const ends = ['refuse', 'reset', 'hang', 'status'].filter(set).length
    if (ends > 1)
      return 'a net fault fails the connection one way: refuse, reset, hang or status'
    if (ends === 0 && !set('delay'))
      return `a net fault needs an effect; ${OFFERS.net}`
    if (set('status')) {
      if (!request)
        return 'status answers a plain-HTTP request; match it with `net: { request: HOST }` (a CONNECT tunnel is opaque)'
      if (typeof f.status === 'number' && (f.status < 100 || f.status > 599))
        return `status ${f.status} is not an HTTP status`
    }
  }
  if (typeof f.probability === 'number' && (f.probability < 0 || f.probability > 1))
    return `probability ${f.probability} is not between 0 and 1`
  if (f.nth === 0)
    return 'nth counts matches from 1'
  if (f.times === 0)
    return 'times: 0 would never fire'
  if (set('nth') && (set('probability') || set('times')))
    return 'nth fires on one match; it takes no probability or times'
  return undefined
}

/**
 * Whether `name` may name a fault: a word of letters, digits, `-`, `_` and `.`, at most 64.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/chaos.rs#L506-L511
 */
const faultName = (name: string) => /^[\w.-]{1,64}$/.test(name)

/**
 * What is wrong with a service, if anything: its name becomes the host `NAME.needy`, so it is a DNS label, and its
 * URL is an absolute `http://` or `https://` URL.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/validate.rs#L121-L141
 */
// NOTICE: needy parses the URL with the `http` crate's `Uri`; the WHATWG `URL` here accepts a few strings `Uri`
// refuses (spaces, for one), so the daemon's check stays the real one.
function serviceProblem(name: string, url: unknown): string | undefined {
  if (!/^[a-z0-9-]{1,63}$/.test(name) || name.startsWith('-') || name.endsWith('-'))
    return 'a service name is a DNS label: lowercase letters, digits and `-`, at most 63'
  let parsed: URL
  try {
    parsed = new URL(String(url))
  }
  catch {
    return `url "${url}" is not a URL`
  }
  if (!['http:', 'https:'].includes(parsed.protocol) || !parsed.host)
    return `url "${url}" must be an absolute http:// or https:// URL`
  return undefined
}

/**
 * What needy refuses when it derives the file mapping and path map of `actor.workspace` (load errors, not findings).
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/workspace.rs#L25-L146
 */
// NOTICE: needy also refuses a `path_map` entry for the workspace's path and checks `path` after `~` expands to the
// profile user's home; the studio checks the home by `user.name` only, the way `User::home` derives it.
function workspace(data: Record<string, any>, error: Report): void {
  const w = data.actor?.workspace
  if (!w || typeof w !== 'object' || typeof w.path !== 'string')
    return
  const home = `/home/${data.user?.name ?? 'agent'}`
  const path = w.path.replace(/^~(?=\/|$)/, home).replace(/(.)\/+$/, '$1')
  if (!path.startsWith('/'))
    return error(['actor', 'workspace', 'path'], `actor.workspace.path: "${path}" must be absolute or start with \`~\``)
  const from = typeof w.from === 'string' ? w.from : undefined
  if (from?.startsWith('actor:'))
    return error(['actor', 'workspace', 'from'], 'actor.workspace.from: the workspace comes from `executor:NAME:PATH` or `host:PATH`; leave `from` out for a fresh directory in the actor')
  const inHome = path === home || path.startsWith(`${home}/`)
  if (!from?.startsWith('executor:') && !inHome)
    return error(['actor', 'workspace', 'path'], `actor.workspace.path: ${path} is outside the user's home ${home}; without an executor mount the workspace is a directory in the actor's home`)
  if (!from)
    return
  if (from.startsWith('executor:')) {
    const name = from.split(':')[1]!
    if (!(name in (data.executors ?? {})))
      return error(['actor', 'workspace', 'from'], `actor.workspace.from: names unknown executor "${name}"`)
  }
  const derived = from.startsWith('host:') && w.writable ? ['workspace', 'workspace-out'] : ['workspace']
  for (const name of derived) {
    if (name in (data.files ?? {}))
      error(['files', name], `files.${name}: actor.workspace defines this mapping; move the workspace's settings to actor.workspace or rename the mapping`)
  }
}

/**
 * The cross-references needy checks after it parsed a profile.
 * Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/validate.rs#L55-L166
 */
function references(doc: ReturnType<typeof parseDocument>, data: Record<string, any>, out: Checked): void {
  const error: Report = (path, message) => {
    const [from, to] = placeOf(doc, path)
    out.findings.push({ from, to, severity: 'error', message })
  }
  const actor = data.actor ?? {}
  const runtime = actor.container?.oci_runtime
  if (actor.driver === 'linux-container' && !runtime)
    error(['actor', 'driver'], 'a linux-container actor needs `container: { oci_runtime: runc }` (or `runsc`, gVisor)')
  if (actor.driver === 'macos-apple-container' && runtime)
    error(['actor', 'container', 'oci_runtime'], 'actor.container.oci_runtime is for `driver: linux-container`; Apple\'s `container` has no OCI runtime to choose')
  if ((actor.driver === 'linux-firecracker' || actor.driver === 'macos-vz') && actor.container)
    error(['actor', 'container'], `actor.container is for \`driver: linux-container\` or \`macos-apple-container\`; this actor's driver is ${actor.driver}`)
  workspace(data, error)

  const executors: Record<string, any> = data.executors ?? {}
  const files: Record<string, any> = data.files ?? {}
  // `actor.workspace` derives `workspace` (and `workspace-out`), which rules and run_files may name.
  const derived = actor.workspace?.from ? (String(actor.workspace.from).startsWith('host:') && actor.workspace.writable ? ['workspace', 'workspace-out'] : ['workspace']) : []
  const mapping = (name: string) => name !== 'ignore_file' && (name in files || derived.includes(name))
  const known = (name: string) => name in executors
  if (data.default_executor && !known(data.default_executor))
    error(['default_executor'], `default_executor names unknown executor "${data.default_executor}"`)
  if (typeof data.unknown === 'string' && !['sticky', 'actor'].includes(data.unknown) && !known(data.unknown))
    error(['unknown'], `unknown names unknown executor "${data.unknown}"`)
  for (const [name, e] of Object.entries(executors)) {
    if (e?.container && e?.driver !== 'linux-container')
      error(['executors', name, 'container'], `executors.${name} has \`container:\` options but its driver is ${e?.driver ?? 'native'}; add \`driver: linux-container\``)
  }
  for (const [name, service] of Object.entries<any>(actor.network?.services ?? {})) {
    const problem = serviceProblem(name, service?.url)
    if (problem)
      error(['actor', 'network', 'services', name], `actor.network.services.${name}: ${problem}`)
  }
  for (const [name, f] of Object.entries<any>(data.chaos?.faults ?? {})) {
    if (!faultName(name))
      error(['chaos', 'faults', name], `chaos.faults.${name}: a fault's name is a word of letters, digits, \`-\`, \`_\` and \`.\`, at most 64`)
    const problem = f && typeof f === 'object' ? faultProblem(f) : undefined
    if (problem)
      error(['chaos', 'faults', name], `chaos.faults.${name}: ${problem}`)
  }
  for (const [i, name] of (data.run_files ?? []).entries()) {
    if (!mapping(name))
      error(['run_files', i], `run_files names unknown file mapping "${name}"`)
  }
  const actions = (path: (string | number)[], at: string, a: Record<string, any>) => {
    for (const [j, name] of (a?.use ?? []).entries()) {
      if (!(name in (data.presets ?? {})))
        error([...path, 'use', j], `${at} uses unknown preset "${name}"`)
    }
    for (const key of ['to', 'executor'] as const) {
      const name = a?.[key]
      if (typeof name !== 'string' || name === 'actor')
        continue
      if (name === 'executor') {
        if (!a.executor && !data.default_executor)
          error([...path, key], `${at} sends to \`executor\`, but there is no default_executor`)
      }
      else if (!known(name)) {
        error([...path, key], `${at} sends to unknown executor "${name}"`)
      }
    }
    for (const [j, name] of (a?.files ?? []).entries()) {
      if (!mapping(name))
        error([...path, 'files', j], `${at} adds unknown file mapping "${name}"`)
    }
  }
  for (const [name, preset] of Object.entries<any>(data.presets ?? {}))
    actions(['presets', name], `presets.${name}`, preset)
  for (const [i, rule] of (data.commands ?? []).entries())
    actions(['commands', i], `commands[${i}] (${rule?.match})`, rule)
  for (const [name, m] of Object.entries(files)) {
    if (name === 'ignore_file')
      continue
    for (const end of ['from', 'to'] as const) {
      const side = m?.[end]
      const exec = typeof side === 'string' && side.startsWith('executor:') ? side.split(':')[1] : undefined
      if (exec !== undefined && !known(exec))
        error(['files', name, end], `files.${name}.${end} names unknown executor "${exec}"`)
    }
  }
}

export function check(text: string): Checked {
  const out: Checked = { data: undefined, findings: [], choices: [] }
  const doc = parseDocument(text, { keepSourceTokens: false })
  for (const e of doc.errors)
    out.findings.push({ from: e.pos[0], to: e.pos[1], severity: 'error', message: e.message.split('\n')[0]! })
  if (doc.errors.length > 0)
    return out
  walk(doc.contents as Node, PROFILE, '', out)
  const data = doc.toJS() as Record<string, any> | null
  if (data && typeof data === 'object') {
    out.data = data
    references(doc, data, out)
  }
  return out
}

/** Replaces the value at [from, to) and keeps the rest of the text. */
export function replace(text: string, from: number, to: number, value: string): string {
  return text.slice(0, from) + value + text.slice(to)
}
