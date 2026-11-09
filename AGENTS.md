# Repository Guidelines

Needy separates a driving program (the actor) from the environments where its
work takes effect (executors). It has no agent-specific concepts.

## Project Structure

- `crates/needy`: the `needy` binary: CLI, daemon, executor, actor guest
  supervisor and proxy.
- `crates/config`: `needy-config`, profiles, recipes and routing rules.
- `crates/api-client`: `needy-api-client`, the Rust client for the daemon API.
- `crates/api-server`: `needy-api-server`, protocol adapters such as bearer
  authentication for serving the daemon API.
- `crates/api-proto`: `needy-api-proto`, generated Protobuf and gRPC types.
  Do not edit `src/gen`; change `proto/` and run `make proto`.
- `proto/`: canonical API schemas, linted and generated with Buf.
- `images/`: actor kernel, Firecracker patches and image builds.
- `docs/`: documentation. See [`docs/README.md`](docs/README.md).

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
