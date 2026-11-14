# Actor survival, reconnection and history

Status: rationale for the approved [client execution and actor lifecycle
contract](2026-11-15-client-execution-and-actor-lifecycle-spec.md), 2026-11-15.
Implementation and behavioral evidence remain pending. Source observations
below describe the present implementation; the required behavior is the
approved contract, not a claim of shipped support.

## Separate connections, live work and history

One run records one actor invocation and its bindings. Losing an attached
client does not mean losing that invocation. Reconnecting to a surviving
program, replaying terminal output and rebuilding an environment are different
operations.

| Dimension | What it describes | What it cannot establish |
| --- | --- | --- |
| Client connection | An attached viewer and its terminal input/output connection. | No viewer does not mean the program exited. |
| Live work | The actual actor, processes and resources, including temporarily unreachable resources. | A missing response is not proof of death; a database row is not proof of life. |
| Persisted history | Requested configuration and observed execution facts and outcomes. | History and screen replay do not restore process memory or make replay safe. |

## Required lifecycle

| Event | Required behavior | Boundary |
| --- | --- | --- |
| Detach | End the viewer connection and retain the actor. | A later attach keeps the original executor and workspace bindings. |
| CLI crash, terminal close or connection loss | Preserve work and allow reconnection. | Client execution needs its automatically managed same-account holder. |
| Graceful control restart or control crash | Recover management of the same surviving actor and driving program. | Verify stable identity and protected access; a PID alone is insufficient. |
| Executor disconnection | Preserve the actor and apply the execution-failure contract below. | Do not change machines or accounts and do not retry uncertain work. |
| Foreground Ctrl-C | Follow PTY foreground signal behavior. | An interactive shell can survive its child command; entry-program exit ends the invocation. |
| Entry-program exit | Preserve its actual result and end owned work. | It is distinct from detach and from infrastructure loss. |
| Explicit actor stop or kill | End the specified invocation and owned processes. | Process kill, actor stop and historical-record deletion are distinct operations. Shared resource references remain authoritative. |
| Actual holder, runtime or host loss | Record failure or unknown outcomes truthfully. | Probe the actual resource; restarting a runtime management service alone does not prove every actor died. |
| Start again after loss | Create a new invocation. | Rebuilding or rerunning is not continuation of the original process. |

The actor remains disposable. Its lifetime is determined by work and explicit
termination, not by the last viewer's lifetime.

## Hold work independently of control management

To survive control restart, process, PTY, driver-channel and per-run service
ownership cannot disappear with control-service cancellation or `Drop`. A
protected local holder keeps those resources alive, and restarted management
reconnects after verifying their identity. Required per-run services must
survive the same gap; preserving only a VM PID is insufficient.

When the client also hosts execution, Needy automatically keeps an execution
holder under the invoking OS account. The user does not manually enroll a
runner or keep a second terminal running `runner serve`. The holder lasts as
long as its work requires. It must not borrow an unrelated root runner's
account, and it does not become an unrestricted shared runner resource.

An execution holder may serve multiple logical client-selected executors in
the invocation. Existing instance and pool policy still decides environment
sharing. The implementation may choose process granularity within this
contract; logical responsibilities do not require another manually installed
service. Bindings and protected recovery access must remain specific enough
to prevent accidental cross-invocation capability substitution.

A background holder cannot make an offline or rebooted client machine's
filesystem remotely available. Actual holder loss remains different from
losing a viewer or a management connection.

NOTICE: this contract does not add process-memory restoration, automatic
actor reconstruction, a new pool-sharing model or a human-account/RBAC system.
Those remain deferred until separately requested by the owner.

## Executor unavailability and actor-visible results

A capture has normally already started the shim/proxy child process when an
executor becomes unavailable. The actor sees that child's stderr and exit
result. It does not receive a raw gRPC exception as if the target application
had thrown one.

Wait only before dispatch while the chosen executor is disconnected, with a
bounded configurable deadline. Once execution was dispatched, missing a
`Started` event is not proof that execution never occurred.

| Known fact | Actor-visible result | Required invariant |
| --- | --- | --- |
| Connection recovers before the deadline, and no request was dispatched yet | Execute and return the target's actual result. | A single dispatch follows the accepted binding. |
| Deadline expires with confirmed non-dispatch | Exit 125; identify executor unavailability and state that the command was not started. | That command cannot execute late after this result. |
| Target executes and exits | Pass through its exit code or signal. | Preserve the program's own outcome. |
| Execution was dispatched but its outcome cannot be established | Exit 255 and report outcome unknown. | Do not label it not-started or retry it automatically. |

An infrastructure timeout must not produce a retry-safe promise without proof
of non-dispatch. Reconnection can reconcile an existing execution identity; it
must not submit the same uncertain work again.

