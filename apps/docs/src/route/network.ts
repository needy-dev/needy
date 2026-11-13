// Whether the gateway lets a host through: the first `net.*` interceptor.
//
// Source: https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/core/src/network.rs#L13-L60
// `Glob::matches_host`, https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/value.rs#L280-L284
// and `service_host`, https://github.com/needy-dev/needy/blob/90a8e8ee3cf1d9010dc7c5a9945f20288700a1d5/crates/config/src/profile/network.rs#L49-L51
// NOTICE: a port, so the page decides as the gateway does. A host matches a pattern in any case; `*.example.com` also
// matches `example.com`; a service's host in the actor, `NAME.needy`, is allowed. picomatch with `dot: true` stands in
// for glob_match.

import picomatch from 'picomatch'

export interface NetPolicy {
  allow: readonly string[]
  /** Services by name, as `actor.network.services` writes them: the name to the URL the gateway forwards to. */
  services: Readonly<Record<string, string>>
}

export function matchesHost(pattern: string, host: string): boolean {
  const p = pattern.toLowerCase()
  const h = host.toLowerCase()
  return (p.startsWith('*.') && p.slice(2) === h) || picomatch.isMatch(h, p, { dot: true })
}

/** A service's host in the actor. */
export const serviceHost = (name: string): string => `${name}.needy`

/** The pattern or service that lets `host` through, or undefined when the gateway refuses it. */
export function allowedBy(policy: NetPolicy, host: string): { kind: 'allow', pattern: string } | { kind: 'service', name: string, url: string } | undefined {
  const pattern = policy.allow.find(p => matchesHost(p, host))
  if (pattern)
    return { kind: 'allow', pattern }
  const name = Object.keys(policy.services).find(n => serviceHost(n).toLowerCase() === host.toLowerCase())
  return name === undefined ? undefined : { kind: 'service', name, url: policy.services[name]! }
}
