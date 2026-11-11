# Terms and Concepts

The working vocabulary for Needy. Design notes, code names, proto fields and
CLI output use these terms. Terms marked **provisional** are design terms that
may still change; do not stabilize them in public APIs yet.

Background: [actor and executor separation](ai/references/architecture/2026-11-09-actor-executor-separation-design.md).

## Roles

### Actor

The side that runs the driving program: one Firecracker microVM, or one
container on a host without KVM, built from a recipe. The program's view of the world, including the recipe identity,
lives here. Actors are stateless: a read-only root and a throwaway writable
layer.

*Avoid*: harness layer, caller, shallow layer.

### Actor runtime

What runs an actor next to the daemon, named by the profile's required
`actor.runtime`: `firecracker` (a microVM; needs `/dev/kvm`) or `container`
(an OCI container through Docker, with the required
`actor.container.oci_runtime`: `runc`, or gVisor's `runsc`; Linux only). The
runtime starts, waits for and stops the actor, names the channels the daemon
serves (guest API, gateway) and dials (terminal), and says who sets the actor
up: a VM's init inside, or the container runtime from the host (workspace
mounts, hostname, identity files). The report names it (`Actor  container
(runsc)`), since it decides the isolation. See the
[container actor runtime design](ai/references/architecture/2026-11-10-container-actor-runtime-design.md).

Provisional, on the `poc/macos-native-actor` branch only: `macos-native`, the
driving program as a process on a Mac, fenced by a Seatbelt profile (the
**fence**), with shims alone on its `PATH`. No VM and no identity. See the
[macOS native actor proof of concept](ai/references/architecture/2026-11-12-macos-native-actor-poc.md).

### Driving program

The program inside an actor that drives the work: an agent harness, a test
suite or an acceptance script. Needy never routes the driving program itself;
only the commands it starts.

With `run create -t`, the driving program has a terminal; `-it` also
attaches immediately. `run attach RUN` reconnects to that terminal, and
detaching leaves the run alive. Init keeps its output in the actor's console
while no client is attached. See the [automated and live-probe evidence](ai/references/cli/2026-11-11-interactive-actor-evidence.md).

### Harness

One kind of driving program: an agent loop such as opencode, Codex or an
agent-written replacement.

### Executor

An environment where routed operations take effect: a container, a microVM, a
pod or a real machine such as a Mac. Executors hold state (the workspace).

*Avoid*: worker, needy client, target. A *runner* is the machine an executor
runs on, not the executor (see Runner).

### Executor instance

One running executor, created from a profile's executor entry by its
**provider** on the runner its selector names: `native` (the runner's own
process space) or `docker` (a container). Commands whose instance key is
equal share one instance.

### Instance key

A template such as `{run}`, `{group}`, `{profile}` or `{rule}` that decides
which commands share an executor instance. Built only from deterministic
inputs, so a run can be replayed.

### Daemon

A `needy daemon` process. In **control** mode it is the control plane:
lifecycle of actors and executor instances, routing, the actor gateway, the
trace and the runner registry. In **executor** mode it only runs processes on
its own machine, as inside a Docker executor's container. In **runner** mode it joins a control plane as a runner,
sends it heartbeats, and runs the processes the control plane asks for over
the connection it dialed; it listens nowhere.

*Avoid*: server.

### Runner

A machine running `needy daemon --mode runner` that has joined a control
plane. It reports its **capabilities** and **labels** (`KEY=VALUE`), and sends a heartbeat at the interval the control
plane sets (15 s by default); it is *online* while one arrived within the last
three intervals, else *offline*, and *removed* once an operator removed it.
Runners are registered (`needy runner token create`, then a join), listed
(`needy runner ls`), rotated (`needy runner rotate`) and removed
(`needy runner rm`). Actors and executors are instances that run on them; a
runner's name is a DNS label. Designed in
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md).

