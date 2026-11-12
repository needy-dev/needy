# Actor images

**Status**: accepted by the owner (2026-11-12) as four steps. Step 1 (inject
the guest `needy` at start) is implemented and verified on 2026-11-12 (see
[Verification](#verification)); step 2 (custom actor images) on 2026-11-13
(see [Step 2 checks](#step-2-checks)); steps 3 and 4 are designed here and
wait for their own slice. Breaking API, config and image changes
are allowed; implementers record refinements under
[Decisions during implementation](#decisions-during-implementation).

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
| **actor image** | What `actor.image` names, for every driver: an OCI reference; without it, the base `needy-actor:latest` (container drivers) or the base VM image (VM drivers). |
| **custom actor image** | An OCI image `FROM` a base image that adds programs; its last step, `/.needy/shim-image`, makes them shims. The container drivers run it; `needy build --from` makes a VM image of it. |
| **base ABI** | The interface between a base image and the guest needy plus the daemon: the paths under `/.needy`, the stubs' hand-off, the guest disk, `binfmt_needy`'s parameters and its never-route rule, and the image's records. It has a version; the image records it and the daemon refuses an image whose version it does not speak. |

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

One Dockerfile (node LTS, pi, Claude Code native, Codex musl, herdr) on top
of the base, and a profile per agent with routing, an allowlist, credential
copy-in and mirror-out, or `upstream` + `inject` so the actor never holds
the key. Built with step 2's `needy image build`.

## Step 4: `needy image pull/ls/build/rm`

- Per-driver directories `images/<driver>/` in the data directory, each with
  `manifest.json`: `version`, `driver`, `arch`, `base_abi`, `source`,
  `sha256`. Step 1's `base-abi` file becomes the manifest's `base_abi`; the
  daemon reads the manifest instead.
- `pull` fetches only the base (no guest needy in it since step 1). VM images
  are not downloaded automatically: a missing one is an error with the exact
  `needy image pull` command. Container images are pulled automatically.
- Integrity: the release's `.sha256` plus `actions/attest-build-provenance`.
- `needy build` becomes `needy image build`, without an alias.
- Remote runners pull by hand (`TODO(runner-actor)`).

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
the stub tests).

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
- **`Text file busy` in the stand-in Firecracker tests**: a test writes a
  script and runs it while another test's thread forks, so the child holds
  the write descriptor; writing the stand-ins once per test binary, or
  retrying `ETXTBSY` in the test helper, would remove the flake.
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
- **Step 4 (`needy image`).** `manifest.json`'s `base_abi` replaces the
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
