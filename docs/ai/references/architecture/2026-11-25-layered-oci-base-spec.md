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

Settled in B-2 (2026-11-26):

- The root (step cache keys' `root`, `BridgeAttached.root`, the remote
  runner's D4 check) is the base's stack digest: the OCI ChainID of
  `actor-base`'s `rootfs.diff_ids`, lowest first (image-spec v1.1.0,
  `config.md`, "Layer ChainID"; `oci_layer::chain_id`). `needy_core_actor::image::root`
  derives it: for a VM image from `manifest.json`'s `base.diff_ids`
  (`of_vm`); for a container builder from `needy-actor`'s DiffIDs without
  the last (the shim layer) (`of_container`). `needy-actor` is recognised by
  its name (as `is_needy_base_image` names it) and its
  `org.opencontainers.image.base.name` label, which must be empty (built
  outside the release) or name an `actor-base`; one with the label but
  another name, or fewer than two layers, is refused rather than guessed at.
- Images that record no base keep their old root: a VM image without
  `base` (made before B-1, or by `needy image build --from`) its
  `manifest.sha256` or the digest of its `rootfs.ext4`; a container root
  that is not needy's base, or a `needy-actor` from before B-1 (one layer,
  no label), the engine's image ID. `TODO(base-identity)` markers in
  `crates/core-actor/src/image/root.rs` name when to drop them.
- Every existing step cache key changes once for images that record a base
  (a new root): the old keys are no longer looked up, and their steps are
  built again once. Lockfile commits name the old top key and are applied
  as rebased (`commit_rebased`).
- The needy layer is not reproducible across builders, so a base built from
  a checkout (`needy image build`) has its own root, different from the
  release's; layers built on one are not reused on the other. The VM image's
  own additions (`actor-root`: systemd, the workspace directory) are not in
  the identity, so a layer built in a container is reused in a VM and the
  other way round, as B2 intends.
- `DEFAULT_ROOT` (`ubuntu:24.04`) is not part of the root: a definition
  without `root:` is built on needy's base. Pinning it by digest is left to
  `TODO(default-root)` in `crates/config/src/image/mod.rs`.

Settled in B-3 (2026-11-26):

