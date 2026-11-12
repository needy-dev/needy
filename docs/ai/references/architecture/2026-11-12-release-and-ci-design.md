# Release and CI

**Status**: implemented, 2026-11-12. `check.yml` runs on every pull request
and push to `main`; `release.yml` was run by hand without publishing
(see [Verification](#verification)). No release has assets yet: the owner
cuts the next release, which is the first to publish them.

Both workflows follow moeru-ai/auv's (`.github/workflows/check.yml` and
`release.yml`): the concurrency group, a Rust cache only `main` writes, the
free-disk step, NOTICE comments on each workaround, archives with a `.sha256`,
a "binary starts" check, `workflow_dispatch` with an optional upload to an
existing release, and `softprops/action-gh-release`. Action versions are the
ones auv pins; the `docker/*` actions, which auv does not use, are their
current majors.

## `check.yml`

Runs on pull requests, pushes to `main` and by hand. A newer push to a pull
request cancels the older run; runs on `main` are never cancelled.

| Job | Runner | What it runs |
| --- | --- | --- |
| `rust ubuntu-24.04` | Linux x86_64 | `cargo fmt --check`, `clippy -D warnings` (as `make check`), `cargo test --workspace --locked`, `needy --version`, `git diff --check`. Compiles the Linux-only guest code (`core-actor::guest`). |
| `rust macos-15` | macOS arm64 | the same without `fmt` (it does not depend on the platform) |
| `rust windows-2025` | Windows x86_64 | the same without `fmt` |
| `proto` | Linux | `buf lint`, `buf format --diff --exit-code`, `buf generate`, then fails if `crates/api-proto/src/gen` differs from the commit |
| `release tooling` | Linux | ShellCheck on `install/install.sh` and the Homebrew scripts, then `scripts/tests/render-homebrew-formula` (see [Install channels](#install-channels)) |

- The toolchain is the one `rust-toolchain.toml` pins, installed with the
  runner's own `rustup toolchain install`
  (`NOTICE(rust-toolchain)`): a setup action would install a second
  toolchain that the file then overrides.
- `Swatinem/rust-cache` saves only on pushes to `main`, so a pull request
  cannot evict or poison `main`'s entry.
- `NOTICE(ci-disk)`: debug info is `line-tables-only` and the Linux job
  removes unused toolchains from the runner (about 20 GB), because auv's full
  workspace test build with a restored cache ran out of disk.
- `NOTICE(proto-breaking)`: there is no `buf breaking` gate while the API is
  unreleased (0.0.x); add one against the last release tag once a release
  promises wire compatibility.

### Windows

Everything builds and `clippy -D warnings` passes on Windows. Making it pass
took only `cfg` changes (`fix(windows)` commit): imports and items only Unix
code uses are `cfg(unix)`, and tests that serve or dial Unix sockets
(`api-client::ready`, `core-executor::relay`, five socket tests of
`api-server::server`) are `cfg(unix)` like the repository's other socket
tests. Test files whose Unix-only tests leave helpers unused say so with
`#![cfg_attr(not(unix), allow(dead_code))]`.

What does not run on Windows, by design rather than as a gap: every actor
driver (they need Linux or macOS). A Windows machine is an executor only
(`native`), as the README says.

`cargo test` runs on Windows with these tests skipped
(`TODO(windows-tests)` in `check.yml`); each failed on the first Windows run
for a reason in the tests or the product, not in CI:

| Skipped | Why it fails on Windows |
| --- | --- |
| `protocol::grpc::v1::` (56 tests) and `server::tests::health_needs_no_token_and_everything_else_does` | `needy-api-server`'s test harness, `testing::serve_with`, serves on a Unix socket, which Windows does not bind (`Unsupported(Unix(..))`) |
| `profile::tests::a_resolved_profile_merges_its_parents_and_makes_host_paths_absolute` | a relative host path is joined with `\` (`...\p/ws/`) and the test expects `/` |
| `driver::linux_container::tests::` (2 tests) | bind-mount sources are joined with `\`; the tests expect Unix paths |
| `signals::tests::names_and_numbers` | `signals::name` has no table on Windows, so `name(9)` is `SIG9`, not `SIGKILL`, although `number("SIGTERM")` has one |

## `release.yml`

Runs when a release is published (releases and prereleases alike) and by
hand. `NOTICE(release-trigger)`: only `published` is listed: publishing a
prerelease also fires `prereleased`, so listing both would start two runs.

A plain `workflow_dispatch` builds and verifies everything and publishes
nothing. Publishing (release assets and GHCR pushes) happens only on a
release event, or on a dispatch with `upload_to_release` and a `tag_name`.

| Job | Runner | Produces |
| --- | --- | --- |
| `binary` (5 targets) | `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`, `macos-15-intel`, `windows-2025` | `needy-<target>.tar.gz` (`.zip` on Windows) and `.sha256` |
| `macos-guest` | `ubuntu-24.04` | `needy-aarch64-apple-darwin.tar.gz` again, with `needy-linux-aarch64` (the run's static `aarch64-unknown-linux-musl` needy, not code-signed: `NOTICE(guest-signing)`) next to `needy`; see the [actor images design](2026-11-12-actor-images-design.md) |
| `images amd64` | `ubuntu-24.04` | `needy-actor-linux-firecracker-x86_64.tar.gz` (`vmlinux`, `rootfs.ext4`, `firecracker`, `base-abi`; no needy since the actor images design); `needy-actor` and `needy-executor` for amd64 |
| `images arm64` | `ubuntu-24.04-arm` | `needy-actor-macos-vz-aarch64.tar.gz` (`Image`, `initrd`, `rootfs.ext4`, `base-abi`); `needy-actor` and `needy-executor` for arm64 |
| `container manifests` | `ubuntu-24.04` | publishing only: the multi-architecture `ghcr.io/needy-dev/needy-actor` and `needy-executor`, tagged with the release tag and `latest` |
| `publish` | `ubuntu-24.04` | publishing only: checks every `.sha256` and uploads the archives to the release |

Binaries:

- `NOTICE(linux-musl)`: the Linux binaries are static musl builds
  (`x86_64-` and `aarch64-unknown-linux-musl`), built natively on each
  architecture's runner. The `needy` stage of `images/Dockerfile` already
  builds the whole CLI for musl, so no old-glibc build container is needed;
  the job checks the result has no program interpreter and no `NEEDED`
  libraries.
- `NOTICE(windows-static-crt)`: the Windows binary links the C runtime
  statically, so it does not need the Visual C++ Redistributable; `dumpbin`
  checks it imports no `vcruntime`/`msvcp` DLL. Copied from auv.
- Every binary runs `needy --version` on its own runner.
- The macOS binaries are signed on every run with the Developer ID
  Application certificate (`apple-actions/import-codesign-certs`,
  `codesign --timestamp --options runtime`, then `codesign --verify`), with
  auv's secret names (`APPLE_DEVELOPER_ID_APPLICATION_CERTIFICATE`, its
  password, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_SPECIFIC_PASSWORD`).
  `notarize-macos` notarizes them (`notarytool submit --wait`) on
  publishing runs only, and `publish` uploads the notarized archives in
  place of the signed ones. A bare executable cannot be stapled, so
  Gatekeeper checks the ticket online on first launch.
- `NOTICE(hardened-runtime)`: notarization needs the hardened runtime, and
  needy needs no entitlement under it: it maps no writable executable
  memory, loads no unsigned libraries, and only spawns vfkit and Apple's
  `container`, which carry their own Virtualization.framework entitlements.
- `TODO(release-signing)`: `needy.exe` is not Authenticode-signed; there is
  no certificate for it.

Images:

- Each architecture builds on its own runner with BuildKit, not under QEMU,
  which would emulate a kernel and three Rust builds. One job per
  architecture builds the VM actor files and both container images on one
  builder, so the `needy` stage is built once.
- The VM actor files are built from the Dockerfile targets (`actor`,
  `actor-macos-vz`) with `--output type=local`, not with `needy build --out`:
  `needy build` picks the target from the host it runs on (the Firecracker
  target on any Linux), so it cannot build the `macos-vz` files on a Linux
  runner. The kernels are built without a recipe; `needy build --recipe`
  still builds one with a recipe's `uname` identity.
- The job checks every file is there, prints its `base-abi`, runs
  `firecracker --version`, and runs `e2fsck -fn` on `rootfs.ext4`. The
  archive is written with `tar --sparse`, so `rootfs.ext4`'s free blocks are
  holes again when GNU tar unpacks it.
- The container images are loaded and started (`needy --version` in the
  executor; in `needy-actor`, a shim runs its real program without the
  guest needy, as an image `FROM` the base needs while it builds, the proxy
  itself fails with the stub's message, `/.needy/shim-image` is there, and
  a shim and the guest needy run with the `guest` target's needy
  bind-mounted, as the daemon injects it)
  before anything is pushed. On a publishing run each architecture pushes by
  digest and `container manifests` creates the tagged multi-architecture
  lists (Docker's documented pattern for builds spread over runners). The
  pushes use `GITHUB_TOKEN` with `packages: write`; the images carry
  `org.opencontainers.image.source`, which links them to this repository.
  The repository is public, so its packages are public too.
- `NOTICE(ci-disk)`: the images jobs remove the runner's unused toolchains;
  the kernel tree, two Rust target directories and the root file systems
  share the builder's disk.
- `NOTICE(build-record)`: `DOCKER_BUILD_RECORD_UPLOAD` is off, because
  build-push-action's build-record artifacts are named
  `needy-dev~needy~<id>.dockerbuild`; `publish` also takes only `.tar.gz`,
  `.zip` and `.sha256` files.
- `NOTICE(artifact-retention)`: workflow artifacts are kept 7 days; the
  release keeps the published copy.

## Install channels

Three channels install the release binaries, each modelled on moeru-ai/auv's
(`install/install.sh`, `toolchain/proto/auv.toml`,
`scripts/render-homebrew-formula` and `update-homebrew.yml`). None works
before a release has assets; the README keeps the from-source path first
until then.

| Channel | Platforms | What it installs |
| --- | --- | --- |
| `install/install.sh` | macOS (arm64, x86_64), Linux (x86_64, aarch64) | `needy`, and `needy-linux-aarch64` next to it when the archive carries one |
| proto plugin `toolchain/proto/needy.toml` | macOS, Linux, Windows x86_64 | the archive into proto's tool directory; `needy` and, where present, `needy-linux-aarch64` linked into `~/.proto/bin` |
| Homebrew formula in `needy-dev/homebrew-tap` | macOS (arm64, x86_64) | `needy`, and `needy-linux-aarch64` on arm64 |

Windows has no script; the README links the `.zip` under
`releases/latest/download/`.

### Install script

- POSIX `sh`. `NEEDY_VERSION` (a tag, with or without `v`; default
  `latest`) and `NEEDY_INSTALL_DIR` (default `~/.local/bin`; a relative path
  is taken from the current directory).
- Picks `needy-<arch>-apple-darwin.tar.gz` or
  `needy-<arch>-unknown-linux-musl.tar.gz` from `uname`. A shell under
  Rosetta 2 on Apple silicon (`sysctl.proc_translated` is 1) gets the arm64
  archive, the one with the guest needy (`NOTICE(rosetta)`).
- Downloads the archive and its `.sha256` with `curl`, compares the digest
  (`sha256sum`, else `shasum -a 256`) and refuses on a mismatch before it
  unpacks anything. It then runs the unpacked `needy --version` and installs
  `needy` and any `needy-linux-*` with mode 0755.
- Ends with the `PATH` hint and a pointer to the README's Getting started.
- `NOTICE(install-url)`: there is no domain yet, so the script is served
  from `https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh`.
- `NOTICE(release-latest)`: `latest` uses GitHub's `releases/latest`, which
  skips prereleases (as do the README's Windows links). While releases are
  published as prereleases, `latest` finds nothing and users must set
  `NEEDY_VERSION`.
- Test hook: `NEEDY_DOWNLOAD_BASE` replaces the release URL with any URL
  that serves the assets directly, such as a local HTTP server over a
  workflow run's artifacts. It is how the script was verified without a
  release.

### proto plugin

A TOML plugin: each platform's `download-file` and `checksum-file` name a
release archive and its `.sha256`, which proto checks; versions come from the
repository's `v*` tags (`[resolve] git-url`). Linux always maps to the musl
archive, so `{libc}` is not used. Windows lists x86_64 only
(`TODO(windows-arm64)`).

`needy-linux-aarch64` is a second exe with `no-shim`: on macOS
`std::env::current_exe` returns the path a program was started by without
resolving symlinks, so a daemon started as `~/.proto/bin/needy` looks for the
guest in `~/.proto/bin`. proto skips the link where the archive has no such
file.

### Homebrew

- `scripts/render-homebrew-formula <vX.Y.Z> <arm64.sha256> <x86_64.sha256>
  <output>` writes `Formula/needy.rb`. It accepts only a plain `vX.Y.Z` tag
  and 64 lowercase hex digits per checksum, and writes the file with mode
  0644 (`brew style` rejects mktemp's 0600).
- The formula selects the archive by CPU, `depends_on :macos`
  (`TODO(homebrew-linux)`: Linux bottles deferred, as in auv), installs
  `needy` and, when present, `needy-linux-aarch64` into `bin`, and tests
  `needy --version`. `skip_clean "bin/needy-linux-aarch64"`: Homebrew's
  cleaner makes a file it does not take for an executable (a Linux ELF on
  macOS) 0444, which would leave the guest unexecutable.
- `REVIEW(license)`: the formula says `AGPL-3.0-only`. The repository's
  LICENSE has no version notice and GitHub reports the deprecated SPDX id
  `AGPL-3.0`, which Homebrew does not accept; the owner should confirm
  `-only` over `-or-later`.
- `scripts/tests/render-homebrew-formula` compares the output with an
  expected formula and checks the refusals (split checksum, invalid checksum,
  prerelease tag, no output left behind, readable mode). `check.yml`'s
  `release tooling` job runs it.
- `.github/workflows/update-homebrew.yml` runs on `release: released` and on
  `workflow_dispatch` with `tag_name`. It fails first, with an error
  annotation, when `HOMEBREW_TAP_GITHUB_TOKEN` is not set; then it downloads
  the two Darwin `.sha256` files of the release, clones
  `needy-dev/homebrew-tap`, renders the formula, and commits and pushes it
  when it changed. `NOTICE(homebrew-stable)`: `released` does not fire for
  prereleases, so a prerelease reaches the tap only by dispatch.

### Owner setup

The tap repository and the secret do not exist yet; `update-homebrew.yml`
fails at its first step until both do. To set them up:

1. Create the public repository `needy-dev/homebrew-tap` (Homebrew maps
   `needy-dev/tap` to it) with a README, so it has a default branch to push
   to. A `Formula/` directory is created by the first run.
2. Create a fine-grained personal access token (or a GitHub App token)
   whose resource owner is `needy-dev`, with access to `needy-dev/homebrew-tap`
   only and the repository permission Contents: Read and write.
3. Add it to `needy-dev/needy` as the Actions secret
   `HOMEBREW_TAP_GITHUB_TOKEN`
   (`gh secret set HOMEBREW_TAP_GITHUB_TOKEN -R needy-dev/needy`).
4. After the first release with assets, publish it as a full release (not a
   prerelease) so `update-homebrew.yml` runs, or run it by hand:
   `gh workflow run update-homebrew.yml -R needy-dev/needy -f tag_name=vX.Y.Z`.
   Then check `brew install needy-dev/tap/needy` on a Mac.

Publishing releases as full releases also makes `latest` work for the
install script and the README's Windows links; proto resolves `latest` from
the tags and does not depend on it.

### Verification of the install channels

On 2026-11-12, with the binary artifacts of the non-publishing release run
https://github.com/needy-dev/needy/actions/runs/43213270575 served by a local
HTTP server (`python3 -m http.server`). Its arm64 macOS archive predates the
`macos-guest` job, so a copy was repacked with the run's
`aarch64-unknown-linux-musl` needy as `needy-linux-aarch64` to stand in for
it.

- `install.sh`, through `NEEDY_DOWNLOAD_BASE`: on an Apple silicon Mac
  (native and under `arch -x86_64`, which picked the arm64 archive), on a
  Linux KVM host (x86_64, Debian 13), and in clean `debian:12` and
  `ubuntu:24.04` containers (arm64, plus `ubuntu:24.04` amd64). Each
  verified the checksum, installed, and `needy --version` printed
  `needy 0.0.1`; the Mac installed `needy-linux-aarch64` (a static aarch64
  ELF, 0755) next to `needy`. With a wrong `.sha256` every one refused with
  `Checksum mismatch` and installed nothing.
- proto 0.63.1 with an isolated `PROTO_HOME`: `proto plugin add` from a copy
  of `needy.toml` whose two URLs point at the local server, then `proto
  install needy latest` resolved 0.0.1 from the tags, verified the checksum
  and installed; `needy --version` worked through `~/.proto/bin` and the
  shim, and `needy-linux-aarch64` was linked; an archive without it
  installed without the link. A wrong `.sha256` failed with
  `proto::install::invalid_checksum`. `proto uninstall needy` and `proto
  plugin remove needy --from global` removed it.
- Homebrew 7.0.13: the formula rendered from the artifacts' checksums passed
  `brew style` and `brew audit --strict --formula` in a throwaway tap
  (`brew tap-new --no-git`). With its URLs pointed at the local server
  (same `/v0.0.1/` path, so Homebrew infers 0.0.1), `brew install --formula`
  installed both files, `brew test` passed, and `needy-linux-aarch64` kept
  0755 (it was 0444 before `skip_clean`).
- `actionlint` passed on the workflows; `check.yml`, with the new
  `release tooling` job, passed on `main`:
  https://github.com/needy-dev/needy/actions/runs/43252806891

Not verified: the real release URLs, the `.zip` through proto on Windows,
proto on Linux, `brew audit --online`, and `update-homebrew.yml` itself,
which needs the tap and the secret.

## Verification

On 2026-11-12, on GitHub-hosted runners:

- `check.yml` passed on all four jobs (Linux, macOS, Windows, proto):
  https://github.com/needy-dev/needy/actions/runs/43213266050
- `release.yml`, dispatched without publishing, built and verified every
  binary and both architectures' images; `container manifests`, `notarize-macos`
  and `publish` were skipped as intended:
  https://github.com/needy-dev/needy/actions/runs/43209411710 (before
  signing) and https://github.com/needy-dev/needy/actions/runs/43213270575
  (signed).
- Artifact sizes: `needy` archives 10 to 15 MB each (Windows `.zip` 10.6 MB,
  macOS 12 to 13 MB, Linux musl 14 to 15 MB); `needy-actor-linux-firecracker-x86_64.tar.gz`
  182 MiB (vmlinux 27 MiB, rootfs.ext4 559 MiB, firecracker 6.2 MiB, needy
  36 MiB unpacked); `needy-actor-macos-vz-aarch64.tar.gz` 174 MiB (Image 29
  MiB, rootfs.ext4 584 MiB). The images jobs took 17 (arm64) and 25 (amd64)
  minutes without a cache.
- Downloaded on an Apple silicon Mac with `gh run download`: both archives'
  `.sha256` matched; the macOS arm64 `needy --version` printed
  `needy 0.0.1`; `codesign -dv` showed a Developer ID Application signature
  with the hardened runtime (`flags=0x10000(runtime)`) and a timestamp;
  `spctl --assess --type execute` rejected it as `Unnotarized Developer ID`,
  as expected from a run that does not notarize; `needy profile check
  --local examples/macos-vz/profile.yaml` worked under the hardened
  runtime. The Linux x86_64 binary is `static-pie linked`.

Not verified, because it publishes: the GHCR pushes and manifests,
notarization and the upload to a release. The first release exercises
them.

## Candidate next slices

- **`needy image pull`** (or `needy build --prebuilt`): download the release's
  actor image and verify its `.sha256` into the directory `needy build`
  writes. Today that is a manual `tar -xzf` (README, Install), and a machine
  that only wants to run actors still needs Docker and a 20-minute kernel
  build without it.
- **Authenticode signing** of `needy.exe` once the owner has a certificate
  (`TODO(release-signing)`), so Windows SmartScreen does not warn on it.
- **The skipped Windows tests** (`TODO(windows-tests)`): a TCP listener in
  `needy-api-server`'s test harness on Windows would bring back 57 tests;
  the path-join and signal-name failures each need a look at whether the
  product or the test is wrong.
- **`buf breaking` in `check.yml`** once a release promises wire
  compatibility (`NOTICE(proto-breaking)`).
- **Guest lookup through symlinks**: the daemon looks for
  `needy-linux-<arch>` next to `current_exe()`, which on macOS is the
  symlink a program was started by. Also looking next to the resolved path
  would make any symlinked install find the guest, not only the channels
  that link it beside `needy` (Homebrew, proto).
- **A short install URL** (`NOTICE(install-url)`) once needy has a domain.