A runner dials the control plane and keeps a **control stream** open on which
the control plane announces **tasks**; for an exec the runner opens a **task
stream** that carries the exec. `needy exec --runner NAME` runs a command on a
connected runner's machine. Each heartbeat reports the runner's **host**: its
platform, CPU and the CPUID entries its KVM supports. Profiles place executors
and actors on runners with **runner selectors**.

*Avoid*: agent, worker, node.

### Capabilities

What a runner can host: each **role** it holds (`actor`, `executor`) with its
actor **runtimes** (`firecracker`, `container`) or executor **providers**
(`native`, `docker`), written `actor(firecracker) executor(docker,native)`. A
runner detects them when it starts: a KVM device that opens for reading and
writing and a complete actor image give `actor(firecracker)` (Linux only); a
Docker daemon that answers gives `executor(docker)` and `actor(container)`
(whether it has the profile's OCI runtime and the container actor image is
checked when a run is created); `executor(native)` is always there. `--roles` restricts a runner to some roles and cannot add one the
machine lacks. Capabilities travel in the join and every heartbeat, are kept
in `state.db`, and are what scheduling and `profile check` match: an actor
needs `actor(RUNTIME)` of its runtime, an executor `executor(PROVIDER)`. A join token's
scope still names roles.

### Ownership and grace period

A runner **owns** the processes it runs for a run: each carries the run's
ID. When a run ends, the control plane tells every runner it held an
executor on to **end the run**, and the runner kills the run's processes,
a workspace mount's SFTP server among them. A runner that cannot reach the
control plane keeps them for its **grace period** (2 minutes), then ends
them itself; processes outside any run (`needy exec --runner`) are never
ended this way.

### Reconciliation

What a control plane does with what it finds. When a runner connects it
**reports** the processes of runs it still runs; the control plane
**claims** those of live runs that hold an executor on it and tells it to
end the others. When a control plane starts, it loads from `state.db` the
runs it left open: it does not adopt their actors, so each ends `failed`
(`the control plane restarted; the actor was lost`), or `stopped` if it was
stopping; it ends a still-running actor VM and removes their control-host
containers, and waits for runners to report the instances the runs held
there.

### Lost instance

An instance of a run that nobody reported after the control plane
restarted: no runner reported it within the wait (60 s), or it was one of
the control host's own native executors, whose processes a killed daemon
leaves unrecorded. Whether it still runs is unknown; `run ls` marks it
`(lost)`. The runner's own grace period still ends its processes.

### Runner selector

A profile's `runner:` on an executor or the actor: a runner's name, or
`{ labels: [KEY, KEY=VALUE] }` for the one connected runner with the capability
the instance needs and every label. Without one, the actor or executor runs on the control host
itself. The control plane resolves selectors when a run is created
(**scheduling**); a selector that names no connected runner, or labels that
fit several, is an error naming the candidates.

### Local runner

The runner that runs on the control host, named by `needy daemon --mode
control --local-runner NAME`. Actors and Docker executors placed on it are
created by the control plane itself; as built, actors and Docker executors
run nowhere else.

### Control stream and task stream

A runner's **control stream** is the one long-lived stream it opens to the
control plane (`RunnerService.Control`) while it runs; it connects again with
backoff when the stream drops. The control plane announces each **task** on
it with an ID: listing or killing processes, answered on the control stream,
or an exec, for which the runner opens a **task stream**
(`RunnerService.Task`) that carries the exec's own messages. A runner whose
control stream is not open is *not connected*; tasks for it fail at once.

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

### Control token

The bearer token a control daemon reads from `--token-env`: the bootstrap and
admin credential, held by the **operator**. It is required for a TCP listener
on a non-loopback address; the control socket on the control host needs none
(its file permissions guard it). An operator uses it to pair the first clients
and runners; day-to-day clients use their own **client tokens**.

### Client

