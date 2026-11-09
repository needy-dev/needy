# Closed-loop results

**Status**: evidence, 2026-11-09. Results of implementing the
[closed-loop milestone](2026-11-09-closed-loop-milestone-plan.md) against the
[CLI](../cli/2026-11-09-cli-exec-and-report-design.md),
[profile](../config/2026-11-09-profile-and-rules-design.md) and
[recipe](../config/2026-11-09-recipe-design.md) notes.

Hosts:

- **Linux host**: Intel Core i5-13600KF, 20 threads, 94 GB, RTX 4080 SUPER,
  Debian 13, kernel 7.1, KVM, Docker; actor host and Docker executors.
- **Mac**: Apple M5 Max, macOS 27.2, Xcode 27.2; native and remote executor.
- **Windows host**: Windows 11 (10.0.26100), OpenSSH, no Docker.

The Mac reaches the Linux host over a LAN with a 4.8 ms average ping. The Mac
executor is exposed to the Linux host with `ssh -R`, the same model as a Cloudflare
Tunnel: the executor listens locally and the tunnel carries the connection.

## What runs end to end

| Loop | Result |
| --- | --- |
| `needy build`: one multi-stage Dockerfile builds the guest kernel (`binfmt_needy`, fake coretemp), patched Firecracker, a static needy, the actor rootfs with opencode, and the executor image | Pass |
| Actor boots from a recipe: hostname, CPU brand, DMI and PCI identity | Pass: `instance-…`, `Intel(R) Xeon(R) CPU @ 2.20GHz`, `Google Compute Engine` |
| Exec routing in the guest kernel; proxy asks the daemon; command runs on a Docker executor; exit code returns | Pass, including exit code 7 |
| Workspace mounted in the actor with sshfs in passive mode over a needy exec stream to `sftp-server` | Pass in both directions, owner `agent` on both sides |
| **opencode in the actor** with Qwen3 14B through the actor gateway | Pass: Write tool writes over sshfs, the bash tool is routed to the executor, and the report shows the routed command and model calls |
| **Mac executor from a Linux actor**: `sw_vers`, `xcrun`, `xcodebuild`, `simctl list`, `swiftc` compile and run, `xcrun swift build` and run, cwd mapped to the Mac path | Pass |
| `needy exec -t -- ssh` from the host into an sshd container through a Docker executor | Pass, interactive |
| `needy exec` PTY, Ctrl-C, stdin, json and ndjson output, detach, attach with replay, kill | Pass on the Mac executor |
| Bearer tokens on remote executors | Pass: a request without the token is rejected |
| `max_procs` with `on_full: reject`; a detached process holds its slot until it exits | Pass |
| Copy-in with rclone filters (`- /secret.txt`), mirror-out to `out/{session}/` | Pass on a native executor and in an actor ([`examples/chaos`](../../../../examples/chaos/profile.yaml)) |
| Chaos: `exec` errno (`curl **` → `ECONNREFUSED`), `exec` delay (+500 ms) | Pass, on the CLI path and for a routed command in an actor; both exit 126 with `needy: fault injected: ECONNREFUSED` |
| Chaos: SFTP `open` of `**/*.json` on the actor's workspace mount | Pass: `cat broken.json` gets `Input/output error`, `ok.txt` reads normally |
| Chaos: scheduled hwmon fault | Pass: `sensors` shows `+101.0°C ALARM (CRIT)` two seconds after start |
| Terminal recordings in asciicast v2 under `sessions/<id>/casts/` | Removed by design in the trace slice (owner, 2026-11-09): output bytes are not traced |
| `needy report` from SQLite traces | Pass |
| Windows executor: `needy.exe` cross-compiled with mingw, resident on Windows 11 | Pass with pipes; terminals do not work yet (see below) |

Found on the way:

- **Identity drift between sides.** In the first opencode run,
  `platform.node()` printed the container hostname while the actor said
  `instance-…`. Docker executors now take the recipe's hostname.
