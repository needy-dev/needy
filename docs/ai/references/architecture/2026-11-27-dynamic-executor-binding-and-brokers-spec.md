# Dynamic executor binding and runner brokers (spec)

Status: **proposed spec for owner review**, 2026-11-27. Nothing here is
approved or implemented. Names marked *(provisional)* are not stable. It
revises the data-plane recommendation of
[An agent platform's execution mesh over needy](2026-11-27-agent-platform-execution-mesh-design.md)
(§4, D1) and builds on the
[External Client Penetration spec (Phase 3)](2026-11-23-external-client-penetration-and-oci-layers-spec.md),
whose control-plane brokering it keeps and whose cluster ingress it replaces.

The goal is a design that holds for years, not the shortest path to a demo.

## 1. Problem

A run binds each of its executors to a runner once, when the run is
created (`crates/daemon/src/schedule.rs`: "a profile never moves silently").
Runners reach the daemon in one way only: they dial it. Three needs do not
fit:

1. **The executor is chosen after the actor starts.** A long-lived actor
   (an agent platform's device agent running in a VM) serves many agent runs, and
   each run may pick a different machine to work on.
2. **The executor goes away and comes back.** A laptop sleeps, a remote box
   reboots, the user switches machines. Today a run either keeps failing
   routed commands or has to be recreated.
3. **Runners cannot reach the daemon.** A user's machine has no route into
   the cluster that hosts the daemon, holds no cluster credentials, and
   sits behind NAT. Something both sides can reach (a platform's device
   gateway) already knows those machines and who may use them.

## 2. Decisions this spec proposes

1. **Routing stays local to the daemon.** Every captured exec is decided by
   the daemon's `exec.decide` chain, as today. No exec waits on a call to an
   outside service; outside systems change *bindings*, not decisions.
2. **Bindings become run state that can change.** An executor in a profile
   is a logical name with requirements; which runner serves it is a
   *binding* in the run, recorded in the trace, and changeable during the
   run by authorized writers.
3. **The daemon dials out to brokers.** A *broker* *(provisional)* is an
   outside service the daemon connects to. It offers runners it can reach
   and opens byte streams to them. Each stream carries the
   [sandbox protocol](2026-11-27-sandbox-protocol-spec.md), so a runner can be
   any implementation of it, not only needy's. Neither the cluster nor the
   runner needs an inbound route.
4. **End to end, the daemon and the runner authenticate each other.**
   Brokered streams carry TLS between the daemon and the runner, and the
   daemon checks the runner's credential inside it, so an honest broker
   relays only ciphertext. How the runner learns which daemon certificate
   to trust without taking the broker's word for it is open (§6.4).
5. **Static profiles keep working unchanged.** A profile that does not opt
   in binds at creation exactly as now; runners that dial the daemon are one
   more source of runners, not a legacy path.

## 3. Terms (to add to `docs/TERMS_AND_CONCEPTS.md` when approved)

- **Binding**: which runner serves one executor of one run, with a
  generation number. Static (fixed at creation) or dynamic (changeable).
- **Binding writer**: who may change a dynamic binding: the daemon's
  scheduler, a broker, or an operator through the client API.
- **Broker** *(provisional)*: a service the daemon dials that offers runners
  and opens streams to them. Example: an agent platform's device gateway.
- **Brokered runner**: a runner the daemon reaches through a broker stream.
  To the daemon it is a runner like any other once connected.

*Avoid*: provider (the pre-2026-11-12 name of executor driver), target
(already avoided for executor).

## 4. Model

### 4.1 Executors, bindings, instances

```
profile    executors.NAME          requirements: driver, os, labels, workspace, key, max_procs …
run        binding(NAME)           runner, generation, writer, lease
pool       instance(key, runner)   created on the bound runner on first acquire (as today)
```

- A static executor gets one binding at creation from the scheduler, as
  today, and it never changes.
- A dynamic executor (*provisional*: `executors.NAME.binding: dynamic`) may
  start **unbound**. Its binding is set and changed by writers allowed by
  `executors.NAME.bind_by` *(provisional; default `[scheduler]`)*.
- A binding change bumps the generation. The pool keys instances by
  `(key, runner)`, so a new binding creates a new instance on first use and
  the old instance drains: commands already dispatched to it run to the end
  (dispatched work is never retried, as `unavailable_timeout` already
  states), and it is destroyed when its last holder releases it.
- A routed command to an executor that is unbound or whose runner is
  offline waits up to the executor's existing `unavailable_timeout`, then
  fails before dispatch with a reason that names the executor and its
  binding state. The wait ends early when a binding arrives.

### 4.2 Routing

Unchanged: rules decide `actor` or an executor name. Two additions,
both optional:

- **Context-derived executor** *(provisional)*: a rule's `to:` may name an
  environment variable of the command, `to: { env: NEEDY_EXECUTOR }`. The
  value must name an executor of the profile; anything else is denied, so a
  command cannot pick a runner the profile does not declare. This lets one
  long-lived actor serve several agent runs at once, each with its own
  executor, by setting the variable in the run's environment.
- **Binding-aware trace**: `exec.decide` records the binding generation the
  command was dispatched under.

### 4.3 Workspace mounts

`actor.workspace.from: executor:NAME:PATH` mounts an executor's directory
into the actor at start. A mount cannot follow a binding silently: files the
agent already read would change under it.

- Static executors: unchanged.
- Dynamic executors: each binding gets its own mount at a path derived from
  the executor and the binding *(provisional:
  `~/executors/NAME` remounted on rebind, with `files.remount` in the trace,
  or one path per generation)*. Open question 2.

## 5. Writers and authority

| Writer | How | May bind |
| --- | --- | --- |
| Scheduler | Inside the daemon, from labels and connected runners | Executors whose `bind_by` lists `scheduler` |
| Broker | Over its broker connection | Executors whose `bind_by` lists `broker`, in runs whose profile names that broker; only to runners that broker offered |
| Operator | `needy run bind RUN EXECUTOR RUNNER` *(provisional)*, client API | Any dynamic executor of a run the caller may manage |

- Each binding records its writer. A writer may replace only bindings that
  its own class may write. Precedence when two classes are allowed is open
  question 3.
- Bindings are leases: a broker's binding expires unless renewed (default
  of the order of the broker heartbeat × 3), so a broker that vanishes
  cannot hold an executor forever.

## 6. Brokers

### 6.1 Connection

- The daemon dials each configured broker
  (*provisional*: `needy daemon serve --broker NAME=wss://…
  --broker-credential-file NAME=PATH`). One connection per broker, with
  reconnect, backoff and jitter.
- **Transport: WebSocket, binary frames, protobuf messages** from a new
  `needy.v1.broker` schema. WebSocket because a broker is likely to run
  where HTTP/2 trailers and bidirectional gRPC are not available
  (serverless runtimes such as Cloudflare Workers and Durable Objects), and
  because it passes the proxies that enterprise networks keep. The schema
  is protobuf, so a gRPC transport can be added later without a second
  message set.
- Stream data is chunked at 64 KiB with a per-stream credit window (512 KiB
  by default), acknowledged by the receiver. Credits are kept end to end
  between the daemon and the runner, never in the broker, so one slow stream
  cannot stall the others on the same connection (see §12, Konnectivity).
  Durable Objects accept received WebSocket messages up to 32 MiB
  ([limits](https://developers.cloudflare.com/durable-objects/platform/limits/),
  read 2026-11-27); 64 KiB chunks stay far below that and below the device
  gateway's own 1 MiB tunnel cap.

### 6.2 Messages *(provisional)*

| Direction | Message | Meaning |
| --- | --- | --- |
| daemon → broker | `Hello` | protocol version, features, daemon identity, the TLS certificate fingerprint runners must pin |
| broker → daemon | `Welcome` | accepted version and features, broker identity, heartbeat interval |
| broker → daemon | `Offer` / `Withdraw` | a runner the broker can reach: broker-scoped runner name, os/arch, capabilities, labels |
| daemon → broker | `Want` | open a stream to an offered runner (on a binding, or for a scheduler that needs it) |
| broker → daemon | `Bind` / `Unbind` | set or clear a binding (run, executor, runner, lease, request id); idempotent by request id |
| daemon → broker | `BindResult` | applied, or why not (not dynamic, not allowed, unknown runner) |
| either | `StreamOpen`, `StreamData`, `StreamAck`, `StreamClose` | a byte stream to one runner, flow-controlled |
| either | `Ping` / `Pong` | liveness; three missed beats close the connection |
| broker → daemon | `MintRequest` | ask for a TTL-bound executor-role credential for an offered runner name |

### 6.3 What a brokered stream carries

A broker stream is an opaque byte stream between the daemon and a runner.
Inside it, in order:

1. TLS (§6.4).
2. The sandbox protocol's length-prefixed binding
   ([sandbox protocol §3](2026-11-27-sandbox-protocol-spec.md)): one session,
   with exec and file streams multiplexed and flow-controlled end to end.

The daemon is the session's client. It maps sandbox exec and file streams
onto the same relay, routing and file-domain interception it uses for
`RunnerService.Task` and `FsTask`, so a brokered runner looks like any other
runner to the rest of the daemon. A runner needs protobuf, TLS and a byte
stream, not gRPC: a device agent in TypeScript can be one. needy's own
`needy runner serve` speaks the same session through its `exec:` stdio
endpoint (`crates/api-client/src/command.rs`).

### 6.4 End-to-end security

- **TLS inside every brokered stream.** The daemon presents a certificate
  whose fingerprint it sent in `Hello`. The broker delivers that fingerprint
  to the runner with its credential, and the runner pins it
  (*provisional*: `needy runner serve --daemon-cert-sha256 HEX`). An honest
  broker then sees only ciphertext.
- **The pin is only as trustworthy as its delivery.** A broker that
  delivers the fingerprint could deliver its own and sit in the middle.
  chisel avoids this because its client gets the fingerprint out of band.
  Teleport documents the same split: proxy-recording mode can read
  sessions, node-recording mode cannot. Options, open question 6: the
  daemon's certificate is signed by a tenant CA the runner learns
  independently of the broker (for example at device enrolment), the runner
  keeps the first pin it saw for that daemon and refuses a change, or the
  broker is trusted and this is stated as a limit. This needs the
  TLS work tracked as gap G4 in the `rc` integration guide, applied to the
  daemon's end of a broker stream and to the runner's stdio endpoint.
- **The runner credential stays the daemon's to check.** A broker may ask
  for credentials (`MintRequest`) only for names under its own prefix and
  only with the executor role. The daemon refuses an actor-role mint over a
  broker connection.
- **A broker's authority is per run.** A broker may bind executors only in
  runs whose profile names it (*provisional*: `executors.NAME.broker:
  NAME`), and only to runners it offered.
- **Who may use which machine is the broker's decision.** The daemon does
  not know the broker's users; it trusts the broker connection for which
  runners exist and which run may use them, and records every binding
  with its writer in the trace.

## 7. Failure modes

| Event | Behaviour |
| --- | --- |
| Broker connection drops | Streams to brokered runners close; their runners enter `DISCONNECT_GRACE` on both ends. The daemon reconnects with backoff; the broker re-sends `Offer`s; runners that come back within the grace keep their processes. Broker-written bindings survive until their lease ends. |
| Runner machine sleeps | Its stream closes; in-flight execs end with `runner X disconnected`. New commands wait up to `unavailable_timeout` for it or for a new binding. |
| Rebind during a command | The command finishes on the old runner; later commands go to the new one. The trace shows both generations. |
| Stale runner reconnects after a rebind | It is a connected runner with no binding; it receives no new commands, and its old instance is destroyed when idle. |
| Broker sends `Bind` for a static executor, an unknown run, or a runner it did not offer | Refused with a reason in `BindResult`; nothing changes. |
| Daemon restarts | Bindings and leases are in `state.db`; brokers reconnect and re-offer. Runs whose actors survived (holder) keep their bindings. |
| Two writers race | Each `Bind` carries the generation it expects; a stale one is refused (compare-and-set). |

## 8. Trace and replay

New events *(provisional names)*: `executor.bind`, `executor.unbind`,
`broker.connect`, `broker.disconnect`, each with run, executor, runner,
generation and writer. `exec.decide` gains the binding generation. A replay
uses the recorded bindings, so placement is reproducible even though it was
decided at run time.

## 9. Compatibility and migration

- Profiles without `binding: dynamic` behave exactly as today.
- Runners that dial the daemon are unchanged and remain the right choice
  inside a cluster (`rc` Workspaces) and on reachable hosts.
- A runner build that cannot pin the daemon certificate cannot join through
  a broker: the daemon refuses brokered streams that do not start TLS.
- The broker schema is versioned in `Hello`; unknown features are ignored,
  and an unsupported major version closes the connection with a reason.

## 10. Phases

Each phase ends with an e2e harness cell (`cargo xtask e2e`), not a manual
script.

1. **Bindings in the daemon.** Binding table in `state.db`, generations,
   `binding: dynamic`, `bind_by`, `unavailable_timeout` waits that end on a
   new binding, operator `needy run bind/unbind`, trace events. Cell: one run,
   two native runners, rebind mid-run, check the trace and the drained
   instance.
2. **Sandbox protocol v1 core.** `proto/sandbox/v1`, the frame and
   session layer, the daemon's client side, `needy runner`'s server side, and
   the conformance suite
   ([sandbox protocol](2026-11-27-sandbox-protocol-spec.md)). TLS over a
   stream source with a pinned fingerprint. Cell: a runner joining through a
   local stdio relay and passing the suite.
3. **Broker protocol.** `needy.v1.broker` schema, the daemon's broker client,
   leases, `MintRequest`, and a reference broker in `xtask` for tests. Cell:
   daemon dials the reference broker, which offers a runner and binds it.
4. **A platform broker** (outside needy). A device gateway endpoint for
   daemons, per-stream relay, device stream frames, and the device agent's
   stdio channel to `needy runner serve`. Its own repository's tests, plus a
   needy cell that runs the reference broker's conformance checks against it.
5. **Context-derived executors and per-binding mounts**, if open questions 1
   and 2 are decided for them.

## 11. Out of scope

- Per-exec calls to an outside decider. If one is ever needed it belongs in
  the `exec.decide` chain as an interceptor with a strict deadline and a
  local fallback, in its own spec.
- Moving a running process between runners.
- Brokers that carry actor runners. Actor runners stay where KVM is; this
  spec is about executors.

## 12. Prior art and lessons

Verified 2026-11-27 against primary sources; no single project combines
all of this spec, but each part has a precedent.

- **Kubernetes apiserver-network-proxy (Konnectivity)**: agents dial out to
  the proxy server, which picks an agent per dial and prefers agents that are
  not draining
  ([`DRAIN`, client.proto](https://github.com/kubernetes-sigs/apiserver-network-proxy/blob/811699a170543045e1b7030dfd8c2a156d37e9c9/konnectivity-client/proto/client/client.proto#L92)).
  One slow connection stalled every connection on an agent's stream
  ([#881](https://github.com/kubernetes-sigs/apiserver-network-proxy/issues/881));
  bounded queues only delay it. Hence end-to-end per-stream credits here.
- **BuildBuddy and Buildbarn**: executors and workers dial the scheduler and hold work on
  leases that are renewed before expiry, with stale lease IDs ignored and a
  reconnect token
  ([BuildBuddy scheduler.proto](https://github.com/buildbuddy-io/buildbuddy/blob/1aa6b561ac0617bc0eb43258e6a25fcd77c9b4f3/proto/scheduler.proto#L552-L555),
  [Buildbarn remoteworker.proto](https://github.com/buildbarn/bb-remote-execution/blob/65cbecb86a0fa6ba39a99e457d7ced267d529c69/pkg/proto/remoteworker/remoteworker.proto#L126-L131)).
  The model for §5 leases and §7 compare-and-set.
- **Coder**: the workspace agent's control channel is a binary WebSocket
  carrying yamux streams (per-stream windows) and dRPC
  ([agentsdk.go](https://github.com/coder/coder/blob/cf7267e9701ee382a5ff59b8fd89d981bbefd45c/codersdk/agentsdk/agentsdk.go#L481-L502)),
  and `coder ssh --stdio` serves as an SSH `ProxyCommand`
  ([configssh.go](https://github.com/coder/coder/blob/cf7267e9701ee382a5ff59b8fd89d981bbefd45c/cli/configssh.go#L180)).
  Coder later forced WebSockets for its relay because load balancers block
  custom upgrades, which supports §6.1's choice.
- **Tailscale DERP, Microsoft dev tunnels, chisel**: relays that forward
  traffic already encrypted end to end
  ([DERP](https://tailscale.com/kb/1232/derp-servers)); SSH over a WebSocket
  relay ([VS Code tunnels](https://code.visualstudio.com/docs/remote/tunnels));
  a client that pins the server key ([chisel](https://github.com/jpillora/chisel)).
- **Bazel remote execution**: the client decides local or remote per
  action, and its `dynamic` strategy races both
  ([docs](https://bazel.build/remote/dynamic)), which supports §2.1. icecream is the
  counterexample: a central scheduler places every compile job.
- **Durable Objects as a broker**
  ([WebSockets](https://developers.cloudflare.com/durable-objects/best-practices/websockets/),
  [limits](https://developers.cloudflare.com/durable-objects/platform/limits/)):
  - Only WebSockets that other parties open *to* an object hibernate.
  - In-memory state is lost on hibernation and on deploys.
  - An object takes about 1,000 requests per second.

  So both the daemon and the runner dial in, flow state stays end to end,
  relays are split per run or per runner, and reconnects are routine.
- **Against WebSocket**: Cloudflare Tunnel prefers QUIC; e2b splits its
  process API into server streams plus unary calls that need no duplex
  stream; Workers are gaining inbound TCP and server-streaming gRPC
  ([blog](https://blog.cloudflare.com/grpc-workers/)). The schema in §6.1 is
  protobuf so the transport can change when these mature.

## 13. Open questions

1. Context-derived executors (§4.2): adopt, or keep one executor per actor
   and run one actor per session?
2. Workspace mounts for dynamic executors (§4.3): one remounted path with a
   trace event, or one path per binding?
3. Writer precedence when an executor allows several classes (§5).
4. Default lease length and heartbeat for broker bindings.
5. Whether `rc` configures broker connections for its daemons (flags and a
   credential Secret), which would replace `rc`'s per-device runner
   credentials for external executors.
6. How a runner learns the daemon's certificate pin without trusting the
   broker for it (§6.4).
