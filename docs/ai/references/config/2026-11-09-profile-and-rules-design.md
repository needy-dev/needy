# Profiles and rules

> **Migration notice (2026-11-13)**: `needy build` is now `needy image
> build`, with the same flags, and a VM image records its base ABI in
> `manifest.json` instead of a `base-abi` file (step 4 of the
> [actor images design](../architecture/2026-11-12-actor-images-design.md)). This note keeps the command it was
> written with.

> **NOTICE (2026-11-12, migration):** the
> [drivers design](../architecture/2026-11-12-drivers-and-extension-points-design.md) renamed what this note describes, without
> compatibility: `actor.runtime` is `actor.driver` (`firecracker` is
> `linux-firecracker`, `container` is `linux-container`); an executor's
> `provider` is its `driver` (`docker` is `linux-container`) and its `docker:`
> options are `container:`; the `ActorRuntime` trait is the actor `Driver`,
> the executor `Provider` is the executor `Driver`; capabilities read
> `actor(linux-firecracker) executor(linux-container,native)`. The text below
> keeps the names of its time, and its links to `examples/container` point to
> what is now `examples/linux-container`. `crates/core-actor/src/host/` is
> gone: its files are in `driver/` (one directory per driver), `contract.rs`,
> `image/`, `identity.rs` and `sidecar/gateway/`; `core-executor`'s
> `provider/` is `driver/` (`docker.rs` is `linux_container/`).

