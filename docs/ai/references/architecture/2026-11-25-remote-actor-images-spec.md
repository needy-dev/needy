# Spec: Image Layers for Remote Actors and a Layer Registry

Status: **approved for implementation**, 2026-11-25. The owner asked for the
remote actor case to be completed: image layers on remote actor runners, a
layer registry, and tests on Kubernetes clusters with KVM nodes. Names marked
*provisional* may change in review.

> **Revision notice (2026-11-25):** D1-D4 and slice R1 stand as implemented.
> D5 (a needy-specific layer artifact in a registry) and slices R2 and R3 are
> under revision and not merged: after a review of prior art, the owner chose
> to reuse standard OCI images and conventions instead. Pushed steps become
> ordinary OCI image manifests; registry credentials and TLS follow
> `DOCKER_CONFIG` and containers-style `certs.d`; needy's base becomes one
> layered OCI image shared by the VM and container drivers; remote runners
> import `actor.image` OCI layers without an engine; builds are done in CI or
> by standard in-cluster builders. A design for the shared base follows; this
> note is updated when it lands.
>
> **Update (2026-11-26):** D5's daemon side landed as slice B-3 of the
> [layered OCI base spec](2026-11-25-layered-oci-base-spec.md) ("Settled in
> B-3"): steps are pushed as ordinary OCI images, not the
> `application/vnd.needy.layer.config.v1+json` artifact below.
>
> **Update (2026-11-26):** B-4 of the layered OCI base spec imports an
> `actor.image` OCI reference for remote VM actors on the control daemon;
> the runner fetches those layers with `FetchLayer` as in D2.

Parent notes:

- [Remote actor host and runner ergonomics (Phase 1)](2026-11-23-remote-actor-host-and-runner-ergonomics-spec.md):
  `needy runner serve --roles actor --ephemeral`, the bridge, Node Authorizer.
- [Image definitions, hand edits and merged run data](2026-11-17-images-and-run-data-spec.md):
  the layer store, cache keys, build steps in sessions.
- [External client penetration and OCI layers (Phase 3)](2026-11-23-external-client-penetration-and-oci-layers-spec.md):
  "OCI Registry Distribution". This spec makes that item concrete.

## Problem

A run whose actor is placed on a remote actor runner (`actor.runner`) ignores
`actor.image`:

- `run create` skips `image::prepare` when the actor is remote
  (`crates/daemon/src/run/mod.rs`, `if remote_actor_runner.is_none()`), so no
  plan is resolved, nothing is built, and the run's `Boot` is empty.
- The runner boots the VM with `layer_disks: &[]`
  (`crates/daemon/src/actor_runner.rs`, `run_actor`).

The actor silently boots the base image: no feature programs (`builtin:codex`
has no `codex`), no image `PATH` or environment, no hooks, no feature sync
mappings. Observed on v0.0.15 on a Kubernetes cluster: base-only profiles work
remotely, image profiles cannot.

A second gap follows from the deployment: a control daemon without KVM or a
container engine cannot build a VM layer at all, and every host keeps its own
layer store, so a layer built once is rebuilt on every host that needs it.

## Decisions

### D1. The control daemon prepares the image of every run

`run create` runs `image::prepare` for remote actors too. The daemon resolves
the plan (features, cache keys, `PATH`, environment, hooks, sync mappings) and
records the snapshot in the run's `Boot`, as for a local actor. It does not
materialize layer disks for a remote actor; the runner does (D3).

Where a step's layer comes from, in order:

1. the daemon's layer store (a cache hit, as today);
2. the layer registry, when one is configured (D5): the daemon looks the cache
   key up and pulls the layer into its store;
3. a build, when this host can build (KVM, or a Docker-compatible engine),
   pushing the result to the registry when one is configured.

When none applies, `run create` fails before any actor starts, with an error
that names the missing step and says how to build it (on a host that can
build, with the same registry). It never boots the base silently.

*Settled in R1:* for a remote actor the plan is made on the control
daemon's base VM image (`root_digest`, whatever builder the host would pick
for a local run), and a missing step is built here only when the host can run
`linux-firecracker` sessions: a `linux-container` build would be made on
another root. Remote runs skip `actor::check_host`, so a daemon without KVM
serves image runs whose layers it holds (`image::prepare_remote`).

