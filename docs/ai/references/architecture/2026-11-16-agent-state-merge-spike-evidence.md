# Agent state in the actor, merged back at stop: spikes

Evidence for the [data placement design](2026-11-16-data-placement-design.md)
(pending). Recorded 2026-11-16. Hand-written profiles and throwaway merge
scripts, not product code and not a support claim. Follows
[agent state on live mounts](2026-11-16-agent-state-on-live-mounts-evidence.md),
which showed Codex's SQLite losing writes when two VMs share it over sshfs.

## Harness survey

The harnesses on the FrontierHarness leaderboard
([`eval-data.json`](https://frontierharness.org/leaderboard/eval-data.json),
generated 2026-08-22; versions in
[`harness-versions.json` @ `e837a70b`](https://github.com/frontier-harness-eval/eval/blob/e837a70bd6beb4e72eeeda62dd06e3bd34f6cb63/metadata/harness-versions.json)),
plus Devin, read from source or official docs at the revisions linked:

| Harness | Sessions | SQLite | Cross-process lock | Source |
| --- | --- | --- | --- | --- |
| Codex | one JSONL rollout per session | 7 databases; the session index is backfilled from rollouts, goals and memories only in SQLite | `File::try_lock` (`flock`) | [`rust-v0.167.2`](https://github.com/openai/codex/tree/rust-v0.167.2/codex-rs) `rollout/src/writer_lock.rs`, `state/src/runtime/backfill.rs` |
| Claude Code | one JSONL transcript per session; `history.jsonl`; `~/.claude.json` | none documented | `sessions/` marker files; superseded transcripts set aside | [docs](https://code.claude.com/docs/en/claude-directory) |
| pi | one JSONL file per session | none | `proper-lockfile` | [`v1.5.2`](https://github.com/earendil-works/pi/tree/v1.5.2/packages/coding-agent) `package.json` |
| oh-my-pi | one JSONL file per session; a second writer forks a sibling session | `history.db` (WAL): titles (backfilled by scans), recaps (only there) | Linux abstract socket, `flock` elsewhere | [`0730879`](https://github.com/can1357/oh-my-pi/blob/0730879325b611f75a3d1494b0d35c5b260f683b/docs/session.md) L557-L559 |
| Kimi Code | one directory per session, `session_index.jsonl` | none | temp file + `link(2)`, holder pid and hostname | [`6e35efa`](https://github.com/MoonshotAI/kimi-code/blob/6e35efa98402b2a5a198aa929ef178e79ab26bb4/packages/agent-core-v2/src/persistence/backends/node-fs/sessionLockService.ts) L166-L211 |
| DeepSeek Harness | JSONL logs (zstd) | FTS5 query index over the logs | `flock` beside each log | [`39163dd`](https://github.com/deepseek-ai/deepseek-harness/blob/39163dd30f703a79079e2d2446545cff3c453738/packages/session/session-persistence-jsonl/src/lease.ts) |
| opencode | **SQLite** `session`, `message`, `part` (event-sourced) | the only store, WAL, `OPENCODE_DB` moves it | SQLite | [`v1.19.5`](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/core/src/database/database.ts) L27, L44-L54 |
| Hermes | **SQLite** `state.db` sessions and messages | the only store, WAL | SQLite | [`e96221f`](https://github.com/NousResearch/hermes-agent/blob/e96221fa73d2dac7ba9f3bc5a8b866b3d0fc188e/hermes_state_common.py) L468-L539 |
| Devin | **SQLite** session history by default; experimental `DEVIN_SESSION_STORE=files` | the only store by default; `DEVIN_SESSIONS_DIR` moves it | per-process session lock | [changelog](https://docs.devin.ai/cli/changelog/stable) v3000.11.92, v3000.11.205 (closed source) |
| exo | event log owned by its service | `sqlx` (SQLite or Postgres) | the service orders writes | [`1eeab02`](https://github.com/exoharness/exo/tree/1eeab021a854dc8db502d82b9fe656cbaf044671) `Cargo.toml` |

What it means for a shared live mount: three harnesses keep sessions only in
SQLite, which loses writes across VMs; most locks are kernel-local and
exclude nothing across VMs. Kimi Code's lock records the hostname, which every
VM actor shared before the fix below.

## Fix: a hostname per actor

Before, a profile without a recipe hostname left every VM actor with its
kernel's `CONFIG_DEFAULT_HOSTNAME` (`sandbox-vm` on macos-vz, `(none)` on
Firecracker). `crates/daemon/src/guest.rs` now names such an actor
`needy-<run id>` (commit `bfc9601` on `fix/actor-hostname`); regression test
`actors_without_a_recipe_hostname_get_one_named_after_their_run` in
`crates/daemon/src/guest_test.rs`. Live: two concurrent macos-vz actors
reported `needy-3f9bd554bafd` and `needy-c5d651d47870`.

## Setup

As in the live-mount note: an Apple silicon Mac (macOS 27.2), OrbStack
29.8.2, needy from this repository with the hostname fix, `macos-vz`
actors from `needy-agents`, a native executor, scratch `NEEDY_*`
directories. Each run's agent home is local to its VM: copy-in from a
per-run seed (a copy of the canonical store taken before the run),
mirror-out to a per-run directory. After the runs, a merge script runs on
the Mac under `flock` on a lock file next to the store, one run at a time:

- files: three-way by content hash (seed, run, store); added and changed
  files are taken when the store still has the seed's version, otherwise
  the run's copy is kept beside it as a conflict; rules `ignore`, `append`
  (lines a run appended to an append-only file), `newest` (by mtime);
- SQLite (opencode): three-way by primary key per table, same rule per
  row, written by one connection to the store's database.

## Results

**Codex 0.167.2**, whole home local (SQLite included); rules: ignore SQLite,
logs, caches and locks; `newest` for `auth.json`, `models_cache.json`.

| Step | Result |
| --- | --- |
| s1, s2 at once from one seed | both exit 0; 67 and 65 files mirrored out |
| merge s1, then s2 | both rollouts added; `models_cache.json` newest; identical `skills/.system/` files from both runs skipped; **one conflict: `installation_id`**, a random ID each run generated because the seed had none |
| s3 from the merged store | `codex exec resume` of s1's and of s2's thread both answered from their context (`OK-s1`, `OK-s2`); its fresh `state_5.sqlite` backfilled 2 threads |
| merge s3 | both rollouts `updated` (resume appends to the existing file) |

**opencode 1.19.5**, data directory local; rules: ignore `opencode*.db*`
(merged by rows), logs, snapshots; `newest` for `auth.json`. Both of the
account's API keys answered "This API key has expired", so each run stored
a session with its prompt and an error reply and no model output.

| Step | Result |
| --- | --- |
| o1, o2 at once from one seed without a database | both stored a session |
| merge o1 | the store's database is o1's (none existed) |
| merge o2 | o2's `session`, 2 `message`, 1 `part`, 9 `event` and 1 `event_sequence` rows written; **39 conflicts: every `migration` row and the `project` row `global`**, which both runs created with their own timestamps |
| merged store | `integrity_check` ok; 2 sessions |
| o3 from the merged store | `opencode session list` showed both sessions; `opencode export` of each carried its own prompt |

**Claude Code 2.1.311**, `CLAUDE_CONFIG_DIR` local (it holds `.claude.json`
too), signed in with a `claude setup-token` OAuth token read from a password
manager into a private file on the Mac, copied in to a separate actor
directory, exported as `CLAUDE_CODE_OAUTH_TOKEN` by the driving program and
removed before it ended. Rules: ignore `shell-snapshots`, `session-env`,
`sessions`, `debug`, `backups`, `plugins` and caches; `newest` for
`.claude.json`, `policy-limits.json*`, `remote-settings.json*`.

| Step | Result |
| --- | --- |
| c1, c2 at once from one seed | `claude -p` answered `OK-c1` and `OK-c2`; one transcript each under `projects/-home-agent-workspace/` |
| merge c1, then c2 | both transcripts added; `.claude.json` and policy files by `newest`; no conflict |
| c3 from the merged store | `claude -p --resume` of c1's and of c2's session answered `OK-c1` and `OK-c2` |
| merge c3 | both transcripts `updated` |
| token | an exact-match search found it in no mirrored file, no file under the scratch needy state and data directories, and no log; the file was deleted afterwards |

The two runs' `.claude.json` differed in 11 of 1615 keys: identity set once
(`userID`, `machineID`, `firstStartTime`, different only because both runs
started from an empty store), cached feature flags, and plugin last-used
times.

Not reached: continuing an opencode session (expired keys).

## What it shows

1. The per-run model works for file-session harnesses with rules alone
   (Codex and Claude Code): concurrent sessions merge, and a later run
   resumes any of them.
2. A generic row merge works for opencode's SQLite: string IDs do not
   collide across runs, the merged database is consistent, and the harness
   lists both runs' sessions.
3. The rule vocabulary needs a fourth kind beside `ignore`, `append` and
   `newest`: **keep the store's** (Codex's `installation_id`, opencode's
   `migration` rows and shared `project` row), so identity and bookkeeping
   set once are never replaced and never reported as conflicts.
4. Two runs that continue the same session both change one rollout file, or
   one aggregate's `event_sequence` in opencode; the merge reports that as a
   conflict and keeps the run's copy aside, rather than interleaving them.
5. Rows keyed by `INTEGER PRIMARY KEY AUTOINCREMENT` would collide across
   runs; this spike's harness has none, so a harness that does needs an
   adapter that renumbers, not the generic row merge.
6. Whole-file `newest` is coarse for application state in one JSON file
   (`.claude.json`): it holds set-once identity beside caches. A key-level
   three-way JSON merge would keep identity and take caches; whole-file
   `newest` is enough once the store exists, since every seed then carries
   the same identity.

## Also observed

- In every cell that started two VMs together (three here, one in the
  live-mount note), vfkit's `tcpproxy` logged
  `error dialing "unix:…/gateway.sock": … bad file descriptor` for one
  connection, and the run still completed. Not investigated; a candidate
  for its own bug report.
- **`npm install -g` in an image `FROM` a shimmed image installs outside
  `PATH`.** In an image built on `needy-agents` (which ran
  `/.needy/shim-image`), `npm prefix -g` is `/.needy/real/usr/local`: `node`
  is a shim and npm takes its prefix from the real `node`'s path.
  `examples/agents` is not affected because it installs before its shim
  step. Worked around with `--prefix /usr/local`.
- **Detection that reads a program file sees the shim stub.** In the same
  image npm reported `libc: null` and refused
  `@anthropic-ai/claude-code-linux-arm64` (`EBADPLATFORM`): npm-install-checks
  8.0.0 (`lib/current-env.js` L17-L25) reads `/usr/bin/ldd` as text for
  "GNU C Library" or "musl", and `/usr/bin/ldd` is the shim stub (the real
  file, `/.needy/real/usr/bin/ldd`, has the string). `--libc glibc` did not
  change it; `--force` for the named package did. Any tool that reads,
  rather than runs, a program on `PATH` is affected the same way.

Both shim findings bear on the
[environments design](2026-11-16-environments-and-snapshots-design.md),
whose layers are built on top of each other.