- **The driving program must never be routed.** Shells are on the kernel route
  list, and a script whose commands all go to one executor would move the whole
  driving program there. The kernel now never routes execs made by programs in
  `/.needy` (init and the proxy).
- **Recipe consistency check works.** The built-in GCE recipe (Cascade Lake,
  AVX-512) is rejected on the Raptor Lake host; the example uses a Broadwell
  Xeon signature and hides newer features instead.
- **Small models need narrow prompts.** Qwen3 14B invented paths
  (`/home/user`, `/current/directory`) and delegated to sub-agents; the example
  gives an absolute path and disables opencode's task tool.
- **The user's swiftly toolchain crashes `swift build`** on the Mac by itself;
  Xcode's toolchain through `xcrun` works.

## Re-verified after the CLI slice

The [module layout redesign](2026-11-09-module-layout-redesign.md)'s slice 1
(the CLI's `commands/`, XDG paths, `tracing` logs, `comfy-table` output,
`needy build` through `bollard`, `bench` moved to `cargo xtask`) was rerun on
2026-11-09, with every daemon's state in a scratch directory through
`NEEDY_STATE_DIR`, `NEEDY_RUNTIME_DIR` and `NEEDY_DATA_DIR`.

| Loop | Host | Result |
| --- | --- | --- |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` through the Docker API with BuildKit | Linux | Pass: kernel, rootfs, Firecracker and needy exported with their file modes; `needy-executor:latest` built |
| `examples/smoke` in an actor; `needy session logs` prints its console | Linux | Pass: GCE identity, routed `python3`, workspace both ways, `routed exit=7` |
| `examples/chaos` in an actor | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/` |
| `needy exec` through a Docker executor: exit 7, `-t` (exit 3 on a PTY), `--output json`, a missing command exits 127, detach, `ps`, `attach` with replay, `kill`, `fs cp` | Linux | Pass |
| `examples/local` without an actor: copy-in filter, exec fault (126), delay fault (+500 ms), json and ndjson, detach, `ps`, `attach`, `kill`, `fs cp`, stop with mirror-out, `report` | Mac | Pass |
| `session ls` lists a failed create and a stopped session with their states | Mac, Linux | Pass |
| `cargo xtask bench run --profile examples/bench/profile.yaml`: probes built for musl and copied into the actor | Linux | Pass: actor-routed Docker `true` p50 1.44 ms, p99 2.10 ms; 256 MiB pipe 689 MiB/s; in line with the numbers below |
| Windows executor: `needy.exe` from `images/Dockerfile`'s `windows` stage, started through WMI, driven by `needy.exe` on the same host over TCP with a bearer token | Windows | Pass: `cmd /c exit 7` returns 7, stdin to PowerShell, `--output json`, a missing program exits 127, detach, `ps`, `kill`, a wrong token is rejected |

Not rerun in this pass: opencode with a model, the Mac executor from an actor,
and Windows `fs cp`. The Windows check ran the client on the Windows host
because a local port forward was not available on the Mac.

## Re-verified after the config slice

Slice 2 of the module layout redesign (the `needy-config` crate: schemas,
`extends` with one merge model, unknown keys rejected, `validate`, explicit
`captures:`, built-in recipes moved out of `core`) was rerun on 2026-11-09,
with every daemon's state, image and output in a scratch directory.

