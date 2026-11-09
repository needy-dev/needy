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
| Terminal recordings in asciicast v2 under `sessions/<id>/casts/` | Pass |
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
