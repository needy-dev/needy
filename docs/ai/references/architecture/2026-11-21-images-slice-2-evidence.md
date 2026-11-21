# Slice 2: container snapshots, trampolines, and container build fallback

Evidence for slice 2 of the
[images and run data spec](2026-11-17-images-and-run-data-spec.md#slices).
Recorded 2026-11-21.

## Scope & Setup

- **Base revisions**:
  - `daca6fa` (`core-executor`: container image commit, export, load, and remove primitives)
  - `008052f` (`core-actor`: container OCI archive materialization with shell shim step)
  - `9df591e` (`daemon` & `xtask`: container snapshot materialization, non-KVM container build fallback, and container e2e `image` cells)
  - `b8a6059` (`image-store`: auto-flatten >8 VM layers, unified `Digest`, rollback base persistence, sshfs parent ownership)
  - `c0ecc6c` (`xtask`: container trampoline `tool-path` assertion in `xtask/src/e2e/image.rs`)
- **Host tested live**: a Linux x86_64 KVM host, non-root daemon (`uid != 0`).
- **Hosts / runtimes not checked live**:
  - `linux-container.runsc/image`: `runsc` on the Linux test host is registered
    without `--host-uds=all`, which container actors require.
  - `macos-apple-container/image`: Apple `container` apiserver is not running
    on the local Apple silicon Mac.

## E2E `image` scenario on container and VM drivers

`cargo xtask e2e --remote <KVM_HOST> --only /image` at `c0ecc6c` exercises the
full `image` lifecycle on each covered driver:

1. `needy image build --profile`: builds a `run:` layer installing
   `/opt/e2e-bin/e2e-layer-tool` and `/etc/e2e-layer.txt` with
   `env.PATH: "/opt/e2e-bin:${PATH}"` and `env.E2E_LAYER_ENV: "from-image"`,
   and hits cache on a second invocation with the same `sha256:` snapshot
   digest.
2. First `needy run create`:
   - On container drivers (`linux-container`, `macos-apple-container`), verifies
     `command -v e2e-layer-tool` resolves through the PATH trampoline at
     `/run/needy/shims/e2e-layer-tool` ahead of `image_path` (while VM drivers
     resolve directly to `/opt/e2e-bin/e2e-layer-tool` via `binfmt_needy`).
   - Verifies the tool executes, `/etc/e2e-layer.txt` and `E2E_LAYER_ENV` are
     visible, and `needy run report -o json --level detailed` records the exact
     `snapshot` digest.
3. `needy image edit --profile` on a pseudo-terminal: creates
   `/etc/e2e-edited.txt` and `/usr/local/bin/e2e-edit-tool`, exits cleanly,
   commits the layer to `test-image.yaml.lock`, and prints a pasteable
   `- run: |` snippet.
4. Second `needy run create`: verifies both the declared `run:` layer and the
   committed edit are active with a new `snapshot` digest.
5. `needy image rollback --profile`: clears the lockfile commit entry.
6. Third `needy run create`: verifies the committed edit is gone, the declared
   layer and `env`/`PATH` remain, and the original `snapshot` digest is
   restored.
7. Standard harness invariants (private sockets, discovery descriptor, clean
   daemon shutdown, zero leftover containers/VMs/processes/mounts/sockets).

### Live E2E results

| Cell | Host | Command | Result | Duration |
| --- | --- | --- | --- | --- |
| `linux-container.runc/image` | Linux x86_64 KVM host (non-root) | `cargo xtask e2e --remote <KVM_HOST> --only /image` (at `c0ecc6c`) | **pass** | 41.3 s |
| `linux-firecracker/image` | Linux x86_64 KVM host (non-root) | `cargo xtask e2e --remote <KVM_HOST> --only /image` (at `c0ecc6c`) | **pass** | 28.4 s |
| `linux-container.runsc/image` | Linux x86_64 KVM host (non-root) | `cargo xtask e2e --remote <KVM_HOST> --only /image` (at `c0ecc6c`) | **not covered** | `runsc` registered without `--host-uds=all` |
| `macos-apple-container/image` | Apple silicon Mac (`arm64`) | — | **not covered** | Apple `container` apiserver not running |

## Unit and in-process daemon tests

Covered in automated `cargo test --locked`:

- **OCI snapshot materialization & shell shim layer**
  (`crates/core-actor/src/image/store/oci_test.rs`):
  - Docker-save and OCI-layout (gzip) base archives, deterministic needy-owned
    snapshot tags, shell shim generation when layers replace shells, top-layer
    extraction from exported images, cache reuse and garbage collection, and
    missing-layer errors.
- **Container engine image primitives**
  (`crates/core-executor/src/engine_test.rs`):
  - `commit_container`, `export_image`, `load_image`, and `remove_image`.
- **Daemon container materialization & non-KVM container build fallback**
  (`crates/daemon/src/image_test.rs`):
  - `run_create_on_linux_container_materializes_loads_and_boots_snapshot_image`
  - `run_create_on_macos_apple_container_saves_base_materializes_loads_and_boots_snapshot_image`
  - `linux_container_fallback_builds_and_edits_without_vm_driver_and_cleans_up_temp_image`
- **Actor driver unit tests**
  (`crates/core-actor/src/driver/linux_container/mod_test.rs`,
  `crates/core-actor/src/driver/macos_apple_container/mod_test.rs`):
  - `snapshot_image` override on start, writable session containers, and
    refusal of non-empty VM `layer_disks` on container drivers.

## Remaining / Not checked

- Live `linux-container.runsc/image` on a host with `runsc --host-uds=all`.
- Live `macos-apple-container/image` on a macOS host with Apple `container`
  apiserver running.
- Live non-KVM Linux host e2e exercising the container build/edit fallback
  without `/dev/kvm`.
