# Repository Guidelines

Needy separates a driving program (the actor) from the environments where its
work takes effect (executors). It has no agent-specific concepts.

## Project Structure

Dependencies point downward; a crate never depends on one above it.

- `crates/cli` (`needy-cli`, binary `needy`): argument parsing, terminal and
  output formats, `build`, `profile check`, `bench`, `report` printing. It
  composes the crates below and, inside an actor, dispatches to the guest init
  and proxy by `argv[0]`.
- `crates/daemon` (`needy-daemon`): listeners and authentication for every
  role, and the control plane (sessions, the executor instance pool, the
  session, process and guest APIs). Composition only; logic lives below.
- `crates/core-actor` (`needy-core-actor`): the actor. `guest` is the in-guest
  init and exec-routing proxy (Linux only); `gateway` is the actor gateway.
- `crates/core-executor` (`needy-core-executor`): where commands run. The
  process host (PTY, pipes, replay, detach), the process service, and the exec
  client used by the CLI and the proxy.
- `crates/core` (`needy-core`): domain logic shared by everything: profiles,
  rules, recipes, chaos, traces and reports. No processes, listeners or VMs.
- `crates/common` (`needy-common`): shared helpers that depend on neither the
  platform nor an async runtime: globs, durations, filters, tar archives,
  exit status and signal names.
- `crates/api-proto`, `crates/api-client`, `crates/api-server`: generated
  Protobuf and gRPC types, the Rust client, and server-side adapters such as
  bearer authentication. Do not edit `api-proto/src/gen`; change `proto/` and
  run `make proto`.
- `proto/`: canonical API schemas, linted and generated with Buf.
- `images/`: actor kernel sources, the Firecracker patch and image builds.
- `docs/`: documentation. See [`docs/README.md`](docs/README.md).

## Shared Vocabulary

- Use [`docs/TERMS_AND_CONCEPTS.md`](docs/TERMS_AND_CONCEPTS.md) as the shared
  vocabulary for actors, executors, sessions, profiles, recipes, routing,
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

## Testing Practices

- Focused unit tests live next to the code they cover with `#[cfg(test)]`.
- The repository has Rust unit coverage for profiles, rules, recipes, chaos,
  archives, signals and bearer authentication. Daemon, guest and exec-stream
  behavior is so far verified end to end on hosts (see the closed-loop results).
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
