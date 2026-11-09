# Module layout redesign

**Status**: accepted, 2026-11-09. Slices are implemented in order; see
[Slices](#slices).

The crate list stays as it is, plus an `xtask` crate for developer tasks. This note redesigns what lives inside each
crate: the flows, the module tree, which code moves where, and which libraries
replace hand-written code. It follows the conventions of
[auv](https://github.com/moeru-ai/auv) where they fit.

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
- `core::rules` and `core::recipe` are single 1000-line files. Command
  patterns are matched twice (`rules`, `chaos`), basenames are computed four
  times, and `extends` has two merge models.
- Event kinds, attribute names, session states, fault kinds and exec origins
  are strings shared by convention between the daemon, the gateway and the
  report.
- There is no logging (`eprintln!` in 24 places), errors are dropped with
  `let _ =` in 45 places, gRPC statuses are flattened to strings in 12, and
  paths are `$HOME/.needy`.
- Known defects that follow from the shape: `Session.actor` is never set, so
  stopping a session does not stop its VM; listener tasks and sockets leak;
  a failed session create leaks the instances it already acquired; the pool
  lock is held across `docker run`.

## Principles

1. **Mechanism below, policy above.** `core-actor` knows how to boot a VM and
   run the guest; `core-executor` knows how to run a process and how to create
   an executor instance. `daemon` decides when: sessions, the pool, routing
   and relaying. `core` decides what: profiles, rules, recipes, chaos, the
   trace model. No crate shells out to a tool another crate wraps.
2. **Transport is a layer.** gRPC types stop at `api-server` (inbound) and
   `api-client` (outbound). The daemon implements domain traits; adapters map
   proto to domain and errors to statuses in one place.
3. **Every shared string becomes a type.** Event kinds, attributes, session
   states, fault kinds, placements and exec origins are enums or structs in
   `core` (and in `proto` where they cross the wire).
4. **Every task has an owner.** Long-lived tasks are spawned into a
   `TaskTracker` under a `CancellationToken` owned by the thing they serve
   (daemon, session, actor). Stopping the owner stops and awaits them.
5. **A library before hand-written code.** See [Libraries](#libraries).

## Dependency direction

```
cli ──────────────┐
daemon ───────────┼──► api-server ──► api-proto
                  │    api-client ──► api-proto
                  ├──► core-actor ───┐
                  ├──► core-executor ┼──► core ──► common
                  └──────────────────┘
```

`core-actor` and `core-executor` may use `api-client` (the guest and the relay
speak gRPC). `core` and `common` never see gRPC, tokio processes, sockets or
the filesystem beyond reading a profile or recipe file.

## Flows

Each step names the module that owns it after the redesign.

### Daemon start

1. `cli::commands::daemon` resolves `cli::paths` (XDG) and builds
   `daemon::Config` (state, data, runtime directories; images; token).
2. `daemon::Daemon::start` opens `core::store`, reconciles persisted sessions
   and instances (a later slice), and starts the in-process local executor
   from `core-executor::process::Host`.
3. `api-server::server::Server::bind` binds every listener (Unix, TCP), then
   serves the adapters in `api-server::protocol::grpc` against the daemon's
   domain traits. `api-server::termination` turns SIGINT, SIGTERM and SIGHUP
   into cancellation; `Daemon::shutdown` stops sessions in order.

### Create a session

1. `api-server` adapter → `daemon::session::Sessions::create`.
2. `core::profile::Profile::load`; optional `core::recipe::Recipe::compile`
   with `core-actor::host::detect` for the host CPU.
3. For each executor entry: `daemon::pool::Pool::acquire(key)`, which calls a
   `core-executor::provider::Provider` (`native`, `remote`, `docker`). The
   pool lock covers only the map; provisioning runs outside it behind a
   per-key `OnceCell`.
4. `daemon::files::copy_in` through `api-client::files` (tar over exec).
5. `core-actor::host::vm::Vm::start` boots Firecracker and returns a handle
   that owns the child. `daemon::actor` wires the guest listener and the
   gateway to the session's cancellation token.
6. Any failure unwinds the steps already done (release instances, stop the
   VM, remove the directory) and records `session.fail`.

Session state is an enum (`Creating`, `Running`, `Stopping`, `Stopped`,
`Finished`, `Failed`) with transitions in one place; the first terminal state
wins.

### Routed exec from the actor

1. `core-actor::guest::proxy` asks `Decide` over vsock. The guest listener
   authenticates the caller as `Caller::Actor { session }`; the adapter calls
   `daemon::routing::decide`, which runs `core::rules::decide`, applies the
   executor's path map and asks `core::chaos::Injector` for an `exec` fault.
2. Local placement: the proxy `execve`s the real program. Otherwise it calls
   `ProcessService.Exec` with `Origin::ActorProxy`.
3. `daemon::relay::exec` takes the instance's slot (`daemon::pool`), opens the
   upstream stream with `api-client`, and relays both ways. Recording
   (`core::trace::cast`) and the SFTP fault tap (`core::chaos::sftp`) are
   stream filters plugged into the relay, not branches inside it.
4. `core-executor::relay` on the proxy side owns the terminal (raw mode,
   resize, signal forwarding) and returns the exit status.

### Workspace mount

`core-actor::guest::init::sshfs` starts `sshfs -o passive` and bridges it to
an `Exec` stream with `Origin::ActorMount`. The relay attaches the SFTP tap
when the session has `file.open` faults.

### Stop a session

`daemon::session::Sessions::stop` moves the state to `Stopping`, cancels the
session token (the guest listener, the gateway and the relays stop), stops
the VM through its handle, runs `daemon::files::mirror_out`, releases
instances (`Provider::destroy` at refcount zero), records `session.stop`, and
removes the socket files.

### Sessionless `needy exec`

`daemon::relay::exec` with no session uses the local executor or a named
instance; an ambiguous name is an error instead of the first match.

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
profile's copy-in. The product binary loses its hidden `bench` command.

### daemon

```
src/
  lib.rs             Config, Daemon::start / shutdown
  session/
    mod.rs           Sessions: create, get, list, stop; rollback
    state.rs         SessionState and transitions
  pool.rs            instance keys, refcounts, slots (max_procs, on_full)
  routing.rs         Decide: rules + path map + exec faults
  relay/
    mod.rs           exec relay between a caller and an instance
    filter.rs        stream filters: recording, SFTP tap
  files.rs           copy-in and mirror-out (session side)
  actor.rs           wires a session to core-actor (VM, guest API, gateway)
  guest.rs           GuestService domain: config, events, exit, file streams
  store.rs           persistence of sessions, instances, processes (later slice)
```

`daemon` implements the domain traits that `api-server::control` defines. It
no longer has a `server.rs`; listeners belong to `api-server`.

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
`Caller::Actor { session }`; the adapters stop checking `bound` by hand.

### api-client

```
src/
  lib.rs             Client, connect (unix, vsock, http)
  ready.rs           wait until healthy, with backoff
  error.rs           Error, including Status mapped to a kind
  exec.rs            ExecStream: start, stdin, resize, signal, events, run to completion
  files.rs           tar over exec: push a directory, pull a directory
```

`files.rs` replaces three copies: `control.rs` copy-in and mirror-out,
`cli::fs_cli`, and the guest file streams' callers. The token comes from the
caller, not from `NEEDY_TOKEN` inside the library.

### core

```
src/
  lib.rs
  pattern.rs         command patterns: compile once, match, captures, basename
  profile/
    mod.rs           Profile, load
    schema.rs        top-level and actor types
    executor.rs      ExecutorConfig, Runtime, OnFull, limits
    rules.rs         Rule, Actions, When, presets, resolvers (types only)
    files.rs         FileMapping, Side, filters
    network.rs       NetworkPolicy and its host matcher
    extends.rs       extends and merge, shared with recipes
  rules/
    mod.rs           decide, route_names, Decision, Placement
    when.rs
    actions.rs       one merge of preset and rule actions
    launcher.rs      built-in and user launcher resolvers
    shell.rs         sh -c scripts
  recipe/
    mod.rs           Recipe, load
    spec.rs          CPU, kernel, OS, firmware, devices, network types
    compile.rs       CompiledRecipe
    identity.rs      placeholders, SMBIOS, PCI IDs, chassis numbers
    cpu.rs           CPUID encoding, feature table, model checks
    host.rs          HostInfo and parsing /proc/cpuinfo text (no reading)
  chaos/
    mod.rs           Injector
    fault.rs         FaultKind, Errno, typed fault settings
    sftp.rs          SFTP packet framing for the file.open tap (pure bytes)
  trace/
    mod.rs           Trace handle
    event.rs         EventKind and typed attributes
    writer.rs        store and JSONL writer task
    cast.rs          asciicast v2 encoding
  store/             (exists) entities and migrations
  report/
    mod.rs           render from typed rows
    summary.rs
```

Moves out of `core`: Docker flag policy (`profile::check_docker_args`) to
`core-executor::provider::docker`; `/proc/cpuinfo` reading to
`core-actor::host`; the Firecracker environment names produced by
`recipe::compile` become a typed `VmIdentity` that `core-actor::host::vm`
turns into environment variables.

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
      mod.rs         Gateway, Policy
      http.rs        absolute-form requests, upstreams, header injection
      connect.rs     CONNECT tunnels
```

`guest` runs inside the actor; `host` runs next to it. The split keeps the
Linux-only guest code apart from code that runs on any daemon host.

### common

```
src/
  archive.rs  filter.rs  duration.rs  exit.rs  seed.rs  time.rs
```

`util.rs` goes away: the glob re-export is dropped (callers use `glob-match`
directly), `expand_home` moves to `core::profile` (it encodes the actor's
`/home/<user>` convention), `mix` becomes `seed.rs`, `now_ms` moves to
`time.rs` and replaces the copy in `prochost`.

## Cross-cutting

| Concern | Decision |
| --- | --- |
| Paths | `etcetera` with the XDG strategy on Linux and macOS and known folders on Windows. Config `$XDG_CONFIG_HOME/needy`; traces and sessions `$XDG_STATE_HOME/needy`; images `$XDG_DATA_HOME/needy`; build cache `$XDG_CACHE_HOME/needy`; the socket in `$XDG_RUNTIME_DIR/needy`, or the state directory where there is none (macOS). `NEEDY_*` variables override each. `~/.needy` is not migrated; there are no users yet. |
| Logging | `tracing` in every crate. `cli::logging` installs `tracing-subscriber` (stderr, off by default, `-v` levels); the daemon also writes a daily file with `tracing-appender` in the state directory. The guest logs to the console. Product events stay in `core::trace`. |
| Errors | `thiserror` in `common`, `core`, `core-*`, `api-client`; `anyhow` in `cli` and `daemon`. One status mapping in `api-server::protocol::status`. `let _ =` only with a comment. |
| Cancellation | `tokio-util` `CancellationToken` and `TaskTracker` per daemon, session and actor. |
| Wire types | `ExecStart.origin` and session state become proto enums. |
| Config parsing | Unknown keys in profiles are errors (`deny_unknown_fields`), except where a map is intended. |

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

1. **cli**: `commands/`, `output/` with comfy-table, `paths` with XDG,
   `logging` with tracing; `bench` moves to `xtask`.
2. **core**: module split, shared `pattern`, one `extends`, typed events,
   faults, placements and states. Report reads typed rows.
3. **core-executor**: `process/` split, `relay/` with crossterm, `provider/`
   with native, remote and Docker over bollard.
4. **core-actor**: `guest/init` split with nix, `host/vm` moved from the
   daemon, `host/gateway` split.
5. **api-client** and **api-server**: `exec`, `files`, `ready`, error
   mapping; adapters, `control` traits, `Server`, `termination`.
6. **daemon**: rebuilt on the above. Session states, rollback, cancellation,
   pool locking. Fixes: the VM stops with its session, listeners and sockets
   are cleaned up, failed creates release instances.
7. **persistence**: instances and processes in `core::store`, reconcile on
   start (the slice agreed earlier).

Slices 1 to 4 are independent of each other; 5 needs 3; 6 needs all of them.

## Decisions

- **bench** becomes `cargo xtask bench` (owner, 2026-11-09).
- **Docker** goes through `bollard` (owner, 2026-11-09).

## Open questions

- **Shell parsing in rules**: keep the small lexer in `rules/shell.rs` or adopt
  `brush-parser` as the profile note proposes. Until decided, the lexer stays
  with a `TODO:` at its definition.
- **bollard and Podman**: bollard speaks the Docker API, which Podman also
  serves on its socket; the provider would take a socket path.
- **auv uses `Result<_, String>` at the CLI edge**; this note keeps `anyhow`
  there because `AGENTS.md` already says so.
