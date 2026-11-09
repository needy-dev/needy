# gRPC API crates

**Status**: current, 2026-11-09. Describes the schema and generation as they
are after slice 7a of the
[module layout redesign](../architecture/2026-11-09-module-layout-redesign.md).
Slice 7 restructures `needy-api-client` and `needy-api-server`; the crate
layout below changes with it.

The daemon speaks gRPC only. The previous HTTP API was removed on 2026-11-09.
The layout follows AUV: one canonical `proto/` tree, a crate of generated
types, a client crate that other frontends reuse instead of wrapping the CLI,
and a server crate with what every listener needs.

## Layout

```text
proto/
├── buf.yaml          module, STANDARD lint, FILE breaking rules
├── buf.gen.yaml      neoeinstein-prost and neoeinstein-tonic remote plugins
└── needy/v1/
    ├── health.proto   HealthService
    ├── session.proto  SessionService
    ├── process.proto  ProcessService
    └── guest.proto    GuestService
crates/
├── api-proto/        needy-api-proto: generated prost and tonic code (src/gen, committed)
├── api-client/       needy-api-client: Client with one channel and bearer auth
└── api-server/       needy-api-server: AuthLayer and Authenticator; protocol::domain
```

Every schema file is marked `EXPERIMENTAL / UNSTABLE`. There are no users yet,
so a schema change removes old fields instead of keeping them beside the new
ones; removed field numbers are `reserved`.

## Services

| Service | RPC | Shape | What it does |
| --- | --- | --- | --- |
| `HealthService` | `Check` | unary | Instance ID, version, role (`control` or `executor`) and platform. The only RPC that needs no token |
| `SessionService` | `CreateSession` | unary | Creates a session from a profile's YAML, its directory and its name; optionally starts the actor |
| | `GetSession`, `ListSessions` | unary | Running, stopped, finished and failed sessions |
| | `StopSession` | unary | Stops a session and releases its executors |
| | `GetSessionLogs` | server stream | The actor's console output so far; `NOT_FOUND` when the session has no actor |
| | `Decide` | unary | Where a command runs and how it is rewritten. The actor's proxy calls it with the command's ancestry |
| `ProcessService` | `Exec` | bidirectional stream | Start (`ExecStart`) or attach (`ExecAttach`), then stdin, signals and resizes; responses carry `event`, `started`, output, `exited` or `error` |
| | `ListProcesses`, `KillProcess` | unary | The process table; a signal by name |
| `GuestService` | `GetConfig` | unary | The actor's configuration: user, routed names, identity, mounts, driving program, files, hwmon faults |
| | `PushEvents` | client stream | Events observed inside the actor, such as an injected hwmon fault |
| | `ReportExit` | unary | The driving program's exit |
| | `FetchFiles`, `PushFiles` | server / client stream | Tar archives for copy-in and mirror-out |

Who serves what:

- The control daemon's socket serves `HealthService`, `SessionService` and
  `ProcessService`.
- An executor daemon, and the control daemon's in-process local executor,
  serve `HealthService` and `ProcessService`.
- Each actor gets a listener bound to its session (a vsock Unix socket). It
  serves `GuestService`, `SessionService` and `ProcessService`; `Decide`,
  `Exec` and the guest calls act on that session, and `CreateSession`,
  `StopSession` and `GetSessionLogs` are refused.

### Exec

`ExecStart.origin` says who starts the process and is required. It decides
whether the daemon runs the session's rules: `ORIGIN_ACTOR_PROXY` was placed by
`Decide` already, and `ORIGIN_ACTOR_MOUNT` is a mount's SFTP server, not a
command. The trace and the report use it to count commands.

For an exec within a session, the daemon sends `ExecResponse.event` messages
before `started`: the faults chaos injected and the placement decision, as
`ExecEvent { kind, payload }` where `payload` is the JSON of a
`needy_core::event` kind. A refused start fails with `PERMISSION_DENIED`, and
its status details are an `ExecRefusal` holding the same events. A command that
cannot start fails with `FAILED_PRECONDITION`.

### Decide

`DecideRequest.ancestors` lists the processes the command runs under, nearest
first, as argument vectors, up to but not including the actor's init. The
proxy reads them from `/proc`, since the kernel hands it the routed process
itself. The daemon puts them in the rules' context for `when.under`.

`DecideResponse.target` is `actor`, an executor name, or empty with `deny` set
to the message the proxy prints.

### Guest events

`TraceEvent { time_ms, kind, payload, version }` carries one
`needy_core::event` payload as JSON with the guest build's schema version. The
daemon decodes it and refuses a version it does not read, so an actor image
built from other sources fails loudly instead of recording a wrong event. The
listener is bound to a session, so the event names none.

## Enums

| Enum | Values | Domain type |
| --- | --- | --- |
| `Origin` (`process.proto`) | `CLI`, `FS`, `FILES`, `ACTOR_PROXY`, `ACTOR_MOUNT` | `needy_core::types::Origin` |
| `SessionState` (`session.proto`) | `CREATING`, `RUNNING`, `STOPPING`, `STOPPED`, `FINISHED`, `FAILED` | `needy_core::types::SessionState` |

Each enum has an `_UNSPECIFIED` zero value, as Buf's STANDARD lint requires.
`needy_api_server::protocol::domain` maps each enum to its domain type and
back, one function per direction. A zero value or a value this build does not
know is refused with an error that names the field, never read as a default.
Producers on the client side (the CLI, the proxy, the mount) set the proto
enum directly.

Other values are still strings: `CheckResponse.role`, `ExecutorRef.runtime`,
`DecideResponse.target`, event kinds and signal names.

## Profile names

`CreateSessionRequest.profile_name` is required. The client derives it with
`needy_config::profile::name`: the file name without `.yaml` or `.yml` and
then without `.profile`, or the directory's name for a file named only
`profile.yaml`. The daemon uses it in executor keys (`{profile}`), the
`session.create` event, listings and reports.

## Generation

Buf generates the Rust code with the `neoeinstein-prost` and
`neoeinstein-tonic` remote plugins. The output in `crates/api-proto/src/gen` is
committed, so Cargo builds need neither `protoc` nor a build script.

This differs from AUV, which compiles schemas in `build.rs` with a vendored
`protoc` and uses Buf only for TypeScript and OpenAPI. Committed output keeps
the build simple and makes schema changes visible in review. The cost is that
a contributor must run `make proto` after editing a schema.

`make proto` runs `buf lint` (STANDARD), `buf format -w` and `buf generate`.
`buf breaking --against '.git#branch=main,subdir=proto'` (FILE rules) reports
every removed or retyped field; while the schemas are unstable a break is
expected and is stated in the change, not avoided.

## Open questions

- **REST/JSON gateway.** AUV keeps a REST surface through `google.api.http`
  annotations and OpenAPI. Needy has no such consumer yet, so the schemas do
  not import googleapis.
- **Plugin versions.** `buf.gen.yaml` does not pin remote plugin versions yet.
- **Remaining strings.** Whether `role`, `runtime`, `target` and event kinds
  become enums too. Event kinds are many and owned by `needy_core::event`, so
  they stay strings until a consumer needs more than their names.
