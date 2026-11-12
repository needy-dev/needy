# Repository Guidelines

Needy separates a driving program (the actor) from the environments where its
work takes effect (executors). It has no agent-specific concepts.

## Project Structure

Dependencies point downward; a crate never depends on one above it.

- `crates/cli` (`needy-cli`, binary `needy`): argument parsing, one module per
  command in `commands/` (each owns its `Args` and `run`), output formats
  (`output/`, tables and JSON), XDG paths (`paths`) and process logs
  (`logging`, `tracing`). It composes the crates below and, inside an actor,
  dispatches to the guest init and proxy by `argv[0]`.
- `crates/daemon` (`needy-daemon`): the control plane's policy and the
  executor role: `run` (create, get, list, stop, and the run state
  transitions), `pool` (instances by key, refcounts, slots), `schedule`
  (which runner a profile's `runner:` selector names, and where an actor or
  executor is created), `check` (`profile check` through the control plane),
  `routing` (the `exec.decide` chain, path maps), `relay` (exec streams and their stream
  filters), `files` (copy-in, mirror-out), `actor` and `guest` (a run's
  VM, gateway and guest API), `runners` (the runner registry) over `store`
  (`state.db`, SeaORM, separate from `trace.db`), and the `executor` and
  `runner` roles. It implements
  `needy-api-server`'s domain traits and serves them through its `Server`; it
  never builds a gRPC status. Every long-lived task belongs to the daemon's
  or a run's `TaskTracker`. Mechanisms live below.
