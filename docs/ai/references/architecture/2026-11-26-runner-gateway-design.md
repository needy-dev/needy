# S1: the actor gateway moves into the actor runner

Status: **implemented on `feat/runner-gateway`, not released**, 2026-11-26.
Owner-approved slice ("S1: the gateway moves into the actor runner").
Evidence level: **code** and in-process tests (listed below); no cluster
probe yet.

Related notes:

- [Integrating needy into `rc`](2026-11-26-rc-integration-guide.md), sections 1, 5 and 8
- [Remote actor host and runner ergonomics (Phase 1)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md)
- [Drivers and extension points](2026-11-12-drivers-and-extension-points-design.md) (the gateway is a sidecar)

## Problem

A remote actor runner (`needy runner serve --roles actor`) piped the VM's
gateway channel (`v.sock_7001`) to the daemon's shared gateway listener
(`--gateway-listen`), and the daemon ran the actor gateway. On Kubernetes
that made every actor's egress leave from the tenant's daemon Pod: one
egress IP for all actors, a daemon NetworkPolicy open to the internet, and
the daemon in the data path of every request.

## Decision

The runner runs the actor's gateway itself, next to the actor. The daemon
stays the policy source and the trace owner. Local single-host runs are
unchanged: the daemon runs their gateway as before.

### Policy: what the runner enforces

The runner builds the gateway with the daemon's one constructor
(`crates/daemon/src/actor.rs`, `actor::gateway`), from:

| Input | Where it comes from |
| --- | --- |
| Allowlist and `actor.network.services` | `BridgeAttached.profile_yaml`, which the runner already received |
| Network faults and `chaos.seed` | the same profile |
| Fault windows (`after`, `until`) | counted from `BridgeAttached.created_ms`, the run's creation on the daemon's clock (`Injector::since_ms`) |
| Blocked CIDRs | `BridgeAttached.gateway_blocked_cidrs`, the daemon's `--gateway-block-cidr`; there is no runner flag |
| Loopback, link-local, ULA and unspecified blocking | the gateway's default `EgressPolicy`, as on the daemon |

A blocked CIDR the runner cannot parse fails the actor's start, reported as
the actor's exit, so the run fails with the reason rather than running with
a weaker policy.

NOTICE: fault windows compare the daemon's creation time with the runner's
wall clock; a clock skew between the two hosts shifts the windows by that
much.

### Trust shift

The allowlist was enforced in the daemon's process; it is now enforced in
the runner's. A runner already held the run's actor credential and its VM's
host side, so a compromised actor Pod could already act as that run's
actor; it can now also bypass the run's allowlist for that actor's own
traffic. It cannot reach other runs, and it cannot write the run's
lifecycle into the trace: the daemon takes only what a gateway records
(below).

### Events and ordering

The runner's gateway records into a `Dispatch` of its own whose sink
forwards over the bridge (`BridgeRequest.events`, `BridgeEvents`: batches
of `TraceEvent {time_ms, kind, payload, version}`, at most 64 per
message). The daemon decodes each with `Event::decode` (refusing a schema
version it does not read) and records it with `run.trace.emit_at`, the
`PushEvents` path.

- `time_ms` is the runner's time of the event.
- `seq` on the daemon is **arrival order**. The bridge keeps the gateway's
  order, but an event of the daemon's own (an `exec.decide`, say) may land
  between two gateway events that were on their way, so `seq` order and
  `time_ms` order can differ across sources.
- The daemon takes only `net.request`, `net.connect`, `net.deny`,
  `net.error`, `fault.injected` of the `net` domain, and `trace.gap`; any
  other kind is dropped with a warning. A runner that did not get
  `local_gateway` gets its events dropped, so a request is never recorded
  twice.
- Drops: the runner's queue (1024 events) drops when the bridge is slow and
  records a `trace.gap`, which is forwarded like any event. The daemon's own
  queue drops and records its own gap as before.
- At the actor's end the runner flushes its trace (up to 3 s) before it
  reports `BridgeExit`, so the gateway's last events precede the actor's
  exit.

### Activity

