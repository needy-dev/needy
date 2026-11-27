# Boxide v1 (spec)

Status: **proposed spec for owner review**, 2026-11-27. Nothing here is
implemented. The owner named the protocol **Boxide**, version **v1**, on
2026-11-27 (package `boxide.v1`).

The owner decided where this lives, on 2026-11-27: in this repository under
`proto/boxide/v1/`, as a package that names nothing of needy. Both sides are
kept in sync by hand. It moves to its own repository once it is stable.

Related:
[Dynamic executor binding and runner brokers](2026-11-27-dynamic-executor-binding-and-brokers-spec.md),
which carries this protocol through brokers.

## 1. What it is

A data-plane protocol between a **client** that wants commands run and files
served and a **server** that runs them on its machine and serves a directory
of it. One server is one sandbox: a VM, a container, a pod or a person's
machine.

The model is LSP and DAP: one specification, capability negotiation, several
transports, reference implementations and a conformance suite.

In scope:

- **Session:** handshake, version, capabilities, liveness.
- **Exec:** processes with a PTY or pipes, stdin, signals, window size, exit
  status. Optionally, processes held across a disconnect and replayed.
- **Files:** inode-based, POSIX-like operations, enough for a FUSE client to
  mount the served directory.
- **Bindings:** how frames ride gRPC, a WebSocket or stdio.

Out of scope, left to the systems that use it:

- which command goes to which sandbox (routing, scheduling, bindings);
- creating and destroying sandboxes (a control API, not this protocol);
- who the user is and what they may do. The binding authenticates the peer;
  the protocol carries no user identity.

## 2. Prior art and why a new protocol

| Protocol | Covers | Missing for this use |
| --- | --- | --- |
| e2b envd (process and filesystem over Connect RPC) | exec, path-based files | Inode operations a FUSE mount needs (readdirplus, handle-based I/O, locks, hard-link counts) |
| 9P2000.L | Files, with locks and `renameat` | Exec; capability negotiation; flow control |
| SFTP v3 | Files | Locks, symlink fidelity, Windows semantics; this repository has just replaced it with FUSE3 (#136) |
| Kubernetes or Docker exec | Exec | Files; tied to their systems |

The exec messages borrow from envd and from needy's `process.proto`, and the
file messages from 9P2000.L and needy's `fs.proto` (#136, hardened through
v0.0.24).

**SSH was considered and set aside (owner, 2026-11-27).** Running SSH over
the stream would give channels, per-channel windows, encryption and
certificate-based trust for free. But:
- files would still need a custom subsystem, because SFTP is what #136
  replaced;
- environment and signal requests depend on the server's configuration and
  implementation;
- SSH has no hold-and-replay;
- a second encrypted layer hides operations from needy's interception and
  trace;
- an "SSH server" on users' machines invites the wrong expectations.

Boxide keeps SSH's ideas instead: per-stream windows (§4), extension
negotiation (§8), and identity checked before the session starts (§3.1).

## 3. Layers

```
messages   boxide.v1 protobuf:  Frame, session, exec, fs   (the contract)
framing    Frame per logical stream, with credits             (the session)
binding    gRPC bidi stream | WebSocket binary | stdio        (the transport)
```

Every binding carries the same message, `Frame`:

- **gRPC:** `service Boxide { rpc Session(stream Frame) returns (stream Frame); }`;
- **WebSocket:** one binary message per `Frame`, at most 1 MiB;
- **stdio:** a big-endian `uint32` length, then one `Frame`.

A peer that speaks any binding speaks all of the protocol. A device agent
needs only protobuf encoding and a WebSocket, not a gRPC stack.

### 3.1 Securing a binding

Boxide neither encrypts nor authenticates peers by itself; that happens
outside the session.

- **Authentication and authorization belong to whatever opens the
  stream.** A relay authenticates both ends of each connection and connects
  only the peers its policy allows. A direct transport uses its own means.
  Boxide carries no user identity.
- **Session token.** `Hello` carries an opaque `token` that the server
  checks before it sends `Welcome`. The system that opens streams issues it
  for one pairing of client and server, so a stream connected to the wrong
  server fails at once. It guards against misrouting, not against the relay.
- **Confidentiality** comes from the transport: for example TLS between
  each peer and a relay. In v1 a relay can read frames. Deployments whose
  relay is already trusted to act on the server need nothing more.
- **End-to-end encryption** for relays outside that trust is reserved as a
  future optional capability, `secure.e2e` *(provisional)*, for example Noise
  or TLS inside the binding. It is not part of v1.

## 4. Session and frames *(provisional shapes)*

