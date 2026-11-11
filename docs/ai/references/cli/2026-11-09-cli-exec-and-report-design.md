# CLI, exec streams and reports

**Status**: pending; implemented as described unless the [closed-loop results](../architecture/2026-11-09-closed-loop-results-evidence.md#not-yet-verified) list it as missing. Terms follow the
[actor and executor separation](../architecture/2026-11-09-actor-executor-separation-design.md)
note. Profiles are described in
[Profiles and rules](../config/2026-11-09-profile-and-rules-design.md).

## Command surface

```text
needy daemon   [--mode control|executor|runner]  # control plane; executor or runner on every other machine
needy build    [--recipe FILE] [--out DIR] [--executor]  # build machine images through the Docker API
needy run  create|ls|stop|logs [-f] [--group G] # one actor plus its executors; logs prints (and follows) its console
needy exec     [--run ID | --executor NAME | --runner NAME] -- CMD …   # run a command through placement
needy ps [-a] [--run ID | --runner NAME] | attach ID | kill ID   # manage detached processes
needy fs       cp [--run ID | --runner] SRC EXECUTOR:DIR | EXECUTOR:PATH DIR   # tar over an exec stream
needy report   RUN|latest [--level summary|detailed] [-o human|markdown|json] [--file FILE] [--local]
needy profile  check PROFILE [--cmd "COMMAND"]…  # routing decisions, matching faults, recipe findings
needy runner   token create [--ttl 1h] [--uses N] [--roles R,…] [--labels K=V,…] | token ls | token revoke ID
needy runner   ls | rotate NAME | rm NAME      # runners that joined this control plane
needy client   pair create [--ttl 10m] [--name NAME] | pair CODE | ls | revoke ID   # paired clients
```

A paired client may use every command above except `runner token create`,
`runner token revoke`, `runner rotate`, `runner rm`, `client pair create` and
`client revoke`, which need the control token (`NEEDY_TOKEN`) or the control
socket; the control plane refuses them with "… needs the control token".

Every command takes `--endpoint` (or `NEEDY_ENDPOINT`) and `-v`, which turns
on logs on stderr: `-v` info, `-vv` debug for needy's crates, `-vvv`
everything. Logs are off by default; the daemon also writes
`daemon.YYYY-MM-DD.log` in its state directory.

`ps` and `run ls` print borderless tables, or a JSON array with
`-o/--output json`. `ps` shows each process's `RUN` (`-` outside any
run) and `ORIGIN`, who started it: `cli` (`needy exec`), `actor-proxy`
(a command the actor routed), `fs` (`needy fs cp`), `files` (the daemon's
copy-in and mirror-out) or `actor-mount` (the SFTP server behind an actor's
workspace mount). The last three are needy's own processes; `ps` hides them
unless `-a/--all` asks for them, in the table and in JSON alike, whose
objects carry `run` and `origin`. The control daemon records the origin
from the start it relays; a process it did not start shows `-` (JSON
`null`). `--run ID` lists one run's processes, `--runner NAME` one runner's;
without either, `ps` lists every executor's and every connected runner's.
`STATE` is `running`,
`exited` or `killed` (a signal ended it); `EXIT` is the exit code or the
signal name, empty while the process runs. JSON carries `state`,
`exit_code` and `signal` as separate fields.

`run ls` lists stopped, finished and failed runs with their state from
the control plane's `state.db`, so they stay listed across control plane
restarts. Its columns are `ID`, `NAME`, `STATE`, `GROUP`,
`EXECUTORS` and `EXIT`. `NAME` and `GROUP` show `-` when they were not
given and default to the ID; JSON keeps the real values. `EXECUTORS` keeps
the executors a run had after it gives them back, marked
`build=docker (released)`; an executor that no runner reported after a
control plane restart is marked `mac=runner (lost)`. A run that was
running when its control plane died ends `failed` with `the control plane
restarted; the actor was lost` at the next start. `EXIT` is the driving program's exit code or
signal, or the first 48 characters of why a failed run failed; it is
empty while the driving program runs and for a run without an actor. JSON
carries `executors` as objects with `name`, `runtime`, `released` and
`lost`, and
`exit_code`, `signal` and `failure` (the whole reason) as separate fields.