| Loop | Host | Result |
| --- | --- | --- |
| `needy profile check` on every profile under `examples/` | Mac, Linux | Pass: exit 0, no error findings |
| `needy profile check` on a profile whose `actor.recipe` is `builtin:gce-ubuntu-22.04`, and `builtin:dell-optiplex-ubuntu-24.04` | Mac, Linux | Pass: both load and compile. On the Linux host the GCE built-in (Cascade Lake) reports errors for the missing AVX-512 features and exits 1, as in the first run; the Dell built-in has no findings |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` in an actor | Linux | Pass: GCE identity, routed `python3`, workspace both ways, `routed exit=7`; `report` |
| `examples/chaos` in an actor | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; `report` counts 3 injected faults |
| `examples/local` without an actor: copy-in filter, exec fault (126), delay fault (+500 ms), `--output json`, stop with mirror-out, `report` | Mac | Pass, once the native workspace directory exists (copy-in into a missing directory is fix slice 8g) |

## Re-verified after the core slice

Slice 3 of the module layout redesign (`core` split into `rules`, `recipe`
and `chaos` modules, one `pattern` matcher, domain types, the `exec.decide`
interceptor chain, typed `core::event` payloads in the trace, and the report
rendered from them) was rerun on 2026-11-09, with every daemon's state, image
and output in a scratch directory. The trace database gains a payload schema
version; rows written before it are reported as unreadable.

| Loop | Host | Result |
| --- | --- | --- |
| `examples/local` without an actor: copy-in filter, `exit 7` returned, exec fault (126, `needy: fault injected: ECONNREFUSED`) with and without `--executor`, delay fault (+500 ms), `--output json`, stop with mirror-out, `report` and `report --level detailed` | Mac | Pass: the report names profile `local`, counts 3 failed (one exit 7, two faulted `curl`), says no actor ran, and omits the empty network lines |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` in an actor; `report` | Linux | Pass: GCE identity from the typed VM identity, routed `python3`, workspace both ways, `routed exit=7`; the report names profile `smoke`, counts 4 routed and 1 failed, and names the kernel's route list under Kept on actor |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)` (its event now sent by the guest as a typed payload), mirror-out to `out/{session}/`; the report counts 3 faults by kind (exec, file.open, hwmon) and the faulted `curl` as failed |

## Re-verified after the trace slice

Slice 4 of the module layout redesign (the `needy-trace` crate with a bounded
per-session `Dispatch`, `trace.gap`, SQLite and JSONL sinks and the reader,
per-hook timing, the profile's `trace:`, placement events in `ndjson`,
`session.fail`, and no terminal recording) was rerun on 2026-11-09 with debug
builds and every daemon's state, image and output in a scratch directory.
`trace.db` has a new schema; a database from before the slice is refused, not
migrated.

| Loop | Host | Result |
| --- | --- | --- |
| `examples/local` without an actor: `exec --output ndjson` for a command that exits 7, a faulted `curl` and a delayed `sleep`, `exec -t`, stop with mirror-out, `report` and `report --level detailed` | Mac | Pass: `ndjson` starts with `fault.injected` and `exec.decide` lines (outcome `run` on `here`, or `fault` `ECONNREFUSED` before the `error` and `exit` 126 lines); the report counts 4 routed, 4 failed and 2 faults; `session.stop` is `{}`; the session directory holds `profile.yaml` and `trace.jsonl`, no `casts/` |
| A profile with `trace: { sinks: [sqlite], timing: true }`; a failed create with a missing recipe | Mac | Pass: one `trace.timing` event after `session.stop` with `exec.decide` and `exec.start`; no `trace.jsonl`; the failed session's report shows state `failed` and the reason under Findings |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` in an actor; `report` | Linux | Pass: GCE identity, routed `python3`, workspace both ways, `routed exit=7`; the report counts 4 routed and 1 failed; `trace.jsonl` has the same 18 events |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind |
| `exec -t` in a session with a Docker executor and without a session | Linux | Pass: a terminal on both (`/dev/pts/N`, exit codes 4 and 5 returned); nothing recorded besides the events; the exec without a session is not in the session's trace (it was before the fix in this slice) |
| `cargo xtask bench run --profile examples/bench/profile.yaml` with `NEEDY_STATE_DIR` | Linux | Pass: the probes' lines, then one `timing` line: `exec.decide` 601 samples (31–113 µs), `exec.start` 602 (0.5–1.2 ms), `net.request` 100 (0.15–0.6 ms); no container or VM left |

