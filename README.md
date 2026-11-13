<picture>
  <source width="100%" srcset="./docs/assets/banners/hero-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/hero-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="Needy, an isolated sandbox for agent harnesses with seamless exec routing and policy: opencode runs in an actor, a Firecracker microVM on a Linux KVM host; its xcodebuild goes to a Mac executor and succeeds, and a rule denies its git push." src="./docs/assets/banners/hero-light.webp" />
</picture>

<h1 align="center">Needy</h1>

Needy separates the program that drives work from the environments where that
work takes effect.

- Run the driving program, such as an agent harness or a test suite, in a small
  stateless sandbox: the **actor**.
- Route its commands, file access and network traffic by declarative rules to
  **executors**: containers, microVMs or real machines such as a Mac.
- Record every operation, inject faults, and build each machine from a
  **recipe** that fixes its identity: OS, kernel, CPU, firmware and devices.

> [!NOTE]
> Needy is experimental (0.0.1). The first closed loop works: opencode runs in
> an actor while its commands run in Docker or on a Mac. See the
> [results](docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md).

## Table of Contents

- [Getting started](#getting-started)
  - [Install](#install)
  - [Start the control daemon](#start-the-control-daemon)
  - [Pick a profile](#pick-a-profile)
  - [Run it](#run-it)
  - [One command from your laptop](#one-command-from-your-laptop)
  - [Add machines](#add-machines)
- [Actor drivers](#actor-drivers)
- [Configuration](#configuration)
- [Examples](#examples)
- [How it works](#how-it-works)
- [Architecture](#architecture)
- [Documentation](#documentation)
- [Development](#development)
- [Acknowledgements](#acknowledgements)

## Getting started

The flow is always the same:

1. Install `needy` on every machine that takes part, and pull its images
   with `needy image pull`.
2. Start a **control daemon** on the machine that runs actors.
3. Write a **profile**: which actor driver, which executors, which commands go
   where.
4. `needy run create` starts a **run**: one actor and its executors.
5. Look at it with `needy report`, `needy exec`, `needy ps`.

<picture>
  <source width="100%" srcset="./docs/assets/banners/getting-started-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/getting-started-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="Getting started in five steps: install needy and pull its images, start the control daemon, check a profile with a linux-firecracker actor and a rule that sends python3 to a build container, run bash in the actor where python3 runs in the container, and read needy report." src="./docs/assets/banners/getting-started-light.webp" />
</picture>

### Install

No release has assets yet. Prebuilt binaries and images come with the next
release, and the channels below work from then on; until then, build from a
checkout. `needy image build` needs the checkout too (it builds the actor
and executor images from `images/`).

#### From source

```sh
git clone https://github.com/needy-dev/needy && cd needy
cargo install --path crates/cli          # installs `needy`
make guest                               # bin/needy-linux-<arch>, with Docker
cp bin/needy-linux-* "$(dirname "$(command -v needy)")/"
```

Actor images hold no needy: the control daemon brings its own **guest
needy** into every actor it starts, a static Linux build of its own version.
It looks for `needy-linux-<arch>` next to its executable (or takes
`--guest-needy PATH`); a static Linux release `needy` is its own guest.

#### macOS

```sh
brew install needy-dev/tap/needy
needy --version
```

Alternatively, without [Homebrew](https://brew.sh/):

```sh
curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
```

Both install the guest needy next to `needy` on Apple silicon.

#### Linux

```sh
curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
needy --version
```

> [!NOTE]
> The script checks the archive's `.sha256` before it installs anything. Set
> `NEEDY_VERSION` to a release tag or `NEEDY_INSTALL_DIR` to change the
> release or the install directory (default: `~/.local/bin`). `latest` is the
> newest release that is not a prerelease. The Linux build is static, so it
> runs on any distribution.

#### Windows

Download
[needy-x86_64-pc-windows-msvc.zip](https://github.com/needy-dev/needy/releases/latest/download/needy-x86_64-pc-windows-msvc.zip),
extract `needy.exe` to a permanent directory and add that directory to your
user `PATH`. A Windows machine is an executor only.

#### Install with proto

Install and configure [proto](https://moonrepo.dev/docs/proto) first. Then add
the needy plugin and install the latest release:

```sh
proto plugin add needy "https://raw.githubusercontent.com/needy-dev/needy/main/toolchain/proto/needy.toml" --to global
proto install needy latest --config-mode global --pin global
needy --version
```

After installing, pull the base images of your needy's release for the
drivers this machine has (see the table below):

```sh
needy image pull                 # or one driver: needy image pull --driver macos-vz
needy image ls
```

For a VM driver (`linux-firecracker`, `macos-vz`) it downloads the release's
VM image, checks it against its `.sha256` and unpacks it into needy's data
directory; for a container driver it pulls the base image (and the executor
image) through the engine or Apple's `container`. A machine whose engine can
run `linux-container` executors but not actors (a Mac) gets the executor
image alone. needy never downloads a VM
image by itself: a run without one fails and names `needy image pull`. The
container drivers pull a missing container image themselves.

From the next release on, each
[GitHub release](https://github.com/needy-dev/needy/releases) carries these
assets, each with a `.sha256` file:

| Asset | What it is |
| --- | --- |
| `needy-x86_64-unknown-linux-musl.tar.gz`, `needy-aarch64-unknown-linux-musl.tar.gz` | `needy` for Linux, statically linked |
| `needy-aarch64-apple-darwin.tar.gz`, `needy-x86_64-apple-darwin.tar.gz` | `needy` for macOS, signed with a Developer ID and notarized; the arm64 one also holds `needy-linux-aarch64`, the guest needy (not signed: a Linux binary) |
| `needy-x86_64-pc-windows-msvc.zip` | `needy.exe` for Windows (executor only), not Authenticode-signed |
| `needy-actor-linux-firecracker-x86_64.tar.gz` | the `linux-firecracker` base image: `vmlinux`, `rootfs.ext4`, `firecracker`, `manifest.json` |
| `needy-actor-macos-vz-aarch64.tar.gz` | the `macos-vz` base image: `Image`, `initrd`, `rootfs.ext4`, `manifest.json` |

Each archive has a build provenance attestation:
`gh attestation verify needy-actor-macos-vz-aarch64.tar.gz -R needy-dev/needy`
checks that this repository's release workflow built it. The container images
are on GHCR for amd64 and arm64, tagged with the release (`v<version>`) and
`latest`: `ghcr.io/needy-dev/needy-actor` and
`ghcr.io/needy-dev/needy-executor`. A profile without `actor.image`, and a
daemon without `--executor-image`, use those of its own version.

A needy built from a checkout has no release to pull from, and `needy image
pull` says so. Build the images instead, and name the container images it
builds, `needy-actor:latest` and `needy-executor:latest`:

```sh
needy image build                                  # the base VM image of this host's VM driver
needy image build --linux-container --executor     # or --macos-apple-container on a Mac
needy daemon --mode control --executor-image needy-executor:latest &
# and in a container actor's profile: actor: { image: needy-actor:latest, ... }
```

`needy image pull --file ARCHIVE` imports a VM image archive on a machine
without a network, and `needy image rm IMAGE` removes a VM image
(`--guest-disks` also the guest disks of older needy versions). The prebuilt
kernels have no recipe's identity; `needy image build --recipe` builds one
that does. An image works with every needy that speaks its base ABI; needy
names the command when it does not.

What each machine needs besides `needy`:

| Machine | Needs | Then |
| --- | --- | --- |
| Linux with KVM (actor host) | Docker or another Docker-compatible engine | `needy image pull` (the `linux-firecracker` base, and the container images) |
| Linux without KVM (actor host) | a Docker-compatible engine; `sshfs` and `user_allow_other` in `/etc/fuse.conf` for workspace mounts; `lxcfs` for resource views | `needy image pull` (the container images) |
| Apple silicon Mac (actor host) | `brew install vfkit`, or Apple's [`container`](https://github.com/apple/container) | `needy image pull` (the `macos-vz` base, or the base in Apple's `container`) |
| Any Linux, macOS or Windows machine (executor only) | nothing | join as a runner, see [Add machines](#add-machines) |

#### Uninstall

Use the commands that match how you installed needy. They keep needy's
state and data directories.

```sh
brew uninstall needy                                      # Homebrew
rm -f ~/.local/bin/needy ~/.local/bin/needy-linux-*       # install script
proto uninstall needy && proto plugin remove needy --from global   # proto
cargo uninstall needy-cli                                 # from source; also remove the copied needy-linux-*
```

On Windows, delete the directory you extracted `needy.exe` to and remove it
from `PATH`.

### Start the control daemon

```sh
needy daemon --mode control &        # listens on a Unix socket in the state directory
```

It warns when it finds no guest needy; actors then cannot start. Add
`--listen 127.0.0.1:7400 --token-env NEEDY_CONTROL_TOKEN` to reach it from
other machines (through `ssh -L` or another tunnel; needy does not encrypt the
connection itself).

### Pick a profile

Which actor do you want? Each block is a complete profile: the driving program
is `bash`, and `python3` is routed to an executor while everything else runs
in the actor. Save it as `~/.config/needy/profiles/demo.yaml`.

<details>
<summary><b>Linux with KVM</b>: a Firecracker microVM (<code>linux-firecracker</code>)</summary>

```yaml
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-firecracker
  recipe: builtin:gce-ubuntu-22.04
  command: [bash]
executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "python3 **", to: build }
```

</details>

<details>
<summary><b>Linux without KVM</b>: a container with runc or gVisor (<code>linux-container</code>)</summary>

```yaml
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-container
  container: { oci_runtime: runc }      # or runsc, if gVisor is installed
  recipe: builtin:gce-ubuntu-22.04
  command: [bash]
executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "python3 **", to: build }
```

</details>

<details>
<summary><b>Apple silicon Mac</b>: a Virtualization.framework VM (<code>macos-vz</code>)</summary>

```yaml
# mkdir -p /tmp/needy-demo first: the Mac itself is the executor.
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: macos-vz
  command: [bash]
executors:
  mac: { driver: native, workspace: /tmp/needy-demo }
commands:
  - { match: "python3 **", to: mac }
```

</details>

<details>
<summary><b>Apple silicon Mac</b>: Apple's <code>container</code> (<code>macos-apple-container</code>)</summary>

```yaml
# mkdir -p /tmp/needy-demo first: the Mac itself is the executor.
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: macos-apple-container
  command: [bash]
executors:
  mac: { driver: native, workspace: /tmp/needy-demo }
commands:
  - { match: "python3 **", to: mac }
```

</details>

`needy profile check demo` says where the actor and each executor would run,
what of the recipe the driver cannot present, and where a command would go:
`needy profile check demo --cmd "python3 -V"`.

### Run it

```sh
needy run create -it --profile demo
#   agent@instance:~$ uname -a                                  # the actor
#   agent@instance:~$ python3 -c 'import platform; print(platform.node())'   # the executor
#   agent@instance:~$ exit
needy report latest                       # what was routed, faults, the actor's identity
```

`ctrl-]` detaches and leaves the run going. You never copy a run ID:
`run attach`, `run logs` and `run stop` take the only live run; every `--run`
takes `latest`, an ID prefix, or `NEEDY_RUN`; `report` defaults to `latest`.

```sh
needy run ls                                    # ACTOR: starting, configured, ready (attach connects), exited
needy run attach                                # back to the driving program
needy exec --run latest -- uname -a             # one command through the run's rules
export NEEDY_RUN=latest                         # or set it once
needy exec -d -- python3 -m http.server         # prints a process ID
needy ps; needy attach PROCESS                  # replays recent output
needy fs cp ./src build:/home/agent/workspace
needy run stop
```

`run stop` ends a running driving program gracefully: SIGTERM, then SIGKILL
after `--grace` (default 10s); the actor then mirrors out and ends, and the
stop goes on. `--now` kills the actor at once, without its mirror-out. The
report says which it was.

Without `--run` or `NEEDY_RUN`, `needy exec` runs on this machine outside any
run, so starting a run never changes what a plain `needy exec` does.
Arguments after `--` in `run create` are appended to the profile's
`actor.command`.

### One command from your laptop

Pair the laptop once with a code the operator creates on the control host,
then call the remote program as if it were local:

```sh
NEEDY_TOKEN=... needy pair create --name laptop       # on the control host, with the control token
needy --endpoint http://127.0.0.1:7400 pair CODE      # on the laptop, once; the endpoint is remembered
```

Then save the profile on the laptop as `~/.config/needy/profiles/opencode.yaml`
(see [`examples/opencode`](examples/opencode/profile.yaml)) and add a script:

```sh
#!/bin/sh
# ~/bin/ropencode: opencode in an actor on the control host
exec needy run create -it --profile opencode -- "$@"
```

`needy pair ls` and `needy pair revoke ID` manage paired clients; the trace and
`needy report` name each one as `client:NAME`.

### Add machines

Any other machine, such as a Mac or a Windows box, joins the control plane as
a **runner**. It dials out and keeps that connection open, so it needs no
listening port:

```sh
needy runner token create --ttl 1h --roles executor          # on the control host
JOIN=... needy daemon --mode runner --join http://127.0.0.1:7400 --join-token-env JOIN
needy runner ls
#   NAME  STATUS  CAPABILITIES                                              HOST
#   kvm   online  actor(linux-firecracker) executor(linux-container,native) linux/x86_64
#   mac   online  actor(macos-vz) executor(native)                          macos/aarch64
needy exec --runner mac -- uname -a        # on that machine, outside any run
```

Profiles place executors (and the actor) on runners by name or label:
`executors: { mac: { runner: mac } }`. On Windows, start the runner outside
the SSH session (for example with `Invoke-CimMethod Win32_Process`), with full
paths and the token in `--join-token-file`.

## Actor drivers

`actor.driver` picks where the actor runs. The prefix is the operating system
whose isolation the driver relies on.

| | `linux-firecracker` | `linux-container` | `macos-vz` | `macos-apple-container` |
| --- | --- | --- | --- | --- |
| Host | Linux x86_64 with KVM | Linux with a Docker-compatible engine | Apple silicon macOS | Apple silicon macOS |
| Isolation | Firecracker microVM, needy's kernel | `runc` (shared kernel) or `runsc` (gVisor) | Virtualization.framework VM through vfkit, needy's arm64 kernel | Apple `container`: one VM per container, Apple's kernel |
| How execs are caught | kernel module `binfmt_needy` | shims on every `PATH` program of the image | kernel module `binfmt_needy` | shims |
| Ready in | ~0.17 s | | ~0.27 s | ~0.86 s |
| **Routing** | | | | |
| Route a command by name | ✅ | ✅ | ✅ | ✅ |
| Route by absolute path (`/usr/bin/git`) | ✅ | ✅ | ✅ | ✅ |
| Route programs added at run time (`pip`, `npm`, built in the workspace) | ✅ | ❌ | ✅ | ❌ |
| Network only through the gateway (allowlist, header injection) | ✅ | ✅ | ✅ | ✅ |
| Workspace mount from an executor | ✅ | ✅ | ✅ | ✅ |
| Copy-in, mirror-out | ✅ | ✅ | ✅ | ✅ |
| Exec and file faults | ✅ | ✅ | ✅ | ✅ |
| Interactive (`-it`, attach, detach) | ✅ | ➖ | ✅ | ✅ |
| **Identity from the recipe** | | | | |
| Hostname | ✅ | ✅ | ✅ | ✅ |
| `/proc/cpuinfo` | ✅ | ✅ | ❌ (arm64) | ❌ |
| `CPUID` | ✅ | ❌ | ❌ | ❌ |
| DMI / SMBIOS | ✅ | ✅ (where the host has DMI; any host under `runsc`) | ❌ | ❌ |
| PCI IDs | ✅ | ❌ | ❌ | ❌ |
| hwmon sensors and the overheat fault | ✅ | ✅ (where the host has hwmon; any host under `runsc`) | ✅ | ❌ |
| Kernel version | ✅ | ❌ (host or gVisor) | ➖ | ❌ |
| Uptime offset | ✅ | ❌ | ➖ | ❌ |

✅ verified end to end, ❌ not possible with this driver, ➖ expected to work but
not yet recorded. `needy profile check` lists every gap for your recipe and
host; a recipe's `required:` turns a gap into an error. Evidence:
[closed-loop results](docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md),
[container actor](docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md#identity),
[`macos-vz`](docs/ai/references/architecture/2026-11-12-macos-vz-actor-evidence.md),
[`macos-apple-container`](docs/ai/references/architecture/2026-11-12-macos-apple-container-actor-evidence.md).

Executor drivers: `native` (the machine itself: Linux, macOS, Windows) and
`linux-container` (a container through the same engine). A `macos-native`
actor (Seatbelt and shims, no VM) exists as a proof of concept on the
`poc/macos-native-actor` branch.

## Configuration

A profile is YAML; unknown keys are errors, and `extends:` builds on another
profile. The full reference is in the
[profile design](docs/ai/references/config/2026-11-09-profile-and-rules-design.md).

### Commands

Rules decide where each exec runs. The first match wins; a command no rule
names runs in the actor.

```yaml
commands:
  - { match: "{bash,sh,zsh} **", to: build }            # glob over the command line
  - { match: "git push **", deny: "no pushes from the agent" }
  - match: "pytest **"
    when: { cwd: "~/workspace/**", tty: false }
    to: build
    timeout: 10m
    env: { set: { CI: "1" } }
```

### Workspace

The directory the work operates on is one setting. The driving program
starts in it.

```yaml
actor:
  workspace:
    path: ~/project                      # in the actor
    from: executor:build:~/project       # live from an executor; or host:PATH (copied in), or omit it
    writable: true
```

`from: executor:NAME:PATH` mounts the executor's directory at `path` and maps
`path` to `PATH` for the commands routed there. `~` is the profile user's home
in the actor, the control host's home on `host:`, and the executor's own home
on an executor side.

### Files

Named mappings move other files between the control host (`host:`), the actor
(`actor:`) and an executor (`executor:NAME:`). `run_files` lists the ones every
run uses; a rule's `files:` adds some for the commands it matches.

<picture>
  <source width="100%" srcset="./docs/assets/banners/files-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/files-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="Files: copy-in brings a login from the control host into the stateless actor; the build container serves the workspace and the actor mounts it over sshfs, so a file the actor writes is on the executor; mirror-out copies the login back when the run ends." src="./docs/assets/banners/files-light.webp" />
</picture>

```yaml
files:
  login-in:                        # copied in when the run starts
    mode: copy-in
    from: "host:~/needy-state/opencode/"
    to: "actor:~/.local/share/opencode/"
    filter: ["- /cache/**", "+ **"]
  login-out:                       # copied back when the run ends (not on `run stop --now`)
    mode: mirror-out
    from: "actor:~/.local/share/opencode/"
    to: "host:~/needy-state/opencode/"
run_files: [login-in, login-out]
```

A copy-in / mirror-out pair is how state such as a login survives stateless
actors. A `filter` (rclone-style rules, first match wins) applies on the
control host, where it packs a copy-in and where it unpacks a mirror-out.

### Network

The actor has no network of its own: everything goes through the gateway on
the control host. HTTPS is tunnelled as is (no MITM, no injected CA, and so no
injected headers).

<picture>
  <source width="100%" srcset="./docs/assets/banners/network-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/network-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="The gateway: the actor has no network of its own. models.dev is on the allowlist and is tunnelled; example.com is refused with 403 over HTTP and HTTPS; the service model.needy is forwarded to a local model at 127.0.0.1:11434." src="./docs/assets/banners/network-light.webp" />
</picture>

```yaml
actor:
  network:
    allow: [opencode.ai, "*.opencode.ai"]   # `*.x` also matches `x`
    services:
      ollama:                               # in the actor: http://ollama.needy
        url: http://127.0.0.1:11434         # resolved on the control host
```

A service is a plain-HTTP name the actor uses; the gateway forwards it to
`url`, so the actor never learns the real address. `needy profile check`
lists the services and warns when a URL does not answer from the control
host.

### Chaos

Named faults, each in one domain, with real effects. They fire from a seed,
so a replayed run injects the same ones; a fault without `probability`,
`nth`, `times`, `after` or `until` fires on every match.

<picture>
  <source width="100%" srcset="./docs/assets/banners/chaos-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/chaos-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="examples/chaos from seed 1: opening broken.json fails with Input/output error, curl fails with its own error and exit 7 without running, the sensors read +101.0°C ALARM (CRIT) two seconds in, and needy report counts the three injected faults by name." src="./docs/assets/banners/chaos-light.webp" />
</picture>

```yaml
chaos:
  seed: 1
  faults:
    curl-down:
      exec: "curl **"                   # the command exits 7 without running
      exit: 7                           # or signal: SIGSEGV, hang: true, delay: 2s
      stderr: "curl: (7) Failed to connect\n"
      times: 3
    flaky-json:
      file: { open: "**/*.json" }
      errno: EIO                        # ENOENT, EACCES, EPERM, EOPNOTSUPP or EIO
      probability: 0.1
    slow-example:
      net: { connect: "*.example.com" }
      delay: 2s                         # or refuse, reset, hang; status: 503 with net: { request: ... }
    overheat:
      sensor: { temp_c: 101 }
      at: 30s
```

Change a live run's faults; the trace records each change:

```sh
needy chaos add api-down --request ollama.needy --status 503 --after 1m
needy chaos ls
needy chaos rm api-down
```

### Executors

```yaml
executors:
  build:
    driver: linux-container        # or native
    runner: { labels: [kvm] }      # where; the control host when absent
    key: "{run}"                   # commands with the same key share an instance
    max_procs: 8
    container:                     # typed engine options
      image: needy-executor:latest
      cpus: 2
      memory: 4GiB
      env: { LANG: C.UTF-8 }
  mac: { runner: mac, path_map: { ~/tools: ~/needy-tools } }   # values: the runner's home
```

Privileged options (`privileged`, `cap_add`, `devices`, `gpus`, host mounts
outside the workspace) need the operator. Containers needy creates carry the
labels `needy.run` and `needy.role` (`actor` or `executor`):
`docker ps --filter label=needy.run=RUN` lists a run's containers.

### Customize the actor

```yaml
actor:
  driver: linux-firecracker
  runner: kvm
  recipe: builtin:gce-ubuntu-22.04   # or a path to your own recipe
  command: [opencode]
  env: { OPENCODE_DISABLE_AUTOUPDATE: "1" }
user: { name: bob, uid: 1001, gid: 1001 }   # whoami, id, HOME and getent agree in the actor
```

A [recipe](docs/ai/references/config/2026-11-09-recipe-design.md) sets the
machine: CPU model and count, memory, kernel, firmware strings, devices,
hostname (`instance-{time}`), and which identity fields are `required:`.
`needy image build --recipe RECIPE` builds the actor kernel with a recipe's
kernel options. `linux-container` takes `actor.container.oci_runtime`.
`actor.image: REF` runs your own image `FROM` needy's base on every driver
(`needy image build --from REF` for the VM drivers); see
[`examples/custom-image`](examples/custom-image/Dockerfile).

### Environment and data

| Variable | Meaning |
| --- | --- |
| `NEEDY_ENDPOINT` | the daemon to talk to, as `--endpoint`; both override the endpoint `needy pair` remembered |
| `NEEDY_TOKEN` | a bearer token that overrides the paired one |
| `NEEDY_RUN` | the run that `--run` defaults to |
| `NEEDY_STATE_DIR` | traces, `state.db`, run directories, the socket on macOS (`$XDG_STATE_HOME/needy`) |
| `NEEDY_DATA_DIR` | VM images in `images/<driver>/` and guest disks (`$XDG_DATA_HOME/needy`) |
| `NEEDY_CONFIG_DIR` | client tokens and `profiles/` (`$XDG_CONFIG_HOME/needy`) |
| `NEEDY_RUNTIME_DIR` | the daemon's socket (`$XDG_RUNTIME_DIR/needy`) |
| `NEEDY_REPO` | the checkout `needy image build` builds from, when not run inside it |
| `DOCKER_HOST` | the Docker-compatible engine (Docker Engine, OrbStack, Podman) |

`--token-env NAME` and `--join-token-env NAME` name the variables that hold the
control token and a join token; `-v` turns on logs.

## Examples

| Profile | Shows |
| --- | --- |
| [`examples/opencode`](examples/opencode/profile.yaml) | opencode in an actor with a GCE identity, a local model as the service `http://model.needy`, shells in a container |
| [`examples/macos`](examples/macos/profile.yaml) | A Linux actor running `xcodebuild`, `simctl` and `swift build` on runner `mac` |
| [`examples/smoke`](examples/smoke/profile.yaml) | Routing, the shared workspace and identity probes |
| [`examples/chaos`](examples/chaos/profile.yaml) | Copy-in and mirror-out, an `EIO` file fault on the workspace, a `curl` that exits 7 and an overheating sensor |
| [`examples/linux-container`](examples/linux-container/profile.yaml) | The smoke test in a container actor, for hosts without KVM |
| [`examples/macos-vz`](examples/macos-vz/profile.yaml) | The smoke test in a `macos-vz` actor, routing by absolute path |
| [`examples/macos-apple-container`](examples/macos-apple-container/profile.yaml) | The smoke and chaos tests in Apple's `container` |
| [`examples/custom-image`](examples/custom-image/profile.yaml) | An actor image `FROM` needy's base with jq added, on every driver |
| [`examples/agents`](examples/agents/README.md) | pi, Claude Code, Codex, opencode and herdr in an actor: one image, a profile each, their logins kept across runs |
| [`examples/local`](examples/local/profile.yaml) | This machine as a native executor: copy-in, mirror-out, chaos and the trace |
| [`examples/cli`](examples/cli/profile.yaml) | A container executor for `needy exec`, with an actor that only waits |
| [`examples/bench`](examples/bench/profile.yaml) | Latency and throughput probes inside an actor, with `cargo xtask bench run` |

## How it works

```mermaid
flowchart LR
  subgraph A[actor]
    P[driving program] --> C[capture: binfmt_needy or shims]
  end
  C -->|exec, by rule| E1[executor: container]
  C --> E2[executor: Mac]
  P -->|all network| G[gateway sidecar] --> M[model API]
  P -->|workspace| F[sshfs from an executor]
  D[control daemon] -. guest API sidecar: rules, config, trace .- A
  D -. lifecycle .- E1
```

<picture>
  <source width="100%" srcset="./docs/assets/banners/routing-dark.webp" media="(prefers-color-scheme: dark)" />
  <source width="100%" srcset="./docs/assets/banners/routing-light.webp" media="(prefers-color-scheme: light), (prefers-color-scheme: no-preference)" />
  <img width="100%" alt="Rules decide where each command runs: python3 -m pytest and npm install go to the build container, /usr/bin/git push matches the deny rule by its basename and exits 126, and the trace records the exec.decide." src="./docs/assets/banners/routing-light.webp" />
</picture>

- **Capture, decide, apply.** A driver's capture catches each exec, the
  gateway sidecar every connection, the workspace mount every file open. The
  daemon runs the domain's interceptors (rules, then chaos; the allowlist,
  chaos and header injection for the network) and the program sees ordinary
  PIDs, pipes, terminals and exit codes.
- **Observe.** Routing decisions, routed execs, file transfers, gateway
  requests and faults are typed events in the trace (SQLite or JSONL, as the
  profile's `trace:` says). `needy report` summarizes a run as text, markdown
  or JSON. Commands that stay in the actor and terminal output are not
  recorded.
- **Shape.** Faults (`EIO`, delays, an overheating sensor) and a machine
  identity built from a recipe.

The actor's network sits near its main dependency, such as inference, and its
gateway injects credentials so the actor never holds them. Executors sit near
what the work operates on.

## Architecture

| Crate | Role |
| --- | --- |
| `cli` | the `needy` binary; inside an actor, injected as the guest needy, also its init and exec proxy |
| `daemon` | the control plane: runs, pool, scheduling, routing, relay, files, runners, `state.db` |
| `core-actor` | actor drivers, the guest (init, capture, proxy), the gateway sidecar, identity |
| `core-executor` | executor drivers, the container engine client, the process host |
| `core` | interceptor domains (exec, net, file) and their chains, rules, chaos, network policy, recipes, events, reports; no IO |
| `trace` | the event pipeline and its sinks |
| `config` | profile and recipe schemas, loading and validation |
| `api-proto`, `api-client`, `api-server` | the gRPC API and its two sides |

Extension points are directories with one implementation each: actor drivers
(`core-actor/src/driver/`), executor drivers (`core-executor/src/driver/`),
interceptor domains (`core/src/intercept/`), sidecars
(`core-actor/src/sidecar/`) and trace sinks (`trace/src/sink/`). See the
[drivers and extension points design](docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md)
and [`AGENTS.md`](AGENTS.md).

## Documentation

- [Terms and concepts](docs/TERMS_AND_CONCEPTS.md)
- [Actor and executor separation](docs/ai/references/architecture/2026-11-09-actor-executor-separation-design.md):
  model, terms, network and experiments.
- [Runners and runs](docs/ai/references/architecture/2026-11-10-runner-and-run-design.md)
- [CLI, exec streams and reports](docs/ai/references/cli/2026-11-09-cli-exec-and-report-design.md)
- [Profiles and rules](docs/ai/references/config/2026-11-09-profile-and-rules-design.md)
- [Recipes](docs/ai/references/config/2026-11-09-recipe-design.md)
- [Closed-loop results](docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md):
  what runs, and latency and throughput numbers.
- [Where documents go](docs/README.md)

## Development

```sh
make check
make test
make proto
cargo xtask bench --help      # latency and throughput probes
cargo xtask e2e               # the end-to-end matrix on this host
```

`cargo xtask e2e` runs every cell of the end-to-end matrix this host can
run: each actor driver it has, with each terminal mode, way a run ends, file
mapping, fault domain, network rule and user, each cell against its own
non-root control daemon, checked for invariants (ownership and modes,
mirror-out after every ending, placement in the trace, private sockets,
nothing left behind). It builds the guest needy and the images from the tree
first. A cell it cannot run here is reported as not covered, with the reason,
and the exit code says so. `--list` shows the cells, `--only` and `--skip`
pick some, `--remote HOST` runs on another machine from `git archive HEAD`,
and `--hosts FILE` runs on several at once, each building natively; see the
[e2e harness design](docs/ai/references/architecture/2026-11-13-e2e-harness-design.md).

GitHub Actions runs the checks and tests on Linux, macOS and Windows, the e2e
cells without an actor on Linux and macOS, and builds the release assets; see
[Release and CI](docs/ai/references/architecture/2026-11-12-release-and-ci-design.md).

The figures in this README are animated scenes in [`apps/docs`](apps/docs), a
page you can also open to step through them and route your own commands:

```sh
cd apps/docs && pnpm install
pnpm dev                      # the page
pnpm banners                  # re-render docs/assets/banners (needs Chrome and img2webp)
```

## Acknowledgements

Needy borrows from, was compared against, or was tested with these projects.
Thank you to their authors.

**Built on**

- [Firecracker](https://github.com/firecracker-microvm/firecracker): the actor
  microVM; needy carries a patch for CPUID, SMBIOS and PCI identity.
- [vfkit](https://github.com/crc-org/vfkit) and Apple's
  [`container`](https://github.com/apple/container): the macOS actors.
- [sshfs](https://github.com/libfuse/sshfs) and OpenSSH `sftp-server`: file
  routing in passive mode, an idea taken from
  [Lima](https://github.com/lima-vm/lima)'s reverse-sshfs mounts.
- [tonic](https://github.com/hyperium/tonic), [prost](https://github.com/tokio-rs/prost),
  [Buf](https://github.com/bufbuild/buf) with the
  [neoeinstein](https://github.com/neoeinstein/protoc-gen-prost) plugins,
  [tokio-vsock](https://github.com/rust-vsock/tokio-vsock),
  [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty),
  [hyper](https://github.com/hyperium/hyper),
  [bollard](https://github.com/fussybeaver/bollard),
  [SeaORM](https://github.com/SeaQL/sea-orm) and
  [brush](https://github.com/reubeno/brush)'s shell parsing approach.

**Design references**

- [AUV](https://github.com/moeru-ai/auv): repository layout, documentation
  structure, Buf workflow and agent skills.
- [agentsh](https://github.com/canyonroad/agentsh): human and JSON output modes,
  session reports, composite trace storage and its platform comparison.
- [Reverie](https://github.com/facebookexperimental/reverie) and
  [Hermit](https://github.com/facebookexperimental/hermit): syscall
  interception, chaos scheduling and virtual time.
- [kbox](https://github.com/sysprog21/kbox): seccomp user notification, FD
  injection and shadow memfds.
- [mirrord](https://github.com/metalbear-co/mirrord): running a local process
  against remote resources.
- [containerd shim v2](https://github.com/containerd/containerd/blob/main/api/runtime/task/v2/shim.proto):
  the process lifecycle API.
- [herdr](https://github.com/ogulcancelik/herdr): terminal viewers and redraw.
- [rclone](https://rclone.org/filtering/), [Mutagen](https://mutagen.io/) and
  [Lima mounts](https://lima-vm.io/docs/config/mount/): file mapping and
  filters.
- [Codex](https://github.com/openai/codex/pull/7127) and
  [Claude Code](https://code.claude.com/docs/en/permissions) permission rules:
  matching shell commands.
- [macstab chaos-testing-libraries](https://github.com/macstab/macstab-chaos-testing-libraries):
  rule-based syscall fault injection.
- [DeepSeek DSec](https://arxiv.org/html/2609.22978v1): sandbox infrastructure
  for agent RL at scale, and the agent attacks it reports.

**Compared or tried**

- Isolation: [gVisor](https://github.com/google/gvisor),
  [Kata Containers](https://github.com/kata-containers/kata-containers),
  [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor),
  [libkrun](https://github.com/containers/libkrun),
  [Containerization](https://github.com/apple/containerization),
  [agentc](https://github.com/laosb/agentc),
  [microvm.nix](https://github.com/astro/microvm.nix),
  [Incus](https://github.com/lxc/incus) and [lxcfs](https://github.com/lxc/lxcfs).
- OS identity: [Darling](https://github.com/darlinghq/darling) (tested),
  [Wine](https://www.winehq.org/),
  [tart-skills](https://github.com/jonnyzzz/tart-skills),
  [faux-mac](https://github.com/stpettersens/faux-mac),
  [fake-uname](https://github.com/brenkem/fake-uname) and
  [fakehostname](https://github.com/dtcooper/fakehostname).
- Agent sandboxes and policy:
  [Anthropic sandbox-runtime](https://github.com/anthropics/sandbox-runtime),
  [E2B](https://e2b.dev/), [Modal](https://modal.com/),
  [AgentSentinel](https://github.com/abhinandanpandey-in/agentsentinel) and
  [safe-agent](https://github.com/TurboRx/safe-agent).
- Observation and faults: [Tetragon](https://github.com/cilium/tetragon),
  [Falco](https://github.com/falcosecurity/falco),
  [libfiu](https://github.com/albertito/libfiu), `strace --inject` and the
  kernel fault-injection framework.
- From the earlier research: [PRoot](https://github.com/proot-me/proot),
  [LKL](https://github.com/lkl/linux), [LegoOS](https://github.com/Wuklab/LegoOS)
  and [Telepresence](https://github.com/telepresenceio/telepresence).
- Driving program and model used in tests: [opencode](https://github.com/sst/opencode),
  [Ollama](https://github.com/ollama/ollama) with Qwen3.

Thanks to [Makito](https://github.com/sumimakito) for CLI design ideas.
