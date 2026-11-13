# End-to-end harness (`cargo xtask e2e`)

**Status**: implemented, 2026-11-13. The harness is `xtask/src/e2e/`. Results
before and after the four fixes of 2026-11-13, and of the second round of
fixes with the TUI cells, are under [Results](#results).

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
| `run` | Compiles nothing. Gets what it lacks of its build host's export, streamed through this machine (`tar` and `gzip -1` over ssh): the static needy and harness always, the base VM image only where `/dev/kvm` exists, each container image only when it does not have that image ID already (`docker load`), the base VM image only when its hash changed, and the executor image rebuilt there from the static needy when it has one already. Then runs the static harness with `--no-prepare`, as `user` when the ssh login is root. | same architecture as its build host, Docker |
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
| Transport | how a local runner reaches the control plane: `direct`, `ssh-l`, `idle-proxy`, `cloudflare` (see [Transports](#transports)) |
| Daemon user | non-root; no cell needs root (see [Decisions](#decisions)) |
| TUI | the cases of `xtask/src/e2e/tui.rs`: Codex's start screen, before a login (see [TUI cells](#tui-cells)) |

### Cells per actor driver

28 to 35 cells per driver (`cargo xtask e2e --list` prints them), IDs `DRIVER/SCENARIO[/DIM=VALUE,…]`:

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
| `runner` | `at=local,transport=direct|ssh-l|idle-proxy|cloudflare`, `at=windows` | a native executor on a runner; for `at=local`, the workspace from it |
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

### Transports

The `runner/at=local` cells vary how the runner reaches its cell's control
daemon, which listens on a loopback TCP port as well as its socket
(`xtask/src/e2e/transport.rs`):

| Transport | The runner joins | Runs when |
| --- | --- | --- |
| `direct` | the daemon's port, `http://127.0.0.1:PORT` | always |
| `ssh-l` | an `ssh -L` forward to that port | `ssh.destination` is set in the local settings |
| `idle-proxy` | `cargo xtask link-proxy`'s proxy in front of the port, which drops a connection idle for 30 s | always |
| `cloudflare` | `exec:cloudflared access tcp --hostname H`, with a service token; the harness runs the tunnel's connector (`cloudflared tunnel run`) in front of the port | `cloudflare` is set in the local settings and the variables it names are |

Each cell's actor mounts its workspace from the runner's executor, writes
to it, stays idle for 45 s (longer than the proxy's limit; needy's HTTP/2
pings every 20 s keep the connections), reads it again, and prints
`cut-me`. The harness then drops every connection of the transport at
once, as a tunnel that restarts does (the proxy closes them; the `ssh -L`
or the tunnel connector is ended and started again; nothing for
`direct`), and writes `cut.done` on the runner's side. The actor must see
`cut.done` through the mount, write `after.txt` back, and route one more
command: the runner reconnects, and the mount starts its SFTP stream again
(`crates/core-actor/src/sshfs/stream.rs`).

The local settings name real machines and accounts, so they are a file
outside the repository: `transports.json` in the work directory (`.tmp/e2e/`,
git-ignored), or the file `NEEDY_E2E_TRANSPORTS` names. Secrets stay in the
environment; the file names the variables:

```json
{
  "ssh": { "destination": "user@host.example.com" },
  "cloudflare": {
    "hostname": "needy-e2e.example.com",
    "origin_port": 7400,
    "tunnel_token_env": "CF_TUNNEL_TOKEN",
    "client_id_env": "CF_ACCESS_CLIENT_ID",
    "client_secret_env": "CF_ACCESS_CLIENT_SECRET"
  }
}
```

`ssh.destination` must reach this host's loopback (its own sshd). The
Cloudflare tunnel's ingress must forward `hostname` to
`tcp://localhost:ORIGIN_PORT` on this host, behind an Access application
whose policy admits the service token; no other connector may run for that
tunnel meanwhile, and the Cloudflare cells run one at a time. Without the
settings, a cell is not covered and says which setting is missing.

`cargo xtask link-proxy --to PORT [--delay 75ms] [--idle 30s]` is the same
proxy on its own: it prints its port, and adds the delay to each direction.

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

## TUI cells

`xtask/src/e2e/tui.rs` registers each TUI case in `cases()`; each becomes a
cell per actor driver, `DRIVER/tui/NAME`. Today there is one case, `codex`:
Codex's sign-in picker before a login, which needs no account and is the
same on every start.

The cell drives the program with the TUI fidelity harness (`cargo xtask
tui`, `xtask/src/tui/`, called in process through `tui::report` and
`tui::compare`), twice, through one scenario: wait for the start screen,
snap it (`start`), resize to 40x120 and snap (`grown`), resize back to
30x100 and snap (`back`).

- **Direct**: `docker run -it` of the agents image as its user in its home,
  with the program under `/.needy/real` (as `xtask/tui/direct.sh` does),
  then the program's own quit keys.
- **Through needy**: `needy run create -it` of a profile that runs the same
  program in the agents image, with its profile's allowlist, then ctrl-]
  (detach).

It asserts that each snap through needy equals the direct one (`tui diff`
semantics: digits and long hexadecimal IDs masked, text, cursor, modes, wide
cells, title and truecolor compared), that `run create -it` returns 0 on the
detach and leaves nothing on in the client's terminal (`left_on` empty: the
relay's restore), that the detached run is `running` with its actor
`ready`, and that `run stop` ends it as every stop cell checks. The other
paths of the fidelity matrix (typing, pastes, mouse, a read-only viewer, a
background start, `exec -t`) stay with `xtask/tui/matrix.sh` (`NOTICE` in
`tui.rs`).

The agents image is `examples/agents/Dockerfile` (embedded in the harness)
on the harness's actor image, tagged `needy-e2e-agents:latest`, built on
every host that runs a TUI cell, run hosts included: it downloads the
agents' pinned releases and compiles nothing. From it the harness makes this
host's VM image (`needy image build --from`, into the work directory's
`images/`, which each cell's data directory links to) or loads it into
Apple's `container`.

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

### Second round: the remaining failures, and the TUI cells

`main` at `1083056`. What failed after the first round, and what became of
it:

| Failure | Class | Root cause | Fix |
| --- | --- | --- | --- |
| `identity/user=renamed,executor=linux-container` (2 on the build host, 1 per Linux run host) | product | A container executor runs as the profile's user, and the daemon shared its socket and default workspace directories by giving them to that user, which only root may do; the executor's socket was 0600, its own user's only. | `e874f0b`: a daemon that is not root shares them by its group (mode 0770, the container joins it with `group_add`) and the executor gives its socket to that group, mode 0660 (`needy daemon --socket-group`). |
| `*/end=daemon-kill` mirror-out (6 on the build host, 2 on the Mac, 3 per Linux run host) | deferred | The actor and executors are lost with the control plane; surviving that is D1. | None: the cells are not covered, `deferred: D1 (runs do not survive a control-plane crash)`, after asserting everything else (`TODO(graceful-shutdown)` in `scenarios.rs`). |
| An ended run's actor `configured` (flake, `macos-apple-container`) | product | The guest's exit report ended the run, while the actor ended only when its VM or container exited (after init's mirror-out and shutdown), so `run ls` could read the run `finished` with its actor `configured`. | `1c3014d`: the exit report records the exit; the run finishes once its actor exited. |
| `exec --runner` returned 1 for `cmd /c exit 5` (flake, Windows runner) | product | A runner is `online` from its join and heartbeats, but its control stream, which an exec needs, opens a moment later; an exec in between failed "runner … is not connected" (exit 1), not an exit status or relay problem. | `5f11cd4`: an exec on an online runner without its control stream waits for it, up to one heartbeat interval; a runner that is not online still fails at once. |
| `reset` fault on `macos-vz` hangs (curl 28) | product, through vfkit | vfkit's vsock relay passes no close to the guest (`VirtioSocketConnection` has no `CloseWrite`), so the gateway's close never reached the guest's forwarder. | `854f722`: at a connection's start the gateway writes a marker (`contract::GATEWAY_RESET`) before closing; the forwarder then resets the program's connection (`SO_LINGER` 0), on every driver. A reset of a later request on a kept-alive plain-HTTP connection is still only a close (`TODO(macos-vz-reset)`). |
| `linux-firecracker/executor-files/executor=native,end=daemon-kill`: a routed `python3` failed with `Text file busy` (126), found in this round | product | Init rewrote every VM route stub in place each time the route list came, and it comes twice at start (configuration, then the guest control stream), so a program's exec met a stub open for writing. | `d67b73a`: a stub that exists stays; a new one is renamed into place. |
| TUI harness tests failing together on the Mac, found in this round | test | Two tests started in the same clock tick shared a directory. | `1083056` |

On the Linux build host, the 8 failing cells were 6 `end=daemon-kill`
(deferred, D1) and 2 renamed users on a container executor (product, fixed);
none was a harness or environment fault.

Harness changes in `98ef353`: the TUI cells (see [TUI cells](#tui-cells)), a
cell that defers an invariant is not covered with the deferral as its
reason, and a run host receives only changed images (the executor image is
rebuilt there from the static needy).

#### Final runs

Three `--hosts` runs back to back on `1083056` plus this note: the Mac
(with the Windows runner) and the Linux build host, at once. The cloud run
hosts could not be reached for them: the jump host's key was not loaded in
the session's ssh agent while the owner was away. Their results are from the
run just before on `1083056` (`cloud-runc`), or there are none.

| Host | Run 1 | Run 2 | Run 3 | Not covered because |
| --- | --- | --- | --- | --- |
| Apple silicon Mac: `macos-vz`, `macos-apple-container`, `none`, the Windows runner | 49 pass, 0 fail, 4 not covered | same | same | deferred: D1 (4) |
| Linux KVM host, build host: `linux-firecracker`, `linux-container` (runc, runsc), `none` | 58 pass, 0 fail, 40 not covered | same | same | runsc is registered without `--host-uds=all` (31; the owner installs it with sudo), deferred: D1 (6), no Windows runner for its cells (3) |
| Cloud VM, no KVM, run host: `linux-container` (runc), `none` | 30 pass, 0 fail, 4 not covered (the run before) | not run | not run | deferred: D1 (3), no Windows runner (1); later runs: host unreachable, jump-host key not loaded while the owner is away |
| Cloud VM, nested KVM, run host: `linux-firecracker`, `none` | not run | not run | not run | all its cells: host unreachable, jump-host key not loaded while the owner is away |
| Cloud VM, gVisor, run host: `linux-container` (runsc), `none` | not run | not run | not run | all its cells: host unreachable, jump-host key not loaded while the owner is away |

The TUI cells pass on `macos-vz`, `macos-apple-container`,
`linux-firecracker` and `linux-container.runc`; `linux-container.runsc/tui`
waits for runsc. No flake showed in the three runs; the two flakes of the
first round (the actor state and `exec --runner`) are fixed above, and the
`Text file busy` failure was seen once, before its fix.

### Transport cells

The `runner/at=local` transport cells, run on their own on 2026-11-13
(`--only runner/at=local`):

| Host | `direct` | `idle-proxy` | `cloudflare` | `ssh-l` |
| --- | --- | --- | --- | --- |
| Apple silicon Mac, `macos-vz` | pass | pass | not covered (the tunnel's connector ran elsewhere) | not covered (no sshd accepts this host's key) |
| Linux KVM host, `linux-firecracker` (static harness, `--no-prepare`) | pass | pass | pass, after the connector-registration wait (`fa22458`) | not covered (same) |

On a tree whose mount does not reconnect (the reconnect window set to
zero), `macos-vz/runner/at=local,transport=idle-proxy` fails with
`after-cut: expected ok, got fail` and no `after.txt`: the cell catches the
bug it was written for. Measurements over these transports are in the
[Cloudflare transport evidence](2026-11-13-cloudflare-transport-evidence.md).

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
- **A daemon SIGKILL defers the mirror-out (2026-11-13, second round).** The
  actor and the executors are lost with the control plane, and surviving that
  is the deferred item D1, keeping runs alive across a control-plane restart.
  The `end=daemon-kill` cells still assert everything else (the run `failed`
  naming the restart, its executors released or lost, nothing left), and are
  then **not covered** with the reason `deferred: D1 (runs do not survive a
  control-plane crash)`, never passed (`Checks::defer`,
  `TODO(graceful-shutdown)` in `scenarios.rs`).
- **A run host rebuilds its executor image instead of receiving it** when it
  has one already: `FROM needy-e2e-executor:latest` with this commit's static
  needy copied in, which compiles nothing. Every commit changes only that
  layer, and sending the whole image through this machine's uplink took most
  of a run. The base VM image is sent only when its sources' hash changed
  (`images.hash`).
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

- `ssh-l` needs an sshd that reaches the harness's host; neither the Mac
  nor the Linux build host of 2026-11-13 accepts its own key, so those
  cells were not covered there (the
  [Cloudflare transport evidence](2026-11-13-cloudflare-transport-evidence.md)
  ran `ssh -L` by hand).

- A `runner` host for a build host's cells (`TODO(e2e-runner)` in
  `remote.rs`): the build host would need its own ssh access to the runner.
- A cell that runs its daemon as root, should a feature genuinely need one
  (`TODO(e2e-root)`).
- More TUI cases (pi, opencode, Claude Code, herdr) and more of the
  fidelity matrix's paths as cells; Codex alone already covers the start,
  resize and detach path on every driver.
