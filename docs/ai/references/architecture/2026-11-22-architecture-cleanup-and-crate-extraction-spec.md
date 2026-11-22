# Architecture cleanup, CLI unification, and standalone image crates

Status: approved scope contract synthesized from the owner's decisions on
2026-11-22 following the
[codebase architecture review](2026-11-22-codebase-architecture-review.md).
Nothing in this contract is implemented yet; it does not claim shipped support.

## Problem Statement

As `needy` grew rapidly across VM actors, container actors, remote runners,
client execution, and layered images, four classes of structural debt
accumulated:

1. **Concrete bugs and broken loops**: `Daemon::serve` has an unreachable
   incomplete-shutdown branch; `Daemon::provision` calls `sync::check` twice;
   `Daemon::push_layer` leaves orphaned `upper.tar` files; `restore_run` drops
   most `Boot` fields across control restarts; `reconcile.rs` still skips
   `run.fail` traces despite `Dispatch::resume` existing; `needy fs cp --help`
   documents the wrong syntax; `needy proc exec -e` silently drops malformed
   entries; and `image pull`/`image rm` bypass `--diagnostic-format json`.
2. **Agent concepts and reusable OCI/Dev Container mechanics coupled into
   `needy` core**: `core-actor/src/image/feature.rs` embeds coding-agent
   builtins (`codex`, `pi`, `opencode`, `herdr`) in violation of `AGENTS.md`
   ("has no agent-specific concepts"), and `images/Dockerfile` bakes ~200 MB of
   `opencode` into base actor images. Meanwhile, `needy`'s pure-Rust OCI layer
   canonicalization/SquashFS writer and Dev Container feature resolver are
   tangled with `daemon`, `core-actor`, and `core`, preventing standalone reuse
   or clean Nix integration.
3. **Inconsistent CLI surface and leaking business logic**: `-o`/`--output`
   switches between four format enums and collides with `image export -o <PATH>`;
   global remote flags leak into local-only subcommands; human status prose
   mixes stdout and stderr; dozens of subcommands and flags have blank or
   jargon-heavy `--help`; and `crates/cli` directly hosts 1,075 lines of OS
   service management and 1,800+ lines of image pull/build mechanics.
4. **Monoliths, missing Rust traits, and duplicated plumbing**: 2,494-line
   `daemon/src/image.rs`, 1,332-line `daemon/src/run/mod.rs`, 1,119-line
   `api-server/src/control.rs`, near-zero `From`/`TryFrom`/`Display`/`FromStr`
   adoption, 3 `SingleFlight` implementations, 4 `Fake Engine` test mocks, 4
   `User::home()` copies, 54 poisoned-mutex unwraps, and colliding names
   (`holder`, `Placement`, `runner`/`runners`).

## Solution

Execute this cleanup in four ordered, independently verifiable tracks without
breaking the core actor/executor model:

1. **Track 1 — P0 Correctness & Loop Closure**: Fix all 8 identified runtime,
   recovery, and CLI bugs first, each preceded by a focused regression test.
2. **Track 2 — CLI Flag, Output, Help & Layering Unification**: Resolve the
   `-o`/`--output` collision, scope remote flags away from local-only commands,
   unify stdout/stderr/progress/JSON formatting in `crates/cli/src/output/`,
   complete and clarify all `--help` strings, and push OS service management
   and image mechanics down into `needy-daemon` and `needy-core-actor`.
3. **Track 3 — Agent Decoupling & Standalone Pure-Rust Crates (Nix-ready)**:
   Remove hardcoded coding-agent builtins from `core-actor` and `opencode` from
   `images/Dockerfile`; extract two zero-needy-dependency workspace crates —
   `oci-layer` and `devcontainer-features` — designed to plug cleanly into both
   `needy` and future daemonless Nix (`nix2container` / FOD) pipelines; unify
   Docker test mocks and move live-Docker unit tests to `xtask e2e`.
4. **Track 4 — Daemon/API Modularization, Idiomatic Rust Traits & Naming**:
   Split `daemon/src/image.rs`, `daemon/src/run/mod.rs`, `api-server/src/control.rs`,
   and `api-server/src/protocol/domain.rs`; adopt standard `From`/`TryFrom`/
   `Display`/`FromStr`/`AsRef` traits where orphan rules allow; eliminate
   duplicated plumbing (`SingleFlight`, `MutexExt`, atomic file writes,
   `User::home()`, session launchers); and resolve colliding module/type names.

