# Actor images

**Status**: accepted by the owner (2026-11-12) as four steps. Step 1 (inject
the guest `needy` at start) is being implemented; steps 2 to 4 are designed
here and wait for their own slice. Breaking API, config and image changes
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
| Stub failure | The stub prints `needy: ...` with the path and the error on stderr and exits 127. The boot stub as a VM's PID 1 first ends the VM (as init does: a reboot on x86_64, a power-off on arm64), so a broken injection fails the run instead of hanging it |
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
- a static musl C binary is about 20 KB; a static Rust binary with `std` is
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

- An actor image is an OCI image `FROM ghcr.io/needy-dev/needy-actor` that
  adds tools. Its last step must re-run the shim step for the container
  drivers, since programs it adds are not shims; the base image ships
  `/.needy/shim.sh` (or an equivalent `needy image build` step) for that.
- `needy image build --from <OCI ref>` turns such an image into a VM rootfs
  (`rootfs.ext4`) next to the base kernel, for `linux-firecracker` and
  `macos-vz`; the kernel and the stubs stay the base's. The image's
  `needy.base-abi` label carries over to the VM image's `base-abi`.
- Open question: whether the VM rootfs drops the container's shims
  (`/.needy/real`), which a VM does not use since its kernel captures.

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

(Filled in as step 1 lands.)

## Verification

(Filled in as step 1 lands: each piece by itself on each platform, then the
full end-to-end run once.)

## Candidate next slices

(Filled in as step 1 lands.)
