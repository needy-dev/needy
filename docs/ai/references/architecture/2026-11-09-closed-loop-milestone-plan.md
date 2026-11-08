# Closed-loop milestone

**Status**: plan. The goal is one end-to-end loop that exercises every part of
the [actor and executor separation](2026-11-09-actor-executor-separation-design.md)
at least once, before any part is deepened. Decided on 2026-11-09: snapshots
are out of scope; the previous HTTP daemon, Docker sessions and batch runs are
deleted, not migrated; `proto/` is redesigned from scratch.

## The loop

On one Linux host with KVM (kvm-host for development):

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

| # | Item | Depends on | Notes |
| --- | --- | --- | --- |
| 1 | Delete the previous daemon, Docker sessions, batch runs and their docs and tests | — | Keep `exec-intercept` as the fallback outside microVMs. |
| 2 | Redesign `proto/`: session, process (exec, attach, list, kill), file, trace and executor services | Transport findings | Must work over one outbound HTTP/2 connection for tunnels. |
| 3 | Daemon core: session manager, keyed executor pool, Docker and remote providers | 2 | No scheduler; runtime options pass through with protected fields. |
| 4 | Actor provider: Firecracker, `needy build --recipe` from the proof-of-concept Dockerfile | Recipe compiler | Includes CPUID, hwmon, DMI and PCI where ready. |
| 5 | Actor ↔ daemon channel | vsock findings | Rules in, events out. |
| 6 | Exec routing in the guest kernel, with a proxy that speaks the process service | Kernel prototype findings | ptrace fallback for comparison. |
| 7 | Exec streams: PTY, pipes, signals, detach, attach, replay | 2, 3 | Executor side first, then routed exec. |
| 8 | FUSE file routing, same path and UID | 2, 3 | Read and write; no change notifications yet. |
| 9 | Two networks: actor gateway with key injection and allowlist; executor network allowlist | 3, 4 | Plain HTTP to the actor gateway, TLS upstream. |
| 10 | Rules: YAML profiles, command matching, accumulation, built-in launcher resolvers | — | Declarative only. |
| 11 | Trace (SQLite + JSONL) and `needy report` | 3 | OpenTelemetry export later. |
| 12 | End-to-end test of the loop on kvm-host | all | The milestone is done when this passes. |

## Out of the milestone

Snapshots; OpenTelemetry export; terminal recording; Arm hosts and
apple/container; executor dial-out through a real tunnel (the transport must
already allow it); workspace `overlay` and `worktree` modes for shared
executors.