## User Stories

### Correctness and reliability
1. As an operator, I want `needy daemon` shutdown and reconciliation to record
   accurate `run.fail` trace events and `IncompleteShutdown` status, so that
   logs and traces reflect what actually happened.
2. As an operator, I want a control-plane restart to preserve a surviving run's
   full `Boot` metadata (including feature `sync_files` and hooks), so that
   `sync::end` does not lose state when the run later stops.
3. As a CLI user, I want `needy proc exec -e BAD` to fail with a clear argument
   error rather than silently ignoring the variable, so that typos are caught
   immediately.
4. As a CLI user, I want `needy fs cp --help` to show the exact `remote:PATH`
   syntax the command accepts, so that following `--help` succeeds on the first
   try.
5. As an automation user passing `--diagnostic-format json`, I want errors from
   `image pull`, `image rm`, and `run create` to emit structured JSON
   diagnostics on stderr, so that my scripts never have to scrape prose.

### CLI consistency and discoverability
6. As a CLI user, I want `-o`/`--output` to always select output format and
   `--file` (or positional path) to select an output file, so that flags mean
   one thing everywhere.
7. As a CLI user, I want `--help` on every subcommand (including `daemon
   service *`, `config *`, `runner *`, `client *`, `proc *`) to concisely
   explain what the command and each flag do, with examples where helpful, and
   without showing irrelevant remote flags on local-only commands.
8. As a CLI user piping stdout (`-o json` or table output), I want human status
   confirmations and progress spinners to stay off stdout and suppress spinners
   in JSON mode, so that machine-readable output stays clean.

### Decoupled image crates and Nix readiness
9. As a `needy` maintainer, I want `needy`'s core crates and base actor images
   to contain zero hardcoded coding-agent binaries or builtin features, so that
   `needy` stays true to its agent-agnostic boundary and base image builds stay
   small and cache-friendly.
10. As a Rust or Nix developer, I want `oci-layer` and `devcontainer-features`
    available as clean, standalone Rust crates with no `needy-*` dependencies,
    so that I can reuse OCI layer canonicalization, SquashFS packing, and Dev
    Container feature resolution/topological sorting in `needy` and in
    daemonless Nix workflows.
11. As a contributor running `cargo test`, I want container driver tests to use
    one shared fake engine and never silently skip live-Docker checks, so that
    unit tests are fast, deterministic, and honest.

### Maintainability and idiomatic Rust
12. As a Rust contributor, I want standard conversions (`From`/`TryFrom`/
    `Display`/`FromStr`/`AsRef`) and focused per-service modules instead of
    multi-thousand-line monoliths, so that adding a field or RPC does not
    require hunting across parallel boilerplate functions.
13. As a contributor reading `crates/daemon`, I want distinct names for
    distinct concepts (local restart-survival `holder` vs client execution
    bridge, `Placement` vs host placement, worker `runner` vs control-plane
    `runners`), so that module boundaries are obvious at a glance.

## Implementation Decisions

### Track 1: P0 Correctness & Loop Closure
- **`Daemon::serve` (`crates/daemon/src/lib.rs`)**: Remove the redundant second
  `suspend_control()` call, align the doc comment with actual suspend vs
  shutdown behavior, and ensure `IncompleteShutdown` is only checked on paths
  that actually set `cleanup_incomplete` (or propagate incomplete suspension
  truthfully).
- **`Daemon::provision` (`crates/daemon/src/run/mod.rs`)**: Remove the duplicate
  `sync::check(self, run)?` call.
- **`Daemon::push_layer` (`crates/daemon/src/guest.rs`)**: Either remove the
  unused `upper.tar` write path on non-session runs (returning a clear error if
  called outside an image session) or explicitly wire its lifecycle; do not
  leave unconsumed files in `run.dir`.
- **`restore_run` (`crates/daemon/src/run/recovery.rs`)**: Persist and restore
  the complete `Boot` fields needed after restart (`sync_files`, `start_hooks`,
  `stop_hooks`, `env`, `path`, `resolved_features`, `snapshot_image`,
  `commit_rebased`).
- **`reconcile.rs`**: Use `Dispatch::resume` (`run_trace_at`) to emit `run.fail`
  when reconciliation marks a lost run failed, removing the stale `TODO(trace)`.
