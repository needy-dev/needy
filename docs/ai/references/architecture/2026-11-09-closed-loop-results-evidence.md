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

## Re-verified after the fix slices

Fix slices 8b–8i of the module layout redesign (the VM stops with its
session; the guest listener runs under the session and the actor's sockets
are removed; a failed create releases what it acquired; the pool lock is not
held while an instance is created; an `under` rule test; a push creates its
directory; an actor that exits releases its executors and sockets; short
Docker executor socket paths and a clear error when one is too long) were
rerun on 2026-11-10 with debug builds and every daemon's state, image and
output in a scratch directory. Each fix has a regression test in `make test`.

| Loop | Host | Result |
| --- | --- | --- |
| `examples/local` without an actor, its native workspace a missing nested directory: copy-in, `exit 7`, `curl` fault (126), `fs cp --session` and `fs cp` without a session into missing directories, stop with mirror-out, `session ls`, `report` | Mac | Pass: copy-in creates the workspace (no manual `mkdir`); both copies land; the report names profile `local` |
| A control daemon whose runtime directory makes `local.sock` longer than `sun_path` | Mac | Pass: exits 1 at once with `cannot listen on unix:<path>/local.sock: path must be shorter than SUN_LEN` |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor`; `images/Dockerfile`'s `windows` stage | Linux | Pass |
| `examples/smoke` with `/bin/sleep 300` as the driving program, then `session stop` after 8 s | Linux | Pass: the stop returns in 0.27 s; no Firecracker process, no container and no socket left (`v.sock`, `v.sock_7000`, `v.sock_7001` and the executor's socket, 85–93 bytes long under the scratch state directory, were there before). The Docker executor's socket directory is `executors/<12 hex digits>` |
| `examples/smoke` finishing on its own | Linux | Pass: GCE identity, routed `python3`, workspace both ways, `routed exit=7`; once the VM exits the container and sockets are gone without a stop and `session ls` says `finished`; a later `session stop` keeps it `finished` |
| A profile whose second executor (a remote on a missing socket) cannot be created after a Docker one was | Linux | Pass: the create fails naming `executor b`; no container left; `session ls` says `failed` and the report shows the reason from `session.fail` |
| Two sessions in one group whose Docker executor key is `{group}`, created concurrently | Linux | Pass: one container, both sessions run on it (same `hostname`); it is removed after the second stop |
| A profile that routes `python3` to a Docker executor and denies it `under: "bash **"` | Linux | Pass: under `sh` it runs on the executor; under `bash -c` it prints `needy: denied: under bash` and exits 126 |
| `examples/chaos` | Linux | Pass: copy-in, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out to `out/{session}/`; the report counts 3 faults by kind; no container or socket left |
| Windows executor: `needy.exe` over TCP on the same host, `fs cp` of a file (backslash path) and a directory (forward-slash path) into missing nested directories, into an existing one, and onto a file | Windows | Pass: both copies land (the push falls back to `cmd`'s `mkdir`); an existing directory is fine; a path that is a file fails with `tar on local failed: … could not chdir` |

Changed on purpose by these fixes:

- A session whose actor exits releases its executors then; `session ls`
  lists it as `finished` without executors.
- A Docker executor's socket directory is named by 12 hex digits derived
  from its container name instead of the key, and is removed with the
  container.

## Re-verified after the follow-up fixes

Three follow-up fixes (the report keeps the first terminal state, as
`session ls` does; a second termination signal makes a daemon exit at once;
shutdown unwinds a session create in progress) were rerun on 2026-11-10 with
debug builds and every daemon's state, image and output in a scratch
directory. Each fix has a regression test in `make test`.

| Loop | Host | Result |
| --- | --- | --- |
| `examples/local` without an actor: `exec` (exit 0), `curl` fault (126), `session stop`, `session ls`, `report` | Mac | Pass: `stopped` in both |
| Control daemon with a session and an attached `exec -- sleep 300`: SIGTERM, then SIGTERM again 0.5 s later | Mac | Pass: still shutting down after the first; exits 1 0.08 s after the second with `exited at a second termination signal; these sessions were not cleaned up: <id>`, also in the daemon log. One SIGTERM with nothing open exits 0 in 0.04 s |
| A no-actor session whose copy-in takes 67 s, SIGTERM 1.5 s into the create | Mac | Pass: the daemon waits for the copy-in step, then the create fails with `the daemon shut down while the session was being created`; `report` shows `failed` and that reason; the daemon exits 0 |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` with a second, remote executor whose socket listens 8 s later: SIGTERM once the Docker executor is up | Linux | Pass: the daemon outlives the 5 s grace and waits for the create; once the remote executor answers, the create unwinds: `session create` exits 1 with the shutdown reason, the daemon exits 0 7.3 s after the signal, no container, Firecracker or session socket is left, and `report` shows `failed` with the reason |
| `examples/smoke` (and a variant with a 1 GiB copy-in) signalled while `creating` | Linux | Pass: the create ended within the server's 5 s grace for open calls, so the session ran and was stopped; nothing left behind |
| `examples/smoke` finishing on its own, then `session stop` | Linux | Pass: `finished` in `session ls` before and after the stop, and `finished`, `exit 0` in `report`; `routed exit=7` in `session logs` |
| `examples/smoke` with `/bin/sleep 300` and an attached routed `python3` sleep: SIGTERM, then SIGTERM again | Linux | Pass: exits 1 0.02 s after the second, naming the session; its container, Firecracker and sockets are left behind, as the message says, and were removed by hand |

