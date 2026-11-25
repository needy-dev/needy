# Spec: One Layered OCI Base for VM and Container Actors

Status: **approved**, 2026-11-25 (owner review of #107). Names marked *provisional* may change in
review. This revises and completes slice 5 of
[Image definitions, hand edits and merged run data](2026-11-17-images-and-run-data-spec.md)
("the needy layer on any root; the default root") for remote actors and
standard OCI tooling.

Related notes:

- [Image layers for remote actors and a layer registry](2026-11-25-remote-actor-images-spec.md):
  D1-D4 (remote runners attach layer disks) stand; D5 (a needy-specific
  layer artifact) is under revision in favour of this note.
- [Actor images](2026-11-12-actor-images-design.md): the base ABI and the
  image directory a VM driver boots.

## Problem

needy prebuilds its base, but as two flattened artifacts with different
content and different identities (`images/Dockerfile`):

```text
actor-base (ubuntu:24.04 + packages + user agent:1000)
 ├─ actor-root (+ systemd, /.needy stubs) -> one rootfs.ext4, in a release tarball (no OCI layers)
 └─ container-root (no systemd, shells shimmed in place) -> one-layer image ghcr.io/needy-dev/needy-actor
```

Consequences observed while building remote actors:

1. A layer built by the container builder cannot be reused by a VM actor:
   step cache keys include the root digest, which is the `rootfs.ext4` hash
   for the VM builder and the container image ID for the container builder
   (`root_digest_for` in `crates/daemon/src/image/build.rs`).
2. `actor.image: REF` (an image `FROM needy-actor`) runs on a VM only after
   `needy image build --from REF` exports the whole image with BuildKit,
   undoes the container shims and writes a new `rootfs.ext4`
   (`crates/cli/src/commands/image/build/from.rs`, `from.Dockerfile`). A
   remote runner without an engine cannot do it.
3. Images built by standard tools (BuildKit, `devcontainer build`,
   Shipwright, envbuilder) cannot be recognised as "needy's base plus
   these layers", because the VM base has no OCI layers to compare with.
4. Sharing built layers needed a needy-specific registry artifact (D5 of
   the remote actor images spec), which standard registries and tools do
   not understand.

The owner's direction (2026-11-25): prebuild the root file system and the
core operating system layers, not agents (agents stay features; "needy
ships no built-in images" stands); reuse standard OCI images, registries and
builders; keep needy's own builder for local work and egress-traced builds.

## Decisions

### B1. Published base images, named by what they are

needy publishes only the images users or drivers pull, under the
repository's package namespace, each named by its use:

| Image | What it is | Why it is published |
| --- | --- | --- |
| `ghcr.io/needy-dev/needy/actor-base` | The start of every actor image, shared by the VM and container drivers | Users and CI build agent images `FROM` it; needy recognises the layers above it (B4) |
| `ghcr.io/needy-dev/needy/executor-base` | The `linux-container` executor's image (today `needy-executor`) | The `linux-container` executor driver pulls it |

Each is an OCI image index for `linux/amd64` and `linux/arm64`, published by
the release workflow with build provenance attestations. Tags are
`<needy version>-<distribution>`, for example
`actor-base:0.0.17-ubuntu-24.04`. Profiles and `FROM` lines pin a digest; the
tag is for people. The first release publishes `ubuntu-24.04` only (today's
root); another distribution is another tag, built on its own root, so its
DiffIDs and cache keys differ by construction.

`actor-base`'s layers, lowest first:

| Layer | Content | Source |
| --- | --- | --- |
| root | The distribution's own layers, unchanged | `ubuntu:24.04` pinned by digest (images spec: "a plain distribution image pinned by digest") |
| userland | Packages and the `agent` user (today's `actor-base` stage) | `images/Dockerfile` |
| needy | `/.needy` stubs and the base ABI (`needy.base-abi` label); the same for every distribution, since the stubs are static | `images/Dockerfile` |

What only one driver needs is not published as an image, because it is
needy's implementation and a published name would only add one more thing
to understand:

- The VM's own additions (systemd, which reads the recipe's DMI identity
  through `systemd-detect-virt`; `/home/agent/workspace`) go into the
  `rootfs.ext4` the release derives from `actor-base` (B3).
- The container shims are synthesized when a container image is assembled,
  as the layer store already does for snapshots (`synthesize_shim_layer` in
  `crates/core-actor/src/image/store/oci.rs`).

### B2. The base's identity is its layers

The root digest that step cache keys and `BridgeAttached.root` name becomes
the base's stack digest: the chain of the base layers' DiffIDs for the
actor's platform. It is the same whichever builder built a step, so a layer
built in a container, in a VM session, in CI or by a standard builder is
reusable by every driver of that platform. `needy image pull` records the
base's image digest and DiffIDs in the VM image's `manifest.json`.

### B3. The VM keeps booting a `rootfs.ext4`, derived from the base

The release workflow writes `rootfs.ext4` from the base's layers (as
`actor-image` does today from `actor-root`) and records which base it was
derived from. The guest init and boot arguments do not change; feature and
image layers keep stacking above it as layer disks.

`TODO(base-layer-disks)`: booting a minimal root device with the base's
layers as layer disks would remove the derived `rootfs.ext4`; deferred
because it changes the guest init's boot path, and nothing needs it yet.

### B4. `actor.image` imports layers above the base

For `actor.image: REF` on a VM driver, needy (the control daemon, or a
remote runner per D2 of the remote actor images spec) pulls the image's
manifest for the actor's platform, checks that its first layers are exactly
the base's (by DiffID) and that it records this needy's base ABI, and pulls
only the layers above the base, by digest, into the layer store. They are
attached as layer disks like feature layers. An image that does not start
with the base's layers is refused with an error naming the base it needs.

`from.Dockerfile` and the BuildKit path of `needy image build --from` are
removed; `--from` becomes an import into the layer store.

Pulling uses `oci-client` with standard credentials and TLS: Docker
credential store (`DOCKER_CONFIG`), the system trust store
(`rustls-tls-native-roots`), containers-style `certs.d/<host:port>/*.crt`,
and a per-host `--insecure-registry HOST` *(provisional)*. Plain HTTP stays
an explicit opt-in that never sends credentials. Each process resolves its
own credentials; the control daemon never forwards them. (From the registry
auth and TLS research of 2026-11-25; details move to the slice that
implements them.)

### B5. Building on the base, inside or outside needy

- needy's own builder (VM sessions) builds steps on the base identity of
  B2, pushes each step as an ordinary OCI image manifest (standard config,
  `rootfs.diff_ids`, needy's metadata in annotations) and the final snapshot
  as an ordinary image. This replaces D5's custom artifact.
- Standard builders build `FROM ghcr.io/needy-dev/needy/actor-base@sha256:...`
  and push an ordinary image that B4 imports: `devcontainer build`, BuildKit
  or Buildah, Shipwright `BuildRun`s, envbuilder. They do not record the
  egress trace needy's builder records.
- *Provisional:* a one-shot `needy image build --push REF --output json`
  that needs no resident daemon, so CI jobs and in-cluster builders (a
  Shipwright `ClusterBuildStrategy` on a KVM node) can run needy's builder.

### B6. Compatibility

- `ghcr.io/needy-dev/needy-actor` and `ghcr.io/needy-dev/needy-executor`
  stay published for a transition period, assembled from `actor-base` (plus
  the shims) and `executor-base`, so existing `FROM needy-actor` images and
  container actors keep working. Their labels name the image they come from.
  `TODO(base-images)`: drop them once a release has shipped the new names and
  the docs and examples use them.
- Changing the root digest invalidates every step cache once (a new root);
  the base ABI does not change unless the boot contract does.

## Slices

| Slice | Scope | Depends on |
| --- | --- | --- |
| B-1 | Restructure `images/Dockerfile` and the release workflow: publish `actor-base` and `executor-base` (indexes, attestations, `ubuntu-24.04` tags); derive `rootfs.ext4`, `needy-actor` and `needy-executor` from them; `manifest.json` records the base | none |
| B-2 | Root identity: step cache keys and `BridgeAttached.root` use the base stack digest for every builder | B-1 |
| B-3 | Standard registry access: credentials, trust store, `certs.d`, `--insecure-registry`, the `identitytoken` fix; push steps and snapshots as ordinary OCI images (replaces #105) | B-2 |
| B-4 | `actor.image` import above the base (control daemon and remote runners); remove `from.Dockerfile` | B-1, B-3 |
| B-5 | One-shot `needy image build --push`; a CI example; runners take sources from the import (replaces #106) | B-3 |

Settled in B-1 (2026-11-25):

- `images/Dockerfile` targets: `actor-base` (`FROM ubuntu:24.04` pinned by
  index digest through `UBUNTU_IMAGE` and `UBUNTU_DIGEST`, then one userland
  layer and one needy layer) and `executor-base` (today's executor on the
  same pinned root; its own userland, since it is a toolchain image that
  carries needy). `actor-root` (the VM's `rootfs.ext4`) and
  `actor-container` (`needy-actor`) are `FROM actor-base`; `executor`
  (`needy-executor`) is `FROM executor-base`. The target names `needy image
  build` and the e2e harness use did not change.
- The needy layer holds `/.needy/{init,proxy,needy,disk,channels,base-abi}`
  and the `shim-image` script, so an image `FROM actor-base` that runs
  `RUN ["/.needy/shim-image"]` last is a container actor image as one `FROM
  needy-actor` is. `actor-base` has no shims and no entrypoint; its labels
  are `needy.base-abi` and `org.opencontainers.image.base.{name,digest}`
  naming the distribution root.
- `needy-actor` is `actor-base` and exactly one layer (the shim step); its
  labels `org.opencontainers.image.base.name` and, when the release pushed
  the base, `.digest` name the `actor-base` it comes from (empty when built
  outside the release). `needy-executor` has `executor-base`'s layers and
  the same labels for it.
- `manifest.json` gains an optional `base`: `image` (the published name and
  tag, or `needy-actor-base:latest` for `needy image build`), `diff_ids` (the
  base image's `rootfs.diff_ids`) and `digest` (the base's image manifest
  digest for the architecture, only when the release pushed it). Older
  manifests without it are read as before; an older needy refuses a
  manifest with it (`deny_unknown_fields`), which only a downgrade meets.
- The release workflow builds and verifies the bases on every run (the
  root's DiffIDs prefix both bases, `actor-base` has two layers above the
  root and no shims, `needy-actor` has `actor-base`'s layers and one more,
  the VM build used the verified `actor-base`), pushes them by digest on
  publishing runs only, tags `ghcr.io/needy-dev/needy/{actor-base,executor-base}:<version>-ubuntu-24.04`
  and attests each index with `actions/attest-build-provenance`.

## Testing

- Unit: DiffID prefix matching and refusal; the stack digest is equal for a
  step built by the container builder and by the VM builder on one base.
- In process: a remote run with `actor.image` naming an image `FROM` the
  base in an in-process TLS registry with basic auth, imported by a runner
  without an engine; an image on another base is refused.
- End to end: the `remote_actor` e2e cell with an image built `FROM
  ghcr.io/needy-dev/needy/actor-base` by `devcontainer build` (on a host with Docker) and booted on
  `linux-firecracker`.
- Live probes on Kubernetes (evidence only): an image built in CI or by a
  Shipwright `BuildRun`, pulled by a remote runner from a registry with
  credentials and a private CA.

## Open questions

1. Custom roots (`root:` in an image definition): publish the needy layer
   (stubs, base ABI; the same for every distribution) as its own artifact so
   it can go on any root, as the images spec planned, or keep custom roots on
   the container path only.
2. Which distributions follow `ubuntu-24.04` (for example `debian-trixie`,
   `ubuntu-26.04`): each adds the published images times the architectures to
   every release.
3. `macos-vz` and Apple's `container` share the arm64 base: confirm one
   arm64 index entry serves both.
4. Retention of base images on GHCR across releases.

Settled in review (2026-11-25): images are named by what they are under
`ghcr.io/needy-dev/needy/`; only `actor-base` and `executor-base` are
published; systemd and the container shims stay needy's implementation
(B1).
