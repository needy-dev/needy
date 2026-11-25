# Terms and Concepts

The working vocabulary for Needy. Design notes, code names, proto fields and
CLI output use these terms. Terms marked **provisional** are design terms that
may still change; do not stabilize them in public APIs yet.

Entries labeled **approved contract** describe required behavior from the
[client execution and actor lifecycle spec](ai/references/architecture/2026-11-15-client-execution-and-actor-lifecycle-spec.md).
Their implementation and acceptance remain pending; they are not support claims.

Background: [actor and executor separation](ai/references/architecture/2026-11-09-actor-executor-separation-design.md).

## Roles

### Actor

The side that runs the driving program: one Firecracker microVM, one
container on a host without KVM, or one Virtualization.framework VM on an
Apple silicon Mac, built from a recipe. The program's view of the world, including the recipe identity,
lives here. Actors are stateless: a read-only root and a throwaway writable
layer.

Stateless describes where durable data belongs; it does not mean a live
program has no memory or terminal state. Rebuilding an actor after its
environment is lost is different from reconnecting to the original process.

*Avoid*: harness layer, caller, shallow layer.

### Actor driver

What runs an actor next to the daemon, named by the profile's required
`actor.driver`. A driver is named after the operating system whose isolation
it relies on: `linux-firecracker` (a microVM; needs `/dev/kvm`),
`linux-container` (an OCI container through a Docker-compatible **engine**,
with the required `actor.container.oci_runtime`: `runc`, or gVisor's
`runsc`), `macos-vz` (needy's arm64 kernel and rootfs in a
Virtualization.framework VM that vfkit runs; Apple silicon macOS only) or
`macos-apple-container` (a container that Apple's `container` runs in its own
lightweight VM; macOS on Apple silicon). The driver starts, waits for and
stops the actor, names the channels the daemon serves (guest API, gateway)
and dials (terminal), and says who sets the actor up: a VM's init inside, the
`linux-container` driver from the host (workspace mounts, hostname, identity
files), or both (`macos-apple-container`: init mounts the workspace and sets
the hostname). The report names it (`Actor  linux-container (runsc)`), since
it decides the isolation. See the
[drivers design](ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md),
the [container actor design](ai/references/architecture/2026-11-10-container-actor-runtime-design.md)
and the [macos-vz actor evidence](ai/references/architecture/2026-11-12-macos-vz-actor-evidence.md).

