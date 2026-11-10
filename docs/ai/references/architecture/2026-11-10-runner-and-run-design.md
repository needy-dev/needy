# Runners and runs

**Status**: design, 2026-11-10. Slices 1 to 7 are implemented: *session* is
renamed *run* and `needy run create --no-actor` is gone (slice 2, finished
in slice 5); the runner registry and enrollment (slice 3), runner dial-out
(slice 4), scheduling and profiles (slice 5), ownership and
reconciliation (slice 6), and clients (slice 7); see
[As built: slice 3](#as-built-slice-3), [slice 4](#as-built-slice-4),
[slice 5](#as-built-slice-5), [slice 6](#as-built-slice-6) and
[slice 7](#as-built-slice-7). Nothing else is implemented. The owner chose
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
  runner: { labels: [kvm] }         # the connected runner with the actor role and these labels
executors:
  mac:  { runner: mac }             # a native executor on a named runner
  build: { runner: { labels: [kvm] }, provider: docker, key: "{run}" }
commands:
  - { match: "{xcrun,swift} **", to: mac }
```

`profile check` asks the control plane, which checks the recipe against the
runner the actor would be scheduled on. As built, with the cuts this slice
made, in [As built: slice 5](#as-built-slice-5). An actor `runtime:`
(`firecracker`, `container`) is not a profile key yet; the
[container actor runtime](2026-11-10-container-actor-runtime-design.md) is
designed, not built.

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
   recorded as the principal on every trace event the client causes. The
   control daemon's own token stays the bootstrap and admin credential. As
   built in [slice 7](#as-built-slice-7).
4. **Actors** keep channel-bound identity: an actor reaches the control plane
   only through its own guest channel, bound to its run.
5. **Refuse unauthenticated network listeners**: a TCP listener on a
   non-loopback address without authentication does not start.

Transport encryption comes from the tunnel (SSH, Cloudflare Tunnel/Access);
needy serves no TLS (owner decision, 2026-11-10). The runner credential is a
per-runner random token, not a key pair (owner decision, 2026-11-10); so is
a client's token (owner decision, 2026-11-10).

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
  the control plane reconciles them (below; as built in slice 6).

The streams as built are in [As built: slice 4](#as-built-slice-4).

## Ownership and cleanup

Every instance (actor, executor container, executor process) belongs to a run
and lives on a runner. When a run ends, the control plane tells each runner to
end that run's instances; the runner kills the run's processes (including a
workspace mount's `sftp-server`) and removes its containers. A runner that
loses the control plane keeps running instances for a grace period and ends
them if the control plane does not claim them on reconnect. As built in
[slice 6](#as-built-slice-6).

## Persistence (`state.db`)

Replaces slice 9. Tables: runners (name, roles, labels, credential hash, last
seen) and join tokens (hash, TTL, scope, uses), built in slice 3; clients (credential hash,
principal), runs (state, profile, timestamps, result), instances (run, runner,
kind, state), processes (instance, origin, state). On start the control plane
loads runs and waits for runners to reconnect and report; instances no runner
reports are marked lost; runs whose actor is gone end `failed` with the reason.
`trace.db` stays separate. Runs, instances and processes are as built in
[slice 6](#as-built-slice-6); clients and pairing codes in
[slice 7](#as-built-slice-7).

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
`exec --runner` (done in slice 5; a workflow that needs a profile's rules
and executors without a driving program uses a run whose actor only waits,
as `examples/cli` and `examples/local` do); profile executor entries lose
`url` and `token_env` (done in slice 5).

## Slices (proposed)

1. **Hardening now** (independent of this design): refuse unauthenticated
   non-loopback TCP listeners; explicit socket and directory permissions.
2. **Run rename**: `session` → `run` across proto, CLI, daemon, trace, docs;
   remove `--no-actor`; `exec --runner`.
3. **Runner registry and enrollment** (done, 2026-11-10): `state.db` with
   runners, join tokens, credentials; `runner token create/ls/revoke`,
   `runner ls/rotate/rm`; `daemon --mode runner --join`.
4. **Runner dial-out** (done, 2026-11-10): control stream and per-task
   streams; `exec`, `ps` and `fs cp` reach a runner with `--runner`. The
   `remote` executor provider stayed until slice 5 replaced it.
5. **Scheduling and profiles** (done, 2026-11-10): `runner:` selectors for
   actors and executors; `profile check` through the control plane; the
   `remote` provider and `run create --no-actor` removed.
6. **Ownership and reconciliation** (done, 2026-11-10): runs, instances and
   processes in `state.db`; run-scoped cleanup on runners; reconnect and
   restart reconciliation.
7. **Clients** (done, 2026-11-10): pairing, per-client tokens, principals in
   the trace; `report` and `run logs -f` through the control plane.
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
lowercase. A restart under another name than the stored one is refused. In
slice 3 it still served the executor role on `--listen` as `--mode executor`
does; slice 4 removed that listener (see below). Roles are declared by `--roles` (default `executor`); detecting them,
such as `/dev/kvm` for the actor role, is a follow-up (`TODO(runner-roles)`).

**Status table.** `ControlError::Unauthenticated` → `UNAUTHENTICATED` was
added for an invalid, expired, spent or revoked join token; a role or label
outside the token's scope is `PERMISSION_DENIED`, a name that is taken
`FAILED_PRECONDITION`.

## As built: slice 4

**Streams.** Two new `RunnerService` methods, both reached only by a runner's
credential (`Caller::Runner`); an operator's token gets `PERMISSION_DENIED`:

| RPC | Opened by | Up (runner → control plane) | Down (control plane → runner) |
| --- | --- | --- | --- |
| `Control(stream ControlRequest) returns (stream ControlResponse)` | the runner, once, kept open | `TaskResult { task_id, list_processes \| kill_process \| error }` | `RunnerTask { id, exec \| list_processes \| kill_process }` |
| `Task(stream TaskRequest) returns (stream TaskResponse)` | the runner, once per exec task | first `task_id`, then `exec: ExecResponse` …, or `error: TaskError` | `exec: ExecRequest`, the first being the start or the attach |

For an exec (`exec`, `attach`, `fs cp`, which runs `tar` over exec) the
control plane announces `RunnerTask { id, exec }` on the control stream and
waits for the runner to open `Task` with that ID; the exec's own
`ExecRequest` and `ExecResponse` messages then travel unchanged, so terminal
mode, window sizes, signals, replay, backpressure and refusals behave as on
any executor. The runner sends the first response only once its process
service accepted the start or attach, or `TaskError { code, message }` (the
gRPC status, such as `FAILED_PRECONDITION` for a command that cannot start)
in its place; the control plane returns that status to its caller as an
executor would. List and kill are one request and one reply, so their
result comes back on the control stream (`TaskResult`): a task stream for
them would cost a round trip and carry nothing. Task IDs count up per
control stream. The control plane waits 10 s for a task stream or a result;
a runner whose connection died without a reset then fails the task instead
of holding the caller.

On the control plane a connected runner is an `Executor`
(`needy_api_client::Executor`, which `Client` also implements), so the relay,
`ps`, `kill` and file copies use one code path for a daemon reached over gRPC
and a runner reached over its streams. The executor looks the runner's
current stream up on every call, so a process started before a reconnect
can be listed, attached and killed after it.

**Heartbeats stay unary.** Liveness (`online` within three intervals) and
the crash-safe rotation pickup of slice 3 pair one request with one response
and were verified that way; the control stream answers a different question,
whether a task can reach the runner now. A runner whose control stream is not
open fails a task at once with `runner NAME is not connected`
(`UNAVAILABLE`), even while `runner ls` still shows it `online` from its last
heartbeat. Moving heartbeats onto the control stream is a follow-up if one
connection per runner matters.

**Reconnect.** The runner connects with HTTP/2 pings every 20 s
(`Client::connect_kept_alive`), so a connection that dies without a reset
fails within a minute. When its control stream ends or fails it connects
again with exponential backoff (`backon`): 0.5 s doubling, with jitter, at
most 15 s, starting over after a stream that was open. A credential the
control plane refuses ends the runner as a refused heartbeat does. A new
control stream of the same runner replaces the older one. Execs in flight
when the connection drops end for their callers with `ended` ("the
connection ended before the command exited: runner NAME disconnected", exit
255 in `needy exec`). On the runner, an exec whose task stream ended has lost
its client: a process it did not detach gets the same hangup as with any
client that leaves (when it next writes, see the follow-ups), and detached
processes keep running. In slice 4 the runner did not report or
reconcile its processes when it reconnected; slice 6 added that.

**Ends.** `runner rm` ends the runner's control stream, so the runner connects
again at once, is refused and exits. The control daemon ends every control
stream when it stops serving, so its shutdown grace period does not wait for
them.

**No listener.** `needy daemon --mode runner` listens nowhere and refuses
`--listen` and `--token-env`; it serves its processes only over the streams it
opened. Nothing needed the slice 3 listener: profiles reach another machine
through `needy daemon --mode executor` and a `remote` executor, as before.

**Profiles: the CLI-only cut.** In slice 4 profiles could not place
executors on runners yet; slice 5 added that (below). A runner's
machine is reachable outside any run: `needy exec --runner NAME`, `needy ps
--runner NAME`, `needy fs cp --runner SRC NAME:DIR`; attach and kill go by
process ID. `ps` without a filter also lists every connected runner's
processes. A command names a run or a runner, not both (`INVALID_ARGUMENT`).

## As built: slice 5

**Profile schema.** An executor is `{ runner: SELECTOR, provider: native |
docker, … }` and the actor takes `runner: SELECTOR` beside its recipe and
command. A selector is a runner's name (`runner: mac`) or `runner: { labels:
[KEY, KEY=VALUE] }`; `KEY` matches the label with any value. Without
`runner:` an actor or executor runs on the control host itself, as before
runners. `provider` defaults to `native`: the runner's own process space, or
the control daemon's for an executor without `runner:`. An executor with
`docker:` options and another provider is an error finding, since the
default changed from Docker. `containerd`, `firecracker` and `k8s` stay
refused at create (`TODO(provider)`). Removed, with no compatibility:
`runtime:` (its message names `provider:`), `runtime: remote` and `remote:
{url, token_env}` (the message says to join the machine as a runner), the
`remote` provider, and `run create --no-actor` with the `start_actor` field
of `CreateRunRequest` (reserved): every run has an actor.

**Scheduling** (`needy_daemon::schedule`, a pure function over the runners
the registry holds). `runner: NAME` picks that runner; it must have joined,
hold the role (`actor` or `executor`) and be connected (its control stream
open, not merely `online`). A label selector picks the one connected runner
with the role and every label. No match is an error that names every runner
and why it does not fit (`mac (no label os=linux)`, `kvm-2 (not
connected)`, `gpu (no actor role)`); several matches are an error naming
them, so a run never lands on a machine by chance and the same profile
against the same connected runners always lands in the same place.
Choosing among several (by load, by name) is `TODO(runner-schedule)` and an
open question. A run's create resolves every selector before it acquires
anything, so a selector that does not resolve fails the create with nothing
to release.

**Docker on runners: the control-host cut.** A runner reaches the control
plane through its task streams only; creating a container on a runner would
need create and destroy tasks on its control stream and execs relayed to
the container through the runner. That is deferred
(`TODO(runner-docker)` in the pool and the scheduler). The control daemon
names the runner that runs on its own host, `needy daemon --mode control
--local-runner NAME`; a Docker executor placed on that runner is created by
the control daemon's own Docker provider, as before. `provider: docker` on
any other runner is refused when the run is created and by `profile check`.
`native` on any runner, including the local one, goes through the runner's
streams, so it runs as the runner's user in the runner's process space; its
`workspace` is a path on the runner, as written, and a workspace mount runs
the SFTP server of the runner's platform (`/usr/libexec/sftp-server` on
macOS).

**Actor placement: the control-host cut.** An actor runs where its VM can be
started: the control host. `actor: { runner: … }` is resolved like any
selector, against runners with the `actor` role, and must name the control
daemon's `--local-runner`; any other runner fails the create with `runner
NAME cannot host actors yet`. Running an actor on another runner needs the
actor role over dial-out (the VM's lifecycle and its guest channel relayed
through the runner), `TODO(runner-actor)`. The recipe is compiled against
the control host's CPU when the run is created, which for the local runner
is the same machine.

**Host reports.** Every heartbeat carries the runner's machine
(`HeartbeatRequest.host`, `RunnerHost`): its operating system and
architecture as Rust names them, its CPU vendor, model and flags from
`/proc/cpuinfo` (empty off Linux), and the CPUID entries its KVM supports
(unset without a usable KVM). The runner reads it once at start. The
control plane keeps the last report per runner in memory, since a runner
reports it again within one heartbeat of a control plane restart; a join
carries none.

**`profile check` through the control plane.** `RunService.CheckProfile`
takes the resolved profile and recipe as `CreateRun` does, creates nothing,
and returns where the actor and each executor would run (a runner's name,
or the control host), the CPU model the recipe was checked against, and
findings: the profile's own, a selector that does not resolve, an actor or
Docker executor placed where this slice cannot run it, and the recipe
compiled against the host of the actor: the scheduled runner's reported
CPU, or the control host's own. A runner that reported no host yet is a
warning, and the recipe's host checks are skipped. The CLI prints the
placements, the control plane's findings, the recipe summary (which does
not depend on the host) and the `--cmd` decisions, and exits 1 on an error.
`needy profile check --local` checks offline against this machine's CPU and
resolves no selector.

**Tests.** The daemon's tests run a control plane with joined, connected
runners in one process, and set a runner's reported host through
`Runners::heartbeat` to check the recipe against a CPU without AVX-512.
Every run now starts an actor, so the daemon's and the CLI's tests start a
stand-in Firecracker from a fake image against a stand-in KVM device (the
control daemon's hidden `--kvm` flag; Firecracker itself opens `/dev/kvm`).
The pool's regression test for a slow creation used the `remote` provider
and was dropped; the providers left create at once or need Docker
(`TODO(test)` in the pool's tests).

## As built: slice 6

**Persistence.** A second migration, `m20261110_000060_runs`, adds three
tables to `state.db` (`needy_daemon::store`, module `runs`). States are
words, exits a code and a signal name.

| Table | Columns |
| --- | --- |
| `runs` | `id` (key), `name`, `group_name`, `profile_name`, `profile_yaml` (the resolved profile), `state`, `created_ms`, `updated_ms`, `actor_exit_code`, `actor_exit_signal`, `failure`, `actor_runner` (the actor's placement; empty for the control host) |
| `instances` | `id` (key: `RUN:actor`, `RUN:executor:NAME`), `run`, `name`, `kind` (`actor`, `executor`), `runner` (the runner that owns it; empty for the control host), `provider` (`firecracker`, `native`, `docker`), `runtime` (as `run ls` shows it), `handle` (container name, runner name, `local`, or the VM's directory), `pid` (the VM's Firecracker), `state` (`running`, `released`, `ended`, `lost`), `created_ms`, `ended_ms` |
| `processes` | `id` (key: the executor's process ID), `run`, `instance`, `origin` (empty for a process a runner reported that this control plane did not start), `state` (`running`, `exited`, `ended`, `lost`), `exit_code`, `exit_signal`, `started_ms`, `ended_ms` |

An instance row is a run's hold on an instance: runs sharing a pooled
executor have a row each. Writes happen where the trace events are emitted:
the run at `run.create`, `running`, `stopping`, `stopped`, `finished` and
`failed` (a failed create included); an executor when acquired and
released; the actor when its VM starts and ends; a process when the relay
sees it start and exit, and when a run's end kills it. The registry keeps
live runs in memory and reads ended ones from `state.db`, so `run ls`,
`run logs` and `GetRun` find them after a restart; the in-memory list of
ended runs is gone.

**Ownership protocol.** Two additions to `RunnerService`'s streams:

| Message | Direction | Meaning |
| --- | --- | --- |
| `ControlRequest.report: RunnerReport { processes }` | runner → control plane, first on every control stream | The runner's running processes that carry a run ID |
| `RunnerTask.end_run: EndRun { run_id }`, answered by `TaskResult.end_run: EndRunResponse { processes, ended }` | control plane → runner | Kill the run's processes (`SIGKILL`, a workspace mount's `sftp-server` included), wait up to 5 s for them to exit, and return the run's processes after the end with the IDs this task killed |

When a run ends (stop, finish, a failed create), the control plane sends
`EndRun` to each runner it held a native executor on, in place of listing
and killing processes one by one; the control host's own executors (the
daemon's native executor, Docker containers, which are removed anyway) keep
the list-and-kill path. A runner that is not connected then is not waited
for: its report on reconnect, or its grace period, ends them. On a report,
the control plane claims the processes of runs that are `creating` or
`running` and hold an executor on that runner (recording them if it did not
know them), and sends `EndRun` for every other run; instance rows a restart
left `running` on that runner become `ended`.

**Grace period on runners: 2 minutes** (`runner::DISCONNECT_GRACE`). It
starts when a control stream that was open ends and stops when a new one
opens; when it passes, the runner ends every run's processes itself
(processes outside runs stay). Two minutes is eight reconnect attempts at
the 15 s backoff cap, enough for a control plane or a tunnel restart to come
back and decide; a control plane gone for good leaves a run's processes for
minutes, not hours. A restarted control plane fails the runs whose actors
it lost anyway (below), so a longer grace keeps nothing useful alive. The
hidden flag `needy daemon --mode runner --disconnect-grace` shortens it for
tests.

**Control plane restart.** `Daemon::start` loads the runs left `creating`,
`running` or `stopping` before it serves:

- The actor is **not re-adopted** (decision, 2026-11-10). The Firecracker
  of an actor on the control host survives a killed daemon, and the vsock
  sockets are paths a new daemon could bind again, but the VM is no longer
  its child (its exit status cannot be collected), the guest's connections
  in flight broke (routed commands, the gateway's), and the run's chaos
  counters and trace sequence lived in the old daemon's memory, so an
  adopted run would no longer replay deterministically. The new daemon
  kills a still-running Firecracker (`SIGKILL`) after checking that the
  PID's command line names the run's `vm.json` (`sysinfo`; a PID another
  process took is left alone), and removes the run's sockets.
- The run ends `failed` with `the control plane restarted; the actor was
  lost` (`… while the run was being created; its actor was lost` for a
  create), or `stopped` if it was stopping.
- Its Docker containers on the control host are removed (`ended`); its
  native executor on the control host is marked `lost`
  (`TODO(runner-ownership)` in `reconcile`: a daemon killed with `SIGKILL`
  leaves those processes orphaned and unrecorded).
- Its instances on runners wait for the runners to report, at most **60 s**
  (`REPORT_WAIT`, four times the reconnect backoff cap, shorter than the
  runners' grace period): a report makes them `ended` (after `EndRun`); the
  rest become `lost`, with their processes. `run ls` shows them
  `mac=runner (lost)` (`ExecutorRef.lost`, a new proto field).

The reconciled run gets no `run.fail` in its trace (`TODO(trace)`: a new
`Dispatch` would number events from 0); `state.db` and `run ls` carry the
reason.

**Graceful shutdown.** A control daemon stopped by a termination signal
stops every run as soon as the signal comes, before it ends the runners'
control streams, so the runners hear `EndRun` and nothing of the runs stays
on them. Before slice 6 the streams ended first and the runs stopped after
the server's 5 s grace for open calls; a run's open execs now end at once
with the run (`daemon shutting down`), and only calls outside runs get the
grace. Found on the Mac: a detached process of a run outlived the shutdown
until the runner connected to the next control plane.

**Tests.** The store's API (`store::runs`); a run's life visible after an
in-process restart (shutdown and start on the same directories); through a
control plane and a runner dialing through a proxy that can be cut or taken
down: a reconnect keeps a live run's process, a run stopped while the
runner was away is ended on its report, a runner past its grace period ends
a run's process and leaves one outside runs; a seeded `state.db` with a
stand-in leftover VM: the run fails with the reason, the VM is killed, the
unreported runner instance and the control host's native one are lost; a
PID another process took is left alone; a stopping run ends stopped. The
CLI's tests kill a control daemon (`SIGKILL`) during a run on a runner
daemon and restart it: the run is listed `failed` with the reason and the
runner's process ends.

## As built: slice 7

**Persistence.** A third migration, `m20261110_000070_clients`, adds two
tables to `state.db` (`needy_daemon::store`, module `clients`):

| Table | Columns |
| --- | --- |
| `pairing_codes` | `id` (key), `hash`, `name` (the client's name once it pairs), `created_ms`, `expires_ms`, `used_ms` |
| `clients` | `id` (key), `name`, `pairing_code` (its ID), `token_hash` (cleared at revocation), `created_ms`, `last_used_ms`, `revoked_ms` |

**Secrets.** A pairing code is `needy-pair.<id>.<64 hex digits>`, a client
token `needy-client.<id>.<64 hex digits>`, made and checked as join tokens
and runner credentials are: 32 random bytes, only the SHA-256 stored,
compared in constant time, shown once. A wrong secret, an unknown ID and a
malformed code all read "the pairing code is not valid"; only a caller with
the secret learns that it expired or was used.

**Pairing.** `needy client pair create [--ttl 10m] [--name NAME]` (an
operator, or a client) creates a single-use code; the name is a DNS label,
`client-<code ID>` by default, and no client that is not revoked and no open
code may hold it, so `client:NAME` in a trace names one client. `needy
client pair --endpoint URL CODE` calls `ClientService.Pair` without a bearer
token (the code authenticates it), and stores the returned token on the
client. Ten minutes is long enough to carry a code to another machine by
hand and short enough that an unused one rarely matters, so pairing codes
are not listed or revoked (`TODO(clients)` in `control.rs`).

**Client side.** The CLI keeps one token per endpoint in
`credentials.json` in its config directory (`$XDG_CONFIG_HOME/needy`, or
`NEEDY_CONFIG_DIR`; the `TODO(config)` in `paths.rs` now covers only the
build cache): `{version: 1, endpoints: {ENDPOINT: {client_id, name, token,
paired_at_ms}}}`, keyed by the endpoint as given without a trailing `/`,
written 0600 in a 0700 directory through a temporary file renamed over the
old one. Every command presents `NEEDY_TOKEN` when set (an operator with the
control token), else the stored token for its endpoint, else none. A file
of another version is an error, not overwritten. An endpoint reached under
two spellings (`localhost` and `127.0.0.1`) needs two pairings; resolving
them would guess at what a tunnel forwards.

**Authentication.** `Caller::Cli` is renamed `Caller::Operator`, and a new
`Caller::Client { id, name }` comes from a client token, which
`Clients::credential` resolves. A client has an operator's rights except
the runner methods (`PERMISSION_DENIED`); its token works on a listener with
or without a control token, so a client is named even on the control
socket. A token shaped like a client token that no client holds (revoked or
never issued) is `UNAUTHENTICATED` everywhere. The control token remains the
bootstrap and admin credential; nothing else changed for it, and no other
compatibility path was added. Every adapter now reads the request's caller
(set by the middleware) instead of its listener's. `last_used_ms` is written
at most once a minute, so authenticating a request is a read.

**Principals in the trace.** `needy_core::types::Principal` is `operator`,
`client:NAME`, `runner:NAME` or `actor:RUN`, one string in a payload. The
event schema is version 2: `run.create`, `run.stop` and `exec.start` carry
`by` (absent when the control plane acts on its own, such as stopping runs
at shutdown, and for its own copy-in and mirror-out), and a new `exec.kill
{process, executor, signal, by}` records a signal sent to a run's process
through the control plane (`needy kill`). The trace crate still knows no
event types: the principal lives in core's payloads, not in the envelope.
`Event::decode` reads versions 1 and 2 (`READ_VERSIONS`): a version 1
payload is a version 2 payload without `by`, so traces written before and
events an older guest build pushes still decode, with no principal. The
adapters set who asks (`Caller::principal`) on `NewRun`, `ExecSpec`,
`Runs::stop` and `Processes::kill`. `needy fs cp` is an exec, so its
`exec.start` names the client; routed commands name `actor:RUN`.
`runner:NAME` has no event yet: runners reach only their own streams. The
report shows `Started by` and `Stopped by` (JSON `started_by`,
`stopped_by`).

**Report and logs through the control plane.** `RunService.GetRunEvents`
streams a run's stored events (an ID, a prefix or `latest`; the daemon
flushes a live run's trace first) and `needy report` renders them on the
client; `needy report --local` keeps the offline path that reads this
machine's `trace.db`. `GetRunLogsRequest.follow` keeps the console stream
open: the daemon reads the console file again every 250 ms (polling, see
the `NOTICE` at `FOLLOW_POLL`) until the run is no longer `running` or the
caller leaves, and `needy run logs -f` prints each chunk as it arrives. A
follow of a run that is not running returns what the console holds.

**Tests.** The store (codes: TTL, single use, names, revocation, hashes
only, `last_used` resolution); the adapters over the fake (pairing needs no
token, a client token reaches operator methods and is named, a revoked one
is refused, a client token is refused the runner methods, events in
messages of 1000, followed logs); the daemon in-process (pair, use, revoke,
refused; `run.create`, `exec.start`, `exec.kill` and `run.stop` name the
client and the operator in the events read back through `GetRunEvents`;
followed logs end with the run); the CLI against a control daemon on a
loopback TCP port with a control token, from a second home whose state
directory holds no `trace.db`: pair, the token file 0600, a spent code,
`run create`, `exec`, `run logs -f` until `run stop`, `report` human and
JSON naming `client:laptop`, `--local` failing, `client ls`, `client
revoke`, refused.

## Open questions

- **The control plane as a runner**: is the control host implicitly a local
  runner, or does it join like any other? As built in slice 5, both: an
  actor or executor without `runner:` runs on the control host directly,
  and a runner that joined from the control host is named its own with
  `--local-runner`, an operator's statement the control plane does not
  verify. Detecting it (a machine identity in the host report) would remove
  the flag.
- **Several runners that fit a label selector**: slice 5 refuses the
  ambiguity. Should a run pick one (least loaded, first by name), and how
  does a replay then land on the same machine?
- **Actors and Docker executors on other runners**: both need tasks beyond
  exec, list and kill on the control stream (`TODO(runner-actor)`,
  `TODO(runner-docker)`). Which comes first?
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
- **One connection per runner**: heartbeats stay unary beside the control
  stream (as built). Should they move onto it, so that `online` means "a task
  can reach it now"?
- **Graceful restarts**: a control plane stopped by a termination signal
  still stops every run, and one killed fails them at the next start. With
  actors that cannot be adopted (slice 6) both end the run; a mode that
  keeps runs through an upgrade would need the actor's guest channel and
  its run's state to survive the daemon.
- **Processes outside runs**: the in-memory map of processes the daemon
  has seen (for `attach`, `kill` and `ps`) still grows until it exits, and
  processes outside runs are not in `state.db`. Prune, persist, or both?
- **Client scopes**: a client token has every operator right (owner's
  slice 7 scope). Read-only clients (reports and logs only), or clients
  limited to their own runs, would need a scope on the pairing code, as join
  tokens have.
- **Client token rotation**: a client re-pairs to get a new token; the old
  one stays valid until revoked. Should `client pair` revoke the token it
  replaces on the same endpoint?
- **Pruning clients**: revoked clients and used or expired pairing codes stay
  in `state.db` and `client ls`, as removed runners do.
- **Silent processes after a lost client**: an exec whose client left, on a
  runner as on any executor, ends only when the process next writes. Should
  the executor end a non-detached process as soon as its stream ends?