## Re-verified after the core-executor slice

Slice 5 of the module layout redesign (`core-executor` split into `process`,
`service`, `provider` and `relay`; Docker executors created through the
Docker API with `bollard`; the terminal relay on `crossterm`; finished
processes expire from `ps` after 10 minutes) was rerun on 2026-11-09 with
debug builds and every daemon's state, image and output in a scratch
directory.

| Loop | Host | Result |
| --- | --- | --- |
| Native executor: exit 7 with stdout and stderr apart, stdin, `SIGTERM` death exits 143, a missing program exits 127, `--output json`, `-t` under `script` (a terminal, its window size, the local terminal restored), detach, `ps`, `attach` with replay, `kill`, `fs cp` both ways | Mac | Pass |
| Finished processes in `ps` | Mac | Pass: listed with their exit right after they finish; gone from `ps` once 10 minutes have passed, while a running process stays |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| The Docker provider's create and destroy round trip (`cargo test`): an executor container from `needy-executor:latest` answers, has the recipe's hostname, and is gone after destroy | Linux | Pass |
| `examples/cli` through a Docker executor: exit 7, stdin, `SIGTERM` (143), 127, `--output json`, `-t` (`/dev/pts/0`, exit 3), detach, `ps`, `attach` with replay, `kill`, `fs cp` both ways; `session stop` leaves no container | Linux | Pass |
| A profile with an untranslated `docker.args` option (`--gpus all`) | Linux | Pass: `session create` fails with `executor build: --gpus is not a supported docker run option` |
| `examples/smoke` in an actor; `report` | Linux | Pass: GCE identity, the routed `python3` prints the actor's hostname, workspace both ways, `routed exit=7`; the report counts 4 routed and 1 failed; no container left after stop |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind; no container left |
| Windows executor: `needy.exe` from `images/Dockerfile`'s `windows` stage, started through WMI, driven by `needy.exe` on the same host over TCP with a bearer token | Windows | Pass: `cmd /c` exit 7, stdin to PowerShell, `--output json`, a missing program exits 127, detach, `ps`, `kill`, `fs cp` both ways, a wrong or missing token is rejected. `attach` was tried only after the process had expired and correctly answered `no such process` |

Found on the way: a daemon started in the background of a non-interactive
shell inherits `SIGINT` as ignored and passes that on to every process it
starts, so `needy kill -s INT` has no effect there. This predates the slice.

## Re-verified after the core-actor slice

