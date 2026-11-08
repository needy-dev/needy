# Profiles and rules

**Status**: draft, under discussion. The matching syntax and composition rules
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
| Recipe identity | Yes, but static | Fixed when the session starts. |
| Did the program probe its environment? | No | The whole trace answers better. |
| Reward hacking, reading answers | No | Needs context before and after. |
| Did the final answer get each dimension right? | No | Naturally after the fact. |
| Model A vs. model B, regressions | No | Spans sessions. |

Run-time rules are therefore declarative only: no scripts and no LLM calls on
the syscall path. Each decision is a function of the profile, the seed and the
event, so a session can be replayed exactly. After-the-fact analysis runs
outside needy on the trace (see the
[CLI note](../cli/2026-11-09-cli-exec-and-report-design.md#trace)).

A later escape hatch may add an `ask` action that pauses the syscall, calls an
external decision service and records the answer in the trace, so replay reuses
the recorded answer. It is off by default.

## Format

- The schema is defined in `proto/`, so Buf can also generate TypeScript types
  for people who prefer to author profiles in code and emit JSON.
- On disk a profile is YAML or JSON.

## Profile structure

A profile describes one session. Machines are described by
[recipes](2026-11-09-recipe-design.md) and referenced by name.

```yaml
extends: base-linux              # optional parent profile

actor:
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
```

## Executors

Needy does not schedule and does not track live resource use. Resource limits
belong to each runtime's own options. Needy keeps a pool of executor instances
indexed by key and a counter per instance.

```yaml
executors:
  build:
    runtime: docker
    docker:
      build:                              # image build: docker build options
        context: ./env
        dockerfile: Dockerfile.build
        target: toolchain
        args: { RUST_VERSION: "1.97" }
      args: [--cpus=4, --memory=8g, --pids-limit=512]   # run options: docker run
    key: "{session}"
    max_procs: 4          # matched commands running at once per instance
    on_full: wait         # wait | new | reject
    max_instances: 16
    idle_timeout: 10m

  vm:
    runtime: firecracker
    recipe: gce-e2-standard-4             # same machine as the actor
    key: "{session}"
    max_procs: 2

  mac:
    runtime: remote                       # another machine's needy daemon; nothing to build
    remote: { url: https://mac-mini.local:7331 }
    key: "{rule}"
    max_procs: 1
    on_full: wait
```

Runtimes: `native` (this machine), `docker`, `containerd`, `firecracker`, `k8s`
and `remote`. `native` and `remote` machines are managed by their owners and
have no build step.

### Instance key

Commands whose key is equal share one instance; different keys get separate
instances.

| Key | Effect | Use |
| --- | --- | --- |
| `{session}` (default) | One instance per session | RL: sessions are isolated |
| `{profile}` | Shared by every session of a profile | Shared build caches during development |
| `{workspace}` | Shared per workspace | Several sessions on one repository |
| `{rule}` | Shared per matching command rule | One Mac VM for every `xcodebuild` |
| Combinations, such as `{session}:{cwd}` | Finer split | — |

Commands that operate on one workspace must land on one instance, or at least on
one volume. A key may use only deterministic inputs, so that a session can be
replayed.

### Full instances and limits

| `on_full` | Behaviour |
| --- | --- |
| `wait` | Queue until a slot frees; the command is slower. |
| `new` | Start another instance with the same key. Becomes `wait` at `max_instances`. |
| `reject` | Return an error. |

`idle_timeout` stops unused instances. Per-instance runtime limits multiplied by
`max_instances` bound what one executor can take from its host.

### Builds

`build` options and run options are separate: `docker.build.args` are build
arguments, `docker.args` are `docker run` arguments. Builds are cached by the
hash of their inputs. The resolved image digest is written to the trace, so a
session can be reproduced after `latest` moves.

### Pass-through limits

Runtime options pass through, except fields the daemon owns:

- **Set by the daemon, never overridden**: `--network` (the executor network and
  gateway), the workspace mount, `--name`, `--label`.
- **Denied unless the daemon's operator configuration allows them**:
  `--privileged`, `--cap-add`, `--device`, `--pid host`, mounting the host root.

containerd options are merged into the OCI spec as a JSON patch with the same
fields protected.

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
  script: "**/pi/dist/cli.js"     # glob on a captured argument
  cwd: /workspace/**
  env: { CI: "1" }
  under: "pnpm test **"           # an ancestor process matches
  tty: true
```

Launchers such as `bash -lc '…'`, `pnpm dlx`, `npx` and `node script.js` hide
the command that really runs. Built-in launcher resolvers turn them into an
effective command first, and `match` applies to that.

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

### Composition

- **Rules are ordered; the first match wins**, like a firewall. Reading from the
  top gives the result without reasoning about merges.
- **Presets** are named blocks of actions reused with `use:`, like CSS classes.
- **`extends`** inherits a parent profile; its rule list is appended after the
  child's rules.

```yaml
presets:
  clean-env: { env: { clear: true, pass: [PATH, HOME, LANG, TERM] } }
  mac:       { to: mac }

commands:
  - match: pnpm dlx pi **         # the harness itself stays on the actor
    to: actor
  - match: "{node,bun} {script} **"
    when: { script: "**/pi/dist/cli.js" }
    to: actor
  - match: "{uname,lscpu,sw_vers} **"   # identity commands stay on the actor's recipe
    to: actor
  - match: curl **
    when: { has: [-k, --insecure] }
    deny: insecure TLS is not allowed
  - match: git **
    to: build
    args: { prepend: [-c, color.ui=never] }
  - match: "{xcodebuild,codesign} **"
    use: [mac, clean-env]
  - match: "**"                   # everything else goes to the build executor
    to: build
    use: [clean-env]
```

## File mappings

| Kind | Direction | When | Example |
| --- | --- | --- | --- |
| `copy-in` | Host → actor or executor | Session start | `.agents/`, `AGENTS.md`, Codex configuration |
| `mount` | Live remote view | During the session | The workspace: commands run on the executor, the actor views it remotely. |
| `mirror-out` | Actor or executor → host | Continuously or at the end | Session directories: `~/.codex/sessions`, `~/.claude/projects` |
| `exclude` | Never transferred | — | Credential files |

```yaml
files:
  - { copy-in: .agents/, to: actor:/home/agent/.agents/ }
  - { copy-in: ~/.codex/, to: actor:/home/agent/.codex/, exclude: [auth.json] }
  - { mount: executor:/workspace, at: actor:/workspace }
  - { mirror-out: actor:/home/agent/.codex/sessions/, to: ./sessions/{session}/codex/ }
```

Home directories of harnesses contain credentials, such as Codex's `auth.json`.
Model keys are injected by the actor gateway, so credentials must never be
copied into the actor; otherwise an agent that replaces its harness can read
them. `copy-in` supports `exclude`, and harness presets exclude known credential
files by default.

Mirrored-out session directories are stored with the trace.

## Open questions

- YAML, TOML, or TypeScript that emits JSON as the primary authoring format.
- Whether first-match-wins is enough, or some actions (environment, files)
  should accumulate across matching rules.
- Which launcher resolvers ship built in, and how users add one.
- Whether `files` rules can also be attached to individual command rules.
