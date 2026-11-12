# `macos-vz` actor runtime: evidence

> **Migration notice (2026-11-13)**: `needy build` is now `needy image
> build`, with the same flags, and a VM image records its base ABI in
> `manifest.json` instead of a `base-abi` file (step 4 of the
> [actor images design](2026-11-12-actor-images-design.md)). This note keeps the command it was
> written with.

> **NOTICE (2026-11-12, migration):** the
> [drivers design](2026-11-12-drivers-and-extension-points-design.md) renamed what this note describes, without
> compatibility: `actor.runtime` is `actor.driver` (`firecracker` is
> `linux-firecracker`, `container` is `linux-container`); an executor's
> `provider` is its `driver` (`docker` is `linux-container`) and its `docker:`
> options are `container:`; the `ActorRuntime` trait is the actor `Driver`,
> the executor `Provider` is the executor `Driver`; capabilities read
> `actor(linux-firecracker) executor(linux-container,native)`. The text below
> keeps the names of its time, and its links to `examples/container` point to
> what is now `examples/linux-container`. `crates/core-actor/src/host/` is
> gone: its files are in `driver/` (one directory per driver), `contract.rs`,
> `image/`, `identity.rs` and `sidecar/gateway/`; `core-executor`'s
> `provider/` is `driver/` (`docker.rs` is `linux_container/`).

**Status**: implemented and verified end to end on one Mac, 2026-11-12. The
runtime is named `macos-vz` (see [Name](#name)).

NOTICE: the checks below ran under the runtime's first name, `vz` (profile
value, `actor(vz)`, the `actor-vz` image target, `examples/vz`). The owner
renamed it to `macos-vz` the same day, and this note uses the new name
throughout, also in recorded commands and output; nothing else in them
changed.

Related notes:

- [Container actor runtime](2026-11-10-container-actor-runtime-design.md): the
  `ActorRuntime` trait and the runtimes table this note adds a row to.
- [Closed-loop results](2026-11-09-closed-loop-results-evidence.md): the
  Firecracker and container actors' evidence, which these checks mirror.

Evidence levels used below: **[V-test]** seen in a run on the host named,
**[V-unit]** asserted by a unit test, **[V-code]** read in upstream source at
the cited revision, **[I]** inferred and not checked.

## What it is

An actor runtime for Apple silicon Macs that is the Firecracker actor's
counterpart: needy's own arm64 guest kernel and rootfs in a
Virtualization.framework VM, with vfkit as the VM process, as Firecracker is
on Linux.

- `actor: { runtime: macos-vz }` in a profile (`crates/config`), `actor(macos-vz)` as a
  runner capability (`ACTOR_RUNTIME_MACOS_VZ` in `proto/needy/v1/runner.proto`).
- `needy_core_actor::host::macos::vz`: `Vz` implements `ActorRuntime`; `Launch`
  builds vfkit's arguments from a compiled recipe; `Vm` owns the vfkit
  process.
- The guest is the Firecracker actor's in all that needy does inside it:
  needy's init is PID 1 and powers the VM off at the end, `binfmt_needy`
  routes execs, fake coretemp gives sensors and the hwmon fault, the
  workspace is mounted with sshfs in the guest (`Setup::Guest`).
- Channels: the guest API (vsock port 7000) and the gateway (7001) are Unix
  sockets the daemon serves in the run's directory (`guest.sock`,
  `gateway.sock`), which vfkit connects guest connections to; the process
  service (7002) is `process.sock`, which vfkit serves and connects to the
  guest's port (`Terminal::Unix`). The guest's init needed no change for
  them: it dials the same vsock ports as under Firecracker.
- No network device: everything goes through the gateway
  (`TODO(actor-network)` at `macos::vz::Launch::new`, as at `firecracker::Launch`).
- `needy build` on an Apple silicon Mac builds the `actor-macos-vz` target of
  `images/Dockerfile` for `linux/arm64`: `Image` (the uncompressed arm64
  kernel), `initrd` (an empty initramfs), `rootfs.ext4` and the guest's
  `needy`.

