# Needy design decisions

> **NOTICE (2026-11-12, migration):** the
> [drivers design](2026-11-12-drivers-and-extension-points-design.md) renamed what this note describes, without
> compatibility: `actor.runtime` is `actor.driver` (`firecracker` is
> `linux-firecracker`, `container` is `linux-container`); an executor's
> `provider` is its `driver` (`docker` is `linux-container`) and its `docker:`
> options are `container:`; the `ActorRuntime` trait is the actor `Driver`,
> the executor `Provider` is the executor `Driver`; capabilities read
> `actor(linux-firecracker) executor(linux-container,native)`. The text below
> keeps the names of its time, and its links to `examples/container` point to
> what is now `examples/linux-container`.

> **Migration notice (2026-11-10)**: *session* is now *run* in the code, the
> API, the CLI and the trace (slice 2 of
> [Runners and runs](2026-11-10-runner-and-run-design.md)). This note keeps
> the word it was written with: read *session* as *run*.

Reference for agents working in this repository. It records the decisions behind
Needy's shape, including decisions that are accepted but not yet implemented. It
is not a specification; read the code for current behavior.

> [!NOTE]
> The [actor and executor separation](2026-11-09-actor-executor-separation-design.md)
> note (2026-11-09) changes D5, D6, D9 and R2 and promotes O2, O3, O5 and O7.
> Read it before relying on those entries.

Status values:

- **accepted** — intended to be kept.
- **open** — discussed, not decided, not implemented.
- **rejected** — considered and deliberately not taken.

## Current snapshot

Implemented: Docker sessions with persistent workspace/home volumes, batch
commands with durable events, native host execution, pairing/TLS, and Linux
exec interception with a real proxy PID. There are no agent workloads.

Not implemented: profiles, authoritative mounts, the server/daemon/CLI role
split, an interactive `needy bash`, network policy, and a general placement
resolver.

## Language

**Workload**:
A command accepted by Needy for execution. Needy's only execution unit.
_Avoid_: agent, job, prompt

**Session**:
A persistent Docker workspace/home pair that may have one active run.
_Avoid_: sandbox, container

**Run**:
One accepted execution of a workload, with durable events and a terminal state.
_Avoid_: job, task

**Host daemon**:
The process that owns sessions, runs, pairing and native host execution.
_Avoid_: server, node

**Native host process**:
A command executed directly on the daemon's machine instead of in Docker.
_Avoid_: local exec

**Intercept**:
The Linux-side supervisor that replaces a selected exec with a proxy.
_Avoid_: hook, shim

**Proxy**:
The Linux process that keeps the original PID and bridges execution to a host
process.
_Avoid_: wrapper, stub

**Placement**:
Where an exec actually runs. The unit of placement is normally an execution
group, not an isolated binary.

**Execution group**:
A process subtree that shares one placement decision.

**Backend**:
The machine or daemon that executes a placed command.
_Avoid_: target, runner

**Profile**:
Configuration that maps commands and resources to placement, filesystem,
network and descriptor policy.
_Avoid_: config, rules

**Resource-level protocol**:
A protocol that transports operations such as spawn, signal and open, not raw
kernel syscalls.
_Avoid_: syscall forwarding

## Accepted decisions

### D1. Needy is agent-agnostic

**Status**: accepted, implemented

The core model contains no prompts, models, providers, agent sessions,
credentials or agent-specific transports. A workload is any command. Anything
agent-like runs on top of Needy like any other executable.

Why: agent behaviour changes faster than the runtime, and hard-coding it made the
runtime answer the wrong question.

### D2. Placement happens at exec; fork stays local

**Status**: accepted, implemented for one hop

`fork`, `clone` and `vfork` remain on Linux because they copy an address space.
Placement is decided at `execve`/`execveat`. A redirected exec is replaced with a
proxy executable rather than emulated.

Why: it preserves real PID, parent/child, process-group and wait semantics
without virtualizing the Linux process model.

### D3. Remote execution keeps a real Linux proxy PID

**Status**: accepted, implemented for one hop

The parent sees a real Linux child. The proxy holds the remote process
relationship and reports output, signals and exit status. The remote PID exists
only in runtime state.

Why: agents and harnesses already depend on `spawn`, `pid`, `kill` and `waitpid`.
Faking a PID would break them.

### D4. The cross-machine protocol is resource-level

**Status**: accepted, implemented

Needy transports operations such as spawn, signal, resize and stream data. It
does not forward Linux syscall numbers and register arguments to Darwin.

Why: Linux and Darwin differ in syscall numbers, flags, structures, ioctls and
process semantics. A resource protocol is smaller and more stable.

### D5. Only stdio, PTY and signals cross the boundary

**Status**: accepted, current scope

Inherited pipes, PTYs and standard streams are bridgeable. Arbitrary file
descriptors, Unix sockets, eventfds, memfds and shared memory are force-local.

Why: descriptor ownership and `exec` boundaries are where transparency breaks.
A narrow, explicit capability set is safer than pretending everything migrates.

### D6. Docker sessions are the isolation and persistence unit

**Status**: accepted, implemented

A session owns a workspace volume and a home volume. Runs execute in a
constrained container with a read-only root, non-root user, dropped capabilities
and no Docker socket.

Why: it gives a reproducible workload environment without inventing a sandbox.

### D7. Paired clients are trusted operators