Changed on purpose by these fixes:

- `report` shows the first of `actor.exit`, `actor.vm_exit`, `session.stop`
  and `session.fail` as the session's state; a session stopped after it
  finished reports `finished`.
- A second SIGINT, SIGTERM or SIGHUP (a second Ctrl-C on Windows) makes the
  control daemon or an executor exit 1 at once, naming the sessions or
  processes it did not clean up. Nothing reconciles them yet (slice 9).
- Shutdown cancels the sessions being created; each finishes the step in
  progress (a provider create or a copy-in), then rolls back and records
  `session.fail`.

## Re-verified after fixes b, d, f, g

Four fixes (piped children get default signal dispositions; a routed
program keeps its absolute path and falls back to the executor's `PATH`; an
actor without a workspace mount starts in `~`; an exec whose stream ends
before the command exited fails with 255 and the reason) were rerun on
2026-11-10 with debug builds and every daemon's state, image and output in a
scratch directory. Each fix has a regression test in `make test`.

| Loop | Host | Result |
| --- | --- | --- |
| Control daemon started with SIGINT ignored (`sh -c 'trap "" INT; exec needy daemon …'` in the background): `exec -d -- sleep 300`, then `kill -s INT` | Mac, Linux | Pass: the sleep ends with `SIGINT` in `ps`. On Linux the daemon's `SigIgn` has the SIGINT bit set |
| `exec -- /nonexistent/x`, without a session and in a session | Mac, Linux | Pass: exit 127, `needy: cannot start /nonexistent/x: No such file or directory` |
| A no-actor session whose rule routes `**` to a Docker executor: `exec -- /opt/actor-only/bin/python3`, and the same with `--executor build` | Linux | Pass: routed by the rule, it runs the container's `/usr/bin/python3` from `PATH`; placed with `--executor`, it is not rewritten and exits 127 naming `/opt/actor-only/bin/python3` |
| An actor with no workspace mount and no `actor.cwd`; its driving program runs `~/py/python3` (a symlink in the actor) and `~/py/needy-missing`, both routed to a Docker executor | Linux | Pass: the driving program starts in `/home/agent` (`~/workspace` does not exist); `python3` runs from the executor's `PATH` and does not see `~/py`; `needy-missing` fails with `needy: cannot start /home/agent/py/needy-missing: No such file or directory`, exit 126 from the proxy |
| `exec --session <id> -- sleep 60` attached, then `session stop` | Mac | Pass: exit 255, `needy: the connection ended before the command exited: session stopped` |
| `exec --session <id> -- sleep 60` routed to a Docker executor, then SIGTERM to the daemon | Linux | Pass: exit 255, `needy: the connection ended before the command exited: daemon shutting down`; the daemon exits |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| `examples/smoke` | Linux | Pass: `google`, both workspace files, `routed exit=7` |
| `examples/chaos` | Linux | Pass: `Input/output error` on `broken.json`, `fault injected: ECONNREFUSED` with exit 126, hwmon `+101.0°C ALARM (CRIT)`, mirror-out of `result.txt`; the report counts 3 faults by kind; no container or process left |
| `make check`, `make test` | Mac, Linux | `make check` passes on both, `make test` on the Mac. On Linux, `exec_without_a_session_stays_out_of_a_running_session` and `exec_returns_the_command_exit_code_and_output` fail in most full runs with empty stdout and the right exit status. The base commit fails the same way (4 of 8 runs), so the cause predates these fixes: a fresh start subscribed to its process without replay, so output written before the subscription was lost. Fixed since; see the next section |

Changed on purpose by these fixes:

- A routed command's absolute `argv[0]` is no longer cut to its basename.
  `ExecStart.search_path_if_missing` (set by routing only) lets the executor
  run the program of the same name from its `PATH` when the path is missing.
- Without `actor.cwd`, the driving program starts in `~` unless a mount
  provides `~/workspace`.
- `exec` and `attach` exit 255 when the stream ends before the command
  exited; `ExecResponse.ended` carries the daemon's reason.

## Re-verified after the output race and exit-code fixes

Three fixes (a started client subscribes with the replay buffer as soon as
the process exists, so output written before the subscription reaches it;
the proxy exits 127 when a routed program cannot start; `exec -d` exits 255
when the stream ends before the process started) were rerun on 2026-11-10
with debug builds and every daemon's state and output in a scratch
directory. Each fix has a regression test in `make test`; the race test
starts 32 short commands at once for 10 rounds and failed in round 0 on
every run before the fix.

