# CLI, exec streams and reports

**Status**: pending; implemented as described unless the [closed-loop results](../architecture/2026-11-09-closed-loop-results-evidence.md#not-yet-verified) list it as missing. Terms follow the
[actor and executor separation](../architecture/2026-11-09-actor-executor-separation-design.md)
note. Profiles are described in
[Profiles and rules](../config/2026-11-09-profile-and-rules-design.md).

## Command surface

```text
needy daemon                                   # control plane; also runs on every executor machine
needy build    --recipe NAME                   # build a machine image
needy session  create|ls|stop [--group G]     # one actor plus its executors
needy exec     [--session ID] -- CMD …         # run a command through placement
needy ps | attach ID | kill ID                 # manage detached processes
needy fs       cp SRC EXECUTOR:DIR | EXECUTOR:PATH DIR   # tar over an exec stream
needy report   SESSION|latest --level summary|detailed [--output FILE]
needy profile  check PROFILE [--cmd "COMMAND"]…  # routing decisions and recipe findings
needy bench    spawn|pipe|fs|suite …            # hidden: latency and throughput probes
```

Deliberately absent:

- **No `analyze`.** Needy produces data: the trace and the files mirrored out
  of the actor. Custom analysis, such as LLM judges or regression across models,
  runs outside needy on exported data.
- **No device `attach`.** A machine that can execute runs `needy daemon` and the
  actor's daemon connects to it. Executors behind NAT may later get a dial-out
  option on `needy daemon`. `needy attach` reattaches to a detached process.

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
`needy attach ID` reconnects and first replays recent output from a bounded
buffer. Long-running commands such as `pnpm dev` or watch-mode tests depend on
this, for both people and driving programs.

### Output modes

| `--output` | For | Content |
| --- | --- | --- |
| (default) | People | The command's own terminal output. |
| `json` | Tools, after completion | `exit_code`, `signal`, `stdout`, `stderr`, the executor that ran it, and events, as in agentsh's compact mode. |
| `ndjson` | Tools, while running | One JSON object per line: placement decision, output chunk, injected fault, exit. |

`--events summary|all` controls how many events `json` and `ndjson` include.

### Current state

Implemented on every executor platform: PTY or pipes, raw mode, resize, signal
forwarding, stdin half-close, exit with a signal name, detach, attach with a
256 KiB replay buffer, `json` and `ndjson` output, and backpressure from a slow
client to the process. `--events` is not implemented yet. On Windows a signal
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

If executors later dial out, use one control stream plus one stream per task,
all opened by the executor: the daemon announces task 42 on the control stream,
and the executor opens `Exec(task_id=42)`. This stays plain gRPC without a
custom multiplexer.

## Trace

Every event is defined in `proto/` and carries the session ID, a sequence
number, a timestamp and the process identity. Event kinds include exec
(arguments, side, executor, PID, exit), file operations, network connections per
network, injected faults, recipe answers and each routing decision.

Storage follows agentsh's composite store:

```text
SQLite (primary, queryable) + JSONL (write-only) + OpenTelemetry logs or spans (batched, filtered)
```

Writes are asynchronous; recording must not delay an intercepted syscall.
auv's `auv-tracing` already exports runs through OpenTelemetry; a compatible
event model lets both projects share one observability stack.

**Terminal recording.** Every terminal session that passes through the control
daemon is recorded in asciicast v2 format under `sessions/<id>/casts/`, and the
`exec.start` event names the file.

**Mirrored files.** Session directories that a profile mirrors out of the actor,
such as `~/.codex/sessions` or `~/.claude/projects`, are stored with the trace.
Together they show what the model saw and what actually happened.

## `needy report`

A fixed-format markdown report for people, modelled on
[agentsh's session report](https://github.com/canyonroad/agentsh/blob/main/docs/plans/2025-12-30-session-report-design.md).

- `--level summary`: about one page.
- `--level detailed`: adds per-decision tables, the event timeline, per-category
  lists and links to terminal recordings.

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
# Session report: 7f3a… (profile: rl-default, recipe: gce-e2-standard-4)

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

## `needy build`

Builds a machine image from a [recipe](../config/2026-11-09-recipe-design.md):
kernel, rootfs, Firecracker configuration and identity overlay. Actors and
Firecracker executors use the same builds.

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

- Replay buffer size and retention for detached processes.
- Whether `ndjson` output chunks are text or base64.
- Trace retention and size limits per session.
