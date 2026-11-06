# Rust rewrite verification

Checked on 2026-11-04 in `.worktrees/rust-rewrite`, branch `feat/rust-rewrite`,
starting from `dbea069`. Package publication and remote pilot rollout were not
performed. The previous Go implementation remains available at that Git
baseline and in `.worktrees/headless-mvp`.

## Evidence and limits

| Boundary | Evidence |
| --- | --- |
| Rust checks | macOS ARM64: format, Clippy with warnings denied, unit tests; Linux x86-64: the same unit tests and Clippy |
| Independent Linux library | 11 actual-kernel tests passed on x86-64 Linux and ARM64 Linux; standalone redirect example executed; `cargo package` verified on macOS and Linux |
| Root/process identity | Root first exec remains local; redirected PID/parent/wait relationship preserved; proxy descendants remain local; execveat empty path and nonleader-thread exec verified |
| Kernel error/cleanup | Denial returns errno; native signal death retained; invalid pointers return EFAULT; arbitrary argv/env bytes retained; cancellation/error cleanup and vfork scratch reclamation verified |
| Native host execution | Actual macOS and x86-64 Linux commands, stdin/stderr/status, PTY/resize, SIGINT, client/daemon SIGKILL cleanup, revocation and persisted one-time pairing |
| Linux to Mac | Actual ARM64 Linux VM to native macOS over HTTPS with a private test CA: platform identity, pipeline/status, SIGTERM and cross-OS SIGUSR1 mapping, interactive PTY/resize/Ctrl-C, terminal restoration after supervisor cancellation |
| Docker product | Non-root persistent workspace, upload/download and path validation, idempotency, client disconnect, daemon SIGKILL/recovery without duplicate side effects, event cursors, host/session concurrency, cancellation, early stdout EOF and output limits |
| Docker faults | Actual containers with one external CLI failure/delay: log observation retry retains reservation without replay; cancellation before create prevents execution; missing or foreign volumes rejected; partial volume deletion recoverable |
| Durable journal | Incomplete final append truncated; complete corruption rejected |
| Installer | Optimized Rust binary installed into an isolated prefix and exercised with the native host suite |

The kernel environments were native x86-64 Linux in a dedicated rc workspace
and native ARM64 Linux in the local Docker VM. ARM64 kernel tests ran as a
non-root user with all capabilities dropped, `--init`, no-new-privileges and a
per-container seccomp exception for ptrace. Linux-to-Mac testing used separate
OS environments on the same physical Mac; a second physical client machine was
not tested.

The crate is experimental and unpublished. Its package name and license still
need a publication decision; `cargo package` currently emits a missing-license
warning. No full shell job control, filesystem/descriptor virtualization or
automatic source synchronization is claimed. See the [crate contract](../crates/exec-intercept/README.md).

## Reproduce local checks

Rust 1.97+, Python 3.9+ and a running Docker backend are required for the full
suite. Linux kernel tests also need a C compiler, or an executable C fixture
specified by `NEEDY_KERNEL_FIXTURE`. From the repository root:

```sh
make check
make test
make build
make host-test
make test-image
make docker-test
for fault in observation cancel volume foreign-volume volume-delete; do
  python3 scripts/test-rust-docker-faults.py bin/needy "$fault"
done
cargo package -p needy-exec-intercept --allow-dirty
```

Tests generate private temporary profiles and remove only their own labelled
containers/volumes.

On Linux, run the independent kernel suite and example directly:

```sh
cargo test -p needy-exec-intercept --test kernel -- --test-threads=1
cargo run -p needy-exec-intercept --example redirect
```

## ARM64 Linux from macOS

The cross-machine script expects these local artifacts. Install
`cargo-zigbuild@0.24.0`, put Zig 0.15.2 on PATH and add Rust target
`aarch64-unknown-linux-musl`, then build:

```sh
mkdir -p .needy
CARGO_TARGET_DIR=.needy/linux-arm64 cargo zigbuild \
  --target aarch64-unknown-linux-musl --workspace --all-targets
zig cc -target aarch64-linux-musl -static -pthread -O0 \
  crates/exec-intercept/tests/fixture.c -o .needy/kernel-fixture-arm64
python3 scripts/test-rust-cross-machine.py
```

The script uses the local `needy-rust-test:dev` image from `make test-image`
for `/bin/sh`, Node and the Docker TTY, and mounts the new Rust binary explicitly.
Override `NEEDY_LINUX_IMAGE` to select another compatible local utility image. It listens temporarily
on HTTPS with a generated one-day test CA and removes its own state afterwards.

For the ARM64 kernel executable built above:

```sh
kernel=$(find .needy/linux-arm64/aarch64-unknown-linux-musl/debug/deps \
  -name 'kernel-*' -type f -perm -111 | head -n 1)
docker run --rm --init --cap-drop=ALL --security-opt=seccomp=unconfined \
  --security-opt=no-new-privileges --user "$(id -u):$(id -g)" \
  -v "$PWD:/work:ro" -e NEEDY_KERNEL_FIXTURE=/work/.needy/kernel-fixture-arm64 \
  --entrypoint "/work/$kernel" needy-rust-test:dev --test-threads=1
```

## Image build acceptance

`images/Dockerfile` has source-build and prebuilt binary stages and a single
runtime target. The source-build path uses Rust 1.97 and the locked workspace. An
optional prebuilt Linux binary must match the target image's architecture:

```sh
mkdir -p .needy/image
cp .needy/linux-arm64/aarch64-unknown-linux-musl/debug/needy .needy/image/needy
make test-image BINARY_STAGE=prebuilt
```

The production recipe's prebuilt path was exercised using an existing local
Node-based image through `--build-arg NODE_IMAGE=...`. Pulling a fresh public
Node base stalled, so a completely cold Docker source-build was not accepted
here. Native/cross Rust builds, real Linux compilation and the built runner's
execution were verified separately. This does not require the old Go source.

## State and rollout boundary

Use a new private Rust data directory. A Go-only `state.json` directory is
rejected rather than implicitly converted. Existing Go session metadata,
volumes and the earlier remote pilot were not migrated or modified.
The Rust state directory and per-run inputs remain private data.

The dedicated remote kernel-validation workspace is temporary; validation
workspace and its 20 GiB home PVC were confirmed deleted after the run;
no `needy-rs-` test containers remained. Build binaries, local development
images and the unpublished `.crate` archive can be retained for inspection.