| Loop | Host | Result |
| --- | --- | --- |
| `make test` | Mac | Pass, 3 of 3 runs in a row |
| `make test`, `make check` | Linux | Pass: `make test` 8 of 8 runs, including `exec_without_a_session_stays_out_of_a_running_session` and `exec_returns_the_command_exit_code_and_output`; `make check` passes |
| `needy build --recipe examples/opencode/gce.recipe.yaml --executor` | Linux | Pass |
| An actor whose driving `sh` runs `~/py/needy-missing` (a symlink to `/bin/true` in the actor) routed to a Docker executor, and `python3` started by `bash -c`, which a rule denies | Linux | Pass: `needy: cannot start /home/agent/py/needy-missing: No such file or directory (os error 2)` and exit 127 from the proxy; `needy: denied: under bash` and exit 126; a routed `python3` outside `bash` runs |
| `examples/smoke` | Linux | Pass: `google`, `routed exit=7` |
| `examples/chaos` | Linux | Pass: `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` with exit 126 for `curl`; the report counts 3 faults by kind (exec, file.open, hwmon); no container or process left |

Changed on purpose by these fixes:

- A routed program that cannot start on its executor exits 127 from the
  proxy, as `needy exec` and a shell do; it exited 126 before.
- `exec -d` exits 255 with `needy: the connection ended before the command
  started` and the daemon's reason when the stream ends before the process
  started; it exited 1 before. `--output json` prints `error`.
- A program name that only a deny rule names is not routed by the actor's
  kernel (`rules::route_names` lists names placed on executors), so the rule
  applies to it only on the CLI path. This is unchanged; it is noted because
  a deny-only rule in an actor profile looks like it should apply.

## Re-verified after sizing the actor rootfs

The actor's `rootfs.ext4` was a fixed 3 GiB ext4 image (786432 blocks,
643021 free) and took 3.1 GB on disk, because `needy build` unpacked the
exported tar by writing every zero. `images/Dockerfile` now sizes the image
to its content (the blocks `du` counts plus 10% and 64 MiB, inodes in use
plus 10%; the rootfs is attached read-only, so it has no room to grow), and
`needy build` writes blocks of zeros as holes. The regression test
`unpack_leaves_runs_of_zeros_as_holes` failed before the fix. Rerun on
2026-11-10 on a Linux KVM host with every daemon's state in a scratch
directory.

