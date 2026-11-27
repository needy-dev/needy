# An agent platform's execution mesh over needy (design)

> **Revision notice (2026-11-27):** the data plane in §4 (D1, a cluster
> byte-stream ingress) and the static executor binding assumed in §3 A are
> superseded by the proposed
> [Dynamic executor binding and runner brokers](2026-11-27-dynamic-executor-binding-and-brokers-spec.md):
> bindings change during a run, and the daemon dials out to a broker such
> as the platform's device gateway, so no cluster ingress is needed. End-to-end
> TLS through the gateway (§4, D2) is not planned either: authentication stays
> outside the session (broker spec §6.4). The text below is kept as written.

Status: **proposed design for owner review**, 2026-11-27. Nothing here is
approved; names marked *(provisional)* are not stable. It extends the
proposed
[External Client Penetration spec (Phase 3)](2026-11-23-external-client-penetration-and-oci-layers-spec.md)
and does not replace it. Where the two disagree, the Phase 3 spec stands
until the owner decides.

Related:
[`rc` integration guide](2026-11-26-rc-integration-guide.md),
[Kubernetes and `rc` design](2026-11-23-kubernetes-rc-integration-design.md),
[Remote actor host spec (Phase 1)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md),
[Linux actor and Mac executor walkthrough](../../explanations/linux-actor-mac-executor.md).

## Terms

These are this note's words, not product names:

- **The platform**: an agent platform that runs external agent CLIs for its
  users, on machines they connect to it.
- **The device agent**: the platform's CLI that connects a machine as a
  *device* and runs agent CLIs there when the platform dispatches a run.
- **The device gateway**: the platform's relay that carries dispatches, tool
  calls and RPCs to connected devices.

## 1. Question

An agent platform runs external agent CLIs (Codex, OpenCode, Claude Code, …)
on a *device*: a machine running the platform's device agent, reached through
the platform's device gateway. Today the device that hosts the agent is also
the device where every command runs. needy separates the two: an **actor** hosts the driving program,
and **executors** run the commands it starts, chosen per command by the
profile's routing rules.

The owner asked whether the device agent, the device gateway and `rc` could
themselves provide a needy-like mesh: several devices, a choice of execution target, and
routing between them, with needy's own daemon/actor/executor arrangement
left as a needy-only mode.

## 2. What exists today

### 2.1 Evidence from 2026-11-27 (probe, not a support claim)

On an EKS development cluster with a nested-virtualization KVM node pool,
running `rc` 0.26.1 (plus the `spec.env` and `spec.pod` change on branch
`feat/actor-env`, not merged) and needy v0.0.22:

- An `rc` Actor started a `linux-firecracker` actor whose driving program was
  the device agent. The actor registered as an ordinary platform device.
