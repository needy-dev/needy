# Container actor runtime

**Status**: design, 2026-11-10. Slice 1 (the runtime abstraction) is
implemented, see [As built: slice 1](#as-built-slice-1); the container runtime
is not. The owner's decisions are under [Decisions](#decisions); two open
questions remain.

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
| `container` | Docker API through `bollard`, `HostConfig.runtime` = `runc` or `runsc` | Docker; gVisor's `runsc` registered as a Docker runtime for `runsc` |
| `container` on macOS and Windows | Docker API through `bollard` against Docker Desktop, whose Linux containers run inside a VM (WSL 2 on Windows) | Docker Desktop |
| `apple-container` | Apple's `container` (each container is a lightweight Linux VM on Apple silicon) | macOS with Apple's `container` installed |
| `wsl`, `hyper-v` | later: a WSL distribution or a Hyper-V VM as the actor on Windows (`TODO(windows-actor)`) | Windows |

containerd directly (without Docker) is a later provider, like the executor
`TODO(provider)`.

Profile:

```yaml
actor:
  runtime: container        # required: firecracker | container
  container:
    oci_runtime: runsc      # runc | runsc
    image: needy-actor:latest
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
| `apple-container` | vsock if Apple's `container` exposes it to clients, otherwise loopback TCP with a per-run token (to be settled by an experiment) |

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

| Field | Firecracker | `runc` | `runsc` (gVisor) |
| --- | --- | --- | --- |
| Hostname | yes | yes (UTS namespace) | yes |
| `/proc/cpuinfo` brand and flags | yes | file bind-mounted over it | to be tested: gVisor synthesizes `/proc` |
| `CPUID` instruction | yes (patched Firecracker) | no, host values | no, host values unless the host has CPUID faulting |
| DMI (`/sys/class/dmi/id`) | yes | files bind-mounted | to be tested |
| PCI IDs | yes | no | no |
| hwmon sensors and the hwmon fault | yes (guest kernel module) | files bind-mounted; the daemon rewrites them for faults | to be tested |
| Kernel version (`uname -r`) | recipe kernel | host kernel | gVisor's reported version |
| Firecracker fingerprints (MPTABLE `FC`, ACPI `FIRECK`, MAC `02:fc`) | present today, see follow-ups | none | none |

`profile check` lists, for the chosen runtime, each recipe field it cannot
provide, as a warning. A recipe may mark fields as required; a required field
the runtime cannot provide is an error.

### Isolation

- `runc`: shares the host kernel. Suitable for trusted driving programs or
  hosts dedicated to one run.
- `runsc`: a user-space kernel between the program and the host. Weaker than a
  microVM and with syscall overhead, but no KVM.

The report shows the runtime of each run so a reader knows which
isolation a trace came from.

## Slices (proposed)

1. **Runtime abstraction** (done, 2026-11-11, see
   [As built: slice 1](#as-built-slice-1)): `ActorRuntime` with the
   Firecracker implementation only; no behavior change.
2. **Container runtime with `runc`**: guest API and gateway over Unix sockets,
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

## Open questions

- **gVisor and identity files**: whether `runsc` accepts bind mounts over
  `/proc/cpuinfo`, `/sys/class/dmi/id` and `/sys/class/hwmon`. Settled by slice
  3's experiment.
- **Shim and the real program's own `argv[0]`**: programs that behave by
  `argv[0]` (busybox, `python3` vs `python3.11`) must receive their original
  name when the proxy executes `/.needy/real/...`.