| Loop | Host | Result |
| --- | --- | --- |
| `make test`, `make check` | Mac | Pass |
| `build::tests` on ext4 | Linux | Pass |
| `needy build --recipe examples/opencode/gce.recipe.yaml --out DIR` (actor only) | Linux | Pass: `rootfs.ext4` is 550 MB long and takes 442 MB on disk; 140662 blocks of 4 KiB with 23452 free, 8400 inodes with 764 free; `e2fsck -fn` is clean; `firecracker`, `needy` and `vmlinux` keep their modes |
| `images/Dockerfile` `windows` stage | Linux | Pass: `needy.exe` builds |
| `examples/smoke` | Linux | Pass: `google`, routed `python3`, workspace both ways, `routed exit=7` |
| `examples/chaos` | Linux | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` with exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out; the report counts 3 faults |
| `examples/opencode` with a local Ollama model | Linux | Pass: opencode writes `hello.py` and runs it through a routed `bash`; 3 model calls through the actor gateway |

## Re-verified after the CLI usability fixes

`ps` shows `SESSION` and `ORIGIN`, hides needy's own processes unless `-a`,
and splits `STATE` from `EXIT`; the daemon refuses to attach to needy's own
processes; `session ls` keeps released executors and shows `EXIT`; `report`
prints plain text by default with `-o markdown|json`, shows sub-second
durations, and names a command routed through a route stub by its bare name.
Each change has regression tests in `make test`. Rerun on 2026-11-10 with
every daemon's state in a scratch directory.

| Loop | Host | Result |
| --- | --- | --- |
| `make test`, `make check`, `make proto` without a diff | Mac | Pass |
| `examples/local` without an actor, `exec`, `exec -d`, `kill --signal SIGINT`, `fs cp` | Mac | Pass: `ps` lists only the `cli` commands, `ps -a` adds the `fs` `mkdir` and `tar`; `STATE` `exited`/`killed`, `EXIT` `2`/`SIGINT`; attaching to the `tar` is refused with exit 1; attaching to an exited command replays `hi` and exits 2; `session ls` shows `here=native (released)` and a failed session's reason; `report` in all three formats, colors only on a terminal and not with `NO_COLOR` |
| `cargo test -p needy-core-actor` | Linux | Pass, with the stub-name regression test |
| `needy build --out DIR` (actor only; the executor image is unchanged) | Linux | Pass |
| `examples/smoke` with a driving program that sleeps 25 s | Linux | Pass: `ps` hides the `actor-mount` `sftp-server`, `ps -a` shows it; attaching to it is refused and the workspace still works both ways afterwards; `session ls` shows `build=docker (released)` and `EXIT` `0` once finished; the report shows `25.2s`, `python3 on build`, and no `/run/needy/bin` anywhere in the detailed JSON |
| `examples/cli` without an actor, `fs cp`, `exec` | Linux | Pass: `ps` hides the `fs` `mkdir` and `tar`, `ps -a` shows them; attaching to the `tar` is refused; the report shows `0.2s`; no container left |

## Re-verified after the hardening and terminal fixes

Owner-approved fixes, each with a regression test in `make test`: a TCP
listener on a non-loopback address without a token is refused (loopback
without a token warns); the daemon's state and runtime directories are 0700
and its sockets 0600 under any umask, and a Docker executor's socket
directory and default workspace are given to the profile's user (bug 1 of
the cloud-host run below); `ps` without `--session` lists a process once, under the executor it
was started through; a session that ends kills the processes it started on
each executor before releasing it (`TODO(runner-ownership)`); the CLI sends
the profile and its recipe resolved, so the control daemon reads no client
path (`CreateSessionRequest.profile_dir` is gone, `recipe_yaml` is new);
`exec` in an ended session says `session ID has ended: STATE`; and the
terminal note's G2, G4, G5, G7, G8 and G9. Rerun on 2026-11-10 with every
daemon's state in a scratch directory.

| Loop | Host | Result |
| --- | --- | --- |
| `make test`, `make check`, `make proto` without a diff | Mac | Pass |
| `cargo test --workspace` | Linux | Pass |
| `daemon --listen 0.0.0.0:…` without a token, both modes | Mac, Linux | Pass: exit 1, `will not listen on 0.0.0.0:0 without a token …; set a token with --token-env NAME` |
| Control daemon under `umask 000`: modes of the state, sessions, a session's and the runtime directory, `daemon.sock`, `local.sock` | Mac | Pass: `drwx------` and `srw-------` |
| Control daemon with umask 022 | Linux | Pass: 700, 700, 600, 600 |
| `ps` without `--session` with a native session's detached `sleep` and a sessionless one | Mac, Linux | Pass: one row each, under `here` and `local` |
| `exec -t` under `script`: `stty size` after `stty rows 33 cols 111` | Mac (native), Linux (Docker executor) | Pass: `33 111` |
| `attach` from a 41x101 terminal to a detached `exec -t` started at 24x80 | Mac (native), Linux (Docker executor) | Pass: `41 101` |
| A program that turns on the alternate screen, mouse, bracketed paste and hides the cursor; `top` quit with `q` | Mac (native), Linux (Docker executor, `top`) | Pass: after the program's last byte the client writes `CSI = 0 ; 1 u`, `CSI ? 1049 l` (only after the program entered the alternate screen), the mouse, paste and focus resets and `CSI ? 25 h` |
| `exec -t -- sh -c 'setsid sleep 600 & exit 3'` (G8, literal) | Linux (local, Docker) | Pass before and after the fix: exit 3 in 23 ms; the descendant dies of the hangup before it leaves the session |
| The same with the descendant in its own session before `exit 3` | Mac, Linux (local, Docker) | Before the fix: hangs on Linux (the regression test times out), passes on macOS. After: exit 3 in 0.12 s (Mac) and 0.62 s (Linux, the 500 ms drain) |
| `TERM=xterm-ghostty COLORTERM=truecolor exec -t -- sh -c 'echo [$TERM] [$COLORTERM]'` on a Docker executor | Linux | Pass: `[xterm-256color] [truecolor]` |
| `examples/smoke`, `examples/chaos` with an image from `needy build --recipe examples/opencode/gce.recipe.yaml --out DIR` | Linux | Pass: as before, the report counts 4 routed (1 failed) and 3 faults by kind |
| `exec --session` of a finished, then stopped session | Linux, Mac (stopped) | Pass: `needy: session ID has ended: finished` (`stopped`), exit 1 |
| A second `needy daemon --mode executor` on loopback with a token as a remote executor; an actor session with a workspace mount from it and `exec -d --executor far -- sleep 555`; `session stop` | Linux | Pass: before the stop the remote lists the `sftp-server` and the `sleep` running; after it both are `killed SIGKILL` and the remote executor has no children. A session whose actor finished ends its `sftp-server` the same way |
| `session create` from a static `needy` in a container whose profile (relative `extends` and `actor.recipe`) exists only in the container, against the host's control daemon over TCP on loopback with a token | Linux | Pass: the session is created; the profile's directory does not exist on the daemon's host |
| A profile whose user is 1001:1001 with a Docker executor, daemon as 1000 | Linux | Pass: fails in 14 ms with `cannot give …/executors/… to the container's user 1001:1001, so its executor cannot create its socket there; run the daemon as root or as that user: Operation not permitted` |
| The control daemon as root (in a container with the Docker socket and the scratch directory at the same path), a Docker executor as 1000:1000 | Linux | Pass: the session is created and `id` in the executor prints `uid=1000(agent)`; the socket directory and the default workspace are `agent:agent 700`; the daemon's directories `root:root 700` and its socket 600 |

