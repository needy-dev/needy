# Actor and executor separation

**Status**: pending. This note records the direction agreed on 2026-11-09 and
the experiments behind it. Nothing in it is implemented in this repository yet.
It changes several entries in the
[design decisions](2026-11-06-needy-design-decisions-design.md); see
[Changes to existing decisions](#changes-to-existing-decisions).

Related notes:

- [CLI, exec streams and reports](../cli/2026-11-09-cli-exec-and-report-design.md)
- [Profiles and rules](../config/2026-11-09-profile-and-rules-design.md) (draft)

## Goal

Needy sits between a program that drives work and the environment where that
work takes effect. It has three jobs:

1. **Route** the driving program's exec, file and network operations to an
   execution environment by rule.
2. **Observe** every routed and local operation, so that a session can be
   audited and replayed.
3. **Shape the environment**: inject faults (chaos) and present a chosen system
   identity (recipe).

Agent RL rollouts and agent evaluations are the first use. In that use the
driving program is an agent harness, and a key scenario is an agent that may
replace its own harness with code it wrote, in any language, and is then asked
what platform and device it runs on. Integration tests and acceptance
environments have the same shape: a test suite or acceptance script drives a
target environment.

## Terms

| Term | Meaning | Avoid |
| --- | --- | --- |
| **actor** | The side that runs the driving program: an agent harness, a test suite or an acceptance script. Usually one microVM. The program's view of the world, including its recipe identity, lives here. | harness layer, caller, shallow layer |
| **executor** | An environment where routed operations take effect: a container, microVM, pod or real device such as a Mac. The executor layer is the set of executors bound to a session. | runner, worker, needy client, target |
| **harness** | One kind of driving program: an agent loop such as Claude Code, Codex or an agent-written replacement. | — |
| **daemon** | The control plane: lifecycle of actors and executors, routing rules, both network gateways and the trace. | server |
| **CLI** | The `needy` command. Its `Client` struct only talks to the daemon. | — |
| **recipe** | A declarative description of one machine: image, kernel, CPU, memory, firmware, devices and network identity. Built by `needy build`. | persona |
| **profile** | A declarative description of one session: which recipe the actor and each executor use, routing rules, network policies, chaos and file mappings. | config |
| **preset** | A named, reusable block of rule actions inside a profile, referenced with `use:`. Built-in recipes are `builtin:NAME`, not presets. | — |
| **session** | One actor and the executors bound to it, started from a profile. One RL rollout is one session. | run, rollout |
| **trace** | Every event needy records during a session. | log |

The old Docker *session* (a workspace and home volume pair) and *run* (one batch
command) become one executor implementation. The `SessionService` and
`RunService` schemas in `proto/` follow the old model and must be revised.

## Layers

```text
                    needy daemon (control plane: routing, gateways, trace, lifecycle)
                        │                                   │
┌─ Actor host ───────────────────────────────┐      ┌─ Executors ───────────────────────────┐
│ microVM ×N (≈60 MiB each)                  │      │ container, microVM, pod or device     │
│  ├ driving program (harness, test suite)   │      │  ├ authoritative workspace            │
│  └ needy supervisor (in guest)             │      │  ├ toolchains: git, node, compilers…  │
│     ├ exec rule hit ─── proxy PID ────────────────▶│  ├ real processes                     │
│     ├ file rule hit ─── FUSE ─────────────────────▶│  ├ file server                        │
│     └ bridge rule hit ─ tunnel ───────────────────▶│  └ executor network ─▶ its gateway    │
│  actor network ─▶ actor gateway            │      │ may be a real Mac, a GPU host, …      │
└────────────────────────────────────────────┘      └───────────────────────────────────────┘
```

- **Actor**: the driving program and the needy supervisor. It is small, dense
  and cheap to snapshot.
- **Executor**: the workspace, toolchains, real processes and the task's own
  network. Executors advertise capabilities through labels and may differ from
  each other.
- **Neither side is the control plane.** The daemon is. Both actor and executor
  run workloads; they differ in role.
- **The actor sits near its main dependency** (inference, for a harness); **the
  executor sits near what it operates on.** See [Network routing](#network-routing).
- **Compute stays with the workspace.** Builds, `git status` and test runs make
  many small file operations. Commands run on the executor that holds the
  workspace. The actor reads and edits workspace files remotely; that volume is
  small.
- **Every machine that can execute runs `needy daemon`.** The actor's daemon
  connects to it, as the current Linux-to-Mac bridge already does. A dial-out
  mode for executors behind NAT can be added later as a daemon option.

- **Needy does not schedule.** Resource limits come from each runtime's own
  options; needy keeps a keyed pool of executor instances with a per-instance
  process limit. See [Executors](../config/2026-11-09-profile-and-rules-design.md#executors).

`needy exec` and interception are two front ends of one placement API:
interception in the actor is an automatic `needy exec`.

## Mechanisms

| Need | Mechanism | Notes |
| --- | --- | --- |
| Exec routing | Existing exec interception and proxy PID ([D2, D3](2026-11-06-needy-design-decisions-design.md#accepted-decisions)) | One hop is implemented. Executor PTYs, detach and attach, process groups and failure handling remain. |
| File routing | FUSE in the actor guest, served by the executor | Gives mmap, readdir and change semantics without rewriting each file syscall. |
| Network routing | One network and gateway per side; bridge rules for exceptions | See [Network routing](#network-routing). |
| Observation | eBPF in the guest kernel | The driving program cannot see a tracer. Block `bpf` and `perf_event_open` in the guest. |
| Result rewriting and binary replacement | ptrace with a seccomp `RET_TRACE` subscription | Only where arguments or results must change. ptrace leaves visible traces (`TracerPid`, failed `PTRACE_TRACEME`). |
| Simple fault injection | eBPF `bpf_override_return` | Errno only. |
| Rules | Declarative only at run time | No scripts or LLM calls on the syscall path, so decisions depend only on profile, seed and event, and a session can be replayed. See [Profiles and rules](../config/2026-11-09-profile-and-rules-design.md). |
| Single-process harness replacement | One allowed `execve`, then deny `fork`, `vfork`, `clone` without `CLONE_THREAD`, `clone3` and further exec | Threads stay allowed. Also deny io_uring, 32-bit compat syscalls, `personality`, `ptrace` and `process_vm_*`. |

## Session state is split

A session has state in two places: the actor VM and the executor workspace.
Reset and fork must restore both to the same point. Firecracker snapshots cover
the actor; the executor needs filesystem snapshots (btrfs, ZFS or an overlay
layer). Needy needs one session snapshot that names both.

## Recipes and identity

A **recipe** describes one machine and is compiled into every layer. Recipes are
specified in the [recipe note](../config/2026-11-09-recipe-design.md); this
section records the principles. Earlier drafts called the identity part a
*persona*.

```text
recipe.yaml ─▶ Firecracker config + CPU template
            ─▶ guest kernel build options + built-in fake devices
            ─▶ rootfs overlay (os-release, hostname, lived-in files, logs)
            ─▶ metadata service content
```

Principles:

- **Make it true where cheap.** Memory, core count, kernel version, distribution
  and uptime are real inside a microVM. Lie only about what cannot be made true.
- **Check consistency.** A recipe fails on contradictions more than on single
  values: CPU model vs. instruction set, kernel vs. distribution, cloud vendor vs.
  MAC prefix, DMI and metadata, uptime vs. log dates.
- **Prefer an identity that is a VM.** A cloud VM recipe matches Firecracker's
  virtio devices and the VM timing signature.
- **Keep identity consistent across sides.** An in-process `uname()` is
  answered by the actor VM; a routed `uname` command is answered by the
  executor. Give both the same recipe, or keep identity commands on the actor.

Identity fidelity is an evaluation variable, not a claim of equivalence. Log
every probe the agent makes; the probes are useful data.

### Fidelity levels

| Level | Setup | What still reveals it |
| --- | --- | --- |
| L0 | Plain Docker | `/.dockerenv` |
| L1 | System container, lxcfs, time namespace | mountinfo, cgroup, syscalls newer than the claimed kernel |
| L2 | L1 plus needy rewriting kernel version, CPU and DMI | vDSO version note, tracer detection, timing |
| L3 | microVM with recipe kernel and hardware, hypervisor hidden | Timing side channels only |

## Experiments

All runs on kvm-host: Intel Core i5-13600KF, 20 threads, 94 GB RAM, Debian 13,
kernel 7.1. Artifacts are in `~/fc-exp` on that host.

### macOS identity with Darling

Darling v0.1.20260608 (Ubuntu 24.04 packages) in a privileged container.

- Looks like macOS: `uname` reports `Darwin 20.6.0`, `sw_vers` reports 11.7.4,
  binaries are Mach-O, and the official darwin-x64 Node 22 runs with
  `process.platform` `darwin`.
- Configuration leaks: `sw_vers` BuildVersion is `Darling`; `/Volumes/SystemRoot`
  exposes the Ubuntu host root; `/proc` exists; `hw.model` and
  `machdep.cpu.brand_string` are missing; `ps` prints
  `Unimplemented syscall (539)`; `os.cpus()` is empty; `clang` is a stub.
- Fundamental leaks: a raw Linux `uname` syscall from Python ctypes returns
  `Linux` and the host kernel version; a raw Darwin syscall returns `ENOSYS`;
  `cpuid` returns `13th Gen Intel(R) Core(TM) i5-13600KF`, which no Mac shipped
  with.

Darling translates syscalls only inside its `libsystem_kernel`, so direct
syscalls bypass it. Syscall User Dispatch could close that gap. The macOS
identity was judged not worth the cost; a Linux identity is the target.

### Linux identity with Firecracker

Firecracker v1.17.0, guest kernel 6.18.62 with Firecracker's CI config, Ubuntu
24.04 rootfs.

| Probe | Default | Custom CPU template | Patched Firecracker + template + built-in fake coretemp |
| --- | --- | --- | --- |
| `cpuid` brand string | `Intel(R) Xeon(R) Processor` | unchanged | `Intel(R) Xeon(R) CPU @ 2.20GHz` |
| Family/model | 6/183 (host Raptor Lake) | unchanged | 6/85/7 |
| Hypervisor bit | 1 | 1 | 0 |
| `KVMKVMKVM` signature | present | cleared | cleared |
| `systemd-detect-virt` | `docker` (image leftover) | — | `none` for VM and container |
| `lspci` (`--enable-pci`) | — | host bridge `8086:0d57`, virtio block | same |
| `sensors` | none | none | `coretemp-isa-0000` at `/sys/devices/platform/coretemp.0/hwmon/hwmon0` |
| `cpuid` cost | 2809 TSC cycles | 2884 | 2883 (host native: 332) |

Findings:

- Firecracker normalizes CPUID after applying a template, so the brand string
  and hypervisor bit need a patch. The experiment patch reads
  `NEEDY_BRAND_STRING` and `NEEDY_HIDE_HYPERVISOR`; it touches only the Intel
  path.
- The CPU identity is bounded by the host instruction set. The Cascade Lake
  signature still showed `avx_vnni` and `serialize` and lacked `avx512f`.
- A fake coretemp built into the kernel matches the real driver's sysfs layout,
  follows load and accepts an override for overheating faults. Built in, it does
  not taint the kernel. Its parameter path, `/sys/module/fake_coretemp`, still
  needs a neutral name.
- Several leaks came from the image, not the VM: Ubuntu's Docker image ships
  `/run/systemd/container`, the hostname is `(none)`, and `/lib/modules/<version>`
  is missing.
- DMI is absent and still to do.
- The whole proof of concept builds as one multi-stage Dockerfile in 2 min 25 s
  and runs with `docker run --device /dev/kvm`, without `--privileged`.

### Timing

On Intel, `cpuid` always causes a VM exit, so a spoofed `cpuid` is always slower
than native. Options, in order of cost:

1. Measure how often agents try timing probes before investing.
2. Use a VM identity, where the slow `cpuid` is the correct observation.
3. Compensate the guest TSC for exit time. This needs a host KVM patch and fails
   against a long-running comparison with an external clock.
4. Give the guest fully virtual time, with the gateway keeping external time
   consistent. This also makes rollouts reproducible, at a large engineering and
   runtime cost.

On Arm, MIDR reads are served from `VPIDR_EL2` without an exit, so this channel
may not exist there. Not yet measured.

### Resources

Stock Firecracker, read-only shared rootfs.

| Measurement | Result |
| --- | --- |
| One VM, 1 vCPU / 256 MiB | ready in 194 ms, 68 MiB host RSS |
| One VM, 4 vCPU / 2 GiB | ready in 161 ms, 105 MiB host RSS |
| 50 VMs at once (1 vCPU / 256 MiB) | all ready in 1.0 s, 65 MiB each |
| 200 VMs at once | all ready in 4.1 s, 58 MiB each |
| Full snapshot, 256 MiB / 2 GiB | 66 ms / 413 ms; memory file equals guest size |
| Restore from snapshot | 16 ms, 14–16 MiB RSS, independent of guest size |

These numbers cover the VM only. Real cost is dominated by the driving program
and the work it runs. Restored clones share page cache, so forking many rollouts from one
snapshot is cheap; clones must reseed randomness.

### macOS host

apple/container 1.7.1 supports nested virtualization through
`container machine create --virtualization` on Apple silicon M3 or later with
macOS 15 or later. A Firecracker-in-apple-container run is not yet measured.

## Prior art

| Project | Relation |
| --- | --- |
| [agentsh](https://github.com/canyonroad/agentsh) | Policy enforcement and audit for agents. Full on Linux; macOS and Windows natively are partial, and full support there runs Linux in a VM. No fault injection, result rewriting or machine identity. |
| [Reverie](https://github.com/facebookexperimental/reverie) / [Hermit](https://github.com/facebookexperimental/hermit) | Syscall interception framework and deterministic execution with virtual time and CPUID. Borrow the design; do not depend on it yet. |
| [kbox](https://github.com/sysprog21/kbox) | seccomp user-notification techniques: FD injection, shadow memfds, binary replacement. |
| [macstab chaos-init](https://github.com/macstab/macstab-chaos-testing-libraries/pull/35) | Rule-based syscall fault injection through seccomp user notification. |
| [agentc](https://github.com/laosb/agentc) | Runs coding agents in Apple Containerization, Kata or gVisor with an MCP host bridge. Isolation only. |
| [DSec](https://arxiv.org/html/2609.22978v1) | Agent RL sandbox infrastructure at scale. No per-syscall observation or fault injection; lists real agent attacks worth using as tests. |

## Network routing

**Status**: accepted (2026-11-09).

**The actor and the executors are on two separate networks**, each with its
own gateway, policy and audit log. A connection belongs to the network of the
process that opens it. Traffic between the two networks is denied unless a rule
bridges it.

| Source | Destination | Route |
| --- | --- | --- |
| Actor | Model API | Actor gateway. The gateway injects the API key, so the actor never holds the real credential. |
| Actor | Other external hosts, for example a web fetch tool | Actor gateway, under the actor policy. |
| Executor | External hosts | Executor gateway, under the task policy. |
| Executor | Loopback ports on its own machine | Local to the executor. |
| Either | The other network | Denied by default; allowed only by an explicit bridge rule. |

DNS resolves inside each network, so answers match the path the traffic takes.

The actor does not use loopback services. When the driving program reaches a
local service, such as a dev server, it does so through a command, and that
command runs on the executor next to the service.

Each network is placed by the traffic it carries:

- **Actor network: near the inference cluster** (for a harness). Its traffic is
  almost only model calls, with long contexts and streamed responses. Latency and bandwidth
  to inference matter most. Its policy is short: allow the model API, inject the
  key, deny most else. The two networks may have different public addresses;
  this is expected and not hidden.
- **Executor network: near what the task operates on.** That is the workspace
  and its caches, real devices (a Mac, a phone, a GPU host), internal services,
  and package mirrors that can be recorded and replayed. Side effects happen
  here, so audit, allowlists, record and replay, and reward-hacking defences
  concentrate in this network.

The link between the sides may then cross data centres. Actor file reads over
FUSE pay several round trips each (`stat`, `open`, `read`); at 50–100 ms of WAN
latency a file read takes a few hundred milliseconds, which is small next to an
inference step. Add caching or prefetch when it matters.

Audit and record and replay run per network; a session's network history is the
union of both logs.

## Changes to existing decisions

| Decision | Change |
| --- | --- |
| D1 agent-agnostic | Kept. A harness is one kind of driving program. |
| D2, D3 exec placement and proxy PID | Kept; they implement actor-to-executor exec routing. |
| D4 resource-level protocol | Kept. |
| D5 only stdio, PTY and signals cross | Extended: files cross through FUSE and network through tunnels. Other descriptors stay local. |
| D6 Docker sessions as isolation unit | Replaced: the actor runs in a microVM; executors provide their own isolation. A Docker session becomes one executor implementation. |
| D7 paired clients are trusted operators | Kept. The actor's daemon is a paired client of each executor daemon. Executor-side guards (`--host-root`, command allowlists) remain the machine owner's last line and take precedence over profiles. |
| D8 no implicit credentials | Kept and strengthened: the gateway injects model credentials. |
| D9 tar workspace transfer | Replaced by executor-owned workspaces and file routing (O2). |
| R2 no syscall-virtualizing kernel | Narrowed: still no general syscall-virtualizing kernel, but selective result rewriting for recipes and chaos is in scope. |
| O2, O3, O5, O7 | Become the main direction: executor-owned filesystem, actor/executor split, placement-relative network, executor capabilities as labels. |
| O9 long-lived interactive sessions | Becomes detach and attach for `needy exec`; see the [CLI note](../cli/2026-11-09-cli-exec-and-report-design.md). |

## Open questions

- Atomic session snapshots across the actor VM and the executor workspace.
- Process-group signals and executor failure semantics for routed exec.
- DMI and a fuller PCI tree for richer recipes.
- Firecracker on Arm inside apple/container: identity registers and timing.
- How often agents attempt timing probes in practice.