What a driver presents and how it is reached are its **facets**: its
capture (the kernel's `binfmt_needy` or shims), who mounts the workspace
(the guest or the host), who presents the hostname, the sensors and the
uptime offset (the guest, the host, or nobody: a gap), and its transport
(vsock or Unix sockets). **Provisional**.

*Avoid*: actor runtime (the name before 2026-11-12); "driver" alone for the
driving program.

### Runtime holder

The protected local process that owns actor driver handles and native executor
processes independently of control management. A control restart reconnects to
its stable identity and saved bindings. A PID alone does not prove ownership.
Viewer loss and control loss leave held work alive. Explicit run stop ends it.
See the [implementation evidence and limits](ai/references/architecture/2026-11-15-actor-holder-implementation-evidence.md).

### Executor driver

What creates and destroys executor instances, named by a profile's
`executors.NAME.driver`: `native` (the runner's own process space; no OS
facility, no prefix) or `linux-container` (a container through the same
Docker-compatible engine as the actor driver of that name, configured by the
executor's `container:` options).

*Avoid*: executor provider (the name before 2026-11-12).

### Engine

The Docker-compatible container engine a `linux-container` driver talks to,
wherever `DOCKER_HOST` points: Docker Engine, OrbStack, Podman or containerd
through its Docker-compatible tooling, treated alike. needy has no
per-engine code, and one client (`core_executor::engine`) that both
`linux-container` drivers use.

### Driving program

The program inside an actor that drives the work: an agent harness, a test
suite or an acceptance script. Needy never routes the driving program itself;
only the commands it starts.

With `run create -t`, the driving program has a terminal; `-it` also
attaches immediately and exits with the driving program's exit status.
Arguments after `--` are appended to the profile's `actor.command`.
`run attach [RUN]` reconnects to that terminal (without `RUN`, to
`NEEDY_RUN` or the only live run), and detaching leaves the run alive.
Init keeps its output in the actor's console
while no client is attached. See the [automated and live-probe evidence](ai/references/cli/2026-11-11-interactive-actor-evidence.md).

### Harness

One kind of driving program: an agent loop such as opencode, Codex or an
agent-written replacement.

### Executor

An environment where routed operations take effect: a container, a microVM, a
pod or a real machine such as a Mac. Executors hold state (the workspace).

*Avoid*: worker, needy client, target. A *runner* is the machine an executor
runs on, not the executor (see Runner).

### Executor instance

One running executor, created from a profile's executor entry by its
**executor driver** on the runner its selector names: `native` (the runner's
own process space) or `linux-container` (a container). Commands whose instance key is
equal share one instance.

### Instance key

A template such as `{run}`, `{group}`, `{profile}` or `{rule}` that decides
which commands share an executor instance. Built only from deterministic
inputs, so a run can be replayed.

### Daemon

A `needy daemon` process. In **control** mode it is the control plane:
lifecycle of actors and executor instances, routing, the actor gateway, the
trace and the runner registry. In **executor** mode it only runs processes on
its own machine, as inside a `linux-container` executor's container. In **runner** mode it joins a control plane as a runner,
sends it heartbeats, and runs the processes the control plane asks for over
the connection it dialed; it listens nowhere.

*Avoid*: server.

### Runner

A machine running `needy daemon --mode runner` that has joined a control
plane. It reports its **capabilities** and **labels** (`KEY=VALUE`), and sends a heartbeat at the interval the control
plane sets (15 s by default); it is *online* while one arrived within the last
three intervals, else *offline*, and *removed* once an operator removed it.
Runners are registered (`needy runner token create`, then a join), listed
(`needy runner ls`), rotated (`needy runner rotate`) and removed
(`needy runner rm`). Actors and executors are instances that run on them; a
runner's name is a DNS label. Designed in
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md).

A runner dials the control plane and keeps a **control stream** open on which
the control plane announces **tasks**; for an exec the runner opens a **task
stream** that carries the exec. `needy exec --runner NAME` runs a command on a
connected runner's machine. Each heartbeat reports the runner's **host**: its
platform, CPU and the CPUID entries its KVM supports. Profiles place executors
and actors on runners with **runner selectors**.

*Avoid*: agent, worker, node.

### Capabilities

What a runner can host: each **role** it holds (`actor`, `executor`) with its
**actor drivers** (`linux-firecracker`, `linux-container`, `macos-vz`,
`macos-apple-container`) or **executor drivers** (`native`,
`linux-container`), written
`actor(linux-firecracker) executor(linux-container,native)`. A runner detects
them when it starts: a KVM device that opens for reading and writing and a
complete actor image give `actor(linux-firecracker)` (Linux only); a
Docker-compatible engine that answers gives `executor(linux-container)`, and
on Linux `actor(linux-container)` (elsewhere the engine runs containers in a
VM, which cannot reach the run's channel sockets)
(whether it has the profile's OCI runtime and the container actor image is
checked when a run is created); macOS on Apple silicon with vfkit and a
complete `macos-vz` actor image gives `actor(macos-vz)`; Apple's `container`
CLI whose services answer gives `actor(macos-apple-container)` (macOS only);
`executor(native)` is always there. `--roles` restricts a runner to some roles and cannot add one the
machine lacks. Capabilities travel in the join and every heartbeat, are kept
in `state.db`, and are what scheduling and `profile check` match: an actor
needs `actor(DRIVER)` of its driver, an executor `executor(DRIVER)`. A join token's
scope still names roles.

### Ownership and grace period

A runner **owns** the processes it runs for a run: each carries the run's
ID. When a run ends, the control plane tells every runner it held an
executor on to **end the run**, and the runner kills the run's processes,
a workspace mount's SFTP server among them. A runner that cannot reach the
control plane keeps them for its **grace period** (2 minutes), then ends
them itself; processes outside any run (`needy exec --runner`) are never
ended this way.

### Reconciliation

What a control plane does with what it finds. The following describes the
current implementation, before the approved restart-survival change. When a
runner connects it
**reports** the processes of runs it still runs; the control plane
**claims** those of live runs that hold an executor on it and tells it to
end the others. When a control plane starts, it loads open invocations from `state.db`.
It adopts the original actor only after the protected runtime holder proves
its identity. It reconnects saved executor handles without replacing work.
If ownership cannot be proved, it records failure or lost instances and
cleans up verified leftovers. See the
[implementation evidence and limits](ai/references/architecture/2026-11-15-actor-holder-implementation-evidence.md).

The **approved contract** instead requires recovering management of the same
surviving actor and driving program, using stable identity and protected
holder access. A PID alone is insufficient. Actual runtime loss can still
end work; replacement actors are new invocations. See the
[lifecycle design](ai/references/architecture/2026-11-15-actor-survival-and-history-design.md).

### Lost instance

An instance of a run that nobody reported after the control plane
restarted: no runner reported it within the wait (60 s), or it was one of
a native executor whose protected holder identity cannot be recovered. Whether it still runs is unknown; `run ls` marks it
`(lost)`. The runner's own grace period still ends its processes.

### Runner selector

A profile's `runner:` on an executor or the actor: a runner's name, or
`{ labels: [KEY, KEY=VALUE] }` for the one connected runner with the capability
the instance needs and every label. Without one, the actor or executor runs on the control host
itself. The control plane resolves selectors when a run is created
(**scheduling**); a selector that names no connected runner, or labels that
fit several, is an error naming the candidates.

An **approved contract** adds explicit client execution alongside these
selectors. An omitted selector retains its control-host meaning; a registered
runner name must not be guessed to represent the creating client.

### Client executor (approved contract)

A logical executor explicitly bound to the execution capability supplied by
the creating invocation. Its holder inherits the invoking OS account. Attach
from another client or a connection-context change does not replace that
binding. Missing capability fails without falling back to the daemon, a
privileged runner or another client. One invocation can bind several logical
executors to its holder while keeping existing instance and pool policy.

### Execution holder (approved contract)

The protected local owner of work and its required process, terminal, driver
and per-run service channels across viewer loss or control restart. Client
execution retains its same-account holder automatically, without manual
runner enrollment or `runner serve`. Work lifetime determines holder lifetime;
actual holder loss is distinct from losing a viewer. It does not become an
unrestricted shared runner resource.

### Local runner

The runner that runs on the control host, named by `needy daemon --mode
control --local-runner NAME`. Actors and `linux-container` executors placed
on it are created by the control plane itself; as built, actors and
`linux-container` executors run nowhere else.

### Control stream and task stream

A runner's **control stream** is the one long-lived stream it opens to the
control plane (`RunnerService.Control`) while it runs; it connects again with
backoff when the stream drops. The control plane announces each **task** on
it with an ID: listing or killing processes, answered on the control stream,
or an exec, for which the runner opens a **task stream**
(`RunnerService.Task`) that carries the exec's own messages. A runner whose
control stream is not open is *not connected*; tasks for it fail at once.

### Join token

A short-lived secret an operator creates with `needy runner invite` and
hands to one machine so it can join as a runner. It has a time to live, a
number of uses (1 by default), the roles it allows (every role when it names
none) and labels it gives every runner that joins with it. The control plane
stores only its hash; it is shown once. A token that expired, is used up or
was revoked is refused.

New invitations use `<id>.<secret>` without a product/type prefix. The secret
contains 32 random bytes encoded as 64 hexadecimal digits. Already-issued
v0.0.4 invitations with the `needy-join.` prefix remain readable until expiry.
This format change does not change stored runner credentials.
`runner join` enrolls the machine and saves its credential; `runner serve`
opens the persistent connection used for work.

### Runner credential

The secret a runner gets when it joins, its bearer token for heartbeats from
then on; the join token is not needed again. The runner keeps it in
`runner.json` in its state directory (mode 0600); the control plane stores
only its hash. `needy runner rotate` hands the runner a new one at its next
heartbeat, and the old one stops working once the runner uses the new one;
`needy runner rm` revokes it.

### Control token

NOTICE: this section describes current behavior. The proposed
[access redesign](ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md)
replaces the manually supplied shared control token in normal use with local
OS admission and individually revocable remote client credentials. Existing
paired clients would retain their granted permissions during migration.

The bearer token a control daemon reads from `--token-env`: the bootstrap and
admin credential, held by the **operator**. It is required for a TCP listener
on a non-loopback address; the control socket on the control host needs none
(its file permissions guard it). An operator uses it to pair the first clients
and runners; day-to-day clients use their own **client tokens**.

NOTICE: the current server applies its configured control token across its
listeners, including Unix sockets. The token-free local-socket statement above
describes a server without that token configured. Separating local and network
admission is proposed below; it has not landed.

### Local discovery (proposed)

A control daemon publishes its actual local management endpoint and instance
identity in a protected descriptor. The CLI resolves that record to reach the
local operator entrypoint automatically. Discovery locates a daemon; OS access
to its management socket or a verified credential grants authority. Actor and
runner channels retain their existing identities. This is a proposed contract,
not current behavior; see the
[local discovery design](ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md).

### Client

A program that calls the control plane on behalf of a person or a tool, such
as the CLI on another machine or a script. A **paired** client holds its own
**client token**, is listed with `needy client ls`, and can be revoked with
`needy client revoke ID`. The trace names it `client:NAME`. The `runs` client
role permits run and process operations; `admin` also permits client and
runner administration. This authority is separate from a local OS account
or a creator-only ACL.

A **static** client is declared with its token (or the token's SHA-256) in
the daemon's `--clients-file` instead of pairing, so it exists before the
daemon starts. Its ID is `static-NAME`; it is listed with paired clients,
cannot be revoked through the API (the file is its only source), and holds
its name against paired clients while the file declares it.

The CLI stores saved connection contexts, their server-issued identities,
and credentials in its private `config.yaml` document. A context contains an
address or connector argv and the credential used with that transport.
`needy config context pair` exchanges a one-time invitation for a credential;
it does not change the current selection. `needy config use NAME` saves the
default context, and `--context NAME` selects one for an invocation.
`--address URI` selects a direct transport; an optional private
`--credential-file FILE` supplies its outgoing bearer. Legacy endpoint/token
environment selectors do not select a connection. The control plane stores
only the token hash. An active client name is a unique DNS label. See the
[configuration and access specification](ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md)
and [authority evidence](ai/references/cli/2026-11-15-configuration-authority-and-lifecycle-discussion.md#protocol-identity-and-os-account).

Authenticated clients retain existing role grants and daemon-wide visibility
under the **approved contract**. This slice adds no human-account model,
creator ACL or fine-grained RBAC; creator audit facts do not establish
creator isolation. Protocol identity remains distinct from the local OS
account used by an execution holder.

*Avoid*: user, account. Needy has no users; a client is a credential with a
name.

### Client role and responding service

A **client role** is the authority granted to a paired client: `runs` permits
run and process operations, and `admin` also permits client and runner
administration. These are the existing grants, not a new access system.
A local context records the server-issued grant; its name does not confer
authority. Runner roles (`actor`, `executor`) describe hosting capabilities
and are separate from client roles.

A **responding service** is the API endpoint inspected by a connection check:
`control` or `executor`. Its service instance identifies one service lifetime.
The endpoint being reachable does not by itself establish the caller's client
role. An unavailable identity inspection leaves the client role unknown;
`unknown` is an observation limit, not a third grant.

The [CLI output design](ai/references/cli/2026-11-15-cli-output-audit.md#needy-config-context-check)
uses human headings `SERVICE` and `CLIENT ROLE` to separate these concepts.
This presentation does not rename JSON `role` and `access`, persisted
configuration fields, or Protobuf fields. A responding API service is also
separate from an actor network [service](#service).

### Connection context (proposed)

A named, persistent control-plane connection: its address or stdio connector
and the credential used with it. Multiple contexts may name different control
planes or different identities on the same control plane. An explicitly
selected current context is the default for new commands; pairing does not
change that choice. Proposed `needy config context pair` and `needy config use`
commands obtain a credential and select a connection, while `client invite` authorizes a client
on the server. Its authority is recorded by the server, not self-declared in
the context. Proposed `needy config context` commands manage these records;
they are distinct from task profiles and execution contexts. Under the
**approved contract**, changing a connection context never changes an
existing execution binding, OS account or workspace location. See the
[context design](ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md).

### Execution context (proposed)

Non-secret connection-context and run references inherited by a needy-launched
process, so it can resolve the intended control plane and run without manual
exports of independent settings. Credentials are resolved separately and the
context does not grant authority. This is proposed, not the existing CLI's
internal `Context` type or a task profile; see the
[discovery and context design](ai/references/cli/2026-11-14-cli-configuration-and-access-spec.md).

### Pairing code

A short-lived (10 minutes by default), single-use secret an operator creates
(`needy pair create [--name NAME]`) and hands to a new client, which
exchanges it for its client token once (`needy pair --endpoint URL CODE`).
Shown once; stored hashed.

### Principal

Who caused a trace event: `operator` (the control token or the control
socket), `client:NAME` (a paired client's token), `runner:NAME` (a runner's
credential) or `actor:RUN` (an actor, through its own guest channel).
`run.create`, `run.stop`, `exec.start` and `exec.kill` record it as `by`;
what the control plane does on its own, such as stopping runs at shutdown,
records none. `needy report` shows who started and stopped a run.

## Runs and configuration

### Run

One execution of a driving program in an actor, with the executor instances
bound to it, started from a profile, and its trace and report. One RL rollout
is one run. Every run has an actor; `needy exec --runner` runs commands on a
runner's machine without a run, and a run whose actor only waits serves
`needy exec --run` (see
[Runners and runs](ai/references/architecture/2026-11-10-runner-and-run-design.md)).

One run records one actor invocation, not each child exec or each attached
client connection. A daemon can manage several independent actor invocations
at once. A terminal attachment can end while its actor continues. The
**approved contract** requires viewer loss and control restart to preserve
surviving work, with no automatic rerun of uncertain commands. Current
reconciliation does not implement that adoption behavior.

Current state transitions: a run is `creating` while it acquires its
executors, copies files in and starts its actor, then `running`. It ends `stopped` (after `stopping`, by
`needy run stop`), `finished` (its actor's driving program exited, once the actor ended too) or
`failed` (its create failed, its actor's VM exited before the driving
program did, or its original actor or runtime holder was lost, with the
reason). The first of these end states stays: stopping a
finished or failed run releases its executors but keeps its state. A
**graceful stop** (the default of `needy run stop`) ends a running driving
program with SIGTERM, then SIGKILL after a grace period, and waits for the
actor to mirror out and end; `--now`, an actor whose guest does not wait
for a stop, or one that does not end in time is killed without its
mirror-out (**forced**). `run.stop` records which. The control plane writes every change of a run's state, and of the instances
and processes it holds, to `state.db`.

A command names a run by its ID, an ID prefix, its name or `latest` (the
run created last), through `--run` or `RUN`, else `NEEDY_RUN`. A **live**
run is `creating` or `running`; `run attach`, `run stop` and `run logs`
without a run take the only live one. `exec`, `ps` and `fs cp` without a
run stay outside any run.

A live run's **actor state** is `starting` (its driver starts it, its guest
boots), `configured` (its guest fetched the run's configuration), `ready`
(its process service answered a request, so `run attach` works) and
`exited`; `run ls` shows it in the `ACTOR` column, and `-` for a run that
ended. Ready means the same for every driver: a socket that accepts is not
enough. Only a driving program on a terminal has a process service, so an
actor without one goes from `configured` to `exited`. The trace records
`actor.start`, `actor.config`, `actor.ready` and `actor.vm.exit`.

*Avoid*: session (the earlier name), rollout.

### History (provisional)

Durable records of actor invocations and their execs: what was requested and
what result was observed. They remain useful after the live environment is
gone. History lookup, terminal replay and starting again do not by themselves
restore a process. Keep existing trace/report storage under the approved
contract.

NOTICE: history formats, synchronization and additional secret collection are
not part of this slice; the owner deferred their redesign until a separately
approved contract.

### Lifecycle

Every long-lived object has one: an enum of states, a table of the
transitions it allows, and one owner that advances it and refuses the rest.
The first end state reached stays.

| Object | States | Owner |
| --- | --- | --- |
| Run | `creating`, `running`, `stopping`; `stopped`, `finished`, `failed` | the daemon's `run::state` |
| Actor | `starting`, `configured`, `ready`; `exited` | the daemon's `actor::state` |
| Instance (a run's hold on one) | `running`; `released`, `ended`, `lost` | `state.db` (`store::lifecycle`) |
| Process | `running`; `exited`, `ended`, `lost` | `state.db` (`store::lifecycle`) |
| Runner | `joined`; `removed` (shown `online` or `offline` while joined) | `state.db` (`store::lifecycle`) |

A runner's and a join token's shown states (`online`, `offline`; `valid`,
`expired`, `used up`, `revoked`) are derived from their rows and a clock,
not stored. See the
[drivers design](ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md#lifecycles).

### Group

A name shared by several runs so that executors keyed by `{group}` serve
all of them, for example a parent agent and its sub-agents. **Provisional.**

### Profile

A declarative YAML description of one run: which recipes to use, executors,
command rules, file mappings, both networks and chaos. `--profile NAME`
(no path separator, no `.yaml`) is `profiles/NAME.yaml` in the CLI's config
directory.

The invoking client reads and resolves the profile and submits its contents.
A run keeps the submitted configuration; editing the source file affects a
later run, not the existing one. A profile describes requested behavior and
resources; it is not itself a grant to use a host account or executor.

The **approved contract** adds `run create -f PATH` as a profile-input alias
and temporary actor image, workspace, environment-entry and entry-command
overrides. They change only the submitted snapshot, never the source file.
Trailing `--` arguments retain their append meaning. Validate the final
profile before remote side effects.

*Avoid*: config.

### Preset

A named, reusable block of rule actions inside a profile, applied with `use:`.

### Recipe

A declarative YAML description of one machine: kernel, CPU, memory,
firmware, devices and network identity. The software the machine runs is
its actor image's, not the recipe's: `os.image` is removed and refused. `needy image build --recipe` builds
an actor kernel with its kernel options. Built-in
recipes are referenced as `builtin:NAME`.

*Avoid*: persona.

## Images

See the [actor images design](ai/references/architecture/2026-11-12-actor-images-design.md).

### Base image

An actor image without needy's own program in it: the kernel with
`binfmt_needy` (VM drivers), the root file system, the boot stub and the
shim stub. One base image serves every needy version that speaks its base
ABI. Every release publishes `ghcr.io/needy-dev/needy/actor-base`
(`<version>-ubuntu-24.04`), the start of every actor image for the VM and
container drivers: the distribution's own layers, unchanged and pinned by
digest, then the userland (packages and the `agent` user) and `/.needy`
(the stubs and the base ABI label), with no container shims. The base images
the drivers use are derived from it: a VM image archive per VM driver, whose
`rootfs.ext4` is `actor-base` with the VM's own additions (systemd) and
whose `manifest.json` records the base (its name, DiffIDs and, when
published, digest), which `needy image pull` downloads into
`images/<driver>/base` in needy's data directory; and
`ghcr.io/needy-dev/needy-actor:v<version>`, `actor-base` with one layer that
shims the shells, which the container drivers pull. The executor's image is
published the same way as `ghcr.io/needy-dev/needy/executor-base` (and
`needy-executor`). `needy image build` builds one from a needy checkout (a
VM image directory, with `needy-actor-base:latest` as its base, or
`needy-actor:latest`). The daemon brings the guest needy when each actor
starts. See the
[layered OCI base spec](ai/references/architecture/2026-11-25-layered-oci-base-spec.md).

*Avoid*: actor image with needy, versioned image.

### Actor image

What a profile's `actor.image` names for every driver: an OCI image
reference, or an [image definition](#image-definition-provisional). The
container drivers run that image; the VM drivers boot the base VM image
with the image's layers above `actor-base` attached as layer disks, after
an [import](#image-import). Without `actor.image`, the container drivers run the release's
base image `ghcr.io/needy-dev/needy-actor:v<version>` (pulled when missing)
and the VM drivers the base VM image (`needy image pull`; never downloaded
by the daemon itself).

### Image definition (provisional)

The software an actor runs on, written instead of a Dockerfile: a `root`
(an OCI image reference), an ordered list of **layers** built over it, and
`env`. `actor.image` takes one inline (a map), or from an **image file** by
path (a string that starts with `./`, `../` or `/`, or ends in `.yaml`,
which must exist); any other string is an image reference. An image file
is its own document kind with `extends`: along `extends`, `layers` append
(the parent's first), and a child `feature` layer replaces a parent's of
the same reference, ignoring the tag, in place. needy ships no built-in
images. A layer is of kind `run` (a script run as root); `feature` layers
and hooks come with later slices. needy loads and checks definitions but
does not build them yet, so a run of one is refused. See the
[images and run data spec](ai/references/architecture/2026-11-17-images-and-run-data-spec.md).

### Build plan (provisional)

What building an image definition takes, worked out without IO from the
definition, the root's digest and the profile's user (`needy-core`'s
`image::plan`): the profile's user created in the root, one **step** per
layer in order, the image's commit layer on top, and the image's `env` and
`PATH` (`${PATH}` in `env` expands to the system `PATH`). Each step has a
**cache key**, the digest of its parent's key and its input (a `run`
script); the first parent is the root's digest with the user (name, UID,
GID), so another root or user is another image, a changed layer changes
its key and every key above it, and a layer built under a key is reused.
Nothing builds a plan yet.

### Custom actor image

An OCI image `FROM` a base image that adds programs (an agent CLI, node,
jq). A container actor routes the programs it added through trampolines;
its last step, `RUN ["/.needy/shim-image"]`, keeps the shells shimmed in
place after a package added or upgraded one. It inherits the base's
`needy.base-abi` label. For the VM drivers it is built `FROM`
`actor-base` without the shim step and pushed to a registry, and a run
[imports](#image-import) it.

*Avoid*: agent image, user image.

### Image import

What a VM actor of an OCI image (`actor.image: REF`) does instead of
booting a VM image of its own: needy reads the image's manifest for the
actor's platform and its config, checks that it records this needy's base
ABI and that its first layers are exactly the `actor-base` the base VM
image records (by DiffID), and pulls only the layers above the base into
the layer store, which are attached as layer disks as an image
definition's steps are. A layer that writes, removes or hides `/.needy` is
refused, and so is an image `FROM needy-actor`. The control daemon imports
for remote actors too. `needy image build --from REF` imports ahead of a
run. See the [agent images guide](guides/agent-images.md) and B4 of the
[layered OCI base spec](ai/references/architecture/2026-11-25-layered-oci-base-spec.md).

*Avoid*: VM image of an OCI image (what `--from` made before the import).

### Guest needy

The `needy` binary that runs inside an actor as its init, proxy and shim
handler: a static Linux build of the daemon's own version for the actor's
architecture. The daemon injects it at `/.needy/needy`: a read-only bind
mount of one file in a container, a small read-only SquashFS disk (the
**guest disk**, `/dev/vdb`) in a VM. It is `--guest-needy PATH`, else
`needy-linux-<arch>` next to the daemon's executable, else on Linux the
daemon's own executable when that is static.

### Boot stub

`/.needy/init` in a base image, the actor's PID 1: a tiny static C program.
In a VM it mounts the guest disk and binds the guest needy over
`/.needy/needy`; then it runs `/.needy/needy init`.

### Shim stub

`/.needy/proxy` in a base image, a tiny static C program: the program the
kernel's `binfmt_needy` hands a routed exec to, and in a container actor
the file every shim is a hard link to (an in-place shim to it, a trampoline
to its copy on a tmpfs). It runs `/.needy/needy` with
the caller's argv, its own path (`NEEDY_STUB_EXE`) and the exec's path
(`NEEDY_STUB_EXECFN`). While no guest needy is injected (an image build), a
shim runs its real program instead, unrouted.

### Base ABI

The interface between a base image and the guest needy and daemon: the
paths under `/.needy`, the stubs' hand-off, the guest disk, and
`binfmt_needy`'s parameters and never-route rule. It has a version: a VM
image records it in its manifest, a container image in its
`needy.base-abi` label, and a driver refuses an image whose version this
needy does not speak before the actor starts.

### VM image manifest

`manifest.json` in a VM image's directory (`images/<driver>/<name>/` in
needy's data directory): the needy version that made it, its driver,
architecture and base ABI, where it came from (a release, an imported
archive, a build from a checkout, or, for a VM image an older needy made,
`--from` an OCI image with its image ID) and the SHA-256 of the archive it
was unpacked from. The VM drivers read
its base ABI; `needy image ls` shows the rest. A directory without one is
not an image: `needy image pull` or `needy image build` writes one.

### Layer store (provisional)

Where the daemon keeps an image's layers
(`needy-core-actor`'s `image::store`; see the
[images and run data spec](ai/references/architecture/2026-11-17-images-and-run-data-spec.md#the-layer-store-new-deep-module)).
A layer is kept in one form, an OCI layer archive, and named by its
digest. A **snapshot** is a stack of layers named by the digest of the
stack. An image's **commits** (hand edits) collapse into one commit layer
with a history to roll back through. Built layers, snapshots and layer
disks are cache; commit layers and their history are data. Nothing calls
it yet (slice 1 of the spec wires it).

### Layer registry (provisional)

An OCI Distribution repository that hosts share built image steps through
(`needy daemon serve --layer-registry REPOSITORY`, `daemon.layer_registry`;
`needy-core-actor`'s `image::store::registry`; see the
[layered OCI base spec](ai/references/architecture/2026-11-25-layered-oci-base-spec.md#slices),
"Settled in B-3"). Each built step is a **step image**: an ordinary OCI
image tagged `key-<cache key hex>` with the step's layer only (gzip), a
standard image config naming its DiffID, and needy's metadata (cache key,
parent layer, root, feature) in `dev.needy.image.*` manifest annotations. A
daemon looks a missing step up there before building it and pushes what it
builds; a failure is traced and never fails the build.

### Registry access

How every needy OCI client reaches a registry (`oci-registry`'s `Access`):
credentials from the Docker credential store (an `identitytoken` is
exchanged for an access token first), the system trust store plus the
host's CA files in containers-style `certs.d`, and per-host **insecure
registries** (`--insecure-registry HOST`: TLS certificates not verified).
Plain HTTP is a separate, explicit choice (`http://`) and never carries
credentials. Each process resolves its own credentials.

### Layer disk (provisional)

A layer as a read-only SquashFS disk a VM driver attaches after the guest
disk: the layer's tree under `root/`, and its digest in a marker file,
`needy-layer`, by which the guest needy finds it whatever the attach order.
Deletions are overlayfs whiteouts; an opaque directory is written as
whiteouts of what it hides below, since the disk carries no extended
attributes.

### Session (provisional)

An internal primitive: an actor booted from a snapshot with three settings,
its **upper layer** (disposable, or kept and streamed to the daemon when the
session ends), **routing** (on or off) and its **ending** (the daemon
commits a kept upper layer or discards it). A run, a build step and an edit
session are its uses; users see only runs, `image build` and `image edit`.
The guest configuration carries the upper layer and routing; the ending is
the daemon's. Not the earlier name of a run (see Run).

## Routing

### Mesh

The application graph of actors, executors and accepted execution bindings,
coordinated by Needy's control role. Under the **approved contract**, this
means application connectivity, not a VPN or direct network connections
between every pair of participants. It adds neither multi-actor runs nor new
instance-sharing semantics. See the
[topology design](ai/references/architecture/2026-11-15-topology-and-daemon-responsibilities-design.md)
for current source observations and implementation limits.

### Placement

Where an operation runs: the actor, or a named executor.

### Decision

The result of evaluating a command against a profile: the placement, and the
rewritten arguments, environment and working directory, or a denial.

### Command rule

A profile entry that matches a command (`match`, `when`) and applies actions
(`to`, `args`, `env`, `deny`, …). Matching rules accumulate in order.

### Effective command

The command a launcher such as `env`, `sudo`, `npx` or `sh -c` really runs,
found by a launcher resolver. Rules match the effective command.

### Route list

The executable names the actor hands to the proxy: its kernel in a VM, its
shims in a container. Execs not on it run in the actor without consulting
the daemon. It holds the command name of every rule, whatever the rule does
(place, deny, or change the environment, arguments, working directory,
timeout or faults), the command name of every `chaos.faults` entry with
`on: exec`, and the shells; a rule or exec fault whose pattern may match any
program, or an exec fault without a pattern, makes it `*`, every exec.

### Shim

In a container actor, a link to the shim stub through which the guest
needy sees an exec: it runs the real program with the caller's argv, or
hands the exec to the proxy when the run routes its name. Two placements
(see the [trampolines design](ai/references/architecture/2026-11-16-container-trampolines-design.md)):

- a **trampoline**, `/run/needy/shims/<name>`, which init makes at each
  start for every program on the system `PATH`, first on the driving
  program's `PATH`; the real program runs at its own path;
- an **in-place shim**, which the image's shim step puts at the program's
  own path for the shells (`NEEDY_SHIM_NAMES`, `sh bash dash zsh` by
  default), so calls by absolute path and `#!` interpreters are caught too;
  the real program is at `/.needy/real/<path>`.

A program run by another absolute path, or created while the actor runs
and run directly, is not routed.

### Trampoline

A container actor's shim on `PATH`; see [Shim](#shim). The term is pixi's,
whose global tools dispatch the same way.

### Proxy

The guest needy's part that replaces a routed exec, reached through the
shim stub at `/.needy/proxy`. It asks the daemon for a decision, and either
runs the real program locally or streams it from an executor. The parent
sees an ordinary child process and exit status.

### Capture

The mechanism that catches an exec inside an actor and hands it to the
proxy: the kernel's `binfmt_needy` (VM drivers) or shims (container
drivers), in `core-actor`'s `guest/capture/`. A capture never decides; what
it cannot see (a program created at run time under shims) is a coverage gap
its driver declares. A driver's capture is one of its facets; `actor.start`
records it, and the report says what shim routing leaves out from it, not
from the driver's name. **Provisional**: see the
[drivers design](ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md).

### Interception domains

Interception has three domains, each with its own hook points, context,
decision and chain of **interceptors** (policy): **exec** (`exec.decide`:
rules, chaos), **net** (`net.request`, `net.connect`: allowlist, chaos) and
**file** (`file.open`: chaos's file faults; the hook point was `exec.output`
before 2026-11-12). One interceptor may serve several domains. Policy never
knows the driver. Each domain lives in `core::intercept::{exec, net, file}`,
which also assembles its chain in one place; the allowlist is
`core::network`. Header injection was removed on 2026-11-12: on HTTPS it
needs a separate MITM infrastructure (`TODO(mitm)`). **Provisional**.

### Adapter

The code that turns what a capture caught into a domain's context, runs the
domain's chain and applies the decision: the proxy's decide call for exec,
the gateway for net, the relay's stream filter for file. **Provisional**.

### Path map

A prefix mapping from actor paths to executor paths, for executors whose
workspace lives elsewhere, such as a Mac. `actor.workspace` adds the entry
for the workspace; `path_map` holds the others. A key is an actor path (`~`
is the profile user's home there); a value is a path on the executor (`~` is
that executor's home).

## Files and networks

### Workspace

The directory the work operates on, set in one place, `actor.workspace`: its
`path` in the actor (the driving program's working directory), `from` (an
executor's directory mounted live, a host directory copied in, or nothing for
a fresh directory in the actor) and `writable`. needy derives the mount and
the executor's path map from it. Without `actor.workspace`, the **default
workspace** is the folder `needy run create` runs in, passed through live at
`~/workspace` when the executor that runs the commands can reach it, else the
create fails and asks for `workspace.from` (a profile without executors keeps
the actor's home).

### File mapping

A named profile entry that moves files: **copy-in** at run start,
**mount** for a live view, **mirror-out** back to the host, or **sync**
(below).

### Sync mapping

A file mapping in `mode: sync` (approved contract,
[images and run data spec](ai/references/architecture/2026-11-17-images-and-run-data-spec.md#sync-mappings)):
its **store** (`from: host:PATH`, a directory on the control host) is copied
into a **per-run copy** in needy's data directory, which the actor uses live
at `to`, and the **seed** (the store as it was copied) is kept. When the run
ends, however it ends, the per-run copy is **merged back** into the store
three-way against the seed, under an exclusive lock on the store, one run at
a time: an entry only the run changed is taken; one both changed follows the
mapping's `on-conflict` choice (`store`, `run` or `lines`), and otherwise the
store keeps its copy, the run's is kept outside the store and `needy report`
names it as a **conflict**. A merge that cannot run stays **pending** and runs
when the daemon starts or before the store is next seeded. Merge drivers are
not implemented yet (`TODO(merge-drivers)`).

### Sidecar

A service needy runs for one run, next to the actor and outside it, that the
actor reaches through one of its channels, and that ends with the run: the
**actor gateway** (the net sidecar) and the **guest API** (configuration,
events, exit, files, and the exec decision the proxy asks for). Not the VM
or container process (the actor driver owns that) and not init (inside the
actor). **Provisional**.

### Actor network and executor network

Two separate networks. The actor network sits near the actor's main dependency
(inference) and reaches the outside only through the **actor gateway**, which
enforces an allowlist (HTTPS is tunnelled as is) and forwards **services**.
The executor network sits near what the work operates on.

**Gateway placement.** The actor gateway runs next to the actor: in the
daemon for an actor on the control host, and in the **actor runner** for a
remote actor whose runner and daemon both support it (S1, see
[the runner gateway design](ai/references/architecture/2026-11-26-runner-gateway-design.md)).
The daemon stays the policy source (it sends the profile, its blocked CIDRs
and the run's creation time on the bridge) and the trace owner (the runner
sends what its gateway records back on the bridge); the actor's egress then
leaves from the runner's host. An older runner proxies the gateway channel
to the daemon's shared gateway listener instead, and the daemon runs the
gateway as before.

### Service

A plain-HTTP name the actor uses for an endpoint outside it:
`actor.network.services.NAME.url`. In the actor it is `http://NAME.needy`;
the gateway forwards each request to the URL, which the host where the
gateway runs (the control host, or a remote actor's runner) resolves, and a `CONNECT NAME.needy:80` (clients that
tunnel plain HTTP through a proxy) to the URL's host and port. The actor
never learns the real address.

### User

The profile's `user: { name, uid, gid }`, the same in the actor and in every
`linux-container` executor: `HOME`, `USER`, `LOGNAME`, `whoami`, `id` and
`getent passwd` agree, because needy writes the user's passwd and group
entries before the driving program starts (a VM's init, or the container
driver from the host). A `native` executor runs as the runner's own user.

## Shaping

### Chaos

Deterministic fault injection: a fault fires from the seed, its name and how
many times it has matched, so a replayed run injects the same faults.
`needy chaos add`, `ls` and `rm` change a live run's faults; the trace
records each change (`chaos.add`, `chaos.remove`) where it happened.

### Fault

One named chaos rule in `chaos.faults`, in exactly one **domain**: `exec` (a
command pattern; exit, signal, hang, delay, stderr), `file` (`open` glob;
errno, delay), `net` (`connect` or `request` host; refuse, reset, hang,
delay, status) or `sensor` (readings, at a time). Its triggers are
`probability`, `nth`, `times`, `after` and `until`; without one it fires on
every match. `fault.injected` and the report name it.

### Identity

What a program can learn about the machine it runs on: OS, kernel, CPU,
firmware, devices, network identity. A recipe defines it.

## Observation

### Trace

Every event recorded during a run, in order: each carries the run, a
sequence number, a time, a kind such as `exec.decide`, a payload and the
payload's schema version (9 since `fault.injected` names its fault and
domain and `chaos.add` and `chaos.remove` exist, after 8 recorded the
actor's capture in `actor.start`; traces of other versions are refused).
The profile's `trace:` selects the sinks: SQLite (`trace.db`) and JSONL under
the run directory. Terminal output is not part of the trace.

### Trace gap

A `trace.gap` event: the trace dropped this many events at this point because
its queue was full. A report of a trace with gaps says it is incomplete.

### Report

A fixed-format summary of one run, written by `needy report` as plain text,
markdown or JSON from the run's events, which the control plane sends to any
client it reaches (`--local` reads this machine's `trace.db` instead).

## Processes

### Process

A process an executor started for a client. The executor owns it, so a client
can **detach** and later **attach**, replaying recent output.
