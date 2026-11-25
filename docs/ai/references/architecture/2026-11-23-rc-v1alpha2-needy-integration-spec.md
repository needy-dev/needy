# Spec: `rc` Needy Integration and Bare-Metal KVM IaaS Loop (Phase 2)

Status: **proposed spec**, 2026-11-23.
Parent design: [Needy on Kubernetes with `rc` and Managed IaaS](2026-11-23-kubernetes-rc-integration-design.md).
Prerequisite: [Remote Actor Host Transport and K8s-Friendly Runner Ergonomics (Phase 1)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md).

> **Revision notice (2026-11-26):** Parts of this spec changed on
> 2026-11-25 and 2026-11-26; the text below is kept as written.
>
> - The KVM pool is not bare metal: it is a nested-virtualization pool of
>   EC2 8th generation Intel families on the stock EKS AL2023 AMI (no LXCFS
>   or DMI mask), tainted, with `/dev/kvm` exposed by a generic-device-plugin
>   as an extended resource (named by the pool's device plugin, for example `example.com/kvm`). The bare-metal
>   sizing and warm-capacity notes below no longer apply as written.
> - `rc`'s `Workspace` already has `hotMount`, `lxcfs`, `supplementalGroups`
>   and `podReplacementPolicy` (`api/workspaces/v1alpha1/workspace_types.go`
>   in `rc`); `needyExecutor` and `Actor` remain to be added.
> - `devices-api` only creates `rc` resources and keeps quota and billing;
>   `rc` owns the namespace's `needy-daemon`, the `Actor` reconciler and
>   Workspace executor runners (owner decision, 2026-11-26).
> - Image layers follow
>   [One layered OCI base for VM and container actors](2026-11-25-layered-oci-base-spec.md),
>   not `needy image edit` pushes.
> - Current integration details, verified against needy's code, are in the
>   handoff [Integrating needy into `rc`](2026-11-26-rc-integration-guide.md).

## Problem Statement

Running Needy's actor-executor architecture on Kubernetes with `rc` and a
managed IaaS layer currently hits several hard constraints:
- `rc` forces every Linux `Workspace` Pod to carry a Home PVC, `rc-kube` as
  PID 1, `hostPath` worktree hot-mounts, optional LXCFS `/proc/cpuinfo`
  overrides (which break KVM CPU probing), and a high-privilege default
  ServiceAccount Role (`secrets`, `pods/exec`) unless disabled.
- `rc` creates no Services or NetworkPolicies, and `Workspace` topology
  replacement ignores active `spec.services` (risking mid-flight kills of
  Executor Pods running Needy tasks).
- The managed IaaS layer (`devices-api`) hardcodes a single `Workspace` GVR
  and `Running <-> Suspended` lifecycle model, lacks a `backend_kind`
  discriminator, and its warm-capacity controller assumes a single uniform
  node pool per AZ without KVM extended resources or node taints.

## Solution

Extend `rc` in a backward-compatible way (`v1alpha1`, avoiding CRD Conversion
Webhook overhead): retain heavy persistent `Workspace` resources for
**Executors** (with explicit opt-out toggles, secure default SA lockdown when
Needy is enabled, and busy-lease protection), add a lightweight **`Actor` CRD**
for pure bare-metal KVM Firecracker Pods, and let `rc-controller` lazily
reconcile a **per-namespace `needy-daemon`** (`Recreate` strategy, explicit
`storageClassName`, strict anti-SSRF NetworkPolicies). Extend the IaaS control
plane with a bare-metal KVM node pool (dedicated AMI without `lxcfs`/DMI-mask,
Node Taint, KVM Device Plugin, pre-pulled base VM images) and `Actor`
observation/accounting support.

## User Stories

1. As a developer, I want to create an `rc` `Actor` resource using a
   Needy-compatible profile configuration, so that a profile tested locally
   with Needy runs identically in Kubernetes.
2. As a developer, I want my existing `rc` `Workspace` resources (Linux
   Worktree, cache acceleration, Wayland GUI, Windows, GPU) to opt in as Needy
   Executors, so that a single Firecracker Actor can route commands across
   heterogeneous `rc` Workspaces.
3. As a developer, I want multiple Actors in my namespace to route to multiple
   Executor Workspaces (M:N mesh) using Needy's native runner selectors and
   labels, so that Actors and Executors stay loosely coupled.
4. As a platform operator, I want `rc` `Actor` Pods to be pure (no `rc-kube`,
   no Home PVC, no `hostPath` hot-mounts, no LXCFS, and
   `automountServiceAccountToken: false`), so that Firecracker runs cleanly
   with `/dev/kvm` and zero privileged escalation or double-PTY nesting.
5. As a platform operator, I want explicit CRD toggles on `Workspace` for
   `hotMount`, `lxcfs`, and `supplementalGroups` (alongside the existing
   `automountServiceAccountToken` field), so that operators can control mount
   and security boundaries per Workspace.
6. As a security engineer, I want any `Workspace` with `needyExecutor` enabled
   to default to `automountServiceAccountToken: false`, so that an Executor
   Pod cannot use the namespace's default workspace Role to read `needy-daemon`
   Secrets or `pods/exec` into Actor or daemon Pods.
7. As a platform operator, I want `rc-controller` to inject the static `needy`
   binary via an initContainer (mirroring `rc-kube` injection), so that user
   and runner images do not bake in a fixed Needy version.
8. As a platform operator, I want `rc-controller` to lazily create a
   per-namespace `needy-daemon` (Deployment with `Recreate` strategy,
   ClusterIP Service, Secret, and a 1–5 GiB PVC with explicit
   `storageClassName`) when Needy resources exist in a namespace, hot-reloading
   static runner Secrets without restarting the daemon, and scaling to zero
   only after a grace period with zero active Actors or Executors.
9. As a security engineer, I want `rc-controller` to configure `needy-daemon`
   with `--disable-host-files` and enforce two NetworkPolicies per namespace—
   locking Actor egress strictly to the namespace's `needy-daemon` Service, and
   blocking `needy-daemon` egress to foreign namespaces, cluster-private ranges
   (except same-namespace Executors), the Kubernetes API Server, and cloud
   metadata (`169.254.169.254`)—layered on top of Needy's in-process
   resolve-then-validate SSRF guard.
10. As a platform operator, I want `rc` `Actor` resources to honor
    `idleTimeout` (queried via `needy-daemon`'s `last_activity_ms`) and
    `maxRunDuration`, so that abandoned Actor Pods never become zombies.
11. As a developer, I want an Executor `Workspace` running `needy runner serve`
    to report activity to `rc-kube` while tasks run and be protected from
    destructive topology rebuilds while busy, while still honoring
    `maxRunDuration` hard stops so wedged executors are eventually reclaimed.
12. As a developer using `rcctl`, I want `rcctl actor` (`create`, `ls`,
    `attach`, `logs`, `report`, `rm`) to reach the namespace's ClusterIP
    `needy-daemon` seamlessly over Kubernetes `pods/portforward` (using the
    namespace's client credential Secret and automatically waking a
    scaled-to-zero daemon when reading historical `report`s), while all
    existing `rcctl` commands keep working unchanged.
13. As a platform operator, I want the IaaS layer to provision a tainted
    bare-metal KVM Karpenter `NodePool` using a dedicated AMI (without `lxcfs`
    or DMI-mask interference), a KVM Device Plugin, and pre-pulled base VM
    images (`vmlinux` and `rootfs.ext4`), without polluting gVisor warm-pool
    calculations.
14. As a platform operator, I want `devices-api` to distinguish `Actor` from
    `Workspace` via an explicit backend resource kind, properly close usage
    billing segments when an ephemeral `Actor` reaches a terminal state
    (`Succeeded`/`Stopped`/`Failed`) rather than flagging it as
    `ReasonBackendMissing`, and apply the same per-principal quota ledger.

## Implementation Decisions

- **Backward-Compatible `rc` API Evolution (`v1alpha1`)**:
  - Keep `Workspace` on `v1alpha1` by adding only optional fields (`hotMount`,
    `lxcfs`, `supplementalGroups`, `needyExecutor`, and optional
    `podReplacementPolicy`) and introducing `Actor` as a new sibling CRD. Avoid
    introducing a `v1alpha1 <-> v1alpha2` Conversion Webhook unless breaking
    schema changes are later approved.
- **`rc` `Workspace` Executor Mode**:
  - When `needyExecutor` is configured, `rc-controller`:
    - Defaults `automountServiceAccountToken` to `false` (rejecting or warning
      on insecure overrides).
    - Injects the static `needy` binary via an initContainer.
    - Adds the executor's static runner credential to the namespace's
      `needy-daemon` static runners Secret (consumed via Phase 1 hot-reload
      without restarting the daemon) and mounts the executor's own token into
      the Workspace Pod.
    - Runs `needy runner serve` in `spec.services` with `--address`,
      `--name`, `--credential-env`, and `--activity-hook` invoking
      `rc-kube process touch`.
  - Protect busy Executor Pods from topology replacement via a lightweight
    `rc-kube` busy lease or `podReplacementPolicy: OnSuspend`, while ensuring
    `maxRunDuration` always overrides the busy lease so stuck executors cannot
    bypass hard run limits.
- **`rc` Lightweight `Actor` CRD, Independent Credential Lifecycle & Retention**:
  - `Actor.spec` separates typed Kubernetes orchestration fields (`resources`
    including KVM, `nodeSelector`/`tolerations`, `idleTimeout`,
    `maxRunDuration`, `deleteAfterCompleted`, `command`, and `env` with
    `secretKeyRef`) from the **native Needy profile passthrough**
    (`spec.profile.{builtin | configMapRef | inline}`), so `rc` never
    duplicates Needy's profile schema and delegates all YAML validation and
    `extends` resolution directly to `needy-daemon`.
  - `ActorReconciler` manages an independent per-Actor credential lifecycle:
    it ensures the namespace's `needy-daemon` is Ready, mints a short-lived
    `Runner Credential` (TTL bounded by `maxRunDuration` + grace) into an
    `Actor`-owned Secret (cascaded by Kubernetes GC and explicitly revoked via
    `RevokeRunnerCredential` on completion/deletion), launches a pure Actor
    Pod (no `rc-kube`, no Home PVC, no `hot-mount`, no LXCFS,
    `automountServiceAccountToken: false`, `restartPolicy: Never`) running
    `needy runner serve --roles actor --ephemeral`, and calls `CreateRun`
    targeting `actor.runner: actor-<name>`.
  - `Actor.status` exposes terminal states (`Succeeded`, `Stopped`, `Failed`)
    plus `RunningSince`, `LastActivityTime`, `CompletedAt`, and
    `LastAutomaticSuspendReason` to align with IaaS observation.
  - `ActorRetentionReconciler` mirrors `WorkspaceRetentionReconciler`: queries
    `needy-daemon`'s `last_activity_ms` on `idleTimeout`, enforces
    `maxRunDuration` unconditionally, and garbage-collects completed `Actor`
    Pods, Secrets, and CRDs after `deleteAfterCompleted`.
- **Per-Namespace `NeedyDaemonReconciler`**:
  - Expand `rc-controller`'s ClusterRole with `apps/deployments`, `services`,
    and `networking.k8s.io/networkpolicies`.
  - Reconcile one Deployment (`strategy.type: Recreate`, `--disable-host-files`,
    static runners Secret with hot-reload), ClusterIP Service, small PVC
    (1–5 GiB using explicit configurable/zone-matched `storageClassName`), and
    the two namespace NetworkPolicies.
  - Never scale to zero while any `Actor` is live; scale to zero after an idle
    grace period once all Needy workloads are stopped/suspended, preserving the
    PVC.
- **`rcctl actor` Transport**:
  - Implement `rcctl actor attach/logs/report` over Kubernetes
    `pods/portforward` to the namespace's `needy-daemon` Pod using the
    namespace client token Secret. When querying `report` against a
    scaled-to-zero daemon, either wake the daemon transiently until Ready or
    report an actionable wake instruction.
- **Managed IaaS (`devices-api` & Pulumi Infra)**:
  - Add a bare-metal KVM Karpenter `NodePool` with a dedicated Node Taint and
    bare-metal AMI (excluding `lxcfs` and DMI-mask), KVM Device Plugin, and
    node-local read-only base VM image pre-pulling.
  - Isolate warm-capacity calculations by node pool / device class so
    bare-metal KVM workloads never skew gVisor placeholder sizing.
  - Extend `devices-api` schema (`backend_kind` on device records and
    `BackendRef`), `DeviceClass` (nodeSelector, tolerations, KVM extended
    resources), and `rcbackend` observation so ephemeral `Actor` terminal
    transitions close `usageevents` segments cleanly without triggering
    `ReasonBackendMissing`.

## Testing Decisions

- **What makes a good test**: Assert rendered Kubernetes manifests, RBAC/SA
  defaults, controller status transitions, NetworkPolicy enforcement, and
  billing/quota ledger outputs—not internal reconciler helper calls.
- **`rc` controller seam (prior art: existing `controller-runtime` tests)**:
  - Verify `Workspace` toggles, `needyExecutor` forced-default
    `automountServiceAccountToken: false`, initContainer/service injection, and
    busy-lease protection vs `maxRunDuration` override.
  - Verify `Actor` Pod purity (no `rc-kube`, no Home PVC, no `hot-mount`, no
    LXCFS, SA disabled) and daemon-Ready ordering before Pod creation.
  - Verify `NeedyDaemonReconciler` `Recreate` strategy, explicit
    `storageClassName`, NetworkPolicies, static Secret hot-reload path, and
    scale-to-zero guard while Actors are active.
  - Verify `ActorRetentionReconciler` using a thin fake activity client seam
    isomorphic to `rc`'s existing `ActivityReader`.
- **IaaS ledger seam (prior art: existing `rcbackend` + SQLite tests)**:
  - Verify `Actor` and `Workspace` coexist via `backend_kind`, quota checks
    isolate pools properly, and terminal `Actor` completion closes usage
    segments cleanly (not `ReasonBackendMissing`).
- **Cluster security & e2e seam**:
  - Verify on a real cluster: multi-executor routing, Executor SA blocked from
    reading `needy-daemon` Secrets, cross-namespace and Gateway SSRF denial
    (including loopback and metadata), `rcctl actor report` surviving
    scale-to-zero, and active Executor survival past `idleTimeout`.

## Out of Scope

- External user device penetration via device gateway and client CLI (Phase 3).
- Pushing and pulling `needy image edit` layers via tenant OCI Registries
  (Phase 3).
- Non-KVM gVisor `in-pod` shim fallback (Phase 3).
- `kubernetes-sigs/agent-sandbox` compatibility adapter controller.

## Further Notes

- Keeping `Workspace` on `v1alpha1` avoids deploying cert-manager and
  maintaining bidirectional CRD conversion webhooks during early rollout.
- Because bare-metal instances are large (e.g., 96 vCPU), warm capacity for
  the bare-metal pool should use explicit pool-level minimum node/slot targets
  rather than per-device placeholder scaling.
