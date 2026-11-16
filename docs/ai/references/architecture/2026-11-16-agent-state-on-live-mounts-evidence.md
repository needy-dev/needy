# Agent state on live mounts: Codex in concurrent VM actors

Evidence for the [data placement design](2026-11-16-data-placement-design.md)
(pending). Recorded 2026-11-16. This is a probe of today's needy with
hand-written profiles, not a support claim.

## Question

Can several runs share one coding agent's home (Codex's `CODEX_HOME`) when
each run is its own VM and the home is live-mounted from the machine that
started the runs? In particular: what happens to Codex's SQLite state, do its
writer locks hold across VMs, and can a run continue a session another run
created?

## Setup

- Machine: an Apple silicon Mac (arm64, macOS 27.2), Docker through OrbStack
  29.8.2, vfkit and Apple's `container` installed.
- needy built from this repository at `0b11817` (`make build`, `make guest`),
  daemon run with `needy daemon serve` and private scratch `NEEDY_*`
  directories; no service installed.
- Actor: `macos-vz`, image `needy-agents` from `examples/agents/Dockerfile`
  on `ghcr.io/needy-dev/needy-actor:v0.0.6`, turned into a VM image with
  `needy image build --from needy-agents`. Codex 0.167.2 inside.
- Executor: `native` on the Mac.
- Codex's home: a scratch directory on the Mac holding only a copy of a
  ChatGPT-mode `auth.json`, mounted with today's `files` mapping
  (`mode: mount`, `from: executor:mac:…`, `to: actor:~/.codex`,
  `writable: true`), which `needy-daemon`'s `actor::mounts` serves over SFTP
  and the guest mounts with sshfs.
- Each run: `codex exec --skip-git-repo-check --sandbox read-only` with a
  one-line prompt, then a read-only `node:sqlite` query of `state_5.sqlite`
  (thread count, `journal_mode`, `quick_check`, `backfill_state`), and a
  `flock -n` probe on a file in the mounted home held for 20 s.
- No token refresh could happen: Codex 0.167.2 refreshes a ChatGPT token
  within 5 minutes of the access token's expiry or 8 days after
  `last_refresh` (`codex-rs/login/src/auth/manager.rs` L221-L222 and
  L3528-L3545 at
  [`rust-v0.167.2`](https://github.com/openai/codex/blob/rust-v0.167.2/codex-rs/login/src/auth/manager.rs#L3528-L3545)),
  and the token had days left. The copied `auth.json` had the same SHA-256
  after the runs.

Private data removed: account, host paths and thread IDs.

## Results

| Cell | Runs | SQLite | Result |
| --- | --- | --- | --- |
| A | 1 | on the mount (Codex's default) | Codex answered, exit 0. It made 7 databases on the mount (`state_5`, `logs_2`, `goals_1`, `memories_1`, `queue_1`, `thread_history_1`, ...), `state_5` in WAL mode. One error line: `failed to load models cache: EOF while parsing` |
| B | 2 at once | on the mount | Both answered, exit 0. **3 rollout files, 2 threads in `state_5`**: one run's thread is missing from the shared index while `quick_check` is `ok`; no error was printed. **Both runs acquired the same `flock` within a second of each other** |
| C | 2 at once | per run, `CODEX_SQLITE_HOME=/tmp/codex-sqlite` in the VM | Both answered, exit 0. Each fresh database backfilled every earlier rollout from the mount, **including the thread cell B lost**: 4 threads each (3 earlier + its own). Neither saw the other's concurrent thread. No SQLite file on the mount changed; `models_cache.json` on the mount did |
| D | 1, later | per run | `codex exec resume <thread of a C run>` in a new VM continued that session and answered from its context, exit 0; the fresh database had all 5 threads |

One unrelated line in cell C: vfkit's `tcpproxy` logged
`error dialing "unix:…/gateway.sock": … bad file descriptor` for one
incoming connection while two VMs started; Codex still completed. Not
investigated.

## What it shows

1. **Sharing Codex's SQLite across VMs through sshfs loses writes
   silently** (cell B). This matches Codex's own reports for NFS
   ([openai/codex#30957](https://github.com/openai/codex/issues/30957),
   [#44950](https://github.com/openai/codex/issues/44950)): WAL needs
   shared memory that two kernels do not share.
2. **`flock` on the mount excludes nothing across VMs** (cell B). Codex's
   thread writer and coordination locks are `File::try_lock`
   (`codex-rs/rollout/src/writer_lock.rs` at `rust-v0.167.2`), so two runs
   resuming one thread at once are not kept apart. Within one VM they still
   work.
3. **SQLite per run plus shared rollout files works for sessions** (cells C
   and D): `CODEX_SQLITE_HOME` moves every Codex database off the mount;
   Codex's rollout metadata backfill (`codex-rs/state/src/runtime/backfill.rs`)
   rebuilds the session index from the mounted rollouts at start; a session
   started in one VM can be resumed in another.
4. What cells C and D lose: state that lives only in SQLite (`goals`,
   `memories`, `queue`, `thread_history`, logs) is per run and gone when the
   VM ends; sessions started concurrently are not listed until a later run.
5. Plain files Codex writes from several runs at once (`models_cache.json`)
   are written without coordination; cell A's EOF error is consistent with
   reading it while another process writes it, but this was not isolated.

## Not checked

- Interactive Codex (the TUI), the app-server, or `codex resume` listing.
- Backfill time with a large home (thousands of rollouts).
- pi, opencode, herdr.
- `linux-firecracker` and the container drivers; the mount is the same
  sshfs path there.
- Two runs resuming the same thread at the same time.
