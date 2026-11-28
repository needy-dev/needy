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

**Platform differences.** The capability model follows VS Code's file
system providers, which declare flags such as `PathCaseSensitive` (set on
Linux only) and `FileAtomicRename`, so callers branch on the flag and not on
the OS
([diskFileSystemProvider.ts](https://github.com/microsoft/vscode/blob/e433f25/src/vs/platform/files/node/diskFileSystemProvider.ts#L60-L80)).
Win32-OpenSSH is the counter-example. It advertises `posix-rename` but
implements it with `MoveFileExW`, and it ends any signalled child with exit
code 0 while reporting success
([signal_sigchld.c](https://github.com/PowerShell/openssh-portable/blob/0172cc6/contrib/win32/win32compat/signal_sigchld.c#L177-L195)).
The name mapping follows WSL and Cygwin
([Cygwin special names](https://cygwin.com/cygwin-ug-net/using-specialnames.html)),
and the Windows signal table follows hcsshim's mapping of `TERM` to a console
shutdown event
([signal.go](https://github.com/microsoft/hcsshim/blob/02c8742/internal/signals/signal.go#L165-L202)).
All were read on 2026-11-28.

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

- **gRPC:** `service BoxideService { rpc Session(stream SessionRequest)
  returns (stream SessionResponse); }`, where each message wraps one `Frame`
  (`frame = 1`). The wrapper satisfies Buf's STANDARD lint and adds nothing
  else;
- **WebSocket:** each binary message carries one or more frames, each
  after a big-endian `uint32` length, as on stdio. A sender batches small
  frames (a burst of `FsResponse`s, say) into one message, because a relay
  pays per message. A frame never spans two messages, and a message is at
  most the receiver's frame limit (§4) plus 4 bytes per frame it carries;
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
  A client that launches the server itself over the stdio binding (through
  `ssh`, say) is paired with it by the launch; see question 9 (§13).
- **Confidentiality** comes from the transport: for example TLS between
  each peer and a relay. In v1 a relay can read frames. Deployments whose
  relay is already trusted to act on the server need nothing more.
- **End-to-end encryption** for relays outside that trust is reserved as a
  future optional capability, `secure.e2e` *(provisional)*, for example Noise
  or TLS inside the binding. It is not part of v1.

## 4. Session and frames

The schema is in `proto/boxide/v1/session.proto`; in outline:

```proto
message Frame {
  uint32 stream = 1;           // 0 is the session itself
  oneof body {
    Hello hello = 2;           // client → server, first frame on stream 0
    Welcome welcome = 3;       // server → client
    StreamOpen open = 4;       // open a stream: ExecStart, ExecAttach or FsMount
    Credit credit = 5;         // grant bytes on a stream
    StreamClose close = 6;     // end a stream (or the session), with a StatusCode
    Ping ping = 7;  Pong pong = 8;
    ExecInput exec_input = 10;    // client → server on an exec stream
    ExecOutput exec_output = 11;  // server → client on an exec stream
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
    intersection) and the server's implementation, plus the server's `TERM`
    grace period (§5) and, with `exec.hold`, how long it holds a process and
    how many output bytes it keeps.
  - Each side states `max_frame_bytes`, the largest encoded `Frame` it
    accepts. It is at least 1 MiB, which is also the value when it is 0.
    A peer never sends a frame larger than the other side's limit; a larger
    one is a protocol error that ends the session with `invalid`.
  - A server with no common major version answers with `Close` and a reason.
- **Streams:**
  - Odd stream IDs are opened by the client and even ones by the server
    (reserved: the server opens none in v1).
  - Within a stream, frames are ordered. Across streams, nothing is ordered.
- **Flow control:**
  - Each stream starts with as much credit in each direction as the
    receiver's frame limit, and the session with twice that, shared by all
    its streams. A `Credit` on stream 0 grants session credit; on any
    other stream, that stream's.
  - Data counts against both: `stdin`, `stdout` and `stderr` bytes on an
    exec stream, and on a file stream the encoded size of the `FsRequest` or
    `FsResponse` message itself, not of the `Frame` around it. Both sides
    count exactly these bytes; a sender and a receiver that count
    differently drift apart until the stream stalls. The receiver returns
    `Credit` as it consumes. Control frames (`Open`, `Credit`, `Close`,
    `Ping`, `Pong`) are not counted.
  - NOTICE: an earlier draft started streams at 256 KiB and the session at
    1 MiB. A file response cannot be split and may be as large as the
    frame limit (at least 1 MiB), so a sender holding 256 KiB could never
    send it. The first implementations hit this (2026-11-28); a window at
    least as large as one frame is what keeps every frame sendable.
  - A receiver may grant more than it was granted at the start, to fit the
    path's bandwidth-delay product: it measures the round trip with `Ping`
    and grows a window that is used up within one round trip, as HTTP/2
    and QUIC receivers do. It never takes back credit it has granted. A
    sender that exceeds its credit ends the session with `invalid`.
  - Credits are end to end, so a relay keeps no flow state and one slow stream
    cannot block another. That is the head-of-line problem Konnectivity hit
    (apiserver-network-proxy #881). The session credit bounds what one
    session can leave queued in a relay.
  - NOTICE: needy's own gRPC runs with 16 MiB stream and 32 MiB
    connection windows on HTTP/2 (`crates/api-client/src/lib.rs`,
    `crates/api-server/src/server.rs`, v0.0.26), because a 256 KiB window
    caps a 100 ms path near 2.5 MiB/s. Boxide starts at one frame
    for relays and grows by measurement instead of fixing large windows.
- **Liveness:** `Ping` every 15 s. Three missed `Pong`s end the session.
  Bindings may add their own keepalive.
- **Errors:** `StreamClose.code` uses a closed set of codes (`ok`, `cancelled`,
  `invalid`, `not_found`, `permission`, `unsupported`, `unavailable`,
  `internal`) and a message. File errors use errno values in `FsResponse`
  (§6.2).

## 5. Exec

A client opens an exec stream with `StreamOpen{exec: ExecStart}` and then
sends `ExecInput` and receives `ExecOutput` (`proto/boxide/v1/exec.proto`):

| Client → server | Server → client |
| --- | --- |
| `StreamOpen{exec: ExecStart{argv, cwd, env, clear_env, tty{rows, cols}?, no_stdin}}` | `ExecOutput.started{pid, process}` |
| `ExecInput.stdin`, `ExecInput.stdin_eof` | `ExecOutput.stdout`, `ExecOutput.stderr` (one stream when `tty` is set) |
| `ExecInput.signal` (POSIX names without `SIG`: `INT`, `TERM`, `KILL`, `HUP`, …) | `ExecOutput.exited{code}` or `ExecOutput.exited{signal}`; `ExecOutput.signal_unsupported` |
| `ExecInput.resize{rows, cols}` | `StreamClose{code}` when the stream fails (for example the program cannot start) |

Rules:

- **Never report what did not happen.** A server answers a signal it cannot
  deliver as asked with `ExecOutput.signal_unsupported` and leaves the process
  running. It
  does not turn the signal into something else unless the table below says
  so, and it never ends a process because of a signal it does not support.
  `Exited{signal}` names a signal only when one really ended the process.
  (Win32-OpenSSH ends every child with exit code 0 and reports success for any
  signal; §2 lists it as the behaviour this rule forbids.)
- **Signal delivery:** with `exec.pgroup`, a signal goes to the process group
  the server started for the command; without it, to the process and the
  children the server can find. With `exec.signals.posix`, every POSIX
  signal is delivered as itself. Without it, the server delivers only
  these:

  | Signal | Without `exec.signals.posix` (e.g. Windows) |
  | --- | --- |
  | `INT` | With a PTY, write `0x03`. Without one, `CTRL_BREAK_EVENT` to the command's console process group (started with `CREATE_NEW_PROCESS_GROUP`). |
  | `TERM` | `CTRL_BREAK_EVENT`. If the process is still running after a grace period stated in `Welcome`, end the tree as for `KILL`. |
  | `KILL` | End the whole process tree (a job object, then `taskkill /T /F` as a fallback). `Exited{signal: KILL}`. |
  | `WINCH` | Ignored; size changes go through `Resize`. |
  | any other | `unsupported`. |
- **Exit status:** `Exited{code}` is the process's own exit code. A server
  that ended the process reports `Exited{signal}` with the signal it acted
  on.
- **Started means started:** a server sends `started` only once the
  program runs. A program that cannot start (it does not exist, it is not
  executable, the `cwd` is not a directory) is a `StreamClose{not_found}`
  instead, and no `started` comes first, so a client can tell a command
  that never ran from one that failed.
- **End of stream:** an exec stream ends after `Exited` *and* the end of
  output. A server sends no output after `Exited`.
- **A client that leaves:** when the client closes an exec stream, or the
  session ends, without `exec.hold` the server ends the command's process
  tree as for `KILL`.
- **Environment:** `env` is complete when `clear_env` is set. Otherwise it is
  added to the server's environment.

Optional capability **`exec.hold`**:

- The server keeps the process and its output after the stream or session
  drops, for a time it states in `Welcome`.
- Output carries an absolute byte offset. `StreamOpen{attach: ExecAttach{process, offset}}`
  resumes from that offset.
- The server keeps only raw output bytes, in a buffer whose size it states in
  `Welcome`. If the requested offset has left the buffer, the server first
  sends `ExecGap{from, to}` naming the bytes that are lost, then resumes from the
  oldest byte it still holds. The client decides what to show (for example
  a notice, or a terminal reset).
- TODO(boxide-snapshot): a server-side terminal repaint (a headless terminal
  emulator serializing the screen, as Orca and VS Code do) is left out of v1.
  It would make every server run a terminal emulator. Revisit as an optional
  capability once a client needs a repaint that it cannot produce itself.

## 6. Files

### 6.1 Session

`StreamOpen{fs: FsMount{root, read_only, lock_mode}}` opens a file stream on
the server directory `root` (bytes; the server decides which roots it serves).
The first response, `FsMounted`, returns the root inode and the file
capabilities granted for this mount. All later requests on the stream name inodes and
handles from that mount and carry a request `id`, so many can be in flight.

`FsMounted.max_io_bytes` is the largest `Read.size` and `Write.data` the
server accepts on the mount. A read's reply travels to the client and a
write's request to the server, so the server chooses it below *both*
frame limits (§4), with room for the rest of the message. A client splits a larger read or write into several requests
in flight at once. A server answers a `Read` with every byte asked for
unless the file ends first, so a client never takes a short read for the
end of the file.

Optional capability **`fs.resume`**:

- `FsMounted` returns a `resume` token and `resume_ms`. When the stream or
  the session drops, the server keeps the mount's inodes, open handles and
  locks for `resume_ms`.
- A client that reconnects opens `FsMount{resume: token}`. The server
  answers `FsMounted{resumed: true}` with the same root and generation, and
  every inode and handle the client holds is still valid. If the token has
  expired or is unknown, it answers with a new mount and `resumed: false`.
- After a resume the client re-sends what was unanswered at the drop when
  repeating it is harmless: `Lookup`, `GetAttr`, `ReadDirPlus`,
  `ReadLink`, `StatFs`, `Read`, `Write` (explicit offsets), `Flush`,
  `Fsync`, `GetLock`, and `Release` (a server answers `EBADF` for a handle
  it already released, and the client takes that as done). Every other
  operation that was unanswered fails to its caller with `EIO`, because
  its outcome is unknown. `Forget` is never re-sent: a lost one only keeps an
  inode alive until the mount ends.
- Without `fs.resume`, or when a resume fails, every inode and handle of
  the old mount is gone. A client answers operations on them with `ESTALE`
  or looks the paths it knows up again on the new mount; the protocol
  promises no identity across mounts.
- TODO(boxide-resume-replay): a server-side log of finished replies, so a
  re-sent non-idempotent request is answered instead of failing with `EIO`,
  is left out of v1. It would make a server keep replies it cannot know were
  delivered. Revisit when a client shows the `EIO`s matter in practice.
- NOTICE: needy's own file client re-sends only `FsMount` after a
  reconnect, and the server gives out fresh inode numbers, so inodes cached
  from before it are not remapped (`TODO(fs-reconnect-inodes)` in
  `crates/core-actor/src/fs/fuse.rs`, v0.0.26). `fs.resume` is what lets a
  Boxide client avoid that.

### 6.2 Operations

| Core (every file server) | Optional capability |
| --- | --- |
| `Lookup`, `Forget`/`BatchForget`, `GetAttr`, `SetAttr` (mode, size, times) | `fs.link` (hard links) |
| `ReadDirPlus` (entries with attributes, paged) | `fs.locks` (`GetLock`, `SetLock`, by owner) |
| `Open`, `Read{handle, offset, size}`, `Write{handle, offset, bytes}`, `Flush`, `Fsync`, `Release` | `fs.compound` (`CreateAndWrite`, `ReadWhole`) |
| `Create`, `Mkdir`, `Unlink`, `Rmdir`, `Rename{flags}`, `StatFs` | `fs.symlink` (`Symlink`, `ReadLink`); `fs.symlink-dangling` (to a missing target) |
| | `fs.prefetch-tree` (`FsPrefetchTree`: directory listings several levels deep in one round trip; a reply that would pass the frame limit stops early and lists the directories it did not visit in `unvisited`) |
| | `fs.resume` (§6.1) |
| | `fs.strict-root`, `fs.mode-bits`, `fs.ownership`, `fs.case-sensitive`, `fs.unlink-open`, `fs.rename-replace-open` (§7) |

Rules:

- **Names and paths are bytes**, not strings: POSIX names need not be UTF-8.
  A Windows server encodes UTF-16 names as WTF-8.
- **Names the host cannot store:** a server maps the characters
  `" * : < > ? \ |`, the control characters U+0001 to U+001F, and a
  trailing `.` or space to U+F000 plus the character's code point (`foo:bar`
  is stored as `foo\uF03Abar`), and maps them back when it reads names.
  This is the scheme WSL and Cygwin use, so files stay readable to both. A
  name it cannot map (`CON`, `NUL`, `COM1` …) is `EINVAL`, never `ENOENT`.
- **Errors are Linux errno numbers**, whatever the server's OS, for example
  `EPERM` 1, `ENOENT` 2, `EIO` 5, `EBADF` 9, `EACCES` 13, `EBUSY` 16 (a sharing violation),
  `EEXIST` 17, `EXDEV` 18, `EINVAL` 22, `ENOSPC` 28, `EROFS` 30,
  `ENOSYS` 38, `ENOTEMPTY` 39, `ELOOP` 40, `EOPNOTSUPP` 95, `ESTALE` 116. The numbers
  are fixed by this spec, not taken from the host.
- **Attributes report what is true.** `SetAttr` applies what the host
  supports and its reply carries the resulting attributes. Without
  `fs.mode-bits`, only the write bits are kept (on Windows, the read-only
  attribute), and the reply shows the mode as it now is rather than the mode
  asked for. Without `fs.ownership`, a change of uid or gid is `EPERM`.
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
- **Paging:** `ReadDirPlus` returns fewer than `max_entries` entries, with
  `eof` unset, when more would not fit a frame. `FsPrefetchTree` lists whole
  directories only and returns the inodes it reached but did not list in
  `unvisited`, so a client continues from them.

## 7. Capabilities

| Name | Meaning |
| --- | --- |
| `exec`, `exec.pty` | exec streams; with a PTY |
| `exec.hold` | §5 hold and replay |
| `fs`, `fs.write` | file streams; writable mounts |
| `exec.pgroup` | signals reach the command's whole process group |
| `exec.signals.posix` | every POSIX signal is delivered as itself (§5 table otherwise) |
| `fs.link`, `fs.locks`, `fs.compound`, `fs.prefetch-tree`, `fs.strict-root` | §6.2 |
| `fs.resume` | a mount survives a dropped stream (§6.1) |
| `fs.symlink`, `fs.symlink-dangling` | symlinks; symlinks to missing targets (Windows fixes a link's file or directory type when it is created) |
| `fs.mode-bits`, `fs.ownership` | all permission bits; uid and gid |
| `fs.case-sensitive` | names differing only in case are different files (mount-wide in v1) |
| `fs.unlink-open` | unlinking an open file removes the name while handles keep working |
| `fs.rename-replace-open` | renaming over an open target replaces it atomically |

A client that requires a capability refuses a server that does not grant it.
It never degrades silently: a client that needs `fs.strict-root` refuses a
best-effort server and says so.

Capabilities describe behaviour, not operating systems: a client checks
`fs.case-sensitive`, never "is this Windows". A Linux server grants almost
all of them. A Windows server grants what its implementation reaches: needy's
Rust executor reaches `fs.unlink-open` and `fs.rename-replace-open` through
`FileDispositionInfoEx`/`FileRenameInfoEx` with POSIX semantics, which
Node's `fs` does not expose (libuv renames with `MoveFileExW`).

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
  The core cases are the same on every operating system.
- The suite also checks the negative side: an operation that needs a
  capability the server did not grant must fail with the documented errno
  or `unsupported`, never succeed partially or report success.
- It lives in `xtask` at first (`cargo xtask boxide-conformance --server
  'CMD …'`, *provisional*) and moves with the protocol.
- The suite ships with a minimal reference server in `xtask`, used only to
  test the suite and needy's client. The first real server is a device
  agent's TypeScript implementation, which runs the same suite in its own CI.

## 12. needy's use of it

- needy's daemon is a Boxide client, over the length-prefixed binding inside
  broker streams (broker spec §6.3).
- *Experiment (2026-11-28, not a support claim):* an executor with
  `boxide: { command: [...] }` *(provisional)* has the daemon launch a
  server and speak the stdio binding to it, before brokers exist. Evidence
  level: one local run on an Apple silicon Mac, a `macos-vz` actor whose
  workspace and routed commands were served by a TypeScript server through
  the daemon's translation of needy's process and file sessions. Nothing
  else (Linux actors, relays, Windows servers) was run.
- What needy's own sessions carry that Boxide does not, found by that
  translation:
  - needy has no message for a signal the server could not deliver
    (`signal_unsupported`), no field for a prefetch's `unvisited`
    directories, and no attribute validity (`valid_ms`).
  - needy names files with strings, so a name that is not UTF-8 is left
    out of listings.
  - A Boxide server knows no runs: process ids, `ListProcesses`,
    `KillProcess` and the `max_procs` limit are the client's.
  - needy's routing retries a missing absolute program by its bare name on
    the server's `PATH` (`search_path_if_missing`); the client does that
    retry.
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
4. *Decided 2026-11-28 (owner):* platform differences are capabilities
   (§7). The core suite is the same everywhere, invalid names map to U+F000,
   and a server never reports a signal or an operation it did not carry out
   (§5, §6.2, §11).
5. *Decided 2026-11-28 (owner):* `exec.hold` keeps raw bytes and reports
   lost ranges with `Gap`; no server-side snapshot in v1 (§5). Previously:
   whether `exec.hold`'s snapshot is a terminal repaint (`Snapshot` bytes) or
   left to the client.
6. *Decided 2026-11-28 (owner):* what needy's file service learned
   (v0.0.24 to v0.0.26) goes into v1: a frame limit each side states, with
   reads, writes and listings split to fit (§4, §6.1, §6.2); a session
   credit next to stream credits, and windows that grow by measurement
   (§4); several frames per WebSocket message (§3); and `fs.resume` (§6.1).
7. *Open (2026-11-28):* what `stdin_eof` means on a PTY. A PTY has no end
   of input; the first server writes `^D`, which ends input only for a
   program in canonical mode.
8. *Open (2026-11-28):* whether `Welcome` reports the server user's home
   directory. needy expands `~` in an executor's paths and workspace with
   it; without it, a Boxide executor needs absolute paths.
9. *Open (2026-11-28):* whether the stdio binding may use an empty session
   token when the client launched the server itself (the first
   implementations do), or whether the launcher must pass one.
10. *Open (2026-11-28):* `Rename` flags on a server without `renameat2`.
    Node has none, so the first server checks for the target before it
    renames (`RENAME_NOREPLACE`, racy) and refuses `RENAME_EXCHANGE`. A
    capability (`fs.rename-noreplace`, provisional) would let a server say
    it cannot do it atomically.
11. *Open (2026-11-28):* a `boxide.command` executor runs a command of the
    profile's on the control host. That is fine for a developer's own
    daemon and not for a shared control plane, which must refuse it (as
    `--disable-host-files` refuses host mappings) and use brokers.
12. *Open (2026-11-28):* what a server reports for a device, FIFO or
    socket. `FsFileType` has regular files, directories and symlinks only;
    the first server reports the others as regular files.
