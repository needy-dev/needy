# Usage details

This page keeps the operational details that the [README](../README.md) leaves
out. See [API](api.md) for the wire contract.

## Install and state

`./scripts/install.sh` builds `needy`, checks Docker and builds the runtime
image `needy-runtime:rust`. It installs the binary at `PREFIX/bin/needy`. Use
`--skip-image` to install only the binary, or `--image NAME` to change the image
tag. Docker is required only when building or running Docker workloads. The
script does not install a background service.

The same `bin/needy` binary is the daemon, client, container runner and Linux
proxy. Build it separately for each host architecture. `make release` and the
installer produce optimized binaries. Images build the Linux runner from the
same Rust source using `images/Dockerfile`.

The workspace contains two crates: [`needy`](../crates/needy) implements the
product, and [`needy-exec-intercept`](../crates/exec-intercept) contains only
Linux seccomp/ptrace supervision. The latter depends only on `libc` at runtime
and can be packaged independently. Its package name and license remain
provisional; it has not been published.

Use a new data directory for this rewrite, for example `.needy/rust-host`. Rust
state uses `rust-state.json`; the daemon refuses a directory containing only Go
`state.json`. There is no implicit state or volume migration. The previous
implementation is retained in Git at `dbea069`.

## Host daemon

Keep the daemon in the foreground. Its private state directory stores client
authorization, session metadata, run inputs and durable run events. Run inputs
and output may contain workload secrets, so treat the state directory and its
backups as secrets. Docker volumes hold each session's workspace and home. Keep
this directory and the Docker volumes together when preserving a host.

For direct network access, configure HTTPS using `--tls-cert FILE --tls-key
FILE` and an appropriate `--listen` address. Clients reject unencrypted HTTP to
non-loopback addresses and do not follow redirects.

## Pairing

Without `--code-file`, `pair` reads the code from standard input. Pairing stores
a bearer token in the user's private configuration directory, at
`needy/client.json`. Use the global `--config FILE` option for a separate client
profile. Treat that file as a credential. A host operator can inspect and revoke
clients with `needy clients list` and `needy clients revoke CLIENT_ID`.

For a private CA, pass `needy pair --ca-cert /path/to/ca.crt ...`; the profile
stores its absolute path.

## Runs and workspaces

For batch jobs, `run` returns after the host accepts the work. Closing `events`,
losing the network connection or closing the client does not cancel the run.
Reconnect from the last received event ID with
`needy events RUN_ID --after 12 --follow`.

Each session permits one active run. The daemon also limits total active runs
with `--max-concurrent`. A full host returns a conflict; Needy does not queue or
distribute tasks across hosts.

Workspace archives contain regular files and directories only. Archive
symlinks, hardlinks and unsafe paths are rejected. The CLI skips source symlinks
and paths named `.git`, `.needy`, `.env`, `.env.local` or `target`; it does not
apply `.gitignore`. Other secret files are still included. Prepare an export
directory containing only the intended inputs when needed. Transfers are
limited to 10,000 entries and a 32 MiB tar archive, including headers.

Upload the source before starting the session's first run. After any run is
created, further uploads are rejected; create a new session to upload a new
source tree. This avoids merging files into a workspace already changed by a
workload.

`--stdin FILE` sends a file to the command's standard input; `--stdin -` reads
Needy's own standard input. Needy does not interpret prompts, select models or
inject credentials.

Stop active work before deleting its session. Session deletion removes its
managed workspace and home volumes; download wanted files first. Stopping the
host daemon leaves batch containers under Docker so a restarted daemon can
reconcile and observe them.

## Linux-to-Mac execution

The root process's first exec always stays on Linux. Later selected execs are
replaced with a local Needy proxy, keeping their Linux PID and parent/wait
relationship. Proxy descendants remain local to prevent recursive interception.
The library uses seccomp events only for `execve`/`execveat`, and `PTRACE_CONT`
for normal execution. Linux x86-64 and ARM64 are supported.

Stdin, separate stdout/stderr, exit status and common process signals cross the
connection. When both stdin and stdout are terminals, the proxy allocates a Mac
PTY and forwards window changes and Ctrl-C; use `--no-pty` to force pipes.
`--forward-env LANG,LC_ALL` explicitly selects environment values to forward;
the host must allow them through `--host-env` (default `TERM,LANG,LC_ALL`). The
bridge currently requires UTF-8 arguments; the library preserves arbitrary
argument and environment bytes.

Host execution is trusted operator access. `--host-root` limits the initial cwd;
it is not a filesystem jail. An allowed shell or tool can access the host with
the daemon's permissions. The bridge does not synchronize files, virtualize
inherited non-stdio descriptors, mirror remote grandchildren into Linux or
provide full shell job control. The selected executable must reach an exec
syscall; shell lookup failures before exec cannot be intercepted. Prepare any
shared source tree and path mapping explicitly. See the
[crate contract](../crates/exec-intercept/README.md).

## Trust and limits

Pair only trusted users. A paired client can submit arbitrary commands and
manage this daemon's sessions. Sessions isolate files and processes through
Docker; paired clients are not separate tenants with private authorization
boundaries. Workloads can access the network and any credentials deliberately
uploaded with them, and can return those secrets as output. Use a dedicated host
or VM when the workload's trust level requires it.

Run containers use a non-root user, a read-only root filesystem, a temporary
`/tmp`, dropped Linux capabilities, no-new-privileges and an init process. The
default limits are 2 CPUs, 2 GiB of memory and 256 processes per container.
Only the managed workspace, home and read-only run inputs are mounted.
Containers use Docker's ordinary bridge network; they do not receive the host
network, a published port or the Docker socket.

For batch jobs, the image runner limits each output line to 8 MiB and cumulative
enveloped output to 16 MiB, including while the daemon is offline. The daemon
also enforces a 16 MiB observed-output limit. Limit violations fail the run.
Docker's per-container log limit is larger than the runner's output budget so
normal runner output stays available for replay until session deletion. These
are workload limits for trusted clients, not a hostile-tenant disk quota:
workspace writes and retained sessions still consume host disk. Monitor disk
space and delete sessions when their results are no longer needed.
