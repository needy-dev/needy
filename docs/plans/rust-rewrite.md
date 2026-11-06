# Rust rewrite

Baseline: `dbea069`, worktree `.worktrees/rust-rewrite`, branch `feat/rust-rewrite`.

## Goal and boundaries

Reimplement the documented Linux-to-macOS transparent exec prototype in Rust,
extract the Linux mechanism into a separately packageable crate, and replace the
Go/Node runtime without dropping its headless Docker, pairing, workspace and
durable batch capabilities. Needy treats every workload as an ordinary command;
agents are not part of its model. Original prototype source is not required.
Retain the old implementation as a reference until replacement acceptance.
Committing and pushing this rewrite to `main` was subsequently authorized.
Package publication, deployment to the earlier remote pilot and real model
account use remain outside this execution.

The Linux library owns seccomp, ptrace, process-tree state and bounded argument
access. Needy owns placement, network transport, PTYs and host execution. Root
launch and proxy-runtime locality are explicit invariants.
The intercept mechanism is not a sandbox or a shared-filesystem implementation.

## User journeys

| Journey | Status |
| --- | --- |
| Launch a Linux process tree and redirect selected execs without changing the root process's placement | passed: x86-64 and ARM64 kernels |
| Run the selected command on macOS with stdin, output, status, signals and PTY support | passed: actual Mac process, HTTPS, pipes, signal names and PTY |
| Pair a Rust client, run isolated persistent Docker sessions and recover batch events | passed: real containers, restart, event replay and injected faults |
| Package the Linux crate, document runnable commands and verify the replacement boundaries | passed: package, docs, installed CLI and real image regression |

## Acceptance

- Real Linux kernel checks for execve/execveat, descendants, argv/env, root and
  proxy locality, redirection failure, signal exit and cleanup. Native aarch64
  first; x86-64 must be separately verified before claiming its acceptance.
- Actual Linux process to macOS execution, not a canned platform response.
- Actual Docker pairing, upload, command execution, download, cancellation,
  event replay and daemon restart.
- Formatting, clippy, cargo tests and a local package build. Publication is a
  separate action; licensing and the final public package name remain undecided.
- Explicitly record anything unverified.

## Implementation record

- Product runtime is Rust: CLI, pairing/TLS, durable batch/store, Docker lifecycle,
  host worker, PTY and Linux proxy.
- `needy-exec-intercept` contains only the Linux mechanism and libc runtime
  dependency, with architecture-specific register operations behind its API.
- The old Go/Node runtime was removed after replacement acceptance; Git baseline
  `dbea069` and the original worktree preserve it for comparison.
- Details and evidence limits are in [current verification](../verification.md).
- No package publish or migration of the earlier remote pilot is part of this execution.
