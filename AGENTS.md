# Repository Guidelines

Needy is a headless runtime for running commands on another machine. It has no
agent-specific concepts: a workload is any command.

## Project Structure

- `crates/needy`: daemon, client, container runner and Linux proxy.
- `crates/exec-intercept`: Linux seccomp/ptrace supervision library.
- `images/`: runtime image build.
- `scripts/`: installer and integration test drivers.
- `docs/`: documentation. See [`docs/README.md`](docs/README.md).

## Build and Test

```sh
make build
make check
make test
make host-test
make docker-test
```

Linux kernel tests must run on Linux; macOS cannot exercise ptrace.

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
- Keep user-facing behavior in `README.md`, `docs/usage.md`, `docs/api.md` and
  `docs/verification.md` current with the code. The README stays short; move
  details into `docs/usage.md`.

### Documentation Placement

- `docs/ai/references/<responsibility>/`: durable design notes, plans, handoffs
  and evidence. Content should describe the current state or a clearly labeled
  historical decision.
- `docs/ai/explanations/`: tutorials, walkthroughs, diagrams and interactive
  explainers. Prefer English.
- `docs/notes/<owner>/`: personal or exploratory material. Do not commit it
  unless the user asks for that exact material.

## Commits

- Use `<type>(<scope>): <message>`, for example `feat(needy): ...`.
- Use `docs: <message>` for primarily documentation changes.
- Run formatting and tests before submitting changes that touch Rust code.
