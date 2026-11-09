# Closed-loop milestone

**Status**: mostly done; see
[closed-loop results](2026-11-09-closed-loop-results-evidence.md). The goal is one end-to-end loop that exercises every part of
the [actor and executor separation](2026-11-09-actor-executor-separation-design.md)
at least once, before any part is deepened. Decided on 2026-11-09: snapshots
are out of scope; the previous HTTP daemon, Docker sessions and batch runs are
deleted, not migrated; `proto/` is redesigned from scratch.

## The loop

On one Linux host with KVM (a Linux KVM host for development):

1. `needy build --recipe gce-e2-standard-4` builds an actor image and a
   Firecracker executor image from one recipe.
2. `needy session create --profile demo` starts the daemon-managed actor
   microVM and binds a Docker executor and, optionally, a remote Mac executor.
3. A driving program inside the actor, a shell script first and then a small
   model-calling harness, runs `uname -a`, `git status`, `pnpm test` and
   `curl https://example.com`:
   - `uname` stays on the actor and answers from the recipe;
   - `git` and `pnpm` run on the executor in `~/workspace`, with a PTY when
     attached to a terminal;
   - `curl` follows the executor network policy.
4. The harness edits a workspace file through the FUSE mount; the executor sees
   the change at the same path with the same owner.
5. A chaos rule fails one `open` with `EIO` and sets the fake coretemp to 101 °C.
6. Model calls go through the actor gateway, which injects the API key.
7. `needy exec -t -- bash` from the host opens an interactive shell on the same
   executor; `needy exec -d -- sleep 600` detaches, and `needy attach` replays.
8. `needy report latest --level summary` shows the routing decisions, the fault,
   the identity probes and both networks.

## Work items

| # | Item | State |
| --- | --- | --- |
| 1 | Delete the previous daemon, Docker sessions, batch runs and their docs and tests | Done; the ptrace-based `exec-intercept` crate was later removed because nothing used it |
| 2 | Redesign `proto/`: health, process (exec, attach, list, kill), session (with `Decide`), guest (config, events, exit, files) | Done |
| 3 | Daemon core: session manager, keyed executor pool, Docker, native and remote providers | Done; `containerd`, `firecracker` and `k8s` executors remain |
| 4 | Actor provider: Firecracker, `needy build --recipe` | Done |
| 5 | Actor ↔ daemon channel | Done: gRPC over vsock |
| 6 | Exec routing in the guest kernel | Done: `binfmt_needy`; execs by `/.needy` programs are never routed |
| 7 | Exec streams: PTY, pipes, signals, detach, attach, replay | Done |
| 8 | File routing, same path and UID | Done: sshfs passive mode over an exec stream to `sftp-server`; `path_map` for executors with other paths |
| 9 | Two networks | Actor gateway done (allowlist, upstream mapping, key injection); executor network policy remains |
| 10 | Rules: YAML profiles, matching, accumulation, launcher resolvers | Done in `needy-config` |
| 11 | Trace (SQLite + JSONL) and `needy report` | Done, plus asciicast recordings |
| 12 | End-to-end test on a Linux KVM host | Done with opencode, a Mac executor and an sshd scenario |

## Out of the milestone

Snapshots; OpenTelemetry export; terminal recording; Arm hosts and
apple/container; executor dial-out through a real tunnel (the transport must
already allow it); workspace `overlay` and `worktree` modes for shared
executors.
