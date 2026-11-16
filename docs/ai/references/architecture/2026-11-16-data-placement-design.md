# Where a run's data lives

Status: **design, every open question decided** (2026-11-17); not yet
implemented. Proposed 2026-11-16: user data stays with the account that
started the run, and needy keeps the directories it owns under XDG base
directories. Names marked *provisional* are not stable.

Independent of the [environments design](2026-11-16-environments-and-snapshots-design.md);
either can land first.

## Why

A profile names directories on three machines, and a user has to know which
machine each one is on:

| Written as | On | Created by |
| --- | --- | --- |
| `host:~/needy-state/pi` | the **control host**, the daemon's machine, which need not be the machine the CLI runs on | the user; a missing copy-in source fails the run (`readable` in `crates/daemon/src/files.rs`) |
| `executor:build:~/workspace`, a native executor's `workspace: /tmp/needy-demo` | the executor's machine | the user (`mkdir` in the README and examples) |
| `actor:~/.pi/agent` | inside the actor | needy |

Refusing to create a missing `host:` path is deliberate:
`TODO(runner-files)` in `crates/daemon/src/files.rs` says `host:` paths are
resolved on the control host, not on the client that sent the profile, so
"a path the control host does not have fails the run instead of copying
nothing or creating it". The problem is not a missing `mkdir`; it is that a
profile has to name a path on a machine the user may not be on.

Keeping an agent's login across runs takes thirteen lines today
(`examples/agents/pi.yaml`: a copy-in, a mirror-out with a filter, and
`run_files`) and a `mkdir`; `examples/agents/herdr.yaml` repeats every other
agent's mappings because `extends` takes one parent.

## Principle: three kinds of data

| Kind | Examples | Lives on | Lifetime |
| --- | --- | --- | --- |
| **User data** | the workspace, an agent's state and credentials | the machine and OS account that **started the run** (the client), under that account's XDG directories | the user's |
| **Cache** | images, layers, snapshots, features, guest disks | the machine that uses it (an actor's or executor's host) | reproducible; deletable at any time |
| **Control records** | `state.db`, `trace.db`, run records, logs | the control host | needy's retention |