> **NOTICE (2026-11-12, migration):** the
> [schema redesign](#schema-redesign-2026-11-12) changed the profile without
> compatibility: `actor.network.inject` is gone (header injection on HTTPS
> needs a MITM infrastructure, `TODO(mitm)`), `actor.network.upstream` is
> `actor.network.services` (`NAME: { url }`, `http://NAME.needy` in the
> actor); `chaos.faults` is a map of named faults with one domain key each
> (`exec`, `file`, `net`, `sensor`) instead of a list with `on:`/`match:`/
> `set:`, and effects are real; a rule's `faults:` action is gone;
> `actor.cwd` is gone and `actor.workspace` replaces it with the workspace
> mount and its path map; `~` on an executor side is that executor's home.
> The sections below were updated; older examples elsewhere keep the names
> of their time.

> **NOTICE (2026-11-13, migration):** step 2 of the
> [actor images design](../architecture/2026-11-12-actor-images-design.md) moved the
> actor image out of `actor.container`, without compatibility:
> `actor.container.image` is `actor.image`, one OCI reference for every
> driver (the VM drivers boot the VM image `needy build --from` made of it).

**Status**: draft, under discussion; implemented as described unless the [closed-loop results](../architecture/2026-11-09-closed-loop-results-evidence.md#not-yet-verified) list it as missing. The matching syntax and composition rules
below are proposals. Terms follow the
[actor and executor separation](../architecture/2026-11-09-actor-executor-separation-design.md)
note.

## Run-time rules are declarative

A decision must be made at run time only when the syscall cannot continue
without it:

| Decision | At run time? | Why |
| --- | --- | --- |
| Routing: actor or which executor | Yes | The syscall waits for it. |
| Network allowlists, denials | Yes | Afterwards is too late. |
| Chaos: fail this `open` with `EIO` | Yes | It applies to this call. |
| Recipe identity | Yes, but static | Fixed when the run starts. |
| Did the program probe its environment? | No | The whole trace answers better. |
| Reward hacking, reading answers | No | Needs context before and after. |
| Did the final answer get each dimension right? | No | Naturally after the fact. |
| Model A vs. model B, regressions | No | Spans runs. |

Run-time rules are therefore declarative only: no scripts and no LLM calls on
the syscall path. Each decision is a function of the profile, the seed and the
event, so a run can be replayed exactly. After-the-fact analysis runs
outside needy on the trace (see the
[CLI note](../cli/2026-11-09-cli-exec-and-report-design.md#trace)).

A later escape hatch may add an `ask` action that pauses the syscall, calls an
external decision service and records the answer in the trace, so replay reuses
the recorded answer. It is off by default.

## Format

- Profiles and recipes are **YAML** on disk (decided 2026-11-09).
- The schema is defined in `proto/`, so Buf can also generate TypeScript types
  for tools that emit profiles.
- `needy-config` loads profiles and recipes. Unknown keys are errors, except in
  maps whose keys are names (executors, presets, file mappings, environment
  variables). Values are checked when the document loads: durations
  (`500ms`, `10m`, `3d4h`, or a number of seconds), sizes, sides and enum
  values. An error names the key path, for example
  ``commands[2].when.script: unknown field `script` ``.
- After loading, `needy-config`'s `validate` checks the names a profile uses:
  presets in `use:`, executors in `to:`, `executor:`, `default_executor`,
  `unknown` and `executor:NAME:` sides, and file mappings in `run_files`
  and a rule's `files:`. Each problem is an error finding. `needy profile
  check` prints the findings and exits 1 on an error; run create refuses
  the profile.
- `needy profile check` asks the control plane, which adds what only it
  knows: whether each `runner:` selector resolves among the connected
  runners, and the recipe checked against the CPU of the host the actor
  would run on (a runner reports its CPU and KVM in every heartbeat).
  `--local` checks offline against this machine.

## Profile structure

A profile describes one run. Machines are described by
[recipes](2026-11-09-recipe-design.md) and referenced as `builtin:NAME` or by a
path relative to the profile.

```yaml
extends: base.yaml               # optional parent profile; see Extends

user: { name: agent, uid: 1000, gid: 1000 }   # same in the actor and every container executor; ~ = /home/agent

actor:
  runtime: firecracker           # required: firecracker (a microVM, needs /dev/kvm) | container
  # image: needy-custom-jq       # optional, every driver: an OCI image FROM needy's base (VM drivers: `needy build --from` it)
  # container: { oci_runtime: runsc }   # required with `container`: runc | runsc
  runner: kvm                    # optional; the control host when absent, see Runners
  recipe: gce-e2-standard-4
  limits: { single_process: true, self_replace: true }
  workspace:                     # see Workspace
    path: ~/workspace
    from: executor:build:~/workspace
    writable: true
  network:
    allow: [openrouter.ai]       # HTTPS tunnelled as is
    services:                    # see Actor network
      ollama: { url: "http://127.0.0.1:11434" }

executors: { … }                 # see Executors

executor_network:
  allow: [registry.npmjs.org, pypi.org]
  record: true

presets: { … }                   # see Composition
commands: [ … ]                  # see Commands
files: [ … ]                     # see File mappings

chaos:                           # see Chaos
  seed: 42
  faults:
    flaky-json: { file: { open: "**/*.json" }, errno: EIO, probability: 0.01 }
    slow-npm: { net: { connect: registry.npmjs.org }, delay: 2s, nth: 3 }
    overheat: { sensor: { temp_c: 101 }, at: 5m }

trace: { sinks: [sqlite, jsonl], timing: false }   # see Trace
```

A `network.allow` entry is a glob matched against the host name in any
case, and `*.example.com` also matches `example.com` itself
(`Glob::matches_host`, which the actor gateway and the profile share).

### Actor network

The actor has no network device of its own. Its programs reach the outside
through `HTTP_PROXY` (`http://127.0.0.1:3128` in the actor), which init
forwards to the **actor gateway**, a sidecar next to the daemon on the
control host.

```yaml
actor:
  network:
    allow: [opencode.ai, "*.opencode.ai"]   # HTTPS tunnelled as is, no MITM
    services:
      ollama:                                # in the actor: http://ollama.needy
        url: http://127.0.0.1:11434
```

- `allow`: a `CONNECT` tunnel or plain-HTTP request to a matching host goes
  through; anything else is answered 403 and recorded as `net.deny`. HTTPS is
  tunnelled as is: the gateway sees only the host.
- `services`: a name (a DNS label) and a `url` (`http://` or `https://`,
  with an optional base path). In the actor the service is
  `http://NAME.needy/...` (`needy_config::profile::service_host`); the
  gateway answers that host itself, so no DNS is involved, forwards the
  request to `url` plus the request's path and query, and records
  `net.request` with the host `NAME.needy`. A service's host is allowed
  without an `allow` entry. `url` is resolved where the gateway runs, on the
  control host, so `127.0.0.1` is the control host's loopback, never the
  actor's. A public host under `.needy` cannot be reached.
- `needy profile check` prints `services: http://ollama.needy ->
  http://127.0.0.1:11434`; loading refuses a name that is no DNS label and a
  URL that is not absolute `http(s)`, and the control plane warns when the
  URL's host and port do not accept a TCP connection from the control host
  within 2 s.
- Header injection (`inject:`) was removed: the gateway cannot add a header
  to an HTTPS request it only tunnels. `TODO(mitm)`: it needs a separate MITM
  infrastructure (its own CA, opt-in, per host), deferred until that exists
  and the owner approves it.

## Executors

Needy places executors on runners by selector (see Runners) but does not
balance load or track live resource use. Resource limits belong to each
provider's own options. Needy keeps a pool of executor instances
indexed by key and a counter per instance.

```yaml
executors:
  build:
    runner: { labels: [kvm] }             # see Runners
    provider: docker
    docker:
      build:                              # image build: docker build options
        context: ./env
        dockerfile: Dockerfile.build
        target: toolchain
        args: { RUST_VERSION: "1.97" }
      cpus: 4                             # run options: typed fields, see Run options
      memory: 8GiB
      pids_limit: 512
    key: "{run}"
    max_procs: 4          # matched commands running at once per instance
    on_full: wait         # wait | reject
    max_instances: 16
    idle_timeout: 10m

  vm:
    provider: firecracker                 # not built yet
    recipe: gce-e2-standard-4             # same machine as the actor
    key: "{run}"
    max_procs: 2

  mac:
    runner: mac                           # a Mac that joined as a runner; nothing to build
    workspace: /Users/user/work
    key: "{rule}"
    max_procs: 1
    on_full: wait
```

Providers: `native` (the default: the runner's own process space, or the
control host's without `runner:`), `docker`, and `containerd`, `firecracker`
and `k8s`, which are refused when a run is created until each is built.
`native` machines are managed by their owners and have no build step. An
executor with `docker:` options and another provider is an error. `runtime:`
was renamed `provider:`; `runtime: remote` and `remote: {url, token_env}`
were removed (2026-11-10): a machine joins as a runner and the profile
selects it, and loading a profile that still has them says so.

### Runners

`runner:` places an executor or the actor on a runner, a machine that joined
the control plane ([runners and runs](../architecture/2026-11-10-runner-and-run-design.md#as-built-slice-5)):

| Form | Picks |
| --- | --- |
| absent | The control host itself |
| `runner: NAME` | That runner, which must hold the role and be connected |
| `runner: { labels: [KEY, KEY=VALUE] }` | The one connected runner with the role and every label; `KEY` matches any value. None or several is an error naming the candidates. |

The control plane resolves selectors when a run is created, before it
acquires anything. As built, actors and Docker executors run on the control
host: directly, or on the runner the control daemon names as its own
(`--local-runner`); other placements are refused with a clear error.

### Instance key

Commands whose key is equal share one instance; different keys get separate
instances.

| Key | Effect | Use |
| --- | --- | --- |
| `{run}` (default) | One instance per run | RL: runs are isolated |
| `{profile}` | Shared by every run of a profile | Shared build caches during development |
| `{workspace}` | Shared per workspace | Several runs on one repository |
| `{rule}` | Shared per matching command rule | One Mac VM for every `xcodebuild` |
| Combinations, such as `{run}:{cwd}` | Finer split | — |

Commands that operate on one workspace must land on one instance, or at least on
one volume. A key may use only deterministic inputs, so that a run can be
replayed.

### Full instances and limits

| `on_full` | Behaviour |
| --- | --- |
| `wait` | Queue until a slot frees; the command is slower. |
| `reject` | Return an error. |

`on_full: new` (start another instance with the same key) was removed
(2026-11-11): it behaved like `wait`, since a second instance per key needs
`max_instances`, which is not built. A profile that names it is refused.

`idle_timeout` stops unused instances. Per-instance runtime limits multiplied by
`max_instances` bound what one executor can take from its host.

### Builds

`build` options and run options are separate: `docker.build.args` are build
arguments; run options are the typed `docker:` fields below. Builds are
cached by the hash of their inputs. The resolved image digest is written to
the trace, so a run can be reproduced after `latest` moves.

### Run options

Run options are typed fields under `docker:` (2026-11-11). The daemon
creates containers through the Docker API, and each field sets the API
field that the matching `docker run` flag sets, with the value `docker run`
would send (`crates/core-executor/src/provider/docker.rs` cites the
docker/cli and bollard sources). They replaced `docker.args`, which took
`docker run` strings and translated an allowlist of them; a profile that
still has `docker.args` is refused with the list of fields, without
compatibility.

```yaml
docker:
  image: example.com/toolchain:1
  cpus: 1.5                     # HostConfig.NanoCpus
  cpu_shares: 512               # CpuShares
  cpuset_cpus: "0-3"            # CpusetCpus
  memory: 8GiB                  # Memory; sizes as in recipes, a bare number is MiB
  memory_swap: unlimited        # MemorySwap: a size, or unlimited (-1)
  shm_size: 64MiB               # ShmSize
  pids_limit: 512               # PidsLimit
  ulimits: { nofile: 1024, nproc: { soft: 64, hard: 128 } }   # Ulimits
  env: { RUST_LOG: debug }      # Config.Env
  workdir: /src                 # Config.WorkingDir
  read_only: true               # ReadonlyRootfs
  init: true                    # Init
  mounts:                       # Mounts
    - { type: bind, source: ./cache, target: /cache, read_only: true }
    - { type: volume, source: cargo, target: /usr/local/cargo/registry }
    - { type: tmpfs, target: /tmp, size: 1GiB }
  add_host: { db.example.com: 192.0.2.10 }   # ExtraHosts
  dns: [192.0.2.53]             # Dns
  sysctls: { net.ipv4.ip_forward: "1" }      # Sysctls
  gpus: all                     # DeviceRequests: all, a count, or [device IDs]
  cap_drop: [NET_RAW]           # CapDrop
```

A value of the wrong form fails when the profile loads, with its path
(`executors.build.docker.memory: invalid size unit "g"`). Paths in the
container must be absolute. A relative bind `source` is resolved against the
profile's directory, like `workspace`. A bind source must exist; Docker does
not create it.

Fields the daemon owns are not profile fields, so naming one is an unknown
field: the network (the executor network and gateway), the workspace and
socket mounts (a mount at or under either is refused), the name, labels, the
hostname (the recipe's), the user (the profile's `user`), the entrypoint and
command, and auto-removal.

These fields are refused when the executor is created unless the daemon's
operator allows them: `privileged`, `cap_add`, `devices`
(`{ host, container, permissions }`), `gpus`, `pid`, `ipc`, `userns` or `uts`
set to `host` (another container's namespace is fine), a `security_opt` with
`unconfined`, and a bind mount whose source is outside the executor's
workspace on the host. There is no operator configuration yet, so they are
always refused. `gpus` asks the host's GPU runtime for GPUs as
`docker run --gpus` does; Docker grants that without `--privileged`, but
GPUs are host devices, so they need the operator as `devices` do (owner
decision, 2026-11-12).

`docker run` options without a field (ports, CPU period and quota, memory
reservation, OOM settings, blkio, `--runtime`, storage options, GPU drivers
and options, a tmpfs mode) are left out until a profile needs one.

Evidence (2026-11-11, a Linux KVM host with Docker, cgroup v2 and one
NVIDIA GPU behind the NVIDIA container runtime): unit tests cover the
schema and the mapping to the container body; `cargo test` created an
executor container with `memory`, `cpus` and `env` and read them back from
its cgroup and environment. A run of a control daemon with `cpus: 1.5`,
`memory: 512MiB`, `pids_limit: 256`, `env`, `add_host`, a 16 MiB `tmpfs`
mount and `gpus: all`: `needy exec` read `memory.max` 536870912,
`cpu.max` `150000 100000`, `pids.max` 256, the variable, the `/etc/hosts`
entry and the tmpfs size, and `nvidia-smi -L` listed the GPU; `docker
inspect` showed the matching `HostConfig` and a device request with count
-1 and the `gpu` capability. (That run predates the operator gate on `gpus`; the provider now refuses the same
`gpus: all` with `gpus all is not allowed by the operator`, which the unit test
`privileged_fields_need_the_operator` covers.) A profile with `docker.args` and one with
`privileged: true` were refused at `run create` with the messages above.

containerd options are merged into the OCI spec as a JSON patch with the same
fields protected.

## Chaos

```yaml
chaos:
  seed: 1
  faults:
    curl-down:
      exec: "curl **"                 # command pattern
      exit: 7                         # or signal: SIGSEGV, hang: true; delay: 2s
      stderr: "curl: (7) Failed to connect\n"
      times: 3
    flaky-json:
      file: { open: "**/*.json" }
      errno: EIO                      # ENOENT, EACCES, EPERM, EOPNOTSUPP or EIO
      probability: 0.1
    slow-example:
      net: { connect: "*.example.com" }
      delay: 2s                       # or refuse, reset, hang
    busy-model:
      net: { request: ollama.needy }
      status: 503                     # plain-HTTP requests only
      after: 1m
      until: 5m
    overheat:
      sensor: { temp_c: 101 }
      at: 30s
```

`faults` is a map: a fault's name (letters, digits, `-`, `_` and `.`)
appears in `fault.injected`, in the report and on `needy chaos` command
lines. Each fault names exactly one domain, which decides what it matches
and which effects it may have; any other key is a load error that names
what the domain takes, so `profile check` reports it and `run create`
refuses the profile.

| Domain | Matches | Effects | Delivered by |
| --- | --- | --- | --- |
| `exec: PATTERN` | The command as typed, word by word like a rule's `match` (`curl **`); its command names join the actor's route list | one of `exit: N` (0 to 255), `signal: NAME` (one that ends a process: `SIGSEGV`, `SEGV`), `hang: true`; `stderr: TEXT` before it; `delay` | `exec.decide`. In the actor the proxy writes `stderr`, then exits with the code, kills itself with the signal (default disposition, `raise`), or waits until it is killed; the command never runs. From `needy exec` the control plane answers the stream itself: the stderr, then the exit or the signal, as a process would. A `delay` alone holds the command, which then runs; the daemon waits before it answers. |
| `file: { open: GLOB }` | The path the actor opens on a workspace mount, as the actor sees it (`/home/agent/workspace/a.json`; `**/*.json` matches any) | `errno` (`ENOENT`, `EACCES`, `EPERM`, `EOPNOTSUPP`, `EIO`), `delay` | `file.open`, on the mount's SFTP stream. An errno answers the `open` with an SFTP status that sshfs turns into that errno; the executor never sees it. A delay alone forwards the open and holds its reply; other requests go on. Errnos sshfs cannot deliver (it maps every other status to `EIO`) are refused at load. |
| `net: { connect: HOST }` | Every connection the actor opens to a matching host: a `CONNECT` tunnel or a plain-HTTP request (glob as `allow`) | one of `refuse`, `reset`, `hang`; `delay` | `net.connect` / `net.request` at the gateway, after the allowlist. `refuse` answers 502 `needy: HOST: connection refused (needy chaos: NAME)`, as the gateway answers a real refusal; `reset` closes the actor's connection without an answer (on `macos-vz` it holds it: vfkit does not pass the close on); `hang` holds it until the actor gives up or the run ends; `delay` waits, then goes on. |
| `net: { request: HOST }` | Plain-HTTP requests only, such as a service's | as `connect`, or `status: N` | `status` answers with that HTTP status and the body `needy chaos: NAME`; a tunnel is opaque, so `status` with `connect` is a load error. |
| `sensor: READINGS` | — | the readings (`temp_c`) | At `at` after the actor starts: a VM's init sets the guest kernel's fake coretemp; a `linux-container` actor's driver rewrites its hwmon files from the host; Apple's `container` has no sensors (a `profile check` warning). |

Triggers, shared by `exec`, `file` and `net` faults:

- `probability: P`: fires when the roll from the seed, the fault's name and
  its match count is below `P`.
- `nth: N`: fires on the Nth match only (it takes no `probability` or `times`).
- `times: N`: fires at most N times.
- `after: D`, `until: D`: matches only from `after` and before `until`,
  counted from the run's creation. A match outside the window is not counted.
- A fault with no trigger fires on **every** match.

A sensor fault takes only `at`. Faults are checked in name order: an
operation gets the first fault that fires, and every fault whose subject and
window match counts the match. Each fired fault is a `fault.injected` event
with its name, domain, target and effect; the report counts them by name
(`injected faults: 3 (curl-down (exec) 2, flaky-json (file) 1)`).

A fault's rolls depend only on the seed, its name and how often it matched,
not on the other faults, so adding or removing one does not change another's.
An exec fault applies in the actor as a rule does: its pattern's command names
join the actor's route list (`profile check` prints it as `route names:`); a
pattern that may match any program routes every exec.

### Chaos at run time

```sh
needy chaos add curl-down --run latest --exec "curl **" --exit 7 --times 3
needy chaos add slow --connect "*.example.com" --delay 2s
needy chaos add flaky --file fault.yaml          # a chaos.faults entry
needy chaos ls                                   # NAME DOMAIN SOURCE MATCHED FIRED FAULT
needy chaos rm curl-down
```

`add` takes the fields of a `chaos.faults` entry as flags (`--exec`,
`--open`, `--connect` or `--request` for the domain; `--exit`, `--signal`,
`--stderr`, `--hang`, `--delay`, `--errno`, `--refuse`, `--reset`,
`--status`; `--probability`, `--nth`, `--times`, `--after`, `--until`) or
as YAML (`--file`), checked as a profile's. `--run` takes an ID, a prefix, a
name or `latest`, else `NEEDY_RUN`, else the only live run. The control API
is `RunService.AddFault`, `ListFaults` and `RemoveFault`; the operator and
paired clients may call them, an actor and a runner may not.

- An added fault's counts start at zero; `rm` removes a profile's fault as
  well as an added one.
- The run's trace records `chaos.add` (the name, the fault and who added it)
  and `chaos.remove` at their place in the event sequence, and the report
  lists them (`chaos changed while running: +curl-down (exec), -slow`). A
  replay applies each change before the event that follows it. Because a
  fault's rolls depend only on the seed, its name and its own match count,
  the same sequence of operations after the change fires the same faults;
  `after` and `until` count wall time from the run's creation, so a replay
  that runs faster or slower may see a window differently.
- An added exec fault reaches every command in the actor, as `needy exec`
  commands: when `add` or `rm` changes the run's route list (the profile's
  rules and the run's exec faults now, `rules::route_names_for`), the daemon
  sends it on the control stream the actor's init holds
  (`GuestService.Control`, server-streaming, the list now and then each
  change, ending with the run). Init creates the route stubs of new names
  the actor lacks and applies the list to its capture: the shims' route file
  is written beside and renamed into place, and `binfmt_needy` copies its
  parameters under `kernel_param_lock`, the lock their sysfs writes take,
  so an exec sees the old list or the new one whole. A fault whose pattern
  may match any program routes every exec (`*`); removing it shrinks the
  list again. Before 2026-11-13 init read the list once at boot and `add`
  warned which commands it did not reach.
- Decision (2026-11-13): one guest control stream for everything the daemon
  tells a running actor. `GuestServiceControlResponse` is a `oneof`: the
  route list (`routes`) and a graceful stop (`stop`, `GracefulStop`). The
  graceful stop landed first as a unary `Guest.WaitStop` held by init; it
  was folded into this stream when the route list arrived, so init holds
  one call, not two. The daemon counts its guest as waiting for a stop for
  as long as the stream is open (`StopRequest::wait`, polled from the
  stream's start); init passes a stop to the driving program's watch
  (`init/stop.rs`), which signals it as before. New daemon-to-init messages
  are new cases of the `oneof`.
- Checked on 2026-11-13 with a profile that routes only `python3` and runs
  `date` every second: `chaos add late-date --exec 'date **' --exit 9` at
  8 s and `chaos rm` 12 s later. On `linux-firecracker` (Linux KVM host, a
  base VM image rebuilt with the locked `binfmt_needy`), `macos-vz` (Apple
  silicon Mac, image rebuilt likewise) and `macos-apple-container` (same
  Mac): init logged each route list as it arrived (`route list:
  ash,bash,dash,date,python3,sh,zsh`, then without `date`); `date` exited 0
  before, 9 with the fault's stderr in between (12, 12 and 10 times), and 0
  after; the report showed the injected faults and `chaos changed while
  running: +late-date (exec), -late-date`. With the stop folded into the
  stream, a `needy run stop` 12 s after the `rm` then ended each run
  gracefully: init logged `stopping the driving program: SIGTERM, then
  SIGKILL after 10s`, and `run ls` showed the run `stopped` with exit
  `SIGTERM`.
- A sensor fault cannot be added at run time; its `at` counts from the
  actor's start (`TODO(chaos-sensor)`).
- The external decider (`ask`) stays deferred (`TODO(intercept)`).

## Trace

`trace:` selects where the run's events go and whether hook latencies are
recorded. Both keys are optional.

| Key | Default | Meaning |
| --- | --- | --- |
| `sinks` | `[sqlite, jsonl]` | `sqlite` writes the daemon's `trace.db`, which `needy report` reads; `jsonl` appends to `runs/<id>/trace.jsonl`. An empty list records nothing. |
| `timing` | `false` | Records the latency of `exec.decide`, `exec.start`, `net.request` and `net.connect` as one `trace.timing` event when the run stops |

A run whose profile cannot be read uses the defaults, so its `run.fail`
event is still recorded. An OpenTelemetry sink is deferred. Terminal output
is not recorded.

## Commands

### Matching

A rule's `match` is written like the command it matches:

```yaml
match: git **                     # git with any arguments
match: git commit *               # git commit with exactly one argument
match: pnpm dlx prettier **       # literal words at any position
match: "{node,bun} {script} **"   # alternatives; {script} captures one argument
match: /usr/local/bin/python3 **  # a path matches the resolved executable; a bare word matches the command name
```

| Token | Meaning |
| --- | --- |
| `git` | Literal word |
| `*` | Exactly one argument |
| `**` | Zero or more arguments |
| `{a,b}` | One of the alternatives |
| `{name}` | Capture one argument for use in `when` and actions |

Conditions that are not positional go in `when`:

```yaml
when:
  has: [--watch, -w]              # any of these flags anywhere in the arguments
  captures:
    script: "**/pi/dist/cli.js"   # glob on the argument {script} captured
  cwd: /workspace/**
  env: { CI: "1" }
  under: "pnpm test **"           # an ancestor process matches
  tty: true
```

Launchers such as `bash -lc '…'`, `pnpm dlx`, `npx` and `node script.js` hide
the command that really runs; see [Launcher resolvers](#launcher-resolvers).

### Actions

| Target | Form | Example |
| --- | --- | --- |
| Placement | `to: actor`, or `to: NAME` for an executor | Send `xcodebuild` to the `mac` executor. |
| Arguments | `prepend`: after the command name, before the user's arguments | `git` becomes `git -c color.ui=never …` |
| | `append`: after the user's arguments | `pytest … -p no:cacheprovider` |
| | `wrap`: around the whole command | `timeout 600 <command>` |
| | `exec`: replace the command; may use captures | Replace `curl` with an audited fetcher. |
| Environment | `set` | `CI=1` |
| | `default`: only when unset | `LANG=C.UTF-8` |
| | `prepend` / `append` on list variables | Put `/opt/tools/bin` before `PATH`. |
| | `unset` | Remove `SSH_AUTH_SOCK`. |
| | `clear` with `pass` | Keep only `PATH HOME LANG TERM`. |
| Working directory | `cwd`: map or fix | `/workspace` maps to `~/projects/foo` on the executor. |
| Files | Mappings, as below | — |
| Other | `network`, `timeout`, `deny: MESSAGE` | — |

A routed command's program keeps its path. When an executor's `path_map`
maps `argv[0]`, the command runs from the mapped path. Any other absolute
`argv[0]`, such as the actor's `/usr/bin/python3`, is sent as it is, and the
executor runs it from that path when the path exists there; otherwise it runs
the program of the same name from its own `PATH`. When neither exists, the
error names the original path. A `path_map` key is an actor path: a leading
`~` in it is the profile user's home, as on a file mapping's actor side; a
value is a path on the executor's machine, and a leading `~` in it is that
executor's home (see [Home directories](#home-directories-)). A command placed with `--executor`, or run
without a run, is never rewritten and gets no `PATH` fallback.

### Composition

Every rule that matches applies, in order:

- **Single-valued fields take the first rule that sets them**: `to`, `deny`,
  `timeout`, `cwd`. Reading from the top tells you where a command goes.
- **Collections accumulate in rule order**: environment changes, `args.prepend`,
  `args.append`, `wrap` (outermost first) and `files`.
- **`final: true`** stops evaluation after that rule.
- **Presets** are named blocks of actions reused with `use:`, like CSS classes.
  A preset's fields count as if written in the rule that uses it.
- **`extends`** inherits a parent profile; the parent's rules come after the
  child's (see [Extends](#extends)).

```yaml
presets:
  clean-env: { env: { clear: true, pass: [PATH, HOME, LANG, TERM] } }
  mac:       { to: mac }

commands:
  - match: "**"                   # accumulates for every command
    env: { set: { CI: "1" } }
  - match: pnpm dlx pi **         # the harness itself stays on the actor
    to: actor
    final: true
  - match: "{node,bun} {script} **"
    when: { captures: { script: "**/pi/dist/cli.js" } }
    to: actor
    final: true
  - match: "{uname,lscpu,sw_vers} **"   # identity commands stay on the actor's recipe
    to: actor
  - match: curl **
    when: { has: [-k, --insecure] }
    deny: insecure TLS is not allowed
  - match: git **
    args: { prepend: [-c, color.ui=never] }   # also gets CI=1; placement comes from a later rule
  - match: "{xcodebuild,codesign} **"
    use: [mac, clean-env]
  - match: "**"                   # everything not yet placed goes to the build executor
    to: build
    use: [clean-env]
```

### Evaluation

Deciding a command is the `exec.decide` hook point: an ordered chain of
interceptors (`needy_core::intercept`), each of which sees the decision so
far and returns it. Interceptors only decide; the daemon applies their effects.

1. **Rules** (`needy_core::rules`). Launcher resolvers rewrite the command
   into its effective form. Every rule whose `match` and `when` hold applies
   in order, as in [Composition](#composition). Patterns are compiled once per
   decision and shared with resolvers, `when.under` and `exec` faults
   (`needy_core::pattern`). The outcome is the first `deny`, else the first
   placement, else a shell script's common placement (see
   [Launcher resolvers](#launcher-resolvers)), else `default_executor`, else
   the actor.
2. **Chaos** (`needy_core::chaos`). Unless the command is already denied,
   exec faults are checked against the command as typed. An `exit`,
   `signal` or `hang` turns the outcome into the fault (`fault: NAME`); a
   `delay` keeps the placement and the daemon waits before answering. Each
   fired fault becomes a `fault.injected` event.

The daemon records each decision as an `exec.decide` event with its origin
and outcome:

- A command the actor's kernel routes is decided once, when the proxy asks;
  its later exec on the executor is not decided again.
- A `needy exec` in a run without `--executor` runs the whole chain. A
  command the rules keep on the actor runs on `default_executor` (or the
  run's first executor), and the event records that executor, since a CLI
  command has no actor to run on.
- A `needy exec --executor NAME` skips the rules; only chaos runs, and no
  `exec.decide` event is recorded.

`profile check --cmd` runs the rules and lists the `exec` faults that match
without firing them.

The actor's proxy reads the routed command's ancestors from `/proc` and sends
them in `Decide`, and the daemon matches `when.under` against them. A
closed-loop run confirms it; its regression test is fix slice 8f. Commands
from `needy exec` have no ancestors. A user decider after chaos (`ask`,
above) is deferred.

### Extends

`extends` names a parent: a path relative to the extending file, or
`builtin:NAME` (there are no built-in profiles yet). A parent may extend
another; a chain is at most 16 deep and may not cycle. Profiles and recipes
share one merge model, applied to the documents before they are typed:

- Maps merge key by key, recursively. A child that sets
  `executors: { build: { max_procs: 8 } }` keeps the parent's other `build`
  fields and its other executors.
- A child's scalar or list replaces the parent's: `run_files`,
  `actor.network.allow`, a rule's `args`.
- A child's `null` (`~`) removes the parent's value, so the field takes its
  default.
- `commands` and `resolvers` are prepend lists: the child's entries come
  first, then the parent's, so the child's rules are evaluated first.

Relative paths resolve against the directory of the profile that was loaded,
including paths a parent wrote.

The client resolves the chain: `needy run create` sends the merged
profile and the merged recipe file it names as text, so a control daemon on
another machine reads no path on the client's.

## Launcher resolvers

Needy sees every real `execve` in the tree, so it can often decide late: a
launcher's later exec reveals the real tool. Late decisions fail when the
launcher's own placement must be chosen first; `pnpm dlx X` downloads into a
cache, so `X` must run where the cache is. Resolve early from arguments where
possible, and otherwise make the launcher's placement sticky for its subtree.

| Launcher | Effective command | Known at the launcher's exec? |
| --- | --- | --- |
| `env [-i] [-u V] [V=x]… cmd`, `sudo`, `nice`, `timeout`, `nohup`, `stdbuf`, `time`, `ionice`, `setsid` | Arguments after the launcher's own options and assignments | Yes |
| `xargs [opts] cmd` | `cmd`; its arguments come from stdin | Command yes, arguments no |
| `sh -c S`, `bash -c S`, `bash -lc S` | Parse `S` | Only for word-only scripts |
| `npx P`, `npm exec P`, `pnpm dlx P`, `yarn dlx P`, `bunx P`, `uvx P`, `pipx run P` | Package `P`; the binary appears at a later exec | Package yes, binary later |
| `pnpm exec C`, `npm exec -- C`, `uv run C` | `C` from `node_modules/.bin` or `.venv/bin` | Yes |
| `npm`, `pnpm`, `yarn`, `bun` `run S` | `scripts.S` in the nearest `package.json`, then a shell parse | From the project files |
| `cargo run [--bin B]` | The built binary | At the later exec |
| `python -m M` | Module `M` | Yes |
| `node F`, `bun F` | Script path, nearest `package.json` name and version, content hash | Yes, by reading files |
| `corepack pnpm …` | Strip `corepack` and resolve again | Yes |
| `make T` | Recipes are shell; keep the subtree sticky | No |

**Shell scripts.** Parse with [brush-parser](https://docs.rs/brush-parser), a
maintained POSIX and bash parser. Like
[Codex](https://github.com/openai/codex/pull/7127), accept only word-only
scripts: plain commands joined by `&&`, `||`, `;` and `|`. If every
subcommand resolves to the same placement, place the shell there. Redirects,
substitutions, `eval`, functions, aliases and here-documents fall back to the
`unknown` policy. `bash -l` loads a profile, so it is never fully static.

Built-in resolvers cover the table. Users add more:

```yaml
resolvers:
  - match: "mytool run {script} **"
    effective: "{script} **"          # match again after rewriting
  - match: "pnpm dlx {pkg} **"
    identity: { package: "{pkg}" }    # early identity from arguments
    sticky: true                      # children inherit the placement
  - match: "make **"
    sticky: true
unknown: sticky                       # sticky | actor | <executor name>
```

## File mappings

The schema borrows rclone's ordered filters, Mutagen's directions and Lima's
mount keys ([rclone filtering](https://rclone.org/filtering/),
[Mutagen modes](https://mutagen.io/documentation/synchronization/),
[Lima mounts](https://lima-vm.io/docs/config/mount/)).

| Mode | Direction | When | Example |
| --- | --- | --- | --- |
| `copy-in` | Host → actor or executor | Run start | `.agents/`, `AGENTS.md`, Codex configuration |
| `mount` | Live view; the executor holds the data | During the run | The workspace |
| `mirror-out` | Actor or executor → host | Continuously or at the end | `~/.codex/sessions`, `~/.claude/projects` |

```yaml
files:
  skills:
    mode: copy-in
    from: host:./.agents/
    to: actor:~/.agents/
  codex-home:
    mode: copy-in
    from: host:~/.codex/
    to: actor:~/.codex/
    filter: ["- /auth.json", "- /sessions/**", "+ **"]   # ordered; first match wins
  codex-sessions:
    mode: mirror-out
    from: actor:~/.codex/sessions/
    to: host:./runs/{run}/codex/
    when: continuous                    # continuous | end
    delete: false                       # never propagate deletions to the host
  ignore_file: .needyignore             # optional gitignore-style file, read as "-" rules

run_files: [skills, codex-home, codex-sessions]   # actor.workspace's mapping is always selected

commands:
  - match: "{xcodebuild,codesign} **"
    to: mac
    files: [skills]                     # added to run_files for this command
```

- Mappings are **named**. A run lists its mappings; a command rule may add
  more. They accumulate as a union.
- Sides are written `host:`, `actor:` and `executor:<name>:`. `~` is the
  profile user's home on the actor side and the executor's home on an
  executor side (see [Home directories](#home-directories-)); the actor and container executors use
  the profile's UID and GID, so no ownership mapping or `chown` is needed.
- A `host:` path is a path on the control daemon's machine, and `~` there is
  that machine's home. The client makes a relative `host:` path absolute
  against the profile's directory, so it names the intended files only when
  the client runs on the control daemon's machine (`TODO(runner-files)`:
  moving files between a client and the control plane belongs to
  [runners and runs](../architecture/2026-11-10-runner-and-run-design.md)).
  Until then, run create fails when a copy-in source does not exist or
  cannot be read on the control host, or when a mirror-out destination
  would not be under a directory the control host has (the profile's
  directory for a relative path, the home for `~`, the parent of an absolute
  path). A mirror-out that fails at the end of a run is recorded as
  `files.fail` and is a `needy report` finding.
- Filters follow rclone: ordered `+` and `-` rules, `**` for any depth, a
  leading `/` anchored at the mapping root.
  The [file filter guide](../../explanations/file-filters.md) describes the
  implemented rules, default inclusion and examples for state between runs.
- Harness home directories contain credentials, such as Codex's `auth.json`.
  Model keys are injected by the actor gateway, so credentials must never be
  copied into the actor; otherwise an agent that replaces its harness can read
  them. Harness presets exclude known credential files by default.
- Mirrored-out directories are stored with the trace.
- The driving program starts in `actor.workspace.path`, or in `~` without
  a workspace. `actor.cwd` was removed.

### Workspace

```yaml
actor:
  workspace:
    path: ~/project                    # in the actor; the driving program's cwd
    from: executor:build:~/project     # or host:PATH, or omitted for a fresh directory
    writable: true                     # default false
```

One setting replaces three that had to agree: `actor.cwd`, the workspace's
`mount` mapping and the executor's `path_map` entry for it.

- `path` is required, absolute or under `~` (the profile user's home in the
  actor).
- `from: executor:NAME:PATH` derives a `mount` mapping named `workspace`
  (`PATH` on executor `NAME` to `path`, writable as given), always selected
  even when `run_files` does not name it, and a `path_map` entry `path ->
  PATH` on `NAME`. A `native` executor without `workspace:` gets `PATH` as
  its working directory, so `needy exec` without a directory starts there.
- `from: host:PATH` derives a `copy-in` mapping `workspace`, and with
  `writable: true` a `mirror-out` `workspace-out` back to the same host path
  when the run ends (`delete: false`).
- Without `from`, init creates the directory, owned by the user, in the
  actor's home tmpfs. With `host:` or no `from`, `path` must be under `~`,
  because the actor's root is read-only.
- Load errors: `files.workspace` (or `workspace-out`) beside
  `actor.workspace`, a `path_map` entry for the same actor path (the message
  names both targets), an unknown executor, `from: actor:`, a relative
  `path`.
- `files:` stays for other mappings and `path_map` for other prefixes.

### Home directories (`~`)

| Where | `~` means |
| --- | --- |
| Actor side: `actor:` file sides, `path_map` keys, `workspace.path`, a rule's `cwd` | `/home/<profile user>` |
| Host side: `host:` | the control host's home, read by the control daemon |
| Executor side: `executor:NAME:` file sides, `path_map` values, a native executor's `workspace`, the executor path in `workspace.from` | that executor's home: `/home/<profile user>` in a `linux-container` executor; for a `native` executor the home of the user the control daemon or runner runs as (a runner reports it in its heartbeats, `RunnerHost.home`), resolved when the instance is created |

Only a leading `~` or `~/` expands; `~user` and a `~` elsewhere stay as
written. Before 2026-11-12 an executor side's `~` was `/home/<user>` (file
sides), kept as written (`path_map` values) or the daemon's `$HOME`, at any
position (a native `workspace`).

### User

`user: { name, uid, gid }` is the one account the actor and every
`linux-container` executor use. needy writes its passwd and group entries
(and a locked shadow entry where `/etc/shadow` exists) before the driving
program starts, replacing any entry with the same name or ID (Ubuntu's
`ubuntu` at 1000), so `HOME`, `USER`, `LOGNAME`, `whoami`, `id` and
`getent passwd` agree.

- `linux-firecracker`, `macos-vz`, `macos-apple-container`: init writes the
  files under `/run/needy/account/` and bind-mounts each over its `/etc`
  file; where the read-only root has no `/home/NAME`, init mounts a tmpfs at
  `/home` first.
- `linux-container` actor and executor: init has no `CAP_SYS_ADMIN` there,
  so the driver reads the image's files from a container it creates and
  never starts, writes the rewritten files on the host and binds them
  read-only (`needy_core_executor::account`).
- A `native` executor runs as the runner's own user; the profile's `user`
  does not apply there.

## Schema redesign (2026-11-12)

The owner's decisions of 2026-11-12, implemented the same day: header
injection removed and `upstream` redesigned as services ([Actor
network](#actor-network)); chaos as a named map split by domain with real
effects ([Chaos](#chaos)) and changed at run time ([Chaos at run
time](#chaos-at-run-time)); one consistent [user](#user); the workspace in
one place and one rule for [`~`](#home-directories-). Breaking, without aliases.

### Decisions during implementation

- **Service host names are `NAME.needy`.** Short, as the owner's example;
  the gateway answers them without DNS, so the only cost is that a public
  host under `.needy` cannot be reached. `.internal` (ICANN-reserved) was
  considered and rejected as long.
- **`url` may be `https://`.** The actor speaks plain HTTP to the gateway,
  which may speak TLS to the service, so an HTTPS model API can be named
  without the actor holding its address.
- **The actor network and the executor network are separate types.**
  `actor.network` takes `allow` and `services`; `executor_network` keeps
  `allow` and `record` (still not enforced, `TODO(executor-network)`).
- **Faults are checked in name order, and a fault's roll mixes its name, not
  its index**, so adding or removing a fault at run time does not shift the
  others.
- **`net` has two matchers.** `connect` matches every connection to a host
  (tunnel or plain request); `request` matches plain-HTTP requests only, the
  only ones a `status` can answer.
- **`refuse` is a proxy's refusal, `reset` an answerless close.** Through an
  HTTP proxy a program never sees `ECONNREFUSED` itself; the 502 is what a
  real refused upstream gives through the gateway. `reset` closes the
  gateway's side; the in-actor forwarder then closes the program's
  connection, so the program sees the peer close (end of stream), not a TCP
  RST. On `macos-vz` the close does not reach the guest through vfkit
  0.6.4, so a reset looks like a hang there (`TODO(macos-vz-reset)`).
- **Exec faults from `needy exec` are answered by the control plane**, with
  the fault's stderr and exit or signal, so `needy exec` exits as the
  program would (7, or 128 + the signal), instead of 126 with "fault
  injected".
- **`stderr` needs `exit`, `signal` or `hang`**; a `delay` alone lets the
  command run after it. `exit`, `signal` and `hang` exclude each other, as
  `refuse`, `reset`, `hang` and `status` do.
- **Signals are the ones that end a process** (signal(7)'s `Term` and
  `Core`); `SEGV` is spelt `SIGSEGV` when it loads.
- **`nth` excludes `probability` and `times`**; `until` must be later than
  `after`.
- **A file fault's glob matches the path in the actor**, not the path
  under the mount, and the file filter is installed on every workspace
  mount, so a fault added later applies to mounts already open.
- **A file `delay` holds only the open's reply**, by its SFTP request ID;
  other requests on the mount go on.
- **A rule's `faults:` action was removed**: it accumulated in the decision
  but never fired.
- **`fault.injected` carries `name`, `domain` and the effect's own fields**
  (`exit`, `signal`, `stderr`, `hang`, `errno`, `refuse`, `reset`, `status`,
  `delay`, `temp_mc`); the outcome of `exec.decide` names the fault
  (`fault: NAME`). The event schema is version 9; traces of 8 are refused.
- **`hwmon` is the `sensor` domain**, and `GetConfigResponse.hwmon_faults` is
  `sensor_faults` with the fault's name (a breaking proto change, as `buf
  breaking` reports).
- **The chaos RPCs are on `RunService`**, since a run owns its faults, and
  take the fault as YAML, so the CLI, the profile and the control plane share
  one schema and one check.
- **`needy chaos add` refuses sensor faults**. Until 2026-11-13 it also
  warned about commands the actor did not route; the route list is now sent
  to the running actor (see above).
- Workspace and `~` (from the workspace slice): conflicts with the derived
  mapping or path map are load errors, not findings; a `linux-container`
  executor's `workspace:` is not defaulted from `from` (it is a host
  directory bind-mounted at `/home/<user>/workspace`); without
  `actor.workspace` the driving program starts in `~` even when a `files`
  mount provides `~/workspace`; `workspace` and `workspace-out` are reserved
  only with `actor.workspace`.
- User (from the user slice): a collision by name or by ID drops the
  image's entry, the profile user always wins; generated entries have
  `/bin/bash` and an empty GECOS field; the host-side shadow file is 0640 and
  holds only locked entries.

### Verification (2026-11-12 and 13)

Machines: a Linux KVM host (x86_64, Docker Engine with `runc`, no `sshfs`
and no passwordless `sudo`) and an Apple silicon Mac (vfkit 0.6.4, Apple's
`container`, OrbStack's engine). Each check used a private control daemon
(its own state, runtime, config and data directories, PIDs recorded),
images built from this branch (`make guest`, `needy build --out` into the
private data directory, and `docker build --target executor` /
`actor-container` tagged `:config`), and a local `python3 -m http.server`
on `127.0.0.1:18080` as the service. Unit and in-process tests: `make check`
and `make test` pass on both machines.

| Check | Driver | Command | Result |
| --- | --- | --- | --- |
| Services | `linux-firecracker`, `macos-vz` | a profile with `services: { echo: { url: http://127.0.0.1:18080 } }` running `curl http://echo.needy/hello.txt` and `curl http://other.needy/` | `hello from the service`, exit 0; `other.needy` 403 and a `network denied` finding |
| Service check | control plane on the Mac | `needy profile check` with `gone: { url: http://127.0.0.1:18099 }` | `services: http://... -> ...` listed; `warning: actor.network.services.gone: http://gone.needy goes to http://127.0.0.1:18099, which does not accept a connection (Connection refused ...) from this control host, where the gateway runs` |
| exec `exit` + `stderr` | `linux-firecracker`, `macos-vz` | `curl -s http://example.invalid/` with `{ exec: "curl ** http://example.invalid/", exit: 7, stderr: ... }` | `curl: (7) Failed to connect`, exit 7 |
| exec `signal` | both | `python3 -c pass` with `signal: SIGSEGV` | the shell printed `Segmentation fault`, exit 139 |
| exec `delay` | both | `/bin/true` with `delay: 2s` | took 2 s, exit 0 |
| exec `hang` | both | `timeout 2 grep x /dev/null` with `hang: true` | exit 124 |
| exec from `needy exec` | in-process (CLI test) | `needy exec --run R -- curl http://example.com` | stderr `curl: (7) ...`, exit 7; ndjson `fault.injected`, `exec.decide` (`fault: curl-down`), `stderr`, `exit` |
| net `status` | both | `curl http://busy.needy/hello.txt` with `{ net: { request: busy.needy }, status: 503 }` | 503 |
| net `reset` | `linux-firecracker`, `macos-apple-container` | `curl http://cut.needy/hello.txt` with `reset: true` | `curl: (52) Empty reply from server`, exit 52 |
| net `reset` | `macos-vz` | the same, `curl -m 5` | `curl: (28) Operation timed out`: vfkit 0.6.4 does not pass the gateway's close to the guest (`TODO(macos-vz-reset)` at the gateway) |
| net `refuse` | both | `curl https://example.com/` with `{ net: { connect: example.com }, refuse: true }` | `curl: (56) CONNECT tunnel failed, response 502`, exit 56 |
| net `delay` | both | `curl http://slow.needy/hello.txt` with `delay: 2s` | took 2 to 3 s, 200 |
| net `hang` | both | `curl -m 2 http://stuck.needy/hello.txt` with `hang: true` | `curl: (28) Operation timed out after 2002 milliseconds`, exit 28 |
| file `errno` | both | `cat ~/project/a.json` with `{ file: { open: "**/*.json" }, errno: EIO }`, and `present.txt` with `ENOENT` | `Input/output error`, exit 1; `No such file or directory` for a file `ls` lists |
| file `delay` | both | `cat ~/project/slow.md` with `delay: 2s` | read after 2 s |
| `needy chaos add/ls/rm` | `linux-firecracker` | a run asking a service and running `python3 -c pass` every second; after 6 s `chaos add busy --request echo.needy --status 503`, `chaos add crash --exec 'python3 -c **' --exit 3 --stderr boom`, `chaos add late --exec 'curl **' --exit 9`, `chaos rm late`; after 5 s `chaos ls`, then `rm busy`, `rm crash` | `add late` warned that the actor does not route `curl`; `ls` showed `busy` and `crash`, `added`, matched 5, fired 5; the program saw 200 before and after the window; the report: `injected faults: 10 (busy (net) (5), crash (exec) (5))` and `chaos changed while running: +busy (net), +crash (exec), +late (exec), -late, -busy, -crash`; the trace has the `chaos.add` and `chaos.remove` events with `by: operator`; `chaos ls` of the ended run: `run ... has ended: finished` |
| User rename | `linux-firecracker`, `macos-vz` | `user: { name: bob, uid: 1001, gid: 1001 }`, `whoami; id; getent passwd bob; echo $HOME $USER $LOGNAME` | `bob`, `uid=1001(bob) gid=1001(bob)`, `bob:x:1001:1001::/home/bob:/bin/bash`, `/home/bob bob bob` |
| User rename | `linux-container` (`runc`) actor and executor | `user: { name: bob, uid: 1000, gid: 1000 }` and a routed `python3` printing `getpass.getuser()`, the uid and `HOME` | actor as above at 1000; executor `bob 1000 /home/bob` (Ubuntu's `ubuntu` at 1000 replaced) |
| User rename | `macos-apple-container` | as the VM drivers, at 1001 (the user slice's run) | as above |
| Workspace | `linux-firecracker`, `macos-vz` | `workspace: { path: ~/project, from: "executor:NAME:DIR", writable: true }` | `pwd` `/home/bob/project`; files written in the actor appear in `DIR` and back; file faults match `~/project/...` |
| Workspace forms | `macos-vz` (the workspace slice's runs) | `from: executor:mac:~/checkout`; no `from`; `from: host:DIR` writable; an explicit `path_map` for the same path | the runner's home used; a fresh owned directory; copied in and mirrored back; `profile check` exit 1 naming both targets |

End to end, unchanged examples through `extends` wrappers that only point
the executor and container actor at the `:config` images (and, for
wrappers outside the example's directory, its recipe and `host:` paths):

| Example | Driver | Result |
| --- | --- | --- |
| `examples/smoke` | `linux-firecracker` | finished, exit 0; GCE identity (`Google Compute Engine`, `google`); `python3` routed to `build` (4, 1 failed: `exit 7`); files both ways |
| `examples/chaos` | `linux-firecracker` | finished, exit 0; copy-in without `secret.txt`; `broken.json` `Input/output error`; `curl` exit 7 with its stderr; the sensor read `+101.0°C ALARM (CRIT)`; `result.txt` mirrored out; `injected faults: 3 (broken-json (file) (1), curl-down (exec) (1), overheat (sensor) (1))` |
| `examples/linux-container` | `linux-container` (`runc`) | finished, exit 0, as smoke. The control daemon ran as root in a privileged container with `sshfs` on the host's engine (the host has neither `sshfs` nor `user_allow_other`), its state shared with the host (`rshared`) so the actor container could bind the workspace mount |
| `examples/macos-vz` (profile, chaos) | `macos-vz` | finished, exit 0; `python3` and `/usr/bin/arch` routed to the Mac; chaos as on Firecracker, the sensor fault included |
| `examples/macos-apple-container` (profile, chaos) | `macos-apple-container` | finished, exit 0; gateway `example.com 200`, `example.org` 403; chaos: `broken.json` EIO, `curl` exit 7 (no sensors: a `profile check` warning) |

### Candidate next slices

- **Services without `HTTP_PROXY`**: an in-actor resolver and transparent
  listener for `*.needy`, so programs that ignore `HTTP_PROXY` (Node's
  `fetch`) reach services too.
- **Pushing the route list to a running actor**: done 2026-11-13 (see the
  chaos section).
- **The report's route names after `chaos add`**: the report's "kept on
  actor counts only the names the actor routes" note lists the boot list
  (`actor.config`); a `chaos.add` that widened the list is in the trace but
  not in that note.
- **Sensor faults at run time** (`TODO(chaos-sensor)`), applied at once
  through a guest call.
- **Header injection on HTTPS** (`TODO(mitm)`): an opt-in, per-host MITM with
  its own CA.
- **`file` faults on more operations** (`read`, `write`, `rename`), on the
  same SFTP filter.
- **A user-owned tmpfs home in `linux-container` executors**, whose `HOME`
  is root-owned today.
- **A `cwd` inside `actor.workspace`**, if a profile needs to start
  somewhere other than the workspace root.
- **`reset` on `macos-vz`** (`TODO(macos-vz-reset)`): vfkit 0.6.4 keeps the
  guest's side of a gateway connection open after the gateway closes it.

## Open questions

- The `unknown` default for launchers: `sticky` or an executor.
- Workspace concurrency mode defaults for shared executors (`shared`,
  `overlay`, `worktree`).
