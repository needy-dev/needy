# Spec: Remote Actor Host Transport and K8s-Friendly Runner Ergonomics (Phase 1)

Status: **proposed spec**, 2026-11-23.
Parent design: [Needy on Kubernetes with `rc` and Managed IaaS](2026-11-23-kubernetes-rc-integration-design.md).

## Problem Statement

Today, Needy's control daemon and actor host are coupled to the same machine:
the scheduler refuses remote actor placement, actor channels only speak local
Unix sockets or local hybrid vsock, run completion and `needy run logs` rely on
local VM handles and local `console.log` files, and the HTTP gateway has no
authentication or resolve-then-validate SSRF protection. Meanwhile, runners
require interactive `invite` / `join` enrollment with a writable credential
file, runner credentials never expire, and neither runners nor runs expose
activity hooks or last-activity timestamps for external idle-timeout
supervisors.

Because of these constraints, an orchestrator such as Kubernetes / `rc` cannot
safely run a lightweight control daemon on a standard node while placing
Firecracker microVM actors on separate bare-metal KVM Pods, nor can it
declaratively manage in-cluster or short-lived external runners.

## Solution

Decouple the actor host from the control daemon over authenticated outbound TCP
channels while keeping Firecracker microVMs unchanged (vsock-only, zero NIC),
harden actor/gateway/file-mapping security for shared-daemon network operation,
and add orchestrator-friendly runner credentials (both hot-reloaded static
secrets and TTL-enforced ephemeral tokens), activity hooks, and per-run
last-activity observation. After this phase, a control daemon without
`/dev/kvm`, a remote Firecracker actor bridge with `/dev/kvm`, and a statically
or dynamically provisioned runner run end-to-end across separate processes or
hosts with zero Kubernetes dependency.

## User Stories

1. As a platform operator, I want the control daemon to run on a host without
   `/dev/kvm` while hosting a Firecracker actor on a separate KVM-capable host,
   so that control-plane workloads and bare-metal virtualization capacity can
   be scheduled independently.
2. As a platform operator, I want an actor host bridge to connect **outbound**
   over TCP to the control daemon using a per-run bearer token, so that the
   control daemon never needs inbound network reachability to the actor Pod IP.
3. As a security engineer, I want a per-run actor token to be restricted to an
   explicit method allowlist for its own run only, so that a compromised actor
   cannot enumerate, attach to, kill, or inspect other runs on the same control
   daemon.
4. As a security engineer, I want the actor gateway to authenticate per-run
   actor traffic over TCP and enforce resolve-then-validate IP blocking
   (loopback, link-local, ULA, IPv6 metadata, IPv4-mapped IPv6, and configured
   private CIDRs), so that DNS rebinding and IPv6 authority tricks cannot
   bypass egress policy or reach daemon loopback / sibling Pods.
5. As a security engineer, I want an operator flag to disable `host:` file
   mappings (or jail them under a canonical root) on `needy-daemon`, so that
   actor profiles in managed deployments cannot read daemon state or secrets
   via `copy-in` or overwrite daemon files via `mirror-out`.
6. As a platform operator, I want a lightweight, stateless actor host bridge
   command that boots a Firecracker microVM and bridges its local vsock
   channels to the remote control daemon, so that the actor Pod needs no
   `state.db`, routing engine, or local holder subprocess.
7. As a platform operator, I want the Firecracker guest kernel and guest init
   to remain unchanged (vsock-only with zero network interfaces), so that
   hard network isolation inside the microVM is preserved on remote actor
   hosts.
8. As a platform operator, I want the actor host bridge to maintain a
   liveness/lifecycle stream to the control daemon and stream Firecracker
   console output back to the daemon, so that `ActorVmExit`, executor
   `mirror_out`, pool slot release, `needy run logs`, and `needy run report`
   work identically to local runs—and an OOMKilled or `SIGKILL`ed actor Pod
   never leaves a run stuck in `Running`.
9. As a platform operator, I want the actor host bridge to fail fast and exit
   if the control daemon restarts or rejects its session token, and to
   prevent two bridges from concurrently attaching to the same run, so that
   orphaned VMs and split-brain hijackings cannot occur.
10. As a platform operator, I want `needy runner serve` to accept `--address`,
    `--name` (or derive name from the credential), and `--credential-env` /
    `--credential-file` without writing to `runner.json`, so that read-only
    Kubernetes Secrets and ephemeral external runners work without interactive
    `invite` / `join`.
