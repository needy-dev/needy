# Slice 3: Dev Container features as layers

Evidence for slice 3 of the
[images and run data spec](2026-11-17-images-and-run-data-spec.md#slices).
Recorded 2026-11-21.

## Scope & Setup

- **Base revisions**:
  - `decb324` (`core-actor` & `cli`: OCI Dev Container feature fetcher/cache and CLI lockfile feature pinning, PR #45)
  - `106b796` (`config` & `core`: Dev Container feature layer parsing, metadata, ordering, refusals, `containerEnv`/`PATH`, and build plans, PR #46)
  - `1775167` (`daemon`, `core-actor`, `cli`, `proto`: daemon feature resolution and build sessions, lockfile pinning, built-in `sshfs` feature, and VM base auto-pull, PR #47)
  - Plus the `image-feature` e2e scenario and two live-VM bug fixes on branch `feat/slice-3-feature-e2e`.
- **Host tested live**:
  - An Apple silicon Mac (`arm64`, macOS 27.2), non-root daemon (`uid != 0`), for `macos-vz/image-feature`.
- **Hosts / runtimes not checked live**:
  - `linux-firecracker/image-feature`, `linux-container.runc/image-feature`,
    and `linux-container.runsc/image-feature`: Linux host not run in this session.
  - `macos-apple-container/image-feature`: Apple `container` apiserver not
    running on the local Apple silicon Mac.

## E2E `image-feature` scenario

Added `Scenario::ImageFeature` (`*/image-feature` across all five actor drivers)
to `cargo xtask e2e` (`xtask/src/e2e/image.rs`, `xtask/src/e2e/matrix.rs`). The
cell exercises a real published Dev Container feature
(`ghcr.io/devcontainers/features/github-cli:1` with
`with: { installDirectlyFromGitHubRelease: true }`):

1. `needy image build --profile`: fetches the feature artifact from `ghcr.io`,
   stages it into a writable root build session with open network, runs
   `./install.sh`, returns a `sha256:` snapshot digest, and pins
   `ghcr.io/devcontainers/features/github-cli: sha256:...` under `features:` in
   `test-feature-image.yaml.lock`.
2. Second `needy image build --profile`: hits cache with the pinned digest and
   returns the identical snapshot digest.
3. `needy run create`: boots the built snapshot, verifies `command -v gh`
   resolves on `PATH` (`/usr/bin/gh` on VM drivers, `/run/needy/shims/gh` on
   container drivers), verifies `gh --version` outputs `gh version ...`, and
   checks that `needy run report -o json --level detailed` records the exact
   `snapshot` digest.
4. Standard harness invariants (private sockets, discovery descriptor, clean
   daemon shutdown on SIGTERM, zero leftover VMs/containers/processes/mounts/sockets).

### Live E2E results

| Cell | Host | Command | Result | Duration |
| --- | --- | --- | --- | --- |
| `macos-vz/image-feature` | Apple silicon Mac (`arm64`, macOS 27.2, non-root) | `cargo xtask e2e --only macos-vz/image-feature --no-prepare` | **pass** | 137.8 s |
| `linux-firecracker/image-feature` | Linux x86_64 KVM host | — | **not covered** | Linux host not run in this session |
| `linux-container.runc/image-feature` | Linux x86_64 host | — | **not covered** | Linux host not run in this session |
| `linux-container.runsc/image-feature` | Linux x86_64 host | — | **not covered** | Linux host not run in this session |
| `macos-apple-container/image-feature` | Apple silicon Mac (`arm64`) | — | **not covered** | Apple `container` apiserver not running |

## Bugs uncovered and fixed during live verification

Two real-VM bugs in Slice 3 were uncovered while running `macos-vz/image-feature`
live and fixed with narrow regression tests in `crates/daemon/src/image_test.rs`:

1. **`feature_install_script` forced `sh ./install.sh` instead of honoring the feature's shebang**:
   - **Symptom**: building an official feature whose `install.sh` uses
     `#!/usr/bin/env bash` and bash parameter substitution failed immediately
     with `./install.sh: 88: Bad substitution` (exit 2) under Debian's `dash`
     `/bin/sh`.
   - **Root cause**: `feature_install_script` in `crates/daemon/src/image.rs`
     ran `chmod +x ./install.sh` and then executed `sh ./install.sh || status=$?`,
     bypassing the script's `#!` interpreter.
   - **Fix**: execute `./install.sh || status=$?` directly so the kernel honors
     the script's shebang (and POSIX `sh` still falls back to `/bin/sh` when no
     shebang is present).
   - **Regression test**: `feature_install_script_executes_install_sh_directly_to_honor_shebang`
     in `crates/daemon/src/image_test.rs`.

2. **Unbounded carriage-return progress output in `format_build_failure` broke HTTP/2 `grpc-message` trailers**:
   - **Symptom**: when a failing feature build emitted `curl` progress output
     containing thousands of bare `\r` updates, the CLI failed with an opaque
     `h2 protocol error: http2 error [protocol.invalid_response]` instead of the
     build step's real error.
   - **Root cause**: `format_build_failure` in `crates/daemon/src/image.rs`
     split only on `\n` and did not bound the byte length of the console tail.
     After percent-encoding into the gRPC `grpc-message` trailer header, the
     message exceeded `h2`'s default 16 KiB `MAX_HEADER_LIST_SIZE`.
   - **Fix**: keep only the final `\r` segment of each console line and cap the
     console tail to `FAILURE_LOG_MAX_BYTES` (4096 bytes).
   - **Regression test**: `format_build_failure_collapses_carriage_returns_and_bounds_message_size`
     in `crates/daemon/src/image_test.rs`.

## Unit and in-process daemon tests

Covered in automated `cargo test --locked --workspace`:

- **OCI feature fetcher, cache, and built-in `sshfs` feature**
  (`crates/core-actor/src/image/feature_test.rs`)
- **Feature layer config parsing, metadata, `dependsOn`/`installsAfter` ordering,
  refusals, `containerEnv`/`PATH` expansion, and build plans**
  (`crates/config/src/image/mod_test.rs`, `crates/core/src/image/feature_test.rs`,
  `crates/core/src/image/mod_test.rs`)
- **Lockfile feature digest pinning**
  (`crates/cli/src/commands/image/lockfile_test.rs`)
- **In-process daemon feature resolution, pinned zero-HTTP rebuilds, privileged
  feature refusal, shebang execution, and bounded build failure formatting**
  (`crates/daemon/src/image_test.rs`)

## Remaining / Not checked

- Live `linux-firecracker/image-feature`, `linux-container.runc/image-feature`,
  and `linux-container.runsc/image-feature` on a Linux host.
- Live `macos-apple-container/image-feature` on a macOS host with Apple
  `container` apiserver running.