The Docker executor rows mount this tree's static `needy` over the image's
with `docker.args` (`-v …/needy:/usr/local/bin/needy:ro`); the
`needy-executor:latest` image was not rebuilt.

## Re-verified on additional cloud hosts

The tree at `830573e` (its last code change is `45f8065`) was run on
2026-11-10 on three small cloud VMs. That tree predates the rootfs sizing and
the CLI usability fixes above. Nothing was compiled on them: the static
musl `needy`, the actor image (`vmlinux`, `rootfs.ext4`, patched
`firecracker`) and `needy-executor:latest` were built on the Linux host from
the same tree with `needy build --recipe examples/opencode/gce.recipe.yaml
--executor` and copied over. Each control daemon ran as root, started in the
background, with its state, sockets and output in a scratch directory through
`NEEDY_STATE_DIR`, `NEEDY_RUNTIME_DIR` and `NEEDY_DATA_DIR`. `examples/local`
ran on a copy whose native workspace is a missing nested directory in that
scratch directory.

- **A**: a 2-vCPU AMD EPYC cloud VM without hardware virtualization, Debian 13
  (kernel 6.12), Docker 29.
- **B**: a 3-vCPU Intel Xeon (Cascade Lake) cloud VM with nested
  virtualization, Debian 13 (kernel 6.12), Docker 29.
- **C**: a 2-vCPU Intel Cascade Lake cloud VM without hardware virtualization,
  Debian 12 (kernel 6.1, glibc 2.36), Docker 29.

The Docker rows marked "umask 000" ran with the control daemon started under
`umask 000`. This works around bug 1 below in the test harness only.

| Loop | A | B | C |
| --- | --- | --- | --- |
| The static `needy` (static-pie) runs | Pass | Pass | Pass |
| `needy exec` without a session on the local executor: exit 7 with stdout and stderr apart, stdin, `SIGTERM` death exits 143, a missing program exits 127 (by name and by absolute path), `--output json` and `ndjson`, `-t` under `script` (`/dev/pts/1`, `stty size`, exit 4), detach, `ps`, `attach` (143 after `kill`, and again with replay after the exit), `kill` of an unknown process exits 1 | Pass | Pass | Pass |
| `fs cp` of a directory into a missing nested directory and back | Pass | Pass | Pass |
| `examples/local` without an actor: copy-in creates the workspace and skips `secret.txt`, `exit 7`, `curl` fault (126, `needy: fault injected: ECONNREFUSED`) with and without `--executor`, delay fault (`sleep 0.1` takes 609–610 ms against 6–9 ms for `true`), `ndjson` with `fault.injected` and `exec.decide` before `error` and `exit` 126, `fs cp --session` into a missing directory, `session logs` (`has no actor console`, exit 1), `session ls`, stop with mirror-out to `out/{session}/`, `report` (profile `local`, 4 faults) | Pass | Pass | Pass |
| `examples/cli` through a Docker executor, the daemon as root with umask 022 | Fail: bug 1 | Fail: bug 1 (reproduced with `docker run`) | Fail: bug 1 |
| `examples/cli` through a Docker executor, umask 000: create in 0.29–0.52 s, exit 7 with stdout and stderr apart, stdin, `SIGTERM` (143), 127, `--output json`, `-t` (`/dev/pts/0`, exit 3), detach, `ps --session`, `attach` (137 after `kill -s SIGKILL`), `fs cp` of a directory into a missing directory and back; `session stop` leaves no container | Pass | Pass | Pass |
| `max_procs: 1` with `on_full: reject` on a Docker executor, umask 000 | Pass: `needy: executor is full` (exit 1) while a detached `sleep` holds the slot; accepted after `kill` | Pass, same | Pass, same |
| `SIGTERM` to a control daemon with a Docker session, a detached `sleep 300` and an attached `exec -- sleep 60`, umask 000 | Pass: the attached exec exits 255 with `the connection ended before the command exited: daemon shutting down`; the daemon exits after 5.2 s (the 5 s grace for open calls); no container left | Pass, after 5.3 s | Pass, after 5.2 s |
| An actor profile on a host without `/dev/kvm`: `examples/smoke`, and a profile with only an actor running `/bin/true` | Unclear: bug 3. `examples/smoke` with umask 022 fails at its Docker executor first (bug 1, exit 1 after 10 s) | Not applicable | Unclear: bug 3, and bug 4 before KVM is reached |
| `examples/smoke` in an actor with `examples/opencode/gce.recipe.yaml` | Not applicable | Fail: bug 2 | Not applicable |
| `examples/smoke` in an actor with a copy of that recipe without `avx_vnni` in `cpu.features.hide`, umask 000; `session logs`; `report` | Not applicable | Pass: `instance-…`, `Intel(R) Xeon(R) CPU @ 2.20GHz`, `Google Compute Engine`, `systemd-detect-virt` prints `google`, the routed `python3` prints the actor's hostname, workspace both ways, `routed exit=7`; the report counts 4 routed and 1 failed; 3.3 s from create to `finished` | Not applicable |
| `examples/chaos` with the same recipe copy, umask 000; `report` | Not applicable | Pass: copy-in filter, `Input/output error` on `broken.json`, `needy: fault injected: ECONNREFUSED` and exit 126 for `curl`, hwmon `+101.0°C ALARM (CRIT)`, mirror-out of `result.txt` to `out/{session}/`; the report counts 3 faults by kind | Not applicable |
| An actor whose driving `sh` runs `~/py/needy-missing` (a symlink to `/bin/true` in the actor) routed to a Docker executor | Not applicable | Pass: `needy: cannot start /home/agent/py/needy-missing: No such file or directory (os error 2)`, exit 127; a routed `python3` runs | Not applicable |
| `examples/smoke` with `/bin/sleep 300` as the driving program, `session stop` after 8 s | Not applicable | Pass: the stop returns in 0.44 s; `stopped`; no Firecracker process, container or socket left | Not applicable |
| An actor's Docker executor removed after the actor finishes | Not applicable | Pass, with a delay: the container was still up when `session ls` first said `finished`, and gone 1 s later | Not applicable |
| The Docker API through `bollard` on Debian 12 | Not applicable | Not applicable | Pass, umask 000 |
| B's actor routing to A as a remote executor over TCP with a bearer token | Not run | Not run | Not run |

