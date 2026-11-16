# Environments as layers

Status: **pending design**, proposed 2026-11-16. Direction agreed with the
owner on 2026-11-16: needy keeps its own configuration (it does not read
`devcontainer.json`), an environment is written directly as a list of
**layers**, and the toolchains the Dev Container ecosystem already packages
(its **features**) are used as they are, as one kind of layer. Nothing in
this note is approved for implementation, and nothing is implemented. The
[open questions](#open-questions) need the owner's decisions first. Names
marked *provisional* are not stable.

## Owner decisions (2026-11-16)

| # | Decision |
| --- | --- |
| A1 | needy does not read `devcontainer.json`; it borrows its concepts. |
| A2 | An environment is a root and a list of layers; Dev Container features are used as they are, as one kind of layer. |
| B1 | Network is open during builds, and traced. |
| B2 | Build steps run as root without restriction. These privileges and B1 exist at build time only. |
| C1 | Installing needy activates its daemon, as Docker and containerd packages do: the installer runs the service installation (`needy daemon service install --now`), so no separate step starts it. |
| C2 | `sshfs` and `fuse3` stay a one-time host prerequisite on Linux. |

## Why

Putting a custom version of an agent (opencode, pi) into an actor today
takes, after [actor images](2026-11-12-actor-images-design.md) steps 1 to 4:

1. a base image (`needy image build --linux-container` in a needy checkout,
   or `needy image pull`);
2. a Dockerfile `FROM` the base that installs the agent by hand (runtime,
   checksums, an architecture `case`) and ends with
   `RUN ["/.needy/shim-image"]`; an image that leaves that last step out
   still runs, but a container actor does not route its added programs;
3. `docker build`;
4. per driver: `needy image build --from REF` again after every rebuild
   (a VM image does not follow its tag), or `docker save` and
   `container image load` for Apple's `container`;
5. a profile that repeats what the image cannot carry (`ENV` and `USER` do
   not reach the actor).

Every version change repeats steps 3 and 4. A change made inside a running
actor is lost when the run ends; only `files` mappings copy selected
directories back. The base itself carries an agent (`OPENCODE_VERSION` in
`images/Dockerfile`, about 200 MB; `TODO(rootfs)` there already names
moving it out), although needy has no agent-specific concepts. The example
profiles live only in the repository, so using them means a clone. needy
has no lifecycle hooks.

## Goals

- An **environment** is a root and an ordered list of **layers**. Each
  layer is built once, cached by content and shared by every environment
  with the same prefix. needy builds environments on every driver, without
  Docker for the VM drivers and without a needy checkout.
- Toolchains come from **Dev Container features** where one exists
  (`ghcr.io/devcontainers/features/node:1`, ...), run unchanged.
- An environment can be **edited in place**: enter it, change it, keep the
  change as a new layer, without a rebuild.
- **Lifecycle hooks** at the moments a needy run has.
- Runs stay **disposable**: every run starts from an immutable snapshot, as
  the [actor survival design](2026-11-15-actor-survival-and-history-design.md)
  requires ("The actor remains disposable").
- The base shrinks to what needy itself needs.
- Many short runs of one environment (reinforcement-learning style): an
  environment named by digest, and cheap variants that share a prefix.

## Non-goals

- Reading `devcontainer.json`. Its schema is built around one long-lived
  container that an editor attaches to, and around Docker's flags
  (`runArgs`, `privileged`, `capAdd`, `mounts` in `--mount` syntax). needy
  has an actor and executors, a recipe for identity, routing rules, YAML
  with `extends` and one merge model, and it rejects unknown keys. A partial
  reader would accept files that then behave differently from every other
  devcontainer tool.
- needy's own installation-unit format. A Dev Container feature is that
  unit; needy's agents are published as features too (see
  [automation](#automation)), so they also work in any devcontainer.
- Features that need more than an actor gives (see
  [what a feature may declare](#what-a-feature-may-declare)).

## Terms (provisional)

| Term | Meaning |
| --- | --- |
| **environment** | The software an actor runs on: a root, layers and hooks. Separate from the recipe, which is the machine's identity. It resolves to a snapshot. |
| **root** | The OCI image an environment starts from. |
| **layer** | One entry of an environment, and the file-system diff it produces (an overlayfs upper directory, convertible to an OCI layer). Kinds: `feature`, `run`, `snapshot`. |
| **feature** | A Dev Container feature, as its specification defines it: a directory with `devcontainer-feature.json` and `install.sh`, distributed as an OCI artifact. Third-party name, kept. |
| **snapshot** | A root plus an ordered stack of built layers, named by the digest of the stack. Immutable. A run boots a snapshot. |
| **edit session** | An actor started from a snapshot with a writable upper layer and no routing; ending it commits the upper layer as a new layer. |
| **needy layer** | `/.needy`: the boot stub, the shim stub, the guest-needy placeholder and the base ABI record. Today it comes from `FROM` the base image; in this design needy adds it to any root (see [the base](#the-base-becomes-a-layer)). |

`docs/TERMS_AND_CONCEPTS.md` gets these terms when the design is approved.

## Configuration

A profile names an environment as it names a recipe (`actor.recipe` in
`crates/config/src/profile/actor.rs`): `builtin:NAME`, a path relative to
the profile, or inline.

```yaml
# agents.env.yaml (provisional file name)
extends: builtin:minimal
root: ubuntu:24.04
layers:
  - feature: ghcr.io/devcontainers/features/node:1
    with: { version: "26" }
  - feature: ghcr.io/needy-dev/features/pi:1        # needy's agents, published as features
    with: { version: "1.5.2" }
  - run: |                                          # a script layer, like a Dockerfile RUN
      apt-get update && apt-get install -y --no-install-recommends jq
      rm -rf /var/lib/apt/lists/*
  - feature: ./features/task-deps                   # a local feature directory
  - snapshot: sha256:3f1c…                          # a layer committed by `needy image edit`
env:
  PI_SKIP_VERSION_CHECK: "1"
hooks:
  start: ["./scripts/prepare-task.sh"]
```

```yaml
# profile
actor:
  environment: agents.env.yaml    # or builtin:agents, or an inline map
  command: [pi]
```

| Layer kind | Builds | Cache key (with the parent snapshot's digest) |
| --- | --- | --- |
| `feature: REF` with `with:` | the feature's `install.sh`, as the features spec runs it | the feature's resolved digest and the options |
| `run: SCRIPT` | the script by `sh -c`, as root, without routing | the script's text |
| `snapshot: DIGEST` | nothing: a stored layer | the digest |

`root:` is an OCI image; `build: { context, dockerfile, target, args }`
(the shape of `ContainerBuild` in `crates/config/src/profile/executor.rs`)
builds one with an engine and BuildKit, as `needy image build --from` does
today. `actor.environment` and `actor.image` exclude each other;
`actor.image` stays for images built by hand.

A layer may carry `env:` for what it adds to the environment.

> **Open question 1.** In `extends`, a child's list replaces its parent's
> (`crates/config/src/merge.rs`); only a profile's `commands` and
> `resolvers` are prepended. A child environment usually adds layers after
> its parent's. Add an *append* rule for `layers` and `hooks.*`, or keep
> replacement and let a child repeat its parent's list?

> **Open question 2.** A recipe already has `os.image`, "the OCI image the
> rootfs is built from" (`crates/config/src/recipe/kernel.rs`). It is
> compiled into `os_image` (`crates/core/src/recipe/mod.rs:231`) but nothing
> reads it. Does the environment's `root` replace it, or does `os.image`
> stay as the root's default when the environment has none?

## Features

### Running a feature

needy follows the [features reference](#sources) for what a feature sees:

- **References**: an OCI reference (media types
  `application/vnd.devcontainers` and
  `application/vnd.devcontainers.layer.v1+tar`, semver tags `:1`, `:1.2`,
  `:1.2.3`), an `https:` tarball, or a local directory relative to the
  environment file.
- **Options**: `with:` values, plus the declared defaults for the rest,
  written to `devcontainer-features.env` next to `install.sh`; each name is
  sanitized as the spec says (`[^\w_]` to `_`, leading digits and
  underscores to `_`, upper case).
- **Execution**: `install.sh` runs as root, with its execute bit set and
  invoked directly, with `_REMOTE_USER`, `_CONTAINER_USER`,
  `_REMOTE_USER_HOME` and `_CONTAINER_USER_HOME` set (see
  [the user](#the-user-at-build-time)).
- **Order**: the environment's list order, with `dependsOn` (hard: needy
  inserts a missing dependency as its own layer before the feature) and
  `installsAfter` (soft: only between features that are both present). A
  list order that contradicts `installsAfter` is an error that names both,
  instead of a silent reorder.

What the official features expect of the root holds for a needy actor, as
read from `devcontainers/features@b6877129` (`src/{node,python,git,common-utils}/install.sh`,
retrieved 2026-11-16): each reads `/etc/os-release` and picks `apt-get`,
`apk`, `dnf` or `microdnf` itself, checks for root with `id -u`, and
downloads from `github.com` and `api.github.com`. This is reading, not a
run; slice 0 below runs them.

### What a feature may declare

| `devcontainer-feature.json` property | needy |
| --- | --- |
| `options`, `dependsOn`, `installsAfter` | as above |
| `containerEnv` | **applied** to the environment, in layer order, with `${VAR}` expanded against the environment so far. The official features need it: `node` installs under `/usr/local/share/nvm` and adds `PATH: /usr/local/share/nvm/current/bin:${PATH}`, and `python` does the same |
| `onCreateCommand` | a `run` layer after the feature |
| `updateContentCommand`, `postCreateCommand`, `postStartCommand` | the `start` hook, before the environment's own |
| `postAttachCommand` | the `attach` hook |
| `init` | ignored: needy's init is PID 1 everywhere |
| `privileged`, `capAdd`, `securityOpt`, `mounts` | the feature is **refused**, with the property named: an environment does not widen its actor (this rules out `docker-in-docker`) |
| `entrypoint` | refused at first (open question 3) |
| `customizations`, `legacyIds`, `deprecated` | ignored; `deprecated` is a warning |

### The user at build time

Features give files to `_REMOTE_USER`. The official `node` feature
(`src/node/install.sh` at the commit in [sources](#sources)) takes it as
`USERNAME` (L23), falls back to `root` without an error when no such user
exists (L270-L271), creates a group `nvm`, adds the user to it and makes
the user own `NVM_DIR` (L357-L368), then installs node as that user (L378);
`python` does the same with groups `python` and `pipx` (L859-L1023).

A needy actor's user is the profile's (`user: { name, uid, gid }`), written
into the root's passwd, group and shadow only when a run starts
(`needy_common::account`, through `needy-core-executor`'s `account` for the
container drivers and `guest/init/account.rs` in a VM). The rewrite
replaces only entries with the user's name, UID or GID, so groups a
feature created (`nvm`) and their member lists stay. Two failures follow
today:

- built before the user exists, a feature silently installs for `root`:
  the user can run `node` but `npm install -g` is refused, and the build
  reports nothing;
- built with the user under another UID than a later profile's, the files
  belong to a UID that is someone else at run time.

> **Open question 4.** Make the user (name, UID, GID) part of the
> environment: needy creates it in the root before the first layer and it
> is in every cache key, so a different UID is a different environment
> (A); build with a fixed user and remap ownership at run time (B); or let
> the environment declare its user and refuse a profile whose user differs
> (C)? Recommendation: A. Remapping means a `chown` of large trees at every
> start or idmapped mounts that not every driver has, and a profile's user
> rarely changes (every example uses `agent` 1000:1000).

### Lockfile

`environment.lock` next to the environment file. Its feature entries have
the shape of `devcontainer-lock.json` entries (`version`, `resolved` as the
digest-pinned reference, `integrity` as `sha256:` of the artifact,
`dependsOn`), keyed by the reference as written. needy writes it on the
first resolution and builds from it afterwards, so a locked environment
resolves to the same layers on every host. Local features and `run` layers
are hashed into their cache keys instead.

## Lifecycle hooks

Dev Containers have five moments because their container outlives a
start. A needy run creates its actor every time, so the moments collapse:

| Hook | When | Dev Container analogue |
| --- | --- | --- |
| (a `run` layer) | once per snapshot; cached | `onCreateCommand` in a prebuild |
| `start` | every run, after the workspace and `files` copy-ins, before the driving program | `updateContentCommand`, `postCreateCommand`, `postStartCommand` |
| `attach` | every `run create -it` and `run attach` | `postAttachCommand` |
| `stop` | when the driving program ends or `run stop` is graceful, before mirror-outs | none; needy's mirror-out needs a moment to flush state |

Each hook is a list; an entry is a string (run by `sh -c`) or an argv
array. Features' hooks run first, in layer order, then the environment's.
Hooks run as the user in the workspace, as ordinary actor processes: the
profile's rules route what they start and the trace records their execs.
A failing `start` hook fails the run before the driving program starts; a
failing `stop` hook is recorded and the mirror-outs still run.

Per-run hooks cost time in every run; for many short runs, put setup in a
layer, where it is cached.

## Network and privileges during builds

Decided by the owner on 2026-11-16 (B1, B2 in
[owner decisions](#owner-decisions-2026-11-16)):

- **Network is open during a build.** A layer downloads from wherever its
  script points; features declare no hosts, and an allowlist would fail
  most of them. Build steps run in an actor without routing, and its
  connections still go through the gateway, so every host a layer reached
  is traced and kept in the layer's history (`needy image log`).
- **Build steps run as root without restriction**: `install.sh`, `run`
  layers and edit sessions may install packages, create users and groups,
  and write anywhere in the root.

These apply to building only. A run keeps the profile's
`actor.network` policy, and a feature that asks its *runs* for privileged
mode, capabilities, security options or mounts is still refused (see
[what a feature may declare](#what-a-feature-may-declare)).

`layer.network.allow` is not part of the schema. `TODO`: a build allowlist,
if a profile ever needs to require one.

## Snapshots

### Layers as read-only disks

A snapshot is a root plus an ordered list of built layers. Each layer is
stored once, content-addressed, under needy's data directory
(`<data>/layers/sha256-<hex>/`, *provisional*), as a SquashFS image for the
VM drivers and as an OCI layer tar for the container drivers.

- **VM drivers.** Firecracker and `macos-vz` attach the root and each layer
  as read-only virtio-blk disks, as they already attach the guest disk (a
  SquashFS image, `crates/core-actor/src/driver/linux_firecracker/config.rs`).
  The boot stub mounts them and stacks them with overlayfs under a tmpfs
  upper layer. Both kernels have `CONFIG_OVERLAY_FS=y` and
  `CONFIG_SQUASHFS=y` (`images/kernel/firecracker-6.18.config` L2876 and
  L2943; `images/kernel/containerization-arm64.config` L3373 and L3451), so
  this needs no kernel change. There is no full rootfs copy per version,
  and it works on host file systems without reflinks.
- **Container drivers.** A snapshot is an OCI image whose layers are the
  snapshot's layers, loaded into the engine (`linux-container`) or Apple's
  store (`macos-apple-container`) under a needy-owned name
  (`needy-env:<digest>`, *provisional*).

Overlayfs and OCI layers express deletions differently (a 0/0 character
device against a `.wh.` file); the conversion between the two is one
component module, with tests in both directions.

> **Open question 6.** A long chain of layers costs one disk per layer in a
> VM. Flatten past a limit (merge the oldest layers into one), or flatten
> on every edit commit? Recommendation: flatten automatically above a
> fixed count; the count is a `NOTICE` with its measured reason.

### Edit sessions

```sh
needy image edit ENV      # a shell in the environment, writable, unrouted
# change anything; exit
needy image log ENV       # layers, with what each edit session ran
needy image rollback ENV @2
needy image export ENV    # an OCI image
```

An edit session is an actor booted from the snapshot with a writable upper
layer, without routing (every exec runs in the actor, as an image build's
`RUN` does today through the shim stub's fallback). Ending the session
commits the upper layer as a layer and appends `- snapshot: sha256:…` to
the environment's `layers` (or prints it, for a built-in environment), so
the file still says everything the environment is made of. One edit
session per environment at a time. The session's execs are already
traced; the layer's history keeps them, so `image log` shows what made
each one.

A `snapshot` layer is not reproducible from text; `needy image log` shows
its commands so that it can be turned into a `run` layer later.

> **Open question 7.** Where `snapshot` layers live for sharing: only in
> the local data directory, or pushed to a registry as OCI layers so an
> environment file with them works on another host?

### Shims

The VM drivers catch every exec in the kernel and need no shims. A
container actor routes only shims, so needy runs the shim step when it
commits a layer, over the programs that layer added on the environment's
`PATH`, as its `containerEnv` made it (the official `node` feature's
programs are under `/usr/local/share/nvm/current/bin`, outside Docker's
default `PATH`). It can no longer be left out (today's
`/.needy/shim-image`).

### Runs and digests

A run records the digest of the snapshot it booted, in its stored profile
and in the trace. Two runs with one digest boot the same file system. A
harness that starts many runs names environments by digest; a name is a
moving pointer for people.

## The base becomes a layer

The needy layer (`/.needy`) is static (musl C stubs, a placeholder and a
record) and depends on no distribution. Instead of requiring an image
`FROM` needy's base, needy adds the needy layer on top of any root when it
makes a snapshot. Then:

- `root:` may name any image (`ubuntu:24.04`, a devcontainer base image
  such as `mcr.microsoft.com/devcontainers/base`, a project's own);
- `builtin:minimal` is small: what the guest needy calls (`sshfs` and
  `fuse3` for the workspace, `guest/init/mounts.rs`; a `sh`) and
  `ca-certificates`;
- what the current base adds for other reasons becomes layers:
  - toolchains from official features where they exist (`git`, `node`,
    `python`, `common-utils`);
  - opencode and the other agents as needy's own features;
  - recipe fidelity (`dmidecode`, `lm-sensors`, `pciutils`, `systemd` for
    `systemd-detect-virt`) as a needy feature.

The minimal set comes from reading the guest's code. Removing each package
and running the e2e matrix decides it.

The kernel stays needy's: the VM drivers' base keeps `vmlinux` or `Image`,
Firecracker and the initramfs. The base ABI check moves from "the image's
label" to "the needy layer needy added", which needy always knows.

> **Open question 8.** A root without `sshfs` cannot mount a workspace.
> Add a needy feature for it when it is missing, or refuse the root?

## Automation

- **Pull on demand.** The first run or build pulls what is missing: the VM
  base (kernel, Firecracker or initramfs), the root image, features. This
  reverses D4 of the
  [actor images design](2026-11-12-actor-images-design.md#owner-decisions-2026-11-12)
  ("VM images are not downloaded automatically") and needs the owner's
  word. Integrity stays as `needy image pull` checks it today.
- **Built-in profiles and environments**, embedded as built-in recipes are
  (`crates/config/src/builtin/`), so `needy run create --profile builtin:pi`
  works without a clone.
- **needy's agents as features.** pi, Codex, herdr and opencode, from
  `examples/agents/Dockerfile`, become features published by needy's
  release (`ghcr.io/needy-dev/features/<id>`, *provisional*), with the
  provenance their pins carry today. They then also work in any
  devcontainer.

## Security

- A feature's `install.sh` is third-party code run as root inside an actor,
  which is the sandbox it runs in. The lockfile pins what runs.
- An environment cannot widen its actor: features that ask for privileged
  mode, capabilities, security options or mounts are refused.
- No hook runs on the host (Dev Containers' `initializeCommand` has no
  needy counterpart; `TODO` if a case needs it).
- Credentials stay where they are today (`TODO(mitm)`).

## Open questions

1. Append `layers` and `hooks.*` along `extends`, or replace?
2. The environment's `root` and the recipe's unread `os.image`.
3. Feature `entrypoint`: refuse, or run before the driving program?
4. The user at build time: part of the environment, or remapped?
5. ~~Network policy for build steps~~: decided, B1.
6. Flatten long layer chains: when?
7. Sharing `snapshot` layers: local only, or through a registry?
8. A root without `sshfs`: add a feature or refuse?
9. Reverse D4 (pull VM images on demand)?
10. Executors: should a `linux-container` executor take an `environment`
    too, so the project's toolchain is built the same way?
11. Apple's `container` store: load snapshots as OCI archives (`container
    image load`), which works today, or wait for a better path?

## Slices

In order; each needs the owner's approval when it starts.

0. **Evidence first**: run the official `common-utils`, `git`, `node` and
   `python` features in a needy actor by hand (a `linux-container` actor
   with a writable root, then `linux-firecracker`), with `_REMOTE_USER`
   set, and record what breaks, as an evidence note. It checks the premise
   of "features as they are" before any code.
1. **Snapshots and edit sessions** on `linux-firecracker`: layers as
   SquashFS disks, overlayfs in the boot stub, `image edit`, `log`,
   `rollback`. Layers of kind `run` and `snapshot` only.
2. The same for `linux-container`, then `macos-vz` and
   `macos-apple-container`; `image export`.
3. **Features as layers**: references, options, order, `containerEnv`,
   refusals and the lockfile.
4. **Lifecycle hooks** at run time, with the features' hooks.
5. **The needy layer on any root**, and `builtin:minimal`; needy's agents
   become features; `examples/agents` uses them.
6. **Automation**: pull on demand (after open question 9), built-in
   profiles and environments.

Slice 5 comes after 3 so that the examples keep working.

## Candidate next slices (not part of this design)

- **Memory snapshots for fast reset**: Firecracker can restore a paused VM;
  for runs measured in seconds, booting from a memory snapshot would skip
  the boot. Needs measurements first.

## Sources

The Dev Container specification, at
[`devcontainers/spec@c95ffeed`](https://github.com/devcontainers/spec/tree/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs)
(retrieved 2026-11-16):

- Lifecycle scripts:
  [`devcontainerjson-reference.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainerjson-reference.md)
- Environment creation, stop and resume:
  [`devcontainer-reference.md#lifecycle`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-reference.md#lifecycle)
- Features (metadata, options and their sanitizing, `install.sh`, user
  variables, order):
  [`devcontainer-features.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features.md)
- Feature dependencies:
  [`feature-dependencies.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/feature-dependencies.md)
- Feature lifecycle hooks:
  [`features-contribute-lifecycle-scripts.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/features-contribute-lifecycle-scripts.md)
- Distribution (OCI artifacts, semver tags, tarballs, local features):
  [`devcontainer-features-distribution.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-features-distribution.md)
- Lockfile:
  [`devcontainer-lockfile.md`](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/docs/specs/devcontainer-lockfile.md)

The official features read for what they expect of a root, at
[`devcontainers/features@b6877129`](https://github.com/devcontainers/features/tree/b6877129e0a264b8e83a63e8b93de29077132e5b/src)
(retrieved 2026-11-16): `node/devcontainer-feature.json` (`containerEnv`
with `PATH`), `python/devcontainer-feature.json`, and the `install.sh` of
`node`, `python`, `git` and `common-utils`.
