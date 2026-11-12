# Container actor runtime

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

**Status**: design, 2026-11-10; slices 1 to 4 implemented 2026-11-11, see
[As built: slice 1](#as-built-slice-1) and
[As built: slices 2 to 4](#as-built-slices-2-to-4); the `macos-apple-container`
runtime implemented 2026-11-12, see
[As built: macos-apple-container](#as-built-macos-apple-container). The owner's decisions
are under [Decisions](#decisions). Evidence:
[Closed-loop results, container actors](2026-11-09-closed-loop-results-evidence.md#container-actors)
and [Apple `container` actor evidence](2026-11-12-macos-apple-container-actor-evidence.md).

Related notes:

- [Actor and executor separation](2026-11-09-actor-executor-separation-design.md)
- [Module layout redesign](2026-11-09-module-layout-redesign.md)
- [Closed-loop results](2026-11-09-closed-loop-results-evidence.md)

## Why

The actor runs only in a Firecracker microVM, which needs `/dev/kvm`. Many
cloud VMs do not expose hardware virtualization and their providers will not
enable nested virtualization. A research pass on 2026-11-10 found that the only
way to keep the microVM there is PVM, which needs an out-of-tree host kernel
module, a guest kernel rebased onto the PVM tree, and still cannot fake the
`CPUID` instruction for user space on AMD hosts. That is too much for the goal.

This note adds a second, simpler actor runtime: an OCI container, run with
`runc` (namespaces, weak isolation) or `runsc` (gVisor, a user-space kernel,
stronger isolation, no KVM needed with its default `systrap` platform). Both
are OCI runtimes, so one implementation covers both.

## Scope

In scope: an actor runtime abstraction, a `container` runtime next to the
`firecracker` one, exec routing without a kernel module, the guest API and
gateway over Unix sockets instead of vsock, workspace mounts, and an honest
statement of which identity fields each runtime provides.

Out of scope: PVM, peer pods (one cloud VM per actor), QEMU TCG, UML. They are
recorded in the research notes; none is planned.

## Runtime abstraction

`core-actor::host` gets an `ActorRuntime` trait with `start`, `stop` and `wait`,
owned by the run like the `Vm` handle today. Implementations:

| Runtime | Backend | Needs |
| --- | --- | --- |
| `firecracker` | `host::firecracker` (was `host::vm`) | `/dev/kvm` |
| `container` | Docker API through `bollard`, `HostConfig.runtime` = `runc` or `runsc` | Linux, Docker, `sshfs` for workspace mounts, lxcfs for resource views (`runc`); gVisor's `runsc` registered as a Docker runtime with `--host-uds=all` for `runsc` |
| `container` on macOS and Windows | Docker API through `bollard` against Docker Desktop, whose Linux containers run inside a VM (WSL 2 on Windows) | Docker Desktop |
| `macos-apple-container` | Apple's `container` CLI (each container is a lightweight Linux VM on Apple silicon), see [As built: macos-apple-container](#as-built-macos-apple-container) | macOS on Apple silicon with Apple's `container` installed and its services running; Docker (Docker Desktop or OrbStack) to build the image |
| `macos-vz` | `host::macos::vz`: needy's arm64 kernel and rootfs in a Virtualization.framework VM, vfkit as the VM process; the Firecracker actor's guest (init as PID 1, `binfmt_needy`, sshfs inside, vsock channels bridged to Unix sockets). Built 2026-11-12, see [macos-vz actor evidence](2026-11-12-macos-vz-actor-evidence.md) | macOS on Apple silicon, vfkit, the `macos-vz` actor image (`needy build`) |
| `wsl`, `hyper-v` | later: a WSL distribution or a Hyper-V VM as the actor on Windows (`TODO(windows-actor)`) | Windows |

containerd directly (without Docker) is a later provider, like the executor
`TODO(provider)`.

Profile:

```yaml
actor:
  runtime: container        # required: firecracker | container | macos-vz | macos-apple-container
  container:                # required with `container`, optional with `macos-apple-container`, refused with `firecracker` and `macos-vz`
    oci_runtime: runsc      # required with `container`: runc | runsc; refused with `macos-apple-container`
    image: needy-actor:latest   # the default; `needy build --container` (Docker) or `--macos-apple-container` builds it
```

## What changes per mechanism

| Mechanism | Firecracker today | Container runtime |
| --- | --- | --- |
| Guest API (config, events, exit) | vsock to the daemon | a transport the runtime provides (see Guest transport): a host Unix socket bind-mounted at `/.needy/daemon.sock` for Linux containers; loopback TCP with a per-run token for VM-backed runtimes |
| Gateway | vsock port, forwarded to local TCP by init | Unix socket bind-mounted; init's forwarder bridges local TCP to it. The container has no network except loopback (`network_mode: none`) |
| Init | PID 1 in the VM, mounts `/proc` and friends, reboots at the end | PID 1 in the container (`/.needy/init`), skips the mounts the runtime already made, exits instead of rebooting |
| Exec routing | `binfmt_needy` kernel module, route names as a module parameter | Shims, see below |
| Workspace mount | `sshfs` in the guest over an exec stream | `sshfs` on the host side over the same exec stream, bind-mounted into the container; the SFTP fault tap is unchanged |
| Copy-in, mirror-out | in the guest | unchanged (they run in the actor through init) |
| Logs | serial console | container stdout and stderr |

### Guest transport

The guest API and the gateway are endpoints the runtime provides, not a fixed
socket type. Docker Desktop (macOS, Windows) and Apple's `container` run Linux
inside a VM, and a host Unix socket generally cannot be bind-mounted through
the VM's file sharing. So each runtime returns its transport:

| Runtime | Guest API and gateway transport |
| --- | --- |
| `firecracker` | vsock (host-side Unix sockets of the VM) |
| `container` on Linux | host Unix sockets bind-mounted into the container |
| `container` on Docker Desktop | a TCP port on the host's loopback, reached from the container as `host.docker.internal`, authenticated by a per-run token |
| `macos-apple-container` | host Unix sockets (settled 2026-11-12): the guest and gateway sockets mounted into the container one file each, which the CLI relays over vsock; the process socket published from the container (`--publish-socket`). No loopback TCP |

### Exec routing with build-time shims

Without a kernel module, routing uses shims, the way toolchain managers such as
[proto](https://moonrepo.dev/docs/proto) dispatch tools: every program is a
link to one dispatcher that reads `argv[0]`. needy builds the container actor
image, so the programs in it are known when the image is built.

- The last stage of the container actor image walks every directory on the
  image's `PATH`, moves each executable to `/.needy/real/<original path>`, and
  puts a link to the needy proxy at the original path. `/usr/bin/python3` and
  `python3` both reach the shim, so absolute paths are covered without runtime
  bind mounts.
- The proxy finds the program it stands for from `argv[0]`. For a name on the
  run's route list (or for every name when the list is `*`) it asks the
  daemon with the process ancestry, as it does today, and runs the command
  through an exec stream or executes the real program. For any other name it
  executes `/.needy/real/<path>` directly, without asking.
- A script with a shebang is covered through its interpreter: running
  `./build.sh` makes the kernel execute `/bin/sh`, which is a shim.
- The route-everything rule (`*`) therefore works for every program that was in
  the image, with one daemon round trip per exec, as under Firecracker.
- Init and the proxy call `/.needy/real/...` paths themselves, so needy's own
  helpers are never routed, matching the rule the guest kernel enforces today.

Not covered, and stated by `profile check` and the report for the container
runtime: executables that appear after the image was built and are executed
directly, such as native binaries installed at run time (`pip`, `npm`) or
compiled in the workspace. `TODO(container-routing)`: a ptrace tracer that
stops only at `execve` (seccomp `SECCOMP_RET_TRACE`, as PRoot does) could cover
them under `runc`; deferred until a profile needs it.

### Identity

Filled from the runs of 2026-11-11 and, for `macos-apple-container`, of 2026-11-12
(see the evidence linked above); a recipe field name in parentheses is what
`required:` takes.

| Field | Firecracker | `runc` | `runsc` (gVisor) | `macos-apple-container` (default kernel) |
| --- | --- | --- | --- | --- |
| Hostname (`hostname`) | yes | yes: Docker's `hostname` (UTS namespace) | yes, the same | yes: init sets it (`CAP_SYS_ADMIN` in the VM) |
| `/proc/cpuinfo` brand, signature, flags, CPU count (`cpuinfo`) | yes | yes: a file generated from the host's first processor block and the recipe, bind-mounted read-only | yes: gVisor accepts the bind mount over its own `/proc/cpuinfo` (`model name: unknown` without it) | no: the arm64 VM's (Apple implementer `0x61`); a file mount would work, see the gaps |
| `CPUID` instruction (`cpuid`) | yes (patched Firecracker) | no, host values | no, host values | no: an Apple silicon core |
| DMI, `/sys/class/dmi/id` (`dmi`) | yes | yes where the host has `/sys/class/dmi/id` to mount over (a container cannot create a directory in sysfs); a directory of generated files, serials and UUID root-only as the kernel makes them | yes on any host: gVisor's own sysfs has `/sys/class/dmi` | no: the kernel has no `CONFIG_DMI`, and sysfs takes no new directory |
| PCI IDs (`pci`) | yes | no | no | no: virtio |
| hwmon sensors and the hwmon fault (`hwmon`) | yes (guest kernel module, readings follow load) | yes where the host has a `/sys/class/hwmon` directory to mount over (an empty one will do): generated coretemp files, idle readings (the recipe's, else 38 °C); the daemon rewrites them for a fault (`+101.0°C ALARM (CRIT)` seen). Without the directory, no sensors, with a warning | yes on any host: gVisor takes the mount; the fault seen the same way | no: the kernel has no hwmon; the fault has no effect and is not recorded |
| Resource views (`/proc/meminfo`, `/proc/stat`, `/proc/uptime`, `/proc/loadavg`, `/proc/diskstats`, `/proc/swaps`, `/sys/devices/system/cpu/online`) | the VM's own | lxcfs's files bind-mounted, never over `/proc/cpuinfo` (`free` showed the recipe's 4096 MiB on an 8 GB host, lxcfs 6.0.4; and 3925 MiB, the host's whole memory, under the recipe's 4 GiB limit on a 4 GB host, lxcfs 5.0.3); without lxcfs the host's, with a warning | gVisor's own, which follow the container's memory limit; no `/proc/diskstats` or `/proc/swaps` exist | the VM's own (`--memory`); one CPU more than asked |
| Kernel version, `uname -r` (`kernel`) | recipe kernel | host kernel | `4.19.0-gvisor` | Apple's default (Kata 6.18) |
| Uptime offset (`uptime`) | yes (time namespace) | no: the container's uptime (lxcfs) | no: the sandbox's | no: the VM's |
| Fingerprints | Firecracker's (MPTABLE `FC`, ACPI `FIRECK`, MAC `02:fc`), see follow-ups | Docker's (`/.dockerenv`, cgroup paths) | gVisor's (`/proc/gvisor`, `/proc/sentry-meminfo`, `dmesg`'s `Starting gVisor...`) | Apple's (`vminitd`, the Kata kernel string, relayed sockets in `/proc/net/unix`) |

`profile check` lists, for the chosen runtime on the actor's host, each part
of the identity it does not present, as a warning (`actor: cpuid
(container): …`). A recipe's `required:` lists the identity fields an actor
must present (`hostname`, `cpuinfo`, `cpuid`, `dmi`, `pci`, `hwmon`,
`kernel`, `uptime`); a required field the runtime does not present is an
error in `profile check` and fails `run create` before anything is acquired.

### Isolation

- `runc`: shares the host kernel. Suitable for trusted driving programs or
  hosts dedicated to one run.
- `runsc`: a user-space kernel between the program and the host. Weaker than a
  microVM and with syscall overhead, but no KVM.
- `macos-apple-container`: a lightweight VM per actor (Virtualization.framework)
  with Apple's kernel. The container holds `CAP_SYS_ADMIN` inside its VM for
  init's mounts; the driving program runs without capabilities.

The report shows the runtime of each run so a reader knows which
isolation a trace came from.

## Slices (proposed)

1. **Runtime abstraction** (done, 2026-11-11, see
   [As built: slice 1](#as-built-slice-1)): `ActorRuntime` with the
   Firecracker implementation only; no behavior change.
2. **Container runtime with `runc`** (done, 2026-11-11, as are 3 and 4, see
   [As built: slices 2 to 4](#as-built-slices-2-to-4)): guest API and gateway over Unix sockets,
   init in container mode, the container actor image with build-time shims,
   host-side workspace mounts.
   Verify `examples/smoke` and `examples/chaos` in a container on a host
   without KVM.
3. **`runsc`**: install gVisor on a test host, run the same examples, record
   which identity fields work.
4. **Identity files**: `/proc/cpuinfo`, DMI and hwmon files from the recipe,
   and the hwmon fault rewriting them.

## Decisions

- **Explicit runtime** (owner, 2026-11-10): `actor.runtime` is required. On a
  host without `/dev/kvm`, a `firecracker` actor fails with a message that
  names the `container` runtime; needy never lowers isolation silently.
- **Routing by build-time shims** (owner, 2026-11-10): every executable on the
  image's `PATH` becomes a shim when the image is built; `*` is supported for
  those programs. Executables created at run time are a stated gap.
- **Separate container image** (owner, 2026-11-10): the container actor has its
  own image stage, without what only a VM needs (the guest kernel modules,
  systemd for `systemd-detect-virt`), and with the shim stage last. `needy
  build` builds it.
- **Docker through `bollard` first** (owner, 2026-11-10); containerd directly is
  a later provider.
- **Every run has an actor** (owner, 2026-11-11): there is no `actor: none`.
  Hosts without KVM get actors through the container runtimes instead: Docker
  on Linux, Docker Desktop or Apple's `container` on macOS, Docker Desktop on
  Windows, and later WSL or Hyper-V on Windows. The guest API and gateway are
  therefore transports the runtime returns (see [Guest transport](#guest-transport)),
  and the daemon stays transport-agnostic.

## As built: slice 1

Implemented 2026-11-11; no behavior change for Firecracker actors.

- `core-actor::host::runtime` holds the `ActorRuntime` trait: `name`,
  `check_host` (KVM for Firecracker), `channels(dir)` and
  `start(dir, recipe)`, which returns a `RunningActor` handle with `pid`,
  `vcpus`, `mem_mib`, `wait` and `stop(grace)`. `runtime::log(dir)` is the
  actor's console file (`console.log`) every runtime writes.
- `Channels` are the runtime-provided meeting points: `guest` and `gateway`
  are `Endpoint`s the daemon serves (`Endpoint::Unix` only;
  `TODO(container-runtime)` marks loopback TCP with a per-run token),
  `terminal` is how the daemon dials the actor's process service
  (`Terminal::FirecrackerVsock`), `gateway_port` and `process_port` are what
  init is told, and `sockets` are the files the daemon removes when the actor
  ends.
- `core-actor::host::firecracker` (renamed from `host::vm`) implements it with
  today's `Image`, `Launch` and `Vm`; the vsock ports (`GUEST_PORT`,
  `GATEWAY_PORT`, `PROCESS_PORT`) moved there from the daemon.
- The daemon picks the runtime from `actor.runtime` (`actor::runtime`) and no
  longer names vsock: `actor.rs` serves and dials the channels, `guest.rs`
  tells init the runtime's ports, and `run logs` reads `runtime::log`.
  Reconciliation reads the runtime from the run's stored profile to remove
  its sockets; ending a leftover actor is still Firecracker's only
  (`TODO(container-runtime)` in `reconcile.rs`).
- `actor.runtime` is required (`firecracker | container`), with no default;
  `actor:` is therefore required too. Every example names `firecracker`.
  `validate::profile` refuses `container` as not implemented yet, so
  `profile check` and `run create` say so. A profile without `actor:`
  therefore no longer loads, which ends the 2026-11-11 rule that such a
  profile is checked for routing and executors only (REVIEW: needs the
  owner's call, see the runners and runs design's profile check).
- `profile check` probes the actor's host through `ActorRuntime::check_host`
  instead of calling the KVM check directly. The runner capability probe
  (`capabilities::firecracker`) still names `host::firecracker` itself: it
  is about the Firecracker runtime by definition.

## As built: slices 2 to 4

Implemented 2026-11-11.

**Runtime** (`core-actor::host::container`). `Container` implements
`ActorRuntime` over an `Engine` trait (the Docker API through `bollard`;
tests stand in for it). The container is `needy-actor-<run>`, from the
profile's image, entrypoint `/.needy/init` as root, `HostConfig.runtime` the
profile's `oci_runtime`, `network_mode: none`, a read-only root, tmpfs at
`/tmp`, `/var/tmp`, `/run` and the user's home (owned by the user, `exec`),
`nano_cpus` the recipe's vCPUs (at most the host's) and `memory` its memory.
Its stdout and stderr are copied into `console.log`, which `run logs` reads.
Waiting returns Docker's exit code as the wait status; stopping is Docker's
stop with the grace, then the wait; both detach the host-side mounts and
remove the container. A Docker that cannot be reached fails `check_host`
with the reason. A restarted control plane ends a leftover container actor by
its name and detaches its mounts (`reconcile`).

**Channels.** `Channels` gained `Terminal::Unix` and `guest_for_every_user`.
A container's guest, gateway and process sockets are in the run's
`channels/` directory, bind-mounted at `/.needy/channels`; init finds them at
those fixed paths, as a VM's init dials fixed vsock ports, so the guest
configuration needed no new field. The daemon opens the guest socket (0666)
and its directory (0755) after binding them, since the driving program's
proxy runs as the profile's user; the run's directory around them stays
0700 on the host.

**Setup.** `ActorRuntime::setup` says who mounts the workspace and presents
the hostname and sensors: `Guest` (a VM's init) or `Host` (the container
runtime). Under `Host` the daemon sends init no mounts, hostname, sensors,
hwmon faults or uptime offset; it passes the workspace mounts to
`ActorRuntime::start`, which runs `sshfs` in passive mode on the daemon's
machine over an exec stream opened through the run's own guest socket (so
the stream is the actor's and the SFTP fault tap applies), and bind-mounts
each mount point into the container. The daemon schedules hwmon faults
itself and records them as init does (`fault.injected`).

**Init in a container** (`guest::in_container`: the guest socket exists)
skips the system mounts, `lo`, `memfd_noexec`, the home tmpfs, the hostname
and the sensors, forwards `127.0.0.1:3128` to `gateway.sock`, serves the
terminal's process service on `process.sock`, turns routing on by writing
`/run/needy/route` (user ID and route names), and exits (0, or 1 after a
failed boot) instead of rebooting. Its route stubs are links to the proxy.

**Shims.** `images/container/shim.sh` is the last step of the
`container-root` stage: for each directory on Docker's default `PATH` (by
physical path, once), every executable is copied to `/.needy/real/<path>`
and replaced by a hard link to `/.needy/proxy`; a link whose target lies
outside those directories becomes a shim whose real program is a link to
the target; links into them stay links. The `actor-container` stage copies
the result into one layer (`needy-actor`, about 430 MB; the 464 hard links
share one inode). `needy` started through a link whose `/proc/self/exe` has
a `/.needy/real` twin is a shim (`guest::shim`): it reads the route file and
`execv`s the real program with the caller's argv unchanged, or hands the exec
to the proxy when the route's user runs a listed name (matched as the kernel
module matches: `AT_EXECFN`, its file name, the shim's file name, and
`argv[0]` with its file name, which covers a script's interpreter). Init
starts the driving program, and the proxy a kept command, through their real
paths, so neither is routed.

**Identity** (`core::recipe::files` for the text, `core-actor::host::identity`
for the files). `/proc/cpuinfo` is generated from the host's first processor
block: the recipe's brand and signature, one block per recipe vCPU with its
topology, hidden flags removed, `hypervisor` after `rdrand` when the recipe
shows it, the kernel's padding kept. DMI attributes are written with the
kernel's modes; the sensors are the guest kernel's fake coretemp (package
and one core per vCPU, idle at the recipe's `coretemp` or 38 °C). lxcfs's
views are mounted under `runc` where `/var/lib/lxcfs` has them. The compiled
recipe gained `cpu` (a `CpuView`) and `required`; `ActorRuntime` gained
`overheat` and `identity_gaps`, which `profile check` and `run create` turn
into findings.

**gVisor** (slice 3). Installed from gVisor's apt repository as its install
guide says; runsc `release-20261102.0`. It keeps containers from host Unix
sockets by default, and init then cannot reach `guest.sock`; registered with
`runsc install -- --host-uds=all` (open for the guest API and gateway, create
for the process socket) everything works, and `check_host` refuses a `runsc`
registered without it, naming that command. Host-side sshfs mounts pass
through gVisor's gofer. The identity differences are in the table above.

**Capabilities and scheduling.** A runner whose Docker answers reports
`actor(container)`, whatever OCI runtimes it has (checked when a run is
created, on the control host, where actors run); `schedule::Need::Actor`
carries the runtime.

**Image and build.** `needy build --container` builds `needy-actor:latest`
(target `actor-container`): the actor-root's programs without systemd and
sshfs.

Gaps, each marked in the code:

- Executables created after the image was built and run directly are not
  routed (`profile check` warns; `TODO(container-routing)` above).
- `vm.memfd_noexec` is not set in a container actor
  (`TODO(container-actor)` in `guest/init/mod.rs`).
- The uptime offset is not applied (`TODO(container-actor)` in the daemon's
  `guest.rs`); sensor readings do not follow load.
- Docker Desktop, WSL and Hyper-V need the loopback TCP transport
  (`TODO(container-runtime)` on `Endpoint` and `Terminal`); Apple's
  `container` does not (see [As built: macos-apple-container](#as-built-macos-apple-container)).
- The `actor(container)` capability does not carry the OCI runtimes
  (`TODO(runner-actor)` on `ActorRuntime::Container`).

## As built: macos-apple-container

Implemented 2026-11-12, from an experiment with an actor wired by hand; the
commands, timings and results are in the
[Apple `container` actor evidence](2026-11-12-macos-apple-container-actor-evidence.md).

**Runtime** (`core-actor::host::macos::apple_container`). `AppleContainer`
implements `ActorRuntime` by driving Apple's `container` CLI through a `Cli`
trait (the process is the boundary; tests stand in for it). The actor is
`container run --name needy-actor-<run> --network none --cpus N --memory M
--cap-add CAP_SYS_ADMIN --tmpfs ... --entrypoint /.needy/init IMAGE`, run in
the foreground: its output is `console.log`, its exit code the actor's.
Stopping is `container stop --time GRACE`, then the wait; both delete the
container. A restarted control plane deletes a leftover actor by its run's
name (`reconcile`). `check_host` asks `container system status` and
`container image inspect IMAGE`; off macOS the CLI is refused at once.

**Channels.** The same files as the container runtime's (`channels/` in the
run's directory, `/.needy/channels/*.sock` in the actor), but mounted one by
one: `guest.sock` and `gateway.sock` as `--volume` file mounts, which the CLI
relays into the VM over vsock (a mounted directory of sockets fails there),
and `process.sock` as `--publish-socket`, which the CLI listens on and
connects to init's socket for. Because the host side of a published socket
accepts before init serves it, the daemon counts a terminal connection only
once the process service answers a request.

**Root file system.** Writable, unlike a Docker container actor's: `vminitd`
reaches a published socket on the container's root file system, outside its
mounts, so init's process socket cannot be in a tmpfs. Each container has its
own copy of the image's file system, which ends with it
(`TODO(macos-apple-container)` in `launch`).

**Setup.** `Setup::GuestMounts`: init mounts the workspace with sshfs inside
and sets the recipe's hostname, as a VM's init does (the container has
`CAP_SYS_ADMIN` in its own VM; the driving program has no capabilities); the
daemon sends it no sensors, hwmon faults or uptime offset, which Apple's
default kernel cannot present. The container actor image therefore has
`sshfs` and `fuse3`.

**Identity** with Apple's default kernel (Kata 6.18, arm64): the recipe's
hostname; no DMI, hwmon or uptime offset; `/proc/cpuinfo` and the kernel are
the VM's; the VM has one CPU more than asked. `identity_gaps` lists each, and
a field the recipe requires fails `run create`. The needy-built arm64 kernel
(`-k`) that would bring `binfmt_needy`, the fake sensors and DMI is a separate
slice (`TODO(macos-apple-container-kernel)`).

**Capability and build.** A runner on macOS whose `container` services answer
reports `actor(macos-apple-container)`. `needy build --macos-apple-container` builds
`needy-actor:latest` for linux/arm64 through Docker and loads it with
`container image load`; the `needy` and `container-root` stages build for the
platform's `TARGETARCH`.

## Open questions

None left from the design; the two it had were settled by slices 2 and 3:

- **gVisor and identity files**: `runsc` accepts bind mounts over
  `/proc/cpuinfo`, `/sys/class/dmi/id` and `/sys/class/hwmon`, and over
  lxcfs's `/proc/meminfo`, `/proc/stat`, `/proc/uptime`, `/proc/loadavg` and
  `/sys/devices/system/cpu/online`, but not over `/proc/diskstats` or
  `/proc/swaps`, which its `/proc` lacks.
- **Shim and the real program's own `argv[0]`**: the shim executes
  `/.needy/real/...` with the caller's argv, `argv[0]` included.
