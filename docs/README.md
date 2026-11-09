# Needy Documentation

Needy docs are split by **purpose**. Reference notes are further split by
**responsibility folder**, and each folder has an index. The project contract
for contributors and agents is [`AGENTS.md`](../AGENTS.md).

## Layout

```text
docs/
├── README.md                 ← you are here
├── TERMS_AND_CONCEPTS.md     ← shared vocabulary
├── ai/
│   ├── references/           ← durable design / plan / evidence notes
│   │   ├── INDEX.md          ← responsibility-folder index
│   │   └── <responsibility>/ ← e.g. architecture/, cli/
│   │       └── INDEX.md
│   └── explanations/         ← tutorials, walkthroughs, interactive HTML
└── notes/<owner>/            ← personal drafts (do not commit by default)
```

## Where to put what

| Content | Location |
| --- | --- |
| Design, plan, handoff or evidence | `docs/ai/references/<responsibility>/YYYY-MM-DD-<slug>-<type>.md` |
| Tutorial, walkthrough or interactive demo | `docs/ai/explanations/` |
| Personal exploration or local logs | `docs/notes/<owner>/` (commit only if the owner asks) |

After adding a reference, add one line to the folder's `INDEX.md` and, for a
new folder, one row to [`ai/references/INDEX.md`](ai/references/INDEX.md).