A program that calls the control plane on behalf of a person or a tool: the
CLI on another machine, a script. A **paired** client holds its own **client
token**, is listed (`needy client ls`) and can be revoked (`needy client
revoke ID`); the trace names it `client:NAME`. A client drives runs (create,
list, stop, exec, attach, kill, `ps`, `fs cp`, reports and logs) but does not
administer the control plane: creating or revoking join tokens, rotating or
removing runners, creating pairing codes and revoking clients need the control
token. The
CLI keeps one token per control plane endpoint in `credentials.json` in its
config directory (`$XDG_CONFIG_HOME/needy`, mode 0600) and presents it to that
endpoint; `NEEDY_TOKEN` overrides it. The control plane stores only the
token's hash. A client's name is a DNS label that no other client that is not
revoked holds.

*Avoid*: user, account. Needy has no users; a client is a credential with a
name.

### Pairing code

A short-lived (10 minutes by default), single-use secret an operator creates
(`needy client pair create [--name NAME]`) and hands to a new client, which
exchanges it for its client token (`needy client pair --endpoint URL CODE`).
Shown once; stored hashed.

### Principal

Who caused a trace event: `operator` (the control token or the control
socket), `client:NAME` (a paired client's token), `runner:NAME` (a runner's
credential) or `actor:RUN` (an actor, through its own guest channel).
`run.create`, `run.stop`, `exec.start` and `exec.kill` record it as `by`;
what the control plane does on its own, such as stopping runs at shutdown,
records none. `needy report` shows who started and stopped a run.

## Runs and configuration

### Run

One execution of a driving program in an actor, with the executor instances
bound to it, started from a profile, and its trace and report. One RL rollout
is one run. Every run has an actor; `needy exec --runner` runs commands on a
runner's machine without a run, and a run whose actor only waits serves
`needy exec --run` (see
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md)).

A run is `creating` while it acquires its executors, copies files in and
starts its actor, then `running`. It ends `stopped` (after `stopping`, by
`needy run stop`), `finished` (its actor's driving program exited) or
`failed` (its create failed, its actor's VM exited before the driving
program did, or the control plane restarted and lost its actor, with the
reason). The first of these end states stays: stopping a
finished or failed run releases its executors but keeps its state. The
control plane writes every change of a run's state, and of the instances
and processes it holds, to `state.db`.

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

The executable names the actor hands to the proxy: its kernel in a VM, its
shims in a container. Execs not on it run in the actor without consulting
the daemon. It holds the command name of every rule, whatever the rule does
(place, deny, or change the environment, arguments, working directory,
timeout or faults), the command name of every `chaos.faults` entry with
`on: exec`, and the shells; a rule or exec fault whose pattern may match any
program, or an exec fault without a pattern, makes it `*`, every exec.

### Shim

In a container actor, a hard link to `/.needy/proxy` in place of each
program that was on the image's `PATH` when the image was built; the real
program is at `/.needy/real/<path>`. A shim runs its real program with the
caller's argv, or hands the exec to the proxy when the run routes its name.
Programs created after the build and run directly are not routed.

In a `macos-native` actor (provisional, proof of concept), a symbolic link to
the needy binary, one per program name on the actor's search path, in a
directory that is the driving program's whole `PATH`. Its real program cannot
run inside the fence, so a command the daemon keeps, or one the run does not
route, is started by the actor's init under the same fence with that one
program allowed (a **kept command**).

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
sequence number, a time, a kind such as `exec.decide`, a payload and the
payload's schema version (4 since `actor.start` names its runtime; versions 1 to 3
still read).
The profile's `trace:` selects the sinks: SQLite (`trace.db`) and JSONL under
the run directory. Terminal output is not part of the trace.

### Trace gap

A `trace.gap` event: the trace dropped this many events at this point because
its queue was full. A report of a trace with gaps says it is incomplete.

### Report

A fixed-format summary of one run, written by `needy report` as plain text,
markdown or JSON from the run's events, which the control plane sends to any
client it reaches (`--local` reads this machine's `trace.db` instead).

## Processes

### Process

A process an executor started for a client. The executor owns it, so a client
can **detach** and later **attach**, replaying recent output.
