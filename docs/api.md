# HTTP API

The Rust daemon serves versioned HTTP endpoints. It listens on `127.0.0.1:7331` by default. Use a private tunnel for remote loopback access, or configure HTTPS with `--tls-cert` and `--tls-key`. Request and response bodies are JSON except workspace tar transfers, the NDJSON event stream and the upgraded native process tunnel. Rust state is a separate schema; use a new state directory when replacing the Go MVP.

## Authorization

`POST /v1/pair` is the only unauthenticated endpoint. All other endpoints require:

```http
Authorization: Bearer CLIENT_TOKEN
```

Paired clients share access to the daemon's sessions. This is a trusted-operator API, not a multi-tenant authorization system. Tokens must be kept private. Revoking a client prevents subsequent authenticated requests using its token and closes followed event streams when their authorization is rechecked. Existing batch workloads continue until stopped separately.

### Pair

```http
POST /v1/pair
Content-Type: application/json

{"code":"ONE_TIME_CODE","name":"laptop"}
```

The host operator generates the expiring code with `needy pairing-code --data-dir DIR --out FILE`. A successful exchange consumes the code and returns `{"token":"...","client_id":"..."}`. Codes and tokens must not be placed in URLs.

## Host and clients

| Method | Path | Result |
| --- | --- | --- |
| GET | `/v1/info` | Instance ID, image, concurrency limit, active-run count and whether native host execution is enabled |
| GET | `/v1/clients` | Paired client metadata; no bearer tokens |
| DELETE | `/v1/clients/{id}` | Revoke a paired client |

Host image, container limits and allowed host commands are set by the operator. Docker endpoints do not accept image overrides, arbitrary mounts, Docker flags or host paths. The separately enabled native process endpoint accepts a host cwd constrained by the operator's configured root.

## Sessions

| Method | Path | Request or result |
| --- | --- | --- |
| POST | `/v1/sessions` | `{"name":"demo"}`; returns a session |
| GET | `/v1/sessions` | List sessions |
| GET | `/v1/sessions/{id}` | Session metadata |
| DELETE | `/v1/sessions/{id}` | Delete an inactive session and its workspace/home volumes |
| PUT | `/v1/sessions/{id}/workspace` | Upload a tar stream |
| GET | `/v1/sessions/{id}/workspace` | Download a tar stream |

Session metadata includes `id`, `name`, `created_at` and optional `active_run_id`. One session permits one active run. Deleting an active session is rejected. Upload is allowed only before the session's first run; after any run has been created, the host returns HTTP 409 for further uploads. Create a new session to upload a new source tree. Workspaces and home directories persist between runs.

Workspace archives support regular files and directories only, limited to 10,000 entries and a 32 MiB tar archive including headers. Absolute paths, path traversal, symlinks, hardlinks and special files are rejected. Upload only the files intended for the remote workload. The CLI skips source symlinks and paths named `.git`, `.needy`, `.env`, `.env.local` or `target` when packing an upload; it does not apply `.gitignore`. The client validates downloaded paths before extracting them into a new or empty destination directory.

## Batch runs

Create a run for any command:

```http
POST /v1/sessions/SESSION_ID/runs
Content-Type: application/json
Idempotency-Key: UNIQUE_REQUEST_ID

{"command":["sh","-c","printf hello > result.txt"],"stdin":""}
```

The command is an argument array; the runtime does not add a shell. Shell syntax works only when the request explicitly invokes a shell, as above. `stdin` is optional and is written to the command's standard input. There are no agent, prompt or model fields; put an agent in the image or upload it with the workspace and invoke it like any other command.

The CLI selects this request with `needy run SESSION_ID -- COMMAND [ARGS...]`. Use `needy run SESSION_ID --stdin FILE -- COMMAND [ARGS...]` to provide standard input, or `--stdin -` to read the client's own stdin.

Use a fresh `Idempotency-Key` for each intended run. Retry a lost creation response with the same key and body so it does not duplicate work. A busy session or full daemon returns HTTP 409 instead of queueing work.

| Method | Path | Result |
| --- | --- | --- |
| GET | `/v1/runs/{id}` | Current run state and exit details |
| POST | `/v1/runs/{id}/stop` | Request cancellation |
| GET | `/v1/runs/{id}/events?after=N&follow=true` | Replay and follow NDJSON events |