- **`fs cp` & `proc exec`**: Fix `commands/fs.rs` help to document `remote:PATH`;
  validate `proc exec -e KEY=VALUE` with the same `value_parser` used by
  `run create --env` (and add `-e` short alias to `run create --env`).
- **Diagnostics consistency**: Route `image pull`, `image rm`, and `run create`
  validation failures through `diagnostics::Error` so `--diagnostic-format json`
  is respected everywhere.
- **Dead/duplicated paths**: Remove the unreachable 404 pull fallback in
  `core-executor/src/driver/linux_container/mod.rs` and replace the 14 inline
  `.env(...)` calls in `core-actor/src/guest/init/program.rs` with
  `environment(config, home)`.

### Track 2: CLI Flag, Output, Help & Layering Unification
- **Flag contract (breaking in `0.0.x`, documented directly)**:
  - Reserve `-o` / `--output` exclusively for output **format** across all
    commands. Change `needy image export` to take `--file <PATH>` (matching
    `needy run report --file`) or a positional `<PATH>`.
  - Stop marking `--context`, `--address`, and `--credential-file` as
    `global = true`; attach a shared `#[command(flatten)] ConnectionArgs` only
    to commands that talk to a control daemon. Eliminate the `#[arg(skip)]`
    workaround in `config context pair/set`.
  - Add `--quiet` / `-q`, support `RUST_LOG` fallback when `-v` is unset, guard
    `config context pair`'s save window with `Termination`, and include `cache`
    in `needy config paths`.
- **Output contract**:
  - Centralize TTY detection, `NO_COLOR`, `TERM != "dumb"`, time/size
    formatting (`ago`, `time`, human bytes), and all `indicatif` progress bars
    inside `crates/cli/src/output/`.
  - Route every non-data stderr line through `output` (no raw `eprintln!` /
    `writeln!(stderr)` in `commands/` or `diagnostics.rs`).
  - Keep human mutation confirmations (`daemon service install`, `runner join`,
    `run chaos add/rm`, `image rm/rollback`) on stderr via `output::acknowledge`
    so stdout stays reserved for command output / tables / JSON.
  - Suppress spinners whenever `-o json` or `--diagnostic-format json` is active
    (including `needy image log -o json`).
  - Normalize single-object JSON outputs (`config context check`, `daemon
    service status`, `profile check`, `client/runner invite`) under one
    documented JSON convention.
- **Help text contract**:
  - Every command, subcommand, and flag must have a concise one-line `about`/
    `help` (no trailing period inconsistency, no internal jargon) and optional
    `long_about` / `after_help` for complex commands (`image build`, `run
    create`, `daemon service`).
  - Split `needy image build`'s three unrelated modes clearly in help (or move
    contributor base-image builds to `xtask` / an explicit subcommand).
- **Layering**:
  - Move `commands/daemon/service/` (1,075L) into `needy-daemon::service`.
  - Move image pull/build/sparse-unpack/local-store mechanics out of
    `crates/cli/src/commands/image/` into `needy-core-actor::image` (with a
    progress callback trait), removing `bollard`, `reqwest`, `flate2`, `tar`,
    `sha2`, `ignore` from `crates/cli/Cargo.toml` where no longer needed.
  - Move `WRITABLE` config path mutation into `needy-config::global`, tar
    pack/unpack in `fs.rs` into `needy-api-client::files`, and `execution-holder`
    spawning into `needy-daemon::runner`.
  - Reorganize `crates/cli/src/commands/`: `commands/config/{mod,context,profile}.rs`,
    move `commands/daemon/runner.rs` into `commands/runner/`, rename
    `commands/proc/ps.rs` → `ls.rs`, and split `crates/cli/tests/cli.rs` (4,441L)
    into domain submodules.

### Track 3: Agent Decoupling & Standalone Pure-Rust Crates
- **Agent-agnostic core & base images**:
  - Remove hardcoded coding-agent builtins (`codex`, `pi`, `opencode`, `herdr`)
    from `crates/core-actor/src/image/feature.rs`. Keep only generic
    infrastructure features (e.g., `sshfs`) or load agent features from external
    OCI references / user-supplied feature directories / separate presets
    outside core crates.
  - Remove the ~200 MB `opencode` download from `images/Dockerfile`
    (`actor-root` and `container-root`), deduplicate the shared base package
    stage, verify SHA-256 checksums for all downloaded build tarballs (kernel,
    Firecracker), and avoid flattening `actor-container` into a single layer
    with `FROM scratch` + `COPY / /` where layered caching is desired.