```text
ID            NAME  STATE     GROUP  EXECUTORS                 EXIT
b5eac210aee1  -     finished  -      build=docker (released)   0
4c0f3e9d1a27  -     running   -      here=native
```

`run logs RUN` prints the actor's
serial console: the guest's boot messages and the driving program's terminal.
With `-f/--follow` it keeps printing what the actor writes, as it arrives,
until the run stops running (its driving program exits, it fails or it is
stopped); the control plane streams it (`GetRunLogs.follow`), so it works
from any client.

### Clients

Every command reaches the control plane at `--endpoint` (or
`NEEDY_ENDPOINT`; the control socket by default) and presents a bearer token:
`NEEDY_TOKEN` when it is set, else the client token stored for that endpoint,
else none. On the control host the socket needs none; elsewhere a machine
pairs once:

```text
# where the control plane is reachable with the control token
NEEDY_TOKEN=… needy --endpoint http://127.0.0.1:7443 client pair create --name laptop
needy: pairing code 1a2b3c4d5e6f for client laptop, valid until …; it is not shown again
needy-pair.1a2b3c4d5e6f.…
# on the client
needy --endpoint http://127.0.0.1:7443 client pair needy-pair.1a2b3c4d5e6f.…
needy: paired as client laptop (6f5e4d3c2b1a); its token for http://127.0.0.1:7443 is in ~/.config/needy/credentials.json
```

