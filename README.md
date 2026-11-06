# Needy

Needy is a headless runtime for running commands on another machine. The Rust host daemon manages Docker containers and persistent workspaces. An optional Linux exec supervisor can keep a process tree on Linux while selected commands run on macOS. A paired CLI submits batch jobs, reads their events and retrieves files. Needy has no agent-specific concepts: a workload is any command, and any agent or tool can run on top of it.

Docker workloads use an existing Docker installation. On macOS, Docker provides the Linux VM. Native host execution is separately enabled by the operator; it works without Docker. Needy does not install Docker, provide a public relay or attach to an existing terminal.

## Requirements

- Rust 1.97 or later to build the binary.
- Docker Engine with a running daemon, or a Docker-compatible backend on macOS, for container workloads.
- Network access while building the runtime image.
- Any credentials a workload needs must be provided by that workload. Needy does not configure model providers or inject credentials.

## Install

From this checkout on the host:

```sh
./scripts/install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

The script builds `needy`, checks Docker and builds the runtime image `needy-runtime:rust`. It installs the binary at `PREFIX/bin/needy`. Use `--skip-image` to install only the binary, or `--image NAME` to change the image tag. Docker is required only when building or running Docker workloads. It does not install a background service.

For development:

```sh
make build
make image
```

The same `bin/needy` binary is the daemon, client, container runner and Linux proxy. Build it separately for each host architecture. `make release` and the installer produce optimized binaries. Images build the Linux runner from the same Rust source using `images/Dockerfile`.

The workspace contains two crates: [`needy`](crates/needy) implements the product, and [`needy-exec-intercept`](crates/exec-intercept) contains only Linux seccomp/ptrace supervision. The latter depends only on `libc` at runtime and can be packaged independently. Its package name and license remain provisional; it has not been published.

Use a new data directory for this rewrite, for example `.needy/rust-host`. Rust state uses `rust-state.json`; the daemon refuses a directory containing only Go `state.json`. There is no implicit state or volume migration. The previous implementation is retained in Git at `dbea069` and in the original worktree.

## Keep the process tree on Linux and run selected tools on Mac

On the Mac, explicitly enable logical command names and a working-directory root:

```sh
bin/needy daemon --no-docker --data-dir .needy/mac-host \
  --listen 127.0.0.1:7331 --host-root /Users/you/project \
  --allow-host-exec uname,xcodebuild
```

Forward that loopback endpoint to the Linux machine through SSH, or configure
HTTPS. Pair the Linux client using the [pairing steps](#pair-a-client) below.
For a private CA, pass `needy pair --ca-cert /path/to/ca.crt ...`; the profile
stores its absolute path. Then run on Linux:

```sh
needy intercept --remote-cwd /Users/you/project --select uname,xcodebuild \
  -- /bin/sh -c 'cat /proc/sys/kernel/ostype; uname -s'
```

This prints `Linux` from the root shell and `Darwin` from the selected `uname`.
The root process's first exec always stays on Linux. Later selected execs are
replaced with a local Needy proxy, keeping their Linux PID and parent/wait
relationship. Proxy descendants remain local to prevent recursive interception.
The library uses seccomp events only for `execve`/`execveat`, and `PTRACE_CONT`
for normal execution. Linux x86-64 and ARM64 are supported.

Stdin, separate stdout/stderr, exit status and common process signals cross the
connection. When both stdin and stdout are terminals, the proxy allocates a Mac
PTY and forwards window changes and Ctrl-C; use `--no-pty` to force pipes.
`--forward-env LANG,LC_ALL` explicitly selects environment values to forward;
the host must allow them through `--host-env` (default `TERM,LANG,LC_ALL`).
The bridge currently requires UTF-8 arguments; the library preserves arbitrary
argument and environment bytes.

For a direct native command from any paired client:

```sh
needy exec --cwd /Users/you/project -- uname -s
```

Host execution is trusted operator access. `--host-root` limits the initial cwd;
it is not a filesystem jail. An allowed shell or tool can access the host with
the daemon's permissions. The bridge does not synchronize files, virtualize
inherited non-stdio descriptors, mirror remote grandchildren into Linux or
provide full shell job control. The selected executable must reach an exec
syscall; shell lookup failures before exec cannot be intercepted. Prepare any
shared source tree and path mapping explicitly. See the [crate contract](crates/exec-intercept/README.md).

## Start the host

Start a command-only host first:

```sh
needy daemon \
  --data-dir "$HOME/.local/share/needy" \
  --listen 127.0.0.1:7331 \
  --image needy-runtime:rust \
  --max-concurrent 2
```

Keep the daemon in the foreground. Its private state directory stores client authorization, session metadata, run inputs and durable run events. Run inputs and output may contain workload secrets, so treat the state directory and its backups as secrets. Docker volumes hold each session's workspace and home. Keep this directory and the Docker volumes together when preserving a host.

For a remote host, connect through an SSH tunnel from the client machine:

```sh
ssh -N -L 7331:127.0.0.1:7331 user@host
```

The examples below then use `http://127.0.0.1:7331`. For direct network access, configure HTTPS using `--tls-cert FILE --tls-key FILE` and an appropriate `--listen` address. Clients reject unencrypted HTTP to non-loopback addresses and do not follow redirects.

