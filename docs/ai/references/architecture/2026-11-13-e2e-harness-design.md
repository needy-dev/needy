# End-to-end harness (`cargo xtask e2e`)

**Status**: implemented, 2026-11-13. The harness is `xtask/src/e2e/`. Results
before and after the four fixes of 2026-11-13 are under [Results](#results).

Related notes:

- [Closed-loop results](2026-11-09-closed-loop-results-evidence.md): how end to
  end checks were run by hand until now.
- [Release and CI](2026-11-12-release-and-ci-design.md): `check.yml`, which
  runs the cells without an actor.
- [Drivers and extension points](2026-11-12-drivers-and-extension-points-design.md)
  and [Runners and runs](2026-11-10-runner-and-run-design.md): what the cells
  exercise.

## Why

Four bugs survived every end-to-end run of the closed loop, because the runs
were hand-written happy-path scripts that:

- only ended by the program exiting, never by `run stop` or a lost daemon;
- copied into single-level paths (`~/seed/`), never under `~/.config/...`;
- never combined a mirror-out with a filter, from the actor or an executor;
- ran root daemons, which hid permission bugs;
- asserted output, not invariants.

The harness replaces those scripts. It tests invariants, over a matrix, with a
non-root daemon, and it reports every cell it could not run, and every
workaround, as a failure or as not covered: never as a footnote.

## Running it

```sh
cargo xtask e2e                       # every cell this host can run
cargo xtask e2e --list                # the cells, and why some cannot run here
cargo xtask e2e --only macos-vz/ --skip end=daemon-kill --parallel 4
cargo xtask e2e -o json > e2e.json    # one JSON document
cargo xtask e2e --clean               # remove the images and the work directory it built
```

- Exit code: `0` when every selected cell passed, `1` when any failed, `2` when
  none failed but some cells of this platform were not covered.
- `--only` and `--skip` take a glob over the cell ID (`*` within one `/`
  segment, `**` across) or a substring (`macos-vz/`, `end=stop`).
- `--parallel N` runs N cells at once. Each cell has its own short directory
  under `/tmp` (`ne2e-…`: state, runtime, config and data directories, so
  every Unix socket path fits `sun_path`), its own control daemon and its own
  runs.
- Before the cells, the harness builds from the tree it runs in, into
  `.tmp/e2e/` (git-ignored): `needy` (debug, in `target/e2e`), the guest needy
  (`images/Dockerfile`'s `guest` target, as `make guest`), the base VM image of
  this host's VM driver (`needy image build --out`), the container actor image
  and the executor image, tagged `needy-e2e-actor:latest` and
  `needy-e2e-executor:latest` so it never replaces an image someone built by
  hand. A content hash of the inputs skips what is current; `--no-prepare`
  reuses what is there.
- A failed cell keeps its daemon log, profiles and consoles under
  `.tmp/e2e/results/TIME/CELL/`; `--keep` keeps every cell's directory.

### Remote: one host

```sh
cargo xtask e2e --remote HOST --remote-dir DIR [--only …]
```

ships `git archive HEAD` to `HOST:DIR/src` (uncommitted changes are not
shipped, and the harness says so), runs `cargo xtask e2e -o json` there with
`DIR/work` as its work directory, removes `DIR/src` (unless `--remote-keep`),
and prints the remote result here. The remote host builds everything itself,
on its own CPUs. `DIR` is relative to the remote home.

### Remote: every host at once

```sh
cargo xtask e2e --hosts .tmp/e2e/hosts.json
```

The hosts file is local JSON (`.tmp/` is ignored); host names and ssh aliases
are never part of the repository. Each host has a role:

| Role | What it does | Needs |
| --- | --- | --- |
| `local` | This machine, as `cargo xtask e2e`. Gets `--windows-runner` when a `runner` host is listed. | the toolchain |
| `build` | Gets `HEAD`, builds needy, the guest and the images natively, runs its cells; for its `run` hosts it also exports (`--export`) a static needy, a static harness (`images/Dockerfile`'s `e2e` target), the base VM image and the container images. | the toolchain, Docker |
| `run` | Compiles nothing. Gets what it lacks of its build host's export, streamed through this machine (`tar` and `gzip -1` over ssh): the static needy and harness always, the base VM image only where `/dev/kvm` exists, each container image only when it does not have that image ID already (`docker load`). Then runs the static harness with `--no-prepare`, as `user` when the ssh login is root. | same architecture as its build host, Docker |
| `runner` | A Windows machine: builds its own `needy.exe` from `HEAD` and joins the `local` host's `runner/at=windows` cells. | the toolchain (MSVC), OpenSSH with `cmd.exe` |

```json
{ "hosts": [
  { "name": "mac", "role": "local" },
  { "name": "kvm", "role": "build", "ssh": "ALIAS", "dir": "needy-e2e", "parallel": 4 },
  { "name": "small", "role": "run", "ssh": "root@ALIAS", "from": "kvm", "user": "needy-e2e",
    "dir": "/home/needy-e2e/needy-e2e", "only": ["linux-container.runc/", "none/"] },
  { "name": "windows", "role": "runner", "ssh": "ALIAS" }
] }
```

`only`, `skip` and `parallel` per host override the command line's. The
results print host by host, then the totals; the exit code is the worst host's.
`--clean` with `--hosts` removes the harness's images and directories on every
host.

### What each host role needs

| Host | Needs |
| --- | --- |
| Apple silicon Mac (`macos-vz`, `macos-apple-container`) | vfkit; Apple's `container` with its services started; Docker (OrbStack or Docker Desktop: the harness reads `DOCKER_HOST` from the Docker CLI's context) to build the guest and images |
| Linux build host with KVM (`linux-firecracker`, `linux-container`) | a user in the `docker` group with read and write access to `/dev/kvm`; the Rust toolchain; for the `linux-container` workspace cells `sshfs` and `user_allow_other` in `/etc/fuse.conf`; for `runsc` cells gVisor registered with `runsc install -- --host-uds=all` |
| Linux run host, root login | a test user for the daemon (`useradd -m -G docker[,kvm] needy-e2e`), `sshfs` and `fuse3`, `user_allow_other` in `/etc/fuse.conf`; the same architecture as its build host |
| Windows runner | the Rust toolchain (MSVC), `tar.exe` and OpenSSH server with `cmd.exe` as the shell (Windows 11 defaults), TCP forwarding allowed |

## The matrix

The full product of the dimensions is thousands of runs, most testing the
same code twice. The matrix is a covering set: for every actor driver, each
value of each dimension appears in at least one cell, and the combinations
that hid the bugs above are cells of their own. `cargo xtask e2e --list`
prints it; `matrix::tests` keep its coverage.

### Dimensions

| Dimension | Values |
| --- | --- |
| Actor driver | `linux-firecracker`, `linux-container.runc`, `linux-container.runsc`, `macos-vz`, `macos-apple-container`; `none` for the cells without an actor |
| Terminal | none, `-t` (attach later, detach, attach again), `-it` |
| How the run ends | the program exits, `run stop`, `run stop --now` (once it exists), daemon SIGKILL and restart (reconcile) |
| File mappings | copy-in host→actor nested (`~/.config/e2e/nested/seed/`, filtered) and single-level (`~/single/`); mirror-out actor→host with and without a filter; the same three from and to an executor; the workspace from a native executor, from a container executor, and from the host (copied in) |
| Chaos | one fault per domain with its effect observed by the program (exec: exit 7 with stderr, and SIGSEGV; file: `EIO` on `open`; net: refused, reset, a 503 on a service; sensor: an overheat at 2 s), and `needy chaos add` at run time |
| Network | an allowed host, a denied host, a service |
| User | default (`agent`, 1000), renamed (`bob`, 1001) |
| Executor | `native`, `linux-container`, a runner's `native` (local, or a Windows machine) |
| Daemon user | non-root; no cell needs root (see [Decisions](#decisions)) |
| TUI | the cases of `xtask/src/e2e/tui.rs` (none yet; see [TUI hook](#tui-hook)) |

### Cells per actor driver

29 to 31 cells per driver, IDs `DRIVER/SCENARIO[/DIM=VALUE,…]`:

| Scenario | Cells | Fixed dimensions |
| --- | --- | --- |
| `actor-files` | `end=exit`, `end=stop`, `end=stop-now`, `end=daemon-kill` | native executor none; terminal none |
| `executor-files` | `executor=native|linux-container` × `end=exit|stop|daemon-kill` | terminal none |
| `workspace` | `from=executor-native`, `from=executor-linux-container`, `from=host` | |
| `routing` | one | native executor, a `deny` rule |
| `chaos` | `fault=exec`, `file`, `net`, `sensor` (not for `macos-apple-container`, whose README row is ❌), `chaos-add` | |
| `network` | `allow`, `deny`, `service` | |
| `identity` | `user=default|renamed` × `executor=native|linux-container` | |
| `terminal` | `mode=t`, `mode=it` | |
| `fake-agent` | one | `-t`, `run stop`, nested config, routed shells |
| `runner` | `at=local`, `at=windows` | a native executor on a runner |
| `tui` | one per TUI case | `-it` |

And two cells without an actor: `none/exec` (`needy exec`: exit codes,
stdout and stderr, a missing command, `--output json`, `-t`, `-d`, `ps`,
`kill`) and `none/fs-cp` (`needy fs cp` into a nested directory that does not
exist, and back).

A Linux driver's cells run on Linux, a macOS driver's on macOS, and the
`linux-container` executor only on Linux, since its socket is a bind-mounted
host socket (`NOTICE` in `crates/core-executor/src/driver/linux_container/mod.rs`);
a macOS driver has no container-executor cells at all. Cells of the other
platform are listed as "other platforms" and do not change the exit code;
another host covers them.

### The fake agent

`fake-agent` does what agents do, without an account: on its first run it
reads a prompt on a terminal (`fake-agent> `), "logs in" by writing
`~/.config/fake-agent/auth/token.json` (mode 0600) and a cache, spawns routed
shells (`bash -c` to a native executor), writes a session log, is detached
from and waits to be stopped. `run stop` must mirror the login out to the host,
private, without the cache (`- /cache/**`); its second run copies it back in,
under a `~/.config` the user owns, and says it is logged in.

## Invariants

Every cell:

- its daemon starts as the harness's user; the harness refuses to run cells as
  root (each cell fails with the reason);
- the control socket is 0600 in a 0700 runtime directory, and no socket under
  the cell is reachable by another user (the socket, or a directory on its way
  inside the cell, has no group or other bits), at daemon start and while a
  run is live;
- once the daemon stopped on SIGTERM (within 30 s), nothing of the cell is
  left: no process whose command line names the cell's directory (VMs, sshfs,
  SFTP servers, runners), no container labelled with its runs (Docker, Apple's
  `container`), no mount under it, no socket; and the daemon's user can remove
  the cell's directory. What is found is reported, then removed.

Per scenario:

- **Exit codes**: the program's, through `run ls` and `run create -it` and
  `run attach`; a routed command's (7); a denied command's (126); a fault's.
- **Placement**: from the trace (`exec.decide` events in `report --level
  detailed -o json`) and the report (`commands.routed`, `denied`,
  `route_names`): routed commands decided once each, denied ones named, local
  ones never decided; and as the program sees it (a host marker file exists on
  the native executor, not in the actor).
- **Copy-in**: contents, owner and mode of every file and directory, nested and
  single-level, including the parents copy-in created (`~/.config`); the user
  can write next to what was copied in; the filter's exclusions are absent.
- **Mirror-out**: the files are on the host after every way the run ends, the
  filtered mapping without what its filter excludes, the unfiltered one with
  it. Not after `run stop --now` from the actor (see [Decisions](#decisions)).
- **Identity**: `whoami`, `id -un`, `id -u`, `id -g`, `HOME`, `USER`,
  `LOGNAME`, `getent passwd` and `~` agree in the actor and in a container
  executor; a native executor is the runner's user, with `HOME`, its passwd
  entry and `~` in agreement.
- **Faults**: the effect the program observes (exit code, signal, `Input/output
  error`, a refused or reset connection, a 503, `+101.0°C`) and
  `fault.injected` (and `chaos.add`) in the trace.
- **Network**: an allowed host answers 200 and is in the report; a denied host
  fails and is a report finding; a service answers through the gateway and its
  server saw the request.
- **Run and actor state** in `run ls`: `running` with the actor `configured`
  or `ready` while live (`ready` for `-t`, after detaching too); `finished`
  with the exit code, `stopped`, or `failed` naming the restart after a
  reconcile; an ended run's actor `exited` or none; a stopped or failed run's
  executors released or lost.
- **Runners**: the runner joins and is online with `executor(native)` in
  `runner ls`; `exec --runner` returns the command's exit code; a run routes to
  it; nothing of it is left on the remote machine afterwards.

## TUI hook

`xtask/src/e2e/tui.rs` holds a `Case` trait (the driving program, and a
`drive` function given the pseudo-terminal, `term::Term`, which keeps the raw
transcript and a `vt100` screen) and `cases()`, empty today
(`TODO(tui-fidelity)`). Each case becomes a cell per actor driver,
`DRIVER/tui/NAME`, run with `run create -it`. The TUI fidelity harness
(`cargo xtask tui`, `xtask/src/tui/`) landed alongside; its scenarios
(`xtask/tui/*.scenario`) drive agents that need accounts, so none is a cell
yet. A case that needs no account registers in `cases()`, and its `drive`
runs a scenario through `cargo xtask tui` or drives `Term` itself.

## Results

### Before the fixes of 2026-11-13

`main` at `5f64bac`, with the harness on top, run with `--hosts` (the Mac
local, the KVM host as the build host). Non-root daemons everywhere.

| Host | Pass | Fail | Not covered | Not covered because |
| --- | --- | --- | --- | --- |
| Apple silicon Mac: `macos-vz`, `macos-apple-container`, `none`, a Windows 11 runner | 34 | 15 | 2 | no `run stop --now` yet |
| Linux KVM host (Intel, 20 threads, build host): `linux-firecracker`, `linux-container` (runc), `none` | 33 | 24 | 38 | no `runsc` registered (31), no `sshfs` (3), no Windows runner for its cells (2), no `--now` (2) |

The run hosts of that run did not finish: streaming the uncompressed export
through this Mac's uplink ran at about 180 KB/s per host, so the transfers
were stopped and the export was made per image, skip what a host has, and
compressed (see the `run` role).

What failed, by bug:

| Bug | Cells that caught it |
| --- | --- |
| **Copy-in ownership**: a nested copy-in leaves the parents it creates (`~/.config`, `~/.config/e2e`, …) owned by root, so the user cannot write next to its own config | `actor-files/*` and `fake-agent` on every driver: `owner of /home/agent/.config: expected agent:agent, got root:root`, `config-writable=no` |
| **Mirror-out filters**: a mirror-out's filter is ignored, from the actor (init packs with no filter) and from an executor (the daemon pulls with none) | `actor-files/end=exit` and `executor-files/*/end=exit|stop` on every driver: `the filter excludes *.log: skip.log` |
| **Graceful `run stop`**: `run stop` ends the actor without the driving program's exit, so init never mirrors out | `actor-files/end=stop` and `fake-agent` on every driver: `keep.txt on the host: got None`, the login lost between runs |
| **Process socket permissions**: with a non-root daemon a `linux-container` actor's process service is unreachable, so a `-t` actor never becomes `ready` | `linux-container.runc/terminal/mode=t`, `mode=it` and `fake-agent`: `the actor … does not serve its terminal: transport error` |

Also failing, each a finding rather than one of the four:

- **Mirror-out after a reconcile**: after a daemon SIGKILL and restart, neither
  the actor's nor an executor's mirror-out runs (`actor-files/end=daemon-kill`,
  `executor-files/*/end=daemon-kill`). The brief asks for mirror-out after
  every way a run ends; whether a reconcile should mirror out is open.
- **A renamed user on a container executor needs a root daemon**:
  `identity/user=renamed,executor=linux-container` fails at `run create` with
  `cannot give … to the container's user 1001:1001 … run the daemon as root`
  (the documented `NOTICE` in `share_with_container`). It is reported as a
  failure because the workaround is a root daemon.
- **`reset` hangs under `macos-vz`**: `macos-vz/chaos/fault=net` saw curl time
  out (28) on a connection the `reset` fault should have reset; the same cell
  passes under `macos-apple-container` and `linux-firecracker`.
- **An ended run's actor stays `configured`**: once, a finished run still showed
  its actor `configured` a second after the run ended
  (`macos-apple-container/identity/user=renamed,executor=native`); not
  reproduced in the next run.

Not covered on the Linux KVM host: `linux-container.runsc/*` (no `runsc`
registered with the engine), the `linux-container` workspace-from-executor and
file-fault cells (no `sshfs`), and `end=stop-now` (no `--now` yet).

### After the fixes

The harness rebased onto `90a8e8e`, which holds the four fixes (`f804539`
copy-in directories, `bae7025` mirror-out filters, `ff63782` the container
actor's process socket, `9958a64` graceful `run stop` and `--now`). One
`--hosts` run on five hosts at once, plus the Mac again on its own (its leg of
that run found no engine: see the first decision below on `104dca6`).

| Host | Pass | Fail | Not covered | Not covered because |
| --- | --- | --- | --- | --- |
| Apple silicon Mac: `macos-vz`, `macos-apple-container`, `none`, the Windows runner | 45 | 6 | 0 | |
| Linux KVM host, build host | 51 | 8 | 36 | no `runsc` (31), no `sshfs` (3), no Windows runner for its cells (2) |
| Cloud VM, 2 vCPUs, no KVM, run host: `linux-container` (runc), `none` | 28 | 4 | 1 | no Windows runner for its cells |
| Cloud VM, 3 vCPUs, nested KVM, run host: `linux-firecracker`, `none` | 28 | 4 | 1 | no Windows runner for its cells |
| Cloud VM, 2 vCPUs, no KVM, gVisor, run host: `linux-container` (runsc), `none` | 28 | 4 | 1 | no Windows runner for its cells |

Every cell that caught one of the four bugs passes on every host, the
`end=stop-now` cells included. What still fails is the findings above, on
every driver: the mirror-out after a reconcile (`*-files/*end=daemon-kill`),
the renamed user on a container executor with a non-root daemon, and on
`macos-vz` the `reset` fault. One more was seen once and not again:
`exec --runner` on the Windows runner returned 1 for `cmd /c exit 5` seconds
after the runner joined (`macos-apple-container/runner/at=windows`); the
check now records the command's output.

## Decisions

Made on the spot, as the brief allowed:

- **A covering matrix, not the product.** See [The matrix](#the-matrix).
- **Platform-impossible cells are not cells.** A macOS driver with a
  `linux-container` executor, and the sensor cell of `macos-apple-container`
  (README ❌), are left out instead of being reported as not covered forever.
  Cells of the other platform are listed and counted, but leave the exit code
  alone: another host covers them.
- **`linux-container` cells run on Linux only**: its sockets do not cross
  Docker Desktop's or OrbStack's VM (the driver's `NOTICE`). Since `104dca6`
  the capability probe agrees, so the harness asks the engine itself whether
  it can build the guest and images, not the container actor's detection.
- **Not covered is an exit code.** `2` when nothing failed but cells of this
  platform could not run (a missing `runsc`, `sshfs`, Windows runner or
  `--now`): visible in CI and scripts, distinct from a failure.
- **No cell runs a root daemon.** None needs one: every privilege a cell
  wanted was a product bug or a documented limitation, and both are reported
  as failures. `Cell::root` keeps the dimension in the output
  (`TODO(e2e-root)`).
- **`run stop --now` does not expect the actor's mirror-out.** `--now` does not
  wait for the driving program, and init mirrors out after it exits; the
  executor's mirror-out, which the daemon does, is still expected. Revisit
  with the graceful-stop fix.
- **A daemon SIGKILL expects mirror-out too.** The brief says every way a run
  ends; those cells fail today and say so.
- **Own image tags.** `needy-e2e-actor:latest` and `needy-e2e-executor:latest`,
  so a harness run never replaces the owner's `needy-actor:latest`.
- **`DOCKER_HOST` from the Docker CLI's context** when it is unset, so the
  daemons reach OrbStack or Docker Desktop on a Mac as `docker` does.
- **The workspace cells map no `~` path**: a `path_map` entry for `~` won over
  the workspace's longer entry (`REVIEW(path-map)` in `scenarios.rs`), and a
  container workspace path the image lacks failed the routed commands
  (`REVIEW(workspace-missing)`); the cells use paths that exist and the
  findings stay marked.
- **Remote hosts build natively.** A build host builds from `git archive HEAD`;
  run hosts only get its static needy, static harness and images; the Windows
  runner builds `needy.exe` itself.
- **The Windows runner joins through `ssh -R`.** The control daemon also
  listens on a free loopback port. Windows OpenSSH does not end a session's
  processes when it closes, so the runner outlives its ssh client, as a
  runner should; the harness removes it (`runner rm`), ends its `needy.exe`
  with PowerShell, and checks that none is left.
- **Run hosts log in as root, cells run as a test user.** On a root-login
  host the harness drops to `user` with `runuser -l`; the user is in the
  `docker` (and `kvm`) group, so no daemon runs as root.
- **CI runs `none/` only.** Hosted runners have no KVM or Apple virtualization;
  the two cells without an actor run on Linux and macOS in `check.yml`.

## Follow-up candidates

- A `runner` host for a build host's cells (`TODO(e2e-runner)` in
  `remote.rs`): the build host would need its own ssh access to the runner.
- A cell that runs its daemon as root, should a feature genuinely need one
  (`TODO(e2e-root)`).
- TUI fidelity cases (`TODO(tui-fidelity)`).