`client pair create` prints the code alone on stdout; `--ttl` defaults to 10
minutes and `--name` to `client-<code ID>`. `client pair CODE` sends no token
(the code authenticates it) and stores the client token in
`credentials.json` in the config directory (`$XDG_CONFIG_HOME/needy`, or
`NEEDY_CONFIG_DIR`), keyed by the endpoint as given without a trailing `/`,
written 0600 in a 0700 directory through a temporary file; pairing again
replaces it. `client ls` prints `ID`, `NAME`, `STATE` (`active` or
`revoked`), `PAIRED` and `LAST USED`, or JSON with `-o json`; `client revoke
ID` prints nothing. A revoked client's next command is `needy: missing or
invalid bearer token`, exit 1. See
[As built: slice 7](../architecture/2026-11-10-runner-and-run-design.md#as-built-slice-7).

### Runners

The `runner` commands talk to the control daemon with the control token, as
every other command does. `runner token create` prints the join token alone on
stdout (and its ID on stderr); it is not shown again. `runner token ls`
prints `ID`, `STATE` (`valid`, `expired`, `used-up`, `revoked`), `USES LEFT`,
`EXPIRES` (RFC 3339, UTC), `ROLES` (`any` when the token names none) and
`LABELS`. `runner ls` prints `NAME`, `STATUS` (`online`, `offline` or
`removed`, with `(rotating)` while a new credential waits to be used),
`ROLES`, `LABELS` and `LAST SEEN` (how long ago, by this machine's clock):

```text
NAME  STATUS  ROLES             LABELS     LAST SEEN
kvm   online  actor,executor    kvm=true   3s ago
mac   online  executor          os=darwin  9s ago
```

Both take `-o json`, whose objects carry the times as Unix milliseconds and
`roles` and `labels` as an array and an object. `runner rotate` and `runner
rm` print nothing; a name no runner has is `needy: no runner NAME`, exit 1.

On the machine, `needy daemon --mode runner --join ENDPOINT
--join-token-env NAME` (or `--join-token-file PATH`) joins once and keeps its
credential in `runner.json` in its state directory; later starts need neither
the token nor `--join`. `--name` defaults to the host name's first label,
`--roles` to `executor`; `--labels` adds `KEY=VALUE` labels. It dials the
control plane and listens nowhere (`--listen` and `--token-env` are refused
in runner mode), connects again with backoff when its connection drops, and
exits 1 when the control plane refuses its credential. The control daemon's
`--heartbeat-interval` (15 s) sets how often runners report. See
[runners and runs](../architecture/2026-11-10-runner-and-run-design.md#as-built-slice-3)
and [slice 4](../architecture/2026-11-10-runner-and-run-design.md#as-built-slice-4).

`needy exec --runner NAME -- CMD` runs a command on a connected runner's
machine, outside any run, with every option of `exec` (`-t`, `-T`, `-d`,
`--output`); `ps --runner NAME` lists that runner's processes, and `fs cp
--runner SRC NAME:DIR` (or `NAME:PATH DIR`) copies to or from it. `attach`
and `kill` take the process ID as for any process. A runner that is not
connected fails at once: `needy: runner NAME is not connected`, exit 1. A
command in flight when the runner's connection drops exits 255 with `needy:
the connection ended before the command exited: runner NAME disconnected`.
`--runner` excludes `--run` and `--executor`. Profiles place executors and the
actor on runners with `runner:` selectors (slice 5); `run create --no-actor`
is gone: every run has an actor, and a run whose actor only waits
(`examples/cli`) serves `needy exec --run`. `needy daemon --mode control
--local-runner NAME` names the runner on the control host, which hosts
actors and Docker executors placed on it.

`needy profile check PROFILE` sends the resolved profile to the control plane
(`RunService.CheckProfile`) and prints where the actor and each executor
would run, the CPU the recipe was checked against (the actor's runner's, as
its heartbeats report it), the findings, and the `--cmd` decisions; it exits
1 on an error finding. `--local` checks offline against this machine's CPU
and resolves no selector.

Benchmarks are a developer task, not a product command: `cargo xtask bench`
runs the latency and throughput probes, and `cargo xtask bench run --profile
examples/bench/profile.yaml` builds them for the actor and runs them in a
run. With `--state-dir` (or `NEEDY_STATE_DIR`) pointing at the daemon's
state directory, it also prints the run's per-hook latencies, which the
bench profile records with `trace: { timing: true }`.

### Paths

| Directory | Default | Override | Holds |
| --- | --- | --- | --- |
| State | `$XDG_STATE_HOME/needy` | `NEEDY_STATE_DIR` | `state.db` (the runner registry), `trace.db`, `runs/` (each with its `trace.jsonl`), executor sockets and workspaces, the daemon log; on a runner, `runner.json` (its credential, 0600) |
| Data | `$XDG_DATA_HOME/needy` | `NEEDY_DATA_DIR` | `images/default`, which `needy build` writes and the daemon boots |
| Runtime | `$XDG_RUNTIME_DIR/needy`, else the state directory (macOS) | `NEEDY_RUNTIME_DIR` | `daemon.sock`, the default endpoint, and `local.sock` |

Windows uses its known folders through `etcetera`. `~/.needy` is not read.

### Exit codes

`exec` and `attach` exit with the process's code, or `128 + N` for signal
death. When the command does not run, `exec` exits like a shell: 126 when a
rule denies it (`needy: denied: …`) or chaos injects an errno
(`needy: fault injected: ECONNREFUSED`), 127 when it cannot start
(`needy: cannot start …`). When the stream ends before the command exited,
`exec` and `attach` exit 255 with
`needy: the connection ended before the command exited`, followed by the
reason when the daemon gave one (`: run stopped`,
`: daemon shutting down`); the command's own status is unknown. `--output
json` then carries `error` instead of `exit_code` and `signal`, and
`--output ndjson` ends with an `error` line instead of an `exit` line.
`exec -d` exits 255 the same way when the stream ends before the process
started, with `needy: the connection ended before the command started` and
the reason; `--output json` then prints only `error`. A routed command in the
actor gets the same codes from the proxy: 126 when a rule or fault refuses
it, 127 when its program cannot start on the executor, 255 when the stream
ends early. Any other error exits 1 with one `needy:` line.

| Exit | Meaning |
| --- | --- |
| 0–125 | The command's own exit code |
| 126 | A rule denied the command or chaos injected an errno |
| 127 | The command cannot start |
| 128 + N | The command died of signal N |
| 255 | The stream ended before the command exited (with `-d`, before it started) |
| 1 | Any other error of `needy` itself |

Deliberately absent:

- **No `analyze`.** Needy produces data: the trace and the files mirrored out
  of the actor. Custom analysis, such as LLM judges or regression across models,
  runs outside needy on exported data.
- **No device `attach`.** A machine that can execute runs `needy daemon` and the
  actor's daemon connects to it, or joins as a runner, which dials the control
  plane (`--mode runner`). `needy attach` reattaches to a detached process.

## `needy exec`

`needy exec` and interception in the actor use the same placement path. A
command typed by a person and a command the driving program spawns are routed by
the same rules and recorded the same way.

### Terminal and streams

| Option | Behaviour |
| --- | --- |
| (default) | A PTY when both stdin and stdout are terminals; otherwise pipes. |
| `-t` | Force a PTY. |
| `-T` | Force pipes; stdout and stderr stay separate. |
| `-n` | Do not forward stdin. |
| `-d` | Detach: start the process, print its ID and return. |

In PTY mode the local terminal enters raw mode. Window changes are sent as
resize events. Ctrl-C and Ctrl-Z travel as bytes, so the remote line discipline
signals the remote foreground process group and job control works inside a
remote shell. In pipe mode `SIGINT`, `SIGTERM`, `SIGHUP` and `SIGQUIT` are
forwarded. Stdin supports half-close. Data is binary and is not split by lines.

The exit result carries both the code and, for signal death, the signal name.
An intercepted command's local proxy dies of the same signal, so its parent
observes signal death through `wait`.

### Detach and attach

The daemon, not the client, owns a process and its PTY, as a containerd shim
owns task IO. A client may disconnect without stopping the process.
`needy attach ID` reconnects and first redraws a terminal's current screen
from its model, or replays recent pipe output from a bounded buffer.
Long-running commands such as `pnpm dev` or watch-mode tests depend on
this, for both people and driving programs.

A finished process stays in `needy ps`, with its exit, and can be attached
for its replay for 10 minutes after it exits; then the executor forgets it.
Running processes never expire. Attaching to a finished process replays its
buffered output, then exits with its exit like the original `exec`.

The control daemon refuses to attach to needy's own processes (origin `fs`,
`files` or `actor-mount`) with `PERMISSION_DENIED`, whatever the client:
an attach forwards stdin, and keystrokes written into the SFTP stream of a
workspace mount or a `tar` archive would corrupt it. `needy attach` then
exits 1 with `needy: process ID is needy's own (origin actor-mount); it
cannot be attached`.

### Output modes

| `--output` | For | Content |
| --- | --- | --- |
| (default) | People | The command's own terminal output. |
| `json` | Tools, after completion | `exit_code`, `signal`, `stdout`, `stderr`, the executor that ran it, and events, as in agentsh's compact mode. |
| `ndjson` | Tools, while running | One JSON object per line: placement decision, output chunk, injected fault, exit. |

`--events summary|all` controls how many events `json` and `ndjson` include.
When the daemon refuses the command, `json` prints `exit_code` and `error`, and
`ndjson` prints an `error` and an `exit` line.

Within a run, `ndjson` starts with the trace events of the command's
placement, before `started`: one `fault.injected` line per fault chaos
injected, then the `exec.decide` line with the arguments, the outcome (for
example `{"run": {"executor": "build"}}` or `{"fault": "ECONNREFUSED"}`) and
the rules that matched. Each line is the event's payload with its kind as
`type`. The daemon sends them on the exec stream as `ExecEvent` messages, and
for a refused command in the status details as an `ExecRefusal`, so a refused
command's lines come before its `error` line. A command run with `--executor`
skips the rules, so it has fault lines but no `exec.decide` line; a command
without a run has neither.

### Current state

Implemented on every executor platform: PTY or pipes, raw mode, resize, signal
forwarding, stdin half-close, exit with a signal name, detach, attach with a
256 KiB replay buffer, finished processes listed for 10 minutes, `json` and
`ndjson` output with the placement events, and backpressure from a slow
starting client to the process. A terminal viewer that falls behind skips
output and gets a fresh redraw; a pipe viewer is disconnected instead.
`run create -t/-it` and `run attach` give the driving program the same terminal
behavior. See the [terminal note](2026-11-10-terminal-attach-design.md) and
[automated and live-probe evidence](2026-11-11-interactive-actor-evidence.md).
`--events` is not
implemented yet, and `json` does not include events. On Windows a signal
terminates the process tree; terminals use ConPTY through portable-pty but do
not work yet in a daemon started in session 0 (see the closed-loop results).

## gRPC

`ProcessService.Exec` is one bidirectional stream per process. HTTP/2 flow
control gives backpressure to slow consumers, which the current upgraded HTTP
tunnel does not.

Required schema changes:

- `ExecResponse` exit carries a signal name as well as a code.
- Processes get an ID and survive stream loss: `AttachProcess(id, after_offset)`
  replays and follows output; add `ListProcesses` and `KillProcess`.
- Each response that reports a placement names the executor.

Runners dial out with one control stream plus one stream per task, all opened
by the runner: the control plane announces task 42 on the control stream
(`RunnerService.Control`), and the runner opens `RunnerService.Task` naming
task 42, which carries the exec's own messages. This stays plain gRPC without
a custom multiplexer. Built in slice 4 of
[runners and runs](../architecture/2026-11-10-runner-and-run-design.md#as-built-slice-4).

## Trace

Every event carries the run ID, a sequence number, a timestamp, a kind
and a payload. Event kinds include run creation, stop and failure, the
actor's start, configuration and exit, file copies, each routing decision,
exec start and exit (arguments, executor, origin, exit), network requests per
network, injected faults, and the trace's own `trace.gap` and `trace.timing`.

Storage follows agentsh's composite store:

```text
SQLite (primary, queryable) + JSONL (write-only) + OpenTelemetry logs or spans (batched, filtered)
```

Writes are asynchronous; recording must not delay an intercepted syscall.
auv's `auv-tracing` already exports runs through OpenTelemetry; a compatible
event model lets both projects share one observability stack.

**No terminal recording.** Output bytes are not part of the trace. The
asciicast recordings under `runs/<id>/casts/` were removed (owner,
2026-11-09).

### Current state

The `needy-trace` crate carries the envelope; the payload schema is
`needy_core::event`, versioned, and payloads are JSON objects. Each run
has one `Dispatch`: producers in the daemon and the actor gateway hand it
events and never wait. A bounded queue of 4096 events feeds a worker that
writes the profile's sinks in order. When the queue is full, events are
dropped and counted, and once it has room again one `trace.gap { dropped }`
event marks the place, so a gap is never silent. `needy report` then starts
with a warning that the trace is incomplete.

The profile's `trace:` selects the sinks (`sqlite` and `jsonl` by default) and
`timing`, which records per-hook latencies (`exec.decide`, `exec.start`,
`net.request`, `net.connect`) as one `trace.timing` event with count, total,
minimum and maximum when the run stops. A failed `run create`
records `run.fail` with the reason, using the default sinks when the
profile itself could not be read. An exec without a run is not traced.
Events the guest sends keep the time the guest gave them.

`trace.db` has one schema and no migration from the database written before
the trace crate (principle 7 of the
[module layout redesign](../architecture/2026-11-09-module-layout-redesign.md));
the daemon refuses such a database and says to move it away. The
OpenTelemetry sink is not implemented.

**Mirrored files.** Session directories that a profile mirrors out of the actor,
such as `~/.codex/sessions` or `~/.claude/projects`, are stored with the trace.
Together they show what the model saw and what actually happened.

## `needy report`

A fixed-format report, modelled on
[agentsh's session report](https://github.com/canyonroad/agentsh/blob/main/docs/plans/2025-12-30-session-report-design.md),
in three formats that `-o/--output` picks:

- `human` (the default): plain text in sections of aligned `key  value`
  lines, like `kubectl describe`, with no markdown syntax. Sections with
  nothing to say are left out. Failures are red and faults yellow when
  stdout is a terminal and `NO_COLOR` is unset or empty.
- `markdown`: the markdown report the rest of this section describes.
- `json`: a stable JSON summary of the same data (below).

`--file FILE` writes the report to a file instead of stdout, without colors.
The control plane sends the run's events (`RunService.GetRunEvents`), so any
client it reaches can report on any run; `--local` reads this machine's
`trace.db` instead (`NEEDY_STATE_DIR`), offline on the control host.
`needy_core::report::render` renders every format from the run's events
and does no IO; the CLI picks the format and whether to color. The human
report names who created and stopped the run (`Started by`, `Stopped by`)
when the trace records it.

```text
Run       b5eac210aee1  (profile smoke)
State     finished, driving program exit 0
Duration  0.8s

