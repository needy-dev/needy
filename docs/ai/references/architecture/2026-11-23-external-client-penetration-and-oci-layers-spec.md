# Spec: External Client Penetration and Cloud OCI Image Layers (Phase 3)

Status: **proposed spec**, 2026-11-23.
Parent design: [Needy on Kubernetes with `rc` and Managed IaaS](2026-11-23-kubernetes-rc-integration-design.md).
Prerequisites:
- [Remote Actor Host Transport and K8s-Friendly Runner Ergonomics (Phase 1)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md)
- [`rc` Needy Integration and Bare-Metal KVM IaaS Loop (Phase 2)](2026-11-23-rc-v1alpha2-needy-integration-spec.md)

> **Revision notice (2026-11-26):** Parts of this spec changed on
> 2026-11-25 and 2026-11-26; the text below is kept as written.
>
> - OCI layer distribution follows
>   [One layered OCI base for VM and container actors](2026-11-25-layered-oci-base-spec.md):
>   ordinary OCI images built on the published `actor-base`, a layer
>   registry of ordinary step images (B-3), `actor.image` import above the
>   base (B-4) and builds in CI or standard builders (B-5), instead of
>   pushing `needy image edit` layers.
> - "Bare-metal Actor Pods" are Actor Pods on nested-virtualization KVM
>   pools with a device plugin resource.
> - External user-device executors are out of the MVP: they need a cluster
>   gRPC ingress that nobody owns yet. See the handoff
>   [Integrating needy into `rc`](2026-11-26-rc-integration-guide.md).

## Problem Statement

After Phase 2, cloud Actors can route commands across in-cluster `rc` Executor
Workspaces, but two capabilities remain missing for a complete hybrid-cloud
workflow:

1. In the cloud application and external device gateway, a session currently
   assumes a 1:1 binding where the device hosting the agent harness is the same
   device executing commands. A cloud Firecracker Actor cannot transparently
   route commands (such as macOS builds or local file operations) to an online
   user device behind NAT without manual `needy runner join` setup.
2. Custom actor environments created with `needy image edit` cannot yet be
   distributed via a tenant OCI Registry to arbitrary bare-metal Actor Pods
   across the cluster, and nodes without `/dev/kvm` lack an optional
   container-shim fallback.

## Solution

Use the external device gateway strictly as a control-plane broker to deliver
Phase 1's short-lived, TTL-enforced Needy runner tokens and structured
allowlisted ingress parameters to online user devices. The client CLI spawns an
ephemeral `needy runner serve` process connecting through a unified
cluster-side byte-stream token-routing ingress into the tenant's per-namespace
`needy-daemon`. In parallel, connect `needy image edit` and bare-metal Actor
Pod startup to the tenant OCI Registry, and optionally support non-root
`in-pod` shim fallback on non-KVM nodes.

## User Stories

1. As a user, I want to start a cloud agent session that runs its harness in a
   cloud Firecracker Actor while targeting my online local Mac as an Executor,
   so that I get cloud reliability and policy auditing alongside local macOS
   toolchains and files.
2. As a user, I want a single cloud Actor session to target multiple Executors
   simultaneously (e.g., an in-cluster Linux build Workspace and my local Mac),
   so that each command routes to the right OS automatically.
3. As a user, I want my local device to join a cloud session automatically
   when my client CLI is already connected to the device gateway, so that I
   never have to manually run `needy runner invite` or `needy runner join`.
4. As a user, I want the temporary `needy runner` process spawned on my machine
   to terminate automatically when the session ends, when detached, or when its
   `idleTtl` expires, so that no zombie processes or stale remote access remain
   on my device.
5. As a security engineer, I want the client CLI to receive only structured,
   schema-validated ingress parameters (`host`, `port`, `routeToken`,
   `runnerToken`, `idleTtl`) and construct its own allowlisted argv locally—
   never executing a server-supplied command string—so that a compromised
   gateway or server cannot achieve arbitrary command execution on user
   devices.
6. As a security engineer, I want the brokered runner token to enforce a strict
   server-side `expires_ms` TTL (from Phase 1) and be scoped solely to the
   owning tenant's `needy-daemon`, so that a leaked token or missed detach RPC
   cannot grant indefinite access.
7. As a platform operator, I want raw gRPC and SSHFS streams from external
   clients to enter through a dedicated, unified cluster-side byte-stream
   token-routing ingress (rather than per-tenant DNS tunnels or serverless HTTP
   Workers), so that PTY streaming, signals, and large transfers scale cleanly
   across dynamic tenant namespaces.
8. As a reliability engineer, I want `needy.attachExecutor` RPC delivery in the
   device gateway to verify connection heartbeat freshness, return the actual
   serving `connectionId`, and allow `needy.detachExecutor` to target that
   exact `connectionId`, so that dead sockets and multi-channel (`cli` vs
   `desktop`) split-brain leaks are prevented.
9. As a developer, I want `needy image edit` layers (built locally or via a
   remote KVM Actor Pod) and lockfiles to push to my tenant OCI Registry and
   be pulled automatically by bare-metal Actor Pods on any node, so that
   customized agent environments schedule freely across zones.
10. As a platform operator, I want Actor Pods to materialize pulled OCI layers
    into read-only SquashFS layer disks in userspace without host mounts or
    privileged permissions, preserving bare-metal node security.
11. As a platform operator running a cluster without bare-metal KVM nodes, I
    want an optional non-root `in-pod` shim fallback mode for Actors (with
    explicitly documented trampoline coverage gaps and `copy-in`/`mirror-out`
    workspace semantics), so that Needy routing remains usable on standard or
    gVisor node pools.