- The platform dispatched a hetero agent run to it. The device agent's run
  command started Codex (ChatGPT sign-in synced through the `codex` feature's file store) and
  OpenCode. Their shell commands were captured in the VM and routed by needy
  to an `rc` Workspace executor; the output named the executor's host.
- Required beyond released code: the platform API key reached the VM through
  `Actor.spec.env` from a Secret; `NODE_USE_ENV_PROXY=1` so Node uses the
  actor gateway's `HTTPS_PROXY`; a patched device agent whose gateway
  WebSocket uses a `proxyEnv` agent; and a workspace-scoped device token,
  because the production gateway refuses API-key device connections by design (`API_KEY_SERVER_ORIGINS` unset).

So *actor device + in-cluster executor* works with no gateway change. What
is missing is an executor **outside** the cluster, such as the user's Mac,
and a platform-level model that knows about both.

### 2.2 Two meshes already exist, at different granularities

| | Platform tool-call mesh | needy exec mesh |
| --- | --- | --- |
| Unit routed | A tool call (`runCommand`, file ops, MCP) | A process exec, captured transparently |
| Who decides the target | The platform server: the agent's bound device | The daemon's `exec.decide` chain (rules, chaos) |
| Works for the platform's own agent runtime | Yes: the server is the brain, devices are hands | Not needed |
| Works for external CLIs (Codex, OpenCode) | No: the CLI spawns its tools locally, the platform never sees them | Yes: `binfmt_needy` in a VM, shims in a container |
| Process semantics (PTY, stdin, signals, exit status) | Per-RPC, partial (`runCommand`, PTY RPCs) | Full, streamed (`ProcessService`) |
| Shared files between brain and hands | None: each tool call names paths on the device | `actor.workspace` mounts an executor's directory over SFTP |
| Transport | Device WebSocket through a Durable Object per principal | gRPC from runners to the daemon, outbound only |

The gap is external CLIs. They do not ask anyone where to run a command;
they `exec` it. Routing them needs capture below the CLI, plus process and
file semantics that hold up for real tools (`git`, `npm`, `cargo`, test
runners, editors through PTYs).

## 3. Options

### A. The platform as control plane, needy as data plane (recommended)

The platform (server, device gateway, device agent, and `rc` with whatever
provisions managed devices) owns *who*: which devices take part, which is the actor host, which are executors, the
session's lifetime, credentials and the routing choices the user makes.
needy owns *how*: capture, routing decisions, process relay, workspace
mounts, the actor's network policy and traces.

This is the Phase 3 spec's split, made concrete for the platform:

```
platform server ──(1) create session──► provisioner / rc: Actor + executors
     │                                   (rc mints runner credentials)
     │ (2) needy.attachExecutor (structured fields only)
     ▼
device gateway ──RPC──► device agent on the user's Mac
                           │ (3) spawns `needy runner serve --roles executor`
                           │     with a fixed connector (no server-supplied argv)
                           ▼
            cluster byte-stream ingress (routeToken) ──► tenant needy-daemon :7400
                                                             ▲
            actor VM (device agent, Codex) ─captured exec──────┘
```

- **The device agent embeds needy as a supervised child, not as a library.** The
  device agent ships or downloads a pinned needy release (*provisional*: it
  downloads it on first use and checks the release attestation). It runs
  `needy runner serve --roles executor --name … --credential-env …` with
  `--address` built locally from structured fields. needy needs no platform
  concepts, which AGENTS.md requires.
- **What the device agent would write instead of supervising needy.** An
  executor device must speak needy's runner protocol: the `RunnerService.Control` stream and
  one `Task` stream per routed exec, heartbeats and credentials; serve
  `ProcessService.Exec` (PTY or pipes, stdin, resize, signals, exit status,
  detach, replay, expiry of finished processes); move tar archives for
  copy-in and mirror-out; and serve SFTP for `actor.workspace` mounts. In
  needy that is about 5,300 lines of Rust in `core-executor` and the runner,
  and the protocol has no compatibility promise before 1.0, so a TypeScript
  copy would chase every release. The device agent's existing terminal RPCs
  (`createTerminalSession`, `readTerminal`, …) are request/response calls
  through the gateway, not a streamed process relay.
- **Terminals.** Two different terminals are involved. The actor's own
  terminal (`needy run attach`) is not needed in the platform's flow: the driving
  program is the device agent, and the device agent's run command runs the CLI headless with
  JSON events that the platform renders. A browser terminal into the actor already
  works through the device agent's terminal RPCs, because the device agent runs inside the VM and its
  shells are captured like any other exec. The terminals of *routed*
  commands (an interactive tool the CLI starts that is routed to the Mac) are
  relayed by needy between the actor and the executor runner. That is the
  part the device agent gets for free by supervising needy and would have to rebuild
  otherwise.
- **The device gateway stays a broker.** It delivers `needy.attachExecutor`
  and `needy.detachExecutor` over its existing RPC path, with the hardening
  that Phase 3 lists: stale-heartbeat refusal, a returned `connectionId`, and
  detach pinned to that connection.
- **`rc` binds external executors.** `Actor.spec.executors[]` gains a
  reference to an external runner by name (*provisional*: `runnerRef`) next
  to `workspaceRef`. `rc` mints that runner's TTL-bound credential with
  `MintRunnerCredential` and revokes it when the Actor ends.
- **The daemon stays where it is:** one per tenant namespace, created by
  `rc`. It is the routing brain and the only place credentials are checked.

### B. The platform as the daemon (not recommended)

The device agent embeds needy's actor-side capture and executor-side process
host, and the platform server plus the device gateway replace the needy daemon: routing
decisions, process relay, credentials, traces.

- It duplicates needy's control plane in TypeScript and keeps two
  implementations of the same semantics in step.
- Every exec, PTY byte and file transfer would go through the per-principal
  Durable Object. One object serializes a whole workspace's traffic: in a
  workspace with many members, one person's `npm install` output shares a
  single-threaded object with everyone's tool calls. Workers also bill per
  message.
- needy's guarantees would no longer hold: path maps, chaos, trace records
  and the report.

### C. Tool-level mesh without needy (complementary)

Give external CLIs a remote shell as an MCP tool provided by the device
agent, which calls another device's `runCommand` through the gateway, and turn off the
CLI's built-in shell.

- No VM and no needy install on any device.
- Not transparent: the agent sees a different tool, file edits stay on the
  agent's machine, and CLIs that cannot disable their own shell leak
  commands locally.
- It fits the platform's own agent runtime and simple remote commands. It is not
  a replacement for A.

## 4. Data plane: which path carries needy's gRPC

The Phase 3 spec rejects data-plane relaying through the serverless gateway
and proposes a cluster-side byte-stream router. The owner prefers that
the platform's own pieces carry it. Two candidates:

| | D1. Cluster byte-stream ingress (Phase 3) | D2. Device gateway stream relay |
| --- | --- | --- |
| Where bytes go | Mac → public L4/HTTP-CONNECT router in the cluster (`routeToken`) → tenant daemon | Mac → gateway Worker → principal's Durable Object → a bridge device's WebSocket → daemon |
| New component | A router Deployment and a public endpoint (NLB or Cloudflare Tunnel) | `stream_*` frames in the gateway, an `--expose` option and a stdio `stream` command in the device agent |
| Cluster side | Router reaches every tenant's daemon Service | A bridge device agent per tenant, as a sidecar in the daemon Pod |
| Throughput and PTY latency | Direct TCP | Base64 or binary frames through one object per principal |
| Cost | Fixed router | Per message and per duration on Workers |
| Who sees plaintext h2c | The router, inside the operator's cluster | Cloudflare and the gateway, unless the device agent adds TLS end to end |
| Reuse elsewhere | needy only | Any device-to-device stream, such as ssh `ProxyCommand` |

Recommendation: **D1 for the data plane** and the gateway for control only,
as in Phase 3. The per-principal Durable Object is the reason: it is the
right unit for routing tool calls and the wrong unit for bulk streams. D2
remains an option for low volume, or for reaching a device with no cluster
ingress. If it is chosen, the device agent should wrap the stream in TLS between the two
ends, so the gateway carries ciphertext.

## 5. Platform-side model (all provisional)

- **Device capabilities.** The device agent reports what it can be in `system_info`,
  which the gateway already relays opaquely:
  `needy: { actor: true }` when running inside a needy actor (the `--role
  actor` idea from the owner's question becomes a reported fact, not a
  flag), and `needy: { executor: { os, arch, version } }` when it can
  supervise an executor runner. The gateway needs no role model.
- **Agent execution config.** An agent bound to an actor device gains
  `executionTargets`: zero or more devices, each with a logical executor name
  and the commands routed to it. The server compiles these into the profile
  that the provisioner hands to `rc` (`executors.NAME`, `commands`), so routing stays
  needy's.
- **Session lifecycle.** For each run: ensure the Actor (managed: the provisioner
  creates an `rc` Actor instead of a Workspace and passes the bootstrap
  secret through `Actor.spec.env`), attach every external executor before
  the run, detach and revoke when it ends. An offline target fails the run
  clearly; nothing falls back to the actor.
- **Files.** A target that owns the project uses
  `actor.workspace.from: executor:NAME:PATH`, so edits the CLI makes itself
  (Codex's `apply_patch`) land on that device. Over a WAN, heavy commands
  should be routed to the executor rather than scanned through the mount
  (Phase 3, Further Notes).

## 6. Security

- **No remote command strings.** Attach payloads are structured fields that
  the device agent validates; the device agent builds needy's argv itself (Phase 3, story 5).
- **Credentials.** Runner credentials are TTL-bound, scoped to one tenant's
  daemon and revoked at session end. The platform credential that brings an
  actor online stays inside the actor; the runner credential never enters
  the VM.
- **Authorization.** The gateway authorizes per principal. In a workspace,
  every member's devices share a principal, so attach must also check that
  the session's owner may use the target device (its enroller, or a public
  device). *Open question 3.*
- **Exposure.** Executing on a personal machine gives a sandboxed agent the
  account of whoever runs the runner. needy's routing rules decide which
  commands go there; they do not change the CLI's own approval policy.

## 7. Phases (proposal)

1. **Control plane in the gateway and the device agent:** attach and detach RPCs with the
   Phase 3 hardening; the device agent supervises a pinned needy runner; capabilities in
   `system_info`. Tested with a stand-in executable, as Phase 3 describes.
2. **`rc` external executors:** `runnerRef`, credential minting and
   revocation; Actor waits for the runner. Builds on `spec.env`.
3. **Data plane:** D1 (or D2 if the owner chooses it), with an end-to-end
   run from a cloud actor to a Mac executor.
4. **Platform model and UI:** execution targets on agents, compilation into
   profiles, managed actors from the provisioner.

## 8. Open questions

1. D1 or D2 for the data plane, and who owns the router (the provisioner or `rc`).
2. How the device agent obtains needy: bundled per platform, or downloaded and verified on
   first use.
3. Device authorization inside a workspace principal: enroller only, public
   devices, or an explicit grant per agent.
4. Whether the daemon stays per tenant namespace or moves to one per run,
   next to the actor runner, so a session's routing state has the session's
   lifetime.
5. Whether needy should set `NODE_USE_ENV_PROXY=1` wherever it sets
   `HTTPS_PROXY` for an actor, so Node tools work without profile edits.
6. Where the daemon's file store lives in `rc`: today it is on an emptyDir,
   so synced sign-ins (Codex's `~/.codex`) are lost on a daemon restart.
