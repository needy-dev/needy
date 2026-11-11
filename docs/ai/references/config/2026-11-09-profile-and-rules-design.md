# Profiles and rules

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

user: { name: agent, uid: 1000, gid: 1000 }   # same on the actor and every executor; ~ = /home/agent

actor:
  runtime: firecracker           # required: firecracker (a microVM, needs /dev/kvm) | container
  # container: { oci_runtime: runsc, image: needy-actor:latest }   # required with `container`: runc | runsc
  runner: kvm                    # optional; the control host when absent, see Runners
  recipe: gce-e2-standard-4
  limits: { single_process: true, self_replace: true }
  network:
    allow: [openrouter.ai]
    inject:                      # added by the gateway; never visible in the actor
      openrouter.ai: { header: Authorization, secret: OPENROUTER_KEY }

executors: { … }                 # see Executors

executor_network:
  allow: [registry.npmjs.org, pypi.org]
  record: true

presets: { … }                   # see Composition
commands: [ … ]                  # see Commands
files: [ … ]                     # see File mappings

chaos:
  seed: 42
  faults:
    - { on: file.open, match: "/workspace/**/*.json", errno: EIO, probability: 0.01 }
    - { on: net.connect, match: registry.npmjs.org, delay: 2s, nth: 3 }
    - { on: hwmon, set: { temp_c: 101 }, at: 5m }

trace: { sinks: [sqlite, jsonl], timing: false }   # see Trace
```

A `network.allow` entry is a glob matched against the host name in any
case, and `*.example.com` also matches `example.com` itself
(`Glob::matches_host`, which the actor gateway and the profile share).

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

Each fault fires from the profile's `seed`, its index and how many times it has
matched (`nth`, `probability`), so a replayed run injects the same faults.

| `on` | Subject matched by `match` | Effect |
| --- | --- | --- |
| `exec` | The command as typed, word by word like command rules (`curl **`) | `errno` fails the command before it starts: `needy: fault injected: ERRNO`, exit 126; `delay` holds it first |
| `net.connect` | The host, at the actor gateway | `errno` returns 502; `delay` sleeps first |
| `file.open` | The path relative to an actor mount | The SFTP `open` gets an error reply; sshfs reports `ENOENT`, `EACCES`, `EPERM`, `EOPNOTSUPP` or, for anything else, `EIO` |
| `hwmon` | — | At `at`, the fake coretemp reads `set.temp_c` |

Every injected fault is a `fault.injected` trace event that names its kind,
its target (the command, host, path or sensor) and its effect. `exec` faults
are the second step of [rule evaluation](#evaluation).

An `exec` fault applies in the actor as a rule does: its pattern's command
names join the actor's route list (`profile check` prints it as `route
names:`), so `{ on: exec, match: "curl **", errno: ECONNREFUSED }` fails
`curl` in the actor even when no rule names `curl`. An `exec` fault without
`match`, or whose pattern may match any program, routes every exec.

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
| Other | `network`, `faults`, `timeout`, `deny: MESSAGE` | — |

A routed command's program keeps its path. When an executor's `path_map`
maps `argv[0]`, the command runs from the mapped path. Any other absolute
`argv[0]`, such as the actor's `/usr/bin/python3`, is sent as it is, and the
executor runs it from that path when the path exists there; otherwise it runs
the program of the same name from its own `PATH`. When neither exists, the
error names the original path. A `path_map` key is an actor path: a leading
`~` in it is the profile user's home, as on a file mapping's actor side; a
value is a path on the executor's machine and keeps a `~` as written. A command placed with `--executor`, or run
without a run, is never rewritten and gets no `PATH` fallback.

### Composition

Every rule that matches applies, in order:

- **Single-valued fields take the first rule that sets them**: `to`, `deny`,
  `timeout`, `cwd`. Reading from the top tells you where a command goes.
- **Collections accumulate in rule order**: environment changes, `args.prepend`,
  `args.append`, `wrap` (outermost first), `files` and `faults`.
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
   `exec` faults are checked against the command as typed. An `errno` turns
   the outcome into a fault; a `delay` keeps the placement and the daemon
   waits before answering. Each fired fault becomes a `fault.injected` event.

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
  workspace:
    mode: mount
    from: executor:build:~/workspace/
    to: actor:~/workspace/              # same path on both sides
    writable: true
  codex-sessions:
    mode: mirror-out
    from: actor:~/.codex/sessions/
    to: host:./runs/{run}/codex/
    when: continuous                    # continuous | end
    delete: false                       # never propagate deletions to the host
  ignore_file: .needyignore             # optional gitignore-style file, read as "-" rules

run_files: [skills, codex-home, workspace, codex-sessions]

commands:
  - match: "{xcodebuild,codesign} **"
    to: mac
    files: [workspace-mac]              # added to run_files for this command
```

- Mappings are **named**. A run lists its mappings; a command rule may add
  more. They accumulate as a union.
- Sides are written `host:`, `actor:` and `executor:<name>:`. `~` is
  `/home/<user>` on every side, and every side uses the profile's UID and GID,
  so no ownership mapping or `chown` is needed.
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
- Harness home directories contain credentials, such as Codex's `auth.json`.
  Model keys are injected by the actor gateway, so credentials must never be
  copied into the actor; otherwise an agent that replaces its harness can read
  them. Harness presets exclude known credential files by default.
- Mirrored-out directories are stored with the trace.
- The driving program starts in `actor.cwd`. Without one it starts in
  `~/workspace` when a `mount` mapping provides that directory, and in `~`
  otherwise: the actor's home is a fresh tmpfs, so `~/workspace` exists only
  when a mount creates it.

## Open questions

- The `unknown` default for launchers: `sticky` or an executor.
- Workspace concurrency mode defaults for shared executors (`shared`,
  `overlay`, `worktree`).
