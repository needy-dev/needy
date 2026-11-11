# macOS native actor (proof of concept)

**Status**: proof of concept, 2026-11-12, on the branch
`poc/macos-native-actor` only; not for `main` without the owner's decision.
Code: `crates/core-actor/src/host/macos/` (the runtime and the fence) and
`crates/core-actor/src/native/` (init, shims, the kept-command service, the
denial reader). Examples: `examples/macos-native/`.

Labels: **[V-test]** verified here by a run on the Mac, **[V-code]** read in
source or an SDK header at the pinned revision, **[V-doc]** stated by vendor
documentation, **[I]** inferred.

Host for every measurement: an Apple silicon laptop (M5 Max, 18 cores),
macOS 27.2 (build 26B123), needy built from this branch in release mode
unless a figure says debug.

Related notes:

- [Container actor runtime](2026-11-10-container-actor-runtime-design.md): the
  shims design and the route file this reuses.
- The macOS actor research of 2026-11-12 (not committed; it lives in the
  owner's checkout under `.tmp/research/macos-actor/notes.md`), whose Seatbelt
  experiment this builds on.

## Summary

- A `macos-native` actor runs the driving program as a process on the Mac
  under a per-run Seatbelt profile (the **fence**) applied with
  `/usr/bin/sandbox-exec`. Its `PATH` is one directory of shims, a link per
  program name. There is no VM.
- Routing works as in a container actor. A name on the run's route list goes
  to the daemon, which runs it on an executor. Every other command is
  **kept**: init starts the real program under the same fence, with only that
  program allowed to execute.
- Every way around the shims that the research found, and four more, is
  denied, and each denial is an `actor.deny` trace event and a report finding
  **[V-test]**. Interactive `-t -i`, detach and attach work **[V-test]**.
  opencode ran as the driving program: its model calls went through the
  gateway, and its bash tool was routed **[V-test]**.
- Time to ready is about 150 ms from `needy run create` to the driving
  program's first command. A kept exec costs about 18 ms against 1.8 ms
  natively. A routed one costs about 12 ms with a native executor
  **[V-test]**.
- **Recommendation: do not productize now.** Keep it as a labeled,
  experimental "fast local loop, low isolation, no identity" tier, after the
  `apple-container` and `vz` runtimes. The reasons are in
  [Recommendation](#recommendation).

## Design

```
needy daemon ──spawns──▶ needy __needy-macos-init DIR   (outside the fence)
   │  guest.sock ◀────────── GetConfig, ReportExit, PushEvents (actor.deny)
   │  gateway.sock ◀──────── 127.0.0.1:P forwarder ◀── HTTP(S)_PROXY
   │  process.sock ◀──────── terminal service (with -t)
   │                         kept.sock: kept-command service
   │                         log stream (denials, by the run's tag)
   │
   └─ sandbox-exec -f fence.sb -D KEPT=<driver> <driver> …   (inside the fence)
         PATH=DIR/actor/shims   SHELL=DIR/actor/shims/bash   HOME=DIR/actor/home
         └─ `ls` ─▶ shims/ls (the needy binary)
               ├─ routed name ─▶ guest.sock: Decide, then an exec stream to the executor
               └─ otherwise ───▶ kept.sock: init runs
                     sandbox-exec -f fence.sb -D KEPT=/bin/ls /bin/ls …
```

- **Runtime** (`host::macos::MacosNative`). `actor.runtime: macos-native` is
  explicit and compiled only on macOS. On Linux the daemon refuses it with
  "runs only on macOS". `check_host` needs `/usr/bin/sandbox-exec`. The
  channels are Unix sockets in the run's `channels/` directory, as for a
  container. `setup` is `Host`, so the daemon sends init no mounts, hostname
  or sensors. `start` spawns this binary as init, in its own process group,
  with stdout and stderr in `console.log`. `stop` sends SIGTERM to init; after
  the grace period it sends SIGKILL to init and to the driving program's
  process group (`actor/driver.pid`). `identity_gaps` lists every identity
  field, plus "a process sandbox on this Mac's kernel, not a VM". So
  `profile check` and the run's log say so, and a recipe that requires a
  field fails `run create`.
- **Init** (`native::init`), outside the fence:
  1. It gets the configuration on `guest.sock`.
  2. It makes `actor/{home,tmp,shims}` and one symbolic link to the needy
     binary per program name on the search path. The search path is the
     profile's `PATH`, or `/usr/bin:/bin:/usr/sbin:/sbin` plus Homebrew's
     prefixes. That gives 1,881 shims here.
  3. It writes the route list (`actor/route`, the container's format).
  4. It binds `127.0.0.1:0` and forwards it to `gateway.sock`.
  5. It writes `actor/fence.sb`.
  6. It serves `kept.sock`.
  7. It starts `log stream` for the run's tag.
  8. It starts the driving program with `sandbox-exec`, on a PTY with the
     terminal service when the run has `-t`.
  9. It reports the exit and exits.
- **Shim** (`native::shim`), inside the fence: the needy binary, started from
  a link in a directory that holds `.needy-shims`. It finds the run's
  directory from its own path (`_NSGetExecutablePath`). So a program that
  clears its environment still reaches needy.
  - A name on the route list goes to the daemon. The daemon either runs it on
    an executor (`Origin::ActorProxy`, as the guest's proxy does) or keeps it.
  - A kept command goes to `kept.sock`.
  - Either way the shim relays input, output, the window size and the exit or
    signal death.
- **Kept command** (`native::kept`). The client sends only a bare program
  name, never a path. Init resolves the name on the search path to a real
  path, then starts `sandbox-exec -f fence.sb -D KEPT=<real> -D KEPT_2=…
  -D KEPT_3=… -D TOOLS=… <real> ARGS`. `KEPT_2` and `KEPT_3` are what the
  kernel executes before the program: a script's `#!` interpreter, and
  `/bin/bash` for `/bin/sh`, which re-executes it. `TOOLS` is the developer
  directory when the program is a `/usr/bin` trampoline of the developer
  tools (`python3`, `git`). The kept program's children meet the fence and
  the shims again.

### Alternatives for kept commands

| Option | For | Against |
| --- | --- | --- |
| **Init spawns it under a per-command profile** (built) | No daemon round trip for unrouted names. The fence stays one file with parameters. | The kept process is init's child, not the caller's. `getppid`, process groups and job control differ, as for a routed command. Kept commands that the daemon never decided are not traced. |
| The daemon spawns it, as a local executor that applies the actor's fence | Every kept command is an `exec.start` and `exec.exit` in the trace | One more hop on every exec. The executor has to know the fence. |
| Allow the real system binaries in the fence and let shims `exec` them | No relay. Native parent and child. | Every allowed path can then also be run directly by absolute path, unrouted and untraced. That is the guarantee this design exists for. |
| `DYLD_INSERT_LIBRARIES` to rewrite `execve` in process | Could redirect without a relay | SIP strips it for `/bin/sh` and `/usr/bin/env`, hardened binaries ignore it, and static or Go binaries skip libc (research notes) |

## What it guarantees

Each guarantee holds for the driving program and every process it starts,
because the profile is inherited. No root is needed. All are **[V-test]**
with `examples/macos-native/bypass.yaml`. The paths in this table are
reserved stand-ins (`/Users/user`).

| # | Probe | Result | Trace (`actor.deny`) |
| --- | --- | --- | --- |
| 1 | `ls` through `PATH` | ran (a kept command) | none |
| 2 | `/bin/ls` by absolute path | `Operation not permitted`, rc 126 | `process-exec* /bin/ls` |
| 3 | `/bin/echo` copied into the workspace, then run | `Operation not permitted`, rc 126 | `process-exec* …/workspace/e` |
| 4 | `./s.sh` (`#!/bin/sh`) | `bad interpreter: Operation not permitted` | `process-exec* …/s.sh` |
| 5 | `./t.sh` (`#!/usr/bin/env sh`) | same | `process-exec* …/t.sh` |
| 6 | `sh s.sh` | ran: `sh` is a shim, and its commands meet the fence | none |
| 7 | Python `subprocess.run(["/bin/ls"])` | `[Errno 1] Operation not permitted` | `process-exec* /bin/ls` |
| 8 | Python `subprocess.run(["ls"])` | ran through the shim | none |
| 9 | TCP to 1.1.1.1:443 | `[Errno 1]` | `network-outbound remote:*:443` |
| 10 | TCP to 127.0.0.1:22 | `[Errno 1]` | `network-outbound remote:*:22` |
| 11 | DNS (`getaddrinfo`) | `nodename nor servname provided` | `network-outbound /private/var/run/mDNSResponder` |
| 12 | `listen` on 127.0.0.1:5555 | `[Errno 1]` | `network-bind local:*:5555` |
| 13 | write `/tmp/needy-escape` | `Operation not permitted` | `file-write-create /private/tmp/needy-escape` |
| 14 | write in the real home | `Operation not permitted` | `file-write-create /Users/user/needy-escape` |
| 15 | read the real `~/.ssh` | `Operation not permitted` | `file-read-metadata /Users/user/.ssh` |
| 16 | write the workspace | ok | none |
| 17 | `open -a Calculator` (Launch Services) | error -54 | `lsopen` |
| 18 | `osascript` to Finder (Apple events) | `File permission error. (-54)` | `mach-lookup com.apple.coreservices.launchservicesd`, `lsopen` |
| 19 | `launchctl submit` (a launchd job) | rc 1 | `job-creation` |
| 20 | `curl https://example.com` through the gateway (allowed) | 200 | `net.connect` |
| 21 | `curl https://example.org` through the gateway (not allowed) | `CONNECT tunnel failed, response 403` | `net.deny` |
| 22 | `curl --noproxy '*'` | `Could not resolve host` | `network-outbound …/mDNSResponder` |

The report of that run lists each denial once per operation and target
(`fence denied: process-exec* /bin/ls (2x)`, and so on), next to the
gateway's `network denied: example.org`.

Probes 17 to 19 are not in the research's list. Any program that may start
another program outside the sandbox can be asked to: Launch Services (`open`),
Finder and Terminal through Apple events, and launchd (`launchctl submit`).
The fence denies `lsopen`, `appleevent-send`, `job-creation` and the
`launchservicesd` lookup **[V-test]**.

`examples/macos-native/profile.yaml` (the smoke test, with python3 routed)
printed `Darwin arm64`, `macOS`, the workspace, `darwin` from the routed
python3, the files written on both sides, and `routed exit=7` **[V-test]**.

`examples/macos-native/interactive.yaml`, driven on a pseudo-terminal by
`.tmp/macos-native-actor/interact.py`, did the following **[V-test]**:

- `run create -t -i` gave an interactive `sh`. `tty` printed `/dev/ttys016`.
- Kept `uname -sm` and `ls` ran on the shell's terminal. The routed python3
  saw a terminal (`isatty() True`). `/bin/ls /` was refused.
- ctrl-] detached ("detached from run …").
- `run attach` returned to the shell, `echo attached-again` printed, and
  `exit 3` made `run attach` exit 3.

opencode 1.19.3, with its native binary as the driving program and
`run -m opencode/big-pickle "Run uname -a …"`, worked **[V-test]**:

- It reached `opencode.ai` through the gateway (`net.connect`).
- The gateway refused `registry.npmjs.org`.
- Its bash tool ran `uname -a` routed to the executor `build`
  (`bash on build 1`).
- It reported the output.

Before `SHELL` pointed at the shims, the same run failed with `EPERM:
operation not permitted, posix_spawn '/bin/sh'`. opencode starts its shell by
absolute path.

## What it does not guarantee

- **Code inside an allowed program.** An interpreter that is kept or is the
  driving program (`node`, `python3`, `sh`) runs any code in its own process:
  file reads, computation, and syscalls that the fence does not cover. Only
  its execs, network, writes and the listed reads are fenced. The driving
  program's own binary may be executed by absolute path anywhere in its tree
  (it is `KEPT`). A kept `/usr/bin` trampoline allows the whole developer
  directory for its tree (`TOOLS`).
- **Programs that start helpers by absolute path fail.** Routing does not
  pick them up. pnpm's launcher for opencode runs `command -p sed`, which is
  `/usr/bin/sed`, and was refused. opencode itself worked only as the driving
  program by its binary's path, and only after `SHELL` named the shim. Many
  tools behave like this (Homebrew and npm launchers, build systems that
  resolve `/usr/bin/xcrun`). They fail closed with `EPERM`, which is safe but
  breaks them **[V-test]**.
- **Reads.** Apart from the hidden paths, the whole disk is readable: other
  projects, `~/Documents`, browser profiles. Deny-by-default reads would need
  an allow list per program (dyld, frameworks, `/System`, `/usr/lib`, locale
  data), as Codex's and sandbox-runtime's profiles carry **[I]**.
- **The host is visible.** There are no namespaces. `ps` shows the user's
  processes, and `sysctl hw.*` and `machdep.cpu.*`, users, the host name and
  the kernel are the Mac's. There is no identity, and the recipe cannot be
  shown. `identity_gaps` reports all eight fields.
- **Signals and inspection.** `(allow default)` leaves `signal` and
  `process-info*` open. A fenced program can signal the user's other
  processes. `(deny signal (target others))` is a candidate rule; it is not
  tested.
- **The gateway port is loopback TCP** that every local process can reach.
  Such a process could use the run's gateway, including any credential it
  injects for an allowed host. A per-run proxy token
  (`Proxy-Authorization`) would close that **[I]**.
- **Kept commands are not traced** unless the daemon decided them (names on
  the route list). The same is true in a container actor. The report's "kept
  on actor" counts only decided ones.
- **No workspace mounts, copy-in or mirror-out.** sshfs needs macFUSE or FSKit
  on the Mac. The workspace is a local directory (`actor.cwd`), which an
  executor shares through its `workspace:`. A profile with mount mappings
  fails `run create`. Copy-in and mirror-out are skipped with a warning.
  Docker executors do not work on this Mac with OrbStack, independent of the
  actor: OrbStack does not relay the executor's bind-mounted socket ("cannot
  reach …/executor.sock: transport error") **[V-test]**. The examples use a
  native executor.
- **One user.** The driving program runs as the daemon's user. The profile's
  `user` names only the home's place in `actor.cwd` (`/home/NAME` is read as
  the actor's home).
- **No memory or CPU limit.** Seatbelt has none.
- **Rules' `when.under`.** The shim sends no process ancestry
  (`TODO(macos-native)` in `native/shim.rs`).
- **A restarted daemon** marks a leftover native actor lost and does not end
  it (`TODO(macos-native)` in `reconcile.rs`).
- **Mixed platforms.** The driving program sees darwin. Executors may be
  Linux, so path maps and OS-dependent commands must account for that.
- **Socket paths.** macOS limits a Unix socket path to 104 bytes. The
  experiment's state directory had to be short (`/tmp/nmna`), because the
  worktree's path was too long. The default `~/.local/state/needy/runs/<id>/`
  fits.

## Performance [V-test]

Release build. Each figure is from three runs of
`.tmp/macos-native-actor/bench.yaml`; the first run after a build is slower
(cold caches) and is listed apart.

| Measure | Warm | First run |
| --- | --- | --- |
| init: configuration to the driving program's start (1,881 shims, fence, forwarder, services) | 91 to 94 ms | 157 ms |
| `needy run create` started to the driving program's first command (a kept `perl` that prints the time) | 149 to 153 ms | 263 ms |
| kept exec (`uname`, 50 in a loop) | 17.9 to 18.4 ms each | 44.8 ms |
| routed exec to a native executor (`hostname`) | 11.3 to 12.9 ms each | 17.5 ms |
| decided by the daemon, then kept (`id -u`, on the route list with `to: actor`) | 18.6 to 20.5 ms each | 23.1 ms |
| baseline: `uname` with no needy | 1.8 ms | |
| baseline: `uname` under a bare `sandbox-exec -f` (no shim, no daemon) | 10.0 ms | |

So about 8 ms of a kept exec is `sandbox-exec` compiling the profile, and the
rest is the shim (a 33 MB binary), the gRPC connection and the relay. A debug
build costs 23 ms per kept exec and 17 ms per routed exec. For comparison,
Firecracker is ready in 160 to 190 ms on the Linux KVM host, and Apple's
`container` takes 0.60 to 0.75 s (research notes).

Candidate speed-ups, not built:

- Precompile the profile once per run (`sandbox_compile_file` and
  `sandbox_apply`, private API, as Chrome does) instead of `sandbox-exec` per
  exec.
- Use a small dedicated shim binary.
- Keep the shim's connection to init alive across a shell's commands. This
  is not possible per exec.

## The fence

The profile of one run, as generated (`actor/fence.sb`). The paths are
reserved stand-ins, and the run directory is the experiment's.

```scheme
(version 1)
; needy macos-native fence, needy-7e8caec34f95. Generated; do not edit.
(allow default)
; Exec: the shims, the shim binary, and the one program this sandbox-exec starts.
(deny process-exec* (with message "needy-7e8caec34f95"))
(allow process-exec* (subpath "/private/tmp/nmna/state/runs/7e8caec34f95/actor/shims") (literal "/Users/user/bin/needy") (literal (param "KEPT")) (literal (param "KEPT_2")) (literal (param "KEPT_3")) (subpath (param "TOOLS")))
; Programs that start programs outside the sandbox: Launch Services, Apple events, launchd jobs.
(deny lsopen (with message "needy-7e8caec34f95"))
(deny appleevent-send (with message "needy-7e8caec34f95"))
(deny job-creation (with message "needy-7e8caec34f95"))
(deny mach-lookup (with message "needy-7e8caec34f95") (global-name "com.apple.coreservices.launchservicesd"))
; Network: the gateway's loopback port and the actor's own sockets.
(deny network* (with message "needy-7e8caec34f95"))
(allow network-outbound (remote ip "localhost:52748"))
(allow network-outbound (remote unix-socket (path-literal "/private/tmp/nmna/state/runs/7e8caec34f95/channels/guest.sock")))
(allow network-outbound (remote unix-socket (path-literal "/private/tmp/nmna/state/runs/7e8caec34f95/channels/kept.sock")))
; Writes: the actor's directories, the terminal and the null devices.
(deny file-write* (with message "needy-7e8caec34f95") (require-not (require-any (subpath "/private/tmp/nmna/state/runs/7e8caec34f95/actor/home") (subpath "/private/tmp/nmna/state/runs/7e8caec34f95/actor/tmp") (subpath "/private/tmp/needy-macos-native/workspace") (literal "/dev/null") (literal "/dev/zero") (literal "/dev/tty") (literal "/dev/dtracehelper") (regex #"^/dev/ttys[0-9]+$") (regex #"^/private/var/folders/[^/]+/[^/]+/T/xcrun_db"))))
; Reads: the user's keys and credentials.
(deny file-read* (with message "needy-7e8caec34f95") (subpath "/Users/user/.ssh") (subpath "/Users/user/.gnupg") (subpath "/Users/user/.aws") (subpath "/Users/user/.azure") (subpath "/Users/user/.kube") (subpath "/Users/user/.docker") (subpath "/Users/user/.netrc") (subpath "/Users/user/.config/gh") (subpath "/Users/user/.config/gcloud") (subpath "/Users/user/.config/needy") (subpath "/Users/user/Library/Keychains"))
```

Quirks found while building it, each marked `NOTICE:` in the code
**[V-test]**:

- **Unix sockets.** `(remote unix-socket (path-literal A) (path-literal B))`
  allowed only A. The kept-command socket, second in the rule, was denied.
  So there is one rule per socket.
- **Scripts.** Seatbelt checks a script and its `#!` interpreter both. Only
  the script allowed gives `EPERM` from `sandbox-exec`.
- **`/bin/sh`** re-executes `/bin/bash`, so a kept `sh` also allows bash.
- **Developer tools.** The `/usr/bin` trampolines (`python3`, `git`) run
  `<developer dir>/usr/bin/<name>`, and python3 then runs the framework's
  `python3.9`. They also write `xcrun_db-*` in the per-user temporary
  directory (`confstr`, not `TMPDIR`), so those files are writable.
- **dyld** writes `/dev/dtracehelper` at every start; it is allowed, so the
  log is not flooded.
- **Denial messages.** The kernel's denial message ends with the tag. For a
  program that is not Apple's, sandboxd appends a violation report after the
  tag. So the reader matches `eventMessage CONTAINS "<tag>"`, not `ENDSWITH`.
  With `ENDSWITH`, opencode's denial was missed.
- **Shim runtime.** The shim must not drop its tokio runtime before exiting.
  The relay's stdin reader blocks in `read`, and the drop waits for it, so on
  a terminal every kept command ended only at the next keystroke.

## Deprecation risk of `sandbox-exec`

- **Apple marks it deprecated.** `man sandbox-exec` on macOS 27.2 (page
  dated March 9, 2017) reads: "The sandbox-exec command is DEPRECATED.
  Developers who wish to sandbox an app should instead adopt the App Sandbox
  feature" **[V-doc]**. The SDK header `usr/include/sandbox.h` (MacOSX 27.2
  SDK) begins "This header is deprecated and may be removed in a future
  release". It marks `sandbox_init` `API_DEPRECATED("No longer supported",
  macos(10.5, 10.8))` (lines 7 and 46) **[V-code]**. SBPL itself has no
  public documentation; the operation names here come from the kernel's own
  denial messages.
- **It is still shipped and widely relied on.** It has worked in every macOS
  release since 10.8, including 27.2 here. Removing it would break
  well-known tools:
  - OpenAI Codex CLI runs only `/usr/bin/sandbox-exec`
    (https://github.com/openai/codex/blob/392ced7d3a9160fd35d40c606359869215f50522/codex-rs/sandboxing/src/seatbelt.rs#L57-L61).
    Its base policy starts `(deny default)` and allows `process-exec`
    (https://github.com/openai/codex/blob/392ced7d3a9160fd35d40c606359869215f50522/codex-rs/sandboxing/src/seatbelt_base_policy.sbpl#L7-L11).
  - Anthropic's sandbox-runtime, which Claude Code's sandboxing uses, builds
    a profile with `(deny default (with message …))`
    (https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1080-L1090).
    It allows its proxy ports on localhost
    (https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1262-L1341).
    It runs `/usr/bin/sandbox-exec -p`
    (https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1591-L1602).
    It reads denials with `log stream`
    (https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1642-L1650).
  - Chromium's renderer sandbox is an SBPL profile that Codex's policy cites
    (`sandbox/policy/mac/renderer.sb`, `drc=7b3962fe2e5fc9e2ee58000dc8fbf3429d84d3bd`,
    as quoted at the Codex link above). Chromium applies it through the
    library, not the command **[V-code]**.
  - Apple's own system services run under SBPL profiles in
    `/System/Library/Sandbox/Profiles` **[I]**.
- **Risk.** Apple could remove the `sandbox-exec` binary, or change SBPL
  operations or the log format, without notice. A removal of the binary alone
  could be absorbed by calling the same private library (`libsandbox`), as
  Chromium does **[I]**. Removing Seatbelt itself would break App Sandbox and
  is unlikely **[I]**. There is no supported replacement for "sandbox a child
  process with a custom profile": App Sandbox is per signed app bundle with
  fixed entitlements, and Endpoint Security needs a restricted entitlement
  (research notes).
- **Pinning.** A product version should test its fence on each macOS release
  (the bypass probes are that test) and say which releases were verified.

## Recommendation

**Do not productize now.** Keep the branch as evidence.

Why:

1. **It breaks real tools.** Programs that run system binaries by absolute
   path fail closed (pnpm's launcher; any `command -p`, `/usr/bin/xcrun` or
   `/bin/sh -c` caller that ignores `SHELL`). The fix is either an allow list
   of system binaries, which run unrouted and untraced, or giving up the
   guarantee. That is a product decision with no good default.
2. **Isolation is a process sandbox with open reads and a visible host.** It
   suits trusted driving programs and fast local loops, not untrusted code.
   needy's identity features do not apply at all.
3. **The VM tiers cover the Mac better.** `apple-container`, which another
   agent is building, gives a real VM per actor with the existing shims image.
   `vz` gives Firecracker parity (`binfmt_needy`, in-guest identity).
4. **The API is deprecated.** It is stable in practice, but it needs a
   per-release check.

If the owner wants a no-VM tier later, it needs:

- An explicit `macos-native` label in `profile check`, the report and the
  docs as "low isolation, no identity". This is already done here through
  `identity_gaps`.
- A decision on absolute-path system binaries. Choose strict (this PoC), or
  an allow list such as SIP-protected `/usr/bin`, `/bin`, `/usr/sbin`,
  `/sbin` that run unrouted, with a warning in `profile check`.
- Kept commands traced as `exec.start` and `exec.exit` (init can push them).
- Deny-by-default reads with an allow list. A per-run proxy token on the
  gateway. `(deny signal (target others))`.
- The ancestry for `when.under` (`sysctl KERN_PROCARGS2`). Reconciliation of
  a leftover actor. Copy-in and mirror-out.
- A faster exec path: one compiled profile applied through `libsandbox`
  instead of `sandbox-exec` per exec, and a small shim binary.
- A per-macOS-release CI job that runs `examples/macos-native/bypass.yaml`.

Follow-up candidates found on the way (not in this branch):

- **Guest proxy runtime drop.** The guest's proxy (`guest/proxy.rs`) also
  drops a temporary tokio runtime after `block_on`. If its relay's stdin
  reader blocks the same way on Linux, a routed command on a terminal would
  end only at the next keystroke. It is worth one interactive test in a
  container actor.
- **Docker executors on OrbStack** cannot be reached (socket relay). They need
  the loopback-TCP transport that the container design already plans for
  Docker Desktop.

## Reproducing

From the branch, on a Mac:

1. `make release`.
2. Run a control daemon with a short state directory:
   `NEEDY_STATE_DIR=/tmp/nmna/state NEEDY_RUNTIME_DIR=/tmp/nmna needy daemon
   --mode control`.
3. `needy run create --profile examples/macos-native/bypass.yaml`.
4. Then `needy run logs RUN` and `needy report RUN`.

The experiment's scripts (`daemon.sh`, `n.sh`, `go.sh`, `bench.yaml`,
`baseline.sh`, `interact.py`) are in `.tmp/macos-native-actor/` of the
worktree that built this branch, and are not committed.

## Tests

- Unit tests:
  - `host/macos/fence_test.rs`: profile generation, one rule per socket, the
    parameters, scripts' interpreters, developer trampolines, quoting.
  - `native/denials_test.rs`: the kernel's and sandboxd's messages.
  - `native/mod_test.rs`: the route list.
  - `core` event round trip and report finding for `actor.deny`.
  - `config`: `runtime: macos-native` loads.
- `make check` and `make test` passed on the Mac (macOS 27.2): 718 tests, 0
  failed.
- On the Linux KVM host (x86_64), `make check` and `make test` passed on the
  runtime's commit, shipped with `git archive HEAD`: 719 tests, 0 failed.
  There the module is not compiled, and `macos-native` is refused at run
  time.