## Choices

### vfkit as the VM process

vfkit (crc-org, v0.6.4 from Homebrew) is the VM process, looked up on `PATH`.
AGENTS.md prefers a maintained tool, and vfkit already does what a helper of
needy's own would:

- It carries `com.apple.security.virtualization` with ad-hoc signing
  (`codesign -d --entitlements -` on the Homebrew binary shows it) **[V-test]**,
  so needy's own binary needs no entitlement and no signing step.
- `--device virtio-vsock,port=P,socketURL=PATH` bridges a vsock port to a
  Unix socket in both directions (default: the guest connects, vfkit dials
  `PATH`; `connect`: vfkit listens on `PATH` and dials the guest port)
  **[V-code]** (`pkg/vf/vsock.go` at v0.6.4, `connectVsock` L39, `listenVsock`
  L77) **[V-test]**.
- `virtio-blk` with `readonly`, `virtio-serial` to a file, `virtio-rng` and a
  pidfile are flags.

What vfkit costs, each marked in the code:

- It always hands the Linux boot loader an initrd path, so the image carries
  an empty initramfs (`NOTICE` in `images/Dockerfile`, stage `macos-vz-initrd`).
- It opens a serial log file without appending, which would write over its
  own messages in the same file, so the console goes to `/dev/stdout`, which
  is `console.log` opened for appending (`NOTICE` in `macos::vz::Launch::new`).
- It splits `--device` values at commas, so a run directory with a comma in
  its path cannot be given (`NOTICE` in `macos::vz::Launch::new`).