Needy-generated execution diagnostics use the profile's logical executor name
once resolved, for example:

```text
project: executor is unavailable; the wait deadline expired.
project: the command was not started on the executor.
```

A runner name, machine or instance identifier is not the diagnostic prefix.
The target program's stderr remains byte for byte unchanged. Errors before a
logical executor is known may retain the product prefix.

Exit 125 is this contract's infrastructure convention, not a universal shell
standard or proof by itself: a target program can also exit 125. The actual
execution facts must support the classification. Existing denial and
start-failure behavior stays except where the approved distinction requires a
change. The [Docker exit-status reference](https://docs.docker.com/engine/containers/run/#exit-status)
was consulted on 2026-11-15 as an analogy, not as the source of a complete
copied error model; no stable version permalink was recorded.

A failed routed command does not itself instruct infrastructure to destroy the
actor. A driving program can handle the error. An entry script with `set -e`,
or an application that chooses to exit, can still end through ordinary process
semantics.

Current source evidence: [proxy errors and exits](../../../../crates/core-actor/src/guest/proxy.rs),
[exit-mapping tests](../../../../crates/core-actor/src/guest/proxy_test.rs) and
[CLI unknown-result handling](../../../../crates/cli/src/commands/proc/exec.rs).
The current proxy's general pre-start error path uses 126; unknown stream
outcomes use 255. The bounded-wait and logical-prefix behavior above still
requires implementation and regression evidence.

## References and their limits

Evidence level: existing upstream documentation and pinned source/test
inspection. No reference product was run for this documentation work.

| Reference | Observed model | Relevant lesson |
| --- | --- | --- |
| tmux | Clients detach from a server that owns terminals; killing the server destroys its sessions. | A viewer need not own the work's lifetime. |
| Herdr TUI | Documentation separates detach survival, layout restoration after restart and optional live handoff. | Display restoration and live process continuity need separate claims. |
| herdr-core | A holder owns the PTY and child; core reconnects, with a restart test comparing the original PID. | Separate management from protected work ownership; holder loss is a different failure. |
| Atuin | History includes command and context, timing and exit facts. | History informs inspection, not proof of a live process or safe replay. |

Sources retained from the investigation:

- [tmux manual, pinned commit](https://github.com/tmux/tmux/blob/75fac4a77bb40dc655f8b217071c224c0c819c7f/tmux.1) and [getting-started reference](https://github.com/tmux/tmux/wiki/Getting-Started).
- [Herdr TUI session state](https://herdr.dev/tui/docs/session-state/), consulted 2026-11-15; the page has no stable version permalink.
- [herdr-core holder](https://github.com/herdrdev/herdr-core/blob/c84d6e76156a6c2aac1da20efbe800f10acfe897/crates/herdr-core/src/holder.rs#L1-L4) and [restart survival test](https://github.com/herdrdev/herdr-core/blob/c84d6e76156a6c2aac1da20efbe800f10acfe897/crates/herdr-core/tests/persistence.rs#L10-L32).
- [Atuin history context](https://github.com/atuinsh/atuin/blob/6e57c7e3dc8ddcbfdcbd65989ca86f81f5b2908f/docs/docs/guide/basic-usage.md#L5-L14).

## History and durable data

Keep existing trace and report storage. A run's configuration and observed
outcome remain useful after live work ends. Unknown outcomes stay unknown;
do not invent successful exits or treat screen replay as process restoration.
Atuin is a reference for historical facts, not authorization to collect all
environment variables, secrets or terminal output.

Persistent files belong on executors or explicitly persistent targets. Actor
memory, unwritten output and a disposable writable layer can be lost. A mapping
that mirrors out only at the end provides no guarantee after abrupt loss.
Report actual synchronization failures or unknown results truthfully.

NOTICE: history schema, synchronization, search, retention, environment and
secret handling remain as currently implemented. The owner deferred their
redesign; a separate approved slice is required to reopen them.

## Current gap and acceptance

Current [reconciliation](../../../../crates/daemon/src/reconcile.rs) marks open
runs failed after restart and cleans up their actors.
[Shutdown](../../../../crates/daemon/src/lib.rs) also stops runs. That behavior
does not meet the newly approved control-restart survival contract.

Acceptance must show the same actor and driving-program identity after viewer
loss, graceful control restart and control crash. A stateful marker must prove
continuity without duplicated execution. Also verify stable execution account,
explicit stop and cleanup, real holder/actor loss, retained outcomes, and the
pre-dispatch versus unknown-outcome distinction. Add cells or invariants to
`cargo xtask e2e`; a build, a screenshot or a newly created replacement actor
cannot establish continuity. Unavailable host cells remain unverified.