Slice 6 of the module layout redesign (`core-actor` split into `guest` and
`host`; the guest on `nix` with `tracing` to the console; `host::detect`;
`host::vm` building Firecracker's configuration and owning its process; the
gateway on the `net.*` interceptors with its connections and tunnels owned
under the actor's cancellation token) was rerun on 2026-11-09 with debug
builds and every daemon's state, image and output in a scratch directory. The
actor image now carries `systemd` for `systemd-detect-virt`; needy's init is
still PID 1.

| Loop | Host | Result |
| --- | --- | --- |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` in an actor; `session logs`; `report` | Linux | Pass: GCE identity (`instance-…`, `Intel(R) Xeon(R) CPU @ 2.20GHz`, `Google Compute Engine`), `systemd-detect-virt` prints `google` as on a Compute Engine VM, the routed `python3` prints the actor's hostname, workspace both ways, `routed exit=7`; init's log line `the driving program exited: exit status: 0` on the console; the report counts 4 routed and 1 failed; no container or VM left |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind |
| `examples/opencode` with Qwen3 14B on the host's Ollama through the actor gateway | Linux | Pass on the second run: the Write tool writes over sshfs, the bash tool is routed to the Docker executor and prints the actor's hostname; the report shows 3 model calls and 1 routed command. In the first run the model sent malformed bash tool arguments, so nothing was routed; the gateway carried its 3 model calls |
| A gateway check profile: an `upstream` host with an injected header, a `net.connect` fault on its second request, a denied host over plain HTTP and over `CONNECT`, an allowed host over `CONNECT` | Linux | Pass: the upstream receives the header's value from the daemon's environment; the second request gets `502 needy chaos: ECONNRESET`; the denied hosts get 403 (`curl` exits 56 for the tunnel); `models.dev` answers 200 through the tunnel. The trace has `net.request`, `fault.injected`, `net.deny` (with path, and without for the tunnel) and `net.connect`; the report lists both denials and the fault |

Found on the way: an actor whose profile has no workspace mount and no
`actor.cwd` cannot start its driving program, because the default working
directory `~/workspace` does not exist. This predates the slice; the console
now names the failure.

## Re-verified after the proto slice

Slice 7a of the module layout redesign (`ExecStart.origin` and session state
as proto enums mapped in `needy_api_server::protocol::domain`; `Decide`
carries the process ancestry, which the proxy reads from `/proc`; guest
events carry their payload and schema version in `TraceEvent`; the session
request carries the profile's name) was rerun on 2026-11-10 with debug
builds and every daemon's state, image and output in a scratch directory.

| Loop | Host | Result |
| --- | --- | --- |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` in an actor; `session ls`; `report` | Linux | Pass: GCE identity, `systemd-detect-virt` prints `google`, the routed `python3` prints the actor's hostname, workspace both ways over the mount, `routed exit=7`; `session ls` shows `finished`; the report names profile `smoke` and counts 4 routed and 1 failed; no VM left |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out; the report names profile `chaos` and counts 3 faults by kind, the hwmon one pushed by the guest |
| A scratch profile that denies `python3` under `bash **` and routes it to a Docker executor otherwise | Linux | Pass: `python3` started by the driving `sh` runs on the executor; started by `bash -c` it prints `needy: denied: under bash` and exits 126. The report names profile `under` from `under.profile.yaml` |
| `examples/local` without an actor: `exec`, an injected `curl` fault, `session ls` before and after `session stop`, `report` | Mac | Pass: `running`, then `stopped` in the table and in `--output json`; exit 126 for `curl`; the report names profile `local` |

The Docker executors of these sessions were left running after the actor
exited, as before (fix slice 8h), and were removed by hand.

## Re-verified after the API crates slice

Slice 7 of the module layout redesign (`needy-api-client` with `ready`,
`exec`, `files` and an error `Kind`; `needy-api-server` with the domain
traits, `Caller`, `Server::bind`, `termination` and one status table; the
daemon implementing the traits instead of three gRPC services; `fs cp`,
copy-in and mirror-out over `api_client::files`) was rerun on 2026-11-10 with
debug builds and every daemon's state, image and output in a scratch
directory.

| Loop | Host | Result |
| --- | --- | --- |
| Native executor without a session: exit 7 with stdout and stderr apart, stdin, `SIGTERM` death exits 143, a missing program exits 127, `--output json` and `ndjson`, `-t` under `script`, detach, `ps`, `attach` (137 after `kill -s SIGKILL`, and again with replay after the exit), `kill` of an unknown process, `fs cp` of a directory and a file both ways, a failed `tar` and an unknown executor named in the error | Mac | Pass |
| `examples/local` without an actor: copy-in filter, `exit 7`, `curl` fault (126) with and without `--executor`, delay fault (+500 ms), `ndjson` with `fault.injected` and `exec.decide` before `error` and `exit` 126, `--output json`, `fs cp --session`, stop with mirror-out, `session ls`, `report` | Mac | Pass: the report names profile `local` and counts 4 faults |
| An executor daemon with `--token-env`: no token, a wrong token, the right token; `SIGTERM` | Mac | Pass: both refused with `missing or invalid bearer token`, the right one runs; the daemon exits on `SIGTERM` (it now handles SIGINT, SIGTERM and SIGHUP) |
| A socket path over `sun_path`: `daemon --mode executor --listen`, and a control daemon whose runtime directory is too long | Mac | Pass: both exit 1 at once with `cannot listen on unix:<path>: path must be shorter than SUN_LEN`; the control daemon used to wait 10 s for its local executor |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/cli` through a Docker executor: exit 7, stdin, `SIGTERM` (143), 127, `--output json`, `-t` (`/dev/pts/0`, exit 3), detach, `ps`, `attach` (137 after `kill`), `fs cp` of a directory both ways; `session stop` leaves no container | Linux | Pass |
| `examples/smoke` in an actor; `session logs`; `report` | Linux | Pass: GCE identity, `systemd-detect-virt` prints `google`, the routed `python3` prints the actor's hostname, workspace both ways, `routed exit=7`; the report counts 4 routed and 1 failed; no VM left |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind |
| Windows executor: `needy.exe` from `images/Dockerfile`'s `windows` stage, started through WMI with a token, driven by `needy.exe` on the same host over TCP | Windows | Pass: no token and a wrong token are refused; `cmd /c` exit 7 with stdout and stderr apart, stdin to PowerShell, `--output json`, a missing program exits 127, detach, `ps`, `attach` live and with replay after `kill` (exit 1, as `taskkill` ends it), `fs cp` of a directory and a file both ways, a failed `tar` names the executor |

The Docker executors of the actor sessions were left running after the actor
exited, as before (fix slice 8h), and were removed by hand.

## Re-verified after the daemon slice

Slice 8 of the module layout redesign (`daemon` split into `session` with
its state transitions, `pool`, `routing`, `relay` with stream filters,
`files`, `actor` and `guest`; every long-lived task in the daemon's or a
session's `TaskTracker`; `Daemon::shutdown`; session lookups by exact ID,
unique prefix or name; an ambiguous sessionless executor name refused) was
rerun on 2026-11-10 with debug builds and every daemon's state, image and
output in a scratch directory.

| Loop | Host | Result |
| --- | --- | --- |
| Native executor without a session: exit 7 with stdout and stderr apart, stdin, `SIGTERM` death exits 143, a missing program exits 127, `--output json` and `ndjson`, `-t` under `script`, detach, `ps`, `attach` (137 after `kill -s SIGKILL`, and again with replay), `kill` of an unknown process, `fs cp` of a directory and a file both ways, a failed `tar` named, an unknown executor | Mac | Pass |
| `examples/local` without an actor, its workspace created by hand (8g): copy-in filter, `exit 7`, `curl` fault (126) with and without `--executor`, delay fault (+500 ms), `ndjson` with `fault.injected` and `exec.decide`, `--output json`, `fs cp --session`, `ps --session`, stop with mirror-out, `session ls`, `session logs` (no actor console), `report` | Mac | Pass: the report names profile `local` and counts 4 routed and 5 faults |
| `profile check` with `--cmd`; a failed create (missing recipe) listed as `failed` with its reason in the report | Mac | Pass |
| `SIGTERM` to a control daemon with a running session | Mac, Linux | Pass: the daemon stops the session (mirror-out runs, `session.stop` is recorded, the Docker executor is removed) and exits within half a second |
| An executor daemon with `--token-env`: no token, a wrong token, the right token; `SIGTERM` with a detached process | Mac | Pass: both refused with `missing or invalid bearer token`; on `SIGTERM` the executor kills its processes and exits |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/cli` through a Docker executor: exit 7, stdin, `SIGTERM` (143), 127, `--output json`, `-t` (`/dev/pts/0`, exit 3), detach, `ps`, `attach` (137 after `kill`), `fs cp` of a directory both ways; `session stop` leaves no container | Linux | Pass |
| `max_procs: 1` with `on_full: reject` on a Docker executor; `needy exec --executor build` without a session | Linux | Pass: `executor is full` while a detached process holds the slot, accepted after it exits. With one session the name runs on its instance; with two it is refused as ambiguous, naming both keys |
| `examples/smoke` in an actor; `session logs`; `report` | Linux | Pass: GCE identity, `systemd-detect-virt` prints `google`, the routed `python3` prints the actor's hostname, workspace both ways, `routed exit=7`; the report counts 4 routed and 1 failed; no container or VM left after stop |
| `examples/chaos` in an actor; `report` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind |
| A scratch profile that denies `python3` under `bash **` and routes it to a Docker executor otherwise | Linux | Pass: under `sh` it runs on the executor; under `bash -c` it prints `needy: denied: under bash` and exits 126 |
| `examples/opencode` with Qwen3 14B on the host's Ollama through the actor gateway | Linux | Pass: the Write tool writes over sshfs, the bash tool is routed to the Docker executor and prints the actor's hostname; the report shows 3 model calls and 1 routed command |
| `cargo xtask bench run --profile examples/bench/profile.yaml` | Linux | Pass: actor-routed Docker `true` p50 2.95 ms, p99 3.98 ms with a debug daemon on a loaded host; the previous commit's debug daemon, run alternately with this one twice on the same image, measures p50 2.96–3.01 ms against 2.93–2.98 ms. 256 MiB pipe 709 MiB/s; timing `exec.decide` 32–107 µs, `exec.start` 0.5–1.4 ms, as after the trace slice; no container or VM left |
| Mac executor from a Linux actor through an SSH reverse tunnel, with a bearer token: `examples/macos` | Mac, Linux | Pass: `sw_vers`, `xcodebuild -version`, `simctl list`, `swiftc` compile and run, `xcrun swift build` and run, cwd mapped to the Mac path, the Mac workspace mounted in the actor; the same executor without the token is refused with `missing or invalid bearer token` |
| Windows executor: `needy.exe` from `images/Dockerfile`'s `windows` stage, started through WMI with a token, driven by `needy.exe` on the same host over TCP | Windows | Pass: no token and a wrong token are refused; `cmd /c` exit 7 with stdout and stderr apart, stdin, `--output json`, a missing program exits 127, detach, `ps`, `attach` live and with replay after `kill` (exit 1, as `taskkill` ends it), `fs cp` of a directory and a file both ways, a failed `tar` names the executor |

Changed on purpose in this slice:

- A session is listed as `creating` while it is created, and `stopping`
  while it stops.
- The first end state stays: stopping a session whose actor finished keeps it
  `finished` in `session ls`, where it used to become `stopped`. The report
  still says `stopped`, since it takes the last of `actor.exit` and
  `session.stop`.
- `session stop` ends the session's open exec streams; a detached process on
  a native executor keeps running, as before.
- `session get`, `session stop`, `session logs` and `Decide` no longer take an
  empty ID as a prefix of every session; a prefix or name that fits several
  sessions is refused as ambiguous.

Not rerun in this pass: `needy exec -t -- ssh` into an sshd container. The
VMs of these sessions had exited before each stop; a VM still running at stop
keeps running until fix slice 8b.

## Latency and throughput

Release builds. `p50 / p99` in milliseconds unless stated.

### Exec

| Path | p50 | p99 |
| --- | --- | --- |
| Mac, local `true` | 1.51 | 2.27 |
| Mac, `needy exec` CLI → executor daemon → `true` (includes CLI start-up) | 9.50 | 10.17 |
| Actor, local exec (`/usr/bin/test`) | 0.30 | 0.53 |
| Actor, routed to the proxy and kept local (`sh -c true`) | 1.61 | 2.35 |
| **Actor, routed to a Docker executor (`true`)** | **1.45** | **1.97** |
| Actor, routed to the Mac through the tunnel (`true`) | 12.3 | 83.6 |
| Linux host CLI → Mac executor through the tunnel | 18.4 | 90.2 |

### Streams

| Path | Throughput |
| --- | --- |
| Mac, `needy exec` of `head -c 1G /dev/zero` | 333 MiB/s |
| Actor, routed to Docker, 256 MiB | 809 MiB/s |
| Actor, routed to the Mac through the tunnel, 64 MiB | 32 MiB/s |

### Files in the actor

| Directory | Write | Read | Create | Stat | Read small | readdir (n) |
| --- | --- | --- | --- | --- | --- | --- |
| tmpfs | 1699 MiB/s | 8.7 GiB/s | 326k/s | 1.13M/s | 530k/s | 0.16 ms (1000) |
| sshfs → Docker executor | 575 MiB/s | 11 GiB/s (page cache) | 2.1k/s | 9.9k/s | 4.5k/s | 4.3 ms (1000) |
| sshfs → Mac through the tunnel | 9.4 MiB/s | 14.3 MiB/s | 34/s | 133/s | 64/s | 27 ms (300) |

Every sshfs metadata operation is at least one round trip. Over the tunnel the
round trip dominates, as the separation note predicted for cross-site links.

### Terminal echo

Keystroke to echo, 100 samples, driven through a pseudo-terminal:

| Path | p50 | p99 |
| --- | --- | --- |
| Local PTY | 0.005 | 0.015 |
| `docker exec -it` + `cat` | 0.056 | 0.129 |
| **`needy exec -t` + `cat`** | **0.098** | 0.238 |
| `docker exec -it` + `ssh` into an sshd container | 19.3 | 20.2 |
| **`needy exec -t` + `ssh` into an sshd container** | **21.1** | 22.2 |

### Gateway and actor

| Measurement | Result |
| --- | --- |
| `curl` in the actor through the gateway to Ollama | p50 2.86 ms, p99 14.0 ms |
| Firecracker VM alone, 1 vCPU / 256 MiB | ready in 194 ms, 68 MiB RSS |
| 200 VMs at once | ready in 4.1 s, 58 MiB each |
| Snapshot restore | 16 ms (snapshots are out of scope) |

## Windows executor

`needy.exe daemon --mode executor` runs on Windows 11 (10.0.26100), started
through WMI so it outlives the SSH session, with a bearer token, reached over an
SSH tunnel.

| Works | Detail |
| --- | --- |
| `cmd`, PowerShell 5.1, exit codes, stdin, `json` output | `cmd /c "exit 7"` returns 7 |
| Detach, `ps`, `kill` | `kill` ends the process tree with `taskkill /T /F` |
| `needy fs cp` both ways | `tar.exe` ships with Windows |
| Tools on PATH | git and scoop, `tar`, winget, python, node through proto, `wsl` |
| Bearer token | Rejected without it |

Not working yet:

- **Terminals (`-t`).** portable-pty starts the child under ConPTY, but in the
  executor's session 0 (started by WMI from SSH) the child produces no output
  and its exit is never observed. Pipes work. A daemon started in the user's
  interactive session should be tried next.
- **Signals** other than termination have no Windows equivalent.
- **File routing into an actor.** Windows ships `sftp-server.exe` in
  `C:\Windows\System32\OpenSSH`, but a Windows workspace mount was not tried.

| Measurement | Result |
| --- | --- |
| Mac → Windows `cmd /c exit 0` through the tunnel | p50 36 ms, p99 129 ms |
| Mac → Windows 64 MiB from PowerShell | 35 MiB/s |
| LAN ping Mac → Windows | 8.0 ms average |

## Not yet verified

- Windows terminals, and a Windows workspace mounted into an actor.
- Executor runtimes `containerd`, `firecracker` and `k8s`; pool settings
  `max_instances`, `idle_timeout` and `on_full: new`; `continuous` mirror-out.
- Network policy for the executor network (only the actor gateway is enforced).
