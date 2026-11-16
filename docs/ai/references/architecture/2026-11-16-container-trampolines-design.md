# Container capture: trampolines on PATH, shells in place

Status: **approved** by the owner on 2026-11-16 (direction: "trampolines as
pixi does, with the shells kept in place"); implemented on
`feat/container-trampolines`, verified on the Mac drivers (see
[results](#results-2026-11-16)), not yet on Linux hosts. It changes the capture of the container
drivers (`linux-container`, `macos-apple-container`) described in the
[container actor runtime design](2026-11-10-container-actor-runtime-design.md#exec-routing-with-build-time-shims);
the VM drivers' `binfmt_needy` capture is unchanged.

## Why

The shim step (`images/container/shim.sh`) replaces every program on the
image's `PATH` in place with a hard link to the shim stub and moves the real
program to `/.needy/real/<path>`. The real program therefore always runs
from `/.needy/real`: in an image build (the stub's fallback,
`images/stub/shim.c` L100-L104) and in a running container actor (the guest
needy, `crates/core-actor/src/guest/capture/shim.rs`). Two kinds of program
break, both seen on 2026-11-16 while adding Claude Code to an image `FROM`
`needy-agents`
([merge spike evidence](2026-11-16-agent-state-merge-spike-evidence.md#also-observed)):

- **A program that finds its files from its own path** finds
  `/.needy/real/...`: Node.js's `process.execPath` is
  `/.needy/real/usr/local/bin/node`, so `npm prefix -g` is
  `/.needy/real/usr/local` and `npm install -g` installs outside `PATH`.
- **A tool that reads a program file** reads the stub: npm-install-checks
  8.0.0 (`lib/current-env.js` L17-L25) reads `/usr/bin/ldd` as text to tell
  glibc from musl, finds neither, and npm refuses glibc-only packages.

In-place shims were chosen so that absolute paths (`/usr/bin/python3`) and
`#!` interpreters (`#!/bin/sh`) are routed without runtime bind mounts.
That still matters: pi's bash tool runs `/bin/bash` by its absolute path
before it looks on `PATH`
([`packages/coding-agent/src/utils/shell.ts` L128-L133 at `v1.5.2`](https://github.com/earendil-works/pi/blob/v1.5.2/packages/coding-agent/src/utils/shell.ts#L128-L133)),
and `examples/agents` routes every shell to an executor.

## How others do it

Tool managers dispatch through a directory of their own on `PATH` and leave
the real programs where they are. pixi's trampolines are one small binary,
hard-linked once per name into `$PIXI_HOME/bin`, which reads a JSON file
named after the link and executes the real program at its original path
([`docs/global_tools/trampolines.md`](https://github.com/prefix-dev/pixi/blob/9355b58b80ff7bbedd6fb003e81e6f58b09c452d/docs/global_tools/trampolines.md),
[`trampoline/src/main.rs` L87-L135](https://github.com/prefix-dev/pixi/blob/9355b58b80ff7bbedd6fb003e81e6f58b09c452d/trampoline/src/main.rs#L87-L135),
pixi `9355b58b`). rustup, mise, asdf and proto work the same way. They
select versions, so a call by absolute path that bypasses them costs
nothing; needy routes, so it would.

## Design

Two placements of the same shim stub:

| Placement | Which programs | Where | Made |
| --- | --- | --- | --- |
| **Trampoline** | every program on the system `PATH` that is not an in-place shim | `/run/needy/shims/<name>` (tmpfs), first on the driving program's `PATH` | by init at each start, before routing is turned on |
| **In-place shim** | the shells: `sh`, `bash`, `dash`, `zsh` by default | at the program's own path, the real one at `/.needy/real/<path>` | by the shim step when the image is built |

- **Trampolines.** Init copies `/.needy/proxy` into the trampoline directory
  once (a hard link cannot cross from the root file system to the tmpfs)
  and hard-links one name per program it finds in the system `PATH`
  directories, first match in `PATH` order, as a lookup would. A name whose
  program is an in-place shim gets no trampoline, so the lookup reaches the
  shim itself. The stub hands over its path as today; the guest needy
  finds the real program by the trampoline's name on the system `PATH` and
  executes it **at its own path**. Hundreds of links on a tmpfs take
  milliseconds; nothing is copied, and the root file system stays
  read-only.
- **In-place shims** stay for the programs that are called by absolute path
  or named by `#!` lines. `NEEDY_SHIM_NAMES` (space-separated) replaces the
  default list for an image that needs more (`/usr/bin/python3` called by
  its full path, say). The shim step converges: a listed program becomes a
  shim, and an in-place shim whose name is no longer listed gets its real
  program back at its own path.
- **What the executor is told.** A trampoline is only reached through a
  `PATH` lookup, so a routed exec is sent with the bare name the caller
  used (`python3`), as a route stub's is, never with the trampoline's path.
  This changes what a container actor reports for a program it has: `git`
  called by name was `/usr/bin/git` in the decision, the trace and the
  executor's argv, and is `git` now, which the executor finds on its own
  `PATH` (on a Mac, perhaps `/opt/homebrew/bin/git`). A VM actor still
  reports the path the kernel resolved.
- **Matching** is unchanged: the names are the called path, its file name,
  the program's own file name at the end of a link chain (`python3.12`),
  and `argv[0]`.
- **`#!/usr/bin/env python3`** is covered: `env` is not a shim, and it finds
  `python3` on `PATH`, which is the trampoline.

What it fixes: every program that is not an in-place shim runs from its own
path and is its own file, so self-location (Node.js, npm) and file reads
(`ldd`) behave as without needy. Programs added to an image without the
shim step are routed too, since trampolines are made at start, so a custom
image's `RUN ["/.needy/shim-image"]` is needed only to keep its shells
shimmed (after a package upgrade replaced one, say).

What it leaves out:

- A program other than the listed shells called by its absolute path
  (`/usr/bin/python3`) is not routed in a container actor. It is in a VM
  actor. `profile check` and the report say so.
- The in-place shells keep the old behavior: `$BASH` is
  `/.needy/real/usr/bin/bash`, and a tool that reads `/bin/sh` reads the
  stub.
- `TODO(container-routing)`: a program installed into the system `PATH`
  while the actor runs gets no trampoline. It rarely can be: a
  `linux-container` actor's root file system is read-only
  (`readonly_rootfs` in `crates/core-actor/src/driver/linux_container/mod.rs`),
  the driving program runs as the profile's user, not root, and an install
  that a rule routes lands on the executor, not in the actor. Left out
  until a profile installs into the system `PATH` in the actor itself;
  `macos-apple-container`'s root is writable only by a workaround its
  driver names.

## Compatibility

- **Base ABI:** unchanged. The paths under `/.needy`, the stubs and their
  hand-off are the same; the trampoline directory is the guest needy's
  own, under `/run`.
- **Images built with an earlier shim step** (every program in place, such
  as `ghcr.io/needy-dev/needy-actor:v0.0.6` and images `FROM` it) keep
  working as they did: every program is an in-place shim, so init makes no
  trampolines. They keep the two bugs until they are rebuilt from a base
  built by this change.
- **VM images** are unchanged: `needy image build --from` puts every real
  program back over its shim, in-place or not.

## Verification

- Unit: the shim step's default list and its convergence
  (`crates/cli/src/commands/image/build_test.rs`), trampoline creation and
  the real program behind a trampoline (`crates/core-actor/src/guest/capture/shim_test.rs`),
  the routed command's name (`crates/core-actor/src/guest/proxy_test.rs`).
- End to end: the `routing` scenario checks that a local program runs from
  its own path (`readlink /proc/self/exe`) and that `/usr/bin/ldd` is the
  real file, on every driver.

### Results, 2026-11-16

On an Apple silicon Mac (macOS 27.2, OrbStack 29.8.2, Apple `container`
1.7.1), with the harness building every image from this branch:

- Unit: `cargo test --workspace` on macOS (1109 passed); the guest's tests,
  which build on Linux only, in a `rust:1` container on arm64 (164 passed,
  among them the 8 trampoline tests); clippy clean on both. The two shim
  step reproductions failed before the change (`node` and an unlisted shim
  were the stub).
- `cargo xtask e2e` for `macos-apple-container/**` and `macos-vz/**`: 47
  passed, 8 not covered (no ssh, Cloudflare, Windows runner or remote
  client), 6 failed:
  - `tui/codex` on both drivers: the harness's direct baseline started
    `/.needy/real/usr/local/bin/codex`, which an unshimmed program no
    longer has. Fixed in `xtask/src/e2e/tui.rs` and `xtask/tui/direct.sh`;
    both cells pass since.
  - `runner/at=local,transport=direct` and `transport=idle-proxy` on both
    drivers: cleanup's `needy run stop` answers `no such run`. The same cell
    fails the same way on `main` (`macos-apple-container`, direct), so it
    predates this change; a follow-up of its own.
- The new invariants catch the bug: one rerun reused an Apple `container`
  image a build of `main` had left under the harness's tag, and
  `macos-apple-container/routing` failed with `self-path`
  `/.needy/real/usr/bin/readlink` and no "GNU C Library" line in
  `/usr/bin/ldd`; after the image was rebuilt from this branch it passed
  with `/usr/bin/readlink` and 4 lines, as `macos-vz/routing` does.

Not run here: the `linux-container` and `linux-firecracker` cells, which
need a Linux host (`cargo xtask e2e --remote`).
