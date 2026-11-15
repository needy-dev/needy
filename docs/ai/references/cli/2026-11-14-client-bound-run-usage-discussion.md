# Starting a run from the execution machine

Status: workflow rationale aligned on 2026-11-15 with the approved
[client execution and actor lifecycle contract](../architecture/2026-11-15-client-execution-and-actor-lifecycle-spec.md).
This note replaces the earlier open-ended proposal. The approved behavior
still requires implementation and acceptance; the examples below must not be
read as evidence of shipped support.

## Everyday workflow

Start on the machine that owns the project. Select a control connection, edit
a local profile, and create an actor on the control host. Commands routed to
the client-selected executor execute on the initiating machine as the invoking
OS account. The actor's driving program can issue many commands within that
same run.

A separately enrolled persistent runner remains useful for an intentionally
provided remote resource. It is no longer a prerequisite for the approved
client-execution path. Better invitation wording alone cannot provide this
behavior: creation must supply an execution capability and automatically keep
its holder alive.

## Current behavior and evidence

Evidence level: source inspection. These links describe present code paths,
not acceptance of the new behavior.

| Question | Current behavior | Source |
| --- | --- | --- |
| Where is a profile read? | On the invoking client. The client resolves inheritance and recipe references and sends text. | [profile lookup](../../../../crates/cli/src/cli.rs), [run creation](../../../../crates/cli/src/commands/run/mod.rs) |
| What does a context change? | The control connection and identity, not the profile's executor placement. Each daemon has its own runner registry. | [configuration contract](2026-11-14-cli-configuration-and-access-spec.md), [scheduling](../../../../crates/daemon/src/schedule.rs) |
| Where does a runner name come from? | Enrollment names a persistent resource; `runner:` selects it rather than the current client. | [runner commands](../../../../crates/cli/src/commands/runner.rs), [guide generator](../../../../apps/docs/src/setup/guide.ts) |
| Can a profile have several executors? | Named entries are routing targets and can select different runners. | [profile schema](../../../../crates/config/src/profile/mod.rs), [executor schema](../../../../crates/config/src/profile/executor.rs) |
| What does omitted `runner:` mean? | The control host. | [scheduling](../../../../crates/daemon/src/schedule.rs) |
| Where is a workspace source? | `executor:NAME:PATH` is on that executor; its home expansion uses the instance's home. | [workspace setup](../../../../crates/daemon/src/actor.rs) |
| Does saving a profile on an executor publish it? | No. A CLI there can submit it with accepted client authority; runner enrollment does not grant run-management authority. | [creation](../../../../crates/cli/src/commands/run/mod.rs), [access contract](2026-11-14-cli-configuration-and-access-spec.md) |

The current `run create` source marks client-local workspace support as a
TODO. The approved contract now names that slice; the source marker is not
proof that it is implemented.

## Required creation sequence

1. The creating client reads its selected profile file, inheritance and recipe.
   `needy run create -f PATH` is the approved alias for profile input; current
   source uses `needy run create --profile PATH`.
2. Explicit temporary overrides can replace actor image, actor workspace,
   environment entries and entry command in the submitted snapshot. Arguments
   after `--` retain their append meaning. The source file is never rewritten.
3. The client validates the resulting profile before remote side effects.
   Client execution is selected explicitly, alongside existing named-runner
   selection. Omitting `runner:` retains the control-host meaning.
4. Creation supplies the invoking account's execution capability and binds
   every client-selected logical executor before the actor can issue work.
   Missing capability fails clearly; no registered name is guessed.
5. Needy automatically manages the execution holder under that account and
   creates the actor with accepted resources. The actor stays within current
   placement and driver limits.
6. Attach and detach affect the viewer. Work, its original executor bindings
   and required holders continue until actual work exit, explicit stop or
   genuine runtime loss.

The selector representation and temporary-flag spellings must be documented
with the implementation. This note does not invent runnable YAML or additional
CLI syntax beyond the approved `-f` alias.

## Keep the roles distinct

| Concept | Meaning |
| --- | --- |
| Connection context | Endpoint or connector plus credential for control access. |
| Profile | Requested actor, entry program, logical executors, rules and workspace. |
| Invocation binding | The accepted execution capability for this creation. |
| Daemon | Admission, scheduling, binding, state, routing and cleanup coordination. |
| Execution holder | Local ownership of processes and required channels, retained independently of a viewer or control connection. |
| Persistent runner | Explicitly enrolled execution infrastructure under its hosting account. |

