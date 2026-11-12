# Drivers and extension points

Status: accepted by the owner (2026-11-12); slices 1 to 6 implemented 2026-11-12 (see [Verification of slices 1 to 3](#verification-of-slices-1-to-3), [Slice 4](#slice-4-one-engine), [Slice 5](#slice-5-policy-and-capture) and [Slice 6: lifecycles](#slice-6-lifecycles)). Breaking API, config and proto changes are allowed; implementers may refine the design and record each refinement under "Decisions during implementation".

> **NOTICE (2026-11-12, migration):** the
> [profile schema redesign](../config/2026-11-09-profile-and-rules-design.md#schema-redesign-2026-11-12)
> removed header injection (`inject`, `core::network::Inject`) from the net
> chain, which is now the allowlist then chaos (`TODO(mitm)`); `upstream`
> became `actor.network.services`; chaos faults became a named map split by
> domain (`exec`, `file`, `net`, `sensor`) and the `file.open` chain is
> installed on every workspace mount (`file::chain(injector, root, target)`).
> The text below keeps the names of its time.

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

(The table is the state before slice 6; see [Slice 6: lifecycles](#slice-6-lifecycles) for where each lives now.)
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

Each domain's chain is assembled in one place, in `core::intercept` (a
slice 5 decision; the daemon and the gateway only adapt).

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

### Slice 4: one engine

- `core_executor::engine` is the one client: the `Engine` trait (runtimes,
  image presence, pull, create, start, logs, wait, stop, remove), the
  `bollard` implementation, and `Unreachable`, the stand-in for an engine
  that cannot be reached, which stays the injection boundary both drivers'
  tests use (`with_engine` on each). `connect` reaches `DOCKER_HOST` (else
  the default socket); `local` turns a failed connect into `Unreachable`.
  The engine's types come through `engine::models` (bollard's), so
  `core-actor` no longer depends on `bollard`; it depends on
  `core-executor` on every platform, since the actor `detect` that every
  platform builds takes the engine's probe.
- `engine::Error` is neutral: `Unreachable`, `NotFound` (the engine's 404)
  and `Refused`. The executor's `Error::Docker(bollard)` is
  `Error::Engine(engine::Error)`; the actor maps an engine error to
  `Error::Driver` as before. Messages keep `docker: ...` for what the
  engine refused; an engine that does not answer says `no Docker-compatible
  engine answers: ...` for both drivers (the actor said `no Docker daemon
  answers`).
- The status codes the drivers treat as success live in the engine: 304 to
  a stop (not running), 404 and 409 to a removal (gone, or `--rm` already
  removing it). Not-found is an error kind (`Error::is_not_found`), so the
  executor's pull of a missing image on create (as `docker run` does) is
  the driver's policy over `Engine::pull`, which adds `latest` to a bare
  name. The executor's start now also inspects the container for its PID,
  which it ignores; one call, so the trait has one `start`.
- Detection shares one answer: `engine::Probe` asks the engine once (3 s)
  and both `linux-container` `detect`s read it; `core_actor::driver::detect`
  and `core_executor::driver::detect` take the probe, and the daemon's
  `capabilities::probe` makes one per probe. An explicit value rather than a
  process-wide cache, so a later probe asks again.
- Labels are the engine's: `engine::labels(run, role)` sets `needy.run` and
  `needy.role`, and executor containers now carry `needy.role: executor`
  beside the actor's `actor`. Nothing lists containers by label yet
  (`TODO(engine)` on `labels`): reconcile still finds leftovers by the
  names `state.db` has, and listing by label would change what it ends.
- Verified on 2026-11-12: `make check` and `make test` pass on an Apple
  silicon Mac and on the Linux KVM host (the executor's container test runs
  there against the engine). End to end, with images built from the slice's
  tree and each control daemon in a scratch state directory:
  `examples/smoke` (`linux-firecracker` actor, `linux-container` executor:
  routed `python3`, the sshfs workspace both ways, `routed exit=7`, the
  executor's container labelled `needy.role=executor` and gone after the
  run); `examples/linux-container` with `runc` and without its workspace
  mount (no `user_allow_other` on that host), as in slices 1 to 3: recipe
  identity, routed `python3`, report `Actor linux-container (runc)`, no
  container left; and `examples/macos-apple-container` on the Mac (unchanged
  driver): workspace both ways, routed `python3`, gateway allows
  `example.com` and denies `example.org`.

### Verification of slices 1 to 3

`make check` and `make test` pass after each slice on an Apple silicon Mac
and on the Linux KVM host. End to end, from `7c20af5`, with the images
built from that tree (`needy build`, `--linux-container`, `--executor`,
`--macos-apple-container`) and each control daemon in a scratch state
directory, on 2026-11-12:

| Driver | Host | Profile | Result |
| --- | --- | --- | --- |
| `linux-firecracker` | Linux KVM host | `examples/smoke` | Pass: GCE identity (`systemd-detect-virt` says `google`), `python3` routed to a `linux-container` executor over the sshfs workspace in both directions, `routed exit=7`, report `Actor linux-firecracker` |
| `linux-container` (`runc`) | Linux KVM host | `examples/linux-container` without its workspace mount | Pass: recipe hostname, CPU brand and DMI, 2 CPUs in `/proc/cpuinfo`, `python3` routed, report `Actor linux-container (runc)` with the shim note, no container left |
| `linux-container` (`runc`) | Linux KVM host | `examples/linux-container` | Not run to the end: the host-side sshfs mount needs `user_allow_other` in `/etc/fuse.conf` for a daemon that is not root, which this host has off (no sudo); the run failed with `cannot mount /home/agent/workspace/ on the host: sshfs did not mount within 10 s` and released its executor |
| `macos-vz` | Apple silicon Mac | `examples/macos-vz` | Pass: arm64 guest, sensors, `python3` and `/usr/bin/arch` by absolute path routed to the Mac, workspace both ways |
| `macos-apple-container` | Apple silicon Mac | `examples/macos-apple-container` | Pass: workspace both ways, `python3` routed, gateway allows `example.com` and denies `example.org` |

### For slices 4 to 6

- Slice 4 (one engine): the actor's `linux_container` keeps its own
  `Engine` trait and `bollard` client, with `refused()` mapping engine errors
  to `Error::Driver`; the executor's `linux_container` has its own
  `detect` and `Error::Docker(bollard)` (not yet neutral). Both `detect`s ask
  the engine for `info` within 3 s. (Done: see [Slice 4](#slice-4-one-engine).)
- Slice 5 (policy and capture): `Facets::capture` (`Kernel`, `Shims`, with a
  `TODO(macos-native)` for fenced shims) is where a driver declares its
  capture, and `check.rs` already reads it for the shim warning; the gateway
  is `sidecar::gateway`, with the guest API listed in `sidecar/mod.rs`.
  (Done: see [Slice 5](#slice-5-policy-and-capture).)
- Slice 6 (lifecycles): the actor's start and end are in `daemon/actor.rs`
  (`start`, `watch`, `release_actor`); instance states are written by
  `reconcile.rs` through `end_leftover_actor` and `end_leftover_executor`,
  which return the state to write.

### Slice 5: policy and capture

- `core::intercept` is `intercept/{mod, exec, net, file}.rs`. `mod.rs` has
  the domains table (hook points, context and decision, trait, chain
  assembly, interceptors, adapter) and the private `fold` every domain's
  `decide` uses. The decisions moved with their domains:
  `types::Decision` is `intercept::exec::Decision`, `types::NetDecision`
  and `NetOutcome` are in `intercept::net`; `Outcome`, `Placement` and
  `Fired` stay in `types`, since events carry them.
- The chains are assembled in `core`, not in the daemon as the
  [Extension kinds](#extension-kinds-side-by-side) section said: one
  `exec::Chain::new(profile, injector, executor)` (rules, then chaos; a
  command placed on an executor by name skips the rules), one
  `net::Chain::new(policy, injector)` (allowlist, chaos, header injection)
  and one `file::chain(injector, root)` (the SFTP file faults, or no filter
  when the run has no `file.open` faults). `core` already holds every
  interceptor, so the order lives next to them, and the adapters (the
  daemon's `routing` and `relay::filter`, the gateway) only build a context,
  ask the chain and apply the decision. The table in `intercept/mod.rs`
  points to each.
- The hook point `exec.output` is `file.open`, and its trait is
  `intercept::file::StreamFilter` with `Filtered`. No trace event or timing
  key carried `exec.output`, so no stored name changed. The daemon's
  `FileFaults` filter moved to `chaos::sftp::FileFaults`, beside the tap it
  wraps; `relay/filter.rs` keeps only the adapter's choice of stream (an
  actor's workspace mount).
- The gateway's policy is `core::network` (`Policy`, `Allowlist`,
  `Inject`), named after the profile's `network:` rather than `net`, so it
  is not confused with the `intercept::net` domain. `sidecar::gateway` keeps
  the upstream map (where an allowed request is forwarded, which is
  mechanism) and builds its chain from the policy in `Gateway::new`, whose
  signature is unchanged. New unit tests in `core/src/network_test.rs` cover
  the chain without a gateway; the gateway's tests stay as they were.
- `guest/capture/` holds `binfmt.rs` (the `binfmt_needy` parameters, moved
  out of init) and `shim.rs` (moved from `guest/shim.rs` with its tests).
  `capture::Route` (the route's user and names, and `routes`, which mirrors
  the module's `listed`) is shared by both, so `binfmt` does not depend on
  `shim`; the route file's format (`Route::parse`, `render`) stays in
  `shim.rs`. Init turns routing on through one `capture::enable(route,
  container)`. Route stubs stay in `guest/init/stubs.rs` (candidate below).
- `contract` also holds needy's paths in an actor image: `NEEDY_DIR`,
  `INIT`, `PROXY` and `REAL`, which the CLI's dispatch by `argv[0]`, the
  stubs and the shims used as literals. The guest builds the guest API's
  address from `libc::VMADDR_CID_HOST` and `contract::GUEST_PORT` instead
  of the literal `vsock://2:7000`.
- The report's shim-routing note follows the actor's capture, not the
  driver's label: `types::Capture` moved from `core-actor`'s driver module
  to `core` (the report cannot depend on `core-actor`), the daemon records
  the driver's `Facets::capture` in `actor.start`'s new `capture`, and the
  summary has `actor_capture`. A `macos-apple-container` actor's report now
  has the note (`an_apple_container_actors_report_says_what_its_shim_routing_leaves_out`).
  The trace schema is version 8 (slice 6 made 7 for `actor.ready`); as
  for 7, only the current version is read, although 8 only adds an
  optional field.

### Slice 6: lifecycles

- Instance, process and runner states live in `store::lifecycle`, each with
  `allowed(from, to)`, the diagrams in its module doc, and tests
  (`lifecycle_test.rs`). The store writes them only through
  `start_instance` / `advance_instance`, `start_process` /
  `advance_process` and `remove`; `save_instance`, `set_instance_state`,
  `save_process` and `end_process` are gone. A start of a row that exists
  (an upsert before) is refused too. A refusal is `store::Error::Refused`
  ("instance R:executor:mac is ended; it cannot become lost"); the daemon's
  `advance_instance` and `advance_process` (in `reconcile.rs`, the
  ownership module) log it at debug, as a run's refused transitions are:
  it is a race of two enders, and the first end stays. Before, the last
  writer won: a relay's `exited` over a kill's `ended`, or the report
  wait's `lost` over a runner's `ended`.
- Every end state is terminal, as a run's is. `lost` is not revisited by a
  runner that reports later (a `NOTICE` on the variant): the runner is told
  to end the run's processes, but whether the instance ran meanwhile stays
  unknown.
- The runner's stored lifecycle is `joined -> removed`
  (`RunnerState`); online and offline are derived from the last heartbeat
  in `runners.rs`, like a join token's states, since storing them would need
  a writer on a timer. Removing a removed runner is refused
  ("runner mac is removed; it cannot become removed", `FailedPrecondition`
  through `ControlError::Failed`) instead of `no runner mac`; heartbeats,
  connects and rotations of a removed runner still get `no runner`.
- The actor's lifecycle is `actor::state` (`daemon/actor/state.rs`, beside
  `actor.rs`, which stays put): `None` until `actor::start`, then
  `starting -> configured -> ready -> exited`, any started state may exit,
  `exited` is terminal. `core::types::ActorState` is the enum; `RunInfo`
  and proto `Run.actor_state` carry it, `run ls` shows an `ACTOR` column.
  `state.db` does not keep it (a `NOTICE` in `stored_info`): an ended run's
  actor exited, and a reconciled one's was lost.
- Ready is defined once, in `actor::answers`: the process service answered
  a health call (`Unimplemented` counts), on every transport. The hybrid
  vsock path asks too, though Firecracker's `OK` already means the guest
  listens. `actor::terminal` advances to `ready` and emits `actor.ready` on
  its first answer; for a run with a terminal the daemon probes right after
  start (`await_ready`) and keeps the connection in `run.terminal`, which
  the first attach reuses. A driving program without a terminal has no
  process service, so its actor goes from `configured` to `exited`; giving
  every guest a process service is a candidate below.
- Trace schema 7 adds `actor.ready` (empty payload). `starting`,
  `configured` and `exited` are recorded by the existing `actor.start`,
  `actor.config` and `actor.vm.exit` (not on a stop, which `run.stop`
  records). Only version 7 is read, as before.
- The executor process host's own states (`core-executor/src/process`) are
  a `watch` of an optional exit plus an expiry time: one writer, two states,
  no table needed. Unchanged.

Verification, 2026-11-12: `make check` and `make test` pass on an Apple
silicon Mac and on the Linux KVM host (775 tests there). End to end, with
images built from the slice's tree and each control daemon in a scratch
state directory:

| Driver | Host | What | Result |
| --- | --- | --- | --- |
| `linux-firecracker` | Linux KVM host | `examples/smoke` to its end | Pass: `run ls` showed `running starting`, then `finished exited`; the trace has `actor.start`, `actor.config`, `actor.exit`, `actor.vm.exit` and no `actor.ready` (no terminal); routed `exit=7`, workspace both ways |
| `linux-firecracker` | Linux KVM host | `examples/smoke` with `-t` and a long program, then `run stop` | Pass: `running ready` in `run ls`, `actor.ready` 167 ms after `actor.config`; after the stop `stopped`, `-`, executor released |
| `linux-firecracker` | Linux KVM host | a running run whose daemon was killed with `SIGKILL`, then a restarted daemon | Pass: the run `failed` ("the control plane restarted; the actor was lost"), its Firecracker and executor container ended, `state.db` has both instances `ended` and both processes `ended` |
| `macos-vz` | Apple silicon Mac | `examples/macos-vz` with `run create -it` | Pass: the program's output on the terminal, exit code 3 returned; trace `actor.config` then `actor.ready` 345 ms later, then `actor.exit` |
| `macos-vz` | Apple silicon Mac | `examples/macos-vz` with `-t`, then `run stop` | Pass: `running starting`, then `running ready` in `run ls`; `stopped` after the stop |

Verification: `make check` and `make test` pass on an Apple silicon Mac and
on the Linux KVM host (where the guest code compiles). End to end on
2026-11-12, with the VM and container actor images built from the slice 5
tree and each control daemon in a scratch state directory:

| Driver | Host | Profile | Result |
| --- | --- | --- | --- |
| `linux-firecracker` | Linux KVM host | `examples/smoke` | Pass: GCE identity, `python3` routed through `binfmt_needy` to a `linux-container` executor, the workspace both ways, `routed exit=7`; report `actor_capture: kernel`, no shim note |
| `linux-firecracker` | Linux KVM host | `examples/chaos` | Pass: copy-in without the filtered file, `cat broken.json` fails with EIO (the `file.open` chain on the SFTP stream), `curl` refused with ECONNREFUSED (126), the hwmon fault at 101 °C, mirror-out written |
| `linux-firecracker` | Linux KVM host | a small profile: `allow: [example.com]`, `upstream` and `inject` for `model.needy` to a local echo server | Pass: `example.com` 200, `example.org` 403 with `needy: example.org is not allowed` and a `network denied` finding, the echo server saw the injected `authorization` header |
| `linux-container` (`runc`) | Linux KVM host | `examples/linux-container` without its workspace mount (as in slices 1 to 3) | Pass: shims route `python3`, `ls` stays local; report `actor_capture: shims` with the shim note, no container left |
| `macos-vz` | Apple silicon Mac | `examples/macos-vz` | Pass: the guest reached the guest API on vsock port 7000 from the contract, `python3` and `/usr/bin/arch` routed to the Mac, workspace both ways; report `actor_capture: kernel` |

The table is from the slice 5 tree on slices 1 to 4. After rebasing onto
slice 6, `make check` and `make test` pass again on both hosts, and
`examples/smoke` and the gateway profile pass again on the Linux KVM host
with a rebuilt VM image. `macos-apple-container` was not run end to end;
its report note is covered by the unit test above.

## Candidate next slices

- `needy build` (`cli/commands/build.rs`) builds the container images through
  its own `bollard` client; connecting through `engine::connect` would give
  it the same `DOCKER_HOST` handling and errors (the build itself, BuildKit's
  session, stays the CLI's).
- Reconcile by label: `engine::labels` makes every needy container findable
  by run and role, so a restarted daemon could end containers whose
  `state.db` rows are gone, which today stay running.
- Every guest could serve its process service, with or without a terminal,
  so `ready` holds for every actor and `needy exec --run` could reach the
  actor itself.
- The report could show how long an actor took to become ready
  (`actor.start` to `actor.ready`), the boot time a user waits for.
- `state.db` could keep the actor state of ended runs, if `run ls` should
  show where an actor stopped (for example a run that failed while
  `starting`).
- Route stubs (`guest/init/stubs.rs`) exist because a capture can only
  catch a program that exists; they could move into `guest/capture/` as the
  part both captures share, with init only calling them.
- `NEEDY_DIR`, `INIT`, `PROXY` and `REAL` in `contract` are still written
  out again in `images/Dockerfile`, `images/container/shim.sh` and
  `binfmt_needy.c`; a build-time check that they agree would catch a drift.