`TODO(remote-build)`: dispatching build steps to an actor runner that has KVM
is deferred. A builder host with the same registry covers the known
deployments; reopen when a deployment has no such host.

### D2. The bridge names the snapshot; the runner fetches layers by digest

*Provisional wire, `proto/needy/v1/guest.proto`:*

```protobuf
message BridgeAttached {
  // ... existing fields 1-4 ...
  // The run's snapshot: its layers' digests (`sha256:<hex>`, the OCI DiffID
  // of the uncompressed canonical layer), lowest first. Empty boots the base.
  repeated string layers = 5;
  // The base root digest the snapshot was built on (`sha256:<hex>`).
  string root = 6;
  // Where the runner may pull each layer from directly (D5), keyed by layer
  // digest. Absent entries are fetched from the control daemon.
  map<string, LayerSource> sources = 7;
}

service GuestService {
  // Streams one layer of the caller's run as its uncompressed canonical OCI
  // layer archive.
  rpc FetchLayer(FetchLayerRequest) returns (stream FetchLayerResponse);
}
```

- `FetchLayer` joins `ACTOR_METHODS`. It answers only digests in the snapshot
  of the caller's run (`Caller::Actor { run }`); any other digest is
  `PermissionDenied`. The runner verifies the digest of what it stores
  (`Store::put` returns it) against the requested one.
- The runner keeps its own layer store under its cache and data directories,
  so a layer fetched once is reused by later runs on that host.
- *Landed in R1:* `BridgeAttached` fields 5 (`layers`) and 6 (`root`),
  `GuestService.FetchLayer` (`FetchLayerRequest.digest = 1`,
  `FetchLayerResponse.data = 1`). Field 7 (`sources`) waits for R3. The runner
  stages each fetched layer in its work directory, never holding it whole,
  and refuses one whose stored digest differs from the requested one.

### D3. The runner materializes and reports the disks

The runner calls `Store::snapshot` and `Store::materialize` and passes the
disk paths to `Start::layer_disks`. The guest matches disks by digest
(`needy-layer` markers), and `materialize` may flatten lower layers above
`MAX_UNFLATTENED_LAYERS`, so the guest's `layers` list must name the disks the
runner made, not the snapshot's layers.

*Landed in R1:* `BridgeRequest.prepared = 7` (`BridgePrepared { repeated
string layer_disks = 1 }`), sent before the VM starts. The daemon records it on
the run; `GetConfig` for a remote run with a non-empty snapshot waits for it
(30 s, `REMOTE_LAYERS_WAIT` in `crates/daemon/src/guest.rs`: the wait covers
only the race between the bridge stream and the guest's own stream, since the
runner reports before it boots) and serves those digests as `layers`. A
failure before the VM runs (a root mismatch, a fetch, the VM start) is
reported as `BridgeExit.error`, and the run fails with "the actor runner could
not start the actor: ...".

### D4. The runner checks the root

