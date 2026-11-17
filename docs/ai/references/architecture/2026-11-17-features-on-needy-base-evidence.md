# Slice 0: official Dev Container features on needy's base

Evidence for slice 0 of the
[images and run data spec](2026-11-17-images-and-run-data-spec.md#slices).
Recorded 2026-11-17. Hand-run probes, no product code; not a support claim.

## Question

Do official Dev Container features run as they are on needy's base, and does
a needy actor find and use what they installed?

## Setup

- Machine: an Apple silicon Mac (arm64, macOS 27.2), Docker through OrbStack
  29.8.2.
- needy at `05e63fb` (`main` with trampolines). Container base built from
  that tree: `docker build -f images/Dockerfile --target actor-container`.
  It has user `agent` (1000) and, after the trampolines change, only `bash`,
  `dash` (and `sh` through its link) shimmed in place; `ldd` at its own path.
- Features `common-utils` 2.7.1, `git` 1.3.8, `node` 2.1.2, `python` 1.8.1
  from the source tree of
  [`devcontainers/features@b6877129`](https://github.com/devcontainers/features/tree/b6877129e0a264b8e83a63e8b93de29077132e5b/src)
  (the published artifacts are packed from it), every option at its default.
- Run as the Dev Container tooling does, one feature at a time in
  `installsAfter` order, each in a container started from the image the
  previous one committed: the feature copied to a scratch directory, a
  `devcontainer-features.env` with every option (names sanitized as the
  features spec says), `install.sh` executed as root with `_REMOTE_USER=agent`
  and `_REMOTE_USER_HOME`, `_CONTAINER_USER`, `_CONTAINER_USER_HOME` set.
  `docker diff` per step.
- The result turned into a `macos-vz` VM image (`needy image build --from`,
  over a base VM image the e2e harness had built from the tree; a
  `needy image pull` of the v0.0.6 base failed mid-download with a TLS
  close from the release host and was not retried) and booted as an actor
  with a native executor.

## Results: installing

| Feature | Exit | Time | Changed entries | Notes |
| --- | --- | --- | --- | --- |
| `common-utils` | 0 | 26 s | 8158 | zsh and Oh My Zsh for root and `agent`; packages upgraded; `/usr/bin` 133 entries |
| `git` | 0 | 1 s | 0 | "Detected existing system install": the base has git, and `os-provided` keeps it |
| `node` | 0 | 41 s | 11811 | nvm and node LTS 26.13.1 under `/usr/local/share/nvm`, group `nvm`; build dependencies in `/usr/include`, `/usr/lib` |
| `python` | 0 | 81 s | 18141 | `os-provided` Python 3.12.3 from apt plus pipx; groups `python`, `pipx` |

- **The user existing before the first layer matters (A3).** All 1813 files
  written into `/home/agent` belong to `agent`; `/usr/local/share/nvm` is
  `agent:nvm`, setgid; `agent` was added to groups `nvm`, `python`, `pipx`.
- **A feature can install a shell the shim step has not seen.**
  `common-utils` installed `/usr/bin/zsh` as a real program; `bash` and
  `dash` stayed in-place shims only because `upgradePackages` found nothing
  newer for them. In a container actor, `/bin/zsh` by its absolute path
  would not be routed until the shim step runs again, which the spec does
  on every container materialization.
- No file capabilities were set (`getcap -r /` found none), so the layer
  format's lack of extended attributes did not matter for these four.
- Hosts reached, from the logs: `ports.ubuntu.com` (303 fetches),
  `github.com`, `nodejs.org`. The scripts name many more that these defaults
  did not use (`api.github.com`, `raw.githubusercontent.com`, keyservers,
  `ppa.launchpad.net`, `www.python.org`, `yarnpkg.com`, …), which an
  allowlist would have had to guess (B1 keeps builds open and traced).

## Results: in a `macos-vz` actor

| Check | Result |
| --- | --- |
| `node`, `npm` on the `PATH` needy sets today | **not found** |
| the same with the features' `containerEnv` `PATH` prepended | found; `node` v26.13.1 runs |
| `python3`, `pipx`, `git`, `zsh` | found (`python3` and `pipx` from apt in `/usr/bin`) |
| `npm prefix -g`, `node`'s `process.execPath` | `/usr/local/share/nvm/versions/node/v26.13.1` and its `bin/node`: its own path (no shims in a VM) |
| writing `/usr/local/share/nvm` or `/usr` as the run's user | refused: read-only file system |
| the run user's supplementary groups | **none**, although `/etc/group` lists `agent` in `nvm`, `python`, `pipx` |

## What it shows

1. Official toolchain features run unchanged on needy's base: all four
   succeeded with their defaults.
2. The `PATH` gap the trampolines design predicted is real: a feature's
   programs outside the system `PATH` are not found by the driving program
   today. The spec's decision holds: the image's `PATH` (with `containerEnv`
   applied) is the driving program's and the one trampolines are made over.
3. Creating the profile's user before the first layer (A3) is necessary:
   features give files and groups to it.
4. The shim step must run again after layers that install shells; the
   spec's "on every container materialization" covers it.
5. A run cannot install globally today (read-only root); slice 1's overlay
   root with a disposable upper layer is what makes `npm install -g` work in
   a run.
6. **A bug independent of images:** needy starts the driving program with no
   supplementary groups. Init clears them explicitly
   (`setgroups(&[])` in `crates/core-actor/src/guest/init/program.rs`), and
   the driving program is started through `Command::uid`/`gid`, which also
   leaves them empty. Every image that puts the user in extra groups loses
   them at run time; features rely on it (`nvm` is a group-writable setgid
   directory). Fixed separately, with its own reproduction test, in
   [#23](https://github.com/needy-dev/needy/pull/23): init now gives the
   driving program the groups whose member lists name the user.

## Not checked

- A container actor (`linux-container` needs a Linux host;
  `macos-apple-container` needs Apple's `container` services, not started
  for this probe): whether trampolines miss the features' programs, as point
  2 implies they do for a container actor's routing.
- The features from their published OCI artifacts (the source tree at the
  same commit was used).
- `linux-firecracker`.
- Features beyond these four.