11. As a platform operator, I want `needy-daemon` to hot-reload pre-provisioned
    static runner credentials from a mounted file without restarting, so that
    adding or removing an Executor in a namespace never interrupts active runs.
12. As a platform operator, I want a control-plane RPC to mint and revoke
    **short-lived, TTL-enforced runner credentials** (`expires_ms`), so that
    Phase 3 external clients can join temporarily and automatically lose access
    even if a detach message is lost.
13. As an executor host supervisor, I want `needy runner serve` to invoke a
    throttled `--activity-hook` command whenever routed exec or SFTP activity
    occurs, logging hook failures without aborting tasks, so that external
    idle-timeout controllers do not suspend busy executors.
14. As an external retention controller, I want to query a run's
    `last_activity_ms` timestamp (updated on guest RPC, gateway traffic, and
    terminal I/O) from `RunService`, so that I can enforce idle timeouts on
    actor Pods without polling inside the microVM.
15. As a developer using Needy locally, I want existing local `needy daemon`
    and `needy run create` workflows to remain completely unchanged.

## Implementation Decisions

- **Node Authorizer & Strict `Caller::Actor` Allowlist (No Parallel `actor_token`)**:
  - Keep a single node credential family (`needy-runner.<name>.<64hex>`),
    stored hashed and verified in constant time. Do not introduce a separate
    `needy actor serve` command or a separate `needy-actor.*` token type.
  - When an authenticated `Caller::Runner(runner_name)` supplies
    `x-needy-run-id: <run_id>` on actor-scoped gRPC methods or the TCP
    gateway, authorize it as `Caller::Actor { run: run_id }` **only** when:
    1. `run_id` exists and is active (non-terminal);
    2. `run.actor_runner == Some(runner_name)`;
    3. The runner's active control-stream generation matches the binding.
  - Enforce an explicit gRPC method allowlist for `Caller::Actor { run }`:
    `GuestService/*` (blocking `PushLayer` for non-edit runs),
    `RunService/Decide`, `ProcessService/Exec`, and `HealthService/Check`;
    reject `RunService` management methods, `ClientService`, `RunnerService`,
    and `ImageService`.
  - Close existing run-scoping gaps across `GetRun`, `ListRuns`,
    `ProcessService/Exec(Attach)` with empty `run_id`, and `KillProcess` so
    `Caller::Actor` can never observe, attach to, or signal another run's
    processes.
- **Unified `needy runner serve --roles actor --ephemeral` & Complete Lifecycle**:
  - Lift `actor_refusal()` in the daemon scheduler so profiles declaring
    `actor.runner` schedule onto connected runners advertising
    `actor(<driver>)`. Skip local KVM/image host checks on the control daemon
    when the actor is placed on a remote runner, bind `run.actor_runner` and
    its active link generation, and dispatch `StartActor { run_id, driver }`
    over `RunnerService.Control`.
  - Extend `needy runner serve` with `--ephemeral` (exit cleanly with status 0
    once its assigned run finishes), `--gateway`, `--actor-image`,
    `--guest-needy`, `--kvm`, and `--work-dir`.
  - When a runner receives `StartActor`, it boots Firecracker locally (never
    passing its `Runner Credential` into the guest VM) and opens outbound TCP
    streams to `needy-daemon` using its `Runner Credential` +
    `x-needy-run-id: <run_id>`:
    - **Lifecycle & console stream (`GuestService.Bridge`)**: enforces
      single-bridge exclusivity per run, emits `ActorStart`, streams
      Firecracker console bytes to the daemon's run log store (backing
      `needy run logs`), and reports VM exit (or stream drop) so the daemon
      triggers `ActorVmExit`, executor `mirror_out`, pool release, and
      terminal `RunState`.
    - **Guest API & Gateway channels**: forwards local vsock `7000` (Guest
      gRPC) and `7001` (Gateway) with the runner credential and `run_id`.
    - **Reverse Terminal tunnel (`GuestService.TerminalTask`)**: forwards
      local vsock `7002` (`ProcessService`) over an outbound stream to the
      daemon so the daemon never dials the actor host's IP.
  - On control-plane disconnect or auth rejection, the runner stops its active
    microVM cleanly and exits non-zero. On `SIGTERM`, it runs graceful stop
    before exiting.
  - Support `wait_for_runner_ms` on `CreateRun` so concurrent Pod startup in
    Kubernetes waits briefly for both actor and executor runners to connect.
