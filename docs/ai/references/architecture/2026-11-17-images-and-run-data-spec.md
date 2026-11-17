# Image definitions, hand edits and merged run data

Status: approved feature contract synthesized from the owner discussion of
2026-11-16 and 2026-11-17, revised the same day after three reviews (design,
codebase feasibility, alignment with the discussion) and the owner's answers
to them. It turns two decided designs into one contract:
[images as layers](2026-11-16-environments-and-snapshots-design.md) and
[where a run's data lives](2026-11-16-data-placement-design.md). Where this
contract and those designs differ, this contract holds; the differences are
listed under [Further Notes](#changes-from-the-designs). Nothing in it is
implemented; it does not claim shipped support.

## Problem Statement

A user who wants an agent in a needy actor, at a version of their choosing,
has to leave needy to get it there. They write a Dockerfile `FROM` needy's
base by hand, with checksums and an architecture switch per download, run
`docker build`, run `needy image build --from` again after every rebuild for
a VM driver, or move the image into Apple's store by hand. Changing one
version repeats all of it. Something learned inside a running actor, such as
a package that had to be installed, is lost when the run ends, because the
only way to keep a change is to put it back into the Dockerfile.

Toolchains the Dev Container ecosystem already packages (node, python, git
and others) cannot be used; every user writes their own install steps.

Agent state is worse. Keeping a login across runs takes a copy-in and a
mirror-out per agent, a `mkdir` on the right machine, and a filter. Two runs
of one agent at once overwrite each other's state when they mirror out: a
run that never touched `auth.json` writes its stale copy back over the one
another run just refreshed. A `run stop --now` or a crash loses the run's
state entirely, because it lives on the actor's tmpfs until the driving
program exits. Sharing the state live instead loses writes, because several
harnesses keep their sessions in SQLite, and SQLite does not survive two
kernels writing one database over a network file system
([evidence](2026-11-16-agent-state-on-live-mounts-evidence.md)).

Finally, the workspace has to be spelled out in every profile, even in the
common case where the user runs needy inside the project they want the
agent to work on.

## Solution

The user describes what an actor runs on as an **image definition**: a root
image and an ordered list of layers (Dev Container features, used as they
are, and scripts), plus hooks. `actor.image` takes such a definition, or an
image reference as today. needy builds each layer once, caches it by
content, and runs boot from the resulting **snapshot**, read-only, on every
actor driver. Changing a version rebuilds only that layer and what follows.
`needy run create` builds and pulls whatever is missing, the base VM image
included, so it alone takes a user from nothing to a running agent.

The user can also change an image by hand: `needy image edit` opens a
writable, unrouted shell in it, and leaving the shell **commits** the change.
needy records commits in the image's lockfile, never in the file the user
wrote, keeps them as one layer on top of the built ones, and keeps their
history so `needy image rollback` can undo one.

Agent state is no special concept. It is a file mapping in a new `sync`
mode: at the start of a run its store is copied into a per-run copy on the
host, which the actor uses live; when the run ends, however it ends, the
per-run copy is **merged back** into the store, three-way and one run at a
time, with an `on-conflict` choice for the few files where both sides
changed. needy itself knows no file format: a format that needs more (a
harness's SQLite sessions, say) gets a **merge driver** that the harness's
feature supplies, as git's merge drivers work, run in a sandbox. Concurrent
runs of one agent keep each other's sessions, a stale copy never overwrites a fresh
one, and a hard stop loses nothing. A feature may declare the mappings its
harness needs, so a profile using needy's agent features writes none.

The workspace defaults to the folder the user runs needy in, passed through
to the actor live, never copied, whenever the machine that runs the
commands can reach it.

## User Stories

### Getting started

1. As a new user, I want installing needy to leave its daemon running, so that I do not have to learn about services before my first run.
2. As a new user on Linux, I want the one-time host setup (sshfs, fuse) stated once, so that I know the only manual step.
3. As a new user, I want `needy run create -it --profile builtin:pi` to work in a fresh install, so that I can try an agent without cloning the repository; a built-in profile carries its image definition inline (needy ships no built-in images).
4. As a user of a VM driver, I want a missing base VM image to be pulled by `run create`, with the same checks `needy image pull` makes, so that my first run does not fail on a missing download.
5. As a user who prepares machines ahead of time, I want `needy image pull` to keep working, so that I can pull before going offline or update a base deliberately.
6. As a user, I want `run create` to build an image's missing layers by itself and show progress, so that I never run a separate build first.
7. As a user, I want every later run of an unchanged image to start without building, so that runs are as fast as today.
8. As a user who wants to build ahead of time, I want `needy image build`, so that the first run of a new image is fast too.

### Describing an image

9. As a user, I want to write an image as a root and a list of layers in YAML, so that I never write a Dockerfile to add tools.
10. As a user, I want to use official Dev Container features by reference, with their options, so that I reuse maintained install scripts for node, python, git and the like.
11. As a user, I want to pin a feature to a major, minor or exact version, so that I choose how much it may move.
12. As a user, I want a `run` layer that runs a shell script as root, so that I can install what no feature covers.
13. As a user, I want a local feature directory to work like a published one, so that I can develop my own feature next to my project.
14. As a user, I want `actor.image` to keep accepting an image reference, so that images I built by hand keep working.
15. As a user, I want a path in `actor.image` that does not exist to be an error, not a registry pull, so that a typo is reported as a typo.
16. As a user, I want to name an image file by path, or write the definition inline in my profile, so that I can keep images where I like.
17. As a user, I want an image file to `extends` another of mine and add layers after the parent's, so that variants of my images share one base description.
18. As a user, I want a child layer that names the same feature as a parent layer (same reference, any tag) to replace it in place, so that I can bump one version of a parent image.
19. As a user, I want hooks to append along `extends` too, so that a parent's setup still runs.
20. As a user, I want a lockfile written next to my image file on first resolution, so that every host and every later build resolves the same feature versions.
21. As a user, I want changing a layer to rebuild only that layer and the ones after it, so that a version bump is fast.
22. As a user, I want images that share a prefix of layers to share those layers on disk, so that variants cost little.
23. As a user, I want a layer to be the same whichever driver built it, so that the cache is shared across drivers on one host.
24. As a user, I want the user my profile names to exist while features install, so that features give files to the user my runs use.
25. As a user, I want a different user ID to mean a different image, so that file ownership in the image always matches the run.
26. As a user, I want a feature's `containerEnv`, including its `PATH` additions, applied to my runs, so that the programs it installed are found.
27. As a user of a container actor, I want programs that features installed outside the default `PATH` to be routed like any other, so that rules apply to them.
28. As a user, I want a feature that asks for privileged mode, extra capabilities, security options or mounts to be refused with its name and the property, so that an image can never widen its actor.
29. As a user, I want a feature that declares an entrypoint to be refused with a clear message, so that I know why sshd or nix does not work yet.
30. As a user, I want a root image without sshfs to work anyway, so that any common Linux image can be a root.
31. As a user, I want a recipe's old `os.image` to be an error that points me to `root:`, so that the machine identity and the software are never confused.
32. As a user, I want build steps to reach the network freely and every host they reached to be recorded, so that installs work and I can see what they downloaded.
33. As a user, I want build steps to run as root, so that any install script works.
34. As a user, I want run-time network policy to stay exactly as my profile says, so that build-time freedom never reaches the agent.

### Editing an image by hand

35. As a user, I want `needy image edit` to give me a writable shell in the image my profile uses (or a named image file), so that I can try installs interactively.
36. As a user, I want nothing routed in an edit session, so that what I run there changes the image and not an executor.
37. As a user, I want leaving the edit session to commit my change, so that I keep it without rebuilding.
38. As a user, I want my commits recorded by needy in the image's lockfile, never in the file I wrote, so that my comments and layout stay mine.
39. As a user, I want all my commits kept as one layer on top of the built layers, so that many edits never slow an actor's boot and built layers stay shared.
40. As a user, I want `needy image rollback` to undo my last commit, and to go back further, so that a bad edit is one command to undo.
41. As a user, I want my commits stored as my data, not as cache, so that clearing needy's cache never destroys a hand edit.
42. As a user, I want my commits kept when a layer below them changes (a version bump, a lockfile update, a different UID), applied on the new base with a warning that names them, so that an upgrade does not throw my edits away.
43. As a user, I want only one edit session per image at a time, so that two edits never fight over one image.
44. As a user, I want `needy image log` to show each layer (a feature's reference, resolved version and options; a script's first line; a commit's commands) and the hosts each reached, so that a hand-made change stays explainable.
45. As a user, I want `needy image export` to write an OCI image, so that I can share an edited image with someone else.

### Running

46. As a user, I want every run to boot the same immutable snapshot and lose its own changes to the image when it ends, so that runs stay disposable.
47. As a harness author, I want a run to record the digest of the snapshot it booted, in its stored profile and its trace, so that two runs with one digest are known to have had the same file system.
48. As a user, I want image definitions to work on `linux-firecracker`, `macos-vz`, `linux-container` and `macos-apple-container`, so that switching drivers changes one line.
49. As a user of `macos-apple-container`, I want a built image loaded into Apple's store for me, so that I never `docker save` by hand.
50. As a user of a container actor, I want my image's shells to stay routed by their absolute paths, so that harnesses that run `/bin/bash` stay routed.
51. As a user, I want two runs of an image that is still building to wait for one build, so that nothing is built twice.
52. As a user, I want running runs to keep the snapshot they booted while I edit or roll back, so that a change never pulls the floor from under a run.

### Workspace

53. As a user, I want the folder I run `needy run create` in to be the workspace when the machine running the commands can reach it, so that the common case needs no configuration.
54. As a user, I want the workspace passed through live, with one copy on the executor, so that the agent's own file edits and its routed commands always see the same files.
55. As a user whose commands run on a remote executor, I want `run create` to fail and ask for `workspace.from` with a path on that machine, so that needy never silently works on the wrong folder.
56. As a user, I want the run's output to say which machine and folder the workspace is, so that I can check it at a glance.

### Agent state and other synced files

57. As a user, I want my agent's login to persist across runs without writing any mapping, when I use needy's agent features, so that I log in once.
58. As a user, I want to run several sessions of one agent at once, so that I work the way I do on my own machine.
59. As a user, I want each run's changes merged back when it ends, one run at a time, so that concurrent runs do not overwrite each other.
60. As a user, I want a file only one run changed to be taken from that run, so that a token refresh in one run is never lost to another.
61. As a user, I want a file two runs both changed to follow the mapping's `on-conflict` choice (keep the store's, take the run's, or merge lines), and otherwise to keep the store's and report the run's, so that I never lose a change silently.
62. As a user, I want conflicting copies kept by needy outside the store and named in `needy run report`, so that my agent never loads a conflict copy as real state.
63. As a user of a harness that keeps sessions in SQLite, I want its feature's merge driver to merge the databases, so that sessions from concurrent runs all appear in the next one, without needy knowing SQLite.
64. As a feature author, I want to supply a merge driver for my harness's files (given the seed, the run's copy and the store's current state as three trees), so that a format needy does not know merges correctly.
65. As a user, I want a merge driver to run in an actor, never on the host: in the run's own actor at a normal end, or in a short merge session from the run's image when that actor is gone, so that a feature's code cannot touch my machine and a normal end costs no extra boot.
66. As a user, I want a failing or hanging merge driver (it has a time limit) to leave the store's files as they were and keep the run's, reported, so that a broken driver loses nothing and never holds up a run's end.
67. As a user, I want caches and logs left out of syncing with the mapping's ordinary `filter`, so that I learn no second exclusion syntax.
68. As a user, I want a run stopped with `--now`, crashed, or ended while the store was unreachable to lose nothing, its changes merged later, so that a hard stop does not lose a login.
69. As a user, I want a pending merge for a store done before any new run of that store is seeded, so that a new run never starts from stale state.
70. As a user, I want to use a second account of an agent by pointing a run at another store, so that work and personal logins stay apart.
71. As a feature author, I want to declare my harness's synced files and their conflict choices in the feature, so that every profile using the feature gets them right.
72. As a user, I want a mapping in my profile to replace a feature's mapping of the same name, and two mappings claiming one actor path to be an error, so that overrides are explicit.
73. As a user, I want plain `copy-in` and `mirror-out` to keep their meaning, so that existing profiles behave as before.
74. As a user, I want a store under needy's own data directory to be created when it is missing, so that I never `mkdir` for needy.
75. As a user, I want `needy run report` to say where a store is (which machine and account), so that I know where my logins live.

### Hooks

76. As a user, I want a `start` hook that runs in every run after the workspace and synced files are in place and before the driving program, so that I can prepare a task.
77. As a user, I want a failing `start` hook to fail the run before the driving program starts, so that a broken setup is obvious.
78. As a user, I want a `stop` hook that runs before synced files are merged back, so that a harness can flush what it writes late.
79. As a user, I want a failing `stop` hook recorded and the merge still done, so that state is not lost to a hook.
80. As a user, I want hooks to run as my user in the workspace with the profile's rules applied, so that they behave like anything else the agent runs.
81. As a user, I want features' hooks to run before mine, in layer order, so that a feature's setup comes first.

### Reinforcement-learning style use

82. As a harness author running many short runs, I want setup cached in layers rather than repeated per run, so that each run starts fast.
83. As a harness author, I want to name the environment of my episodes by snapshot digest, so that a batch is reproducible.
84. As a harness author, I want variants that differ in one layer to share every layer before it, so that a matrix of images is cheap.

### Operating needy

85. As a maintainer, I want the base ABI unchanged, so that every base image of version 1 keeps working with this needy.
86. As a maintainer, I want images built by the earlier shim step and plain image references to keep working, so that nobody must rebuild to upgrade needy.
87. As a maintainer, I want build steps, commits and merges recorded as trace events, so that `needy run report` and `needy image log` can explain them.
88. As a maintainer, I want built layers in needy's cache directory and commits, stores and pending merges in its data directory, so that the cache alone is safe to delete.
89. As a maintainer, I want layers that a running run, a lockfile or a commit history names never evicted, so that cleaning up never breaks a run or an image.

## Implementation Decisions

### Vocabulary

- **Image definition**: a root (an image reference, or a Dockerfile build
  as executors' `build:` has it) plus an ordered list of layers, `env` and
  hooks (E0). `docs/TERMS_AND_CONCEPTS.md` gets the terms with the first
  slice that ships them.
- **Layer**: one entry of a definition and the file-system diff it produces.
  Kinds a user writes: `feature`, `run`. Commits are a layer needy keeps.
- **Commit**: a user's hand edit, made in an edit session. All of an image's
  commits are kept as one layer on top of its built layers.
- **Snapshot**: a booted stack only: the base, the root's layers, the
  needy layer, the built layers and the commit layer, named by the digest of
  that stack. Immutable; runs boot it. "Snapshot" no longer names a layer
  kind.
- **Session**: an internal primitive: an actor booted from a snapshot with
  three settings, the upper layer (disposable, or committed), routing (on or
  off) and the ending (discard, or commit). A run, a build step and an edit
  session are its three uses; users see only runs, `image build` and
  `image edit`.
- **Sync mapping**: a file mapping in `mode: sync`.
- **Feature**, **file mapping**, **trampoline**, **actor**, **executor**,
  **run**, **profile** and **recipe** keep their existing meanings.

### Configuration (`needy-config`)

- `actor.image` is decided by its syntax, not by what exists: a string that
  starts with `./`, `../` or `/`, or ends in `.yaml`, is a path to an image
  file and must exist; a map is an inline definition; any other string is
  an image reference, as today. needy ships **no built-in images**: a
  `builtin:` string in `actor.image` is an error that suggests a feature
  layer (owner, 2026-11-17). Agents are features, and `builtin:` names only
  recipes and profiles.
- A definition without `root:` uses needy's **default root**, a plain
  distribution image pinned by digest with its provenance, to which needy
  adds its own layer and the sshfs feature (E8) like any root. It is a
  default, not a named image.
- An image file is its own document kind, loaded with the one merge model:
  `extends` names another image file by path; unknown keys are errors. An inline definition in a
  profile merges along the profile's `extends` with the image rules below.
- The merge model gains keyed append: `layers` and each `hooks.*` list
  append along `extends` (parent first); a child layer whose feature
  reference equals a parent layer's, ignoring the tag, replaces it at the
  parent's position (E1).
- A recipe's `os.image` is removed; using it is an error naming `root:`
  (E2). The two built-in recipes that set it change with it.
- `files` mappings gain `mode: sync` with an optional `on-conflict` map
  from globs to `store`, `run`, `lines` (text, append-only) or a merge
  driver (`{ driver: COMMAND }`, provisional). Exclusions use the existing
  `filter`. needy knows no file format. Two mappings claiming one actor
  path are an error; a mapping name declared by two features is an error
  unless the profile defines that name; a profile's mapping replaces a
  feature's of the same name.
- `actor.workspace` stays; when it is absent the daemon resolves the
  default (below).

### Resolving and planning a build (`needy-core`, no IO)

- A compiled image definition, the resolved feature metadata, the root's
  digest and the profile's user produce an ordered **build plan**: one step
  per layer, each with a cache key (parent digest plus the step's resolved
  input: feature digest and options, or script text), the commit layer on
  top, and the image's final `env`, including `PATH`.
- Feature interpretation follows the Dev Container features specification:
  option names sanitized to environment variables with defaults filled in;
  `dependsOn` inserted before the dependent; `installsAfter` checked against
  the list order (a contradiction is an error naming both); `containerEnv`
  applied in layer order with `${VAR}` expanded; feature hooks mapped to
  needy's hooks.
- Refusals: `privileged`, `capAdd`, `securityOpt`, `mounts` and
  `entrypoint` refuse the feature with its name and the property (E3);
  `init` is ignored; `customizations.needy.files` yields sync mappings.
- A root without sshfs gets needy's sshfs feature as its first layer (E8).
- The profile's user is created in the root before the first layer and is
  part of every cache key (A3).
- A commit layer whose base changed (a lower layer, the lockfile, the user)
  is applied on the new base and reported as such (owner decision,
  2026-11-17).

### The layer store (new deep module)

- Interface: put a layer from a stream and get its digest; resolve a
  snapshot; **materialize** a snapshot for a driver, which returns what the
  driver boots (a list of disks with their markers, or an engine image
  name) and caches whatever format that needs; commit an edit session's
  upper layer into an image's commit layer; roll an image's commit layer
  back; list; collect garbage.
- One canonical form: an OCI layer tar. Driver formats (SquashFS for the VM
  drivers, OCI images for the container drivers) are derived and cached
  inside the store; no caller sees whiteouts, formats or flattening.
- Whiteouts are converted inside the store in both directions. Opaque
  directories are expanded into whiteouts of the lower entries they hide,
  because the SquashFS writer writes no extended attributes.
  `TODO(images)`: file capabilities and other extended attributes are not
  carried; revisit with a writer that keeps them.
- Built layers live in needy's cache directory, keyed by content. An
  image's commit layer and its commit history live in needy's data
  directory, because they cannot be rebuilt. Garbage collection never
  removes a layer that a live run, a lockfile or a commit history names.
- Commits collapse: each commit merges the new edit into the image's single
  commit layer and appends the previous one to the history; built layers
  below stay separate and shared (E6).
- A layer is the same whichever driver built it; a check keeps that true.
- Committed layers stay where they were made; sharing goes through export
  (E7). With a remote daemon, that is the daemon's machine.

### Building and editing (daemon and guest)

- `run create` resolves `actor.image` to a snapshot, building missing steps
  first and pulling a missing VM base (E9), with progress on the CLI (the
  owner's expectation that `run create` does it by itself, 2026-11-16).
  One build runs per cache key; other creates wait for it.
- Build steps and edit sessions run on the host's VM driver when it has
  one. On a host without one, they run on `linux-container`, whose engine
  commits the session's changes; the engine client gains commit, export and
  load. Apple's `container` is never used to build: a Mac has `macos-vz`.
- A build step runs as root, network open and traced (B1, B2), the
  feature's files copied in, its options written as the specification says.
  A failing step fails the build with its output.
- At the end of a VM session the guest archives its upper layer and streams
  it to the daemon; the guest API gains a streaming upload for it, since
  layers can be hundreds of megabytes.
- Each build step, commit and the hosts it reached are trace events.
- One edit session per image at a time. A commit is written to the image's
  lockfile by the CLI, which owns the user's files; the image file is never
  rewritten. Rollback moves the lockfile's commit pointer back. Runs keep
  the snapshot they booted.

### Booting a snapshot

- **VM drivers.** The boot disk stays needy's base (it carries the boot
  stub). The driver attaches the image's layers as read-only virtio-blk
  disks after the guest disk: the root's layers, the needy layer, the built
  layers and the commit layer. Each layer disk carries a marker naming its
  digest, which init reads after mounting it, so no contract depends on
  attach order. **The guest needy's init**, not the boot stub, stacks them
  with overlayfs right after it receives its configuration and before it
  sets up the home, the account and the identity, moving the mounts it
  already made and keeping the guest disk and its bind at `/.needy`, so
  needy's own programs are still never routed. A run's upper layer is a
  tmpfs; a session's is too in slice 1. `TODO(images)`: a host-backed
  scratch disk for sessions larger than the guest's memory.
- The base ABI stays at 1. The layer disks after the guest disk are a
  contract between the daemon and the guest needy of the same version.
- The guest configuration gains the layer list, the session settings and
  the image's `PATH`: a Protobuf change to the guest API, made in `proto/`
  with Buf and regenerated.
- **Container drivers.** A snapshot is an OCI image assembled from the
  root's layers, the needy layer and the snapshot's layers, loaded into the
  engine, or into Apple's store with `container image load` (E11), under a
  needy-owned name per snapshot digest. The shim step runs on every
  container materialization, so in-place shells stay shimmed whatever a
  commit changed.
- **`PATH`.** The image's `PATH` is the driving program's, after the
  trampolines and before the route stubs; init writes it under `/run/needy`
  so a trampoline finds its real program on it at exec time, and a
  container actor's trampolines are made over it. The profile's
  `actor.env.PATH`, when set, replaces the image's.

### Workspace default

- The CLI sends the folder `run create` runs in; `CreateRunRequest` gains
  that field (a Protobuf change).
- With no `actor.workspace`, the daemon uses that folder, mounted at
  `~/workspace` in the actor and passed through live, when the executor
  that runs the commands can reach it: a native executor on the creating
  machine (the client-execution holder serves it), or a `linux-container`
  executor on the same machine, which binds it (on a Mac, through the
  engine's file sharing). Otherwise the create fails and asks for
  `workspace.from`. There is no guard for the home directory or `/`
  (owner decision, 2026-11-17). The run's output names the workspace.
- The daemon derives the workspace's mappings and path map for the default
  as the config crate derives them for an explicit `actor.workspace`.

### Sync mappings

- At the start of a run, the daemon copies the store into a per-run copy in
  its data directory and keeps the run's seed. The actor uses the per-run
  copy live: in a VM through the same pass-through mount as a workspace, in
  a container through a bind. One run writes each per-run copy, so its
  SQLite databases have one writer, which works over the mount (cell A of
  the live-mount evidence). The per-run copy survives any end of the actor:
  a `--now` stop, a crash, the store being unreachable.
- When a run ends, however it ends, the daemon merges its per-run copy into
  the store under an exclusive lock held on the store's machine, one run at
  a time per store, three-way per entry against the run's seed. Only-run
  changes are taken. For an entry both sides changed, the mapping's
  `on-conflict` choice applies (`store`, `run`, or `lines` for append-only
  text); without one, the store's copy stays and the run's is kept by
  needy, per run, outside the store, and reported.
- **Merge drivers.** A glob whose `on-conflict` names a driver is merged by
  it, not by needy: needy hands the driver every file the glob matches as
  three trees (the seed, the run's copy, the store's current state), so it
  also handles side files such as a database's write-ahead log, and takes
  back the merged files for the store. needy's own merge skips the files a
  driver claims. A driver is a command a feature ships (a mapping a feature
  declares names it). It runs inside an actor, never on the host, so a
  feature's code has its harness's own tools and versions:
  - at a normal end, in the run's own actor, after the driving program and
    the `stop` hook and before the actor shuts down, with routing and
    network turned off for it, and the three trees passed through as the
    workspace is;
  - when the run's actor is gone (a `--now` stop, a crash, a store that was
    unreachable), in a short merge session booted from the run's snapshot,
    with no routing and no network.
  The daemon holds the store's lock while the driver runs. A driver that
  fails or exceeds its time limit leaves the store's files unchanged, keeps
  the run's, and is reported. needy's opencode feature ships an SQLite row-merge driver (three
  way per row by primary key; tables keyed by an autoincrement integer are
  reported, not merged), which the merge spike showed works.
- A merge that cannot run yet stays pending. Pending merges for a store run
  at daemon start and before any run of that store is seeded, under the
  same lock. A seed and a per-run copy are removed after their merge.
- The interim store is the daemon account's data directory on the control
  host; `run report` says so. The client's machine follows the client-side
  file transfer (`TODO(runner-files)`). needy's features use
  `files/<name>` under needy's data directory by convention.

### Hooks

- `start` runs in every run after the workspace and sync mappings are in
  place and before the driving program; `stop` when the driving program
  ends or a graceful stop, before merges. Features' hooks first, in layer
  order. Hooks run as the user in the workspace as ordinary actor processes,
  so rules route what they start. Build-time work is a `run` layer.
  `TODO(images)`: an `attach` hook, deferred until a case needs it.

### CLI

- New in slice 1: `needy image edit [IMAGE] [--profile P]`, `needy image
  build [IMAGE] [--profile P]`, `needy image rollback [IMAGE] [--profile P]
  [@N]`. `IMAGE` is an image file path (provisional); `--profile` names the
  image a profile uses, inline definitions included, built-in profiles'
  too.
- Later: `needy image log` and `needy image export` (`TODO(images)`).
- Changed: `needy run create` builds and pulls what it needs; `needy run
  report` lists merge conflicts, pending merges, stores and the snapshot
  digest.
- Unchanged: `needy image pull`, `ls`, `rm`; plain `copy-in` and
  `mirror-out`; image references in `actor.image`.

## Testing Decisions

A good test drives needy through the highest seam that can show the
behavior and asserts what a user or a harness would observe: a run's
outcome, its trace and report, a file's contents in a store, a command's
exit code, the actor's view of its file system. Tests do not assert
internal helpers, and no export, dependency bag or alternate path is added
only to make internals mockable. Every bug fix starts with a failing
reproduction.

The four seams, highest first (agreed with the owner on 2026-11-17):

1. **The e2e harness** (`cargo xtask e2e`), new `image` and `sync`
   scenarios on every driver: an edit session commits, a new run sees the
   change, a rollback removes it; features install and their programs are
   found and routed (trampolines over the image's `PATH`); two concurrent
   runs of one agent merge their state and a third resumes both sessions;
   a `--now` stop's state is merged before the next run is seeded. Prior
   art: the `routing`, `actor-files`, `tui` and `fake-agent` scenarios, and
   the self-path and `ldd` invariants added with trampolines.
2. **The daemon in process**, through `needy-api-server`'s domain traits
   with a native executor and a stand-in VM driver: the snapshot a run
   boots and the layers and `PATH` in its guest configuration; one build per
   cache key; the workspace default and its error; merges of a per-run copy
   into a store, with conflicts and pending merges in the report and the
   lock serializing two runs. Prior art: the guest configuration tests
   (hostname, working directory), the stop tests, and the files tests.
3. **Config loading**: `actor.image` in each form and the syntax rule,
   keyed append along `extends`, the `os.image` error, `mode: sync`
   validation and mapping conflicts, feature-declared mappings. Prior art:
   the profile and recipe loading tests and the workspace derivation tests.
4. **Two deep modules, through their own interfaces**: the layer store
   (content addressing, cache keys, materializing, opaque directories
   expanded to whiteouts, commit collapse and rollback, garbage collection
   that keeps named layers) and the three-way merge (each `on-conflict`
   choice, the merge-driver handoff of three trees and the files it
   claims), on temporary directories. The opencode feature's SQLite driver
   is tested as that feature's own code, with a write-ahead log present. Prior art: the
   trampoline tests and the shim step tests, which run on Linux in a
   container where the code is Linux-only.

The guest's Linux-only code (stacking layers, archiving the upper layer) is
tested on Linux: `cargo test` in a Linux container on a Mac, natively on
Linux hosts, and end to end through the harness.

## Out of Scope

- Reading `devcontainer.json`, Dev Container templates and Docker Compose
  (A1).
- Features that declare an entrypoint, and features that ask for privileged
  mode, capabilities, security options or mounts (E3).
- Pushing or pulling committed layers through a registry; sharing goes
  through export (E7).
- Image definitions for executors; a later slice (E10).
- Managed workspaces, named folders needy keeps for work without a project
  folder; deferred (`TODO(workspaces)`).
- An `attach` hook (`TODO(images)`).
- The client's machine as a store before the client-side file transfer
  exists (`TODO(runner-files)`).
- A key-level merge of JSON application-state files (`TODO(state-merge)`).
- Any file format in needy's core; formats are merge drivers that features
  ship. SQLite tables keyed by autoincrement integers, in the opencode
  driver.
- Extended attributes and file capabilities in layers (`TODO(images)`).
- Building on Apple's `container`.
- Memory snapshots for fast reset.
- Programs installed into the system `PATH` while an actor runs.
- Windows actors.

## Further Notes

### Slices

Each slice needs the owner's approval when it starts and ends with its
evidence recorded.

| # | Slice | Depends on |
| --- | --- | --- |
| 0 | Evidence: run the official `common-utils`, `git`, `node` and `python` features by hand in an actor; no product code | nothing |
| 1 | Snapshots and hand edits on the VM drivers: the layer store, layer disks with markers, stacking in the guest init, the session primitive, streaming upload, `image edit`, `image build`, `image rollback`, commits in the lockfile; layers of kind `run` only | 0 |
| 2 | The same for the container drivers (engine commit, export and load; Apple's store) | 1 |
| 3 | Features as layers: references, options, order, `containerEnv` and `PATH`, refusals, the lockfile's features, the user in the image, needy's sshfs feature, building and pulling in `run create` | 1, and 2 for hosts without a VM driver |
| 4 | Hooks (`start`, `stop`) | 3 |
| 5 | The needy layer on any root; the default root; needy's agents as features; `examples/agents` moves to them | 3 |
| 6 | Built-in profiles, each with its image inline; `image log`, `image export` | 5 |
| I1 | The installers start the daemon (C1) | nothing |
| M1 | Sync mappings: per-run copies on the host, seeds, the three-way file merge with `on-conflict`, the lock, pending merges, the interim store | nothing |
| M2 | Merge drivers: in the run's own actor at a normal end (routing and network off, a time limit), in a merge session otherwise; the three-tree handoff; needy's opencode feature ships its SQLite driver | M1, and 3 for the feature |
| M3 | The current folder as the default workspace (the CLI sends its folder) | nothing |
| M4 | Features declaring sync mappings | 3, M1 |

### Preparing slice 0

- Host: this Apple silicon Mac (`macos-vz`, `macos-apple-container`) and a
  Linux KVM host through `cargo xtask e2e --remote` when one is available.
- Fetch the four features at a pinned commit of `devcontainers/features`;
  in an actor booted from today's base with a writable root, write each
  feature's options file and run its `install.sh` as root with the
  `_REMOTE_USER` variables set, in dependency order.
- Record per feature: exit status; the files it wrote outside `/usr/local`;
  its `containerEnv` and whether its programs are found with and without it
  on `PATH`; ownership for the run's user; network hosts reached; any
  extended attributes or file capabilities it set (the layer format drops
  them).
- Confirm the `PATH` gap the trampolines design predicts.
- Write the result as an evidence note beside this spec; a change it forces
  to slices 1 or 3 comes back here first.

### Preparing slice 1

- Seams to touch: the VM drivers' disk lists; the guest configuration and a
  streaming upload in `proto/` (use the `use-buf` skill); the guest init's
  boot sequence (stacking after `GetConfig`, moving earlier mounts, keeping
  `/.needy`); the image module beside the existing VM and guest disk code;
  the CLI's `image` command family and the lockfile.
- First failing tests, in order: a daemon test that a run of a two-layer
  snapshot gets its layer list in the guest configuration; layer store
  tests for put, digest, opaque directories and commit collapse; a guest
  test that stacking two layers over a root shows the upper file and hides
  a whiteout; an e2e cell that edits, commits, boots and rolls back on
  `macos-vz`.
- Measure on `macos-vz` and `linux-firecracker` and record: boot time with
  zero, one and eight layer disks; whether eight layer disks plus the guest
  disk fit Firecracker's device limit on x86_64; tmpfs use of an edit
  session that installs a large package.

### Changes from the designs

- The layer kind `snapshot:` is gone; commits are recorded in the lockfile
  and kept as one commit layer with a history (owner, 2026-11-17).
- A commit whose base changed is applied with a warning, not refused.
- `actor.state` and `back: merge` are replaced by `mode: sync` with
  `on-conflict`; `ignore` is the mapping's `filter`; conflict copies stay
  outside the store (owner, 2026-11-17).
- needy's core knows no file format: the SQLite row merge moves out of it
  into a merge driver that a feature ships, given three trees, run in the
  run's own actor at a normal end and in a merge session booted from the
  run's image when that actor is gone (owner, 2026-11-17).
- A sync mapping's per-run copy lives on the host, so a hard stop loses
  nothing (owner, 2026-11-17); the data placement design had accepted that
  loss for `--now`.
- The workspace default has no home or `/` guard (owner, 2026-11-17).
- needy ships no built-in images (no `builtin:agents`, no
  `builtin:minimal`): agents are features, a definition without `root:` gets
  a pinned default root, and getting started without a clone goes through
  built-in profiles that carry their image inline (owner, 2026-11-17).
- The `attach` hook is deferred; "the same feature" means the same
  reference without its tag (owner, 2026-11-17).
- Slice 1 ships `image edit`, `build` and `rollback`; `log` and `export`
  move to a later slice (owner, 2026-11-17).
- From the codebase review: the boot disk stays needy's base and a user's
  root is a lower layer; layer disks are found by a marker, not by order;
  opaque directories become whiteouts; layers stream to the daemon;
  container sessions commit through the engine; the image's `PATH` is
  written under `/run/needy` for trampolines.

### Risks

- Third-party features assume a build container; some may need what an
  actor lacks (slice 0 finds out).
- Layers carry no extended attributes; a feature that sets file
  capabilities loses them.
- An edit session's upper layer is in guest memory in slice 1.
- Firecracker on x86_64 may cap the number of block devices below a long
  image's layer count (slice 1 measures it).
- A merge driver depends on its harness's format; a format change between
  harness versions can make it fail, which leaves the store unchanged and
  is reported, not hidden.
- A driver after a hard stop or a crash costs a short merge session; a
  normal end runs it in the run's own actor at no extra boot.
- A per-run copy of agent state over the pass-through mount is slower than a
  local disk for harnesses that write their state often; the e2e `sync`
  scenario measures it.