- **New workspace crate: `crates/oci-layer` (`oci-layer`)**:
  - Pure Rust, zero `needy-*` dependencies.
  - Adopt Tier-1 maintained ecosystem crates (`oci-spec` for OCI Image/Layout
    structs) instead of hand-rolling OCI JSON schemas. Evaluate Tier-2 crates
    (`ocidir`, `ocirender`) during implementation: only adopt them if they fit
    cleanly without regressions; otherwise keep the focused pure-Rust
    overlayfs/whiteout/SquashFS implementation and leave a `// TODO:` note
    recording the revisit condition.
  - Owns `Digest`, overlayfs ↔ OCI whiteout tar canonicalization (`archive`),
    algebraic layer collapse (`collapse`), generic OCI Image Layout & Docker
    `tarexport` read/write (`oci`) with a caller-supplied post-layer hook (so
    needy's shim synthesis stays in `needy-core-actor`), and optional SquashFS
    disk generation (`disk`, behind a `squashfs` feature flag).
  - Designed to accept tar streams from any source, including daemonless Nix
    builders (`nix2container` / `dockerTools.streamLayeredImage`).
- **New workspace crate: `crates/devcontainer-features` (`devcontainer-features`)**:
  - Pure Rust, zero `needy-*` dependencies.
  - Adopt Tier-1 maintained ecosystem crates: `oci-client` (CNCF ORAS OCI
    Distribution v2 client, replacing hand-rolled HTTP/RFC 6750 Bearer parsing),
    `docker_credential` (reading `$DOCKER_CONFIG/config.json` and system
    credential helpers), and `jsonc-parser` (parsing JSONC comments and trailing
    commas in real-world `devcontainer-feature.json` files).
  - Owns `devcontainer-feature.json` schema (`FeatureMetadata`, `OptionSpec`,
    `LifecycleCommand`, `sanitize_option_name`, generic `customizations:
    serde_json::Value`), `dependsOn` transitive resolution + cycle detection +
    true `installsAfter` topological sorting, content-addressed feature caching,
    and `install.sh` environment/wrapper generation.
  - Designed so Nix tooling can resolve and pin features to FODs without
    depending on `needy`.
- **Docker test hygiene**:
  - Consolidate the 4 duplicated `impl Engine for Fake` mocks into one shared
    `needy_core_executor::engine::testing::Fake`.
  - Replace brittle 11–12 step Docker call-sequence assertions in
    `linux_container/mod_test.rs` with contract-level assertions.
  - Move silently-skipping live Docker unit tests (`core-executor/src/driver/
    linux_container/mod_test.rs:317-393`) into `cargo xtask e2e`.
  - Break the `api-client` ↔ `core-executor` dev-dependency cycle.

### Track 4: Modularization, Idiomatic Rust Traits & Naming
- **Decompose monoliths**:
  - Split `crates/daemon/src/image.rs` (2,494L) into `daemon/src/image/{mod,
    build,edit,session,feature,container,single_flight}.rs`, unifying the 3
    session launchers (`run_session_in`, `run_container_session_in`,
    `start_edit`) and replacing all 3 `SingleFlight` implementations with one
    shared `SingleFlight<K, V>`.
  - Split `crates/daemon/src/run/mod.rs` (1,332L) into `registry.rs`,
    `lifecycle.rs`, `logs.rs`, and `recovery.rs`; attach `recovery_test.rs` to
    `run/recovery.rs` instead of `reconcile.rs`; move `store/mod.rs` runner CRUD
    into `store/runners.rs`.
  - Split `crates/api-server/src/control.rs` and `protocol/domain.rs` per
    service to mirror `protocol/grpc/v1/`; extract `Decide` into a focused trait
    so `EditSessionRuns` no longer stubs 11 `role_denied` methods; replace
    `control::GuestUser` with `config::profile::User` and share `ImageTarget`
    across `ImageBuild`/`ImageEditStart`/`ImageExport`.
  - Refactor `crates/core/src/report/mod.rs` so `write_report()` renders from
    `Summary` (eliminating duplicate finding/sorting logic) and extract
    `markdown.rs` beside `human.rs`.
  - Split `crates/core-executor/src/process/proc.rs` (`seats.rs`, `fanout.rs`)
    and `crates/common/src/merge.rs` (replacing `anyhow::Result` with
    `thiserror`).
- **Idiomatic Rust traits**:
  - Implement `From<domain::Error> for tonic::Status` in `api-server`,
    removing 12 `.map_err(ControlError::from)?` sites and 7 manual
    `status::status(e)` calls.
  - Implement `From`/`TryFrom` for all orphan-rule-eligible local `control::*`
    ↔ `v1::*` types (including `WindowSize`, `GuestCopy`, `RecordedCommand`).
  - Add `#[source]` to `ControlError::Diagnostic`.
  - Implement `Display`, `FromStr`, `AsRef`, `From`, and `Default` on value
    types across `common`, `config`, `core`, `api-client`, and `api-server`, and
    emit `From<Payload> for Event` from `core`'s `events!` macro.
  - Share proto enum decoders in `needy-api-client` (or a small wire-helper
    module) so `cli` stops duplicating `api-server/src/protocol/domain.rs`.
- **Shared high-frequency helpers & deduplication**:
  - Add `needy_common::fs::write_atomic` (replacing 14+ sites), a shared
    `MutexExt` / `lock()` helper (replacing 54 sites), a single `User::home()`
    helper (fixing `core-executor/options.rs:80` for `root`), and replace
    hand-rolled `shell_join` with `shell-words`.
  - Unify `config::profile::files::OnConflict` and `common::merge::Choice` via
    conversion or shared type.
- **Naming clarity**:
  - Disambiguate `holder` (keep `daemon::holder` for local restart-survival;
    rename client-side execution holder to `client_bridge` or `client_agent`),
    `api-server::control::Placement` → `HostPlacement`, `run::recovery::Snapshot`
    → `RunCheckpoint`, and group daemon worker roles (`runner`, `executor`)
    separately from control-plane registries (`runners`, `clients`).

## Testing Decisions

- **Regression-first for every Track 1 bug fix**: add a failing unit or
  in-process daemon test before fixing each of the 8 defects (including
  `recovery.rs` `Boot` round-trip, `reconcile.rs` `run.fail` trace emission,
  `proc exec -e` rejection, `fs cp` help/syntax contract, and
  `--diagnostic-format json` on `image pull`/`rm`).
- **CLI contract tests**: verify `-o` format vs `--file` path, absence of
  remote flags on local-only `--help`, non-empty `--help` descriptions for
  every leaf subcommand, clean JSON stdout (no spinners or prose) under
  `-o json`, and exit codes.
- **Standalone crate independence tests**: verify `cargo test -p oci-layer` and
  `cargo test -p devcontainer-features` build and pass with zero `needy-*`
  dependencies, including `installsAfter` topological ordering and SquashFS
  round-trip tests.
- **Workspace validation**: after each track, run `make check` and `make test`
  (plus `cargo xtask e2e` for container/VM integration cells when touching
  drivers or `images/Dockerfile`). Never count skipped live-Docker tests as
  passes.

## Out of Scope

- Implementing full Nix flake/FOD generators or a Nix actor driver in this
  effort (this contract prepares clean crate seams for Nix, not the Nix
  integration itself).
- Rewriting or removing the Firecracker identity patch (`images/firecracker/
  needy.patch`) or switching SquashFS to EROFS.
- Implementing currently deferred features (`TODO(runner-actor)`,
  `TODO(runner-docker)`, `executor_network`, `files.ignore_file`, `otel` sink,
  `ask` interceptor, `onCreateCommand`/`postAttachCommand`, `~/.docker/
  config.json` private registry auth).
- Changing `proto/` wire compatibility for existing remote runners/clients
  beyond what is explicitly required by Track 1–4.
- Introducing new CLI subcommand groups or interactive confirmation prompts
  (`--yes`) unless explicitly requested in a follow-up slice.

## Further Notes

- Source findings, exact file/line citations, crate dependency DAG, and
  end-to-end call-chain maps live in the companion
  [codebase architecture review](2026-11-22-codebase-architecture-review.md).
- When landing Track 2 (CLI flag/output changes) and Track 4 (naming changes),
  update `docs/TERMS_AND_CONCEPTS.md`, `README.md`, and `AGENTS.md` in the same
  slice so documentation stays synchronized with the code.
- Execute the four tracks as separate, reviewable PRs/slices in order
  (Track 1 → Track 2 → Track 3 → Track 4) so each slice remains small,
  bisectable, and independently verifiable.
