# Actor images

**Status**: accepted by the owner (2026-11-12) as four steps. Step 1 (inject
the guest `needy` at start) is implemented and verified on 2026-11-12 (see
[Verification](#verification)); step 2 (custom actor images) on 2026-11-13
(see [Step 2 checks](#step-2-checks)); step 3 (`examples/agents/`) on
2026-11-13 (see [Step 3 checks](#step-3-checks)); step 4 (`needy image`)
on 2026-11-13 (see [Step 4 checks](#step-4-checks)). Breaking API, config and image changes
are allowed; implementers record refinements under
[Decisions during implementation](#decisions-during-implementation).

> **Migration notice (2026-11-13)**: step 4 moved `needy build` to `needy
> image build` (same flags), the base VM image from `images/default` to
> `images/<driver>/base`, and replaced each VM image's `base-abi` and
> `origin.json` files with `manifest.json`. The sections on steps 1 and 2,
> and their checks, keep the names they were written with.

## Why

Today every actor image carries the `needy` binary that runs inside it
(`/.needy/needy`, with `/.needy/init` and `/.needy/proxy` as hard links, and in
a container image every program on `PATH` as a hard link too). So:

- the image must be rebuilt for every needy version, and a prebuilt image
  works only with the needy it was built with;
- a user who wants another program in the actor (pi, Claude Code, Codex,
  herdr) must rebuild needy's whole image from needy's checkout.

The goal: the actor image no longer has to match the needy version, and users
can run any agent in an actor.

## Owner decisions (2026-11-12)

| # | Decision |
| --- | --- |
| D1 | **Inject the guest `needy` at start.** The image is a stable base: the kernel with `binfmt_needy`, the rootfs, a boot stub and a shim stub. The daemon brings its own guest `needy` to every actor it starts: a read-only bind mount of one file for the container drivers, a small read-only disk for the VM drivers. The stubs are tiny static binaries with no needy dependencies; their interface is the **base ABI**, whose version the image records and the daemon checks. |
| D2 | **Custom actor images** are OCI images `FROM ghcr.io/needy-dev/needy-actor` that add tools, for all four drivers; `needy image build --from <OCI ref>` turns one into a VM rootfs. |
| D3 | **`examples/agents/`**: one Dockerfile (node LTS, pi, Claude Code native, Codex musl, herdr) and a profile per agent, with routing, an allowlist, credential copy-in and mirror-out, or `upstream` + `inject`. |
| D4 | **`needy image pull/ls/build/rm`** with per-driver directories `images/<driver>/` holding a `manifest.json` (version, driver, arch, base_abi, source, sha256). `pull` fetches only the base. VM images are not downloaded automatically: a missing one gives the exact command; container images are pulled automatically. Integrity is the sha256 plus `actions/attest-build-provenance`. `needy build` becomes `needy image build`, without an alias. Remote runners pull by hand (a TODO). |

## Terms

| Term | Meaning |
| --- | --- |
| **base image** | An actor image without needy's guest program: the kernel with `binfmt_needy` (VM drivers), the root file system, the boot stub and the shim stub. One base serves every needy version that speaks its base ABI. |
| **guest needy** | The `needy` binary that runs inside an actor as its init, proxy and shim handler: a static Linux build of the daemon's own version, for the actor's architecture. The daemon injects it at `/.needy/needy` when each actor starts. |
| **boot stub** | `/.needy/init`, the actor's PID 1. In a VM it mounts the guest disk and binds the guest needy at `/.needy/needy`; everywhere it then runs `/.needy/needy init`. |
| **shim stub** | `/.needy/proxy`. The kernel's `binfmt_needy` hands a routed exec to it in a VM; in a container every program on the image's `PATH` is a hard link to it. It runs `/.needy/needy` with its own path and the exec's path passed on. |
| **guest disk** | The read-only disk the VM drivers attach as the second virtio-blk device: a SquashFS image with the guest needy, built once per guest binary and cached in needy's data directory. |
| **actor image** | What `actor.image` names, for every driver: an OCI reference; without it, the release's base `ghcr.io/needy-dev/needy-actor:v<version>` (container drivers, pulled when missing) or the base VM image `images/<driver>/base` (VM drivers, `needy image pull`). |
| **custom actor image** | An OCI image `FROM` a base image that adds programs; its last step, `/.needy/shim-image`, makes them shims. The container drivers run it; `needy image build --from` makes a VM image of it. |
| **base ABI** | The interface between a base image and the guest needy plus the daemon: the paths under `/.needy`, the stubs' hand-off, the guest disk, `binfmt_needy`'s parameters and its never-route rule, and the image's records. It has a version; the image records it (a VM image in its `manifest.json`, a container image in its label) and the daemon refuses an image whose version it does not speak. |
| **manifest** | `manifest.json` in a VM image's directory: version, driver, architecture, base ABI, source and the archive's SHA-256 (see [`manifest.json`](#manifestjson)). |

## Step 1: inject the guest needy at start

### The base ABI, version 1

| Part | Version 1 |
| --- | --- |
| `/.needy/init` | the boot stub; the kernel's `init=` (VM drivers) and the container's entrypoint (container drivers) |
| `/.needy/proxy` | the shim stub; `binfmt_needy`'s `proxy` parameter's default; in a container image every program on `PATH` is a hard link to it, and init's route stubs are symbolic links to it |
| `/.needy/needy` | an empty file in the base: the guest needy's mount point. A container gets the guest needy bind-mounted read-only over it; a VM's boot stub binds it from the guest disk |
| `/.needy/disk/` | an empty directory in the base, where a VM's boot stub mounts the guest disk |
| `/.needy/real/<path>` | a container image's real programs, by their original paths (unchanged) |
| `/.needy/channels/` | a container actor's channel sockets (unchanged) |
| `/.needy/base-abi` | the version, as text (`1`), in the root file system |
| The guest disk | `/dev/vdb`, the second virtio-blk device: SquashFS (zlib) with one regular file at its root, `needy`, mode 0755, owned by root |
| Boot stub hand-off | When `/.needy/needy` is empty, mount `/dev/vdb` read-only at `/.needy/disk` and bind `/.needy/disk/needy` over `/.needy/needy`. Then `execve("/.needy/needy", ["/.needy/needy", "init"], <the stub's environment>)` |
| Shim stub hand-off | `execve("/.needy/needy", <the caller's argv, unchanged>, <the caller's environment> + NEEDY_STUB_EXE=<readlink /proc/self/exe> + NEEDY_STUB_EXECFN=<getauxval(AT_EXECFN)>)`. The guest needy removes both variables before it does anything else, so no program it runs sees them |
| Stub failure | When `/.needy/needy` is the empty placeholder (`ENOEXEC`: no guest injected, as in an image build), a shim runs its real program `/.needy/real/<its path>` unrouted, with the caller's argv and environment (since step 2; the guest needy and the daemon see no difference, so the version stays 1). Otherwise the stub prints `needy: ...` with the path and the error on stderr and exits 127. The boot stub as a VM's PID 1 first ends the VM (as init does: a reboot on x86_64, a power-off on arm64), so a broken injection fails the run instead of hanging it |
| `binfmt_needy` | parameters `route`, `route_uid`, `proxy` (unchanged); it never routes an exec made by a process whose executable's path is under `/.needy/` |
| Image records | a VM image directory has a `base-abi` file (`1`); a container image has the label `needy.base-abi=1` |

Why environment variables for the shim stub's hand-off and an argument for
the boot stub's: a shim must keep the caller's argv exactly, `argv[0]`
included (programs such as busybox and `python3` behave by their name), so
the two extra values cannot go in argv; init has no argv of its own to keep,
and `ps` then shows `/.needy/needy init`. `AT_EXECFN` is passed on because
the re-exec replaces it with `/.needy/needy`, and the proxy needs the path
the original `execve` named.

Why `binfmt_needy` changes: it decided "a needy program" by the name of the
executable's parent directory (`.needy`). A bind-mounted file's dentry is the
guest disk's, whose parent is the disk's root, so the guest needy at
`/.needy/needy` would no longer count as needy's, and the proxy's local exec
of a routed program would be routed again. The module now checks the
executable's path (`d_path`) for the `/.needy/` prefix, which holds for the
stubs, a bind-mounted file, and any later layout under `/.needy`.

### The stubs

Two C programs in `images/stub/`, `boot.c` and `shim.c`, built static with
musl in `images/Dockerfile` (`stubs` stage). C, not Rust:

- each is a few dozen lines of system calls (`readlink`, `getauxval`,
  `mount`, `execve`, `reboot`), which C states directly, as `binfmt_needy.c`
  does;
- a static musl C binary is 30 KB (x86_64) to 65 KB (arm64); a static Rust binary with `std` is
  about 300 KB, and a container image has one shim per program on `PATH`
  (hard links in one layer, but a copy each wherever a tool does not keep
  links);
- no Cargo workspace member that builds for Linux only, no second toolchain
  target, and nothing for `cargo test` to compile on macOS and Windows.

`NEEDY_DIR` is a preprocessor value (`-DNEEDY_DIR=...`), `/.needy` by
default, so a test builds the stubs against a directory of its own and runs
them without root.

### Injection by driver

| Driver | How the guest needy gets in |
| --- | --- |
| `linux-container` | a bind `GUEST:/.needy/needy:ro` in the container's binds |
| `macos-apple-container` | `--volume GUEST:/.needy/needy:ro` (a single-file virtiofs mount, which Apple's `container` 1.7.1 supports read-only) |
| `linux-firecracker` | a second drive in `vm.json`: `{"drive_id": "needy", "path_on_host": DISK, "is_root_device": false, "is_read_only": true}` |
| `macos-vz` | a second `--device virtio-blk,path=DISK,readonly` after the root file system's |

The root file system stays read-only in every driver that has it read-only
today.

### Which guest needy

The guest is a static Linux `needy` of the daemon's own version, for the
actor's architecture. Every driver today runs guests of the host's
architecture (x86_64 Firecracker and Docker on Linux, arm64 on Apple
silicon), so the guest's architecture is the daemon's machine's.

The daemon looks, in order, for:

1. `--guest-needy PATH` (control mode);
2. `needy-linux-<arch>` next to its own executable (the macOS release
   archive; `make guest` writes `bin/needy-linux-<arch>` next to `make
   build`'s `bin/needy`);
3. on Linux, its own executable, when it is a static build for the guest's
   architecture (the release binaries are static musl).

Before an actor starts, the daemon checks the binary: an ELF file for Linux
(System V or GNU OS/ABI), of the guest's machine (`EM_X86_64`, `EM_AARCH64`),
with no program interpreter (`PT_INTERP`) and no `DT_NEEDED` entries. A
create fails at once with what was tried and why each was refused, and the
commands that fix it. A given `--guest-needy` is checked when the daemon
starts, so a wrong one fails there; without it, a daemon that finds no guest
logs a warning and still starts, since a daemon may serve executors only.

The VM drivers build the guest disk from the binary: a SquashFS image (zlib,
through the `backhand` crate) named by the first 16 hex digits of the
binary's SHA-256 in `<data>/guest/`, written to a temporary file and renamed,
so concurrent starts share it. A new guest binary gets a new disk; nothing
removes old ones yet (`TODO(guest-disk)`).

### The base ABI check

`needy_core_actor::image::base::ABI` is the version this needy speaks. A VM
driver reads `base-abi` in its image directory, a container driver the
image's `needy.base-abi` label (through the engine, or `container image
inspect`). `check_host`, which a create runs before it acquires anything,
refuses:

- an image without a version: built before the base ABI existed, with its own
  needy inside; rebuild it with `needy build`;
- an image of another version: a newer image needs a newer needy, an older
  one a rebuild.

The guest needy's init also reads `/.needy/base-abi` and stops with a
message on the console when it is not the version it speaks, for a
`--guest-needy` of another needy version.

### Development and release

- `make guest` builds the musl guest binary with Docker (`images/Dockerfile`'s
  `guest` target), on Linux and on macOS alike, into `bin/needy-linux-<arch>`.
- On macOS the release archive gains `needy-linux-aarch64` next to `needy`:
  the `aarch64-unknown-linux-musl` binary the same release workflow builds.
  It is not code-signed or notarized (a Linux ELF that macOS never runs;
  notarization covers the Mach-O `needy`); the archive's `.sha256` covers it,
  and step 4 adds build provenance attestations.
- Linux release archives are unchanged: the static musl `needy` is its own
  guest.

## Step 2: custom actor images

Implemented and verified on 2026-11-13 (see
[Step 2 checks](#step-2-checks)). A **custom actor image** is an OCI image
`FROM` a base image (`needy-actor:latest` from `needy build
--linux-container`, or `ghcr.io/needy-dev/needy-actor:<tag>`) that adds
programs, and every driver runs it:

```dockerfile
FROM needy-actor:latest
RUN apt-get update && apt-get install -y --no-install-recommends jq && rm -rf /var/lib/apt/lists/*
RUN ["/.needy/shim-image"]
```

[`examples/custom-image`](../../../../examples/custom-image/Dockerfile) is
this, with a profile per driver.

### The profile names the image once

`actor.image` is an OCI image reference, the same for every driver; it
replaces `actor.container.image` (removed, without an alias).

| Driver | Without `actor.image` | With `actor.image: REF` |
| --- | --- | --- |
| `linux-container` | `needy-actor:latest` from the engine | `REF` from the engine |
| `macos-apple-container` | `needy-actor:latest` from Apple's store | `REF` from Apple's store |
| `linux-firecracker` | the base VM image (`--actor-image`, default `<data>/images/default`, `needy build`) | the VM image `needy build --from REF` made |
| `macos-vz` | as `linux-firecracker` | as `linux-firecracker` |

A profile names a VM image **by the OCI reference it was made from**: the
one string works for every driver, and switching a profile from
`linux-container` to `linux-firecracker` changes only `driver`. The VM image
lives in `<data>/images/<driver>/<name>-<hash>/`: `<name>` is the reference
with its tag (`needy-agent` is `needy-agent:latest`) and every character
outside `[A-Za-z0-9._-]` written `_`, and `<hash>` the first 12 hex digits of
the tagged reference's SHA-256, so two references never share a directory
(`image::vm::dir_of`). One directory a driver is the layout step 4's
`images/<driver>/` has.

A VM image does not follow its tag: after rebuilding the OCI image, run
`needy build --from REF` again. The directory's `origin.json` records the
reference, the image ID it was made from and its platform, and `profile
check` shows them (`TODO(actor-images)`: step 4's `manifest.json` replaces
`origin.json` and `base-abi`).

### Shims for added programs

The base image keeps the shim step as **`/.needy/shim-image`** (the script
`images/container/shim.sh`, which was only bind-mounted for its one step
before), and a custom image runs it as its last step. Without arguments it
shims Docker's default `PATH`. It may run any number of times: a program
that is a shim already is left alone, and one that replaced a shim (a
package upgrade) becomes the new real program.

For that, the **shim stub** gained a fallback: when `/.needy/needy` is the
base's empty placeholder (`execve` fails with `ENOEXEC`), a shim runs its
real program `/.needy/real/<its path>` with the caller's argv and
environment, unrouted. That is what an image build sees: every `RUN` in an
image `FROM` the base runs `/bin/sh`, a shim, with no guest injected. The
proxy itself (`/.needy/proxy`) stands for no program and still fails with
the stub's message, as does a shim without a real program. A running actor
always has the guest injected, so the fallback never applies there.

Why in the image, not at container start or by `needy build --from`:

- **Immutable and deterministic.** The image that `docker build` (or
  `container build`, or any OCI builder) makes is the image that runs, on
  every engine, with nothing rewritten per start; its digest says what runs.
- **No start cost.** Shimming copies every program on `PATH` (about 500 in
  the base); doing that at each container start is seconds, and needs a
  writable root, which a `linux-container` actor does not have.
- **No second image.** A needy-side step would make a derived image under
  another name, which the profile then has to name instead of the user's.

The cost: an image that forgets the last step runs, but its added programs
are not routed in a container actor (they still are in a VM). `profile
check` keeps its warning that a container actor routes only the programs
its image had when it was built.

### VM images from OCI images

`needy build --from REF` makes the VM image of this host's VM driver
(`linux-firecracker` on Linux, `macos-vz` on Apple silicon) and needs no
needy checkout:

1. The base VM image (`--base DIR`, default `<data>/images/default`) must be
   complete and of this needy's base ABI: its kernel, and Firecracker or
   the initramfs, are the new image's.
2. The engine inspects `REF`, pulling it for the VM's platform when it does
   not have it; it must carry `needy.base-abi` of this needy's version and
   be `linux/amd64` (Firecracker) or `linux/arm64` (`macos-vz`).
3. BuildKit, through the engine's API, builds the Dockerfile embedded in
   needy (`crates/cli/src/commands/build/from.Dockerfile`) with
   `FROM=REF`: it copies the image's file system, puts each real program
   from `/.needy/real` back over its shim (`cp -a --remove-destination`,
   since writing through a shim would change the proxy every shim links to),
   removes `/.needy/real`, and runs `mke2fs -d` sized as the base rootfs is.
   A VM's kernel catches every exec, so it needs no shims.
4. The rootfs, the base's other files, `base-abi` and `origin.json` are
   written to a directory next to the target and renamed into place, so a
   daemon never boots a half-written image.

The kernel and the stubs come from the base: the kernel from the base VM
image, the stubs through the custom image's own `FROM` the base. The image's
configuration (`ENV`, `USER`, `WORKDIR`, `ENTRYPOINT`) does not carry over:
an actor's environment and user are the profile's in every driver, and
needy's boot stub is PID 1. The container drivers start the actor as root
whatever `USER` the image names (`linux-container` did; Apple's `container`
now gets `--user 0:0`).

### Base ABI and architecture checks

| Image | Refused when | Message names |
| --- | --- | --- |
| Container image (both container drivers, `needy build --from`) | `needy.base-abi` missing or another version | the base to rebuild from: `needy-actor:latest` after `needy build --linux-container` (or `--macos-apple-container`), or `ghcr.io/needy-dev/needy-actor:v<this needy's version>`; the base image itself names its `needy build` |
| Container image | its platform is not `linux/` plus the guest's architecture (`amd64` on x86_64, `arm64` on Apple silicon) | the platform to build for (`docker build --platform linux/arm64`); for Apple's store, the image's first variant when it has no `linux/arm64` one |
| VM image from `REF` | missing | `needy build --from REF` |
| VM image from `REF` | `base-abi` of another version | the base to rebuild `REF` from, then `needy build --from REF` |

All are checked when a run is created, before anything is acquired, and
by `profile check`.

### `profile check`

`profile check` prints the actor image as its driver resolves it, with the
base ABI it records when the image is usable (two new fields of
`CheckProfileResponse`, `actor_image` and `actor_base_abi`):

```
actor image: needy-custom-jq:step2 (base ABI 1)
actor image: needy-custom-jq:step2, as the VM image <data>/images/macos-vz/needy-custom-jq_step2-1dceee4bb9cf made from sha256:7dbc… (linux/arm64) (base ABI 1)
```

An unusable image is an error finding with the messages above; the identity
gaps are listed as before.

## Step 3: `examples/agents/`

Implemented and verified on 2026-11-13 (see
[Step 3 checks](#step-3-checks)). The design note said `upstream` +
`inject` as an alternative to credentials in the actor. The schema
redesign removed header injection (`TODO(mitm)`), so the alternative is an
API key in `actor.env`, which also lives in the actor.

[`examples/agents/`](../../../../examples/agents/README.md) holds:

- **One Dockerfile** `FROM needy-actor:latest` (`--build-arg BASE=` names a
  released base). It adds Node.js 26.13.1 (the LTS line), pi 1.5.2, Claude
  Code 2.1.311 (the native build), Codex 0.167.2 (the static musl
  package) and herdr 0.9.5. opencode is in the base already. Each download
  is pinned and checked against the release's checksum, with its source
  and the retrieval date in a comment. The last step is
  `RUN ["/.needy/shim-image"]`.
- **One profile per agent**: `claude.yaml`, `codex.yaml`, `pi.yaml`,
  `opencode.yaml` and `herdr.yaml`. Each one sets `actor.image:
  needy-agents`, a workspace from the `build` executor
  (`executor:build:~/workspace`), rules that send `bash`, `sh`, `dash` and
  `zsh` to that executor, and the agent's API and sign-in hosts in
  `actor.network.allow`. Each one also copies in and mirrors out the
  agent's state directory from `~/needy-state/<agent>/` on the control
  host, sets the agent's update and telemetry switches, and names the API
  key variable in an `actor.env` comment. The profiles use
  `linux-firecracker`. The README shows the short `extends` profile for the
  other three drivers.
- **herdr** runs the other four in panes. Its profile repeats their
  environment, hosts and mappings, because a child's list replaces its
  parent's list. A shell is routed only `when: { under:
  "{claude,codex,pi,opencode} **" }`, so herdr's pane shells stay in the
  actor, where the agents are installed.
- **A README** explains how to build the image for each driver, the
  sign-in flow that works from an actor, where the credentials go and why
  the agent must exit for them to be saved, and how to find missing hosts
  with `needy report`.

## Step 4: `needy image pull/ls/build/rm`

Implemented on 2026-11-13 (see [Step 4 checks](#step-4-checks)). The owner's
step 4 decisions: VM images are never downloaded automatically, a missing
one names the exact command, and container images are pulled automatically
(D1); integrity is the SHA-256 plus `actions/attest-build-provenance`, with
built-in Sigstore verification deferred (D2); `needy build` moves to `needy
image build` with all its flags and no alias (D3); remote runners pull by
hand (D4).

### Commands

| Command | What it does |
| --- | --- |
| `needy image pull [--driver D]... [--version V]` | The base images of release `V` (default: this needy's version) for the drivers this host has (`driver::detect_host`: detection without the image) or those named. A VM driver's: the release asset `needy-actor-<driver>-<arch>.tar.gz`. A container driver's: `ghcr.io/needy-dev/needy-actor:v<V>` through the engine (with `ghcr.io/needy-dev/needy-executor:v<V>`) or `container image pull`. Exit 1 when any driver failed; each failure is one line. |
| `needy image pull --file ARCHIVE [--driver D]` | Imports a VM image archive on this machine (a host without a network); the driver comes from the archive's manifest, and `--driver` must agree. A `ARCHIVE.sha256` next to it is checked when there is one. |
| `needy image ls [-o json]` | The VM images, one row per `images/<driver>/<name>/` (driver, `base` or the OCI reference, version, base ABI, source, size on disk, path), then the needy container images the engine has (a `needy.base-abi` label, and the executor images) and those in Apple's store with the label. |
| `needy image build [...]` | `needy build`, moved whole: `--recipe`, `--out`, `--executor`, `--linux-container`, `--macos-apple-container`, `--from REF`, `--base DIR`. It writes the manifest; the container images it builds are tagged `needy-actor:latest` and `needy-executor:latest`. |
| `needy image rm [--driver D] IMAGE... [--container] [--guest-disks]` | Removes VM images by `base`, OCI reference or directory name (default driver: this host's). `--container` also removes an IMAGE that is an OCI reference from the engine and Apple's store; nothing else touches container images. `--guest-disks` removes every guest disk but the one of the guest needy this needy finds next to itself (`TODO(guest-disk)` of step 1). |

### Where VM images live

`<data>/images/<driver>/` holds one directory per image: `base`
(`image::vm::base_dir`), which `pull` and `build` write and a VM driver
boots without `actor.image`, and `<name>-<hash>` (`image::vm::dir_of`) for
each image `build --from REF` made. The base moved from `images/default`;
`Paths::base_image` picks this host's VM driver's (`image::vm::host_driver`:
`macos-vz` on Apple silicon, else `linux-firecracker`), and the daemon's
`--actor-image` still overrides it.

### `manifest.json`

Each image directory has one (`image::manifest`), and it replaces the
`base-abi` and `origin.json` files, without compatibility:

```json
{
  "version": "0.0.1",
  "driver": "macos-vz",
  "arch": "aarch64",
  "base_abi": 1,
  "source": { "kind": "release", "tag": "v0.0.1", "url": "https://github.com/needy-dev/needy/releases/download/v0.0.1/needy-actor-macos-vz-aarch64.tar.gz" },
  "sha256": "<the archive's SHA-256>"
}
```

| Field | Meaning |
| --- | --- |
| `version` | the needy version whose release (or `needy image build`) made the image; not the version that boots it, which the base ABI decides |
| `driver`, `arch` | `linux-firecracker` and `x86_64`, or `macos-vz` and `aarch64` |
| `base_abi` | the base ABI the image was built for; what the VM drivers check (`image::vm` checks, before every boot and in `profile check`) |
| `source` | `{"kind": "release", "tag", "url"}`, `{"kind": "file", "path"}`, `{"kind": "build", "recipe"?}`, or `{"kind": "oci", "reference", "id", "platform"}` |
| `sha256` | the SHA-256 of the archive the image was unpacked from; absent for an image built here |

Unknown fields are refused. A release archive carries the manifest without
`source` and `sha256`; `pull` adds both. `images/Dockerfile`'s VM targets
still export `base-abi`: `needy image build` and the release workflow fold
it into the manifest and drop the file. The container images keep their
`needy.base-abi` label. `/.needy/base-abi` in the root file system is
unchanged (the guest init's check).

A VM image directory without a manifest (written before this step) is
missing it: `actor image is missing <dir>/manifest.json; run 'needy image
pull --driver macos-vz', or 'needy image build' in a needy checkout`.

### What `pull` verifies

For a VM image, in this order, and nothing is in place until all pass:

1. The `.sha256` asset is downloaded first; a 404 there means the release
   has no such image, which for this needy's own version means a
   development build: the error names `needy image build` and `--version V`.
2. The archive is downloaded (HTTPS, redirects and `HTTPS_PROXY` through
   `reqwest` with rustls) into a hidden file in `images/`, hashed as it is
   written, and compared with the `.sha256` before anything is unpacked.
3. It is unpacked into a staging directory in `images/` (gzip, then tar;
   GNU sparse entries and runs of zeros are written as holes, as
   `needy image build` writes them).
4. Its manifest must exist and name the expected driver (when named), the
   driver's architecture, and this needy's base ABI; the driver's own file
   check must pass.
5. The manifest gets `source` and `sha256`, and the directory is renamed
   into `images/<driver>/base`; an older image is moved aside first and
   removed after.

For a container image: the engine (or Apple's `container`) pulls it, and its
`needy.base-abi` label must be this needy's base ABI.

What it does not verify: the release's build provenance attestation
(`TODO(release-attestation)` in `pull.rs`, owner decision D2). `gh
attestation verify ARCHIVE -R needy-dev/needy` checks it by hand.

**Test hook.** `NEEDY_DOWNLOAD_BASE` replaces the release download URL with
any URL that serves the assets directly, as `install/install.sh`'s does; it
is how `pull` was checked before a release had assets.

### Defaults, and building without a release

| What | Default | Built from a checkout |
| --- | --- | --- |
| A container actor without `actor.image` | `ghcr.io/needy-dev/needy-actor:v<version>` (`profile::CONTAINER_IMAGE`), pulled by the driver when missing | `needy image build --linux-container` (or `--macos-apple-container`) tags `needy-actor:latest` (`BUILT_CONTAINER_IMAGE`); a profile names it with `actor.image: needy-actor:latest` |
| A `linux-container` executor without `image` | `ghcr.io/needy-dev/needy-executor:v<version>` (`linux_container::IMAGE`), pulled when missing (as before) | `needy image build --executor` tags `needy-executor:latest`; the daemon names it with `--executor-image needy-executor:latest` |
| A VM actor without `actor.image` | `images/<driver>/base`, which `needy image pull` writes | `needy image build` writes the same directory |

The container drivers pull in `check_host`, which a create and `profile
check` run before anything is acquired. A pull that fails names the build
for the release's base: `the container actor image
ghcr.io/needy-dev/needy-actor:v0.0.1 is not here and cannot be pulled
(...); a needy without a release (a development build) has none to pull:
build the base with 'needy image build --linux-container' and set
'actor.image: needy-actor:latest'`.

### Messages

| Situation | Message names |
| --- | --- |
| Base VM image missing (daemon, `run create`, `profile check`, runner detection) | `run 'needy image pull --driver D', or 'needy image build' in a needy checkout` |
| Base VM image of another base ABI | `pull this needy's base with 'needy image pull --driver D', or rebuild it with 'needy image build' in a needy checkout` |
| VM image made from `REF` missing | `run 'needy image build --from REF'` |
| VM image of another driver in the directory | the driver it is, and the command above |
| Container base missing and not pullable | the build and `actor.image: needy-actor:latest` (above) |
| Custom container image of another base ABI | the bases to rebuild it from: `needy-actor:latest` after `needy image build --linux-container`, or `ghcr.io/needy-dev/needy-actor:v<version>` |

### Release

- Each VM image job writes `manifest.json` (`jq`, from `Cargo.toml`'s
  version, the matrix's driver and architecture, and the exported
  `base-abi`) and archives it in place of `base-abi`. On a publishing run it
  warns when the tag is not `v<version>`, since `needy image pull` of that
  build looks for `v<version>`.
- A new `attest` job runs after every build job (and `notarize-macos` when
  that runs): it picks the release's archives (the notarized macOS ones
  over the signed ones, as `publish` did), checks every `.sha256`, attests
  the build provenance of every `.tar.gz` and `.zip` with
  `actions/attest-build-provenance@v4`, and uploads them as one artifact,
  `release-assets`, which `publish` uploads unchanged. It attests on a
  publishing run, and on a dispatch only with the new `attest` input:
  attestations go to Sigstore's public log and the repository's
  attestations, so a plain dispatch still publishes nothing.
- Remote runners pull by hand: `TODO(runner-actor)` in
  `crates/daemon/src/capabilities.rs` (D4).

## Decisions during implementation

### Step 1

- **Stubs in C, not Rust** (the reasons are under [The stubs](#the-stubs)).
  They are `images/stub/boot.c` and `shim.c`, built by the Dockerfile's
  `stubs` stage with `musl-gcc -static -Os -s`; the stage fails unless both
  are static. Built that way they are about 30 KB (x86_64) and 65 KB
  (arm64).
- **The hand-off.** The shim stub passes its path and `AT_EXECFN` in
  `NEEDY_STUB_EXE` and `NEEDY_STUB_EXECFN` and keeps argv. It replaces values
  it inherited. The guest needy takes them out of its environment first
  thing in `main` (`guest::Stub::take`), before any thread exists, so
  neither a local program nor a routed command's environment sees them. The
  boot stub passes `init` in argv (`/.needy/needy init`), and init started
  again as the user's program passes `init --as-user ...`. The CLI
  dispatches on these, no longer on its own path (`/proc/self/exe`), which
  is `/.needy/needy` in every role now.
- **`binfmt_needy` checks a path, not a parent name.** `caller_is_needy`
  compared the parent directory's name of the caller's executable with
  `.needy`. A bind-mounted file's dentry is the source file system's, so
  the guest needy would not count as needy's, and the proxy's local exec
  would be routed again. It now compares `d_path` of the executable with
  `/.needy/`. This kernel change is part of base ABI 1.
- **The VM's boot stub binds the guest over the placeholder.** It mounts the
  guest disk at `/.needy/disk` and binds `/.needy/disk/needy` over
  `/.needy/needy`, so the guest needy is at the same path in every driver.
  An empty `/.needy/needy` is how the stub knows it must mount: a container
  has the bind mount already. Without the injection, the boot stub says
  `the guest needy is not injected (... the base image's empty placeholder),
  and cannot mount the guest disk /dev/vdb: ...`, and a shim says `(the
  daemon did not inject the guest needy)` for the placeholder's `ENOEXEC`
  as for `ENOENT`.
- **The guest disk is SquashFS** through `backhand` (zlib only, no default
  features). Both kernels have `CONFIG_SQUASHFS_ZLIB`, it needs no `mke2fs`
  on macOS, and its times are zero, so one binary always makes the same
  disk. A 36 MB x86_64 guest makes a 15 MB disk. Disks are named by the
  first 16 hex digits of the binary's SHA-256 in `<data>/guest/`; old ones
  stay (`TODO(guest-disk)`). The daemon makes one on a blocking thread
  (`driver::guest_disk`) at the first start of a binary.
- **Where the base ABI is recorded.** In three places, all from the
  Dockerfile's `ARG BASE_ABI`, which a test keeps equal to
  `image::base::ABI` (`the_images_record_the_base_abi_this_needy_speaks`):
  `base-abi` next to a VM image's files, the `needy.base-abi` label of
  `needy-actor`, and `/.needy/base-abi` in both root file systems, which the
  guest init checks first.
- **`Engine::has_image` became `image_labels`**, which answers `None` for a
  missing image and the labels otherwise. The executor driver never used
  `has_image`. Apple's `container` has no label query: the driver reads the
  `linux/arm64` variant's `config.config.Labels` from `container image
  inspect`, whose JSON shape is observed (1.7.1), not documented (a
  `NOTICE` says so).
- **Image checks moved to create time.** `linux-firecracker`'s `check_host`
  now checks the image too (files and base ABI), as `macos-vz`'s did, so a
  wrong image fails the create before anything is acquired. `detect` reads
  the base ABI as well, so a runner with an image of another ABI does not
  report `actor(linux-firecracker)`.
- **Which guest, and when.** The guest's architecture is the host's for
  every driver today (a `NOTICE` on `guest::arch`). The daemon looks the
  guest up when a create checks the host and again when the actor starts, so
  an operator can replace it without a restart. A `--guest-needy` that is
  not usable stops the daemon at start; without the flag, a daemon that
  finds none prints a warning and serves executors.
- **The shim step copies its tools aside.** A shim runs nothing until the
  guest needy is injected, so `images/container/shim.sh` copies the tools it
  uses (`readlink`, `dirname`, `mkdir`, `ln`, `cp`, `rm`) to a temporary
  directory before it replaces them, when they live in a directory it
  shims; in its tests, whose root is a directory, nothing is copied (macOS
  kills a copied system binary). The Dockerfile bind-mounts the script for
  its one step, so it does not stay in the image.
- **`make guest`** builds the Dockerfile's new `guest` target with Docker on
  Linux and macOS alike and writes `bin/needy-linux-<arch>`. The VM image
  targets no longer depend on the `needy` stage, so they build without a
  Rust build.
- **Release.** A new `macos-guest` job adds the
  `aarch64-unknown-linux-musl` binary of the same run to
  `needy-aarch64-apple-darwin.tar.gz` as `needy-linux-aarch64` and replaces
  the artifact; `notarize-macos` keeps the file when it repacks. The Intel
  archive gets no guest: no actor driver runs on an Intel Mac. The images
  job checks the container image's label, that a shim without the guest
  fails with the stub's message, and that a shim and the guest needy run
  with it bind-mounted.
- Route stubs (`guest/init/stubs.rs`) are unchanged: in a container they
  are symbolic links to `/.needy/proxy`, now the shim stub, whose
  `/proc/self/exe` is the proxy and whose `AT_EXECFN` is the route stub.

### Step 2

- **`actor.image`, one field for every driver**, replaces
  `actor.container.image` without an alias; `actor.container` keeps
  `oci_runtime` only. A VM driver with `actor.image` boots the VM image made
  from that reference; without it, the daemon's base VM image as before.
- **VM images are named by their OCI reference**, not by a digest or a name
  of their own: the profile is the same for every driver, and no mapping
  file has to be kept. The directory name is readable (the tagged
  reference) and unique (a hash of it). A digest-keyed cache was set aside:
  it needs the engine at every run start to resolve the tag, and a Mac
  running `macos-vz` may have no engine running. Staleness is the cost:
  `origin.json` records the image ID, and a later slice could compare it
  with the tag's (candidates).
- **The shim step ships in the base** as `/.needy/shim-image`, and a custom
  image runs it last (reasons under
  [Shims for added programs](#shims-for-added-programs)).
- **The shim stub runs the real program when no guest is injected.** Without
  it, no `RUN` could work in an image `FROM` the base (its `/bin/sh` is a
  shim). It is `ENOEXEC` on the placeholder only, and the proxy itself never
  falls back, so the release's check that the stub names a missing guest
  now runs `/.needy/proxy`, and `/usr/bin/true` must succeed. Base ABI 1 is
  unchanged: nothing the guest needy or the daemon does differs.
- **The shim step tells a shim by its content, not only its inode.** In an
  image `FROM` the base, the first `ln` of the proxy in the new layer copies
  the proxy up (overlayfs), after which the lower layer's shims no longer
  share its inode; `-ef` alone then took every later shim for a real
  program and copied the proxy over `/.needy/real` (seen on the first try:
  `ls` in the custom image failed). `cmp -s` against the proxy catches both.
  The step also removes a real program before writing it, so an earlier
  run's link is never written through.
- **`needy build --from` embeds its Dockerfile** (`from.Dockerfile`, with
  `include_str!`) and builds it with BuildKit through the engine API, as
  `needy build` does, so it needs no checkout and uses the engine's own
  image store for `FROM` (no export, no unpack, and no `/.dockerenv` or
  mounted `/etc/hosts` that `docker export` would add). It lives in
  `crates/cli/src/commands/build/from.rs` so step 4 can move it to `needy
  image build` whole. Its tool stage is `ubuntu:24.04` with `e2fsprogs`, as
  the base's `actor-image` stage, so both rootfs come from the same
  `mke2fs`.
- **The real programs go back in a VM rootfs** (the open question): the
  kernel catches every exec there, so shims would only add a re-exec per
  program. `cp -a --remove-destination` puts each one in place of its shim.
- **The kernel and Firecracker are copied** into the new VM image's
  directory, so each image is complete by itself (as step 4's per-image
  manifests expect) and can be booted or removed alone. On APFS the copy is
  a clone; on Linux it is about 34 MB per image.
- **`base-abi` of a VM image made with `--from` is this needy's `ABI`**,
  written after the label was checked equal to it.
- **Architecture checks use the guest's architecture**, the same rule as the
  guest needy's (`guest::arch`), mapped to OCI's `GOARCH` names. Docker on
  Linux could run an arm64 image through `binfmt_misc` emulation, but the
  injected guest needy and the shims are of the host's architecture, so it
  is refused rather than tried.
- **Apple's `container` gets `--user 0:0`**, as `linux-container` already
  had `user: 0:0`: a custom image may end with `USER`, and init needs root.
- **A missing custom image** is not built or pulled by needy: the message
  says to build it or pull it (`docker pull`, `container image pull`), and
  `needy build --linux-container` only for the base. Pulling automatically
  is step 4's (`TODO(actor-images)` in the `linux-container` driver).
- **`profile check` shows the image** in two new `CheckProfileResponse`
  fields (`actor_image`, `actor_base_abi`), filled from the driver
  (`Driver::image`) and its `check_host`, which already checks the image.
- **`Engine::image_labels` became `inspect_image`**, which returns the
  image's ID, OS, architecture and labels.

### Step 3

- **pi is `@earendil-works/pi-coding-agent`.** `@mariozechner/pi-coding-agent`
  stopped at 0.73.1 and is deprecated: "please use
  @earendil-works/pi-coding-agent instead". herdr's repository moved from
  `ogulcancelik/herdr` to `herdrdev/herdr`. Both are pinned under their new
  names.
- **Node.js 26 is the LTS line** on 2026-11-13 (`v26.13.1`, "Lithium", in
  `https://nodejs.org/dist/index.json`). Its arm64 build needs
  `libatomic1`, which the base does not have. The image also adds `fd-find`
  (pi accepts `fdfind`, and otherwise downloads fd from GitHub at its first
  start) and `jq`.
- **Claude Code comes from its release bucket, not the installer.** The
  native installer's `claude install` writes into the building user's
  home, which a VM actor hides under its tmpfs. The image downloads the
  binary that the installer downloads,
  `downloads.claude.ai/claude-code-releases/<version>/<platform>/claude`,
  checked against that version's `manifest.json`. The image uses the
  stable channel (2.1.311). The latest channel was 2.1.322.
- **Codex is the release's package, not the bare binary.** The bare
  `codex-<target>.tar.gz` started as a driving program, but in a herdr pane
  it stopped with "this CLI has no complete local package". The image
  unpacks `codex-package-<target>.tar.gz` (the layout of Codex's
  standalone installer) into `/opt/codex` and links `codex` into
  `/usr/local/bin`. That link becomes a shim (step 2).
- **npm's checksum pins pi's own package.** The image checks the tarball
  against the registry's `dist.integrity` (SHA-512) and installs that file
  with `--ignore-scripts`, as pi's own container guide does. npm resolves
  pi's dependencies when the image builds.
- **One self-contained profile per agent, on `linux-firecracker`.** A
  shared parent would hide what each profile sets, and a copied profile
  would lose it. The other drivers are three keys in an `extends` profile
  (README). The host paths use `~/needy-state/<agent>/`, as the main
  README's Files example does, so they resolve the same from any profile
  that extends them.
- **Only shells are routed.** Each agent runs its tool calls through
  `bash`, and everything they start runs under that shell on the
  executor (seen in `needy report` as `bash on build`). `node` is not routed, because pi is a `node` script that must
  stay in the actor. The identity probes stay in the actor, as in
  `examples/opencode`.
- **Updates, telemetry and side downloads are off, not allowed.** For
  Claude Code: `DISABLE_UPDATES`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`
  and `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL` (without them
  the first start asked `downloads.claude.ai` and `github.com`). For Codex:
  `check_for_update_on_startup=false` and `analytics.enabled=false` on the
  command line. For pi: `PI_SKIP_VERSION_CHECK` and `PI_TELEMETRY=0`. For
  opencode: `OPENCODE_DISABLE_AUTOUPDATE`. pi still asks `pi.dev` at the
  start, and its `/oauth` documents serve some sign-ins, so `pi.dev` is
  allowed. herdr's `herdr.dev` checks are refused, and herdr goes on.
- **Codex runs with `--sandbox danger-full-access`.** Its commands leave the
  actor for the executor, so its Linux sandbox would wrap only the actor's
  side of each exec. Approvals stay at Codex's default.
- **`CLAUDE_CONFIG_DIR=/home/agent/.claude`**, so `.claude.json` (onboarding,
  account and the approval of an API key) is in the one mapped directory.
- **Filters are on copy-in** (superseded on 2026-11-13: the daemon now filters mirror-outs, and the filters are back on them). The actor's mirror-out packs its directory
  without the mapping's filter (`mirror_out` in
  `crates/core-actor/src/guest/init/files.rs`: `pack(&root, &[])`). The
  daemon's copy-in applies the filter. So caches and logs reach the host
  but are not copied back in. Each profile says so in a `NOTICE`.
- **pi and opencode map one directory up** (`~/.pi`, `~/.local`; superseded on 2026-11-13: copy-in gives the user the parents it creates). Copy-in
  unpacks with `create_dir_all` and gives the user only the mapping's root
  (`chown_tree`), so `~/.local` and `~/.local/share` stayed root's, and
  opencode stopped at `EACCES: permission denied, mkdir
  '/home/agent/.local/state'`. A `NOTICE` in each profile names the
  removal condition. Both gaps are in the candidates below, and this slice
  does not change Rust.
- **The run must end by the agent's exit to save the login** (superseded on 2026-11-13: `run stop` is graceful). An actor's
  mirror-out runs when the driving program exits. `needy run stop` ends
  the actor first (`stop_run`, whose daemon-side `mirror_out` covers only
  executor sides), so a stopped run's actor files never reach the host.
  This was seen in a Codex run that was stopped before it exited. The
  README says so.

### Step 4

- **The base VM image moved to `images/<driver>/base`**, from
  `images/default`, so every VM image sits in the per-driver layout D4
  names, and `ls` and `rm` treat the base as one image among the others. A
  `base` directory never collides with a `dir_of` name, which ends in a
  12-digit hash. An `images/default` from before is left alone (no
  compatibility); the new layout's missing manifest names the commands.
- **The release archive carries the manifest**, without `source` and
  `sha256`, written by the release workflow from the exported `base-abi`.
  So `--file` needs no driver flag or file-name parsing (the manifest says
  which driver), and an archive of another driver, architecture or base ABI
  is refused before it is put in place. `needy image build` folds the
  exported `base-abi` the same way.
- **`sha256` is absent for a locally built image**: it came from no
  archive, and hashing a 600 MB root file system on every build would only
  describe the build itself.
- **`reqwest` for the download** (0.12, rustls with webpki roots and ring,
  as the workspace already builds): GitHub's release downloads redirect to
  another host, and users may need `HTTPS_PROXY`; the existing `hyper`
  client in the gateway does neither. `flate2` reads the gzip stream; the
  sparse unpacking is `needy image build`'s, which now also writes GNU sparse
  tar entries (the release's `tar --sparse`) as holes.
- **A VM image's driver is checked against its manifest**: a directory
  named by `--actor-image` that holds another driver's image is refused with
  what it holds.
- **The container drivers pull in `check_host`**, so a create pulls before
  it acquires anything, and `profile check` pulls too (it checks the label
  and platform of the image a run would use). A pull that fails names the
  build of the base and `actor.image: needy-actor:latest` for the release's
  base, and `needy image build --linux-container` (or
  `--macos-apple-container`) for `needy-actor:latest`.
- **The executor image's default is the release's too**
  (`ghcr.io/needy-dev/needy-executor:v<version>`): `pull` fetches it, so the
  daemon has to use it; the executor driver already pulled a missing image.
  A development build passes `--executor-image needy-executor:latest`.
- **`pull` for `linux-container` also pulls the executor image**: the
  executor image goes through the same engine, and a host with that engine
  runs `linux-container` executors.
- **`pull` with no `--driver` pulls for every driver `detect_host` finds**:
  a runner's detection without the image check (`linux-firecracker`: Linux
  and `/dev/kvm`; `macos-vz`: Apple silicon and vfkit; the container drivers:
  their engine or Apple's services). Each driver's failure is reported and
  the others still run; the exit code is 1 if any failed.
- **A version without a release is found by the `.sha256` asset's 404**, not
  by guessing from the version string: a development build whose version
  has a release would pull that release's base, which is the right image for
  it when the base ABI matches, and refused when it does not.
- **`rm --guest-disks` keeps the disk of the guest this needy finds**
  (`guest::find` next to its executable, as the daemon looks) and removes
  every other `*.squashfs`; a daemon started with `--guest-needy` elsewhere
  makes its disk again at the next start. `guest::disk_name` was split out
  of `guest::disk` for this.
- **`rm --container`** removes an OCI reference from the engine and Apple's
  store only when asked; plain `rm` never touches container images.
- **Attestations on dispatch only on request** (the `attest` input): an
  attestation is a public record in Sigstore's log and the repository's
  attestation store, which a plain dispatch should not write; the input let
  this step's dispatch check the job without publishing.
- **The checksum check drops `\r`** (`NOTICE(crlf-sha256)`): the Windows
  archive's `.sha256` ends with CRLF, which `sha256sum -c` refused; the check
  had only run on publishing runs before, so this would have failed the
  first release. The `.sha256` itself is unchanged (install.sh and `pull`
  already read the first word).
- **`image ls` lists container images by label**: every engine image with
  `needy.base-abi` (bases, custom images) and the executor images by
  reference, untagged ones left out; in Apple's store, those with the label
  on their arm64 variant.

### Fixes to the gaps found in step 3 (2026-11-13)

The owner approved four fixes from the step 3 candidates. Each one has a
regression test that failed first (or, for the new stop API, a daemon test
with a stand-in Firecracker and a test-played guest).

- **Copy-in gives the user every directory it creates.** `files::place`
  (`crates/core-actor/src/guest/init/files.rs`) first creates each missing
  directory from the user's home down to the mapping (`create_user_dirs`,
  which `working_directory` now uses too), each owned by the user, then
  unpacks. A mapping outside the home is unchanged. `examples/agents` maps
  `~/.pi/agent` and `~/.local/share/opencode` directly. Decision:
  opencode's `~/.local/state/opencode` (recent models, prompt history) is
  no longer kept; a second mapping pair would need a second host
  directory for little value.
- **A mirror-out's filter applies on the control host.**
  `needy_common::archive::unpack` takes the filter and skips an entry that
  a rule excludes, or whose directory a rule excludes, as `pack`'s walk
  does; a leading `./` (`tar -C DIR .`) is not part of the path. The daemon
  passes the mapping's filter for an actor's `PushFiles` and, found while
  reproducing, for an executor's mirror-out too: the brief said executor
  mirror-outs honored it, but `files::mirror_out` unpacked the executor's
  `tar` output whole (the test failed first for both). The guest still
  sends the whole directory; it cannot bypass the filter. The agent
  profiles' filters are back on their mirror-outs.
- **A container actor's process socket belongs to the daemon.**
  `GetConfigResponse` carries `daemon_uid` and `daemon_gid` (the daemon's
  effective IDs); init chowns `process.sock` to them and sets mode 0600
  right after it binds (`bind_for_daemon` in `program.rs`). The chown is an
  error if it fails. A `NOTICE` says the IDs are the host's while the
  engine maps no user namespace. Apple's `container` publishes the socket
  through its relay from inside the VM; the relay still connects (checked).
- **`needy run stop` is graceful.** `Guest.WaitStop` was held by init once
  the driving program started (`init/stop.rs`; since 2026-11-13 the stop is
  a `GracefulStop` message on the guest control stream, `GuestService.Control`,
  see the [profile and rules design](../config/2026-11-09-profile-and-rules-design.md));
  `run stop` answers it with
  the grace period (`StopRunRequest.grace_ms`, default 10 s,
  `--grace DURATION`), init sends SIGTERM to the program's process group
  (through the process host on a terminal, so its exit records the
  signal), SIGKILL after the grace period, then mirrors out and reports the
  exit as usual. The daemon waits for the actor's state to become `Exited`
  (the state is a `watch` now), bounded by the grace period plus
  `MIRROR_OUT_WAIT` (60 s, `Config::mirror_out_wait` for tests), then goes
  on with today's stop, which kills an actor still running. While a stop is
  under way the VM watch leaves mirror-out, release and the end record to
  the stop. `--now` (`StopRunRequest.now`) kills at once. Decisions:
  - **A guest that does not wait is killed at once**, with the reason in
    the trace and on the CLI's stderr: one that has not finished starting,
    or a guest needy older than this change (checked with a guest built
    from the tree before it). The stop never waits for a guest that never
    asks.
  - **`run.stop` records `actor`**: `{"how":"graceful"}` or
    `{"how":"forced","reason":...}`, absent when the stop ended no running
    actor. Schema version 10. The report says `stopped` for such a run
    (the actor's exit, which the stop caused, is recorded first and used to
    make it `finished`), adds a `Stop` line (human), the reason after
    "stopped by" (markdown) and `actor_stop` (JSON).
  - **`StopRunResponse` says how the actor ended** (`graceful`, `reason`),
    so the CLI warns when a stop it did not ask to be `--now` lost the
    mirror-out.
  - **A control plane that shuts down still kills its actors at once**
    (`TODO(graceful-shutdown)` in `Daemon::shutdown`): stopping each
    gracefully would hold a termination signal for up to the grace period
    and the mirror-out wait per run.

## Verification

Step 1, on 2026-11-12, on an Apple silicon Mac (macOS, OrbStack, vfkit
0.6.4, Apple's `container` 1.7.1) and a Linux KVM host (x86_64, Docker
Engine, `runc`). Each piece was checked by itself first, then the whole
once. The scripts were throwaway files in `.tmp/`.

### Each piece

| Piece | Host | Command | Result |
| --- | --- | --- | --- |
| Stubs | Linux KVM host | `cargo test -p needy-core-actor --test stubs` | 8 pass: argv kept (`argv[0]=python3`), `NEEDY_STUB_EXE` and `NEEDY_STUB_EXECFN` set and replaced when inherited, a route stub's link passed as the exec path, `init` passed by the boot stub, the messages for a missing guest and for the placeholder, exit 127 |
| Stubs | both (Mac: in an arm64 Linux container) | the stubs built `-static` against a hand-made `/tmp/l/.needy`, run with and without a stand-in guest | the same hand-off and messages on x86_64 and aarch64 |
| Guest check | both | the check over real binaries: `make guest`'s output, `/bin/ls`, a static C program, the Mach-O `needy`, a script | the musl guests (x86_64 36 MB, aarch64 34 MB) pass; `/bin/ls` is refused naming `/lib64/ld-linux-x86-64.so.2` (`/lib/ld-linux-aarch64.so.1`); Mach-O and the script are "not a 64-bit ELF executable"; a lookup without a guest lists each place tried |
| Container injection | Linux KVM host | `docker run` of `needy-actor` built from the tree, with and without `-v GUEST:/.needy/needy:ro` | label `needy.base-abi=1`; without: `needy: cannot run /usr/bin/true: /.needy/needy: Exec format error (the daemon did not inject the guest needy)`, exit 127, and the boot stub's `the guest needy is not injected ...`; with: `/.needy/needy --version` prints `needy 0.0.1`, `/usr/bin/true` exits 0 and `uname -m` prints `x86_64` through the shim stub, `/proc/mounts` shows the read-only bind, and the boot stub starts `needy init` |
| Container injection | Mac | `container run` of the arm64 `needy-actor` loaded into Apple's store, as above | the same; the guest is a read-only `virtiofs` mount; `uname -m` prints `aarch64` |
| VM injection | Linux KVM host | Firecracker by hand from `needy build`'s base image with the guest disk as drive `needy` | the guest reads `Block:needy`, init runs from the disk and logs `cannot reach the daemon at vsock://2:7000` (no daemon), reboots; Firecracker exits 0 |
| VM injection | Mac | vfkit by hand from `needy build`'s `macos-vz` base image with the guest disk as the second `virtio-blk` | the same; init powers off and vfkit exits 0 |
| Base ABI refusal | Linux KVM host | a daemon on a copy of the VM image with `base-abi` 2; a profile naming a `needy-actor` built with `--build-arg BASE_ABI=2` | `needy run create` fails at once: `the actor image ... has base ABI 2, and this needy speaks base ABI 1; rebuild it with `needy build` ...` (and `needy build --linux-container` for the container), no container left |
| Base ABI refusal | Mac | a daemon on a copy of the `macos-vz` image with `base-abi` 2 | the same message |
| Daemon flag | both | `cargo test` (`a_control_daemon_refuses_a_guest_needy_that_is_not_a_static_linux_build`) and a daemon started without a guest | a bad `--guest-needy` stops the daemon with the check's message; without one the daemon warns |

### End to end

With images built from the step's tree (`needy build --out` for the VM
images; `needy-actor` and `needy-executor` under a scratch tag), the guest
from `make guest` next to `bin/needy`, and each control daemon in a scratch
state directory:

| Driver | Host | Profile | Result |
| --- | --- | --- | --- |
| `linux-firecracker` | Linux KVM host | `examples/smoke` | Pass: GCE identity (`systemd-detect-virt` says `google`), `python3` routed through `binfmt_needy` and the shim stub to a `linux-container` executor, workspace both ways, `routed exit=7` |
| `linux-firecracker` | Linux KVM host | `examples/chaos` | Pass: copy-in without the filtered file, `cat broken.json` fails with EIO, `curl` refused with ECONNREFUSED (126), the hwmon fault at 101 °C, mirror-out written |
| `linux-container` (`runc`) | Linux KVM host | `examples/linux-container` without its workspace mount (no `user_allow_other` on that host, as in earlier slices) | Pass: recipe identity, `python3` routed by the shim stub through the injected guest, `routed exit=7`, report `Actor linux-container (runc)`, no container left |
| `macos-vz` | Mac | `examples/macos-vz` | Pass: arm64 guest, sensors, `python3` and `/usr/bin/arch` by absolute path routed to the Mac (`arm64`), workspace both ways; the guest disk was made in `<data>/guest/` at the first start |
| `macos-vz` | Mac | `-it` with the example, and `-t` with `bash` then `run attach` | Pass: the program's output on the terminal; on attach, PID 1 is `/.needy/needy init`, the guest is root's from the disk, `exit 5` is the run's exit |
| `macos-apple-container` | Mac | `examples/macos-apple-container` with the scratch image | Pass: workspace both ways, `python3` routed, gateway allows `example.com` (200) and denies `example.org` (403) |
| `macos-apple-container` | Mac | `-it` with `bash`, and `-t` then `run attach` | Pass: PID 1 is `/.needy/needy init`, `/.needy/needy` a read-only `virtiofs` mount, `/usr/bin/ls` a hard link to the 66 KB shim stub; exits 4 and 6 returned |

**Version independence.** A second needy, built from the same tree with
the version `0.0.2-b` (its daemon and its guest), ran against the images
the `0.0.1` tree had built, without rebuilding them: on the Linux KVM host
a `linux-firecracker` actor (also `examples/smoke`) and a `linux-container`
actor, on the Mac a `macos-vz` and a `macos-apple-container` actor. Inside
each, `/.needy/needy --version` printed `needy 0.0.2-b`, `/.needy/base-abi`
was `1`, PID 1 was `/.needy/needy init`, and `python3` reached the
executor.

**Not as hoped**, and not caused by this step: with `-it` and the
`macos-vz` example, the run stopped after the first routed `python3` until
the terminal's input ended (the proxy's relay waits for stdin on a
terminal; see the candidates). `make check` and `make test` pass on both
hosts (results in the commit that records this section); a stand-in
Firecracker test failed once on the Linux host with `Text file busy` and
passed in six reruns (candidates).

**CI.** `check.yml` passed on Linux, macOS and Windows:
https://github.com/needy-dev/needy/actions/runs/43250448634 (the first
two runs failed on Windows only, where `clippy` found the daemon's type
split from its `cfg(unix)` and a larger `daemon::Args`; both were fixed).
`release.yml`, dispatched without publishing, built every binary and both
architectures' base images, checked `base-abi` 1, the container image's
label, a shim's message without the guest and a shim and the guest with
it bind-mounted, and packed `needy-linux-aarch64` (static, AArch64) next to
`needy` in `needy-aarch64-apple-darwin.tar.gz`:
https://github.com/needy-dev/needy/actions/runs/43252994149.

### Step 2 checks

On 2026-11-13, on the same two machines: an Apple silicon Mac (macOS,
OrbStack's Docker 29.8, vfkit 0.6.4, Apple's `container` 1.7.1) and a Linux
KVM host (x86_64, Docker Engine 29.7, `runc`). The base images were built
from the step's tree under a scratch tag (`needy-actor:step2`, and
`needy-executor:step2` on Linux), the VM base images with `needy build`
into a scratch data directory, and `examples/custom-image/Dockerfile` with
`--build-arg BASE=needy-actor:step2` as `needy-custom-jq:step2`. Each
control daemon ran with scratch state and data directories, the guest from
`make guest`. The scratch profiles extend the example's, with
`actor.image: needy-custom-jq:step2`.

Each piece, as it was written:

| Piece | Host | Command | Result |
| --- | --- | --- | --- |
| Stub fallback | Linux KVM host | `cargo test -p needy-core-actor --test stubs` | 10 pass: over the empty placeholder a shim runs its real program with the caller's argv (`argv[0]=python3`) and environment, without the stub variables; `/.needy/proxy` and a shim without a real program still fail with the stub's message |
| Stub fallback in the image | Mac | `docker run --entrypoint /usr/bin/true needy-actor:step2`, and `--entrypoint /.needy/proxy` | `true` exits 0; the proxy says `cannot run /.needy/proxy: /.needy/needy: Exec format error (the daemon did not inject the guest needy)`, exit 127; `/.needy/shim-image` is in the image |
| Custom image | Mac | `docker build --build-arg BASE=needy-actor:step2 -t needy-custom-jq:step2 examples/custom-image` | `apt-get install jq` runs through the shims; label `needy.base-abi=1` inherited, `linux/arm64`; `/usr/bin/jq` is the proxy's inode and `/.needy/real/usr/bin/jq` the real jq |
| Shim step run twice | Mac | the first build of the custom image | **failed first**: `ls` in the image said `the daemon did not inject the guest needy`, because the second run had copied the proxy over `/.needy/real/usr/bin/ls` (the overlayfs copy-up under [Step 2 decisions](#step-2)); with `cmp -s` it builds and every program runs |
| Shim step | Mac | `cargo test -p needy-cli shim` | 7 pass, with a second run that shims an added program and keeps the real ones, and a program that replaced a shim becoming the real one |
| VM rootfs | Mac | the `from.Dockerfile` by hand, then `debugfs -R stat` on the result | `/usr/bin/jq` (67512 bytes) and `/usr/bin/ls` (199464 bytes) are the real programs, `/.needy/proxy` has one link, `/.needy/real` is gone |
| `needy build --from` | Mac | `needy build --from needy-custom-jq:step2` | `<data>/images/macos-vz/needy-custom-jq_step2-1dceee4bb9cf/` with `Image`, `initrd` (the base's), `rootfs.ext4`, `base-abi` 1 and `origin.json` (`linux/arm64`, the image ID) |
| `needy build --from` | Linux KVM host | the same | `<data>/images/linux-firecracker/needy-custom-jq_step2-1dceee4bb9cf/` with `vmlinux` and `firecracker` (the base's), `rootfs.ext4`, `base-abi`, `origin.json` (`linux/amd64`) |
| `macos-vz` | Mac | the custom image's `macos-vz.yaml` | `jq` at `/usr/bin/jq` prints `jq` in the actor; `python3` routed to the Mac (`Darwin`), `routed exit=7` |
| `macos-apple-container` | Mac | the image loaded with `docker save` and `container image load`; `macos-apple-container.yaml` plus a rule `jq --version` to the Mac | local `jq` in the actor; `jq --version` routed through its shim to the Mac prints `jq-1.7.1-apple`; `python3` routed; report: `jq on mac 1`, `kept on actor 1` |
| `linux-firecracker` | Linux KVM host | the custom image's `profile.yaml` plus a rule `jq --version` to a native executor | local `jq` in the actor; `jq --version` routed by `binfmt_needy` (report `jq on host 1`); `python3` routed to the `linux-container` executor, `routed exit=7` |
| `linux-container` | Linux KVM host | `linux-container.yaml` without its workspace mount, plus the `jq` rule | local `jq`; `jq --version` routed through its shim (report `jq on host 1`, `kept on actor 1`); `python3` routed, `routed exit=7` |
| Wrong base ABI | both | an image `FROM needy-actor:step2` with `LABEL needy.base-abi=2`: `needy build --from` it, and `profile check` with it on `linux-container` (Linux) and `macos-apple-container` (Mac) | refused: `... has base ABI 2, and this needy speaks base ABI 1; rebuild it FROM a base image of base ABI 1: needy-actor:latest after needy build --linux-container (--macos-apple-container), or ghcr.io/needy-dev/needy-actor:v0.0.1, or use a needy that speaks base ABI 2` |
| No base ABI | both | plain `ubuntu:24.04`, as above | refused: `... records no base ABI: it is not built FROM a needy base image, or was built before needy injected its guest at start; rebuild it FROM ...` |
| Wrong architecture | both | `FROM --platform=linux/amd64 ubuntu:24.04` with the label on the Mac, `linux/arm64` on Linux, as above | refused: `the actor image needy-bad-arch:step2 is linux/amd64, and actors here run linux/arm64, the guest needy's platform; build it for linux/arm64 (docker build --platform linux/arm64)` (and the reverse on Linux) |
| VM image of another base ABI | both | the VM image's `base-abi` set to 2, `profile check` on `macos-vz` / `linux-firecracker` | refused, naming the reference, the directory, the base to rebuild from and `then run needy build --from needy-custom-jq:step2` |
| VM image never built | both | `actor.image: needy-never-built` on a VM driver | `actor image is missing <data>/images/<driver>/needy-never-built_latest-18d11d56fb44/<kernel>; run needy build --from needy-never-built` |
| `profile check` | both | every profile above | prints `actor image: needy-custom-jq:step2 (base ABI 1)` (containers), or the VM image's directory, its image ID and platform `(base ABI 1)`; the identity gaps as before |

`make check` and `make test` pass on both hosts (the Linux host also runs
the stub tests). `check.yml` passed on Linux, macOS and Windows:
https://github.com/needy-dev/needy/actions/runs/43268125985. `release.yml`,
dispatched without publishing, built both architectures' base images and
checked the new image steps (a shim runs `/usr/bin/true` without the guest,
the proxy fails with the stub's message, `/.needy/shim-image` is there):
https://github.com/needy-dev/needy/actions/runs/43268159121 (its
`aarch64-apple-darwin` job was first not picked up by a hosted runner and
passed when re-run).

End to end, from the commits that were pushed, with the base built from
the tree:

| Driver | Host | Profile | Result |
| --- | --- | --- | --- |
| `linux-firecracker` | Linux KVM host | `examples/smoke` (the base VM image) | Pass: GCE identity (`systemd-detect-virt` says `google`), `python3` routed, workspace both ways, `routed exit=7` |
| `linux-container` (`runc`) | Linux KVM host | `examples/linux-container` with `actor.image: needy-actor:step2`, without its workspace mount (no `user_allow_other` on that host, as in step 1) | Pass: recipe identity, `python3` routed, `routed exit=7`; the workspace lines fail for want of the mount |
| `macos-vz` | Mac | `examples/macos-vz` (the base VM image) | Pass: `python3` and `/usr/bin/arch` routed to the Mac (`arm64`), workspace both ways, `routed exit=7` |
| `macos-apple-container` | Mac | `examples/macos-apple-container` with `actor.image: needy-actor:step2` | Pass: workspace both ways, `python3` routed, `example.com 200`, `example.org` refused (403) |
| `linux-firecracker` | Linux KVM host | the custom image (`profile.yaml`) | Pass, as in the table above |
| `linux-container` | Linux KVM host | the custom image (`linux-container.yaml`) | Pass, as above |
| `macos-vz` | Mac | the custom image (`macos-vz.yaml`) | Pass, as above |
| `macos-apple-container` | Mac | the custom image (`macos-apple-container.yaml`) | Pass, as above |

### Step 3 checks

On 2026-11-13, on the same two machines: an Apple silicon Mac (OrbStack's
Docker 29.8, vfkit 0.6.4) and a Linux KVM host (x86_64, Docker Engine,
`runc`). needy was built from the tree after step 4 landed (`make build`,
`make guest`). The base images were built from that tree (`needy image
build --linux-container` on the Mac; on Linux, `needy-actor:step3` and
`needy-executor:step3` from `images/Dockerfile`, and `needy image build`).
The agents image was built as `needy-agents:step3`, and its VM image with
`needy image build --from needy-agents:step3`. Each control daemon ran with
scratch state, runtime, config and data directories. The checks ran each
example through a small scratch profile that `extends` it. These profiles
set the scratch image tag, a scratch credential directory in place of
`~/needy-state/<agent>/`, the driver (with a native executor on a
`/tmp` directory on the Mac), and a **dummy** API key
(`sk-...-needy-dummy-not-a-key`, no real credential). The dummy key lets
each TUI start without a login, so a user's `!` shell command (each
agent's own shell tool) can run, and the provider answers 401. A
throwaway driver ran `needy run create -it` on a pseudo-terminal and
rendered the screen (`pyte`). No account was used, and nothing was logged
in.

Each piece, as it was written:

| Piece | Host | Command | Result |
| --- | --- | --- | --- |
| Image, arm64 | Mac | `docker build -t needy-agents:step3 examples/agents` | each checksum `OK`; in the image `node --version` `v26.13.1`, `pi --version` `1.5.2`, `claude --version` `2.1.311 (Claude Code)`, `codex --version` `codex-cli 0.167.2`, `herdr --version` `herdr 0.9.5`, `opencode --version` `1.19.5`; `/usr/local/bin/claude` has the proxy's inode, and the real programs are in `/.needy/real/usr/local/bin/` |
| Image, amd64 | Linux KVM host | `docker build --build-arg BASE=needy-actor:step3 -t needy-agents:step3 examples/agents`, then the same `--version`s | the same versions, `uname -m` `x86_64` |
| Image, first tries | Mac | the first builds | **failed first**: Node.js's arm64 `node` needs `libatomic.so.1` (added `libatomic1`); pi downloaded fd from GitHub, refused by the gateway (added `fd-find`); the bare Codex binary refused to start in a herdr pane (switched to the package) |
| VM images | both | `needy image build --from needy-agents:step3` | `macos-vz/needy-agents_step3-6a348e749936` (1.6 GiB), `linux-firecracker/needy-agents_step3-6a348e749936`; `profile check` of every profile prints `actor image: needy-agents:step3, as the VM image ... (base ABI 1)` |
| Claude Code sign-in | Mac, `macos-vz` | `claude.yaml` without a key, the subscription choice | the `https://claude.com/cai/oauth/authorize?...` URL with `redirect_uri=https://platform.claude.com/oauth/code/callback` and `Paste code here if prompted >` |
| Codex sign-in | Mac, `macos-vz` | `codex.yaml`, **Sign in with Device Code** | `https://auth.openai.com/codex/device` and a one-time code, from `auth.openai.com` through the gateway |
| pi sign-in | Mac, `macos-vz` | `/login` in `pi.yaml` | the method menu: `Sign in with an account`, `Sign in with an API key` |
| Claude Code hosts | Mac, `macos-vz` | `needy report` of the first runs | denied `downloads.claude.ai` and `github.com`; with `DISABLE_UPDATES` and `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL` none |
| opencode copy-in | Mac, `macos-vz` | `opencode.yaml` mapping `~/.local/share/opencode` | **failed first**: `EACCES: permission denied, mkdir '/home/agent/.local/state'` (copy-in left `~/.local` root's); mapping `~/.local` starts it |
| Login persistence | Mac, `macos-vz` | a run that exited, then a second run | the second Claude Code run skipped the theme, key and onboarding screens; Codex's `auth.json` came back; a Codex run stopped with `needy run stop` before it exited saved nothing |
| Pane routing | Mac, `macos-vz` | `herdr.yaml`; in the first pane `uname -s`, `curl` to every allowed host and to `example.com`, then each agent's `!` command | the pane prints `Linux` (the actor); every allowed host answers (200, or 403/404/421 from the host itself); `example.com` `000` (refused, `network denied: example.com`); `!uname -s` in claude, pi, codex and opencode prints `Darwin` (the Mac executor) |

End to end, from this slice's tree, each agent's profile on one
driver per platform, with fresh scratch credential directories. Each run
did: start, onboarding, `!uname -s; pwd; echo routed-$(uname -s)`, a
prompt `say hello`, and the agent's exit.

| Profile | `macos-vz` (Mac, native executor) | `linux-firecracker` (Linux KVM host, `linux-container` executor) |
| --- | --- | --- |
| `claude.yaml` | Pass: TUI, `routed-Darwin` in `/private/tmp/...`, `Invalid API key` (401 from `api.anthropic.com`); report `bash on build 3`, `sh on build 1`; network `api.anthropic.com`, `platform.claude.com`; no denied host; `.claude.json`, `projects/` and `settings.json` mirrored out | Pass: the same, `routed-Linux`; report `routed to build 4` |
| `codex.yaml` | Pass: TUI after `Detected OPENAI_API_KEY`, `routed-Darwin`, `401 Unauthorized: Incorrect API key provided` from `api.openai.com/v1/responses`; report `bash on build 3`; network `api.openai.com`, `chatgpt.com`; denied `github.com`, `api.github.com`, `raw.githubusercontent.com`, `files.openai.com` (left out); `auth.json` mirrored out | Pass: the same, `routed-Linux` |
| `pi.yaml` | Pass: TUI, `routed-Darwin`, `Error: 401 ... invalid x-api-key`; report `bash on build 1`; network `api.anthropic.com`, `pi.dev`; no denied host; `agent/` mirrored out | Pass: the same, `routed-Linux` |
| `opencode.yaml` | Pass: TUI, `routed-Darwin`, `invalid x-api-key`; report `bash on build 1`; network `registry.npmjs.org`, `api.anthropic.com`, `models.opencode.ai`; no denied host; `share/opencode` and `state/opencode` mirrored out | Pass: the same, `routed-Linux` |
| `herdr.yaml` | Pass: pane `pane-Linux`; `claude-routed-Darwin`, `pi-routed-Darwin`, `codex-routed-Darwin` from agents in a pane; report `routed to build 8`, `kept on actor 7`; denied `herdr.dev`, `example.com` (the probe), `raw.githubusercontent.com` and `ab.chatgpt.com` (Codex in a pane, started without the profile's `analytics.enabled=false`) | Pass: the same pane and routing counts |

**Not as hoped**:

- **`linux-container` with `-it` does not start on the Linux host** (fixed on 2026-11-13), for any
  profile and also for a plain `bash` actor of the base image. The
  container's init creates `channels/process.sock` as root with mode
  0755, and a daemon that is not root cannot connect to it (`the actor is
  not ready: ... does not serve its terminal: transport error`). The
  `linux-container` checks of steps 1 and 2 ran without a terminal. So the
  agent profiles on `linux-container`, and on `macos-apple-container`
  (not tried), are unverified.
- **The authenticated path is unverified.** No check logged in, so the
  token exchange and refresh hosts (`platform.claude.com` for Claude
  Code, `auth.openai.com` for Codex), the subscription model APIs
  (`chatgpt.com` for a ChatGPT plan), a tool call that the model starts,
  and the persistence of a real login are untested. The hosts that a
  logged-in session needs and the profiles leave out are unknown.
- `opencode`'s `/connect` flow, pi's subscription sign-ins and GitHub
  Copilot's device code were not tried.

### Checks of the step 3 fixes

On 2026-11-13, needy built from the fixes' tree on each host natively
(`make build`, `make guest`; the arm64 guest through OrbStack on the Mac),
base VM images from `needy image build` into scratch data directories, and
a scratch control daemon on each host, run as the logged-in user (not
root). The `linux-container` actors used the host's `needy-actor:latest`
(base ABI 1); the `macos-apple-container` actor a copy of the Mac's Docker
`needy-actor:latest` loaded into Apple's store under a scratch tag. The
scratch profile copied a seed directory into `~/.a/b/c`, printed the owner
of each level, wrote `~/out/{keep.txt,cache/blob,debug.log}` with a
mirror-out filter `["- /cache/", "- *.log", "+ **"]`, printed `ready` and
looped with a `TERM` trap; a script waited for `ready` and ran `needy run
stop`. A pseudo-terminal driver typed into `run create -it` of `/bin/sh`.

| Check | `linux-firecracker` (Linux KVM host) | `linux-container` (Linux KVM host) | `macos-vz` (Mac) | `macos-apple-container` (Mac) |
| --- | --- | --- | --- | --- |
| Copy-in to `~/.a/b/c` | `.a`, `.a/b`, `.a/b/c` and the file `agent`'s | the same | the same | the same |
| Mirror-out filter | only `keep.txt` on the host | the same | the same | the same |
| `run stop` (graceful) | `got-TERM` in the console, `keep.txt` on the host, report `stopped, driving program exit 143`, `Stop graceful` | the same, with `-t` | the same, also with `-t --grace 3s` | the same, with `-t` |
| `run stop --now` | nothing on the host, `forced: asked to stop at once` | the same | the same | not run |
| `run create -it` of `/bin/sh` | not run | Pass from a daemon that is not root: prompt, `it-42 agent`, exit 0 (without a workspace mount: `sshfs` is not installed on the host) | Pass | Pass (`actor.ready` in the trace) |
| Guest built from the tree before the fixes (`--guest-needy`) | `run stop` returns at once, stderr and report `forced: the actor's guest was not waiting for a stop: ...`; the console shows `.a` and `.a/b` owned by `root`, the bug 1 reproduction | not run | not run | not run |

### Step 4 checks

On 2026-11-13, on the same two machines: an Apple silicon Mac (macOS,
OrbStack's Docker through `DOCKER_HOST`, vfkit, Apple's `container` 1.7.1)
and a Linux KVM host (x86_64, Docker Engine 29.7, `runc`). No release has
assets yet, so `pull` ran against the VM image archives of the dispatched
`release.yml` run https://github.com/needy-dev/needy/actions/runs/43281065443
(`gh run download`), served by `python3 -m http.server` on loopback through
`NEEDY_DOWNLOAD_BASE`, and through `--file`. Each control daemon ran with
scratch data, state and runtime directories (a short state directory on the
Mac), the guest from `make guest`, and on Linux `--executor-image
needy-executor:latest`.

| Piece | Host | Command | Result |
| --- | --- | --- | --- |
| Unit tests | both | `cargo test` (`image::manifest`, `image::vm`, the drivers, `commands::image::{pull,ls,rm}`) | pass: the manifest round-trips and refuses unknown fields; a missing base names `needy image pull --driver D` then `needy image build`; a directory without a manifest is missing it; `pull` from a loopback HTTP server installs and records release, tag, URL and SHA-256, refuses a wrong `.sha256` and a base of another ABI leaving nothing behind, names `needy image build` and `--version V` for a release without the asset; `--file` takes the driver from the manifest; the linux-container driver pulls a missing image and refuses a pulled one of another ABI |
| Archive | Mac | `tar -tzvf` and `tar -xzOf ... manifest.json` of the run's `needy-actor-macos-vz-aarch64.tar.gz` | `Image`, `initrd`, `rootfs.ext4`, `manifest.json` (`{"version":"0.0.1","driver":"macos-vz","arch":"aarch64","base_abi":1}`); no `base-abi` |
| Missing VM image | both | `needy image rm base`, then `needy profile check examples/macos-vz` (`examples/smoke` on Linux) | `actor image is missing <data>/images/macos-vz/base/Image; run needy image pull --driver macos-vz, or needy image build in a needy checkout` (`linux-firecracker` and `vmlinux` on Linux) |
| Release without the asset | Mac | `needy image pull --driver macos-vz` (no test hook) | exit 1: `release v0.0.1 has no needy-actor-macos-vz-aarch64.tar.gz (https://github.com/.../v0.0.1/...sha256: 404 Not Found). A needy without a release (a development build) has no base to pull: build it ... with needy image build, or pull another release's with --version V` |
| `pull` | Mac | `NEEDY_DOWNLOAD_BASE=http://127.0.0.1:PORT needy image pull --driver macos-vz` | 147 MiB downloaded with a progress line every 32 MiB, `verified ... (sha256 34a9829e...)`, in place in 25 s; manifest with `source.kind release`, tag `v0.0.1`, the URL and the SHA-256; `rootfs.ext4` 549 MiB long, 432 MiB allocated, byte for byte GNU tar's extraction; no staging left in `images/` |
| `pull` | Linux KVM host | the same for `linux-firecracker` | 152 MiB in 8 s; `firecracker` and `vmlinux` keep mode 0755; manifest as above |
| `pull --file` | both | `needy image pull --file needy-actor-<driver>-<arch>.tar.gz` (its `.sha256` beside it) | `verified`, imported into `images/<driver>/base`, `source.kind file`; with `--driver linux-firecracker` on the `macos-vz` archive: `the archive holds a macos-vz image, not a linux-firecracker one` |
| Container base from GHCR | Mac | `needy image pull --driver linux-container`, and `--driver macos-apple-container` | GHCR has no needy images yet: exit 1 with `... ghcr.io/needy-dev/needy-actor:v0.0.1 is not here and cannot be pulled (docker: ... 403 ... denied); a needy without a release (a development build) has none to pull: build the base with needy image build --linux-container and set actor.image: needy-actor:latest`, and for the executor `... start the daemon with --executor-image needy-executor:latest`; Apple's: `(Error: HTTP request to https://ghcr.io/v2/needy-dev/needy-actor/manifests/v0.0.1 failed with response: 401 Unauthorized ...)` with `--macos-apple-container` |
| Driver pull | Linux KVM host | a base pushed to a `registry:2` on loopback and removed from the engine; `profile check` of `examples/linux-container` with `actor.image: 127.0.0.1:PORT/needy-actor:step4` | the driver pulled it (the engine has it afterwards) and printed `actor image: 127.0.0.1:PORT/needy-actor:step4 (base ABI 1)`; a tag the registry lacks: `... is not here and cannot be pulled (docker: ... 404: manifest for ... not found: manifest unknown ...); build it, or check its reference`; the example without `actor.image`: the GHCR message above |
| `needy image build` | Mac | `needy image build` | the `macos-vz` base in `images/macos-vz/base` with `manifest.json` (`source.kind build`), `base-abi` gone |
| `needy image build` | Linux KVM host | `needy image build --linux-container --executor` | the `linux-firecracker` base the same way; `needy-actor:latest`, `needy-executor:latest` |
| `needy image build --from` | both | `docker build --build-arg BASE=needy-actor:latest -t needy-custom-jq:step4 examples/custom-image`, then `needy image build --from needy-custom-jq:step4` | `images/<driver>/needy-custom-jq_step4-63e2e411efe1` with `source.kind oci`, the image ID and platform; `profile check` shows it `made from sha256:... (linux/amd64) (base ABI 1)`; on Linux a run of `examples/custom-image` with it: `jq` local, `python3` routed, `routed exit=7` |
| `ls` | both | `needy image ls`, `-o json` | the VM images with version, base ABI, source, size on disk and path; the engine's labelled images and executor images (untagged ones left out); Apple's store lists none here, its `needy-actor:latest` predates the label |
| `rm` | both | `needy image rm needy-custom-jq:step4` (Mac), `--container` too (both), `needy image rm needy-never-built`, `--guest-disks` with a stand-in `00000000deadbeef.squashfs` next to the daemon's disk | the directory removed; with `--container` also the engine's tag; an unknown image: `no macos-vz VM image needy-never-built`, exit 1; `--guest-disks` removed only the stand-in |
| `make check`, `make test` | both | | pass (Linux also runs the stub tests) |

End to end, with the pulled images:

| Driver | Host | Profile | Result |
| --- | --- | --- | --- |
| `macos-vz` | Mac | `examples/macos-vz` | Pass: the CI-built kernel (`6.18.62 ... Thu Nov 12 18:41:29 UTC 2026 aarch64`), sensors, `python3` and `/usr/bin/arch` routed to the Mac (`arm64`), workspace both ways, `routed exit=7`, exit 0 |
| `linux-firecracker` | Linux KVM host | `examples/smoke` | Pass: GCE identity (`systemd-detect-virt` says `google`), `python3` routed to the `linux-container` executor, workspace both ways, `routed exit=7`, exit 0 |
| `linux-firecracker` | Linux KVM host | `examples/custom-image` with `actor.image: needy-custom-jq:step4` (`--from`, on the pulled base) | Pass, as in the table above |

**CI.** `check.yml` passed on Linux, macOS and Windows for the step's
commits: https://github.com/needy-dev/needy/actions/runs/43281050957.
`release.yml`, dispatched with `attest` and without publishing: the first run
(https://github.com/needy-dev/needy/actions/runs/43281065443) built every
archive with its manifest, and its `attest` job failed at the checksum check
on the Windows `.sha256`, which PowerShell ends with CRLF
(`NOTICE(crlf-sha256)`): the check `publish` had always run, but only on a
publishing run, so no release had reached it. With the check fixed, the second run passed:
https://github.com/needy-dev/needy/actions/runs/43285128375. Its `attest`
job checked every `.sha256` and attested the 7 archives in one attestation
(https://github.com/needy-dev/needy/attestations/66851482, signed through
Sigstore's public-good instance and logged in Rekor). After `gh run download
-n release-assets`, `gh attestation verify
needy-actor-macos-vz-aarch64.tar.gz -R needy-dev/needy` passed (subjects: the
7 archives; signer `.github/workflows/release.yml@refs/heads/main`), a
tampered file was refused (`HTTP 404` for its digest), and `needy image pull`
through the test hook installed that archive. What a dispatch cannot show:
the `publish` job's upload of `release-assets`, the GHCR pushes, and
attestation of notarized archives, which only a publishing run makes; the
first release exercises them.

## Candidate next slices

- **Guest disk cleanup** (`TODO(guest-disk)`): every guest binary leaves a
  15 MB disk in `<data>/guest/`; `needy image rm` (step 4) or the daemon at
  start could remove those of binaries no longer present.
- **`profile check` names the guest needy**: today a create fails with the
  lookup's message, but `profile check` does not say the daemon has no
  usable guest.
- **A routed command on a terminal waits for stdin's end**: with
  `run create -it`, the proxy's relay of a routed command that has exited
  waits until the terminal's input ends (seen with `examples/macos-vz`). The
  relay is unchanged by this step; a test in `core-executor::relay` should
  reproduce it first.
- **Init without channels in a container** loops forever after its reboot
  fails (seen when the base image is run by hand without a daemon); it
  could exit when `reboot` is refused.
- **`Text file busy` in the stand-in Firecracker tests**, done
  (2026-11-13): a test writes a script and runs it while another test's
  thread forks, so the child holds a copy of the write descriptor until its
  exec. Renaming a temporary file does not help (the busy file is the same
  inode), and the forks are in the code under test, so no lock can cover
  them. `needy_common::testing::write_executable` (feature `testing`) writes
  a script from a child process, so no fork of the test process can hold
  it open; every stand-in script that a test then runs is written through it
  (`NOTICE(etxtbsy)`). `a_script_written_while_another_thread_forks_is_never_busy`
  reproduced it on the Linux KVM host (12 of 300 scripts busy with
  `std::fs::write`) and passed 10 times after; the `linux_firecracker`
  tests then passed 200 times and the daemon's 100 times, run side by side.
- **The guest's architecture per driver**, once a driver runs another
  architecture than the host's (`guest::arch` has a `NOTICE`).

Found in step 2:

- **A stale VM image warning**: `origin.json` records the image ID a VM
  image was made from; `profile check` (with an engine at hand) could say
  when `REF` now names another image, so a rebuilt OCI image is not booted
  as its old VM image unnoticed.
- **A forgotten `/.needy/shim-image`**: an image whose added programs are
  not shims still runs; `profile check` could compare the image's last
  history entry, or a label the step sets, and warn for a container actor.
- **`systemd` in the container base**: the VM base has `systemd` for
  `systemd-detect-virt`, the container base does not, so a VM image made
  from a custom image lacks it and `systemd-detect-virt` is missing there
  (the recipe's DMI is still presented). One userland for both bases would
  make a VM from a custom image identical to the base VM.
- **`needy build --from REF --macos-apple-container`**: loading a Docker
  image into Apple's store is `docker save` plus `container image load` by
  hand today; `needy build --macos-apple-container` already does both for
  the base.
- **`container build` of a custom image**: Apple's builder resolving
  `FROM needy-actor:latest` from its own store was not tried; if it works,
  a Mac without Docker can build custom images.
- **Old VM images**: every `needy build --from` of a new reference leaves a
  directory of about 500 MB to 600 MB; `needy image rm` (step 4) removes
  them.

Found in step 3 (the first four were fixed on 2026-11-13, see
[Fixes to the gaps found in step 3](#fixes-to-the-gaps-found-in-step-3-2026-11-13)):

- ~~A `linux-container` actor's terminal with a daemon that is not
  root~~: fixed, the socket belongs to the daemon's user, mode 0600.
- ~~Copy-in gives the user the parents it creates~~: fixed.
- ~~A mapping's filter on the actor's mirror-out~~: fixed on the daemon's
  side, for executor mirror-outs too.
- ~~Mirror-out when a run is stopped~~: fixed, `run stop` is graceful.
- **A graceful shutdown of the control plane**
  (`TODO(graceful-shutdown)`): a shutdown still kills its actors at once,
  so their mirror-outs are lost; a bounded graceful stop per run would keep
  them.
- **Filter in the guest too**: the guest still packs and sends the whole
  directory, which the daemon then filters; sending the filter in
  `GetConfig` would also save the transfer of large caches.
- **Credentials outside the actor** (`TODO(mitm)`): every profile here
  puts a login or a key in the actor, where the agent can read it.
- **A real login per agent**: each profile's authenticated path, with an
  account the owner provides, to find the hosts that a logged-in session
  needs.

Found in step 4:

- **Verify the attestation in `pull`** (`TODO(release-attestation)`, D2):
  `pull` checks the SHA-256 the same release serves; checking the Sigstore
  bundle against `release.yml`'s identity would also catch a replaced
  release asset.
- **`linux-container` on a Mac**, done (2026-11-13): detection took any
  answering engine, so `needy image pull` on a Mac with OrbStack also pulled
  the container base into Docker, which a container actor cannot use there
  (`TODO(container-runtime)`). Decision: `linux-container` actors are
  detected on Linux only (`linux_container::detect`), so a Mac runner no
  longer reports `actor(linux-container)` either; `pull` asks the executor
  drivers' detection separately and pulls the executor image alone where
  `linux-container` executors are possible. On the Mac with OrbStack, `pull`
  now names `macos-vz, macos-apple-container, linux-container executors` and
  pulls `needy-executor` only. Test:
  `a_host_that_is_not_linux_has_no_linux_container_actors_whatever_its_engine`.
- **A broken pipe panics the CLI**, done (2026-11-13): `needy run ls |
  grep -q ...` ended with `failed printing to stdout: Broken pipe` (Rust's
  `println!`). Commands now print through `output::out!` and `outln!`, which
  return the write's error, and `cli::run` ends a command whose error is a
  local `BrokenPipe` quietly with 141 (128 + SIGPIPE, what a shell reports
  for a program SIGPIPE ended). `#![warn(clippy::print_stdout)]` keeps
  `print!` out of the CLI. Test:
  `a_closed_stdout_ends_a_command_quietly_with_141`.
- **Apple's `container` and a plain-HTTP registry**: the driver's `container
  image pull` passes no `--scheme`, so a local registry was tested through
  the engine only.
- **Tag and version**: a release whose tag is not `v<Cargo version>` only
  warns, and its binaries pull from `v<Cargo version>`; making it fail would
  need the owner's word.

### For steps 2 to 4

- **Step 2 (custom images), done.** What follows was the note to it; see
  [Step 2](#step-2-custom-actor-images) for what was built. An image `FROM needy-actor` adds programs that
  are not shims. The shim step must run again over them: ship
  `images/container/shim.sh` in the base (it is bind-mounted for the build
  today and does not stay) or let `needy image build` run it. It copies its
  tools aside before it replaces them, since a shim needs the injected
  guest. `/.needy` (stubs, empty `needy`, `disk/`, `base-abi`) comes from
  the base and must not be changed by a custom image. For a VM rootfs made
  from a container image, the shims are harmless (a shim with no route file
  runs its real program) but cost a re-exec per program; restoring
  `/.needy/real` over the shims would remove that.
- **Step 3 (agent examples).** Agents installed with npm or as single
  binaries under `/usr/local/bin` are shimmed by the shim step if they are
  there before it runs; programs they download at run time are not routed
  in container actors (a known gap).
  After step 2: one Dockerfile `FROM needy-actor:latest` whose last step is
  `RUN ["/.needy/shim-image"]` (installers that write into `~` or
  `/opt` and link into `/usr/local/bin` are covered: a link to a program
  outside the shimmed directories becomes a shim); each agent's profile
  sets `actor.image` to it and works on all four drivers, with `needy build
  --from` once per VM host. `ENV` and `USER` of the image do not reach the
  actor: put what the agent needs in `actor.env` and `user`.
- **Step 4 (`needy image`), done**; see
  [Step 4](#step-4-needy-image-pulllsbuildrm). The note to it was:
  `manifest.json`'s `base_abi` replaces the
  `base-abi` file; `image::base::check_dir` is where the daemon reads it.
  The container images keep the `needy.base-abi` label, which the
  manifest's value must equal. `pull` downloads the release's base archives
  (no needy inside now) and `needy-linux-aarch64` stays in the macOS
  `needy` archive. `rm` is where old guest disks can go too.
  After step 2: `needy build --from` (`crates/cli/src/commands/build/from.rs`)
  moves to `needy image build` whole; its `origin.json` (reference, image
  ID, platform) and `base-abi` fold into `manifest.json` (`source` is the
  reference and image ID, `arch` the platform). The directories already
  follow `images/<driver>/<name>-<hash>/` (`image::vm::dir_of`); the base
  is still `images/default` and may move under its driver too. `ls` lists
  `images/*/*`; `rm REF` removes `dir_of(REF)`. A missing container image
  is the place to pull (`TODO(actor-images)` in the `linux-container`
  driver), and `CONTAINER_IMAGE` the place to switch the default to
  `ghcr.io/needy-dev/needy-actor:v<version>`.