## Pair a client

In a second terminal on the host, generate a short-lived, single-use code:

```sh
needy pairing-code \
  --data-dir "$HOME/.local/share/needy" \
  --out /tmp/needy-pairing-code
```

Transfer that file to the client through a private channel, then pair:

```sh
needy pair \
  --url http://127.0.0.1:7331 \
  --code-file /tmp/needy-pairing-code \
  --name laptop
rm /tmp/needy-pairing-code
needy info
```

Without `--code-file`, `pair` reads the code from standard input. Pairing stores a bearer token in the user's private configuration directory, at `needy/client.json`. Use the global `--config FILE` option for a separate client profile. Treat that file as a credential. A host operator can inspect and revoke clients with `needy clients list` and `needy clients revoke CLIENT_ID`.

## Run a batch workspace

Create a session:

```sh
needy sessions create --name demo
```

Use the returned session ID in place of `SESSION_ID`. Upload a directory that contains only the files the remote workload should receive:

```sh
needy workspace push SESSION_ID ./project
needy run SESSION_ID -- sh -c 'pwd; ls -la; printf "hello from Needy\n" > result.txt'
```

Use the returned run ID to read its events and status:

```sh
needy events RUN_ID --follow
needy runs show RUN_ID
```

For batch jobs, `run` returns after the host accepts the work. Closing `events`, losing the network connection or closing the client does not cancel the run. Reconnect from the last received event ID:

```sh
needy events RUN_ID --after 12 --follow
```

Each session permits one active run. The daemon also limits total active runs with `--max-concurrent`. A full host returns a conflict; this MVP does not queue or distribute tasks across hosts.

Download the workspace into a new or empty directory:

```sh
needy workspace pull SESSION_ID ./downloaded-project
```

Workspace archives contain regular files and directories only. Archive symlinks, hardlinks and unsafe paths are rejected. The CLI skips source symlinks and paths named `.git`, `.needy`, `.env`, `.env.local` or `target`; it does not apply `.gitignore`. Other secret files are still included. Prepare an export directory containing only the intended inputs when needed. Transfers are limited to 10,000 entries and a 32 MiB tar archive, including headers.

Upload the source before starting the session's first run. After any run is created, further uploads are rejected; create a new session to upload a new source tree. This avoids merging files into a workspace already changed by a workload.

## Command input

A run receives whatever the command asks for. Use `--stdin FILE` to send a file to the command's standard input, or `--stdin -` to read Needy's own standard input:

```sh
needy run SESSION_ID --stdin input.txt -- sh -c 'wc -l'
needy run SESSION_ID --stdin - -- cat
```

Any tool, agent or harness can run this way. Needy does not interpret prompts, select models or inject credentials; put an agent in the image or upload it with the workspace and invoke it like any other command.

## Cancel and clean up

```sh
needy stop RUN_ID
needy runs show RUN_ID
needy sessions delete SESSION_ID
```

Stop active work before deleting its session. Session deletion removes its managed workspace and home volumes; download wanted files first. Stopping the host daemon leaves batch containers under Docker so a restarted daemon can reconcile and observe them.

## Trust and limits

Pair only trusted users. A paired client can submit arbitrary commands and manage this daemon's sessions. Sessions isolate files and processes through Docker; paired clients are not separate tenants with private authorization boundaries. Workloads can access the network and any credentials deliberately uploaded with them, and can return those secrets as output. Use a dedicated host or VM when the workload's trust level requires it.

Run containers use a non-root user, a read-only root filesystem, a temporary `/tmp`, dropped Linux capabilities, no-new-privileges and an init process. The default limits are 2 CPUs, 2 GiB of memory and 256 processes per container. Only the managed workspace, home and read-only run inputs are mounted. Containers use Docker's ordinary bridge network; they do not receive the host network, a published port or the Docker socket.

For batch jobs, the image runner limits each output line to 8 MiB and cumulative enveloped output to 16 MiB, including while the daemon is offline. The daemon also enforces a 16 MiB observed-output limit. Limit violations fail the run. Docker's per-container log limit is larger than the runner's output budget so normal runner output stays available for replay until session deletion. These are workload limits for trusted clients, not a hostile-tenant disk quota: workspace writes and retained sessions still consume host disk. Monitor disk space and delete sessions when their results are no longer needed.

This MVP has no dashboard, public relay, automatic service installation, cross-host scheduler, arbitrary Docker host-path mounts or takeover of existing terminal sessions. See [API](docs/api.md) for the wire contract and the [Rust rewrite plan](docs/plans/rust-rewrite.md) for scope and acceptance.

## Verify

```sh
make check
make test
make host-test
make test-image
make docker-test
```

The Docker suite uses real containers. It does not call a model provider.

Linux kernel tests must run on Linux; macOS compilation cannot exercise ptrace.
See [current verification and reproduction](docs/verification.md), including
dual architecture kernel results, Linux-to-Mac execution and injected Docker
faults.