Run metadata includes `id`, `session_id`, `state`, `created_at`, optional `exit_code` and optional `error`.

States are `creating`, `starting`, `running`, `cancelling`, `succeeded`, `failed`, `cancelled` and `lost`. `lost` means execution could not be preserved, for example a missing container. A temporary Docker observation failure keeps a batch run nonterminal, records a visible error and retains its reservation while retrying. It never restarts the accepted workload.

The image runner limits each output line to 8 MiB and total JSON-enveloped output to 16 MiB, including while the daemon is offline. A violation terminates the child process group, reports the limit on stderr and exits with code 125. The daemon independently enforces a 16 MiB observed-output limit and fails the run after confirming container termination. Docker's per-container log limit is larger than the runner's output budget, so normal runner output remains available for replay. These are limits for trusted workloads; no workspace disk quota is enforced.

## Events and reconnection

Every event has a monotonically increasing numeric ID within its run:

```json
{"id":1,"run_id":"RUN_ID","type":"output","data":{"stream":"stdout","line":"hello"}}
```

`state` events contain run metadata. `output` events contain `stream` (`stdout` or `stderr`) and `line`. Clients should retain the last event ID, reconnect with `after=ID` and ignore any already-consumed IDs. `after` is exclusive. Omit `follow=true` to read the currently available events and close the response.

Disconnecting a stream does not cancel the run. The daemon persists metadata and events, and reconciles Docker containers on restart. It observes an existing container rather than rerunning an accepted command. Use the stop endpoint for cancellation; a cancellation request first marks the run as cancelling, and terminal status follows the observed container exit. Check run status to observe the result.

## Native host processes

`POST /v1/exec` uses the same authenticated HTTP/1.1 upgrade with protocol
`needy-exec-v1`. It is available only when the host operator has configured
`--host-root` and `--allow-host-exec`. `--no-docker` can run this endpoint without
a Docker backend. Example request body:

```json
{"command":["uname","-s"],"cwd":"/Users/you/project","environment":{},"pty":null}
```

The first command element is a configured logical executable name, resolved to
an absolute host path at daemon startup. Optional `argv0` preserves the original
intercepted argument zero. Cwd is canonicalized and must be inside the configured
root. Request environment keys must appear in `--host-env`. Host commands also
inherit the daemon's environment. Client-selected executable paths are rejected.
This is host access under the daemon's account, not a filesystem sandbox.

`pty` may be `{"rows":24,"cols":80}`. The stream carries frames with a one-byte
kind, a four-byte unsigned big-endian payload length and then that many payload
bytes. The maximum payload per frame is 8 MiB.

| Kind | Value | Direction and payload |
| --- | --- | --- |
| Stdin | 1 | Client to daemon, process input bytes |
| StdinEOF | 2 | Client to daemon, empty payload, half-close stdin |
| Stdout | 3 | Daemon to client, output bytes |
| Stderr | 4 | Daemon to client, separate diagnostic bytes |
| Exit | 5 | Daemon to client, `{"code":0}` with process exit code 0–255 |
| Failure | 6 | Daemon to client, `{"error":"description"}` for transport, process or cleanup failures |
| Signal | 7 | Client to host, JSON signal number (`SIGINT`, `SIGTERM`, `SIGHUP` or `SIGQUIT`) |
| Resize | 8 | Client to host, JSON window size object |

PTY mode combines stdout/stderr as a terminal normally does. In pipe mode they remain separate. Exit can include a POSIX signal name, for example `{"code":143,"signal":"SIGTERM"}`. The Linux bridge raises the corresponding local signal so its parent observes signal death through wait.

A native process uses one host concurrency slot. Disconnect, revocation or daemon
death closes its worker control pipe and kills its process group. Native commands
have no durable output journal and are not restarted.

## Trust and limits

A paired client is a trusted operator. It can submit arbitrary commands and manage
this daemon's sessions. Sessions isolate files and processes through Docker, but
paired clients are not separate tenants. Workloads can access the network and any
credentials uploaded with them, and can return those secrets as output. Use a
dedicated host or VM when the workload's trust level requires it.