Client and executor are roles. The creating machine can supply both, while a
later client only attaches or observes. Attachment never migrates execution to
the latest viewer. A reusable profile needs no personal runner name for the
client-selected path and can still use named persistent runners for other
logical executors.

## Execution authority

The [native driver](../../../../crates/core-executor/src/driver/native.rs)
and its [pipe](../../../../crates/core-executor/src/process/pipes.rs) and
[PTY](../../../../crates/core-executor/src/process/pty.rs) launch paths inherit
the hosting process's OS identity. A profile's `user` configures actors and
container executors; it does not authorize switching native host accounts.
This is source evidence of identity behavior, not a reproduced claim of
unauthorized privilege escalation.

For client execution, the holder inherits the invoking account. A privileged
persistent service cannot silently supply that account's authority. A context
change, relay or disconnected executor cannot cause fallback to the daemon
host, a root runner or another connected client. Forwarded operations retain
an explicit final execution owner.

Bindings must cover command and file operations and process control, not just
command starts. Associate them before actor startup and release failed
creation's resources. A freely supplied run ID or UID/GID is not execution
authority. One holder can serve several logical client-selected executors in
the invocation without publishing an unrestricted shared resource.

Authenticated clients retain daemon-wide visibility and existing grants.
Audit creator identity does not establish creator isolation. This slice does
not introduce human accounts, per-client resource scopes, creator ACLs or new
RBAC. Its binding checks prevent accidental account substitution and
cross-invocation capability reuse; they do not promise isolation from a
malicious admitted administrator.

NOTICE: persistent-runner delegation redesign, privileged UID-switching
brokers and migration compatibility layers are deferred at the owner's
request. Existing grants and execution semantics remain authoritative unless
this approved client-binding contract specifically changes them.

## Lifecycle and executor failure

Needy automatically retains a same-account execution holder after detach,
CLI termination or loss of a viewer connection. No manual `runner serve` is
needed for client execution. Control restart must recover the same surviving
actor and driving program through protected holders and verified stable
identity. Explicit stop ends the specified invocation; actual runtime or host
loss can still end it with failure or unknown outcomes.

Before dispatch, disconnected execution waits for a bounded configurable
deadline. Confirmed non-dispatch at expiry returns 125 and states that the
command was not started. A dispatched request with unknown outcome returns
255 and is never automatically retried. Missing `Started` is not proof of
non-dispatch. The target's actual exit code or signal passes through.

Needy's execution diagnostics use the profile's logical executor name once
resolved. Target stderr remains unchanged. A failed routed command does not
itself destroy the actor, though the driving program may choose to exit.
See [survival and history](../architecture/2026-11-15-actor-survival-and-history-design.md)
for the full rationale and current reconciliation gap.

## Required acceptance

Extend `cargo xtask e2e` with cells or invariants for the actual journey. Use
non-root execution accounts and a project directory on the intended executor.
Do not substitute a handwritten remote script for harness acceptance.

| Scenario | Observable result |
| --- | --- |
| Create from a local file, edit it and create again | Two distinct submitted snapshots; inheritance and overrides resolve locally and do not modify the file. |
| Linux actor with macOS or Linux client execution | Pipe and PTY commands report the invoking account, expected HOME and cwd; files appear on the correct machine. |
| Windows native execution where the existing toolchain permits | The same identity and process/file contract is verified on that supported test surface. |
| Privileged service exists beside an unprivileged client | Client-selected work runs as the invoking account, never the service's root account. |
| Simultaneous creations use the same reusable profile | Each retains its own supplied capability; missing capabilities fail without substitution. |
| Another client attaches or the connection context changes | Existing bindings, account and workspace stay fixed. |
| Detach, viewer kill, graceful control restart and control crash | Stable actor and program identity plus stateful continuity; no duplicate execution. |
| Executor unavailable before versus after dispatch | Correct 125 versus 255 distinction; no late execution after a not-started result. |
| Explicit stop, actual actor loss and holder cleanup | Correct owned-resource cleanup and retained truthful outcomes. |
| Missing or unreadable workspace | Error identifies the real source and execution side. |

Current [setup coverage](2026-11-15-setup-locations-and-coverage-evidence.md)
is not evidence that this journey passes. Only recorded behavioral acceptance
can justify recommending the new workflow as supported. Unavailable host
cells remain unverified. Existing pool/sharing policy, image preparation,
environment/secret handling and history storage remain as they are.
