# Runners and runs

**Status**: design, 2026-11-10. Nothing here is implemented. The owner chose
the model (runners as registered machines; actors and executors as what runs on
them; runs instead of sessions); open questions are at the end. This note
absorbs slice 9 (persistence) of the
[module layout redesign](2026-11-09-module-layout-redesign.md): what `state.db`
stores changes with this model.

Related notes:

- [Actor and executor separation](2026-11-09-actor-executor-separation-design.md)
- [Container actor runtime](2026-11-10-container-actor-runtime-design.md)
- [Closed-loop results](2026-11-09-closed-loop-results-evidence.md)

## Why

Using needy on real machines (a KVM host, a Mac, cloud VMs) showed:

- **The session hides the actor and the executors.** A session is one ID. You
  cannot list the machines needy can use, see which are online or busy, or act
  on the actor directly; a session without an actor (`--no-actor`) is a set of
  executors that is also called a session. The word "session" does not say
  "one run of a driving program".
- **Machines live inside profiles.** A remote executor's URL and `token_env` are
  written in every profile that uses it. The token is read from the control
  daemon's environment, so adding a machine or rotating its token means
  restarting the daemon.
- **One shared, static token** per daemon, full power, no identity per client,
  no revocation except a restart. A daemon listening on TCP without a token
  accepts anyone.
- **The control daemon dials executors**, so every executor needs a listening
  port and a tunnel (`ssh -R`, Cloudflare Tunnel).
- **Nothing owns a remote executor's processes after a run.** The `remote`
  provider's `destroy` does nothing; a workspace mount's `sftp-server` and
  detached processes outlive the session on the Mac.
- **Reports and checks are local.** `needy report` reads the local `trace.db`;
  `profile check` checks a recipe against the local CPU, not the machine the
  actor will run on.

## Terms

| Term | Meaning |
| --- | --- |
| **Runner** | A machine running `needy daemon` that has joined a control plane. It declares **roles** it can host and **labels**. Runners are registered ahead of time, listed, rotated and removed. |
| **Role** | What a runner can host: `actor` (with runtimes, e.g. `firecracker`, `container`) and `executor` (with providers, e.g. `native`, `docker`). |
| **Actor** | An instance on a runner with the `actor` role: the sandbox that runs one driving program. Usually created per run and destroyed after it. |
| **Executor** | An instance on a runner with the `executor` role where routed commands run: the runner's own process space (`native`) or a container it creates (`docker`). |
| **Run** | One execution of a driving program in an actor, with the executors bound to it, the rules that route between them, and its trace and report. Replaces *session*. |
| **Control plane** | `needy daemon --mode control`: the runner registry, scheduling, routing decisions, the actor gateway, the trace. It may also be a runner itself (a local runner). |

*Runner* is the machine; *actor* and *executor* are what runs on it. In CI
products "runner" often means the thing that executes jobs; the glossary states
the needy meaning explicitly.

## CLI

```sh
# Runners
needy runner token create --ttl 1h [--roles executor] [--labels os=darwin]
needy daemon --mode runner --join https://ctl.example.com --join-token <t>   # on the machine
needy runner ls
#   NAME        STATUS  ROLES                                       LABELS
#   kvm-1       online  actor(firecracker) executor(docker,native)  kvm
#   mac         online  executor(native)                            os=darwin
needy runner rotate mac
needy runner rm mac

# Runs
needy run [-it] --profile opencode.yaml [-- cmd]   # create, stream or attach, clean up on exit
needy run ls
needy run logs -f RUN
needy run attach RUN                               # the actor's terminal (interactive actor)
needy run stop RUN
needy report RUN                                   # through the control plane, from any client

# Direct use of an executor, without a run
needy exec --runner mac -- uname -a
needy exec --run RUN -- make test                  # by the run's rules
needy ps [--run RUN] [--runner NAME] [-a]
```

## Profiles

A profile describes a run and no longer carries machine addresses or secrets.
Executors and the actor select runners by name or labels:

```yaml
actor:
  runtime: firecracker
  runner: { labels: [kvm] }         # any runner with the actor role and these labels
executors:
  mac:  { runner: mac }             # a native executor on a named runner
  build: { runner: { labels: [kvm] }, provider: docker, key: "{run}" }
commands:
  - { match: "{xcrun,swift} **", to: mac }
```

`profile check` asks the control plane, which checks the recipe against the
runner the actor would be scheduled on.

## Enrollment and authentication

Following the pattern of Kubernetes TLS bootstrapping, GitHub Actions runners,
Buildkite agents and Tailscale auth keys:

