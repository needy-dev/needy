# Architecture

Cross-cutting design decisions, shared vocabulary and project scope.

| Date | Note | Type |
| --- | --- | --- |
| 2026-11-06 | [Needy design decisions](2026-11-06-needy-design-decisions-design.md) | design |
| 2026-11-09 | [Actor and executor separation](2026-11-09-actor-executor-separation-design.md) | design |
| 2026-11-09 | [Closed-loop milestone](2026-11-09-closed-loop-milestone-plan.md) | plan |
| 2026-11-09 | [Closed-loop results](2026-11-09-closed-loop-results-evidence.md) | evidence |
| 2026-11-09 | [Module layout redesign](2026-11-09-module-layout-redesign.md) | design |
| 2026-11-10 | [Container actor runtime](2026-11-10-container-actor-runtime-design.md) | design |
| 2026-11-10 | [Runners and runs](2026-11-10-runner-and-run-design.md) | design |
| 2026-11-10 | [Provenance inventory](2026-11-10-provenance-inventory-evidence.md) | evidence |
| 2026-11-12 | [`macos-vz` actor runtime](2026-11-12-macos-vz-actor-evidence.md) | evidence |
| 2026-11-12 | [Apple `container` actor](2026-11-12-macos-apple-container-actor-evidence.md) | evidence |
| 2026-11-12 | [Drivers and extension points](2026-11-12-drivers-and-extension-points-design.md) | design |
| 2026-11-12 | [Release and CI](2026-11-12-release-and-ci-design.md) | design |
| 2026-11-12 | [Actor images](2026-11-12-actor-images-design.md) | design |
| 2026-11-13 | [End-to-end harness](2026-11-13-e2e-harness-design.md) | design |
| 2026-11-13 | [needy through a Cloudflare Tunnel](2026-11-13-cloudflare-transport-evidence.md) | evidence |
| 2026-11-14 | [CLI, discovery, authority, and lifecycle (consolidated proposal)](../cli/2026-11-14-cli-configuration-and-access-spec.md) | spec |
| 2026-11-14 | [Cloudflare docs deployment](2026-11-14-cloudflare-docs-deployment-design.md) | design |
| 2026-11-15 | [Deployment topology and daemon responsibilities](2026-11-15-topology-and-daemon-responsibilities-design.md) | design |
| 2026-11-15 | [Actor survival, reconnection and history](2026-11-15-actor-survival-and-history-design.md) | design |
| 2026-11-15 | [Client execution and actor lifecycle (approved contract)](2026-11-15-client-execution-and-actor-lifecycle-spec.md) | spec |
| 2026-11-15 | [Actor holder and control recovery](2026-11-15-actor-holder-implementation-evidence.md) | implementation evidence |
| 2026-11-15 | [Client execution and lifecycle integration](2026-11-15-client-lifecycle-integration-evidence.md) | integration evidence |
| 2026-11-15 | [Client lifecycle acceptance record](2026-11-15-client-lifecycle-acceptance.json) | sanitized results: 12 cells, 486 checks |
| 2026-11-15 | [Lifecycle acceptance without KVM](2026-11-15-non-kvm-lifecycle-acceptance.json) | separate non-root runc host: 5 cells, 201 checks |
| 2026-11-16 | [Images as layers (decided, not implemented)](2026-11-16-environments-and-snapshots-design.md) | design |
| 2026-11-16 | [Where a run's data lives (decided, not implemented)](2026-11-16-data-placement-design.md) | design |
| 2026-11-16 | [Agent state on live mounts: Codex in concurrent VM actors](2026-11-16-agent-state-on-live-mounts-evidence.md) | evidence |
| 2026-11-16 | [Agent state in the actor, merged back at stop: spikes](2026-11-16-agent-state-merge-spike-evidence.md) | evidence |
| 2026-11-16 | [Container capture: trampolines on PATH, shells in place](2026-11-16-container-trampolines-design.md) | design |
| 2026-11-17 | [Image definitions, hand edits and merged run data (approved contract)](2026-11-17-images-and-run-data-spec.md) | spec |
| 2026-11-17 | [Slice 0: official Dev Container features on needy's base](2026-11-17-features-on-needy-base-evidence.md) | evidence |
| 2026-11-17 | [Slice 1 plan: snapshots and hand edits on the VM drivers](2026-11-17-images-slice-1-plan.md) | plan |
| 2026-11-21 | [Slice 1: snapshots and hand edits on VM drivers](2026-11-21-images-slice-1-evidence.md) | evidence |
| 2026-11-21 | [Slice 2: container snapshots, trampolines, and container build fallback](2026-11-21-images-slice-2-evidence.md) | evidence |
| 2026-11-25 | [Image layers for remote actors and a layer registry (approved)](2026-11-25-remote-actor-images-spec.md) | spec |
| 2026-11-21 | [Slice 3: Dev Container features as layers](2026-11-21-images-slice-3-evidence.md) | evidence |
| 2026-11-22 | [Codebase architecture, Rust idioms, CLI, and ecosystem review](2026-11-22-codebase-architecture-review.md) | review |
| 2026-11-22 | [Architecture cleanup, CLI unification, and standalone image crates (approved contract)](2026-11-22-architecture-cleanup-and-crate-extraction-spec.md) | spec |
| 2026-11-22 | [Transparent network gateway (proposed spec)](2026-11-22-transparent-network-gateway-spec.md) | spec |
| 2026-11-23 | [Needy on Kubernetes with `rc` and Managed IaaS](2026-11-23-kubernetes-rc-integration-design.md) | design |
| 2026-11-23 | [Phase 1: Remote actor host transport and K8s-friendly runner ergonomics (proposed spec)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md) | spec |
| 2026-11-23 | [Phase 2: `rc` Needy integration and bare-metal KVM IaaS loop (proposed spec)](2026-11-23-rc-v1alpha2-needy-integration-spec.md) | spec |
| 2026-11-23 | [Phase 3: External client penetration and cloud OCI image layers (proposed spec)](2026-11-23-external-client-penetration-and-oci-layers-spec.md) | spec |
