# S1: the actor gateway moves into the actor runner

Status: **released in v0.0.19** ([#118](https://github.com/needy-dev/needy/pull/118)),
2026-11-26; written on `feat/runner-gateway` before the release.
Owner-approved slice ("S1: the gateway moves into the actor runner").
Evidence level: **code**, in-process tests (listed below) and a cluster
probe ("Evidence" below); also exercised across hosts in the
[multi-host pass](2026-11-26-multi-host-pass-evidence.md).

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

### Upstream proxy

Released in v0.0.19 with S1 (#118). Owner-approved slice
("per-actor upstream SOCKS proxy", 2026-11-26), written on
`feat/gateway-upstream`, stacked on S1. Evidence level: **code**,
in-process tests (below) and the cluster probe ("Evidence" below).

On Kubernetes each actor runs in its own ephemeral actor runner Pod, and
each actor should leave through a SOCKS proxy of its own. The orchestrator
puts the proxy in the runner Pod's arguments and its credentials in a
Secret:

```sh
needy runner serve --roles actor --ephemeral \
  --gateway-upstream socks5h://proxy-7.example.com:1080 \
  --gateway-upstream-credentials-file /etc/needy/upstream/credentials
```

`needy daemon serve` takes the same two flags for the gateways it runs
itself (local actors, and older runners that proxy to it).

| Decision | Choice and reason |
| --- | --- |
| Where it plugs in | `gateway::Upstream` (`crates/core-actor/src/sidecar/gateway/upstream.rs`), the seam S1 left: `Direct` or `Socks5`. Both the plain-HTTP client (`http.rs`, through `ValidatedConnector`) and `CONNECT` tunnels (`connect.rs`) dial through it. `actor::gateway` takes it, so the daemon and the runner build it the same way. |
| Schemes | `socks5h://HOST[:PORT]`: the gateway sends the target's name to the proxy (address type DOMAINNAME), never resolving it. `socks5://HOST[:PORT]`: the gateway resolves the name, checks every address against `EgressPolicy` as a direct connection does, and asks the proxy for the address. Port 1080 by default (RFC 1928 section 3). No HTTP proxies (`TODO(gateway-upstream)` at the enum). |
| Client | `tokio-socks` 0.5.3 (MIT; the SOCKS client `reqwest` uses): RFC 1928 CONNECT with a name or an address, RFC 1929 username/password. The gateway resolves and connects to the proxy itself (the crate resolves a proxy name with blocking `std` calls). |
| Services | `actor.network.services` URLs are always dialed directly, never through the upstream: they name the operator's endpoints, in-cluster on Kubernetes, which the proxy would not reach. |
| Policy order | The allowlist and chaos decide first, unchanged; only an allowed connection reaches the upstream. |
| Fail closed | A configured upstream is never bypassed. An unreachable proxy, a refusal (any SOCKS reply other than success), an authentication failure or an unreadable credentials file fails the connection: `502` to the actor and a `net.error` with the reason; nothing is dialed directly. |
| Credentials | `--gateway-upstream-credentials-file PATH` holds `USERNAME:PASSWORD` on one line (the username has no `:`; each 1 to 255 bytes, RFC 1929). Never in argv (credentials in the URL are refused), the profile, the trace or logs: they are read for each connection and never kept. |
| File permissions | Refused when other users can access it (any `o` bit); group read is allowed and the owner is not checked; symlinks are followed. A Kubernetes Secret's files are root-owned symlinks into `..data/`, readable by a non-root container through `fsGroup`; the default `defaultMode: 0644` is refused, so the Pod sets `0440`. This is looser than the CLI's own credential files (`0600`, owner, no symlink) on purpose: it is the one way a Secret reaches a non-root Pod. |
| Rotation | The file is read again for each connection, so the kubelet's swap of a rotated Secret applies from the next one. |
| Start-up check | Both commands parse the URL and read the credentials file once at start, failing with a diagnostic; the proxy's reachability is not checked (each connection finds out). |
| Precedence | The upstream belongs to whoever runs the gateway: the runner's for a gateway the runner runs, the daemon's for one the daemon runs. A daemon's upstream is not sent to runners (its credentials file is on the daemon's host, and the per-actor proxy is the runner Pod's to name). A runner with an upstream whose control plane keeps the gateway (an older daemon) refuses to start the actor rather than let its traffic leave directly from the daemon. |
| Trace | `net.request`, `net.connect` and `net.error` carry `via`, the upstream's URL (`socks5h://HOST:PORT`, no credentials), when the connection went through one; it is absent when direct. Trace schema version 16; versions 10 to 15 still read. A schema-16 runner's events are refused by a schema-15 daemon, so S1 and this slice must ship in the same release. |

**Trust shift with `socks5h`.** The proxy resolves the name, so the
gateway cannot check the address it reaches: `EgressPolicy` (loopback,
link-local, ULA, `--gateway-block-cidr`) does not apply to a name sent
through `socks5h`. An IP literal is still checked (it needs no
resolution), and `socks5` checks everything as `Direct` does. With
`socks5h`, blocking private, cluster and metadata ranges is the proxy's job
and the runner Pod's NetworkPolicy's (the Pod may only reach its proxy, the
daemon and the named services; see the rc integration guide, section 8).
The proxy's own address is the operator's choice and is not checked either.

Not done (markers in code): an HTTP `CONNECT` upstream; a handshake or
connect timeout (a direct connection has none either); a
`daemon.gateway_upstream` key in the configuration file (flags only).

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
- Per-actor upstream proxy: implemented, see "Upstream proxy" above.

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

## Evidence (probe, 2026-11-26)

A live probe of `feat/gateway-upstream` (static musl `needy` 0.0.18 dev
build, base VM image from the v0.0.18 release) on a Kubernetes cluster with
Flannel and Cilium chained (kube-proxy replacement, NetworkPolicy enforced),
an ephemeral `linux-firecracker` actor runner Pod on a KVM node, and a
control daemon Pod; every case ran at least three rounds. Probe evidence,
not a support claim:

- An actor downloading 50 MB over HTTPS through the gateway: with v0.0.18 on
  both sides the daemon Pod carried the 50 MB both ways; with S1 on both
  sides it carried about 15-18 KB (control traffic), and the destination saw
  the actor Pod's address. Each `net.*` event reached the daemon's
  `trace.db` once; the daemon's `--gateway-block-cidr` was enforced by the
  runner.
- Mixed versions (new daemon with a v0.0.18 runner, v0.0.18 daemon with a
  new runner): the daemon's gateway enforced, as designed. A runner with
  `--gateway-upstream` and a v0.0.18 daemon refused the actor.
- Two actors at once, each with its own `socks5h` upstream: each actor's
  requests arrived from its own proxy, `via` was on each proxied event, a
  name only one proxy could resolve worked through that proxy only, and the
  actor Pods made no DNS queries (DNS egress denied, requests still worked).
  A wrong password and a proxy scaled to zero were 502s with `net.error`
  and no direct connection; a rotated Secret applied once the kubelet
  updated the mounted file. No credential appeared in any trace or log.
- Not covered: a long CONNECT tunnel does not advance `last_activity_ms`
  after it opens (also on v0.0.18), and the gateway has no connect timeout,
  so a SYN dropped by a NetworkPolicy hangs until the client gives up.

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

Upstream proxy, through public behaviour with a test SOCKS5 proxy
(`gateway::testing::Socks5Server`, behind core-actor's `test-support`
feature; it connects every CONNECT to one test server, so `*.invalid` names
prove no local resolution):

- `crates/core-actor/src/sidecar/gateway/mod_test.rs`: names go unresolved
  through `socks5h` for requests and tunnels, with `via` in the trace;
  `socks5` resolves, checks and sends the address; IP literals are still
  checked through `socks5h`; services are dialed directly; an unreachable
  proxy answers 502 and nothing is dialed directly; a refusing proxy answers
  502 with its reason; the credentials file is sent and read again after
  rotation, and no password reaches an answer, the trace or `Debug`; a file
  others can read fails the connection before the proxy is asked.
- `crates/core-actor/src/sidecar/gateway/upstream_test.rs`: URL forms (no
  credentials echoed), the credentials format, file modes (`0600` and `0440`
  pass, `0644` fails) and a swapped `..data` symlink.
- `crates/daemon/src/actor_runner_test.rs`:
  `the_runner_s_gateway_upstream_carries_the_actor_s_traffic_but_not_its_services`
  (the runner's `ActorConfig.gateway_upstream` reaches its gateway; `via`
  reaches the daemon's trace).
- `crates/daemon/src/bridge_test.rs`:
  `the_daemon_s_gateway_upstream_carries_the_traffic_of_the_gateway_it_runs`.
- `crates/core/src/event_test.rs`: `via` is optional and version 14 reads.

Not covered by a test: the CLI flags themselves (they build the same
`Upstream` the tests use), and an upstream runner against an older daemon
(the refusal is one condition before the VM starts).

Not covered by a test: an S1 runner against an older daemon (the proxy
path is the unchanged pre-S1 code, selected when `local_gateway` is unset).