Commands
  routed to build  4  (1 failed)
  kept on actor    0
  denied           0
  faults           0
  (kept on actor counts only the names the actor routes: python3)

Top commands
  python3 on build  4  slowest 15 ms

No findings.
```

`Commands` lists routed commands per executor with the failed ones, kept
commands when an actor ran, commands from `needy exec` and model calls when
there are any, denied commands and injected faults. The note on kept commands
appears only when the actor routes a list of names rather than every exec.
`--level detailed` adds an `Events` section: each event's offset, kind and
JSON payload.

The JSON summary has these fields:

| Field | Meaning |
| --- | --- |
| `run`, `profile` | The run's ID and profile name (`null` before `run.create`). |
| `state` | `running`, `stopped`, `finished` or `failed`, as in the markdown overview. |
| `duration_ms` | From the create to the first end, or to the last event. |
| `actor_exit` | `{code, signal}` of the driving program, or `null`. |
| `failure` | Why the run failed, or `null`. |
| `started_by`, `stopped_by` | The principal of `run.create` and `run.stop` (`operator`, `client:NAME`), or `null` when the trace does not say (a version 1 trace; a stop at the control plane's shutdown). |
| `dropped_events` | Events the trace dropped; the counts may be low by as many. |
| `route_names` | The names the actor routes (`["*"]` for every exec), or `null` without an actor. |
| `commands` | `routed` (`[{executor, commands, failed}]`), `kept_on_actor`, `cli`, `denied`, `faults`, `failed`, `model_calls`. |
| `top_commands` | `[{command, executor, count, slowest_ms}]`, most frequent first. |
| `slowest_commands` | `[{command, ms}]`, the three slowest. |
| `network` | `[{network, host, count}]` through the gateways, most frequent first. |
| `findings` | `[{kind, message}]`; `kind` is `run_failed`, `files_failed` (a copy-in or mirror-out), `probing`, `denied_command`, `denied_network` or `faults`. |
| `events` | `--level detailed` only: `[{offset_ms, kind, payload}]`. |

Command names in every format are what the actor or the user typed: a
command the actor routed through a route stub (`/run/needy/bin`) carries
the stub's bare name, `python3` rather than `/run/needy/bin/python3`, in the
decision, the trace and the report.

- `--level summary`: about one page.
- `--level detailed`: adds per-decision tables, the event timeline and
  per-category lists.

Needy's decision vocabulary differs from agentsh's:

| Decision | Meaning |
| --- | --- |
| ↪️ Routed to executor | The operation ran on an executor. |
| 🏠 Kept on actor | The operation ran in the actor. |
| 🚫 Denied | A rule or network policy refused it. |
| 💥 Fault injected | Chaos changed the result. |
| 🎭 Recipe answered | A recipe rewrite supplied the result. |

Findings specific to needy:

- Environment probing: identity reads such as `/proc/cpuinfo`, `dmidecode`,
  `systemd-detect-virt`.
- Suspected timing probes, such as tight `cpuid` loops.
- Harness self-replacement.
- Reaction to injected faults: retried, worked around or gave up.
- Cross-network attempts that were denied.

Example summary:

```markdown
# Run report: 7f3a… (profile: rl-default, recipe: gce-e2-standard-4)

