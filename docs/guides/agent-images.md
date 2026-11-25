# Agent Images

An agent image is an OCI image with your agent and its tools, built `FROM`
needy's published base, `ghcr.io/needy-dev/needy/actor-base`. A profile names
it in `actor.image`:

```yaml
actor:
  driver: linux-firecracker        # or macos-vz
  image: registry.example.com/agents/codex@sha256:<digest>
  command: [codex]
```

The VM drivers (`linux-firecracker`, `macos-vz`) do not boot the image as a
whole. They boot needy's base VM image, whose root file system was derived
from `actor-base`, and attach the image's own layers, the ones above
`actor-base`, as read-only layer disks, the way they attach an image
definition's built steps. needy calls this an **import**: it pulls only those
layers from the image's registry, by digest. The container drivers
(`linux-container`, `macos-apple-container`) run the image as it is.

See B4 of the
[layered OCI base spec](../ai/references/architecture/2026-11-25-layered-oci-base-spec.md)
for the design.

## Which base to build FROM

Build on the `actor-base` your base VM image records. `needy image pull`
writes the release's base VM image and its `manifest.json`, which names the
base:

```sh
jq .base "${XDG_DATA_HOME:-$HOME/.local/share}/needy/images/linux-firecracker/base/manifest.json"
```

```json
{
  "image": "ghcr.io/needy-dev/needy/actor-base:0.0.18-ubuntu-24.04",
  "digest": "sha256:<the base's manifest for this architecture>",
  "diff_ids": ["sha256:...", "sha256:...", "sha256:..."]
}
```

Pin the digest in your `FROM` line:
`FROM ghcr.io/needy-dev/needy/actor-base@sha256:<digest>`. A base VM image
built from a checkout (`needy image build`) records `needy-actor-base:latest`,
the `actor-base` that build tagged in your engine; images built `FROM` it
import only on that base, since every build of `actor-base` has its own
layers.

A base VM image that records no base (made before needy recorded it) cannot
have an image imported above it; `needy image pull` replaces it.

## Building

Any builder that keeps the base's layers as they are works, which every
standard builder does: the image's first layers stay byte-identical to the
base's, and needy compares them by DiffID.

**Docker or BuildKit** (`docker build`, `docker buildx build`, Buildah):

```dockerfile
FROM ghcr.io/needy-dev/needy/actor-base@sha256:<digest>
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends jq \
 && rm -rf /var/lib/apt/lists/*
```

```sh
docker buildx build --platform linux/amd64 -t registry.example.com/agents/jq:1 --push .
```

Build for the actor's platform: `linux/amd64` for `linux-firecracker`,
`linux/arm64` for `macos-vz`. A multi-platform index works; needy picks the
manifest of the actor's platform.

Do not run `RUN ["/.needy/shim-image"]` in an image for the VM drivers: it is
the container drivers' shim step, which rewrites the shells under `/.needy`.
[`examples/custom-image/Dockerfile`](../../examples/custom-image/Dockerfile)
and [`examples/agents/Dockerfile`](../../examples/agents/Dockerfile) take
`--build-arg SHIM=0` for that, so one Dockerfile makes both images.

**Dev Container CLI** (`devcontainer build`), with Dev Container features:

```json
{
  "image": "ghcr.io/needy-dev/needy/actor-base@sha256:<digest>",
  "features": {
    "ghcr.io/devcontainers/features/python:1": { "version": "os-provided" },
    "ghcr.io/needy-dev/features/codex:1": {}
  }
}
```

```sh
devcontainer build --workspace-folder . --platform linux/amd64 \
  --image-name registry.example.com/agents/codex:1 --push
```

When a feature `dependsOn` another, list that other feature only with the
same options as the `dependsOn` entry, or not at all: the Dev Container
specification treats a feature with other options as another feature, which
is installed again.

**Shipwright** (`BuildRun` with the BuildKit or Buildah strategy, or
envbuilder and kaniko): build the same Dockerfile, or the
`devcontainer.json`, and push to a registry the control daemon reaches.
envbuilder and kaniko write zstd layers, empty layers for steps that change
no file, and absolute member names; needy imports them. envbuilder leaves its
own `/.envbuilder` tree in the image and sets its entrypoint to it: needy
warns about the tree (an actor does not use it) and ignores the entrypoint.

## What needy checks

When a run starts (or `needy image build --from REF` imports), needy:

- reads the image's manifest for the actor's platform and its config;
- refuses an image without this needy's base ABI (`needy.base-abi` label);
- refuses an image whose first layers are not the base's, naming the base to
  build `FROM` and the first layer that differs;
- refuses an image built `FROM needy-actor` (the container base: its first
  layer above `actor-base` is the shim step), and any layer that writes,
  removes or hides `/.needy`, where the base keeps what the guest and the
  base ABI rely on;
- pulls each layer above the base by digest, verifies the compressed digest
  and the DiffID of what it decompresses to (gzip, zstd or uncompressed), and
  keeps nothing that fails;
- skips empty layers and a layer repeated right above itself, and refuses a
  layer that comes back after others, or a hard link to a file of another
  layer (no builder writes either).

Layers already in the layer store are not pulled again, whatever image they
came from.

## What needy ignores and takes

A VM actor's init is needy's, and the profile says what runs and as whom, so
the image's `Entrypoint`, `Cmd`, `User` and `WorkingDir` are ignored (needy
warns about an entrypoint). Its `Env` is taken: `PATH` as the image's `PATH`,
the rest as the actor's environment, which `actor.env` overrides.

## Registries and remote actors

Registry access is the daemon's: the Docker credential store, `certs.d` CA
files and `--insecure-registry` (see
[registry credentials and TLS](configuration.md#actor-images-recipes-and-builtin)).
Prefix `http://` for a plain-HTTP registry, such as one on `localhost` for
local work: `actor.image: http://localhost:5000/agents/jq:1`.

For an actor on a remote actor runner, the control daemon imports the image
and the runner fetches the layers from it, as it fetches built steps; the
runner itself does not contact the registry yet.

## Importing ahead of a run

`needy image build --from REF` imports `REF` into the layer store for this
host's VM driver without a container engine and prints the snapshot its
layers make; a run of `REF` then pulls nothing but the manifest and config.
`--base DIR` names another base VM image, and `--insecure-registry HOST`
adds to `daemon.insecure_registries`.

## Migrating from `needy image build --from` VM images

Before the import, `needy image build --from REF` exported the whole image
with BuildKit into a VM image of its own. Those VM images are no longer
booted: rebuild the image `FROM actor-base` (without the shim step), push it,
and name the registry reference in `actor.image`. `needy image ls` still
lists the old VM images, and `needy image rm REF` removes them.

## Evidence

The checks and the layer shapes above are tested in process against an
in-process OCI registry with synthetic images (`crates/core-actor/src/image/store/import_test.rs`,
`crates/daemon/src/image/import_test.rs`, and the remote runner case in
`crates/daemon/src/actor_runner_test.rs`). The builder shapes (BuildKit and
envbuilder keeping the base's layers byte-identical, zstd and empty layers,
absolute member names, `/.envbuilder`) were observed in Shipwright
`BuildRun`s on a Kubernetes cluster on 2026-11-25. Booting an imported image
in a real VM is not yet covered by the end-to-end harness
(`TODO(e2e-import)`).