**Status**: accepted, implemented

There is one authorization boundary per daemon, not per tenant. A paired client
may submit arbitrary commands and manage sessions. Pairing uses one-time codes;
bearer tokens are stored privately.

Why: the product is self-hosted execution for a trusted operator, not a
multi-tenant service.

Notice (2026-11-11): paired clients of a control plane no longer administer
it: join tokens, runner rotation and removal, pairing codes and client
revocation need the control token. See the
[runners and runs design](2026-11-10-runner-and-run-design.md#as-built-slice-7).

### D8. No implicit credentials

**Status**: accepted, implemented

Needy does not read a host login, select a model provider or inject secrets. A
workload gets only what its image and uploaded workspace contain, plus the
environment the operator explicitly allows for native execution.

Why: implicit credential projection couples the runtime to specific tools and
creates a false sense of isolation.

### D9. Workspace transfer is currently tar upload and download

**Status**: accepted, temporary

The current implementation copies a bounded tar archive into and out of a
session volume. This is a transfer mechanism, not a filesystem model.

Why: it was the smallest useful implementation. It is expected to be replaced by
O2 before the "reverse mount" experience is claimed.

## Open decisions

### O1. Profile-driven placement and resource policy

**Status**: open

Replace `--select` basename matching with a profile that declares placement
rules, execution-group behaviour, descriptor policy, environment allow-lists,
mounts and network mode.

Why it matters: basename matching cannot distinguish `pnpm dlx pi` from
`pnpm dlx prettier`, cannot express sticky execution groups, and has no place to
state which descriptors may cross the boundary.

### O2. The resource host owns the authoritative filesystem

**Status**: open

Mount the backend's filesystem into the Linux workload (for example FUSE, 9P or
SSHFS) so both sides read one authoritative tree. Tar transfer is not a live
view and cannot represent locks, rename atomicity or watches.

Why it matters: "looks like macOS" requires the same files, not a copy.

### O3. Split server, backend daemon and CLI

**Status**: open

Run orchestration and the workload environment on a Linux server, let resource
backends dial out to that server, and keep the CLI as a transport-only frontend.
The backend advertises its capabilities; the server resolves placement.

Why it matters: the intended topology is CLI on one machine, workload on a Linux
server, tools on a macOS backend. The current daemon only listens, and clients
dial it directly.

### O4. `needy <command>` and `needy bash` as first-class frontends

**Status**: open

A frontend should resolve a command inside the workload namespace and dispatch
it through placement, without requiring the user to create a session and a batch
run first. `needy bash` needs a persistent interactive PTY with attach, detach
and resize.

Why it matters: the current batch model is one run per session with event replay.
It is not an interactive environment.

### O5. Network policy is explicit and placement-relative

**Status**: open

Loopback resolves relative to the backend that executes the process, not the CLI
or the orchestrator. Model network as a profile field with at least `host`,
`proxied` and `deny`, plus an explicit localhost policy.

Trade-off: a real host network makes tools behave like the host, but also exposes
the host's address and local services. A transparent proxy controls egress but
cannot stop a program from contacting a literal address or tunneling over an
allowed domain. This is a trust boundary, not a security guarantee.

### O6. Placement uses execution groups and a resolver

**Status**: open

Once an execution group is placed remotely, its later execs inherit that
placement. Local placement is not sticky. Launcher identities such as
`pnpm dlx`, `npx`, `node <script>` and `bash -c` need explicit resolution.

Known limit: `bash -lc 'pnpm dlx pi'` cannot be resolved perfectly from syscall
observation alone. Accept `known launcher → deep resolve`, `known harness
identity → local`, `unsupported descriptor → local`, otherwise remote.

### O7. Backends advertise capabilities

**Status**: open

A backend should declare which executables, filesystem roots and network modes
it can provide. Placement then chooses a backend that can satisfy the workload
instead of assuming a single hard-coded host.

### O8. Windows can be an initiator

**Status**: open

The frontend should be able to run on Windows. The current CLI depends on Unix
terminal control, signals and `libc`, so this is not implemented.

### O9. Long-lived interactive sessions

**Status**: open

Support attaching to a running session, streaming PTY output, forwarding resize
and signals, and reconnecting. Batch runs remain for job-style work.

## Rejected decisions

### R1. Forward all syscalls to the target OS

**Status**: rejected

Forwarding Linux syscalls to Darwin would require translating ABI, flags,
structures and process semantics, with high per-syscall cost. The resource-level
protocol is the chosen alternative.

Why remembered: it is the obvious first instinct and will be proposed again.

### R2. Build a full syscall-virtualizing kernel

**Status**: rejected

A gVisor-style model is a much larger project than transparent placement and
spends most effort on kernel compatibility unrelated to the goal.

### R3. Put agent concepts in the core

**Status**: rejected, implemented as removal

Prompts, model providers, credentials, agent session IDs and ACP transport were
removed. They are workloads, not runtime concepts.

Why remembered: the previous implementation made the runtime answer only the
agent question; any new agent feature should be built as a workload or a profile,
not as a core kind.

### R4. Treat "agent-agnostic" as "no identity policy"

**Status**: rejected

Needy still needs to decide that some exec should stay local. That decision
belongs in a profile or resolver, not in daemon code. "Agent-agnostic" removes
agent knowledge from the mechanism; it does not remove policy.
