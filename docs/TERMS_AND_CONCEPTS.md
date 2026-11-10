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

A template such as `{run}`, `{group}`, `{profile}` or `{rule}` that decides
which commands share an executor instance. Built only from deterministic
inputs, so a run can be replayed.

### Daemon

A `needy daemon` process. In **control** mode it is the control plane:
lifecycle of actors and executor instances, routing, the actor gateway, the
trace and the runner registry. In **executor** mode it only runs processes on
its own machine. In **runner** mode it joins a control plane as a runner,
sends it heartbeats, and runs processes as in executor mode.

*Avoid*: server.

### Runner

A machine running `needy daemon --mode runner` that has joined a control
plane. It declares the **roles** it can host (`actor`, `executor`) and
**labels** (`KEY=VALUE`), and sends a heartbeat at the interval the control
plane sets (15 s by default); it is *online* while one arrived within the last
three intervals, else *offline*, and *removed* once an operator removed it.
Runners are registered (`needy runner token create`, then a join), listed
(`needy runner ls`), rotated (`needy runner rotate`) and removed
(`needy runner rm`). Actors and executors are instances that run on them; a
runner's name is a DNS label. Designed in
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md).

Roles do not yet carry their runtimes or providers, and profiles do not yet
select runners; until then a runner also serves the executor role on its own
listener, which a profile's `remote` executor dials.

*Avoid*: agent, worker, node.

### Join token

A short-lived secret an operator creates with `needy runner token create` and
hands to one machine so it can join as a runner. It has a time to live, a
number of uses (1 by default), the roles it allows (every role when it names
none) and labels it gives every runner that joins with it. The control plane
stores only its hash; it is shown once. A token that expired, is used up or
was revoked is refused.

### Runner credential

The secret a runner gets when it joins, its bearer token for heartbeats from
then on; the join token is not needed again. The runner keeps it in
`runner.json` in its state directory (mode 0600); the control plane stores
only its hash. `needy runner rotate` hands the runner a new one at its next
heartbeat, and the old one stops working once the runner uses the new one;
`needy runner rm` revokes it.

## Runs and configuration

### Run

One execution of a driving program in an actor, with the executor instances
bound to it, started from a profile, and its trace and report. One RL rollout
is one run. A run can also have no actor, for `needy exec`
(`needy run create --no-actor`, until `exec --runner` replaces it; see
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md)).

A run is `creating` while it acquires its executors, copies files in and
starts its actor, then `running`. It ends `stopped` (after `stopping`, by
`needy run stop`), `finished` (its actor's driving program exited) or
`failed` (its create failed, or its actor's VM exited before the driving
program did, with the reason). The first of these end states stays: stopping a
finished or failed run releases its executors but keeps its state.

*Avoid*: session (the earlier name), rollout.

### Group

A name shared by several runs so that executors keyed by `{group}` serve
all of them, for example a parent agent and its sub-agents. **Provisional.**

### Profile

A declarative YAML description of one run: which recipes to use, executors,
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

A named profile entry that moves files: **copy-in** at run start,
**mount** for a live view, **mirror-out** back to the host.

### Actor network and executor network

Two separate networks. The actor network sits near the actor's main dependency
(inference) and reaches the outside only through the **actor gateway**, which
enforces an allowlist and injects credentials. The executor network sits near
what the work operates on.

## Shaping

### Chaos

Deterministic fault injection: a fault fires from the profile's seed, its index
and how many times it has matched, so a replayed run injects the same
faults.

### Fault

One chaos rule, by kind: `exec`, `net.connect`, `file.open` or `hwmon`.

### Identity

What a program can learn about the machine it runs on: OS, kernel, CPU,
firmware, devices, network identity. A recipe defines it.

## Observation

### Trace

Every event recorded during a run, in order: each carries the run, a
sequence number, a time, a kind such as `exec.decide` and a payload. The
profile's `trace:` selects the sinks: SQLite (`trace.db`) and JSONL under the
run directory. Terminal output is not part of the trace.

### Trace gap

A `trace.gap` event: the trace dropped this many events at this point because
its queue was full. A report of a trace with gaps says it is incomplete.

### Report

A fixed-format markdown summary of one run, written by `needy report`.

## Processes

### Process

A process an executor started for a client. The executor owns it, so a client
can **detach** and later **attach**, replaying recent output.