- `crates/core-actor` (`needy-core-actor`): the actor. `guest` (Linux only)
  is the in-guest init and exec-routing proxy, on `nix`, logging to the
  console. `host` runs next to the actor: `detect` reads this machine's CPU,
  `driver` is the actor `Driver` trait (start, wait, stop, and the guest,
  gateway and terminal channels the daemon serves or dials) with its
  `Running` actor, `firecracker` implements it as `linux-firecracker`: it
  builds the Firecracker configuration from a compiled recipe and owns the
  Firecracker process; `container` implements it as `linux-container`
  through a Docker-compatible engine (`identity` writes the recipe's files),
  and `macos` holds the Mac's drivers: `vz` (`macos-vz`) for
  Virtualization.framework VMs on Apple silicon (vfkit's arguments from a
  compiled recipe, the vfkit process) and `apple_container`
  (`macos-apple-container`) over Apple's `container` CLI; and `gateway` is
  the actor gateway on the `net.*` interceptors.
- `crates/core-executor` (`needy-core-executor`): where commands run.
  `process` is the process host (PTY, pipes, replay, detach, expiry of
  finished processes) with platform code in `process/platform`; `service` is
  the process service one machine serves; `driver` is the executor `Driver`
  trait that creates and destroys executor instances (`native`, and
  `linux-container` through `bollard` in `docker`) and owns the container
  run-option policy; `relay` connects an exec stream to the local
  terminal (`crossterm`) for the CLI and the proxy.
- `crates/core` (`needy-core`): domain logic shared by everything: it
  interprets loaded profiles and recipes (`rules`, `recipe` compilation to a
  typed VM identity, `chaos`), and owns command patterns (`pattern`), the
  interceptor traits of each hook point (`intercept`), the values producers
  and consumers share (`types`), the typed event schema (`event`) and report
  rendering (`report`, no IO). No processes, listeners, VMs or storage.
- `crates/trace` (`needy-trace`): the event pipeline. It carries an envelope
  (`event`: run, sequence number, time, kind and a JSON payload) and does
  not know event types. `Dispatch` is one run's bounded queue: it never
  blocks a producer, counts what a full queue drops and records it as
  `trace.gap`. Sinks sit behind features: `sqlite` (`trace.db`, SeaORM
  entities and migrations, and the reader `needy report` uses), `jsonl`,
  `memory` for tests, and `timing` for per-hook latency. Depends only on
  `common`.
- `crates/config` (`needy-config`): what users write. Profile and recipe
  schemas, loading from files, `builtin:NAME` and text with `extends` chains
  (`source`), the one merge model (`merge`), cross-reference checks
  (`validate`), user-written values such as durations and sizes (`value`), and
  the embedded built-in recipes (`builtin`). Unknown keys are errors. No
  processes, sockets or gRPC.
- `crates/common` (`needy-common`): shared helpers that depend on neither the
  platform nor an async runtime and know no needy concepts: filters, tar
  archives, exit status and signal names, seed mixing and wall-clock time.
- `crates/api-proto` (`needy-api-proto`): generated Protobuf and gRPC types.
  Do not edit `api-proto/src/gen`; change `proto/` and run `make proto`.
- `crates/api-client` (`needy-api-client`): the outbound side. `Client`
  connects over Unix sockets, HTTP(S) and vsock with a token the caller
  gives; `ready` waits until a server answers; `exec` is an exec stream (start,
  attach, input, events, run to completion); `files` moves tar archives over
  exec; `Error` maps a status to a `Kind`. The CLI, the proxy, the drivers,
  the daemon's relay and xtask use it instead of raw streams.
- `crates/api-server` (`needy-api-server`): the inbound side. `control` holds
  the domain traits (`Runs`, `Processes`, `Guest`, `Runners`), `Caller` (the
  CLI, an actor bound to its run, or a runner by its credential) and
  `ControlError`; `server` binds every listener before serving; `protocol`
  has the gRPC adapters, the proto <-> domain conversions and the one status
  table; `middleware` is bearer authentication, which decides each request's
  caller; `termination` turns signals into cancellation.
- `xtask` (`cargo xtask`): developer tasks that are not product commands,
  today the `bench` latency and throughput probes.
- `proto/`: canonical API schemas, linted and generated with Buf.
- `images/`: actor kernel sources and configurations (x86_64 for
  Firecracker, arm64 for `macos-vz`), the Firecracker patch and image builds.
- `docs/`: documentation. See [`docs/README.md`](docs/README.md).

### Extension points

Where needy is meant to be extended sideways (actor drivers, executor
drivers, sidecars, and later captures and interceptors), one layout rule
holds, from the
[drivers design](docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md):

1. An extension point is a directory. Its `mod.rs` holds the trait and a
   table of every implementation.
2. Each implementation is one file or one directory, named after its config
   value with `-` written `_` (`linux-container` is `linux_container`).
3. Implementations never depend on each other. What two of them share moves
   into a component module named after what it is.

## Shared Vocabulary

- Use [`docs/TERMS_AND_CONCEPTS.md`](docs/TERMS_AND_CONCEPTS.md) as the shared
  vocabulary for actors, executors, runs, profiles, recipes, routing,
  traces and reports.
- When a design introduces or changes a core term, update
  `docs/TERMS_AND_CONCEPTS.md` instead of defining the term only inside a
  transient spec.

## Scope Discipline

Public capability claims must name an evidence level and link to current
evidence. A live probe is not a support claim, and compilation is not behavior.

Before editing, classify the change as one of: bug fix, test-only, docs-only,
narrow refactor, or approved feature. If none fits, ask for a smaller slice.

Some missing pieces are intentional deferrals, some are incomplete work, and
some are real bugs. Do not guess which one you are looking at. Classify the
gap from evidence, failing tests, owner instructions, or existing reference
docs before implementing anything.

Scope rules:

- Do not implement TODOs, roadmap notes, or future-phase designs unless the
  owner names that slice.
- Do not run broad repository scans and turn the findings into drive-by
  changes. Search only to understand the assigned slice.
- Do not mix unrelated cleanup into behavior changes. Mention cleanup
  opportunities as follow-up candidates instead.
- Cross-layer changes are allowed when they are the approved contract slice,
  but the dependency direction must be clear. Example: proto field -> executor
  service -> daemon routing -> CLI output.
- Avoid ad-hoc compatibility shims. Versioned read compatibility for existing
  traces or public records is allowed when the migration boundary and tests
  are explicit.

Owner approval means the owner named the function/module/behavior, accepted a
concrete proposal, or asked for a specific next slice. A doc mentioning a
future feature, a TODO marker, or a change that feels like "the obvious next
thing" is not approval.

When a good idea appears mid-task, do not implement it inline. Record it as a
candidate next slice with one sentence explaining why it matters, then finish
the current slice.

When you decide *not* to implement something that a reader could plausibly
expect — a field, an enum variant, a branch, an algorithm stage, an API
surface, a fallback path, or any other surface that has been considered and
intentionally left out for the current slice — leave a `TODO:` or `NOTICE:`
marker at the type or call site where the deferral lives. The goal is to
make intent visible: a future reader (Codex, Claude, the owner, a reviewer)
must be able to tell *not yet, on purpose* apart from *forgot to write it*.
A missing marker forces the next reader to guess and frequently produces
unwanted scope expansion when an agent treats the absence as a gap.

A useful deferral marker names three things in one short comment: the gap
(what was considered but omitted), the reason it is deferred (why it does
not belong in this slice), and the trigger that would unlock it (what
condition or owner approval would re-open the decision). When the deferral
is already covered by a reference doc, the inline marker may simply cite
that doc instead of restating the rationale — for example,
`// TODO(executor-pool): idle_timeout deferred, see 2026-11-09-closed-loop-results-evidence.md`.
The rule is that *something at the code site* must point a reader to the
intentional gap.

Apply this discipline to:

- Enum variants you considered but chose not to add.
- Struct fields you considered but did not add.
- Algorithm stages, fallback paths, or recovery branches you reserved but
  did not implement.
- API surfaces you chose to keep private, omit, or stub for the current
  slice.
- Reader-side checks that mirror a producer-side guarantee (such as
  version rejection on a wire-shape field) when only the producer side
  has landed.

A deferral marker is not approval to implement the deferred work later
without owner involvement. It is documentation of an existing decision so
the decision survives the next read of the code.

## Rust Style

- Rust code uses the 2024 edition.
- The repository `rustfmt.toml` sets two-space indentation
  (`tab_spaces = 2`).
- Prefer idiomatic Rust naming:
  - `snake_case` for functions, modules, and variables.
  - `PascalCase` for types and traits.
  - `SCREAMING_SNAKE_CASE` for constants.
- Keep public APIs small and documented.
- For design terms that are still under discussion, mark them as provisional in
  docs rather than stabilizing them through code names too early.
- Avoid hardcoded Unix, macOS, or Windows path literals unless the path is
  truly platform-defined.
- Prefer `Path`, `PathBuf`, component joins, and path-safe arguments over string
  concatenation.
- Do not move every literal into a constant. One-time or two-time values should
  remain near usage when locality is clearer.
- For retry, timeout, backoff, buffer size, and limit values, avoid one broad
  constant that silently governs unrelated behavior.
- Prefer a maintained library over hand-written parsing, matching, archive or
  platform code. Write it yourself only when no library fits, and say why in a
  comment.
- Use `anyhow` with `.context()` in binaries and composition crates, and
  `thiserror` in library crates whose callers match on errors. Do not drop
  errors with `let _ =` unless the failure is truly irrelevant; say why in a
  comment when it is.

## Naming & Comments

- Keep third-party names intact. When renaming first-party terms, migrate
  producers, consumers, proto fields, tests, and current documentation
  together. Add a migration notice to older design notes instead of rewriting
  historical evidence.
- Prefer names that rely on the module boundary for context instead of
  repeating product, platform, protocol, or transport prefixes inside every
  symbol.
- A well-named module should let exported functions use short action-first
  names; repeat the larger context only when the symbol crosses a boundary where
  that context is no longer obvious.
- Name functions after the domain operation they perform, not after the
  implementation layer that happens to contain them.
- Use nouns for resolved domain concepts and verbs for transformations or side
  effects.
- When a function derives a policy, configuration, selector, or trace decision
  from an event or request, name the domain result explicitly so callers
  understand what decision is being made.
- Avoid names that encode multiple layers of ownership into one symbol.
- If a name needs several qualifiers to be understandable, reconsider the module
  boundary or introduce a clearer local concept.
- Use dependency injection only at real external boundaries: filesystem,
  process, clock, environment, OS APIs, hypervisors, container runtimes, model
  runtimes, network, cache, and feature gates.
- Do not introduce `Dependencies` or `Deps` structs for internal functions that
  only call sibling helpers or forward parameters.
- Add clear, concise comments for utilities, parsing, OS interaction, kernel
  and hypervisor interfaces, wire formats, scheduling, persistence, and
  architectural functions when the intent, invariant, or platform constraint is
  not obvious from names and local context.
- Avoid comments that repeat what the code already says.
- When moving, refactoring, fixing, or updating code, keep existing comments
  intact and move them with the code.
- If a comment is truly unnecessary, replace it with a clearer comment or remove
  it only when the surrounding code fully preserves the original context.
- Use markers:
  - `TODO:` follow-up work that is intentionally left for later.
  - `REVIEW:` concerns that need another maintainer or design pass.
  - `NOTICE:` magic numbers, workarounds, platform quirks, generated-code
    constraints, or important external context.
- When using a workaround, add a `NOTICE:` comment explaining why it is needed,
  the root cause, and the removal condition.
- If a workaround was validated through platform documentation, dependency
  source, generated output, or a command, include the relevant file, command,
  issue, or URL in code-formatted text.

## Provenance of Hard-coded and Copied Data

needy fakes machines, so it carries many values that come from somewhere
else: CPUID leaves and feature bits, `/proc/cpuinfo` and `dmidecode` samples,
SMBIOS/DMI strings, PCI vendor and device IDs, kernel config fragments, the
Firecracker patch, terminal escape sequences, errno and signal tables, protocol
constants, and test fixtures. A reader must be able to tell where each one came
from and check it again.

Every such value, table, file or patch hunk carries, at the place it is
defined (a comment above it, a header comment in a data file, or a
`SOURCES.md` next to a directory of fixtures):

- **Permalink**: a URL pinned to an immutable revision, with line anchors where
  the format allows: `https://github.com/<org>/<repo>/blob/<commit>/<path>#L10-L24`,
  a tagged release, a versioned specification section (e.g. a DMTF SMBIOS
  version, an Intel SDM order number and revision), or a dated archive snapshot
  when the page has no versions. Never link a moving branch (`main`, `master`).
- **Reference link**: the human-readable page that explains the value (the
  specification section, the man page, the upstream documentation).
- **For data captured from a machine**: the exact command, the machine by role
  and hardware only (see [Private Data](#private-data)), and the capture date,
  plus what was removed or changed (serial numbers, UUIDs, MAC addresses).
- **For derived values**: how they were derived from the source (e.g. "bits
  from SDM Vol. 2A Table 3-8, CPUID.07H.0:EBX"), so a reviewer can redo it.

Format in code:

```rust
// Source: https://github.com/firecracker-microvm/firecracker/blob/<commit>/src/vmm/src/cpu_config/x86_64/cpuid/common.rs#L40-L58
// Reference: Intel SDM Vol. 2A (order 325462-0xxUS), CPUID leaf 07H
// NOTICE: avx_vnni is CPUID.(EAX=07H,ECX=1):EAX[4]; hiding it needs subleaf 1,
// which hosts without that subleaf reject.
```

In YAML and other data files use the file's comment syntax at the top of the
file or the entry. If a source has no stable permalink, say so and give the
retrieval date. A value without provenance is a review finding; when you touch
one, add its provenance or a `TODO(provenance):` naming what is missing.

## Testing Practices

- Focused unit tests live next to the code they cover with `#[cfg(test)]`.
- The repository has Rust unit coverage for profiles, rules, recipes, chaos,
  archives, signals and bearer authentication. The daemon is tested in
  process through `needy-api-server`'s traits with a native executor
  (runs, pool, routing, relay, shutdown, the runner registry, the
  runner role, scheduling and profile checks, with runners joined in
  process and a stand-in Firecracker); real actors and linux-container executors are
  verified end to end on hosts (see the closed-loop results).
- Add regression tests for behavior changes and keep them narrow.
- Use `cargo test` for the full suite and include regression tests for bug
  fixes.
- For any investigated bug or issue, try to reproduce it first with a test-only
  reproduction before changing production code.
- Prefer a unit test. If that is not possible, use the smallest higher-level
  automated test or recorded fixture that can still reproduce the behavior.
- When a regression test maps to an external report, include the tracker
  identifier in the test name when practical.
- Add the report link as a comment directly above the regression test.
- For local investigations without a tracker, leave a short root-cause comment
  when it would help future maintainers understand why the test exists.
- Test through stable public behavior.
- Do not create new exports, dependency bags, wrapper services, or alternate
  code paths only to make private implementation details mockable.
- Avoid smoke-only tests for behavior changes.
- Assert the observable output, trace record, artifact, replay input, command
  result, or platform-facing call that would have caught the original failure.
- Prefer explicit assertions over overly broad table-driven tests.
- Use tables when the cases share the same behavior and the table improves
  readability; keep one-off or highly distinct scenarios as separate tests with
  clear names.

Use this root-cause block format in regression tests when relevant:

```rust
// ROOT CAUSE:
//
// If <condition>, <unexpected behavior> happened because <reason>.
//
// Before the fix, <old behavior>.
// The fix keeps <new invariant or behavior>.
```

## Agent Skills

Repository skills live in `.agents/skills/`; `.claude/skills` links to it.
Use `use-buf` and `use-buf-plugins` when changing `proto/` or Buf configuration.

## Build and Test

```sh
make build
make check
make test
cargo xtask bench --help
```


## Documentation Workflow

- Write specs, plans and working notes during active work under
  `docs/ai/references/<responsibility>/` (see
  [`docs/ai/references/INDEX.md`](docs/ai/references/INDEX.md)).
- Do not create `docs/plans/`, `docs/superpowers/specs/` or
  `docs/superpowers/plans/`. If a skill suggests those locations, this guide
  overrides it.
- In-progress documents are the source of truth while the work is underway.
- When an implementation is mostly complete, fold the result into durable
  reference material in the owning folder and update its `INDEX.md`.
- Keep open questions and label provisional names in design docs.
- Keep `README.md` and the design notes current with the code. The README
  stays short; details live in `docs/ai/references/`.

### Documentation Placement

- `docs/ai/references/<responsibility>/`: durable design notes, plans, handoffs
  and evidence. Content should describe the current state or a clearly labeled
  historical decision.
- `docs/ai/explanations/`: tutorials, walkthroughs, diagrams and interactive
  explainers. Prefer English.
- `docs/notes/<owner>/`: personal or exploratory material. Do not commit it
  unless the user asks for that exact material.

## Private Data

- Do not put private data in the repository or on GitHub. This rule applies to
  code, tests, fixtures, docs, comments, commit messages, PR and issue text, and
  review replies.
- Private data is data about a real person, device, or network: names, email
  addresses, usernames, SSH host aliases, host names, domains, IP addresses,
  local paths, device names, tokens, and recorded content.
- If a value looks private, treat it as private. A host alias from
  `~/.ssh/config` or a path from the user's machine is private.
- Do not copy values from the user's configuration, logs, traces, or command
  output into a commit.
- In examples, tests and evidence notes, use reserved values: `example.com`,
  `user@example.com`, `192.0.2.0/24`, `198.51.100.0/24`, `/home/user` and
  `/Users/user`. Describe machines by role and hardware, for example "a Linux
  KVM host", not by name.
- Before you commit or push, search the diff and the commit message for private
  data.
- If private data is already on GitHub, stop and tell the user. Do not rewrite
  pushed history without approval.

## Commits

- Use `<type>(<scope>): <message>`, for example `feat(needy): ...`.
- Use `docs: <message>` for primarily documentation changes.
- Run formatting and tests before submitting changes that touch Rust code.