The runner compares `root` with the digest of the base image it would boot
(`manifest.json`'s `sha256`, as `image::build::root_digest` computes it) and
fails the run with a clear error on mismatch: layers built on another base
must not be stacked on this one.

*Migration notice (B-2 of the [layered OCI base spec](2026-11-25-layered-oci-base-spec.md)):*
the root is now the stack digest of the `actor-base` the base VM image's
manifest records, the same identity a container builder gives `needy-actor`;
`sha256` (or the `rootfs.ext4` digest) is only the root of an image that
records no base.

### D5. A layer registry (OCI Distribution)

*Under revision; see the notice at the top. Kept as the record of what was
proposed.*

*Provisional configuration:* `daemon.layer_registry` / `needy daemon serve
--layer-registry REPOSITORY` (for example `registry.example.com/needy/layers`),
read-write; `needy runner serve --layer-registry REPOSITORY`, read-only.

- Each built step is pushed as one OCI artifact: a gzip layer blob, a needy
  config blob (*provisional* media type
  `application/vnd.needy.layer.config.v1+json`: the DiffID, the parent, the
  feature reference and digest), and a manifest tagged `key-<cache key hex>`.
  The registry verifies the compressed digest; the reader verifies the DiffID
  after decompressing.
- Lookup is a manifest `HEAD`/`GET` of the tag; a hit pulls the blob into the
  store. `BridgeAttached.sources` gives the runner each layer's repository and
  blob digest so it pulls directly; the daemon's `FetchLayer` is the fallback.
- Credentials come from the Docker credential store (`docker_credential`, as
  `devcontainer-features` already does), so credential helpers such as
  `docker-credential-ecr-login` work and needy knows no cloud.
- A plain-HTTP registry is an explicit opt-in (`http://` prefix), for
  in-cluster tests; HTTPS is the default.
- Layers are compressed for the registry only; the store keeps the canonical
  uncompressed tar (`TODO(images)` in `store/mod.rs` stays for local disk).

`TODO(layer-registry-sharing)`: one registry repository per tenant. Sharing
layers across tenants (public, pinned features only) waits for an owner
decision.

### D6. Errors and trace

A remote run records the same `image.*` trace events as a local run for what
the daemon did (lookup, pull, build, push), and the runner's fetch time is
reported in `actor.start`. *Provisional:* `layers_fetched_ms`.

*Deferred in R1* (`TODO(remote-actor-images)` in
`crates/daemon/src/actor_runner.rs`): `ActorStart` refuses unknown fields, so
`layers_fetched_ms` needs a trace schema version of its own.

## Slices

Each slice is a separate PR with its own tests.

| Slice | Scope | Depends on |
| --- | --- | --- |
| R1 | D1 without the registry, D2 (`FetchLayer`, `BridgeAttached.layers`/`root`), D3, D4: remote actors boot image layers that the control daemon has or builds | none |
| R2 | D5 in the store and the daemon: push after build, lookup and pull on a miss, configuration and credentials | none (parallel to R1) |
| R3 | D5 on the runner: `BridgeAttached.sources`, direct pulls, `FetchLayer` fallback; a control daemon without KVM serves image runs from the registry | R1, R2 |

## Testing

- **In process** (`crates/daemon`, stand-in Firecracker): a remote run with an
  image definition dispatches `StartActor`; the runner side fetches every
  snapshot layer, refuses a digest outside the snapshot, reports prepared
  disks, and the guest configuration names them. A root mismatch fails the
  run. A daemon that cannot build and has no registry refuses the run.
- **Store and registry** (`crates/core-actor`): push a layer to a local
  OCI Distribution registry and pull it into a fresh store, checking digest
  integrity and whiteouts (prior art: `devcontainer-features` fetch tests).
- **End to end** (`cargo xtask e2e`): extend the remote actor cell
  (`xtask/src/e2e/remote_actor.rs`) with an image definition, on a Linux KVM
  host.
- **Live probes on Kubernetes** (evidence, not support claims): a control
  daemon on a node without KVM, an ephemeral actor runner on a KVM node, an
  executor runner, and an in-cluster OCI Distribution registry; a builder
  host pushes, the actor boots `builtin:codex` from the registry.

## Open questions

1. ~~Should `GetConfig` wait for `BridgePrepared`, or should the runner wait
   for an acknowledgement before starting the VM?~~ Settled in R1: `GetConfig`
   waits (bounded), so the runner's start sequence stays one-way (D3).
2. Lifecycle of registry artifacts: by last pull or never deleted while a
   lockfile names them.
