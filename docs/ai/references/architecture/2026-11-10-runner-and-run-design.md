# Runners and runs

**Status**: design, 2026-11-10. Slice 2 is partly implemented: *session* is
renamed *run* everywhere, while `--no-actor` stays as `needy run create
--no-actor` until `exec --runner` exists. Slice 3 (runner registry and
enrollment) is implemented; see [As built: slice 3](#as-built-slice-3).
Nothing else is implemented. The owner chose
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
needy runner token create --ttl 1h [--uses N] [--roles executor] [--labels os=darwin]
needy runner token ls | revoke ID
JOIN=<t> needy daemon --mode runner --join https://ctl.example.com --join-token-env JOIN   # on the machine
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

Transport encryption comes from the tunnel (SSH, Cloudflare Tunnel/Access);
needy serves no TLS (owner decision, 2026-11-10). The runner credential is a
per-runner random token, not a key pair (owner decision, 2026-11-10).

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
seen) and join tokens (hash, TTL, scope, uses), built in slice 3; clients (credential hash,
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
3. **Runner registry and enrollment** (done, 2026-11-10): `state.db` with
   runners, join tokens, credentials; `runner token create/ls/revoke`,
   `runner ls/rotate/rm`; `daemon --mode runner --join`.
4. **Runner dial-out**: control stream and per-task streams; the `remote`
   executor provider is replaced by runners.
5. **Scheduling and profiles**: `runner:` selectors for actors and executors;
   `profile check` through the control plane.
6. **Ownership and reconciliation**: run-scoped cleanup on runners; restart
   reconciliation from `state.db`.
7. **Clients**: pairing, per-client tokens, principals in the trace; `report`
   and `run logs` through the control plane.
8. **Interactive actor**: PTY driver, `run -it`, `run attach`.

## As built: slice 3

**Persistence.** `state.db` sits in the control daemon's state directory
beside `trace.db` and is opened by `needy_daemon::store` (SeaORM entities and
one migration, `m20261110_000030_runners`; a database with migrations it does
not know is refused, as `trace.db` does). Tables:

| Table | Columns |
| --- | --- |
| `runners` | `name` (key), `roles` and `labels` as last reported, `scope_roles` and `scope_labels` from the join token, `join_token` (its ID), `credential_hash`, `pending_hash`, `rotating`, `created_ms`, `last_seen_ms`, `removed_ms` |
| `join_tokens` | `id` (key), `hash`, `roles`, `labels`, `created_ms`, `expires_ms`, `uses_left`, `revoked_ms` |

Roles and labels are JSON text. The store uses one SQLite connection, so a
join token's checks and the use it spends cannot interleave with another join.

**Secrets.** A join token is `needy-join.<id>.<64 hex digits>` and a runner
credential `needy-runner.<name>.<64 hex digits>`: 32 random bytes (`rand`)
after a prefix naming the row, so a check reads one row and compares one
hash. Only the SHA-256 (`sha2`) of the whole string is stored, compared in
constant time (`subtle`). A salted, slow hash is not needed for 256 random
bits. Both are shown once: the join token to the operator, the credential to
the runner, which keeps it in `runner.json` in its state directory (0600,
written to a temporary file and renamed).

**Join tokens.** `--ttl` defaults to 1 h and `--uses` to 1. A token's roles
limit what a runner may declare (every role when it names none); its labels
are given to every runner that joins with it, and a runner label with the same
key and another value is refused. The same scope applies to every heartbeat,
which reports the runner's roles and labels whole. A join that fails spends no
use. A wrong secret, an unknown ID and a malformed token all read "the join
token is not valid"; only a caller holding the secret learns that it expired,
is used up or was revoked. Revoking a token leaves the runners that joined with
it.

**Authentication.** The middleware now decides the caller per request and
puts a `Caller` into the request's extensions: the listener's caller for the
control token (or for any request on a listener without one), and
`Caller::Runner { name }` for a runner credential, which reaches
`RunnerService.Heartbeat` only. A token shaped like a runner credential that no
runner holds is refused even on a listener without a control token, so a
removed runner learns that it was removed. `Join` needs no bearer token; the
join token in the request authenticates it. Operator methods keep the control
token.

**Heartbeats.** The control plane owns the interval, 15 s by default
(`needy daemon --mode control --heartbeat-interval`), and tells runners in
every join and heartbeat response. A runner is online while a heartbeat
arrived within three intervals, so one late or lost heartbeat (a tunnel that
reconnects) does not flap it; `runner ls` shows `offline` after about 45 s
without one. 15 s keeps `runner ls` fresh within a minute at the cost of one
small unary call per runner. A runner whose heartbeat fails for another reason
than its credential tries again after 5 s, so it is online soon after the
control plane or a tunnel comes back.

**Rotation pickup.** `needy runner rotate NAME` marks the runner `rotating`
and returns no secret. The runner's next heartbeat response carries a new
credential (stored as `pending_hash`); the runner writes it to `runner.json`
first, then sends a heartbeat with it at once. The first use of the new
credential promotes it and retires the old one, which until then stays valid,
so a runner that crashes between receiving and storing the new credential
loses nothing: its next heartbeat with the old one gets yet another new one.
The new credential is never shown to the operator or stored in plain text.
For a credential that may have leaked, rotation is not enough (whoever holds
the old one can pick up the new one): remove the runner and join again.

**Removal.** `needy runner rm NAME` clears the credential hashes and keeps
the row, so `runner ls` shows the runner as `removed`. Its next heartbeat is
refused (`UNAUTHENTICATED`), and the runner daemon stops its executor role and
exits 1, naming its `runner.json` to delete before joining again. A removed
runner's name may join again with a new join token.

**Runner daemon.** `needy daemon --mode runner --join ENDPOINT
--join-token-env NAME | --join-token-file PATH [--name N] [--roles …]
[--labels …]` joins on its first start; later starts reuse `runner.json` and
need neither the join token nor `--join`. The token never appears in the
process's arguments. `--name` defaults to the host name's first label in
lowercase. A restart under another name than the stored one is refused. It
still serves the executor role on `--listen` as `--mode executor` does, and
profiles still reach it through a `remote` executor (`TODO(runner-dial-out)`,
slice 4). Roles are declared by `--roles` (default `executor`); detecting them,
such as `/dev/kvm` for the actor role, is a follow-up (`TODO(runner-roles)`).

**Status table.** `ControlError::Unauthenticated` → `UNAUTHENTICATED` was
added for an invalid, expired, spent or revoked join token; a role or label
outside the token's scope is `PERMISSION_DENIED`, a name that is taken
`FAILED_PRECONDITION`.

## Open questions

- **The control plane as a runner**: is the control host implicitly a local
  runner, or does it join like any other?
- **Groups**: does `{group}` (executors shared across runs) survive, now that
  runners are shared resources anyway?
- **Warm pools**: pre-started actors per runner; out of scope for now.
- **Rotation for a leaked credential**: a heartbeat-delivered rotation
  cannot shut out whoever holds the old credential. Should `runner rotate`
  also offer an out-of-band mode (print the new credential to the operator
  and revoke the old one at once)? Today the answer is `runner rm` and a new
  join.
- **Pruning**: removed runners and spent, expired or revoked join tokens stay
  in `state.db` and in the listings. Prune them after a while, or on request?