## Implementation Decisions

- **Control-Plane Brokering over the External Device Gateway**:
  - Reuse the device gateway's existing RPC forwarding endpoint to dispatch
    `needy.attachExecutor` and `needy.detachExecutor` to a target `deviceId`
    under the caller's Principal (`user:<id>` or `workspace:<id>`).
  - Harden the gateway's RPC handler with stale-heartbeat checks (matching its
    existing agent-run freshness check) and optional `connectionId`-targeted
    routing so `detachExecutor` reaches the exact client channel that accepted
    `attachExecutor`. Filter out channels that do not advertise Needy runner
    capability.
  - Do not add raw TCP/gRPC data-plane relaying to the serverless gateway
    runtime.
- **Zero-RCE Structured Client CLI Supervision**:
  - The `needy.attachExecutor` RPC payload contains strictly structured fields:
    `ingressHost`, `ingressPort`, `routeToken`, `runnerToken`, `runnerName`,
    `labels`, and `idleTtl`. Arbitrary shell strings or connector command lines
    are rejected.
  - The client CLI validates these fields against an allowlist and spawns
    `needy runner serve` directly (with `--address`, `--name`, and
    `--credential-env` from Phase 1) using a fixed local transport connector
    argv, tracking the child in its task registry with an enforced `idleTtl`
    watchdog and clean `SIGTERM` teardown on detach, disconnect, or shutdown.
- **Unified Cluster Byte-Stream Token-Routing Ingress**:
  - Deploy a single cluster-side L4 / HTTP2-CONNECT byte-stream router
    (inspired by `kubernetes-sigs/agent-sandbox`'s router pattern) that
    validates the signed `routeToken` and forwards the raw byte stream to the
    target namespace's `needy-daemon` `RunnerService` ClusterIP Service. This
    avoids provisioning per-tenant DNS records or per-tenant external tunnels.
- **Cloud Session Model Decoupling**:
  - Decouple a session's **Actor** (harness host) from its **Target Executors**
    (`0..N` execution devices—in-cluster `rc` Workspaces and/or external user
    devices). The server mints a TTL-bounded runner token (Phase 1), brokers
    `needy.attachExecutor` before starting the Actor run, and calls
    `needy.detachExecutor` + token revocation when the session ends.
- **OCI Registry Distribution for `needy image edit`**:
  - Support pushing committed layers and lockfiles from `needy image edit`
    (executed either locally on a developer machine or via a remote KVM Actor
    Pod) to the tenant OCI Registry, and pulling/materializing them into
    read-only SquashFS layer disks in userspace on Actor Pod startup.
- **Optional Non-KVM `in-pod` Shim Fallback**:
  - Adapt `guest/init` to skip `setuid`/`chown` and `/etc/passwd` bind-mounts
    when already running as the target non-root UID, require a writable exec
    `emptyDir` at `/run/needy`, and declare non-FUSE workspace semantics
    (`copy-in`/`mirror-out` or shared PVC) with trampoline capture gaps
    recorded in `actor.start` and `needy run report`.

## Testing Decisions

- **What makes a good test**: Assert observable RPC authorization, stale-socket
  rejection, `connectionId`-pinned detach routing, spawned child-process
  lifecycle and RCE input rejection, and OCI push/pull-to-SquashFS fidelity.
- **Device gateway seam (prior art: existing gateway Vitest + Workers suite)**:
  - Verify `needy.attachExecutor` rejects stale connections, returns
    `connectionId`, and `needy.detachExecutor` routes to that exact
    `connectionId` even after a higher-priority channel connects.
- **Client CLI seam (prior art: existing client CLI connect integration tests)**:
  - Use a lightweight stand-in executable in place of `needy` to verify:
    structured parameter validation (rejecting malformed hosts/injected args),
    spawning with `--address`/`--name`/`--credential-env`, task registry
    tracking, and clean termination on detach, disconnect, or `idleTtl` expiry.
- **Needy OCI layer seam (prior art: existing `image::store` tests)**:
  - Push a committed `image edit` layer to a local OCI registry, pull it into a
    fresh store, materialize the SquashFS layer disk in userspace, and verify
    digest integrity and whiteout preservation.
- **End-to-end hybrid verification**:
  - Run a cloud Firecracker Actor routing commands to a local macOS machine
    connected via the client CLI through the cluster byte-stream ingress;
    verify command output, file access, trace report, server-side token expiry,
    and zero leftover local processes after session termination.

## Out of Scope

- Replacing the device gateway's existing browser HTTP/WebSocket preview
  tunnels (`<port>--<slug>.example.com`).
- Peer-to-peer NAT traversal (STUN/TURN/WebRTC) without a cluster ingress.
- Implementing a `kubernetes-sigs/agent-sandbox` compatibility adapter
  controller (optional future add-on).
- Hosting Windows or macOS Actors inside Kubernetes (external macOS/Windows
  machines participate as Executors or standalone local Needy Actors).

## Further Notes

- High-latency cross-WAN links make `sshfs` sensitive to metadata-heavy
  directory walks (`git status`, `npm`, `cargo`); profiles targeting remote
  user devices should route heavy commands directly to the remote executor
  rather than scanning the repository from the cloud Actor.
- Because the cluster byte-stream ingress terminates only the outer routing
  envelope and forwards opaque HTTP/2 gRPC bytes to `needy-daemon`, Needy's
  end-to-end bearer auth and streaming semantics remain intact.
