# gRPC API crates

**Status**: pending. The crates and schemas are initialized; the daemon does not
serve them yet. The HTTP API in [`docs/api.md`](../../../api.md) is still the
implemented contract.

The layout follows AUV: one canonical `proto/` tree, a crate of generated
types, a client crate that other frontends reuse instead of wrapping the CLI,
and a server crate with the protocol adapters every listener needs.

## Layout

```text
proto/
├── buf.yaml / buf.gen.yaml
└── needy/api/
    ├── daemon/v1/    HealthService, HostService, PairingService
    └── runtime/v1/   SessionService, RunService, ProcessService
crates/
├── api-proto/        needy-api-proto: generated prost + tonic code
├── api-client/       needy-api-client: Client with one channel and bearer auth
└── api-server/       needy-api-server: AuthLayer and Authenticator for listeners
```

- `daemon/v1` describes the daemon itself: health, host capabilities and
  pairing. `runtime/v1` describes work: sessions, batch runs and native
  processes.
- Every schema starts as `EXPERIMENTAL / UNSTABLE`.
- `needy-api-server` does not implement services. The daemon implements the
  generated service traits; the crate adds bearer authentication in front of
  every method except `PairingService.PairClient`.

## Generation

Buf generates the Rust code with the `neoeinstein-prost` and
`neoeinstein-tonic` remote plugins. The output in `crates/api-proto/src/gen` is
committed, so Cargo builds need neither `protoc` nor a build script.

This differs from AUV, which compiles schemas in `build.rs` with a vendored
`protoc` and uses Buf only for TypeScript and OpenAPI. Committed output keeps
the build simple and makes schema changes visible in review. The cost is that a
contributor must run `make proto` after editing a schema.

`make proto` runs `buf lint` (STANDARD), `buf format -w` and `buf generate`.

## Mapping from the HTTP API

| HTTP | gRPC |
| --- | --- |
| `POST /v1/pair` | `PairingService.PairClient` (the only unauthenticated RPC) |
| `GET /v1/info` | `HostService.GetHost` |
| `/v1/clients` | `PairingService.ListClients`, `RevokeClient` |
| `/v1/sessions` | `SessionService` CRUD |
| `PUT`/`GET /v1/sessions/{id}/workspace` | `PushWorkspace` (client stream), `PullWorkspace` (server stream) |
| `/v1/runs/...` | `RunService`; NDJSON events become `WatchRunEvents` |
| Upgraded native process tunnel | `ProcessService.Exec` (bidirectional stream) |

## Open questions

- **Daemon crate.** Should the service implementations move into a
  `needy-daemon` crate, like `auv-daemon`, with `crates/needy` reduced to the
  CLI binary? This also
  touches the server/backend/CLI split (O3 in the
  [design decisions](../architecture/2026-11-06-needy-design-decisions-design.md)).
- **HTTP compatibility.** Serve gRPC beside the current HTTP API during
  migration, or replace it in one step?
- **REST/JSON gateway.** AUV keeps a REST surface through `google.api.http`
  annotations and OpenAPI. Needy has no such consumer yet, so the schemas do
  not import googleapis.
- **Plugin versions.** `buf.gen.yaml` does not pin remote plugin versions yet.
- **Run events.** `RunEvent` covers stdout, stderr and state changes. Check that
  it carries everything the current NDJSON events do before switching.
