# Drivers and extension points

Status: accepted by the owner (2026-11-12); being implemented in the slices below. Breaking API, config and proto changes are allowed; implementers may refine the design and record each refinement under "Decisions during implementation".

## Why

Several kinds of things in needy are meant to be extended sideways: where an
actor runs, where executor instances come from, how a command is caught inside
an actor, what decides a command or request, and where events go. Today a
reader cannot see that from the code:

- Actor runtimes live as loose files under `core-actor/src/host/`, and two of
  them borrow from a sibling: `macos/vz.rs` takes Firecracker's vsock ports,
  `macos/apple_container.rs` takes the container runtime's socket paths.
- Adding an actor runtime touches the config enum, `core::types::ActorRuntime`,
  the proto enum, and hand-written matches in the daemon's `actor`,
  `capabilities`, `schedule`, `check`, `reconcile` and `testing`.
- `Setup { Guest, Host, GuestMounts }` packs independent questions (who mounts
  the workspace, who presents identity) into one enum; `runtime::Error` and
  `Terminal` carry Docker, Apple `container` and Firecracker variants.
- Executor providers (`core-executor/src/provider/`) are the same kind of thing
  as actor runtimes but are named and shaped differently, and `docker.rs` mixes
  the instance lifecycle with the Docker run-option policy.
- Both `core-actor` and `core-executor` talk to a container engine through their
  own `bollard` code.
- The gateway's allowlist and header injection are policy (`net.*`
  interceptors) but live inside the gateway, which is mechanism.

## Terms

