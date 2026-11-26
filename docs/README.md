# Needy Documentation

Needy docs are split by **purpose**. Reference notes are further split by
**responsibility folder**, and each folder has an index. The project contract
for contributors and agents is [`AGENTS.md`](../AGENTS.md).

## Layout

```text
docs/
├── README.md                 ← you are here
├── TERMS_AND_CONCEPTS.md     ← shared vocabulary
├── guides/                   ← user and contributor guides
├── ai/
│   ├── references/           ← durable design / plan / evidence notes
│   │   ├── INDEX.md          ← responsibility-folder index
│   │   └── <responsibility>/ ← e.g. architecture/, cli/
│   │       └── INDEX.md
│   └── explanations/         ← tutorials, walkthroughs, interactive HTML
└── notes/<owner>/            ← personal drafts (do not commit by default)
```

## Guides

- [Installation and prerequisites](guides/installation.md): install script,
  Homebrew, `proto`, Windows, source builds, daemon service management, and
  `linux-container` FUSE/SSHFS setup.
- [Configuration reference](guides/configuration.md): command rules, workspace
  resolution, file and sync mappings, network gateway, chaos faults, executors,
  image definitions, recipes, `builtin:` catalog, and environment variables.
- [Agent images](guides/agent-images.md): building an image `FROM`
  `actor-base` with Docker, BuildKit, `devcontainer build` or Shipwright, and
  what a VM actor imports, checks and ignores.
- [Multi-machine and remote runner setup](guides/multi-machine.md): enrolling
  executor and remote Firecracker actor runners, tunnels, static/TTL credentials,
  and daemon security boundaries.
- [Deployment](guides/deployment.md): choosing a topology (single host,
  remote runners, Kubernetes, Windows), starting each role, credentials,
  where the gateway runs and SOCKS upstreams, images, upgrades and mixed
  versions, operations, known gaps and the evidence behind each claim.
- [Development and architecture](guides/development.md): crate layout,
  extension points, `cargo xtask e2e`, and `apps/docs` banner rendering.

## Tutorials

- [Linux bash or OpenCode with Mac tools](ai/explanations/linux-actor-mac-executor.md):
  one walkthrough, with a machine and terminal for every step.
- [Complete Linux-to-Mac profiles](../examples/linux-mac): the website uses these same files.

## Where to put what

| Content | Location |
| --- | --- |
| User or contributor guide | `docs/guides/<topic>.md` |
| Design, plan, handoff or evidence | `docs/ai/references/<responsibility>/YYYY-MM-DD-<slug>-<type>.md` |
| Tutorial, walkthrough or interactive demo | `docs/ai/explanations/` |
| Personal exploration or local logs | `docs/notes/<owner>/` (commit only if the owner asks) |

After adding a reference, add one line to the folder's `INDEX.md` and, for a
new folder, one row to [`ai/references/INDEX.md`](ai/references/INDEX.md).