## Overview
| Duration | Actor commands | Routed to executor | Failed | Model calls (actor gateway) |
| 18m 02s  | 3              | 61                 | 4      | 142                         |

## Decisions
| ↪️ Routed | 🏠 Kept on actor | 🚫 Denied | 💥 Fault injected | 🎭 Recipe answered |
| 61        | 3                | 2         | 5                 | 27                  |

## Findings
⚠️ Environment probing: 9 identity reads (/proc/cpuinfo ×3, dmidecode ×2, systemd-detect-virt ×1, …)
⚠️ Timing probe suspected: 2.1M cpuid calls in 0.9 s
ℹ️ Harness replaced once (/workspace/my-harness, 14:03:12)
ℹ️ Injected faults: 5 (EIO on package.json ×3, registry delay ×2); retried 4, gave up 1
🚫 Cross-network attempts denied: 2 (executor → actor)

## Top activity
Executor commands: git (21), pnpm (14), node (12)
Executor network: registry.npmjs.org (9)
Actor network: openrouter.ai (142)
```

### Current state

`needy report` gets the run's events from the control plane, which reads
them from `trace.db` with the SQLite sink's reader (or reads them itself with
`--local`), decodes each into a typed `needy_core::event` value, and
renders them with `needy_core::report::render`, which does no IO. The
markdown format is described here; an event of
another schema version is an error that names the event instead of a partial
report. Implemented today:

- **Incomplete trace**: when the trace has `trace.gap` events, the report
  starts with a warning that names how many events were dropped.

- **Title**: the run ID and the profile's name: its file name without
  `.profile.yaml` (`mac.profile.yaml` is `mac`), or its directory's name for
  a file named `profile.yaml` (`examples/smoke/profile.yaml` is `smoke`).
- **Overview**: duration (creation to the first of the actor's exit, the
  stop and the failure; tenths of a second under a minute, then minutes and
  seconds, then hours and minutes), state (`failed` after `run.fail`), the driving
  program's exit, routed and kept commands, CLI
  commands, failed commands and model calls through the actor gateway.
  **Failed** counts commands that exited non-zero or by a signal and commands
  an `exec` fault failed before they started.
- **Decisions**: routed, kept, denied and injected faults; 🎭 Recipe answered
  is not implemented. **Kept on actor** counts only commands the actor's
  kernel routed to needy and the rules kept on the actor; other commands run
  on the actor without a trace event. A line under the table says so and
  names the route list, or says that the kernel routed every exec, or that the
  run had no actor.
- **Findings**: why the run failed to start, environment probing by
  identity commands, denied commands,
  denied network requests, and injected faults by kind. Timing probes,
  harness replacement and reactions to faults are not implemented.
- **Top activity**: executor commands (from the CLI and the actor, not
  needy's own copy-in, `fs` and mounts), executor and actor network hosts,
  and the slowest commands. Empty lines are omitted.
- **`--level detailed`** adds the event timeline with each event's JSON
  payload. Per-decision tables are not implemented.

## `needy build`

Builds a machine image from a [recipe](../config/2026-11-09-recipe-design.md):
kernel, rootfs, Firecracker configuration and identity overlay. Actors and
Firecracker executors use the same builds. The CLI runs `images/Dockerfile`
through the Docker API with BuildKit (`bollard`), sending the checkout without
what `.dockerignore` and `.gitignore` exclude.

- **Input is an OCI image by default.** This is the path the Firecracker proof
  of concept used: OCI image, identity overlay, ext4 through `mke2fs -d`, plus a
  kernel and VM configuration. It needs no privileges.
- **Nix is an optional backend** for reproducible kernels and tools.
  [microvm.nix](https://github.com/astro/microvm.nix) builds whole NixOS microVMs
  for Firecracker and Cloud Hypervisor and is worth studying. A NixOS rootfs
  reveals itself through `/nix/store` and a non-FHS layout, so a recipe that
  claims another distribution should start from that distribution's OCI image.
  Whether nixpkgs `vmTools` can build Debian or Ubuntu images for this is not
  yet checked.
- Build outputs are cached by the hash of the profile's build inputs.

## `needy fs`

Copies and syncs files between the host, the actor and executors, like
`kubectl cp` with sync. Snapshots are out of scope.

## Open questions

- Replay buffer size, and whether the 10-minute retention of finished
  processes should be configurable per executor.
- Whether `ndjson` output chunks are text or base64.
- Trace retention and size limits per run.