| Term | Meaning |
| --- | --- |
| **actor driver** | What starts, waits for and stops a run's actor on a machine: `actor.driver` in a profile. Replaces "actor runtime". |
| **executor driver** | What creates and destroys executor instances: `executors.NAME.driver`. Replaces "executor provider". |
| **engine** | The Docker-compatible container engine a `linux-container` driver talks to, wherever `DOCKER_HOST` points. One client, no per-engine code. |
| **capture** | The mechanism that catches an exec inside an actor (kernel `binfmt_needy`, shims, a Seatbelt fence). Not policy. |
| **interceptor** | Policy at a hook point of one domain (exec, net, file); see [Interception domains](#interception-domains). |
| **sidecar** | A service needy runs for one run, next to the actor and outside it, that the actor reaches through one of its channels, and that ends with the run: the gateway, the guest API. Not the VM or container process itself (the driver owns that) and not init (inside the actor). |
| **adapter** | The code that turns what a capture caught into a domain's context, runs the domain's chain and applies the decision. |

"Driver" is not "driving program": the driving program is what runs inside the
actor. To keep the word unambiguous, `guest/init/driver.rs` (the driving
program's start) becomes `guest/init/program.rs`.

## Driver names

The prefix is the operating system whose isolation facility the driver relies
on. A driver that uses no OS-specific facility has no prefix.

| Actor driver | Was | Isolation it relies on |
| --- | --- | --- |
| `linux-firecracker` | `firecracker` | Linux KVM, through Firecracker |
| `linux-container` | `container` | Linux namespaces and cgroups (`runc`) or gVisor (`runsc`), through a container engine |
| `macos-vz` | (unchanged) | macOS Virtualization.framework, through vfkit |
| `macos-apple-container` | (unchanged) | macOS Virtualization.framework, through Apple's `container` |
| `macos-native` | PoC branch only | macOS Seatbelt (`sandbox-exec`) |

| Executor driver | Was | |
| --- | --- | --- |
| `native` | `native` | this machine; no OS facility, no prefix |
| `linux-container` | `docker` | as the actor driver of the same name, through the same engine |

`linux-container` names the container, not the host: its Linux kernel may be the
host's or the one inside OrbStack's or Docker Desktop's VM. Why not `docker`:
the driver depends on an engine API, not on Docker Engine itself. Docker Engine,
OrbStack, Podman and containerd (through its Docker-compatible tooling) are
treated as equivalent Docker-compatible engines (owner decision, 2026-11-12):
the driver reaches whichever one `DOCKER_HOST` names, and needy has no
per-engine code.

Later drivers follow the same rule (`linux-native`, `windows-wsl`,
`windows-hyper-v`); each needs its own approved slice.

The executor's typed Docker options block (`docker:`) becomes `container:`,
since it configures a `linux-container` instance through the engine.

Capabilities keep their shape: `actor(linux-firecracker,linux-container)
executor(linux-container,native)`.

## Layout

One rule for every extension point:

1. An extension point is a directory. Its `mod.rs` holds the trait and a table
   of every implementation.
2. Each implementation is one file or one directory, named after its config
   value with `-` written `_`.
3. Implementations never depend on each other. What two of them share moves
   into a component module named after what it is.

### `core-actor`

The `host/` level goes away: everything outside `guest/` is the host side.

```text
core-actor/src/
  contract.rs                  # guest <-> host contract: vsock ports, channel paths, socket names
  driver/
    mod.rs                     # trait Driver, trait Running, Channels / Endpoint / Terminal,
                               # Facets (replaces Setup), Error, the table of drivers
    linux_firecracker/
      mod.rs                   # impl Driver for LinuxFirecracker
      config.rs                # the Firecracker configuration from a compiled recipe
      kvm.rs                   # the KVM check and the host CPU probe (was host/detect.rs)
    linux_container/
      mod.rs                   # impl Driver for LinuxContainer
    macos_vz/
      mod.rs                   # impl Driver for MacosVz
      vfkit.rs                 # the vfkit command line
    macos_apple_container/
      mod.rs                   # impl Driver for MacosAppleContainer
      cli.rs                   # Apple's `container` CLI
    macos_native/              # if the PoC is productized
  image/
    vm.rs                      # needy's kernel and rootfs (linux_firecracker, macos_vz)
    container.rs               # the container actor image and its shim layout (linux_container, macos_apple_container)
  identity.rs                  # identity files presented from the host (cpuinfo, DMI, hwmon, lxcfs)
  sidecar/
    mod.rs                     # what a sidecar is, and the table of a run's sidecars with their channels
    gateway/                   # the net sidecar: the HTTP proxy that runs the net.* chain (mechanism only)
  sshfs.rs
  guest/                       # inside the actor (Linux)
    init/                      # driver.rs -> program.rs
    capture/
      binfmt.rs                # sets binfmt_needy's parameters
      shim.rs
    proxy.rs                   # where a caught exec goes; shared by every capture
```

### `core-executor`

```text
core-executor/src/
  driver/
    mod.rs                     # trait Driver (was Provider), Spec (was InstanceSpec),
                               # Instance (was InstanceHandle), Error, connect, sftp_command
    native.rs
    linux_container/
      mod.rs                   # impl Driver for LinuxContainer: create and destroy instances
      options.rs               # typed container options -> engine host config (the run-option policy)
  engine.rs                    # the Docker-compatible engine client both linux-container drivers use (bollard, DOCKER_HOST)
  process/                     # unchanged
  service.rs
  relay/
```

`core-actor`'s `linux_container` driver uses `core_executor::engine`, so there is
one engine client. This keeps the dependency direction (`core-actor` may depend
on `core-executor`, never the reverse).

## Who implements and who uses whom

```text
config      actor.driver / executors.NAME.driver        typed enums, validated
              |
daemon      actor::driver_for / executor::driver_for    the only places that pick a driver;
              |                                           capabilities, check and reconcile ask the driver
              +----------------------------+
core-actor::driver::Driver           core-executor::driver::Driver
  LinuxFirecracker                     Native
  LinuxContainer ---------+            LinuxContainer ---+
  MacosVz                 |                              |
  MacosAppleContainer     +----> core-executor::engine <-+
  (MacosNative)
  start() -> Box<dyn Running>          create() -> Instance
     |
     +-> components: contract, image::{vm, container}, identity, sshfs, gateway
```

Each driver answers, in one place:

| Question | Today |
| --- | --- |
| What does this host have? (`detect`, for capabilities) | `daemon/capabilities.rs` |
| Can this profile run here? (`check`) | `daemon/check.rs`, `config/validate.rs` |
| Clean up what a dead daemon left (`reconcile`) | `daemon/reconcile.rs` |
| Start one (`start` / `create`) | `daemon/actor.rs` factory match |
| What it presents and how it is reached (`facets`, `channels`) | `Setup`, `Channels` |

The config and proto enums stay: they are the typed boundary for validation
and the wire. `core::types::ActorRuntime` and `ExecutorProvider` go; the
capability set uses the config enums.

### Facets

`Setup` becomes a set of independent answers a driver declares:

| Facet | Values |
| --- | --- |
| capture | `kernel` (binfmt_needy), `shims`, `fenced-shims` |
| workspace mounts | in the guest, or from the host |
| identity | per recipe field: presented in the guest, from the host, or a gap |
| transport | vsock, a mounted Unix socket, a published Unix socket; loopback TCP with a token later |

`Error` keeps neutral variants (`Missing`, `Write`, `Mount`, `Unsupported`) and
one `Driver { driver, message }` for what a driver's tool or engine refused.
`Terminal::FirecrackerVsock` becomes a vsock variant named for the transport.

### Sidecars

A run has these sidecars. `sidecar/mod.rs` lists them all, including the one
that lives in the daemon, so a reader who meets `gateway` knows it is the
actor's network sidecar and not a gateway of something else.

| Sidecar | Channel | Domain it adapts | Lives in | Why there |
| --- | --- | --- | --- | --- |
| gateway | `gateway` (vsock port or `gateway.sock`) | net | `core-actor/sidecar/gateway/` | mechanism only; needs no run state beyond its chain |
| guest API | `guest` (vsock port or `guest.sock`) | exec (`Runs::decide`, which the actor's proxy calls on this channel as a caller bound to its run); also configuration, events, exit, files (`Guest`) | `daemon/guest.rs`, `daemon/run` (`decide`) | needs the run's routing, store and trace |

Host-side sshfs mounts of a container actor's workspace are driver work (the
driver's `Facets` say the host mounts), not a sidecar. The terminal reaches the
actor's process service directly over the `terminal` channel; the daemon's
relay is shared by every exec stream and is not per run.

## Interception domains

Interception has three domains. Each domain has its own context, decision,
trait and chain; one interceptor may serve several domains (chaos serves all
three). What differs per driver is only how a domain's events are caught
(capture) and carried to the chain (adapter). Policy never knows the driver.

| Domain | Hook points | Capture, by driver | Adapter | Interceptors, in order |
| --- | --- | --- | --- | --- |
| exec | `exec.decide` | `binfmt_needy` (linux-firecracker, macos-vz); shims (linux-container, macos-apple-container); shims in a Seatbelt fence (macos-native) | the guest proxy's `Decide` call, the same for every capture; `needy exec` for commands from outside | rules, chaos |
| net | `net.request`, `net.connect` | no network device and an in-guest forwarder (VM drivers); `--network none` and a mounted socket (container drivers); `HTTP_PROXY` and a fenced loopback port (macos-native) | the gateway, the same for every driver | allowlist, chaos, header injection |
| file | `file.open` | the workspace mount's SFTP stream, the same for every driver | the relay's stream filter | chaos (file faults) |

`exec.output` is renamed to the domain it serves: the only stream filter today
is the SFTP file-fault tap, which decides `file.open`.

Layout in `core`:

```text
core/src/intercept/
  mod.rs        # the domains table above, and the fold every chain uses
  exec.rs       # ExecContext, Decision, trait ExecDecide
  net.rs        # NetContext, NetDecision, trait NetDecide
  file.rs       # the stream filter trait and its Filtered result
```

Rules: a capture never decides; an adapter turns a capture's event into the
domain's context and applies the decision; interceptors are synchronous and
return decisions only (unchanged NOTICE). What a capture cannot see (an
absolute path under shims, in-process code under Seatbelt) is a coverage gap
the driver declares in its facets and `profile check` reports, not something
an interceptor works around.

Not interceptors, and kept apart: timed faults (the hwmon fault fires at a
time, not on an event; the daemon schedules it) and denials a fence reports
after the fact (macos-native's `actor.deny`: observation, sent to the trace).

## Lifecycles

Every long-lived object has one lifecycle: an enum, a transition table, and
one owner that advances it. The run already works this way
(`daemon/run/state.rs`: `allowed(from, to)`, `advance`, a diagram in the module
doc, tests). The others do not yet:

| Object | States | Owner today | Gap |
| --- | --- | --- | --- |
| Run | creating, running, stopping; stopped, finished, failed | `daemon/run/state.rs` | none: the model for the rest |
| Actor | (implicit) started, guest configured, terminal ready, exited | spread over `daemon/actor.rs` and each runtime | no states; "ready" was defined twice and wrong twice (vfkit and Apple `container` accept on a socket before the guest listens) |
| Instance (a run's hold on an actor or executor instance) | running; released, ended, lost | written directly by `daemon/reconcile.rs` and the pool | no transition table |
| Process | running; exited, ended, lost | written directly by `daemon/reconcile.rs` and the relay | no transition table |
| Runner | online, offline, removed | `daemon/runners.rs` | no transition table |
| Join token | valid, expired, used up, revoked | computed from its row | derived, not stored: correct as is |

Proposal:

- Instance, process and runner get the run's shape: `allowed(from, to)`, one
  `advance`, a diagram, tests; the store writes a state only through it.
- The actor gets an explicit lifecycle owned by the daemon:
  `starting -> configured -> ready -> exited`. Every driver reports the same
  events; `ready` means the process service answered a request, defined once,
  not per driver.
- Derived states (join token) stay computed.
- `InstanceRecord`'s `provider` and `runtime` strings become one `driver`
  column, with the driver names above.

## Extension kinds, side by side

| Kind | Trait | Implementations | Home |
| --- | --- | --- | --- |
| Actor driver | `core_actor::driver::Driver` | linux-firecracker, linux-container, macos-* | `core-actor/driver/` |
| Executor driver | `core_executor::driver::Driver` | native, linux-container | `core-executor/driver/` |
| Capture (mechanism) | declared by each actor driver | binfmt, shim, (seatbelt) | `core-actor/guest/capture/` |
| Interceptor (policy) | one trait per domain in `core::intercept::{exec, net, file}` | rules, chaos, allowlist, inject, file faults | `core/` (allowlist and inject move in from the gateway) |
| Sink (observation) | `trace::Sink` | sqlite, jsonl, memory | `trace/sink/` (already in this shape) |

Each hook point's chain is assembled in one place in the daemon.

## Slices

Each slice changes no behaviour, migrates config, proto, tests and current
docs together without aliases, and passes `make check` and `make test` on a Mac
and on the Linux KVM host.

1. Names: `actor.driver` and `executors.NAME.driver`, the values above, the
   `Driver` traits, `docker:` -> `container:`, proto enums, capabilities,
   `guest/init/program.rs`, `docs/TERMS_AND_CONCEPTS.md`, README and the
   AGENTS.md layout rule.
2. Layout: the `core-actor` and `core-executor` trees above, `contract`,
   `image`, no driver importing another.
3. Registry: `detect`, `check`, `reconcile` and the factory per driver; drop
   `core::types` duplicates; `Facets`, neutral `Error` and `Terminal`.
4. One engine: `core_executor::engine` used by both `linux-container` drivers.
5. Policy and capture: `core::intercept::{exec, net, file}`, allowlist and
   inject into `core`, one chain assembly per hook point, `guest/capture/`.
6. Lifecycles: transition tables for instance, process and runner; the
   actor's lifecycle with one definition of ready.

Slices 4 to 6 are independent of each other once 1 to 3 have landed.



## Decisions during implementation

### Slice 1: names

- The config enums are `profile::ActorDriver` and `profile::ExecutorDriver`:
  both are re-exported from `profile`, where one name `Driver` would collide.
  The executor's `container:` options type is `ContainerExecutor` (beside the
  actor's `ContainerActor`), its build options `ContainerBuild`.
- The executor values `containerd`, `firecracker` and `k8s`, which every
  create refused, are gone (a `TODO(executor-driver)` marks the enum), so
  `schedule::Need::Executor` holds a driver, not an `Option`.
- The profile hints for `executors.NAME.runtime` ("is now `provider`") and
  `docker.args` are gone: they named keys that no longer exist. The old keys
  (`actor.runtime`, `provider`, `docker`) get serde's unknown-field error,
  which lists `driver` and `container`. The `remote` hint stays.
- Trace schema 6: `actor.start`'s and `run.create`'s executors' `runtime`
  are `driver`. Traces of versions 1 to 5 are refused with a version error,
  not read through a translation (no compatibility).
- `state.db`: migration `m20261112_000090_drivers` copies `runtime` into a
  new `driver` column with the old names mapped and drops `provider` and
  `runtime`; it clears the runners' stored capabilities, which every
  connected runner reports again with its next heartbeat. A migration, not a
  reset, since runs, runners and clients would otherwise be lost.
- `driver::Instance::driver` (was `InstanceHandle::runtime`) keeps `runner`
  for a runner's own process space and `actor` for an actor's terminal, which
  no driver creates; `run ls` and `state.db` show the same value.
- `needy build --container` is `--linux-container`; the daemon's
  `--docker-image` is `--executor-image`; `examples/container` is
  `examples/linux-container`; proto `ExecutorRef.runtime` is `driver` (same
  field number).
- The modules are renamed with the traits (`host::runtime` is `host::driver`,
  `core_executor::provider` is `core_executor::driver`); the files move in
  slice 2. The `Docker`/`AppleContainer` error variants and
  `Terminal::FirecrackerVsock` keep their names until slice 3.
- The report's note on what shim routing leaves out fires for
  `linux-container` actors, as it did for `container`; `macos-apple-container`
  routes by shims too but never had the note (candidate below).

### Slice 2: layout

- The `core-actor` tree is the design's, with `guest/capture/` left to slice
  5. `driver/mod.rs` and `sidecar/mod.rs` carry the tables; `lib.rs` lists
  the components.
- `contract.rs` holds the vsock ports and the container channel paths
  (`CHANNELS`, the three socket names, `channels_dir`), which `macos-vz` took
  from Firecracker and `macos-apple-container` from the container driver.
- `image::vm` holds both VM images as `vm::LinuxFirecracker` and
  `vm::MacosVz` (each driver re-exports its own as `Image`);
  `image::container` holds the container's name for a run, the one thing
  both container drivers share beyond the contract.
- The four copies of the actor's size from a recipe (2 vCPUs and 2 GiB
  without one) are one `driver::size`: it is part of what every driver
  presents, so it lives with the trait, not in a component.
- `linux_firecracker::kvm` is the old `host::detect` plus `KVM` and
  `check_kvm`; the CLI and the daemon read the control host's CPU through it
  (`kvm::detect`), as the design's layout says. The cpuinfo fixture moved with
  it (`linux_firecracker/testdata`).
- `core-executor`'s `linux_container` keeps the instance lifecycle in
  `mod.rs` and the container body and run-option policy (`container`,
  `apply_run_options`, socket paths) in `options.rs`; its tests stay in one
  `mod_test.rs`, which shares its helpers between both halves.
- `examples/`, `images/Dockerfile`, `needy.patch` and the GCE recipe named
  `host/vm.rs`, which no longer existed; they name
  `driver/linux_firecracker/config.rs` now.

### Slice 3: registry

- The registry is the extension point's own `mod.rs`: `core_actor::driver`
  has `new` (the factory, for a profile's `actor:`), `detect`, `reconcile`
  and `facets`, each a match over `ActorDriver` that calls the driver's
  module; `core_executor::driver` has `detect`. These are the only matches
  on a driver outside the config and proto boundaries (`api-server`'s and
  the CLI's proto conversions). The daemon's `actor::driver_for` wraps `new`
  with the daemon's image and KVM paths (`driver::Host`).
- `detect` and `reconcile` are free functions in each driver's module, not
  trait methods: a runner detects with no profile, and a restarted daemon
  reconciles with only a `state.db` row (`driver::Leftover`), so neither has
  the options a `Driver` is built from. `end_leftover_vm` moved from the
  daemon into `driver::end_vm`, which both VM drivers call (core-actor now
  depends on `sysinfo` on Unix).
- An executor driver's leftover is its own: `Driver::reconcile(id)` (the
  `linux-container` driver removes the container; `native` ends nothing and
  carries the `TODO(runner-ownership)` that was in the daemon). The pool
  hands a driver out by kind (`Pool::driver`).
- Facets are per driver, not per instance: each module has a `FACETS`
  constant and `driver::facets(kind)` returns it, so `profile check` reads
  them without building a driver (a profile whose `container:` options are
  missing still gets the shim warning, as before). Identity is per field
  only for the fields the daemon acts on (`hostname`, `hwmon`, `uptime`;
  `None` is a gap); the CPU, DMI, PCI and kernel fields have no consumer and
  stay in `identity_gaps` (a `NOTICE` says so). `Transport::Socket` replaces
  `Channels::guest_for_every_user`.
- `detect` builds on every platform, since a runner on Windows detects too
  (the container drivers report themselves missing there); `new`, `facets`
  and `reconcile` are Unix only, as the container drivers and the daemon's
  actor role are.
- A runner now probes the engine twice, once for `actor(linux-container)` and
  once for `executor(linux-container)`, each bounded by its own 3 s; slice 4's
  one engine client can share the answer. Missing lines list the actor
  drivers, then the executor drivers, in the enums' order.
- `Error::Driver { driver, message }` replaces `Spawn`, `Kvm`, `Docker` and
  `AppleContainer`; the messages are unchanged (the KVM advice is built in
  `kvm::check_kvm`). `Terminal::FirecrackerVsock` is `Terminal::HybridVsock`,
  after the transport Firecracker documents. `config/validate.rs` keeps the
  `container:` option checks, since `needy-config` cannot depend on drivers.
- `core::types::{ActorDriver, ExecutorDriver}` are gone; `Capabilities` holds
  the config enums, and `api-server` depends on `needy-config` for them.
  `schedule::Need` holds them too, so `Need::actor` and `Need::executor`, the
  conversions, are gone.

### For slices 4 to 6

- Slice 4 (one engine): the actor's `linux_container` keeps its own
  `Engine` trait and `bollard` client, with `refused()` mapping engine errors
  to `Error::Driver`; the executor's `linux_container` has its own
  `detect` and `Error::Docker(bollard)` (not yet neutral). Both `detect`s ask
  the engine for `info` within 3 s.
- Slice 5 (policy and capture): `Facets::capture` (`Kernel`, `Shims`, with a
  `TODO(macos-native)` for fenced shims) is where a driver declares its
  capture, and `check.rs` already reads it for the shim warning; the gateway
  is `sidecar::gateway`, with the guest API listed in `sidecar/mod.rs`.
- Slice 6 (lifecycles): the actor's start and end are in `daemon/actor.rs`
  (`start`, `watch`, `release_actor`); instance states are written by
  `reconcile.rs` through `end_leftover_actor` and `end_leftover_executor`,
  which return the state to write.

## Candidate next slices

- The guest dials the guest API at a literal `vsock://2:7000`
  (`guest/mod.rs`); it could build the address from `contract::GUEST_PORT`,
  which slice 5 (captures and the guest) may do in passing.
- The report could name its shim-routing note from the trace's driver
  through the facets instead of the label's prefix; it needs the driver in
  `actor.start` as a typed value, not the label.
- The report's shim-routing note for `macos-apple-container` actors: they have
  the same gap as `linux-container` (`profile check` already warns for both).
