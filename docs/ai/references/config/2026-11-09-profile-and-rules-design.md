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
  `unknown` and `executor:NAME:` sides, and file mappings in `session_files`
  and a rule's `files:`. Each problem is an error finding. `needy profile
  check` prints the findings and exits 1 on an error; session create refuses
  the profile.

## Profile structure

A profile describes one session. Machines are described by
[recipes](2026-11-09-recipe-design.md) and referenced as `builtin:NAME` or by a
path relative to the profile.

```yaml
extends: base.yaml               # optional parent profile; see Extends

user: { name: agent, uid: 1000, gid: 1000 }   # same on the actor and every executor; ~ = /home/agent

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

## Chaos

Each fault fires from the profile's `seed`, its index and how many times it has
matched (`nth`, `probability`), so a replayed session injects the same faults.

| `on` | Subject matched by `match` | Effect |
| --- | --- | --- |
| `exec` | The command, word by word like command rules (`curl **`) | `errno` denies the command with `needy chaos: ERRNO`; `delay` sleeps first |
| `net.connect` | The host, at the actor gateway | `errno` returns 502; `delay` sleeps first |
| `file.open` | The path relative to an actor mount | The SFTP `open` gets an error reply; sshfs reports `ENOENT`, `EACCES`, `EPERM`, `EOPNOTSUPP` or, for anything else, `EIO` |
| `hwmon` | — | At `at`, the fake coretemp reads `set.temp_c` |

Every injected fault is a `fault.injected` trace event.

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

### Extends

`extends` names a parent: a path relative to the extending file, or
`builtin:NAME` (there are no built-in profiles yet). A parent may extend
another; a chain is at most 16 deep and may not cycle. Profiles and recipes
share one merge model, applied to the documents before they are typed:

- Maps merge key by key, recursively. A child that sets
  `executors: { build: { max_procs: 8 } }` keeps the parent's other `build`
  fields and its other executors.
- A child's scalar or list replaces the parent's: `session_files`,
  `actor.network.allow`, a rule's `args`.
- A child's `null` (`~`) removes the parent's value, so the field takes its
  default.
- `commands` and `resolvers` are prepend lists: the child's entries come
  first, then the parent's, so the child's rules are evaluated first.

Relative paths resolve against the directory of the profile that was loaded,
including paths a parent wrote.

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
| `copy-in` | Host → actor or executor | Session start | `.agents/`, `AGENTS.md`, Codex configuration |
| `mount` | Live view; the executor holds the data | During the session | The workspace |
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
    to: host:./sessions/{session}/codex/
    when: continuous                    # continuous | end
    delete: false                       # never propagate deletions to the host
  ignore_file: .needyignore             # optional gitignore-style file, read as "-" rules

session_files: [skills, codex-home, workspace, codex-sessions]

commands:
  - match: "{xcodebuild,codesign} **"
    to: mac
    files: [workspace-mac]              # added to session_files for this command
```

- Mappings are **named**. A session lists its mappings; a command rule may add
  more. They accumulate as a union.
- Sides are written `host:`, `actor:` and `executor:<name>:`. `~` is
  `/home/<user>` on every side, and every side uses the profile's UID and GID,
  so no ownership mapping or `chown` is needed.
- Filters follow rclone: ordered `+` and `-` rules, `**` for any depth, a
  leading `/` anchored at the mapping root.
- Harness home directories contain credentials, such as Codex's `auth.json`.
  Model keys are injected by the actor gateway, so credentials must never be
  copied into the actor; otherwise an agent that replaces its harness can read
  them. Harness presets exclude known credential files by default.
- Mirrored-out directories are stored with the trace.

## Open questions

- The `unknown` default for launchers: `sticky` or an executor.
- Workspace concurrency mode defaults for shared executors (`shared`,
  `overlay`, `worktree`).