- **Gateway Authentication and Resolve-Then-Validate SSRF Defense**:
  - Add per-run actor token authentication to the TCP-exposed actor gateway
    (independent of tonic's gRPC middleware).
  - Fix IPv6 authority parsing so bracketed IPv6 hosts are never truncated at
    the first colon.
  - Implement resolve-then-validate in `needy-core-actor`'s gateway: resolve
    DNS once, reject loopback (`127.0.0.0/8`, `::1`), link-local
    (`169.254.0.0/16`, `fe80::/10`), ULA (`fc00::/7`), cloud IPv6 metadata
    (`fd00:ec2::254`), IPv4-mapped IPv6 addresses targeting blocked IPv4 ranges,
    and operator-configured private CIDRs, then connect directly to the
    validated `SocketAddr` (eliminating DNS rebinding TOCTOU).
- **Host File Mapping Lockdown**:
  - Add `--disable-host-files` and `--host-files-root <dir>` (with canonical
    path enforcement against symlink escapes) to `needy-daemon`. Reject
    profiles using `host:` mappings when disabled.
- **Runner Ergonomics, Hot-Reload, Ephemeral Tokens, and Activity**:
  - Extend `needy runner serve` with `--address`, `--name`,
    `--credential-env`, `--credential-file` (read-only, never writes
    `runner.json`), and `--activity-hook` (throttled async invocation on active
    exec/SFTP streams; failures logged only).
  - Support hot-reloading pre-provisioned static runner credentials in
    `needy-daemon` (e.g., on file mtime/inode change or `SIGHUP`), integrating
    them with the runner registry/scheduler while forbidding `rotate` and `rm`
    on static entries.
  - Add control-plane RPCs to mint and revoke short-lived runner credentials
    with mandatory `expires_ms`, enforced on every heartbeat and stream open,
    terminating active streams on expiry or revocation.
  - Track atomic `last_activity_ms` on each live `Run` (updated on guest RPC,
    gateway traffic, and terminal I/O) and expose it in `RunService` via
    Protobuf (regenerated with Buf).

## Testing Decisions

- **What makes a good test**: Assert observable external behavior only—gRPC
  authorization status codes, routed command output, blocked SSRF targets,
  console logs, trace/report records, activity timestamps, and external hook
  invocations—never internal channel plumbing.
- **In-process daemon seam (prior art: existing `needy-daemon` unit suite)**:
  Test through `needy-api-server`'s `Runs`, `Processes`, `Guest`, and `Runners`
  traits with the stand-in Firecracker harness:
  - `Caller::Actor` allowlist and cross-run isolation (`GetRun`, `ListRuns`,
    `Attach` with empty `run_id`, `KillProcess`, and `ImageService` all
    rejected; own `Decide`, `Exec`, and `GuestService` admitted).
  - Gateway resolve-then-validate rejecting loopback, link-local, IPv6
    metadata, IPv4-mapped IPv6, and bracketed IPv6 host parsing.
  - `--disable-host-files` rejecting `copy-in`/`mirror-out` targeting `host:`.
  - Static runner credential hot-reload without restarting the daemon, and
    short-lived runner credential expiry/revocation.
  - Remote actor bridge exclusivity, console log delivery (`needy run logs`),
    abrupt stream drop driving `RunState::Failed` + pool release, and
    `last_activity_ms` advancement.
  - `--activity-hook` throttling and non-fatal handling of non-zero exit codes
    using a minimal shell script boundary.
- **End-to-end seam (prior art: `cargo xtask e2e`)**:
  - Add a matrix cell running three isolated processes: a control daemon
    without KVM access, a remote Firecracker actor host bridge with `/dev/kvm`,
    and a `needy runner serve` with `--address`, `--credential-env`, and
    `--activity-hook`. Verify routed commands, workspace mount, gateway
    allow/deny, `needy run logs`, `needy run report`, hook invocation, and
    clean `SIGTERM` teardown.

## Out of Scope

- Kubernetes CRDs, `rc` controller changes, `rcctl` commands, or NetworkPolicy
  manifests (Phase 2).
- Bare-metal KVM node pools, KVM Device Plugin, or IaaS billing/quota
  integration (Phase 2).
- Non-KVM `in-pod` container shim adaptations and gVisor fallback (Phase 3).
- External device gateway RPC brokering and client CLI runner spawning
  (Phase 3).
- Multi-tenant RBAC inside a single `needy-daemon`.

## Further Notes

- Bridging vsock UDS inside the actor host process keeps the guest kernel and
  base ABI untouched, avoiding any base image rebuilds for Phase 1.
- Static runner credential hot-reload is essential for Phase 2 so adding a new
  Executor Workspace to a namespace never drops existing Actor runs.
