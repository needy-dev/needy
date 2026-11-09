# Terms and Concepts

The working vocabulary for Needy. Design notes, code names, proto fields and
CLI output use these terms. Terms marked **provisional** are design terms that
may still change; do not stabilize them in public APIs yet.

Background: [actor and executor separation](ai/references/architecture/2026-11-09-actor-executor-separation-design.md).

## Roles

### Actor

The side that runs the driving program. Usually one Firecracker microVM built
from a recipe. The program's view of the world, including the recipe identity,
lives here. Actors are stateless: a read-only root and a throwaway writable
layer.

*Avoid*: harness layer, caller, shallow layer.

### Driving program

The program inside an actor that drives the work: an agent harness, a test
suite or an acceptance script. Needy never routes the driving program itself;
only the commands it starts.

### Harness

One kind of driving program: an agent loop such as opencode, Codex or an
agent-written replacement.

### Executor

An environment where routed operations take effect: a container, a microVM, a
pod or a real machine such as a Mac. Executors hold state (the workspace).

*Avoid*: worker, needy client, target. A *runner* is the machine an executor
runs on, not the executor (see Runner).

### Executor instance

One running executor, created from a profile's executor entry. Commands whose
instance key is equal share one instance.

### Instance key

A template such as `{session}`, `{group}`, `{profile}` or `{rule}` that decides
which commands share an executor instance. Built only from deterministic
inputs, so a session can be replayed.

### Daemon

A `needy daemon` process. In **control** mode it is the control plane:
lifecycle of actors and executor instances, routing, the actor gateway and the
trace. In **executor** mode it only runs processes on its own machine.

*Avoid*: server.

### Runner (provisional)

A machine running `needy daemon` that has joined a control plane, with the
roles it can host (`actor` with its runtimes, `executor` with its providers)
and labels. Runners are registered, listed, rotated and removed; actors and
executors are instances that run on them. Designed in
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md);
not implemented yet.

## Sessions and configuration

### Session

One actor and the executor instances bound to it, started from a profile. One
RL rollout is one session. A session can also have no actor, for `needy exec`.

A session is `creating` while it acquires its executors, copies files in and
starts its actor, then `running`. It ends `stopped` (after `stopping`, by
`needy session stop`), `finished` (its actor's driving program exited) or
`failed` (its create failed). The first of these end states stays: stopping a
finished session releases its executors but keeps it `finished`.

*Avoid*: rollout. The [runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md)
design renames *session* to **run**; until that lands, *session* is the
current term.

### Group

A name shared by several sessions so that executors keyed by `{group}` serve
all of them, for example a parent agent and its sub-agents. **Provisional.**

### Profile

A declarative YAML description of one session: which recipes to use, executors,
command rules, file mappings, both networks and chaos.

*Avoid*: config.

### Preset

A named, reusable block of rule actions inside a profile, applied with `use:`.

### Recipe

A declarative YAML description of one machine: image, kernel, CPU, memory,
firmware, devices and network identity. Built by `needy build`. Built-in
recipes are referenced as `builtin:NAME`.

*Avoid*: persona.

## Routing

### Placement

Where an operation runs: the actor, or a named executor.

### Decision

The result of evaluating a command against a profile: the placement, and the
rewritten arguments, environment and working directory, or a denial.

### Command rule

A profile entry that matches a command (`match`, `when`) and applies actions
(`to`, `args`, `env`, `deny`, …). Matching rules accumulate in order.

### Effective command

The command a launcher such as `env`, `sudo`, `npx` or `sh -c` really runs,
found by a launcher resolver. Rules match the effective command.

### Route list

The executable names the actor's kernel hands to the proxy. Execs not on it
run in the actor without consulting the daemon.

### Proxy

`/.needy/proxy` in the actor. It replaces a routed exec, asks the daemon for a
decision, and either runs the real program locally or streams it from an
executor. The parent sees an ordinary child process and exit status.

### Path map

A prefix mapping from actor paths to executor paths, for executors whose
workspace lives elsewhere, such as a Mac.

## Files and networks

### Workspace

The directory the work operates on. It lives on an executor and appears in the
actor at the same path, `/home/<user>/workspace` by default.

### File mapping

A named profile entry that moves files: **copy-in** at session start,
**mount** for a live view, **mirror-out** back to the host.

### Actor network and executor network

Two separate networks. The actor network sits near the actor's main dependency
(inference) and reaches the outside only through the **actor gateway**, which
enforces an allowlist and injects credentials. The executor network sits near
what the work operates on.

## Shaping

### Chaos

Deterministic fault injection: a fault fires from the profile's seed, its index
and how many times it has matched, so a replayed session injects the same
faults.

### Fault

One chaos rule, by kind: `exec`, `net.connect`, `file.open` or `hwmon`.

### Identity

What a program can learn about the machine it runs on: OS, kernel, CPU,
firmware, devices, network identity. A recipe defines it.

## Observation

### Trace

Every event recorded during a session, in order: each carries the session, a
sequence number, a time, a kind such as `exec.decide` and a payload. The
profile's `trace:` selects the sinks: SQLite (`trace.db`) and JSONL under the
session directory. Terminal output is not part of the trace.

### Trace gap

A `trace.gap` event: the trace dropped this many events at this point because
its queue was full. A report of a trace with gaps says it is incomplete.

### Report

A fixed-format markdown summary of one session, written by `needy report`.

## Processes

### Process

A process an executor started for a client. The executor owns it, so a client
can **detach** and later **attach**, replaying recent output.
