# Deployment

A map of how to deploy needy, what to configure for each layout, and how to
run it day to day. It links to the guides that hold the details instead of
repeating them:
[installation](installation.md), [configuration](configuration.md),
[multi-machine](multi-machine.md), [agent images](agent-images.md), the
[`rc` integration guide](../ai/references/architecture/2026-11-26-rc-integration-guide.md)
(Kubernetes) and the
[runner gateway design](../ai/references/architecture/2026-11-26-runner-gateway-design.md).
Terms (actor, executor, runner, control daemon, gateway) are those of
[Terms and concepts](../TERMS_AND_CONCEPTS.md).

Every claim below names its evidence level: **code** (read from the source,
not exercised), **in-process** (an automated test in the repository),
**e2e** (`cargo xtask e2e` on real hosts), or **probe** (a live run on real
infrastructure, not a support claim). The [evidence table](#evidence) lists
where each was seen. Versions are needy releases; v0.0.22 is current.

## 1. Topologies, and which to pick

| You have | Pick |
| --- | --- |
| One Linux machine with KVM, or one Apple silicon Mac | [(a) Single host](#a-single-host) |
| One Linux machine without KVM (most cloud VMs) | (a) with a `linux-container` actor |
| A control machine without KVM, KVM hosts elsewhere, tools on other machines | [(b) Control plus remote runners](#b-control-plus-remote-runners) |
| A Kubernetes cluster with a KVM node pool, one daemon per tenant | [(c) Kubernetes](#c-kubernetes) |
| Windows machines | [(d) Windows](#d-windows): executor and CLI only |

### (a) Single host

Control, actor and executors on one machine. The control daemon runs the
actor, its gateway and the executors itself.

- **Linux with KVM**: `actor.driver: linux-firecracker` (a microVM).
  e2e on a Linux KVM workstation.
- **Linux without KVM**: `actor.driver: linux-container` (a container
  through Docker or another Docker-compatible engine). e2e on 2-vCPU cloud
  VMs without KVM (runc and gVisor).
- **Apple silicon macOS**: `actor.driver: macos-vz` (Virtualization.framework
  through vfkit), or `macos-apple-container`. e2e, see the
  [e2e harness results](../ai/references/architecture/2026-11-13-e2e-harness-design.md#results).

Install with the [quick install](installation.md#quick-install); it starts the
daemon as a user service. Nothing else is needed to start runs from the same
machine.

### (b) Control plus remote runners

The control daemon holds state, policy and traces and needs neither KVM nor
root. **Actor runners** (`needy runner serve --roles actor`) run
`linux-firecracker` actors on KVM hosts; **executor runners**
(`--roles executor`) run commands on other machines (Linux, macOS, Windows).
Every connection is outbound from a runner to the daemon; runners never
listen.

The common split is three hosts:

| Host | Role | Runs |
| --- | --- | --- |
| A machine without KVM (a 2-vCPU cloud VM) | control | `needy daemon serve` |
| A Linux KVM host | actor runner | `needy runner serve --roles actor` |
| A Mac, a Linux box or a Windows PC | executor runner | `needy runner serve --roles executor` |

Only `linux-firecracker` is accepted by a remote actor runner (code). The
three-process form of this layout, on one KVM host, is the `remote_actor`
e2e cell (`xtask/src/e2e/remote_actor.rs`); across hosts it ran in the
v0.0.19 and v0.0.20 passes (probe, see the
[multi-host pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md)). For an actor
on Linux and an executor on a Mac step by step, see the
[Linux actor and Mac executor walkthrough](../ai/explanations/linux-actor-mac-executor.md).

### (c) Kubernetes

One `needy daemon serve` per tenant namespace (a one-replica `Recreate`
Deployment with its state on a PVC), one **ephemeral** actor runner Pod per
actor on a KVM node pool (`--roles actor --ephemeral`, a minted credential),
and executor runners inside existing Workspace Pods (static credentials).
A controller (`rc`) creates the Pods, mints and revokes credentials and calls
`CreateRun`; needy itself creates no Kubernetes objects. The
[`rc` integration guide](../ai/references/architecture/2026-11-26-rc-integration-guide.md)
has the manifests (sections 2 and 5), authentication (3, 4), the run
sequence (6), Workspace executors (7) and NetworkPolicies (8). Probe on a
self-managed cluster and a managed EKS KVM pool.

### (d) Windows

Windows hosts an **executor runner** and runs the CLI. It cannot host the
control daemon or an actor (code: `needy daemon serve` and `StartActor` are
refused off Unix). **CLI authentication on Windows is deferred**: saving a
paired context and reading a private credential or code file
(`--credential-file`, `--code-file PATH`) need private-file ACLs that are
not implemented (`TODO(windows-acl)` in `crates/cli/src/configuration.rs`
and `xtask/src/e2e/windows.rs`), so a Windows CLI has no way yet to
authenticate to a remote daemon. A Windows executor runner can join with
the code on stdin (`--code-file -`) or at the prompt, or serve with
`--credential-env VAR` (code). Automated Windows enrollment is not covered
by the e2e harness for the same reason; the earlier Windows runner cells
are in the
[e2e harness results](../ai/references/architecture/2026-11-13-e2e-harness-design.md#results).

## 2. Setting up each role

### Prerequisites

| Host | Needs |
| --- | --- |
| Every Linux host | A **non-root** user for the daemon or runner (the e2e harness fails every cell under a root daemon, since root hides permission bugs). `XDG_RUNTIME_DIR` set (a logind session, or `loginctl enable-linger USER` for a service), or `NEEDY_RUNTIME_DIR` on a private `0700` directory: the socket lives there, and a `linux-container` actor's mount points must be outside its workspace (fixed in v0.0.22, [#132](https://github.com/needy-dev/needy/pull/132)). |
| `linux-firecracker` (local or actor runner) | `/dev/kvm` readable and writable by that user: add it to the group owning the device (`stat -c %G /dev/kvm`). The base VM image (`needy image pull --driver linux-firecracker`). |
| `linux-container` actor or executor | A Docker-compatible engine the user can reach (`DOCKER_HOST`), plus `sshfs`, `fuse3` and `user_allow_other` for workspace mounts: see [workspace mount prerequisites](installation.md#workspace-mount-prerequisites-linux-container-actor). |
| macOS | `vfkit` (Homebrew installs it) for `macos-vz`. |
| Executor runner with workspace mounts | `sftp-server` (`openssh-sftp-server` on Debian and Ubuntu) at `/usr/lib/openssh/sftp-server` on Linux. |
| Daemon without KVM serving image definitions to remote actors | The base VM image as well: `needy image pull --driver linux-firecracker` ([rc guide §2](../ai/references/architecture/2026-11-26-rc-integration-guide.md#the-base-vm-image-on-a-daemon-without-kvm-code)). |

Runner-only machines install with `NEEDY_NO_DAEMON=1`
([install options](installation.md#other-install-methods)). Keep the daemon
and every runner on **the same needy version** (see [Upgrades](#upgrades-and-mixed-versions)).

### Commands

Control host (flags in `crates/cli/src/commands/daemon/mod.rs`; each has a
`daemon.*` settings key except the upstream flags):

```sh
needy daemon serve \
  --listen tcp://127.0.0.1:7400 \
  --gateway-listen tcp://127.0.0.1:7401 \
  --gateway-block-cidr 10.0.0.0/8 \
  --runners-file /etc/needy/runners.yaml \
  --clients-file /etc/needy/clients.yaml \
  --disable-host-files
```

`--listen` is repeatable and the private local socket always exists.
`--gateway-listen` serves only runners older than the daemon.
`--gateway-block-cidr` is repeatable and sent to actor runners too. The
runners and clients files are hot-reloaded. `--disable-host-files` (or
`--host-files-root DIR`) keeps `host:` mappings off the control host's
files. Other daemon flags: `--layer-registry`, `--insecure-registry` (see
[Images](#4-images)), `--gateway-upstream` and
`--gateway-upstream-credentials-file` (for gateways the daemon runs itself),
`--heartbeat-interval`, `--executor-image`, `--actor-image`,
`--guest-needy`, `--local-runner`. As a service:
`needy daemon service install --now` (see
[service management](installation.md#daemon-service-management--uninstall)).

The listeners are plaintext h2c; there is no TLS listener (gap G4 in the rc
guide). Keep them on loopback and reach them through an SSH or Cloudflare
Access TCP tunnel ([tunnels](multi-machine.md#tunnels-and-proxies)), or on a
private network protected by policy or a mesh.

Runners (flags in `crates/cli/src/commands/runner.rs`):

```sh
# Interactive enrollment: on the control host
needy runner invite --roles actor --ttl 1h          # prints a code once
# on the runner host
needy --address tcp://127.0.0.1:7400 runner join kvm-1 --roles actor
needy runner serve --roles actor --work-dir /home/user/needy-vm

# Pre-provisioned (no runner.json written)
needy runner serve --address tcp://127.0.0.1:7400 \
  --credential-file /home/user/.config/needy/runner.cred --name ws-a \
  --roles executor --labels site=lab
```

Actor-runner flags: `--ephemeral` (serve one run, then exit 0 whatever the
run's outcome; gap G5), `--work-dir DIR` (each run's `vm.json`, sockets and
`console.log`, a fresh directory per run), `--actor-image DIR`, `--kvm PATH`,
`--gateway ADDR` (the daemon's `--gateway-listen`, used only with an older
daemon), `--gateway-upstream`, `--gateway-upstream-credentials-file`.
Executor-runner flags: `--activity-hook COMMAND` and
`--activity-hook-interval` (default 15 s). `needy run create
--wait-for-runner DURATION` waits for a matching runner to connect.

### Credentials

| Kind | How it is made | Use for | Revoke |
| --- | --- | --- | --- |
| **Paired / joined** | `needy runner invite` then `runner join` (saved in `runner.json`, `0600`); `needy client invite [--admin]` then `needy config context pair` | People, long-lived machines | `needy runner rm`, `needy runner rotate`, `needy client revoke` |
| **Static** | An entry in `--runners-file` or `--clients-file` | Workspaces, controllers, anything provisioned before the daemon starts | Edit the file (hot-reloaded; the API refuses static names) |
| **Minted** | `RunnerService.MintRunnerCredential` with a TTL; admin only, API only (no CLI command) | One ephemeral actor runner per run | `RevokeRunnerCredential`, or expiry |

File formats, hashing and the rules for each are in
[multi-machine](multi-machine.md#automated-and-multi-tenant-deployments)
and [rc guide §3-4](../ai/references/architecture/2026-11-26-rc-integration-guide.md#3-how-rc-authenticates-to-the-daemon).
In short: a secret is either **prefixed** (`needy-runner.NAME.<64 hex>`,
`needy-client.static-NAME.<64 hex>`, which carries its name) or, **since
v0.0.22** ([#131](https://github.com/needy-dev/needy/pull/131)), a **plain
random secret** of 32 bytes or more, found by its SHA-256
(`openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`). A runner serving a
plain secret needs `--name`. Store `credential_hash` / `token_hash` in the
daemon's file and the secret only where the runner or client reads it. A
daemon older than v0.0.22 refuses plain secrets: the file fails to load with
a warning and the previous entries stay. Set a minted credential's TTL above
the actor's whole lifetime: expiry cuts the actor off mid-run (code).

## 3. Networking

### Where the gateway runs

The actor has no network interface; its HTTP and HTTPS go through the actor
gateway, which applies `actor.network` (allowlist, services, network faults)
and records `net.*` events.

| Layout | Gateway runs in | Egress leaves from |
| --- | --- | --- |
| Single host | the control daemon | the control host |
| Remote actor, daemon and runner v0.0.19 or later (S1, [#118](https://github.com/needy-dev/needy/pull/118)) | the actor runner, under the policy the daemon sends | the actor runner's host (on Kubernetes, the actor Pod after SNAT) |
| Remote actor, either side older than v0.0.19 | the control daemon, reached through `--gateway-listen` / `--gateway` | the control host |
| Any of the above with `--gateway-upstream` | whoever runs the gateway | the SOCKS proxy |

Exactly one gateway enforces a run's network in every combination
(in-process; the mixed-version table is in the
[runner gateway design](../ai/references/architecture/2026-11-26-runner-gateway-design.md#mixed-versions)).
Keep `--gateway-listen` on the daemon and `--gateway` on runners while any
peer may be older. `--gateway-block-cidr` is a daemon flag only: S1 runners
receive and enforce the daemon's list. `actor.network.services` URLs are
resolved and dialed from wherever the gateway runs, never through an
upstream, and bypass the egress policy: never pass a user-supplied
`services` map through.

### Per-actor SOCKS upstream

Since v0.0.19, `--gateway-upstream socks5h://HOST[:PORT]` (the proxy
resolves names) or `socks5://HOST[:PORT]` (the gateway resolves and checks
them), with `--gateway-upstream-credentials-file PATH` holding
`USERNAME:PASSWORD`:

- The file must not be readable by others: `0600`, or `0440` with a Pod's
  `fsGroup` (Kubernetes' default `0644` is refused). It is re-read for
  every connection, so a rotated Secret applies without a restart.
  Credentials in the URL are refused.
- **Fail closed**: an unreachable, refusing or wrongly authenticated proxy
  gives the actor `502` and the trace a `net.error`; nothing is dialed
  directly.
- With `socks5h` the gateway cannot check where a name resolves: blocking
  private, cluster and metadata ranges is the proxy's job and the runner
  Pod's NetworkPolicy's.

In-process and probe; see "Upstream proxy" in the
[runner gateway design](../ai/references/architecture/2026-11-26-runner-gateway-design.md#upstream-proxy).

### A filtering upstream and NetworkPolicies

A proxy that does not filter destinations lets `socks5h` names reach
cluster Services. Mihomo with an authenticated SOCKS listener, `IP-CIDR`
`REJECT` rules **without** `no-resolve` for private, cluster and metadata
ranges, public resolvers and an egress NetworkPolicy of its own refused
those names (probe, not needy code):

```yaml
rules:
  - IP-CIDR,10.0.0.0/8,REJECT          # also the pod, service and node ranges,
  - IP-CIDR,169.254.0.0/16,REJECT      # 127/8, 172.16/12, 192.168/16, 100.64/10 ...
  - NOT,((IP-CIDR,0.0.0.0/0)),REJECT   # a name that does not resolve
  - MATCH,DIRECT
```

The full rule list, why the proxy's own NetworkPolicy is required, the
Workspace TUN sidecar, and the actor and daemon NetworkPolicy shapes (S1
without an upstream, with a `socks5h` upstream, and pre-S1) are in
[rc guide §8](../ai/references/architecture/2026-11-26-rc-integration-guide.md#8-networking-and-security).
NetworkPolicy ports are the Pod's ports, and under kube-proxy replacement
node ranges belong in every exclusion list.

## 4. Images

- **Base images** from GHCR: `ghcr.io/needy-dev/needy/actor-base` (every
  actor image starts here) and `ghcr.io/needy-dev/needy/executor-base` (the
  `linux-container` executor), tagged `<version>-ubuntu-24.04`, amd64 and
  arm64, attested ([installation](installation.md)). VM drivers boot the
  release's base VM image (`needy image pull`); pull it with the same needy
  version on the daemon and every actor runner, since a runner refuses layers
  planned on another base (code).
- **Agent images**: build `FROM actor-base@sha256:<digest>` with Docker or
  BuildKit, `devcontainer build`, or in-cluster with Shipwright (BuildKit,
  Buildah) or envbuilder and kaniko; push to a registry the **control daemon**
  reaches and name it in `actor.image`. VM drivers import the layers above
  the base; container drivers run the image as is. See
  [Agent images](agent-images.md). For a remote actor, the daemon imports
  and the runner fetches the layers from the daemon; runners do not contact
  registries yet (`TODO(runner-registry)`).
- **Registry access** is per process: the Docker credential store
  (`docker login`, credential helpers), CA files in `certs.d`, and
  `--insecure-registry HOST[:PORT]` as a last resort (it still uses TLS and
  sends credentials). See
  [registry credentials and TLS](configuration.md#actor-images-recipes-and-builtin).
- **Layer registry**: `--layer-registry REPOSITORY` shares built image steps
  between daemons as ordinary OCI images. A daemon without KVM cannot build
  steps (`TODO(remote-build)`): with remote actors and image definitions,
  build on a KVM host into the layer registry first, or use prebuilt
  `actor.image` references.

## 5. Operating it

### Upgrades and mixed versions

Pin the daemon, runners and any generated API client to one release; the
protos are `EXPERIMENTAL / UNSTABLE` with no wire compatibility promise in
0.0.x. What does work across versions:

| Combination | Result | Evidence |
| --- | --- | --- |
| Daemon and runner on either side of v0.0.19 | Falls back to the daemon's gateway; needs `--gateway-listen` and `--gateway` | in-process, probe |
| Runner with `--gateway-upstream`, daemon older than v0.0.19 | The runner refuses to start the actor (never leaves from the daemon) | code, probe |
| Plain-secret static entries, daemon older than v0.0.22 | The file fails to load with a warning; previous entries stay | code |
| Daemon restart or upgrade with a **local** run live | The runtime holder keeps the actor and native processes; the new daemon adopts them when credential, protocol version (1) and identity agree, else fails the run | in-process, e2e ([holder evidence](../ai/references/architecture/2026-11-15-actor-holder-implementation-evidence.md)) |
| Daemon upgraded from v0.0.21 to v0.0.22 with a local run live (only the daemon killed) | The v0.0.22 daemon adopted the running v0.0.21 holder; the run finished normally and the old holder cleaned up its mount at the old location | probe, one run ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#holder-adoption-across-v0021-to-v0022)) |
| Trace schema | v0.0.19 and later write version 16 (`via`); versions 10 to 16 read. An older needy refuses newer rows, so `run report` after a downgrade fails for runs the newer version wrote | code (`crates/core/src/event.rs`) |

To upgrade a service: install the new binary, then
`needy daemon service install --replace --now`; upgrade runners next.

### Daemon restarts are fail-fast for remote runs

A restart, crash or upgrade of the daemon fails every live **remote** run:
the actor runner stops the VM when its bridge breaks (an `--ephemeral` runner
then exits), and the next daemon marks the run `FAILED` ("the original
runtime holder or actor was lost; continuity could not be verified").
Executor runners keep a run's processes for 120 s, then end them. On
Kubernetes the holder is in the daemon's container and goes with it. Code;
see [rc guide §2](../ai/references/architecture/2026-11-26-rc-integration-guide.md#what-a-daemon-restart-does-to-running-actors-code).
Restart the daemon between runs.

### Scale to zero

The daemon can be stopped with no live run, but its `NEEDY_STATE_DIR`
(`state.db`, `trace.db`, run directories) is the only copy of paired
clients, minted runners and every run's history. On an `emptyDir` all of it
is lost when the Pod stops. Keep the state on a PVC, or keep a grace period
after the last run (the `rc` session uses "until the runner credential is
revoked, plus 10 minutes").

### Health and readiness

`needy config context check local -q --timeout 2s` checks the daemon through
its management socket. Use it as an **exec** probe: a `tcpSocket` probe
comes from the node, and CNIs that apply policy to kubelet traffic (Cilium
among them) refuse it under a daemon ingress policy, leaving the Service
without endpoints (probe). needy's health service is
`needy.v1.HealthService/Check`, not `grpc.health.v1.Health`, so Kubernetes'
gRPC probe does not fit. `needy runner ls` shows runners, their roles and
whether they are online.

### Logs and reports

- Daemon and runner logs go to stderr (`-v` or `RUST_LOG=info`); a service's
  logs are in the journal or launchd's log.
- `needy run logs RUN` streams an actor's console; a remote actor's console
  log is in the runner's `--work-dir`.
- `needy run report RUN` (`--level detailed`, `-o markdown|json`) renders a
  run from the daemon; `--offline` reads this machine's
  `NEEDY_STATE_DIR/trace.db` instead. Remote actors' gateway events are
  forwarded into the daemon's `trace.db` (since v0.0.19).

### Known gaps

| Gap | Effect | Workaround |
| --- | --- | --- |
| `via` is not shown by `run report` | Which upstream a request used is only in the raw events | Read `net.*` events (`GetRunEvents`, `-o json`) |
| A CONNECT refused by Mihomo shows as `net.connect` | Mihomo accepts the SOCKS request before applying its rules; the actor sees a TLS failure, the trace no `net.error` | Do not alert on `net.error` alone |
| No gateway connect timeout | A SYN dropped by a NetworkPolicy hangs until the client gives up | Client-side timeouts |
| Remote console streaming is latency-bound | About 10 KB per round trip: 7.2 MB took about 370 s at 0.5 s RTT and about 28 s at 0.25 s (probe) | Keep consoles small over long links; prefer headless runs |
| An invalid `--runners-file` (or `--clients-file`) only warns | The previous entries stay; at start-up the file's runners are simply missing | Validate the file before deploying; watch for the warning |
| A long CONNECT tunnel is not activity | `last_activity_ms` stops advancing once a tunnel opens, so idle timeouts can fire under a long download | Idle timeouts above the longest transfer |
| A minted runner shows online before it connects (G7) | `CheckProfile` fails with empty capabilities | Wait for `capabilities.actor` to list `LINUX_FIRECRACKER` |
| No TLS listener (G4); no switch forbidding control-host placement (G3) | See the rc guide | Tunnels, policies; set every `runner:` |
| Network chaos on runner-gateway runs | `run chaos add`/`rm` of a `net` fault is refused (`TODO(runner-gateway-chaos)`) | Declare network faults in the profile |

## Evidence

All on 2026-11-26. Hosts are described by role and hardware. Counts are
pass / fail / not covered, as `cargo xtask e2e` reports them.

| What | Where | Version | Result | Level |
| --- | --- | --- | --- | --- |
| Non-root e2e matrix, v0.0.19: 20 failures, all fixed since (#122, #123, #128, #132) | Linux KVM workstation (x86_64, 20 threads, KVM, Docker) | v0.0.19 | 56 pass, 20 fail, 48 not covered | e2e ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#v0019)) |
| Cross-host remote actor: S1 egress from the runner host; SOCKS `via` and fail-closed; 7 of 31 runs failed on one path (fixed in #124, v0.0.20) | Control without KVM, actor runner on KVM | v0.0.19 | as stated | probe ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#v0019)) |
| `image build --from` gives the same snapshot digest as Linux; CLI cannot authenticate (`TODO(windows-acl)`) | Windows 11, CLI only | v0.0.19 | as stated | probe ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#v0019)) |
| Remote runs, two paths, including 7.2 MB consoles | Control without KVM, actor runner on KVM | v0.0.20 | 62 of 62 finished | probe ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#v0020-re-verification)) |
| Non-root e2e matrix | Linux KVM workstation | v0.0.21 code (#128) | 75 pass, 1 fail (SIGTERM drain, fixed in #129, v0.0.21), 48 not covered | e2e ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#128-live-v0021-code)) |
| Non-root e2e matrix | Host A: x86_64 cloud VM, Docker, no KVM | v0.0.21 | 39 pass, 0 fail, 85 not covered | e2e ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#v0021-non-root-pass-on-a-b-and-c)) |
| Non-root e2e matrix | Host B: x86_64, KVM, Docker, behind a fake-IP proxy | v0.0.21 | 70 pass, 6 fail (5 from the host's network, 1 sensor cell without hwmon, skipped since #134), 48 not covered | e2e (same) |
| Non-root e2e matrix, two rounds | Host C: x86_64 cloud VM, Docker with gVisor, no KVM | v0.0.21 | 73/1/50 and 72/2/50 (the sensor cell and a terminal-drain flake, #134) | e2e (same) |
| Holder adoption: a v0.0.22 daemon adopted a running v0.0.21 holder; the run finished | Linux KVM workstation | v0.0.21 to v0.0.22 | as stated | probe ([pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md#holder-adoption-across-v0021-to-v0022)) |
| S1: daemon traffic for a 50 MB actor download fell from 50 MB to about 16 KB; mixed versions fell back; runner enforced `--gateway-block-cidr` | Self-managed Kubernetes cluster, Flannel and Cilium chained, KVM node | S1 branch, v0.0.19 | as stated | probe ([design, Evidence](../ai/references/architecture/2026-11-26-runner-gateway-design.md#evidence-probe-2026-11-26)) |
| Per-actor `socks5h` upstream, fail closed, Secret rotation | Same cluster | S1 branch, v0.0.19 | as stated | probe (same) |
| Mihomo filtering upstream; Workspace TUN sidecar with a NetworkPolicy backstop | Same cluster; managed EKS KVM pool | v0.0.19 | refused private and cluster names; sidecar fails closed only with the backstop | probe ([rc guide §8](../ai/references/architecture/2026-11-26-rc-integration-guide.md#8-networking-and-security)) |
| `rc` session end to end: daemon, ephemeral actor runner Pod, NetworkPolicies, upstream | Managed EKS KVM node pool (`/dev/kvm` gid 36, mode `0666`) | v0.0.19 | 50 MB download moved about 16 KB through the daemon Pod; cluster API and metadata denied | probe ([rc guide §5, §8](../ai/references/architecture/2026-11-26-rc-integration-guide.md#kvm-scheduling-probe)) |
| Runner gateway, upstream, static secrets, holder adoption | Repository tests | v0.0.22 | pass | in-process (tests listed in the linked notes) |

The per-host cells and failures of the v0.0.19 to v0.0.22 passes are in
the [multi-host pass note](../ai/references/architecture/2026-11-26-multi-host-pass-evidence.md).