A helper of needy's own (Swift, or `objc2-virtualization` inside `needy`,
which would then need the entitlement) is worth it only for something vfkit
cannot do; nothing in this slice needed one. Candidates are listed under
[Follow-up candidates](#follow-up-candidates).

### Kernel configuration

The arm64 kernel is Linux 6.18.62 (the x86 kernel's `KERNEL_VERSION`), built
in the same `kernel` stage, which picks its base by `uname -m`:

- Base: Apple Containerization 0.54.0's `kernel/config-arm64`, a copy at
  `images/kernel/containerization-arm64.config`. It is made for
  Virtualization.framework guests (virtio over PCI, GICv3, PSCI, `hvc0`),
  and Containerization builds it for 6.18.5 with `olddefconfig`. Firecracker's
  arm64 CI config was not used: it targets Firecracker's virtio-mmio machine,
  not Virtualization.framework's **[I]**.
- `needy.config` on top (the same fragment as x86: `HWMON`, `THERMAL_HWMON`,
  `FUSE_FS`, `TIME_NS`, `VIRTIO_VSOCKETS`), then `needy-arm64.config`, which
  pins what the `macos-vz` runtime depends on (virtio-blk root, `hvc0` console).
- `binfmt_needy.c` and `fake_coretemp.c` are built in unchanged; neither has
  x86-only code. Provenance for both config files is in
  `images/kernel/needy-arm64.config`.

### Where it is built

On the Mac, in Docker (OrbStack 7.2.12, arm64 builder), natively: the first
`actor-macos-vz` build took about 9 minutes (the kernel step 357 s on 18 cores), a
rebuild with the kernel cached about 4 minutes, most of it the guest `needy`.
Cross-compiling on the x86_64 Linux KVM host was not tried: it would need an
arm64 cross toolchain for the kernel, a cross musl target for `needy`, and an
arm64 userland for the rootfs, which the Mac's builder has natively **[I]**.

### Ending the VM

Init ended a Firecracker VM by rebooting (`reboot=k`). On arm64 a reboot is
PSCI `SYSTEM_RESET`, and Virtualization.framework answers it by restarting the
guest: init booted again and again until vfkit was killed **[V-test]**. Init
now powers off on arm64 (PSCI `SYSTEM_OFF`), which stops the VM and ends vfkit
(exit 0 within 0.3 s of the driving program's end) **[V-test]**. For the same
reason the kernel command line has no `panic=`: a panicked guest that rebooted
would boot init again and could run the driving program twice, so it hangs
until the run is stopped (`NOTICE` and `TODO(macos-vz-panic)` at `macos::vz::BOOT_ARGS`).

## Name

`macos-vz`, chosen by the owner on 2026-11-12. `vz` is what the macOS
virtualization ecosystem calls this backend (Lima's `vmType: vz`, the Go
binding `Code-Hex/vz` that vfkit uses); the `macos-` prefix groups it with the
other macOS runtimes (`macos-apple-container`, and the experimental
`macos-native`). It names the hypervisor framework, not the VM process, so a
helper of needy's own could replace vfkit without renaming profiles.

## Checks

Host: an Apple silicon laptop (M5 Max, 18 cores), macOS 27.2, vfkit 0.6.4
(Homebrew), Apple `container` 1.7.1, OrbStack 7.2.12 as Docker. The control
daemon ran as the logged-in user with its state under the repository's `.tmp`
and `--actor-image` pointing at the built image. The executor is the Mac
itself (`provider: native`, see [Gaps](#gaps)).

### `container -k` with the arm64 needy kernel

`container run --rm -k Image alpine:3.22 sh check.sh`, where the script sets
`binfmt_needy`'s parameters (`proxy` to a stand-in script under
`/tmp/.needy/`, `route_uid` 0, `route` `uname,ls`) and runs commands
**[V-test]**:

| Probe | Result |
| --- | --- |
| `uname -srm` before routing | `Linux 6.18.62 aarch64` |
| `/sys/module/binfmt_needy/parameters` | `proxy`, `route`, `route_uid` |
| `uname -a` (by name) | `ROUTED argv0=/tmp/.needy/proxy args=-a` |
| `/bin/ls /` (by absolute path; a busybox link) | `ROUTED argv0=/tmp/.needy/proxy args=/` |
| `echo kept \| cat` (not listed) | `kept` |
| `/sys/class/hwmon/hwmon0` | `coretemp`, `38000` |
| Wall time of the whole `container run` | 1.5 s |

Apple `container`'s guest root could write the parameters without
`--cap-add` (sysfs is writable to root in its containers). A parameter written
with `echo` carries a newline, which `binfmt_needy` takes as part of the value
(the first try routed to `/tmp/.needy/proxy\n` and failed with `not found`);
init writes them without one.

### End to end through `needy run create`

Profiles: [`examples/macos-vz/profile.yaml`](../../../../examples/macos-vz/profile.yaml)
(smoke) and [`examples/macos-vz/chaos.yaml`](../../../../examples/macos-vz/chaos.yaml),
both with the GCE recipe of `examples/opencode`. All **[V-test]**:

| Check | Result |
| --- | --- |
| Smoke: kernel, hostname, user, CPUs, memory | `Linux instance-… 6.18.62 … aarch64`, the recipe's generated hostname, `uid=1000(agent)`, 2 CPUs, 3924 MiB total of the recipe's 4096 |
| Smoke: sensors, `systemd-detect-virt`, `/proc/cpuinfo` | `Package id 0: +38.0°C`, `vm-other`, `CPU implementer : 0x61` |
| Smoke: routed `python3` on the Mac, workspace both ways over sshfs, exit code | `Darwin True`, `hello-from-actor`, `hi-from-executor`, `routed exit=7` |
| Smoke: `/usr/bin/arch` by absolute path, routed by `binfmt_needy` | `arm64` (the Mac's answer; the guest's `arch` says `aarch64`), exit 0 |
| Chaos: copy-in with a filter, file EIO on the mount, exec fault | `note.txt` only, `Input/output error` on `broken.json`, `curl exit=126` |
| Chaos: hwmon fault at 2 s, applied by the guest's init | `+38.0°C`, then `+101.0°C ALARM (CRIT)` |
| Chaos: mirror-out | `out/<run>/result.txt` |
| `run create -it` bash: input, `stty size`, exit | `inside-42`, `30 100`, `aarch64`, exit 3; prompt 0.53 s after the command started |
| `run create -t`, `run attach` at 41x101, `stty size`, detach (`ctrl-]`), attach again, exit | `41 101`; first attach exits 0 detached, run `running`; second attach exit 5, run `finished` with 5 |
| `run stop` of a `-t` run | vfkit gone, sockets removed |
| Control daemon killed with `SIGKILL` during a run, then started again | `ending its actor's VM (macos-vz PID)`, the run `failed` (`the control plane restarted; the actor was lost`), vfkit gone |
| `profile check examples/macos-vz/profile.yaml` | warnings: x86 recipe on arm64, `cpuid`, `cpuinfo`, `dmi`, `pci` (see [Identity](#identity)) |
| Report | `Actor  macos-vz`, 5 routed to `mac` (1 failed: the exit 7) |
| `needy build --out DIR` | `Image`, `initrd`, `rootfs.ext4`, `needy` |

### Time to ready

From `run.create` to `actor.config` (the guest's init fetched its
configuration over vsock: kernel boot, init, and the first call through
vfkit), for ten runs in a row of a profile without a recipe whose driving
program is `/bin/true`, from the trace's timestamps **[V-test]**:

| | ms |
| --- | --- |
| `run.create` to `actor.start` (vfkit spawned) | 1 to 17 |
| `run.create` to `actor.config` | 234, 244, 254, 258, 273, 274, 288, 318, 353, 363 (median 274) |
| `run.create` to the run's last event | 236 to 406 |

Firecracker's ready time on the Linux KVM host is 160 to 190 ms, and an Apple
`container run` is 0.6 to 0.75 s. The daemon was a debug build.

### Tests

**[V-unit]**:

- `core-actor` `host::macos::vz`: vfkit's arguments for a plain VM (read-only rootfs,
  the three vsock bridges, the process socket in `connect` mode, no
  `virtio-net`), sizing from a recipe, the channels and ports, a missing
  kernel, the identity gaps of an x86 recipe, a host without vfkit, and the
  stop of a VM that ignores `SIGTERM`.
- `daemon`: `actor(macos-vz)` detection, a leftover vfkit ended on restart by its
  pidfile, and a terminal that a relay accepted and closed is not taken as
  served (the regression of `run create -it` failing with `transport error`).
- `config`: `runtime: macos-vz` loads and refuses `container:` options.

`make check` and `make test` pass on the Mac (713 tests) and on the Linux
KVM host (732 tests; the tree shipped with `git archive`). One earlier Linux
run failed `a_daemon_starts_over_stale_sockets_nobody_listens_on` (`ps`
against the new daemon); it passed three times alone and in the next full
run, and touches nothing of this slice, so it is recorded as flaky under
load, not fixed here. Followed up on 2026-11-13 on the same host: the test
ran 2,000 times, 8 at a time beside up to four concurrent full suites, and
48 full suites ran 4 at a time, without a failure; reading the daemon's
start, the stale-socket check and `ps`'s path found no race. The cause is
still unknown, so the test now prints `ps`'s stderr when it fails.

## Identity

What a `macos-vz` actor presents of a recipe, next to Firecracker:

| Field | Firecracker | `macos-vz` |
| --- | --- | --- |
| Hostname (`hostname`) | yes | yes (init) **[V-test]** |
| `/proc/cpuinfo` (`cpuinfo`) | yes | no: arm64's format, Apple implementer `0x61` **[V-test]** |
| `CPUID` / ID registers (`cpuid`) | yes (patched Firecracker) | no: Virtualization.framework has no API for MIDR **[V-code]** (`VZGenericPlatformConfiguration`, macOS 27.2 SDK) |
| DMI (`dmi`) | yes | no: no SMBIOS API, and the Linux boot loader starts the kernel with a device tree; `systemd-detect-virt` says `vm-other` **[V-test]** |
| PCI IDs (`pci`) | yes | no: Virtualization.framework's virtio devices |
| hwmon sensors and fault (`hwmon`) | yes | yes: fake coretemp, readings follow load, fault applied by init **[V-test]** |
| Kernel (`kernel`) | recipe kernel | recipe kernel: the same `kernel` stage and build arguments (`needy build --recipe`) **[I]**, not run |
| Uptime offset (`uptime`) | yes | `CONFIG_TIME_NS` is set and init is unchanged **[I]**; the checks' recipe has no offset, so not run |
| Architecture | x86_64 | arm64: every needy recipe is an x86 machine, so `profile check` warns for any recipe |

`profile check` and `run create` report each gap through
`ActorRuntime::identity_gaps`; a recipe that `required:`s one fails the
create. x86 programs through Rosetta are not part of this slice.

## Gaps

Each is marked in the code:

- `TODO(macos-vz-identity)` at `macos::vz::gaps`: no arm64 recipes and no recipe
  architecture; a generated arm64 `/proc/cpuinfo` and a fake DMI module are
  possible in the guest but not approved.
- `TODO(macos-vz-panic)` at `macos::vz::BOOT_ARGS`: a panicked guest hangs until the run
  is stopped; Virtualization.framework has no pvpanic device.
- `TODO(actor-network)` at `macos::vz::Launch::new`: no NIC, as for Firecracker.
- `NOTICE` at `macos::vz::VFKIT`: vfkit is found on the daemon's `PATH`; there is no
  flag for another path.
- A control daemon that is not root cannot use a Docker executor: the Docker
  provider gives the executor's socket directory to the profile's user and
  refuses otherwise (`share_with_container` in
  `crates/core-executor/src/provider/docker.rs`). This is not specific to
  `macos-vz`; the checks used the Mac as a native executor instead. Not changed here.
- The kernel recipe identity and the uptime offset were not run on arm64.

## Found on the way

All fixed in this slice:

- **vfkit needs an initrd** (`invalid initial RAM disk path`): an empty
  initramfs, after which the kernel mounts `root=` as without one.
- **A guest reboot restarts the VM** under Virtualization.framework: init
  powers off on arm64.
- **vfkit accepts on `process.sock` before the guest serves**, then closes
  the connection, and `run create -it` failed with `transport error`. The
  daemon now takes the terminal only once a call through it answers (the
  process service answers the health check with `Unimplemented`; the closed
  relay fails it with `Unknown`, not `Unavailable`).
- **`KVM_GET_SUPPORTED_CPUID` is x86-only** in `kvm-ioctls`, which broke the
  arm64 build of `needy`; the KVM CPUID probe is now x86_64-only.
- **A restarted daemon looked for `vm.json`** on a leftover VM's command line;
  for `macos-vz` it looks for vfkit's pidfile.

## Follow-up candidates

- An arm64 recipe (an Ampere or Graviton cloud VM), so a `macos-vz` actor can be
  faithful instead of warning on every recipe.
- A generated `/proc/cpuinfo` bind-mounted by init (CPU count, model name
  text where arm64 shows one), as the container runtime does.
- A fake DMI module in the guest kernel, like fake coretemp, for
  `/sys/class/dmi/id`.
- Rosetta for x86 programs (`VZLinuxRosettaDirectoryShare`, vfkit's
  `rosetta` device) and its order against `binfmt_needy`.
- `--vfkit PATH` (or a daemon config field) if vfkit outside `PATH` matters.
- Docker executors from a daemon that is not root, for a Mac.

## Cleanup

Every run's vfkit ended (`pgrep vfkit` empty); the `container -k` probes ran
with `--rm`. The experiment daemon was stopped by its recorded PID; its state,
the built images and the `needy-executor:vz` image stay under `.tmp/vz-actor`
and in OrbStack for the next run. `/tmp/needy-macos-vz-ws`, the examples' native
workspace, was removed. vfkit stays installed from Homebrew.
