# Module layout redesign

**Status**: accepted, 2026-11-09. Slices are implemented in order; see
[Slices](#slices).

This note redesigns the workspace: the flows, the module tree of each crate,
which code moves where, and which libraries replace hand-written code. It
follows the conventions of [auv](https://github.com/moeru-ai/auv) where they
fit. Three crates are added: `config`, `trace` and `xtask`.

Related notes:

- [Actor and executor separation](2026-11-09-actor-executor-separation-design.md)
- [Closed-loop results](2026-11-09-closed-loop-results-evidence.md): the
  behavior this redesign must keep.

## Why

The closed loop was built to prove each flow end to end, and the code still has
that shape:

- `daemon/src/control.rs` (1376 lines) holds session lifecycle, the instance
  pool, Docker shell-outs, Firecracker start-up, file sync, routing, chaos,
  recordings and the implementations of three gRPC services. The services
  tell the actor apart from the CLI with an ad hoc `bound` field.
- Mechanisms and policies are mixed. How to start a container or a VM lives in
  the daemon; Docker flag policy lives in `core::profile`; terminal handling
  lives in `core-executor::client`.
- `core` mixes what users write (profile and recipe schemas, `extends`,
  built-ins) with how it is interpreted. `rules` and `recipe` are single
  1000-line files. Command patterns are matched twice, basenames are computed
  four times, and `extends` has two merge models.
- Observation is scattered: `trace.record` calls, chaos checks and recording
  branches sit inline in the daemon and the gateway. Event kinds, attribute
  names, session states, fault kinds and exec origins are strings shared by
  convention between the daemon, the gateway and the report.
- There is no logging (`eprintln!` in 24 places), errors are dropped with
  `let _ =` in 45 places, gRPC statuses are flattened to strings in 12, and
  paths are `$HOME/.needy`.
- Defects that follow from the shape: `Session.actor` is never set, so
  stopping a session does not stop its VM; listener tasks and sockets leak; a
  failed session create leaks the instances it already acquired; the pool lock
  is held across `docker run`; the daemon never fills `ancestors`, so
  `when.under` cannot match.

## Principles

1. **What users write, what it means, how it runs.** `config` parses, merges
   and validates profiles and recipes. `core` interprets them: rules, recipe
   compilation, chaos. `core-actor` and `core-executor` are mechanisms: boot a
   VM, run a process, create an executor instance. `daemon` is policy: when to
   do what, for which session.
2. **Observation and interception are hooks.** The flow exposes named hook
   points. Interceptors may change the outcome (rules, chaos, network policy);
   observers only see it (trace sinks, recordings, timing). Neither is written
   inline in the flow.
3. **Transport is a layer.** gRPC types stop at `api-server` (inbound) and
   `api-client` (outbound). The daemon implements domain traits; adapters map
   proto to domain and errors to statuses in one place.
4. **Every shared string becomes a type.**
5. **Every task has an owner.** Long-lived tasks are spawned into a
   `TaskTracker` under a `CancellationToken` owned by the daemon, a session or
   an actor. Stopping the owner stops and awaits them.
6. **A library before hand-written code.** See [Libraries](#libraries).

## Crates and dependency direction

| Crate | Owns |
| --- | --- |
| `cli` | Commands, output formats, paths, logging setup |
| `daemon` | Control plane policy: sessions, pool, routing, relay, persistence |
| `api-server` | Listeners, authentication, gRPC adapters, status mapping |
| `api-client` | Connections, exec streams, file transfer, readiness |
| `api-proto` | Generated types |
| `core-actor` | The guest (init, proxy) and its host side (VM, gateway) |
| `core-executor` | Processes, executor providers, the terminal relay |
| `core` | Rules engine, recipe compilation, chaos, interceptor traits, domain types |
| `trace` | Event schema, observer dispatch, sinks, reports |
| `config` | Profile and recipe schemas, loading, merging, validation, built-ins |
| `common` | Helpers with no needy concepts and no async runtime |
| `xtask` | Developer tasks: benchmarks |

```
cli ─────────┐
daemon ──────┼──► api-server ─┐
             │    api-client ─┴──► api-proto
             ├──► core-actor ────┐
             ├──► core-executor ─┼──► core ──► config ──► common
             └───────────────────┴──► trace ─► config
```

`core-actor` and `core-executor` may use `api-client`. `config`, `core` and
`common` never see gRPC, processes or sockets. `trace` reaches the filesystem
and SQLite only through feature-gated sinks.

### Where types live

| Crate | Types |
| --- | --- |
| `common` | Domain-free: archive entries, filters, `Exit` |
| `config` | Values users write: `Duration`, `ByteSize`, `Words`, `Glob`, `Template`, and the enums in the schema (`FaultKind`, `Runtime`, `OnFull`, `FileMode`, `Side`) |
| `core` | Values the system produces: `Decision`, `Placement`, `SessionState`, `Origin`, `Fired` |
| `trace` | Events: `EventKind` and one typed payload per kind |

## Hooks

| Point | Interceptors (in order) | Observers |
| --- | --- | --- |
| `session.create`, `session.stop` | — | trace |
| `exec.decide` | rules → chaos → a user decider | trace, timing |
| `exec.start`, `exec.output`, `exec.exit` | stream filters: SFTP fault tap | trace, recording, timing |
| `net.request`, `net.connect` | allowlist → chaos → header injection | trace, timing |
| `file.copy_in`, `file.mirror_out` | — | trace |
| `actor.*` (config, exit, guest events) | — | trace |

- **Interceptors** are traits in `core::intercept`, one per point, for example
  `async fn decide(&self, ctx: &ExecContext) -> Decision`. The user decider
  is the asynchronous `Promise<Decision>` function agreed earlier; it plugs in
  here.
- **Observers** subscribe through `trace::Dispatch`. Dispatch never blocks the
  flow: events go through a bounded queue, and a full queue drops and counts
  rather than waits.
- **Configuration** is per profile:

  ```yaml
  trace:
    sinks: [sqlite, jsonl]
    record: tty       # tty | all | none
    timing: true
  ```

- **Features** gate the sinks at build time: `sqlite`, `jsonl`, `asciicast`,
  `memory` (tests), later `otel`. The guest binary builds `trace` with no
  sinks; it only forwards events to the daemon.

`trace` is product observation. Process logs use the `tracing` library and
are a separate concern; the crate is named `trace` so the two do not blur.

## Flows

Each step names the module that owns it after the redesign.

### Daemon start

1. `cli::commands::daemon` resolves `cli::paths` (XDG) and builds
   `daemon::Config` (state, data and runtime directories; images; token).
2. `daemon::Daemon::start` opens `daemon::store` (`state.db`), reconciles
   persisted sessions and instances (slice 9), builds the trace dispatch with
   the configured sinks (`trace.db`, JSONL), and starts the in-process local
   executor from `core-executor::process::Host`.
3. `api-server::server::Server::bind` binds every listener (Unix, TCP), then
   serves the adapters in `api-server::protocol::grpc` against the daemon's
   domain traits. `api-server::termination` turns SIGINT, SIGTERM and SIGHUP
   into cancellation; `Daemon::shutdown` stops sessions in order.

### Create a session

1. `api-server` adapter → `daemon::session::Sessions::create`.
2. `config::profile::load` and, when named, `config::recipe::load`;
   `config::validate` reports findings. `core::recipe::compile` produces the
   VM identity, with `core-actor::host::detect` for the host CPU.
3. For each executor entry: `daemon::pool::Pool::acquire(key)`, which calls a
   `core-executor::provider::Provider` (`native`, `remote`, `docker`). The
   pool lock covers only the map; provisioning runs outside it behind a
   per-key `OnceCell`.
4. `daemon::files::copy_in` through `api-client::files` (tar over exec).
5. `core-actor::host::vm::Vm::start` boots Firecracker and returns a handle
   that owns the child. `daemon::actor` wires the guest listener and the
   gateway to the session's cancellation token.
6. Any failure unwinds the steps already done (release instances, stop the
   VM, remove the directory) and emits `session.fail`.

Session state is `core::SessionState` (`Creating`, `Running`, `Stopping`,
`Stopped`, `Finished`, `Failed`) with transitions in one place; the first
terminal state wins.

### Routed exec from the actor

1. `core-actor::guest::proxy` asks `Decide` over vsock with the process's
   ancestry. The guest listener authenticates `Caller::Actor { session }`.
2. `daemon::routing` runs the `exec.decide` interceptors: `core::rules`, then
   `core::chaos`, then a user decider if the profile names one. It applies the
   executor's path map.
3. Local placement: the proxy `execve`s the real program. Otherwise it calls
   `ProcessService.Exec` with `Origin::ActorProxy`.
4. `daemon::relay` takes the instance's slot (`daemon::pool`), opens the
   upstream stream with `api-client::exec`, and relays both ways through the
   session's stream filters. Observers receive `exec.start`, `exec.output`
   and `exec.exit`.
5. `core-executor::relay` on the proxy side owns the terminal (raw mode,
   resize, signal forwarding) and returns the exit status.

### Workspace mount

`core-actor::guest::init::sshfs` starts `sshfs -o passive` and bridges it to
an `Exec` stream with `Origin::ActorMount`. The relay installs the SFTP fault
tap as a stream filter when the profile has `file.open` faults.

### Network from the actor

`core-actor::host::gateway` runs the `net.*` interceptors (allowlist, chaos,
header injection) and emits `net.*` events. The gateway no longer calls the
trace store or the chaos injector directly.

### Stop a session

`daemon::session::Sessions::stop` moves the state to `Stopping`, cancels the
session token (the guest listener, the gateway and the relays stop), stops
the VM through its handle, runs `daemon::files::mirror_out`, releases
instances (`Provider::destroy` at refcount zero), emits `session.stop`, and
removes the socket files.

### Sessionless `needy exec`

`daemon::relay` with no session uses the local executor or a named instance;
an ambiguous name is an error instead of the first match.

### Reports

`needy report` calls `trace::report`, which reads typed rows through the
`sqlite` sink's reader. The report no longer reads untyped maps.

## Crate layouts

Tests follow auv: a sibling `foo_test.rs` wired with
`#[cfg(test)] #[path = "foo_test.rs"] mod tests;`. Each module starts with a
`//!` comment that says what it owns. Platform code sits in `platform/` with
`#[cfg]` on the `mod` lines.

### cli

```
src/
  main.rs            argv[0] dispatch (/.needy/init, /.needy/proxy), runtime, cli::run
  cli.rs             root clap types, global flags, dispatch, exit codes
  paths.rs           XDG directories (etcetera)
  logging.rs         tracing-subscriber setup, -v levels, daemon log file
  output/
    mod.rs           OutputFormat (table, json), render helpers
    table.rs         comfy-table, borderless; rows are typed structs
  commands/
    mod.rs
    daemon.rs  exec.rs  attach.rs  ps.rs  kill.rs  fs.rs
    session.rs  build.rs  profile.rs  report.rs
```

Each command module owns its `Args` and an
`async fn run(args, ctx) -> anyhow::Result<i32>`. `exec` keeps its JSON and
NDJSON modes here; they are output formats, not executor behavior. `build`
uses `bollard`'s build API instead of `docker build`. `profile check` splits
`--cmd` with `shell-words`.

### config

```
src/
  lib.rs
  source.rs          files, builtin:NAME, extends chains, base_dir, depth limit
  merge.rs           one merge model for profiles and recipes
  value.rs           Duration, ByteSize, Words, Glob, Template
  validate.rs        cross-references (executors, presets, defaults) -> Finding
  profile/
    mod.rs           Profile, load
    actor.rs  executor.rs  rules.rs  files.rs  network.rs  chaos.rs  trace.rs
  recipe/
    mod.rs           Recipe, load
    cpu.rs  kernel.rs  firmware.rs  devices.rs  network.rs
  builtin/
    mod.rs           the embedded catalog
    recipes/*.yaml
```

**One merge model.** Maps merge recursively. A child's scalar or list
replaces the parent's, except lists the schema declares as prepend
(`commands`, `resolvers`), where the child's entries come first. Both
`profile` and `recipe` go through `merge.rs`; the typed `RawProfile::merge`
and the recipe's YAML deep merge both go away.

Unknown keys are errors (`deny_unknown_fields`) except in maps meant to be
open. `When`'s flattened capture globs move to an explicit `captures:` map so
a typo is no longer read as a capture.

### core

```
src/
  lib.rs
  types.rs           Decision, Placement, SessionState, Origin
  pattern.rs         command patterns: compile once, match, captures, basename
  intercept.rs       interceptor traits per hook point; the chain
  rules/
    mod.rs           decide, route_names
    when.rs
    actions.rs       one merge of preset and rule actions
    launcher.rs      built-in and user launcher resolvers
    shell.rs         sh -c scripts
  recipe/
    mod.rs           compile -> CompiledRecipe, VmIdentity
    identity.rs      placeholders, SMBIOS, PCI IDs, chassis numbers
    cpu.rs           CPUID encoding, feature table, model checks
    host.rs          HostInfo and parsing /proc/cpuinfo text
  chaos/
    mod.rs           Injector (an interceptor)
    sftp.rs          SFTP packet framing for the file.open tap (pure bytes)
```

Moves out of `core`: schemas and loading to `config`; `trace`, `store` and
`report` to `trace`; Docker flag policy to
`core-executor::provider::docker`; reading `/proc/cpuinfo` to
`core-actor::host::detect`. `recipe::compile` returns a typed `VmIdentity`
instead of Firecracker environment names; `core-actor::host::vm` maps it to
the patched Firecracker's variables.

### trace

```
src/
  lib.rs
  event.rs           EventKind, typed payloads, schema version
  dispatch.rs        Dispatch: bounded queue, observers, drop counter
  sink/
    mod.rs           Sink trait
    sqlite.rs        feature "sqlite": SeaORM entities, migrations, reader
    jsonl.rs         feature "jsonl"
    asciicast.rs     feature "asciicast"
    memory.rs        feature "memory"
  timing.rs          per-hook latency (feature "timing")
  report/
    mod.rs           render from typed rows (feature "report", needs "sqlite")
```

### core-executor

```
src/
  lib.rs
  process/
    mod.rs           Host: process table, ids, expiry
    proc.rs          Proc: output fan-out, replay, wait
    pipes.rs
    pty.rs
    platform/
      unix.rs        process groups, waitpid, killpg
      windows.rs     job objects, termination
  signals.rs         names <-> numbers, shell exit codes
  service.rs         ExecutorService (ProcessService for one machine)
  provider/
    mod.rs           Provider trait, InstanceSpec, InstanceHandle
    native.rs  remote.rs  docker.rs (bollard)
  relay/
    mod.rs           local stdio <-> ExecStream, exit status
    terminal.rs      raw mode, window size, resize (crossterm)
    signals.rs       forwarding SIGINT/TERM/HUP/QUIT/WINCH
```

`provider` is the executor runtime abstraction:
`async fn create(&self, spec) -> Result<InstanceHandle>` and
`async fn destroy(&self, handle)`. Later runtimes (containerd, Firecracker,
Kubernetes) are new files here.

### core-actor

```
src/
  lib.rs
  guest/             Linux only
    mod.rs           init_main, proxy_main
    proxy.rs
    init/
      mod.rs         the boot sequence, in order
      mounts.rs      nix::mount, umount
      network.rs     loopback, gateway forwarder
      identity.rs    hostname, fake sensors, time namespace
      sshfs.rs       workspace mounts over exec streams
      files.rs       copy-in, mirror-out, ownership
      stubs.rs       route stubs
      driver.rs      the driving program and its environment
  host/
    detect.rs        HostInfo from this machine
    vm.rs            Firecracker config, spawn, handle (from daemon)
    gateway/
      mod.rs         Gateway, the net.* hook points
      http.rs        absolute-form requests, upstreams, header injection
      connect.rs     CONNECT tunnels
```

`guest` runs inside the actor; `host` runs next to it.

### api-client

```
src/
  lib.rs             Client, connect (unix, vsock, http)
  ready.rs           wait until healthy, with backoff
  error.rs           Error, including Status mapped to a kind
  exec.rs            ExecStream: start, stdin, resize, signal, events, run to completion
  files.rs           tar over exec: push a directory, pull a directory
```

`files.rs` replaces the three copies in `control.rs`, `cli::fs_cli` and their
callers. The token comes from the caller, not from `NEEDY_TOKEN` inside the
library.

### api-server

```
src/
  lib.rs
  control.rs         domain traits: Sessions, Processes, Guest; Caller; ControlError
  server.rs          Server::bind (Unix, TCP, vsock UDS), serve, shutdown
  termination.rs     SIGINT/SIGTERM/SIGHUP, Ctrl-C on Windows
  middleware/
    authentication.rs   bearer tokens (today's AuthLayer)
  protocol/
    domain.rs        proto <-> core type conversions
    status.rs        ControlError -> tonic::Status, one table
    grpc/v1/
      health.rs  session.rs  process.rs  guest.rs
```

The actor's per-session listener is the same server bound with
`Caller::Actor { session }`; adapters stop checking `bound` by hand.

### daemon

```
src/
  lib.rs             Config, Daemon::start / shutdown
  session/
    mod.rs           Sessions: create, get, list, stop; rollback
    state.rs         transitions of core::SessionState
  pool.rs            instance keys, refcounts, slots (max_procs, on_full)
  routing.rs         the exec.decide chain, path maps
  relay/
    mod.rs           exec relay between a caller and an instance
    filter.rs        stream filters installed per session
  files.rs           copy-in and mirror-out (session side)
  actor.rs           wires a session to core-actor (VM, guest API, gateway)
  guest.rs           GuestService domain: config, events, exit, file streams
  store/             state.db: sessions, instances, processes (slice 9)
```

The daemon implements `api-server::control`. It keeps its own state in
`state.db`, separate from `trace.db`.

### common

```
src/
  archive.rs  filter.rs  exit.rs  seed.rs  time.rs
```

`util.rs` goes away: the glob re-export is dropped (callers use `glob-match`
directly), duration parsing moves to `config::value`, `expand_home` moves to
`config::profile` (it encodes the actor's `/home/<user>` convention), `mix`
becomes `seed.rs`, and `now_ms` moves to `time.rs` and replaces the copy in
`prochost`.

### xtask

```
xtask/
  src/
    main.rs          cargo xtask <task>
    bench/
      mod.rs         run: build the probes, start a session, collect results
      probe.rs       spawn, pipe and fs probes; one JSON line each
```

`.cargo/config.toml` defines `cargo xtask`. The probes must run inside an
actor, whose image only carries the needy binary, so `bench run` builds
`xtask` for `x86_64-unknown-linux-musl` and ships it into the actor with the
profile's copy-in. With `trace.timing` on, `bench run` also reads per-hook
latencies from the session's trace. The product binary loses its hidden
`bench` command.

## Cross-cutting

| Concern | Decision |
| --- | --- |
| Paths | `etcetera` with the XDG strategy on Linux and macOS and known folders on Windows. Config `$XDG_CONFIG_HOME/needy`; `state.db`, `trace.db` and sessions `$XDG_STATE_HOME/needy`; images `$XDG_DATA_HOME/needy`; build cache `$XDG_CACHE_HOME/needy`; the socket in `$XDG_RUNTIME_DIR/needy`, or the state directory where there is none (macOS). `NEEDY_*` variables override each. `~/.needy` is not migrated; there are no users yet. |
| Logging | `tracing` in every crate. `cli::logging` installs `tracing-subscriber` (stderr, off by default, `-v` levels); the daemon also writes a daily file with `tracing-appender` in the state directory. The guest logs to the console. |
| Errors | `thiserror` in `common`, `config`, `core`, `trace`, `core-*`, `api-client`; `anyhow` in `cli`, `daemon` and `xtask`. One status mapping in `api-server::protocol::status`. `let _ =` only with a comment. |
| Cancellation | `tokio-util` `CancellationToken` and `TaskTracker` per daemon, session and actor. |
| Wire types | `ExecStart.origin` and session state become proto enums. `Decide` carries the process ancestry. |

## Libraries

| Need | Library | Replaces |
| --- | --- | --- |
| Tables | `comfy-table` | hand-padded `println!` in `ps`, `session ls`, `profile check` |
| Paths | `etcetera` | `$HOME/.needy` |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` | `eprintln!` |
| Docker | `bollard` | `docker run`, `rm`, `build` shell-outs |
| Terminal | `crossterm` | libc `termios`, `isatty`, `TIOCGWINSZ` |
| Syscalls in the guest | `nix` (`mount`, `sched`, `reboot`, `unistd`, `net::if_`) | raw `libc` calls |
| Retry | `backon` | three hand-written retry loops |
| Shell words | `shell-words` | `split_whitespace` on "shell-quoted" input |
| Cancellation | `tokio-util` | untracked `tokio::spawn` |
| Errors | `thiserror` | stringly errors in libraries |

## Slices

Each slice is one commit, keeps the closed-loop behavior, and passes
`make check` and `make test`. Behavior fixes are listed where a slice makes
them.

| # | Slice | Needs |
| --- | --- | --- |
| 1 | **cli**: `commands/`, `output/` with comfy-table, `paths` with XDG, `logging` with tracing; `bench` moves to `xtask` | — |
| 2 | **config**: schemas, `source`, one `merge`, `validate`, built-ins move out of `core` | — |
| 3 | **core**: `rules`, `recipe`, `chaos` split; shared `pattern`; domain types; `intercept` traits | 2 |
| 4 | **trace**: events, dispatch, feature-gated sinks, report; `trace:` in profiles | 2 |
| 5 | **core-executor**: `process/`, `relay/` with crossterm, `provider/` with bollard | — |
| 6 | **core-actor**: `guest/init` with nix, `host/vm` from the daemon, gateway on hooks | 3, 4 |
| 7 | **api-client**, **api-server**: `exec`, `files`, `ready`, errors; adapters, `control`, `Server`, `termination` | 5 |
| 8 | **daemon**: rebuilt on the above. Fixes: the VM stops with its session, listeners and sockets are cleaned up, failed creates release instances, pool locking, `ancestors` reach the rules | 1–7 |
| 9 | **persistence**: `state.db` for sessions, instances, processes; reconcile on start | 8 |

## Decisions

- **Crates**: `config`, `trace` and `xtask` are added (owner, 2026-11-09).
- **bench** becomes `cargo xtask bench` (owner, 2026-11-09).
- **Docker** goes through `bollard` (owner, 2026-11-09).
- **Observation** is hook-based, configured per profile and feature-gated
  (owner, 2026-11-09).

## Open questions

- **Shell parsing in rules**: keep the small lexer in `rules/shell.rs` or adopt
  `brush-parser` as the profile note proposes. Until decided, the lexer stays
  with a `TODO:` at its definition.
- **bollard and Podman**: bollard speaks the Docker API, which Podman also
  serves on its socket; the provider would take a socket path.
- **Ancestry in `Decide`**: the proxy can read its parent chain from `/proc`;
  whether the kernel module should pass it instead is open.
