# Needy Reference Index

`docs/ai/references/` is organized by **responsibility folder**. Each folder has
its own `INDEX.md`.

## Naming

New notes:

```text
<responsibility-folder>/YYYY-MM-DD-<descriptive-slug>-<doc-type>.md
```

Use the date the note was first written. Do not put engineering slice codes in
folder names, filenames or navigation labels.

## Document types

| Type | Purpose |
| --- | --- |
| `design` | Accepted or pending feature or boundary design |
| `spec` | Approved or pending scope statement |
| `plan` | Implementation steps and dependency order |
| `handoff` | Completion handoff for a piece of work |
| `evidence` | Reproducible evidence, benchmarks or smoke records |
| `review` | Review, gate or verdict |
| `note` | Durable note without a stronger type |

## Responsibility folders

| Folder | Status | Responsibility |
| --- | --- | --- |
| [`architecture/`](architecture/INDEX.md) | Active | Cross-cutting design decisions, vocabulary and scope |

Add a folder only when a note does not fit an existing one, for example
`runtime/`, `exec-intercept/` or `pairing/`.