The remote-executor row was not run because the three VMs share no private
network: A and B have only public addresses on unrelated networks, and C
sits alone on a private /24 behind NAT.

### Actor boot under nested virtualization

A profile with only an actor whose driving program is `/bin/true`, three runs
each. "Create" is the time until `session create` returns. "To finished" is
the time from the create call until `session ls` says `finished`, polled every
0.1 s. "Guest kernel" is the last kernel timestamp on the console, and "init"
is init's own timestamp on `the driving program exited`.

| Host | Recipe | Create | To finished | Guest kernel | init |
| --- | --- | --- | --- | --- | --- |
| The Linux host (KVM, Raptor Lake) | `examples/opencode/gce.recipe.yaml` | 6–7 ms | 0.262–0.264 s | 0.144–0.161 s | 0.015–0.016 s |
| B (nested KVM, Cascade Lake) | the same without `avx_vnni` hidden | 21–26 ms | 1.34–1.65 s | 1.09–1.30 s | 0.076–0.101 s |

Under nested virtualization the guest boots about six times slower, and most
of the time is spent in the guest kernel. The Firecracker process starts and
configures the VM in about 70 ms on B.

### Bugs found

1. **A Docker executor cannot start when the control daemon's user is not
   the profile's user.** (Fixed by `a7c27dd`; see "Re-verified after the
   hardening and terminal fixes".) The provider creates the executor's socket directory
   (and the default workspace) with the daemon's UID and umask, then starts
   the container with `--user <profile uid>:<gid>`
   (`crates/core-executor/src/provider/docker.rs`). As root with umask 022
   the directory is `root:root 0755`, so the executor in the container cannot
   create its socket. The profile design note says every side uses the
   profile's UID; the daemon's own host directories do not. The Linux host
   runs its daemon as UID 1000, the profile's UID, which hid this. The
   reproduction on A, B and C, as root:

   ```sh
   mkdir d                                  # root:root 0755
   docker run --rm --user 1000:1000 -v "$PWD/d:/run/needy" needy-executor:latest \
     needy daemon --mode executor --listen unix:/run/needy/executor.sock --name x
   # needy: cannot listen on unix:/run/needy/executor.sock: Permission denied (os error 13), exit 1
   chmod 0777 d                             # the same command now listens
   ```

   Through needy, `needy daemon --mode control` as root, then
   `needy session create --no-actor --profile examples/cli/profile.yaml`,
   exits 1 after 10 s with `needy: executor build: cannot reach
   unix:<state>/executors/<12 hex digits>/executor.sock: transport error`.
   The session is `failed` and no container is left. The message does not
   name the permission error, because the container's stderr goes away with
   the container (`auto_remove`).