```proto
message Frame {
  uint32 stream = 1;           // 0 is the session itself
  oneof body {
    Hello hello = 2;           // client → server, first frame on stream 0
    Welcome welcome = 3;       // server → client
    Open open = 4;             // open a logical stream (exec or fs)
    Credit credit = 5;         // grant bytes on a stream
    Close close = 6;           // end a stream, with a status
    Ping ping = 7;  Pong pong = 8;
    ExecClientMessage exec_client = 10;
    ExecServerMessage exec_server = 11;
    FsRequest fs_request = 12;
    FsResponse fs_response = 13;
  }
  map<string, bytes> extensions = 15;
}
```

- **Handshake:**
  - `Hello` carries the protocol versions the client speaks (`major.minor`),
    its implementation's name and version, the capabilities it wants, the
    extensions it understands, and the session `token` (§3.1).
  - `Welcome` returns the version chosen, the capabilities granted (the
    intersection) and the server's implementation.
  - A server with no common major version answers with `Close` and a reason.
- **Streams:**
  - Odd stream IDs are opened by the client and even ones by the server
    (reserved: the server opens none in v1).
  - Within a stream, frames are ordered. Across streams, nothing is ordered.
- **Flow control:**
  - Each stream has a byte credit in each direction, 256 KiB to start.
  - Data frames count their payload against it, and the receiver returns
    `Credit` as it consumes.
  - Credits are end to end, so a relay keeps no flow state and one slow stream
    cannot block another. That is the head-of-line problem Konnectivity hit
    (apiserver-network-proxy #881).
- **Liveness:** `Ping` every 15 s. Three missed `Pong`s end the session.
  Bindings may add their own keepalive.
- **Errors:** `Close.status` uses a closed set of codes (`ok`, `cancelled`,
  `invalid`, `not_found`, `permission`, `unsupported`, `unavailable`,
  `internal`) and a message. File errors use errno values in `FsResponse`
  (§6.2).

## 5. Exec

A client opens an exec stream with `Open{exec: Start}`:

| Client → server | Server → client |
| --- | --- |
| `Start{argv, cwd, env, clear_env, tty{rows, cols}?, stdin: open/closed}` | `Started{pid}` |
| `Stdin{bytes}`, `StdinEof` | `Stdout{bytes}`, `Stderr{bytes}` (one stream when `tty` is set) |
| `Signal{name}` (POSIX names: `INT`, `TERM`, `KILL`, `HUP`, `QUIT`, `USR1`, `USR2`, `WINCH`, …) | `Exited{code}` or `Exited{signal}` |
| `Resize{rows, cols}` | `Error{status}` |

Rules:

- **Signal delivery:** a signal goes to the process group the server started
  for the command. A server that cannot deliver a named signal returns
  `unsupported` for that `Signal` and keeps the process running. Windows
  servers emulate `INT` by writing `0x03` to a PTY.
- **End of stream:** an exec stream ends after `Exited` *and* the end of
  output. A server sends no output after `Exited`.
- **Environment:** `env` is complete when `clear_env` is set. Otherwise it is
  added to the server's environment.

Optional capability **`exec.hold`**:

- The server keeps the process and its output after the stream or session
  drops, for a time it states in `Welcome`.
- Output carries an absolute byte offset. `Open{attach: {process, offset}}`
  resumes from that offset. If the offset has left the buffer, the server sends
  `Snapshot{bytes}` (a terminal repaint) first.

## 6. Files

### 6.1 Session

`Open{fs: Mount{root, read_only, lock_mode}}` returns the root inode and the
file capabilities granted. All later requests on the stream name inodes and
handles from that mount and carry a request `id`, so many can be in flight.

### 6.2 Operations

| Core (every file server) | Optional capability |
| --- | --- |
| `Lookup`, `Forget`/`BatchForget`, `GetAttr`, `SetAttr` (mode, size, times) | `fs.link` (hard links) |
| `ReadDirPlus` (entries with attributes, paged) | `fs.locks` (`GetLock`, `SetLock`, by owner) |
| `Open`, `Read{handle, offset, size}`, `Write{handle, offset, bytes}`, `Flush`, `Fsync`, `Release` | `fs.compound` (`CreateAndWrite`, `ReadWhole`) |
| `Create`, `Mkdir`, `Unlink`, `Rmdir`, `Rename{flags}`, `Symlink`, `ReadLink`, `StatFs` | `fs.strict-root` (confinement enforced by the kernel) |

Rules:

- **Names and paths are bytes**, not strings: POSIX names need not be UTF-8.
  A Windows server encodes UTF-16 names as WTF-8.
- **Errors are Linux errno numbers**, whatever the server's OS, for example
  `ENOENT` 2, `EACCES` 13, `EEXIST` 17, `EXDEV` 18, `EINVAL` 22, `ENOSPC`
  28, `EROFS` 30, `ENOTEMPTY` 39, `ELOOP` 40. The numbers are fixed by this
  spec, not taken from the host.
- **Inode identity:**
  - An inode number is stable for the life of a mount, and the server keeps it
    until the client's `Forget` count drops it to zero.
  - Two names for one file (hard links, and a case-insensitive host's aliases)
    give the same inode.
- **Writes** are at explicit offsets. A server opens handles so the client's
  kernel owns append and write-back, as FUSE write-back caching requires.
- **Confinement:** a server MUST refuse every operation that would resolve
  outside the mount's root, including through symlinks and `..`. Without
  `fs.strict-root`, it does so on a best-effort basis: check by path, open with
  no-follow, then compare the opened file's identity. It states that it is
  best effort by not granting the capability.
- **Caching:** attributes carry a validity hint (a TTL) that clients may use.
  The protocol promises nothing about changes made on the host beyond it.

## 7. Capabilities

| Name | Meaning |
| --- | --- |
| `exec`, `exec.pty` | exec streams; with a PTY |
| `exec.hold` | §5 hold and replay |
| `fs`, `fs.write` | file streams; writable mounts |
| `fs.link`, `fs.locks`, `fs.compound`, `fs.strict-root` | §6.2 |

A client that requires a capability refuses a server that does not grant it.
It never degrades silently: a client that needs `fs.strict-root` refuses a
best-effort server and says so.

## 8. Extensions

`Frame.extensions` and an `extensions` map on `Start` and `Mount` carry data
the protocol does not define, under reverse-DNS keys
(`dev.needy.run`, `dev.needy.origin`). A peer ignores keys it does not
understand. Each side lists the keys it knows in `Hello` and `Welcome`, so
neither depends on the other knowing a key.

This is where today's needy-only fields go (`run_id`, `executor`, `runner`,
`origin`, `search_path_if_missing` on `ExecStart`), keeping `boxide.v1`
free of them.

## 9. Versioning and stability

- `major.minor`: a minor version only adds fields, messages and
  capabilities. A major version may change them, and a server may speak
  several.
- Messages and fields outside the frozen core are marked
  `// EXPERIMENTAL` in the proto and are not covered by the guarantee.
- Buf's `FILE` breaking check runs on `proto/boxide/` and must pass for a
  minor release.

## 10. Keeping two copies in sync (while it lives here)

- `proto/boxide/v1/` imports nothing from `proto/needy/`. needy maps its own
  messages onto it in code, never in the schema.
- A constant `BOXIDE_PROTOCOL_VERSION` *(provisional)* in the schema's
  options or a top-level comment changes on every edit. A copy elsewhere
  (for example a device agent's generated TypeScript) records the needy tag
  and the version it was generated from, and its CI compares them.
- Splitting later is moving the directory and the conformance suite to a new
  repository unchanged.

## 11. Conformance

- A black-box suite drives a server through the stdio binding: core cases
  for every server, and capability cases only for what the server grants.
- It lives in `xtask` at first (`cargo xtask boxide-conformance --server
  'CMD …'`, *provisional*) and moves with the protocol.
- The suite ships with a minimal reference server in `xtask`, used only to
  test the suite and needy's client. The first real server is a device
  agent's TypeScript implementation, which runs the same suite in its own CI.

## 12. needy's use of it

- needy's daemon is a Boxide client, over the length-prefixed binding inside
  broker streams (broker spec §6.3).
- needy's existing `RunnerService.Task`, `RunnerService.FsTask` and
  `FsService.Session` stay as they are. Boxide is used for brokered
  executors only; needy's runner is not migrated (owner, 2026-11-28).
  Revisit only if keeping two paths becomes a cost.

## 13. Open questions

1. *Decided 2026-11-27:* the protocol is Boxide, and its first package is
   `boxide.v1`.
2. *Decided 2026-11-28 (owner):* no JSON mapping. Frames are binary
   protobuf on every binding (a WebSocket carries them as binary messages);
   debugging goes through tools that decode frames.
3. *Decided 2026-11-28 (owner):* needy's runner keeps its gRPC `Task` and
   `FsTask`; Boxide serves brokered executors only (§12).
4. Windows: which semantics a Windows server must provide to pass the core
   suite (POSIX rename over an open file is not available through
   `MoveFileExW`).
5. Whether `exec.hold`'s snapshot is a terminal repaint (`Snapshot` bytes) or
   left to the client.
