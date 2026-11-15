---
name: create-pr
description: Prepare or update Needy pull requests with source-backed CLI, Protobuf/runner, and public API disclosures, before-and-after usage, and migration notes. Also use for retrospective API audits of specified PRs or revisions.
---

# Create PR

<!--
Source: https://github.com/moeru-ai/auv/blob/a5ef6130b19082a3d8d511415a5c1b4bb37832b3/.agents/skills/create-pr/SKILL.md#L1-L140
Reference: https://github.com/moeru-ai/auv/tree/a5ef6130b19082a3d8d511415a5c1b4bb37832b3/.agents/skills/create-pr
Adapted for Needy's layers, evidence levels, documentation conventions, and proportional disclosures.
The source is Apache-2.0 licensed; see LICENSE in this directory, copied from
https://github.com/moeru-ai/auv/blob/a5ef6130b19082a3d8d511415a5c1b4bb37832b3/LICENSE#L1-L201.
-->

Make the PR useful to reviewers and callers. Explain the final behavior, show
exact calling changes, and link the source that supports them. Follow AGENTS.md,
the shared vocabulary in `docs/TERMS_AND_CONCEPTS.md`, and any PR template.

## Establish the comparison

- Identify the repository, PR or branch, base, and head. Record resolved commit
  SHAs in the disclosure. Use the actual PR diff, not every change since `main`.
- For an open PR, compare its merge base with its head. For a merged or historical
  change, identify the reviewed or landed revision range and state which one you
  used. Read historical behavior at those revisions. A commit alone proves
  neither merge nor release; say "head revision" until that status is verified.
- For stacked PRs, separate changes introduced here from changes inherited from
  the base. State dependencies and merge order. Recheck the base after an upstream
  merge; separate PR URLs do not prove independence.
- Start with changed files, then inspect definitions, exports, and real callers
  at both revisions with `git show`, `git diff`, and targeted `rg` searches.
  PR prose and commit messages are context, not proof of an API contract.

## Inspect caller-visible changes

Trace changed contracts through the layers that expose them: CLI commands and
output, profiles and recipes, shared Rust types, actor and executor drivers,
daemon policy and runner behavior, `proto/`, and the `api-proto`, `api-client`,
and `api-server` crates. Include persisted state or trace/report readers when
their formats or historical interpretation change. Inspect only relevant layers.

Resolve these surfaces explicitly, including when unchanged:

- **CLI:** inspect command definitions and dispatch. Show exact commands before
  and after, with relevant flags, defaults, output, stdin, platform restrictions,
  and privilege requirements. A feature name is not a CLI disclosure. If this
  surface is unchanged, say **No CLI changes**.
- **Protobuf / runner API:** inspect relevant schemas, generated bindings,
  `api-client` calls, `api-server` adapters and domain traits, and daemon runner
  handlers. Show changed RPCs, messages, fields, enums, and runtime behavior,
  including field numbers and compatibility implications. If unchanged, say
  **No Protobuf or runner API changes**. CLI behavior alone does not imply a wire
  change. Generated bindings are evidence, not files to edit by hand.

Classify findings as added, removed, changed, or internal-only. Cover exported
functions, methods, types, fields, variants, and re-exports; accepted inputs,
defaults, return values, errors, and serialization; and identity, authorization,
resource reuse, or lifecycle changes that affect existing calls. Report removals
explicitly, including when none were found in the inspected public surfaces.

Check these distinctions before describing migration:

- A new path or re-export does not remove an old path that still exists.
- A `pub(crate)` helper rename is not a public API break.
- A new public type does not prove a method accepts it. Inspect parameter types
  and real callers; do not describe a planned API as implemented.
- Source compatibility, wire compatibility, and runtime behavior differ. A
  defaulted wire field can break Rust struct literals. An older peer can ignore
  a new field without implementing its behavior.
- An unchanged signature can still require migration when defaults, errors,
  return semantics, lifecycle, or supported platforms change.
- Parsing or compilation does not prove execution on an actor or executor.
  Distinguish a live probe from a supported capability.

## Write the disclosure

Lead with the concrete problem and resulting behavior. Use an English
Conventional Commit title with the relevant scope; primarily documentation
changes use `docs: ...`. Write clear, simple English unless the user requests
another language.

For caller-visible changes, include **CLI changes** and **Protobuf / runner API
changes** sections. Show verified before/after calls or wire shapes in a table:

| Surface / change | Before | After | Caller action |
|---|---|---|---|
| Changed command or API | Exact old call or shape | Exact new call or shape | Required edit or optional adoption |

Replace the example row with findings. Use `Not available` for a new entry point
and name replacements for removals. Include copyable commands for each changed
command family; a short inline command in the table is enough. Put longer code
examples below the table, with receiver types or setup if needed. Label partial
snippets and placeholders, and do not imply they were executed.

Add only migration details that matter:

- Distinguish required edits from optional adoption. Preserve platform-specific
  setup and meaningful behavior differences even when command syntax is unchanged.
- Name client, daemon, or runner version requirements only when supported by
  source. Do not invent released versions or compatibility negotiation.
- Link each group of claims to commit-pinned definitions, callers, tests, or
  durable contract notes. Include both revisions for removals and signature
  changes. Offline drafts may use revision/path references; use permalinks when
  publishing. Keep private data out of prose and examples.
- Name the evidence level and tested revision for capability claims. Separate
  source inspection, compilation, automated tests, live probes, harness e2e
  results, and pending CI. Give commands and outcomes, and link current evidence.
  Compilation is not behavior; a live probe is not a support claim.

Keep the body proportional. For a small docs-only or internal-only change, use a
short impact statement, **No CLI changes**, **No Protobuf or runner API changes**,
and relevant validation. Do not add empty tables or a list of untouched methods.
For a retrospective audit, use the historical range without creating or editing
a PR unless requested. Save a requested durable audit under the owning
`docs/ai/references/<responsibility>/` directory and update its index.

## Deliver within the requested scope

Compare the final diff with the disclosure. Replace obsolete rows when scope
changes, and verify explicit CLI and Protobuf/runner statements against source.
Use a structured body argument or `gh ... --body-file` to preserve code and
newlines. Read a published body back to confirm the intended text.

Follow existing user authorization for PR creation or updates. A draft or audit
request alone does not authorize publication, merging, or changes to other PRs.
When publication is already requested, prepare and publish without another
confirmation. Keep unrelated working-tree changes out of the PR. Concise results
and source links are enough; do not require committed raw logs, screenshots, or
temporary probe scripts unless the task asks for them.