2. **The example recipe cannot boot on a host CPU without CPUID leaf 7,
   subleaf 1.** (Fixed by `cd25f96`; see "Re-verified after the recipe,
   failed-actor and static Firecracker fixes".) `examples/opencode/gce.recipe.yaml` hides `avx_vnni`, which
   `crates/core/src/recipe/cpu.rs` maps to leaf 7, subleaf 1, EAX bit 4. On
   B, Firecracker refuses the template:
   `Template changes a CPUID entry not supported by KVM: Leaf: 7, Subleaf: 1`,
   exit 1. `needy profile check` reports only a warning about `avx512f`.
   With `avx_vnni` removed from the hide list, the actor boots and every
   actor row above passes. Cascade Lake CPUs have no leaf 7, subleaf 1, so
   the same refusal is expected on such a host without nesting, but this was
   not verified.

3. **An actor that never starts is reported as `finished` with no
   findings.** (Fixed by `40a5d12`, `1fad5c0` and `9f55744`; see "Re-verified
   after the recipe, failed-actor and static Firecracker fixes".) This happens when Firecracker exits at once: no `/dev/kvm`
   (A, C), the CPUID refusal (B), or the glibc mismatch (C). Then
   `session create` exits 0 and prints the ID, `session ls` says `finished`,
   and `report` says `finished` with driving program `-`, "No findings" and
   "No actor ran in this session". The daemon log has no warning. The reason
   is only in `session logs`. On A it is
   `Kvm error: Error creating KVM object: No such file or directory (os error 2)`.
   No VM, container or socket is left. To reproduce on a host without
   `/dev/kvm`, create a session from a profile with only `actor.recipe` and
   `actor.command: [/bin/true]`.

4. **The patched Firecracker does not run on Debian 12.** (Fixed by
   `2823c1e`; see "Re-verified after the recipe, failed-actor and static
   Firecracker fixes".) `images/Dockerfile`
   builds it for `x86_64-unknown-linux-gnu` in the `rust:1` stage, so it
   needs glibc 2.38. On C:
   `firecracker: /lib/x86_64-linux-gnu/libc.so.6: version 'GLIBC_2.38' not found`.
   This is reported only through `session logs` (bug 3). The static `needy`
   is unaffected.

No product code was changed in this pass.

## Re-verified after the recipe, failed-actor and static Firecracker fixes

The tree at `9f55744` was run on 2026-11-10 on the Linux host and on the three
cloud VMs A, B and C above. The fixes are:

- `cd25f96`: a recipe hides a feature only in CPUID entries the host's KVM
  supports. The host side reads `KVM_GET_SUPPORTED_CPUID` through
  `kvm-ioctls`, as Firecracker does. A hide in an entry KVM lacks is left out
  of the CPU template with a warning, because the guest cannot see that
  feature. A feature the recipe requires and the host lacks is still an error.
- `40a5d12`, `1fad5c0`, `9f55744`: a host without a usable KVM device fails
  `session create` of an actor at once, before any executor is acquired, with
  a message that names the device. The message says the container actor
  runtime is designed but not available yet. A VM that exits before its
  driving program reported an exit ends the session `failed`, recorded as
  `session.fail`. The reason has Firecracker's exit status and the last five
  lines of its log. `session ls` and `report` show it.
- `2823c1e`: `images/Dockerfile` builds Firecracker for
  `x86_64-unknown-linux-musl` against a static musl `libseccomp`, as upstream
  releases are built. The build fails if the binary has a program interpreter
  or needs a shared library. Before the change, this check failed the gnu
  build (`libgcc_s.so.1`, `libc.so.6`). The musl build also gets Firecracker's
  default seccomp filter, which upstream ships only for musl targets.

The actor image (`vmlinux`, `rootfs.ext4`, `firecracker`, the static
`needy`) was built on the Linux host with `needy build --recipe
examples/opencode/gce.recipe.yaml`. Only the files each VM needs were copied.
A and C got `needy` and `firecracker`. B got the image, the examples and
`needy-executor:latest` (`docker save`, built earlier and not rebuilt). Each
control daemon ran in the background, as root on the VMs with umask 022, with
its state in a scratch directory through `NEEDY_STATE_DIR`, `NEEDY_RUNTIME_DIR`
and `NEEDY_DATA_DIR`.

| Loop | Linux host | A | B | C |
| --- | --- | --- | --- | --- |
| `firecracker` is static: `file` says `static-pie linked`, `ldd` says `statically linked`, `firecracker --version` prints `Firecracker v1.17.0` | Pass | Pass | Pass | Pass, glibc 2.36 |
| `needy profile check examples/smoke/profile.yaml` | Pass: no warning about `avx_vnni` | Not run | Pass, exit 0: `warning: recipe: cpu.features.hide: avx_vnni has no effect: this host's KVM has no CPUID leaf 0x7, subleaf 0x1, so the guest cannot see it`, and the earlier `avx512f` warning | Not run |
| `examples/smoke` with the unmodified `examples/opencode/gce.recipe.yaml` | Pass: `finished`, exit 0, `google`, `routed exit=7`, the report counts 4 routed and 1 failed | Not applicable | Pass: the same output; 2.1 s from create to `finished`; no CPUID refusal in the console | Not applicable |
| `examples/chaos` with the same recipe | Pass: copy-in, `Input/output error` on `broken.json`, `ECONNREFUSED` and exit 126 for `curl`, `+101.0°C ALARM (CRIT)`, 3 faults by kind | Not applicable | Pass, the same | Not applicable |
| `session create` of `examples/smoke` without `/dev/kvm` | Not applicable | Pass: exit 1 after 17 ms with `needy: this host has no usable KVM: cannot open /dev/kvm: No such file or directory (os error 2); a firecracker actor needs it. A container actor runtime, …, is designed in docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md but not available yet`; `session ls` says `failed` with the message in EXIT; `report` says `failed` with the reason; no container was created | Not applicable | Pass, the same, after 9 ms |
| No VM, container or daemon left after `session stop` and the daemon's `SIGTERM` | Pass | Pass | Pass | Pass |

A VM that exits before its driving program did was not reproduced on a host:
each such cause seen on these VMs (no KVM, the CPUID refusal) is now caught
earlier or gone. The daemon test
`a_vm_that_exits_before_its_driving_program_fails_the_session_with_the_reason`
covers it with a stand-in Firecracker that prints the CPUID refusal and exits 1.

## Re-verified after the identity fixes

The provenance audit's findings
([`2026-11-10-provenance-inventory-evidence.md`](2026-11-10-provenance-inventory-evidence.md))
were fixed in `9e1e669` to `d873c23` and run on 2026-11-10 on the Linux KVM
host (Intel Core i5-13600KF, a hybrid Raptor Lake CPU). Each actor image was
built twice with `needy build --recipe` (no `--executor`): once from
`a36f735`, before the fixes, and once from `d873c23`, into a scratch directory,
from `examples/opencode/gce.recipe.yaml` and from
`builtin:dell-optiplex-ubuntu-24.04` (the host's CPU accepts both). A probe
profile ran as uid 0 in each actor; every daemon ran in the background with
its state in a scratch directory. The serials and UUIDs below are the
generated ones.

| Check | Before (`a36f735`) | After (`d873c23`) |
| --- | --- | --- |
| GCE `dmidecode -t system -t baseboard` | Serial `GoogleCloud-6B1F5C2E0D4A9E8B7C3D2A1F0E9D8C7B` and UUID `6b1f5c2e-…` (the patch's defaults, whatever the seed); board serial `Board-GoogleCloud-6B1F5C2E0D4A9E8B` | Serial `GoogleCloud-CAAC3C4E8BA44C1BAF4327A5333ED04F`, UUID `caac3c4e-8ba4-4c1b-af43-27a5333ed04f`, board serial `Board-GoogleCloud-CAAC3C4E8BA44C1BAF4327A5333ED04F` |
| GCE `dmidecode` header and BIOS | `SMBIOS 3.0.0 present`; `UEFI is supported` | `SMBIOS 2.4 present`; `Targeted content distribution is supported`, as on GCE |
| Dell `/sys/class/dmi/id` | `chassis_vendor=Google`, `board_serial=Board-GoogleCloud-…`, `product_serial=GoogleCloud-…`; BIOS `1.21.0`, `06/11/2024`; board `0C8MHW`; chassis type 7 | `chassis_vendor=Dell Inc.`, empty board serial, `product_serial=89431f96` (the recipe's `{hash}`); BIOS `1.7.3`, `10/27/2022`; board `0VC8RJ`; chassis type 3 (Desktop) |
| `/proc/version` build host | not checked | GCE `buildd@lcy02-amd64-115`, Dell `buildd@lcy02-amd64-028` |
| `ls /sys/class/hwmon/hwmon0/`, `sensors` | Core 0 in `temp2`, Core 4 in `temp3`, …, Core 28 in `temp9` | Core N in `temp(N+2)`: `temp2`, `temp6`, …, `temp30`; package in `temp1`; `sensors` lists the same labels |
| `/proc/cpuinfo` flags in the actors | `pni` present; no `hybrid_cpu` | The same; `KVM_GET_SUPPORTED_CPUID` on the host returns CPUID.07H:EDX bit 15 (hybrid) clear |
| `needy profile check` of `examples/smoke` and `examples/chaos` | not run | exit 0, no findings |
| `examples/smoke` (GCE image) | not run | Pass: `finished`, `google`, `routed exit=7`, 4 routed and 1 failed |
| `examples/chaos` (GCE image) | not run | Pass: `Input/output error` on `broken.json`, `curl exit=126`, `+101.0°C ALARM (CRIT)`, 3 faults by kind, `result.txt` mirrored out |
| Nothing left | Pass | Pass: no daemon, Firecracker or container |

Not changed by these fixes, seen on the way:

- `uname -a` shows the guest kernel's own release and date (`6.18.62 #1 SMP
  PREEMPT_DYNAMIC …`), not the recipe's `os.kernel.release` and `version`:
  `images/Dockerfile` reads only the `KBUILD_BUILD_*` build arguments, and
  nothing consumes `NEEDY_KERNEL_RELEASE` or `NEEDY_KERNEL_VERSION`.
- The probe actor has no network interface, and `network.mac` is compiled
  but not applied anywhere, so the MAC fixes are checked by unit tests only.

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