After a run ends, an actor's host keeps no user data, only cache. Isolation
between users then comes from the operating system: a client executor's
holder runs as the invoking account ([client executor](../../../TERMS_AND_CONCEPTS.md#client-executor-approved-contract)),
so each account's runs read and write that account's directories, with that
account's permissions, and needy does not implement per-user isolation of
its own.

> **REVIEW:** a trace on the control host can hold user data too (argv,
> paths, the hosts a run reached). This design does not move traces; whether
> they need a placement rule of their own is a separate question.

## The workspace defaults to the current directory

When `actor.workspace.from` is absent and the profile has a client executor
(`runner: { client: true }`, `crates/config/src/profile/runner.rs`), the
workspace is the CLI's current directory on the client, mounted live into
the actor through the existing `from: "executor:NAME:PATH"` mechanism:

```text
client (CLI and client executor)          control host (daemon and actor)
~/src/my-project  ◀── sshfs, live ──────  ~/workspace in the actor
       ▲                                         │
       └──── routed commands run here, in this directory ◀──┘
```

```sh
cd ~/src/my-project
needy run create -it --profile builtin:pi
```

No directory is created anywhere: the files stay on the client, as with
`docker run -v "$PWD":…` or a devcontainer's local workspace folder.

Guards:

- The current directory is refused when it is the account's home or `/`,
  with a message to `cd` into a project or set `actor.workspace`. A
  writable home directory handed to an agent is not a default.
- A profile without a client executor has no way to reach the client's
  directory. Its default is a [managed workspace](#managed-workspaces) on
  the executor that `workspace` names, and the run's output says so; it
  does not silently copy the current directory.
- The run's output always names the workspace: machine, path, writable or
  not.

> **Decided 2026-11-17: the current folder when it can be reached, else an
> error.** When the commands run on the machine `needy run create` runs on
> (a native executor there, or a local container that can bind it), the
> workspace is the current folder, passed through to the actor; when they
> run on a remote executor, the folder is on the wrong machine and
> `run create` fails and asks for `workspace.from` with a path on that
> machine. Nothing is copied: the workspace is passed through, one copy on
> the executor. The question was:
> Which client executor is the default workspace's
> when a profile has several, and is a profile with none but with an
> executor on the client's own machine a case to handle?

## Agent state: generic file mappings that merge back

Decided with the owner on 2026-11-17: needy has **no agent-specific state
concept**. An agent's login and sessions are files that are copied in when a
run starts and brought back when it ends, which `files` mappings already
express; what is new is how they come back (a merge, below) and that a
feature may declare them. The earlier `actor.state` proposal is dropped.

```yaml
files:
  pi:
    mode: copy-in
    from: "host:~/.local/share/needy/state/pi/"     # the store (see below)
    to: "actor:~/.pi/agent/"
    back: merge                                     # new (provisional): merged back at the end
    rules: { newest: [auth.json], ignore: ["bin/**"] }
```

- **`back: merge`** (*provisional*) replaces a separate mirror-out for
  such a mapping: at the end the actor's copy is merged into the source
  three-way ([below](#two-runs-one-state-seed-then-merge)), with the
  mapping's rules. A plain `copy-in` and a plain `mirror-out` keep their
  meaning.
- **Features declare mappings** (decided 2026-11-17): a feature may carry
  mappings with their rules (`customizations.needy.files` in its
  `devcontainer-feature.json`, the specification's place for a tool's own
  properties), so a profile using needy's pi feature writes none. A
  profile's mapping of the same name replaces the feature's.
- A mapping's store path is the profile's (or the feature's) to name;
  `$XDG_DATA_HOME/needy/state/<name>/` (*provisional*) is the convention
  needy's features use, which needy creates when it is missing, since it is
  needy's own directory.

Where the store is:

| Step | Store | Needs |
| --- | --- | --- |
| target | the client's directory, copied in and merged back through the client's holder | a client-side file transfer, which `TODO(runner-files)` waits for |
| interim (decided 2026-11-17, ships first) | the control host's needy data directory, created by the daemon before `check_host_paths` | nothing new |

The profile is the same for both, so moving from the interim store to the
target changes no profile.

### Two runs, one state: seed, then merge

Decided with the owner on 2026-11-16, after the
[live-mount evidence](2026-11-16-agent-state-on-live-mounts-evidence.md) and
the [merge spikes](2026-11-16-agent-state-merge-spike-evidence.md). Rejected
alternatives: one run holding a state at a time (it forbids several
sessions of one agent, which is how agents are used); a live mount of the
store (three of ten surveyed harnesses keep sessions only in SQLite, which
loses writes across VMs, and most harness locks are kernel-local); and
today's last mirror-out wins (a run that never touched `auth.json` writes
its stale copy back over a refreshed one).

Each run's state lives in its actor, on the actor's own disk, where every
harness runs as it was built to:

1. **Seed.** At start, the store as it is now is copied in, and needy keeps
   that seed until the run's state is merged.
2. **Merge.** When the run ends, needy merges the run's final state into
   the store under an exclusive lock held on the store's machine, one run at
   a time, three-way per entry: the seed, the run's copy, the store's copy.
   An entry only the run changed is taken; one both changed is a conflict:
   the store keeps its own, the run's is kept beside it, and `needy run
   report` names it.
3. **Keep until merged.** A run that ends without a merge (`run stop --now`,
   a crash, an unreachable store) keeps its final state, and the merge runs
   when it can, instead of the change being lost.

Per-entry rules, which a feature declares for its harness (open question
3) and a profile may add to:

| Rule | Meaning | Spike examples |
| --- | --- | --- |
| default | three-way as above | session transcripts, one file per session |
| `ignore` | never merged | caches, logs, locks, SQLite a harness rebuilds from files |
| `append` | the lines a run appended to an append-only file are appended | `history.jsonl` |
| `newest` | the newer copy wins | `auth.json` after a token refresh, `models_cache.json` |
| `keep` | the store's copy wins once set, without a conflict | Codex's `installation_id` |

For a harness that keeps sessions in SQLite, a database is merged by rows
instead: three-way per row by primary key, with one writer on the store's
copy. The spike merged two opencode runs' sessions that way. A harness
whose tables use autoincrement keys needs its own merge adapter that
renumbers rows.

What it costs: running sessions do not see each other's changes (a login
made in one run reaches another at its next start), a stop takes the time
of its merge, and two runs that continue the same session produce a
conflict rather than one transcript.

`TODO(state-merge)`: a key-level merge of JSON application-state files
(`.claude.json` holds set-once identity beside caches) is deferred; the
whole-file `newest` rule is enough once the store exists, since every seed
then carries the same identity.

## Managed workspaces

> **Deferred 2026-11-17 by the owner:** the current folder covers the
> common case and `workspace.from` names any other. `TODO(workspaces)`: a
> named folder needy keeps on an executor, for work without a project
> folder (scratch tasks, RL batches), waits for a case that needs it. The
> rest of this section is the deferred proposal.

```yaml
actor:
  workspace: { path: ~/workspace, name: demo }
```

`name:` instead of `from:` asks for a directory needy owns on the
executor's machine (`$XDG_DATA_HOME/needy/workspaces/<name>/` of the
account the executor runs as, *provisional*). The executor creates it,
since it is the needy process on that machine and knows its own data
directory; nothing has to be predicted from elsewhere.

> **Deferred with managed workspaces (see above).** It asked: a managed workspace's lifetime: kept by name until
> `needy workspace rm`, or removed with its run as an executor instance
> keyed `{run}` is?

## What does not change

- A path the user writes (`from: "executor:mac:~/src/x"`, `host:./data`)
  is the user's: needy never creates it, and today's checks stay.
- Cache and control records keep their places in `crates/cli/src/paths.rs`
  (`$XDG_DATA_HOME/needy` for images, `$XDG_STATE_HOME/needy` for
  `trace.db`, runs and logs), which already uses XDG base directories
  through `etcetera` on Linux and macOS.

## XDG layout

Existing directories are from `crates/cli/src/paths.rs`; new entries are
*provisional*.

| Directory | Holds | Kind |
| --- | --- | --- |
| `$XDG_CONFIG_HOME/needy` | the global document, profiles by name | user configuration (existing) |
| `$XDG_STATE_HOME/needy` | `trace.db`, runs, the daemon log | control records (existing) |
| `$XDG_DATA_HOME/needy` | images | cache (existing) |
| `$XDG_DATA_HOME/needy/state/<name>` | agent state | user data (new) |
| `$XDG_DATA_HOME/needy/workspaces/<name>` | managed workspaces | user data (new) |
| `$XDG_CACHE_HOME/needy` | layers, features (environments design) | cache; reserved today with no reader (`TODO(config)` in `paths.rs`) |
| `$XDG_RUNTIME_DIR/needy` | sockets | runtime (existing) |

## Open questions

All decided on 2026-11-17: the default workspace (the current folder when
reachable), the interim store (the control host first), features that
declare their mappings (yes), two runs and one store (seed, then merge),
managed workspaces (deferred).

## Slices

In order; each needs the owner's approval when it starts.

1. **Merging mappings back** (`back: merge` with `ignore`, `append`,
   `newest` and `keep`): the lock on the store, keeping an unmerged run's
   state, the interim store on the control host. `examples/agents` uses it
   for Codex, pi and opencode.
2. **The SQLite row merge**, and its first harness (opencode).
3. **The current folder as the default workspace**, with the guards above.
4. **The client-side store**, after the client-side file transfer of
   `TODO(runner-files)`.