The gateway's `on_activity` sets a flag; the runner sends `BridgeActivity`
at most once a second while the flag was set (`GATEWAY_ACTIVITY_INTERVAL`),
and the daemon calls `touch_activity`. `last_activity_ms` keeps advancing
for network-only work. Forwarded events do not count as activity; only
`BridgeActivity` does, as only a request counted through the daemon's
gateway.

### Bridge loss is a kill switch

When the bridge breaks, the runner cancels its gateway first (open
connections and tunnels end), then stops the VM, as before S1. Revoking the
runner's credential or stopping the run therefore still cuts the actor's
egress.

### Mixed versions

| Runner | Daemon | Result |
| --- | --- | --- |
| S1 | S1 | The runner sends `BridgeAttach.local_gateway`; the daemon answers `BridgeAttached.local_gateway`, closes the gateway it registered when it placed the run, and refuses that run on its shared gateway listener. The runner serves the gateway. |
| older | S1 | `BridgeAttach.local_gateway` is unset: the daemon keeps its gateway and the runner proxies to `--gateway-listen`, as before. |
| S1 | older | `BridgeAttached.local_gateway` is unset: the runner proxies to its `--gateway`, as before; it runs no gateway. |

Exactly one gateway enforces a run's network in every row. `--gateway-listen`
on the daemon and `--gateway` on the runner stay, for older peers.

### Seam for an upstream proxy

Every connection the gateway opens goes through
`needy_core_actor::sidecar::gateway::Upstream` (only `Direct` exists).
`TODO(gateway-upstream)`: a per-actor upstream proxy (`socks5h`) is the
next slice after S1 (owner discussed 2026-11-26); not implemented.

## Deferred (with markers in code)

- `needy chaos add` and `rm` of a **network** fault on a run whose runner
  runs the gateway are refused with `FailedPrecondition` (`ControlError::Failed`),
  as is one on a remote run whose runner has not attached yet; other
  domains are unaffected. Pushing them to the runner needs a new
  `BridgeResponse` case, as `Guest::control` streams exec route changes
  (`TODO(runner-gateway-chaos)` in `crates/daemon/src/chaos.rs`).
- `needy chaos ls`: a runner-gateway run's network faults match and fire on
  the runner, so their `matched` and `fired` counts on the daemon stay at
  their placement values; the `fault.injected` events in the trace are
  current (`TODO(runner-gateway-chaos)` in `Daemon::faults`).
- Per-actor upstream proxy: `TODO(gateway-upstream)`.

## Networking consequences on Kubernetes

- The actor's egress IP is the **actor Pod's** (its node's, after SNAT),
  not the daemon's.
- The actor Pod's NetworkPolicy needs internet egress, minus the cluster's
  pod, service and node ranges and `169.254.169.254`, plus the in-cluster
  services its profiles name in `actor.network.services`, plus the daemon's
  7400 and DNS. Pass the same ranges to the daemon's `--gateway-block-cidr`:
  the runner enforces them inside the Pod, where a NetworkPolicy does not
  see loopback.
- The daemon no longer needs internet egress for actors (it may still need
  it for the layer registry and image pulls), and 7401 is needed only for
  older runners.

## Tests

In-process, next to the code:

- `crates/daemon/src/actor_runner_test.rs`:
  `a_runner_runs_the_remote_actor_s_gateway_under_the_control_plane_s_policy_and_reports_to_its_trace`
  (a runner joined in process with a stand-in Firecracker: a service request
  passes, a denied host gets 403 and its `net.deny` reaches the daemon's
  trace, the daemon's blocked CIDR is enforced by the runner, the daemon's
  gateway is closed, `last_activity_ms` advances) and
  `the_runner_s_gateway_counts_fault_windows_from_the_run_s_creation_on_the_control_plane`.
- `crates/daemon/src/bridge_test.rs`:
  `a_runner_that_runs_the_gateway_gets_the_policy_and_its_events_and_activity_reach_the_run`
  (attach fields, event filtering, `BridgeActivity`, net-fault refusal) and
  `an_older_runner_without_a_gateway_of_its_own_keeps_the_daemon_s_gateway`.
- `crates/core/src/chaos/mod_test.rs`:
  `a_time_window_counts_from_a_creation_time_in_unix_milliseconds`.

Not covered by a test: an S1 runner against an older daemon (the proxy
path is the unchanged pre-S1 code, selected when `local_gateway` is unset).
