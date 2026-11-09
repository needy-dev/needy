# gRPC API crates

**Status**: current, 2026-11-10. Describes the schema, the generation and the
client and server crates as they are after slice 7 of the
[module layout redesign](../architecture/2026-11-09-module-layout-redesign.md).

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
├── api-client/       needy-api-client: connect, readiness, exec streams, files over exec
└── api-server/       needy-api-server: domain traits, Server, adapters, status table
```

Generated types stop at the two API crates. The daemon implements the domain
traits of `needy_api_server::control` and never builds a `tonic::Status`;
callers of `needy_api_client` build requests from the generated types it
re-exports as `needy_api_client::proto`, and read errors as an `Error` with a
`Kind`.

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
- Each actor gets a listener bound to its session (a vsock Unix socket), with
  the caller `Caller::Actor { session }`. It serves `HealthService`,
  `GuestService`, `SessionService` and `ProcessService`; `Decide`, `Exec`,
  `ListProcesses` and the guest calls act on that session whatever the request
  names, and `CreateSession`, `StopSession` and `GetSessionLogs` are refused.
  The control socket's caller is `Caller::Cli`, which `GuestService` refuses.

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
Each direction has one function. `needy_api_server::protocol::domain` reads
`Origin` and writes `SessionState`; the CLI's `session` command reads
`SessionState`, since it is the only reader of session states on the wire. A
zero value or a value this build does not know is refused with an error that
names the field, never read as a default. Producers on the client side (the
CLI, the proxy, the mount) set the proto enum directly.

Other values are still strings: `CheckResponse.role`, `ExecutorRef.runtime`,
`DecideResponse.target`, event kinds and signal names.

## Profile names

`CreateSessionRequest.profile_name` is required. The client derives it with
`needy_config::profile::name`: the file name without `.yaml` or `.yml` and
then without `.profile`, or the directory's name for a file named only
`profile.yaml`. The daemon uses it in executor keys (`{profile}`), the
`session.create` event, listings and reports.

## Crate layout

### `needy-api-client`

```text
src/
  lib.rs     Client: connect to unix:/path, http(s)://, vsock://CID:PORT (Linux); with_token
  ready.rs   wait(endpoint, within): connect and retry until the health check answers
  error.rs   Error and Kind
  exec.rs    ExecStream: start, attach, input (stdin, eof, signal, resize), next event, run to completion
  files.rs   push an archive into a directory, pull a path as an archive: tar over exec
tests/       exec and files against an in-process ExecutorService
```

- **Tokens** come from the caller (`Client::with_token`); the library reads
  no environment variable. The CLI reads `NEEDY_TOKEN`, a remote executor its
  `token_env`.
- **`ready::wait`** retries every 100 ms with `backon` while the error is
  transient (the server is not up: a transport error or `UNAVAILABLE`); a
  malformed endpoint, or vsock outside Linux, fails at once. Providers wait
  10 s for an executor, the guest 5 s for the daemon's vsock listener.
- **`Kind`** is the inverse of the server's status table: `Unauthenticated`,
  `Denied`, `NotFound`, `Invalid`, `Failed`, `Exhausted`, `Unavailable`,
  `Other`. `Error::refusal` decodes a refused exec's `ExecRefusal` events.
- **`ExecStream`** yields `Event::{Trace, Started, Stdout, Stderr, Exited}`;
  an executor's `error` payload is `Error::Remote`. `run` feeds stdin beside
  the reader and fails with `Error::Ended` when the stream ends before the
  exit. The terminal relay (`needy_core_executor::relay`), the CLI, the
  proxy and the workspace mount use it.
- **`files`** runs `tar` on the executor with `COPYFILE_DISABLE=1`; a non-zero
  exit is `Error::Failed` with tar's stderr. Packing and unpacking on this side
  stay with the caller (`needy_common::archive`, or the CLI's archive of one
  named file or directory), since copy-in filters and `fs cp` name entries
  differently. The daemon's copy-in and mirror-out and `needy fs cp` use it.

### `needy-api-server`

```text
src/
  control.rs          Sessions, Processes, Guest; Caller; ControlError; domain types
  server.rs           Server: services for one Caller; bind every listener, then serve
  termination.rs      SIGINT, SIGTERM, SIGHUP (Ctrl-C on Windows) as a CancellationToken
  middleware/
    authentication.rs AuthLayer, Authenticator, Token
  protocol/
    domain.rs         generated <-> domain conversions, one function per direction
    status.rs         ControlError -> tonic::Status, one table
    grpc/v1/          health, session, process, guest adapters
```

- **Domain traits.** `Sessions` (`create`, `get`, `list`, `stop`, `logs`,
  `decide`), `Processes` (`exec`, `list`, `kill`) and `Guest` (`config`,
  `event`, `exit`, `fetch_files`, `push_files`) take and return
  `needy_core` and `control` types. `Processes::exec` takes an `ExecOpen`
  (start or attach) and the rest of the caller's stream as `ExecInput`s, and
  returns a stream of `ExecOutput`s. The executor's `ExecutorService` still
  implements the generated `ProcessService`, which `Server::process_service`
  serves as is, since `core-executor` sits below `api-server`.
- **`Caller`** is fixed per server: `Cli` on the control socket and on an
  executor daemon, `Actor { session }` on an actor's listener. The adapters
  apply it: an actor's calls act on its own session, and the session
  operations an actor may not call are refused before the daemon sees them.
- **`Server::bind`** binds every listener before any serves, so a socket
  path over `sun_path` or a taken port fails start-up at once with the
  address; a Unix socket's directory is created and a stale socket replaced.
  `serve` stops accepting when its token is cancelled and gives open calls
  5 s, since an attached exec may never end.
- **Status table** (`protocol::status`):

  | `ControlError` | Status | Example |
  | --- | --- | --- |
  | `Denied { message, events }` | `PERMISSION_DENIED`, details `ExecRefusal` | a rule or fault refused an exec; an actor stopping a session |
  | `NotFound` | `NOT_FOUND` | no such session, process or mapping |
  | `Invalid` | `INVALID_ARGUMENT` | an unspecified origin; a guest event of another schema version |
  | `Failed` | `FAILED_PRECONDITION` | an invalid profile; a command that cannot start |
  | `Exhausted` | `RESOURCE_EXHAUSTED` | a full executor with `on_full: reject` |
  | `Unavailable` | `UNAVAILABLE` | a closing executor |
  | `Internal` | `INTERNAL` | a console that cannot be read |

  The daemon maps an executor's relayed error back to the same row by its
  `Kind`. Authentication failures are `UNAUTHENTICATED` from the middleware.
- **Tests** run the adapters through a real `Server` and `needy-api-client`
  against a fake implementation of the traits (`src/testing.rs`).

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
