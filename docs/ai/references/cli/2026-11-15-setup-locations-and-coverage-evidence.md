# Setup locations, workspace checks, and coverage

Date: 2026-11-15. Scope: setup-guide bug fix and the requested location labels
and client-launch choice. Runtime ownership and authorization are unchanged.

## Report and diagnosis

A run with a Linux container actor and a macOS native executor failed while
starting the workspace SFTP service. Its working directory did not exist on
the executor. The error's actor mount destination and executor source path
refer to different machines.

Source inspection shows that the guide included directory creation inside
executor installation. Its runner check tested only the OS. The guide did not
make an existing installation's project/account check a distinct step.
The report does not establish whether the directory was skipped, created under
a different account, or served by a different registered runner.

`config profile check --mode control --cmd ...` checks schema, placement,
driver/host facts, and predicted routing. It does not execute the supplied
command or inspect the executor's workspace. See
[`check_profile`](../../../../crates/daemon/src/check.rs) and the
[CLI's routing prediction](../../../../crates/cli/src/commands/profile.rs).
Do not treat successful prediction as successful mounting or execution.

## Guide changes

- Make executor workspace preparation a separate step, including identity,
  HOME, OS, directory permissions and the resolved project path.
- Probe those same facts through the actual serving runner before creating
  a run. Shell quoting preserves spaces, quotes and literal substitutions;
  `~/` expands on the executor, not on the control host.
- Add a launch-location choice for persistent remote executors. Launching on
  the executor includes a separate client invitation, pairing, context
  selection, local profile save, control check and interactive run creation.
  The invitation grants current run/process access without administrator
  access. It is not a new per-user authorization boundary.
- Show the client/control/actor/executor route and colored location labels.
  Reka UI tooltips open on hover and keyboard focus. Text labels remain
  visible; understanding the locations does not depend on color alone.
- State that `runner serve` remains required and uses its account's native
  permissions. Pairing a control client does not change that account.

An interactive actor started by a paired macOS CLI appears in that Mac
terminal. The driving bash is Linux bash in the actor; routed child commands
run on the registered macOS executor. An actor shell builtin stays in that
shell. The registered executor does not become the launching CLI process.

## Current ways to operate

Evidence level for this inventory: current source and command contracts. This
table does not claim a fresh installation was exercised on each platform.

| Operation | Profile location | Execution relationship | Evidence |
| --- | --- | --- | --- |
| Start on the control host | Invoking host | Actor on control host; native/container executor on that host | [run creation](../../../../crates/cli/src/commands/run/mod.rs), [placement](../../../../crates/daemon/src/schedule.rs) |
| Start on the control host with a remote executor | Invoking host | Actor on control host; selected persistent runner executes commands | [runner scenario](../../../../xtask/src/e2e/scenarios.rs), [Linux-to-Mac walkthrough](../../explanations/linux-actor-mac-executor.md) |
| Start from a paired client on the executor machine | Invoking client | CLI controls the remote run; independent persistent runner on that machine executes commands | [client access and run submission](../../../../README.md#one-command-from-your-laptop), [updated guide](../../../../apps/docs/src/setup/guide.ts) |
| Start from a third paired client | Invoking client | Same placement rules; client need not be an executor | [profile submission](../../../../crates/cli/src/commands/run/mod.rs) |
| Attach to an existing run | No local profile required | Terminal attaches; executor placement remains unchanged | [run attach](../../../../crates/cli/src/commands/run/mod.rs) |
| Run a process directly on a runner | No run profile required | `proc exec --runner NAME` targets that runner account | [process command](../../../../crates/cli/src/commands/proc/exec.rs) |
| Execute inside only the launching CLI's lifetime | Not implemented | Requires a run-scoped execution binding and admission contract | Current [run creation](../../../../crates/cli/src/commands/run/mod.rs) does not provide this binding. |
| Place an actor on an arbitrary remote runner | Not implemented | Current actors stay on the control host | [placement restrictions](../../../../crates/daemon/src/schedule.rs) |

Windows has low-level native execution paths and a harness scenario, but the
current guide rejects Windows enrollment. It does not provide a complete
Windows setup claim. See [guide validation](../../../../apps/docs/src/setup/guide.ts).

## What the current e2e actually tests

The [harness](../../../../xtask/src/e2e/mod.rs) selects cells, detects host
capabilities, prepares images when needed, and starts an isolated non-root
daemon per cell. It drives the CLI and checks exit codes, trace placement,
workspace/file behavior, terminal behavior and cleanup. Unavailable cells are
reported as not covered. See the [harness design](../architecture/2026-11-13-e2e-harness-design.md).

Important limits:

| Surface | Actual coverage | Missing evidence for the reported workflow |
| --- | --- | --- |
| PR CI | Rust tests and `e2e --only none/`; no actor in those two CLI cells | A green PR does not establish actor or workspace mounting behavior. |
| Actor/runner cells | Real actor drivers on equipped hosts; local second runner over direct/SSH/proxy transports; an optional Windows runner | A Linux actor with a macOS executor in a separate filesystem/account is not a dedicated matrix placement. |
| Workspace fixtures | Harness creates its own source directories before mounting | Fresh-user mistakes, missing paths and mismatched serving accounts are not proven by those happy paths. |
| Multi-host orchestration | Runs per-host matrices and can ship built artifacts | Running the matrix on Linux and on macOS separately is not the same as one run spanning both hosts. |
| Wizard tests | Profile/schema consistency and generated command assertions | Previously no executor-side shell execution test and no generated client-launch sequence. |
| Credentials and accounts | Isolated harness state and non-root daemon; platform/user values in actor fixtures | Different actor UID fixtures are not separate host users; no claim of cross-user authorization isolation. |

TODO: add an owner-approved cross-host onboarding slice to `cargo xtask e2e`:
separate client state, separate executor filesystem/home, generated-guide
commands, and a real interactive actor. Include missing workspace and wrong
account cases. This requires extending runner placement in the harness rather
than a hand-written remote e2e script.

TODO: exercise two unprivileged host users and an ungranted root executor once
the owner approves the execution-binding authorization contract. That contract
must define the creating client's identity, the executor account and the
run-scoped grants before this guide can claim cross-user isolation.

## Verification in this change

- Before the fix, `pnpm --dir apps/docs test src/setup/guide.test.ts` failed
  two new tests: missing standalone executor workspace checks and missing
  client authorization/launch steps. The other 42 tests passed.
- After the fix, frontend tests: 258 passed. The guide tests include an actual
  shell probe with distinct control/executor HOME directories. A missing
  directory fails; preparation makes the same probe succeed. Quoted project
  paths remain literal and do not execute command substitutions.
- Type checking and production build passed. The build retains the existing
  large-chunk advisory.
- Local production-preview browser checks passed: Linux/macOS route,
  client pairing and profile location, control-host launch alternative,
  hover/focus tooltip, profile clipboard, and Chinese/light mobile layout.
  Screenshots were inspected. This tests the page, not an authenticated
  cross-machine actor session.
- `cargo xtask e2e --only none/ --needy <isolated-build>/debug/needy
  --work-dir <temporary-directory>`: 2 passed, 0 failed, 0 not covered,
  under a non-root macOS user. No actor cell was selected.

The Rust build and browser tooling used temporary directories; the site build
used its ignored `dist/` directory. This review did not
install a user service, reinstall needy on the remote hosts, or create host
accounts. No deployment is asserted.