- **Registry access** is one crate, `oci-registry` (no needy dependencies),
  used by Dev Container feature fetches and the layer registry, and by B-4's
  import next. `Access::connect` builds one `oci-client` client per registry
  host (TLS settings are client-wide in `oci-client` 0.15):
  - Trust: the system store and the bundled WebPKI roots
    (`oci-client` features `rustls-tls-native-roots` and `rustls-tls`; both
    select `ring`, so no second crypto provider), plus `*.crt` from the first
    `certs.d` directory that exists for the host, in this order:
    `$XDG_CONFIG_HOME/containers/certs.d/<host[:port]>` (default
    `~/.config/containers/certs.d`), `/etc/containers/certs.d`,
    `/etc/docker/certs.d`. "First wins" and reading `/etc/docker/certs.d`
    follow containers/image v5.36.0 (`docker/docker_client.go`,
    `dockerCertDir`), so a CA already set up for Docker or Podman works.
    A host with a port is looked up as containerd's `host_port_` first, then
    `host:port` (`hostport` on Windows, whose file names cannot hold `:`), as
    containerd's `hostPaths` does; `_default` is not read.
  - Credentials: the Docker credential store (`DOCKER_CONFIG` or
    `~/.docker`, `auths`, `credHelpers`, `credsStore`) through
    `docker_credential`, read again for each connection (feature fetches
    reuse a connection per host and repository for at most five minutes, or
    until an exchanged token expires). `REGISTRY_AUTH_FILE` is not read.
  - `identitytoken`: **exchanged**, not refused. The refresh token is POSTed
    to the realm of the registry's `Bearer` challenge with
    `grant_type=refresh_token`, `service`, `scope` and `client_id=needy`
    (distribution v3.0.0 `docs/content/spec/auth/oauth.md`, as
    containers/image's `getBearerTokenOAuth2` does), and the access token is
    given to `oci-client` as `RegistryAuth::Bearer`. The exchange is one
    form POST next to `oci-client`, so it was feasible without a fork. A
    registry without a `Bearer` challenge, an HTTP realm, or a refusal
    (including the spec's 404 fallback, since the GET flow needs a username)
    is `Error::IdentityToken`; the refresh token never reaches `/v2/`.
  - `--insecure-registry HOST[:PORT]` (repeatable) on `needy daemon serve`
    and `daemon.insecure_registries` in the settings file (Docker's
    `insecure-registries` name, per host as containerd's `skip_verify`)
    sets `accept_invalid_certificates` for that host's client only and warns
    on every connection. It is not plain HTTP and still sends credentials.
    `needy runner serve` does not take it yet: a runner contacts no registry
    until B-5 (`TODO(runner-registry)` in `crates/cli/src/commands/runner.rs`).
  - Plain HTTP is only an explicit `http://` and never carries credentials.
  - `TODO(registry-mtls)`: client certificates (`*.cert`/`*.key`) wait for
    `oci-client` to take a client identity. `TODO(registry-mirrors)`:
    `hosts.toml` and `registries.conf` are not read.
- **Step images** replace #105's custom artifact. Each built step is pushed
  as an ordinary OCI image tagged `key-<cache key hex>`: an OCI image
  manifest, a standard image config (`architecture` of the host, `os:
  linux`, `rootfs.type: layers`, `rootfs.diff_ids: [<step DiffID>]`) and the
  step's canonical layer compressed with gzip. needy's metadata is in the
  manifest's annotations (needy's own keys under `dev.needy.`, the
  reverse-DNS prefix of its `dev.needy.control` service label, following
  image-spec v1.1.0 `annotations.md` "Rules"). Example (digests shortened):

  ```json
  {
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": "sha256:c0…", "size": 120 },
    "layers": [{ "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": "sha256:9a…", "size": 31337 }],
    "annotations": {
      "dev.needy.image.step.key": "sha256:5e…",
      "dev.needy.image.step.parent": "sha256:1f…",
      "dev.needy.image.root": "sha256:b2…",
      "dev.needy.image.feature.ref": "ghcr.io/devcontainers/features/node:1",
      "dev.needy.image.feature.digest": "sha256:7d…"
    }
  }
  ```

  `step.parent` is absent for a step on the root, the `feature.*` pair for a
  `run:` step. A reader refuses a tag whose config is not an image config,
  that has another number of layers than one, a layer that is not gzip
  (`TODO(layer-registry-zstd)`), or annotations naming another key; the
  daemon also refuses a step whose `root` is not its plan's root.
- **A step image holds the step's layer only, not the stack down to
  `actor-base`.** A full image (runnable `FROM actor-base`) needs the base's
  compressed blobs in the target repository: they exist only when
  `manifest.json` records the base's registry `digest` (release-pushed
  bases; never for `needy image build`), and getting them there is a
  cross-repository mount within one registry or a copy across registries
  (bandwidth, and credentials for the source). The step chain is also
  already in the cache keys, and tags are last-writer-wins, so a stacked
  image of one host's lower layers could disagree with the steps another
  host pulled. A one-layer step image is standard (any registry and tool
  reads, copies and scans it) and always implementable. B-4 reuses
  `image::store::registry::pull_layer` (pull a gzip blob by descriptor,
  verify its digest, the decompressed DiffID, and the store's canonical
  digest; nothing kept on mismatch). B-4 must add: reading a full image's
  manifest and config for the actor's platform (an index), the DiffID
  prefix check against the base, zstd, naming in the store a layer whose
  canonical form differs from its DiffID (images other tools wrote), and,
  for pushing snapshots as full images, mounting or copying the base's
  blobs.
- Pulls verify the compressed digest (`oci-client`'s `pull_blob`), the
  DiffID of the decompressed stream, and then the store's canonical digest
  (`Store::put_expecting`); a mismatch keeps nothing.
- The daemon does as #105 did: on a local miss, inside the step's single
  flight, it looks the key up and pulls; after a build it pushes; a lookup,
  pull or push failure is logged and traced and never fails the build. The
  root in the plan and the `dev.needy.image.root` annotation are B-2's root.
  Trace kinds `image.layer.pull` and `image.layer.push` are schema version
  14, which only adds kinds, so traces of 10 through 13 still read.
- The layer registry, the TLS registry over a self-signed CA, and the auth
  shapes (ECR's Basic challenge, Harbor's token service with Basic, the
  refresh-token grant) are tested in process against `oci-registry`'s
  `test-support` registry; no live registry was used.

Settled in B-4 (2026-11-26):

- **The import** is `needy_core_actor::image::store::import`, shared by the
  daemon (`run create` of a VM actor whose `actor.image` is an OCI
  reference, local or remote) and `needy image build --from`. It reaches the
  registry through B-3's `oci_registry::Access` (an `http://` prefix is a
  plain-HTTP registry without credentials, as for the layer registry), and:
  - reads the manifest the reference names; an OCI index or a Docker
    manifest list is resolved to the actor's platform (`linux/amd64` for
    `linux-firecracker`, `linux/arm64` for `macos-vz`; BuildKit's
    `unknown/unknown` attestation entries are never chosen). A manifest
    pulled by digest is checked against it. OCI and Docker schema 2
    manifests both read. The config must be `linux` and the platform's
    architecture;
  - checks the `needy.base-abi` label (`base::check`) and that
    `rootfs.diff_ids` start with the `base.diff_ids` the base VM image's
    `manifest.json` records (`import::vm_base`). A refusal names the base to
    build `FROM` (`<name>@<digest> (<tag>)` when the release pushed it), the
    first layer that differs and, when the image has one, its
    `org.opencontainers.image.base.name` label. A base VM image without
    `base` refuses every import;
  - pulls only the layers above the base, by descriptor: `oci-client`
    verifies the compressed digest, the decompressed stream (gzip, zstd or
    uncompressed; Docker's gzip and tar media types too) must hash to the
    config's DiffID, and nothing is kept otherwise. zstd is decoded by
    `ruzstd` (pure Rust, so the static musl and Windows builds need no C
    zstd), every frame of a multi-frame stream with skippable frames
    skipped, as zstd:chunked layers are written;
  - skips empty layers (the DiffID of an empty tar,
    `sha256:5f70bf18...c3c6ef`, or a layer whose canonical form is empty)
    and a layer that repeats the one below it; a layer that comes back
    after others is refused (`TODO(import-repeated-layers)`);
  - reads every member before storing a layer and refuses one that writes
    under `/.needy`, removes it (`.wh..needy`) or hides the root
    (`.wh..wh..opq` at the top), and a hard link to a file outside the layer
    (`TODO(import-hard-links)`). It warns about builder trees
    (`/.envbuilder`);
  - stores each layer in the store's canonical form (relative member names,
    no `/` root entry, fresh headers; hard links kept), named by its own
    digest, with the image's DiffID recorded as its alias
    (`image-store/imported/sha256-<DiffID hex>`): the canonical form of a
    layer another tool wrote differs from its DiffID, and the alias lets the
    next import of the same layer, from any image, pull nothing.
    `needy image prune` does not remove aliases; one whose layer it removed
    is ignored and replaced;
  - ignores the config's `Entrypoint`, `Cmd`, `User` and `WorkingDir` (a VM
    actor's init is needy's, the profile says what runs and as whom) and
    warns about an entrypoint that is not needy's; takes `Env`: `PATH` as
    the run's image `PATH`, the rest as its image environment, which
    `actor.env` overrides (as a feature's `containerEnv`).
- **Images `FROM needy-actor` are refused**, with an error that says to
  build `FROM actor-base`: the first layer above `actor-base` is the shim
  step, which moves the shells' real programs to `/.needy/real` and links
  the proxy in their place, and every later layer was built with the shims
  in place (a package may have replaced a shim). Undoing that is what
  `from.Dockerfile` did on a whole exported root file system; per layer it
  would mean rewriting the layers above the shim step, which then no longer
  match their DiffIDs or anyone else's, and `needy-actor` is a transition
  image (`TODO(base-images)`). The refusal is recognised by that layer
  writing under `/.needy` while the image's
  `org.opencontainers.image.base.name` label is empty or names an
  `actor-base` (as `needy-actor`'s is); any other layer that writes there is
  refused as such.
- **Identity**: the imported layers sit on the base's stack digest (B-2),
  which is the run's `Boot.root` and `BridgeAttached.root`; the runner's D4
  check is unchanged. A layer imported once is the same store layer
  whichever image named it.
- **Runs**: `image::prepare` imports and materializes the layer disks for a
  local VM actor; `image::prepare_remote` imports on the control daemon
  (which needs registry access, not KVM) and the runner fetches the layers
  with `FetchLayer` as it fetches built steps
  (`TODO(runner-registry)`: direct pulls are B-5). An image with nothing
  above the base boots the base. The container drivers still run
  `actor.image` as it is. The VM drivers always boot the base VM image;
  `actor.image` only names the import in `profile check`
  (`<REF>, imported above the base VM image ...`).
- **Trace**: one `image.import` event per import (reference, manifest,
  platform, stored layers, pulled and skipped counts, warnings, time, or the
  error), schema version 15, which only adds the kind, so traces of 10
  through 14 still read.
- **`needy image build --from REF`** is the import, without BuildKit or an
  engine: it prints what it imported on stderr and the snapshot digest on
  stdout. `from.Dockerfile` and the bollard build path are deleted; `--out`
  no longer applies to `--from`; `--base DIR` names the base VM image whose
  base is imported above, and `--insecure-registry HOST` adds to
  `daemon.insecure_registries`. The CLI writes the daemon's layer store
  (`needy_daemon::runner::layer_store`, the same directories).
- **Per-reference VM images** (`vm::dir_of`) are no longer made or booted,
  and `Host.images` and the daemon's and runner's `vm_images` are gone.
  `dir_of` is kept so `needy image rm REF` still finds the directories an
  older needy made, `needy image ls` still lists them (their manifests'
  `source.kind: oci` still reads, shown as no longer booted); nothing
  migrates them.
- **B-2's markers**: `root::of_vm` keeps the archive and file digests of
  base VM images without `base` (releases before B-1); the trigger now
  names only those releases. A custom container `root:` keeps the engine's
  image ID rather than a ChainID of DiffIDs that start with `actor-base`'s:
  its config (`Env`, `User`, `WorkingDir`) is part of what container steps
  are built on, and the identity would pay only once the VM drivers build on
  the same `root:` (slice 5 of the images and run data spec), which B-4 does
  not do. `TODO(default-root)` stays: the import pulls `actor-base`, never
  `DEFAULT_ROOT`.
- **Deferred**: pushing a snapshot as a full image `FROM actor-base`
  (mounting or copying the base's blobs; `TODO(snapshot-image)`, with B-5's
  push), importing without the registry for a reference pinned by digest
  (`TODO(import-offline)`), and an end-to-end cell: the harness runs no
  registry, so the remote actor cell and the TUI cells on the VM drivers do
  not import yet (`TODO(e2e-import)` in `xtask/src/e2e/`). The examples'
  Dockerfiles take `--build-arg SHIM=0` to leave the shim step out of an
  image for the VM drivers.
- **Evidence**: in process only, against `oci-registry`'s test registry
  (plain HTTP, and TLS with a private CA and Basic credentials) with
  synthetic images `FROM` a three-layer stand-in `actor-base`
  (`image::store::import::testing`): gzip, zstd and uncompressed layers,
  empty and repeated layers, absolute member names with a `/` entry and
  epoch times, hard links, `/.envbuilder` and its entrypoint, an index with
  an attestation entry, Docker schema 2; local and remote runs with
  stand-in Firecracker. The builder shapes come from Shipwright `BuildRun`s
  (BuildKit and envbuilder) on a Kubernetes cluster on 2026-11-25: both kept
  the base's layers byte-identical (same compressed blob and DiffID), and
  envbuilder wrote 14 zstd layers above the base, 10 of them the empty
  layer, `/.envbuilder` and its own entrypoint. No real VM booted an
  imported image in this slice.