1. **Join token**: created by an operator, short-lived (TTL), optionally
   single-use, scoped to roles and labels. Stored hashed.
2. **Runner credential**: on join, the runner gets its own credential and the
   join token is no longer needed. Rotation (`runner rotate`) replaces it over
   the existing connection; removal (`runner rm`) revokes it. Credentials are
   stored hashed in `state.db`.
3. **Clients** (the CLI, people, tools): on the control host, the Unix socket
   protected by file permissions (`0700` directory, `0600` socket, set
   explicitly). Remotely, a per-client token from a pairing step, revocable,
   recorded as the principal on every trace event the client causes.
4. **Actors** keep channel-bound identity: an actor reaches the control plane
   only through its own guest channel, bound to its run.
5. **Refuse unauthenticated network listeners**: a TCP listener on a
   non-loopback address without authentication does not start.

Transport encryption comes from the tunnel (SSH, Cloudflare Tunnel/Access) or
from TLS in needy; see open questions.

## Connection direction

Runners dial the control plane. Each runner keeps one **control stream**; for
each task (an exec, a file transfer, an actor) the control plane announces a
task ID on it and the runner opens a stream for that task (`Exec(task_id)`),
as the [CLI note](../cli/2026-11-09-cli-exec-and-report-design.md#grpc) sketched.
Consequences:

- Runners need no listening port: NAT, home networks and Cloudflare all work
  with outbound connections only. Only the control plane is reachable (directly
  or through Cloudflare Tunnel + Access).
- A runner that reconnects reports the instances and processes it still has;
  the control plane reconciles them (below).

## Ownership and cleanup

Every instance (actor, executor container, executor process) belongs to a run
and lives on a runner. When a run ends, the control plane tells each runner to
end that run's instances; the runner kills the run's processes (including a
workspace mount's `sftp-server`) and removes its containers. A runner that
loses the control plane keeps running instances for a grace period and ends
them if the control plane does not claim them on reconnect.

## Persistence (`state.db`)

Replaces slice 9. Tables: runners (name, roles, labels, credential hash, last
seen), join tokens (hash, TTL, scope, uses), clients (credential hash,
principal), runs (state, profile, timestamps, result), instances (run, runner,
kind, state), processes (instance, origin, state). On start the control plane
loads runs and waits for runners to reconnect and report; instances no runner
reports are marked lost; runs whose actor is gone end `failed` with the reason.
`trace.db` stays separate.

## Interactive actor

`needy run -it` and `needy run attach` connect a terminal to the driving
program: it runs in a PTY in the actor, and a newly attached client gets the
current screen redrawn from a terminal-state model rather than a raw byte
replay. Designed in
[Terminals, attach and the interactive actor](../cli/2026-11-10-terminal-attach-design.md).

## Migration

No compatibility (principle 7 of the module layout redesign): `session` is
renamed `run` in the proto, the CLI, the daemon, the trace and the docs in the
slices that touch them; `--no-actor` sessions are removed in favor of
`exec --runner`; profile executor entries lose `url` and `token_env`.

## Slices (proposed)

1. **Hardening now** (independent of this design): refuse unauthenticated
   non-loopback TCP listeners; explicit socket and directory permissions.
2. **Run rename**: `session` → `run` across proto, CLI, daemon, trace, docs;
   remove `--no-actor`; `exec --runner`.
3. **Runner registry and enrollment**: `state.db` with runners, join tokens,
   credentials; `runner token create/ls/rotate/rm`; `daemon --mode runner
   --join`.
4. **Runner dial-out**: control stream and per-task streams; the `remote`
   executor provider is replaced by runners.
5. **Scheduling and profiles**: `runner:` selectors for actors and executors;
   `profile check` through the control plane.
6. **Ownership and reconciliation**: run-scoped cleanup on runners; restart
   reconciliation from `state.db`.
7. **Clients**: pairing, per-client tokens, principals in the trace; `report`
   and `run logs` through the control plane.
8. **Interactive actor**: PTY driver, `run -it`, `run attach`.

## Open questions

- **Runner credential**: a long random per-runner token (simple) or a key pair
  with signed requests / mTLS (no shared secret on the wire)?
- **TLS in needy**: serve TLS (rustls) on the control plane, or always rely on a
  tunnel?
- **The control plane as a runner**: is the control host implicitly a local
  runner, or does it join like any other?
- **Groups**: does `{group}` (executors shared across runs) survive, now that
  runners are shared resources anyway?
- **Warm pools**: pre-started actors per runner; out of scope for now.
