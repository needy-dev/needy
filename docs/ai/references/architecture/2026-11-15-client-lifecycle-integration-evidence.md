# Client execution and lifecycle integration evidence

Date: 2026-11-15. Status: the approved Linux actor and macOS/Linux client acceptance matrix passed.
The final harness checkpoint is `05eea63`; production source is unchanged from `9e26e70`.
Both checkpoints are signed commits. This note does not establish released or general platform support.

The [sanitized acceptance record](2026-11-15-client-lifecycle-acceptance.json)
retains the cell IDs, check counts, source identity, and raw-report digests.
Private host inventories, credentials, paths, and raw output remain outside the repository.

A separate [host without KVM](2026-11-15-non-kvm-lifecycle-acceptance.json)
passed all five selected runc cells and 201 checks at `3738a3e`.
This is an additional environment record for existing cells, not five new scenario definitions.

The [approved contract](2026-11-15-client-execution-and-actor-lifecycle-spec.md)
sets the required behavior. The [holder evidence](2026-11-15-actor-holder-implementation-evidence.md)
and [client usage](../cli/2026-11-15-client-execution-usage.md) describe the implementation and its narrower evidence levels.

## Source inspection

The integration contains these behaviors. Source inspection alone does not prove their runtime results:

| Area | Implementation boundary | Source |
| --- | --- | --- |
| Client execution | Local profile resolution, `-f`, temporary overrides, and explicit `runner: {client: true}` selection. The automatic holder uses the invoking OS account. | [CLI creation](../../../../crates/cli/src/commands/run/mod.rs), [execution holder](../../../../crates/cli/src/commands/run/execution.rs) |
| Execution authority | A private capability binds one creation. Attach does not replace it, and shared runner selection cannot expose it. Existing client grants remain unchanged. | [binding store](../../../../crates/daemon/src/store/client_execution.rs), [pool](../../../../crates/daemon/src/pool.rs) |
| Process capacity | The process host owns `max_procs` permits until actual process exit. Viewer loss and control restart do not release those permits. | [capacity](../../../../crates/core-executor/src/process/capacity.rs), [service](../../../../crates/core-executor/src/service.rs) |
| Capacity protocol | `LimitedStart` is a distinct request variant. An older reader rejects it before execution instead of ignoring an added limit field. | [process protocol](../../../../proto/needy/v1/process.proto), [old-reader regression](../../../../crates/core-executor/src/service_test.rs) |
| Actor lifetime | A protected holder retains actor and native process ownership. Composition supplies its executable instead of using the embedding program as the holder. | [holder](../../../../crates/daemon/src/holder/mod.rs), [daemon composition](../../../../crates/daemon/src/lib.rs) |
| Executor errors | Pre-dispatch unavailability uses a bounded deadline and a confirmed-not-started result. Unknown dispatched outcomes remain distinct and never trigger automatic retry. | [relay](../../../../crates/daemon/src/relay/mod.rs), [guest proxy](../../../../crates/core-actor/src/guest/proxy.rs) |

Recovery first restores all surviving pool references. Only then does it activate services, watches, and pending stop cleanup.
An accepted stop retains its durable `stopping` state through another failed control startup.
Released or ended executor checkpoints override older snapshots, so recovery does not reacquire deleted resources.
Shared survivors retain their references. A release checkpoint does not prove successful mirror-out.
See [recovery](../../../../crates/daemon/src/run/recovery.rs) and its
[regressions](../../../../crates/daemon/src/recovery_test.rs).

File operations retain the owning run and the file origin through their executor requests.
Natural actor completion retains client-holder admission for owned mirror-out until the executor reference is released.
Terminal run state does not permit new user work.
The helper retains typed revocation causes so normal completion can finish without a transport-error result.
See [file requests](../../../../crates/daemon/src/files.rs),
[completion regression](../../../../crates/daemon/src/files_test.rs), and
[holder termination](../../../../crates/daemon/src/runner.rs).

## Trace and report boundary

New events write schema 11. `RunStop.ended` states whether the stop established the terminal run state, including recovered stop completion.
Cleanup after an already finished or failed run does not change its original result.

Readers accept schemas 10 through 11. The strict `StopV10` decoder preserves schema 10 semantics, with `ended` false.
Schema 10 reports remain readable. Earlier schemas remain rejected, and schema 11 requires the new field.
This is a versioned history boundary, not a general migration layer.
See the [event schema](../../../../crates/core/src/event.rs),
[event regressions](../../../../crates/core/src/event_test.rs), and
[report interpretation](../../../../crates/core/src/report/mod.rs).

## Completed automated checks

These are separate completed records from implementation and integration work. They overlap and must not be added into a final workspace total.
The combined workspace suite passed 1,088 tests across 33 reported suites. This total includes the overlapping records below.

| Record | Completed result | Evidence boundary |
| --- | --- | --- |
| Integrated workspace | 1,088 passed; full workspace Clippy passed with warnings denied | Current integrated source, including the review fixes. |
| Linux ARM guest and harness build | Static musl builds passed through the existing Dockerfile | Linux-specific compilation, not actor or cross-machine behavior. |
| Windows GNU cross-build | Existing Dockerfile target produced the executable | Compilation only; separate native results follow below. |
| Windows native MSVC at `9e26e70` | Build passed; 45 CLI tests passed; no warnings | Rust/Cargo 1.95.0, `x86_64-pc-windows-msvc`. Unix-only lifecycle binaries ran zero tests on Windows. |
| Final harness at `05eea63` | 31 tests passed; Clippy passed with warnings denied | Includes the failing-then-passing viewer-crash regression. Production source did not change. |
| Targeted core checks | 134 passed | Event and report behavior, including the versioned stop record. |
| Targeted daemon recovery checks | 2 passed | Focused recovery regressions, not real actor restart acceptance. |
| Capacity branch before integration | Full suite: 1,078 passed | Earlier branch checkpoint, not the final integrated source. |
| Holder implementation | Daemon suite: 248 passed | Domain tests with a stand-in actor and real process services. |
| File ownership changes | Daemon suite: 244 passed | Separate implementation checkpoint with owned file operations. |
| Protobuf tools | Buf 1.72 lint, format, generation, and breaking checks passed | Generation produced no further diff. |
| Documentation application at `433b4af` | 261 tests, typecheck, and build passed | Frontend behavior and build, not deployment or actor acceptance. |

The [process-service tests](../../../../crates/core-executor/src/service_test.rs)
cover capacity ownership after a lost acknowledgement, cancellation, changed policies, and old-reader refusal.
The [daemon file tests](../../../../crates/daemon/src/files_test.rs)
cover mirror-out before client-holder revocation.
The holder note records the separate recovery and service-definition checks.
Independent Standards and Spec reviews reported no remaining actionable findings after the review fixes.
These are source reviews; the live matrix below supplies separate runtime evidence.

## Live records

These records describe actual CLI runs or builds. A failed cell remains failed despite its successful individual checks.

| Checkpoint | Observation | Result and limit |
| --- | --- | --- |
| Integrated review patch | Native Linux ARM container, with the default non-root account, no privileged mode, and only the harness binary mounted read-only. | 2/2 cells and 39/39 checks passed: `none/exec` and `none/fs-cp`. Cleanup found no owned processes, mounts, or sockets. This does not prove actor or cross-machine behavior. |
| `066d503` | Linux Firecracker and runc routing baseline, with non-root control daemons. | 2/2 cells passed. This baseline does not prove client execution or restart survival. |
| `aa79f24` | macOS client execution through a Linux runc actor. | 42/43 checks passed, but the cell failed because harness cleanup stopped one invocation twice. Fix: `2b0ee25`. |
| `b671cc8` | The same cross-machine journey after lifecycle integration. | 41/42 checks passed. Gateway socket files remained after shutdown. Fix: `951e6f3`, with a failing regression followed by a passing regression. |
| `66ccbe4` | Final cross-machine acceptance attempt. | SSH signing failed before archive transfer. Zero of 12 planned cells ran, with zero behavior checks executed. |
| `aa79f24` Windows build | A native build completed with four Unix-import warnings. | Source guards changed at `b671cc8`. This earlier build does not prove current Windows runtime behavior. |
| `9e26e70` | Two macOS client journeys passed. Six of ten Linux-local cells passed; four lifecycle cells failed before their lifecycle assertions. | The harness waited for an unterminated prompt in line-oriented logs. Fix: `fef3a57` waits for the complete startup marker. |
| `fef3a57` | Both macOS client journeys and eight Linux-local cells passed. Two continuity cells failed after the viewer termination step. | The PTY library sent SIGHUP before killing the viewer; the CLI forwarded that deliberate signal to the actor. Fix: `05eea63` uses SIGKILL for the crash probe. |
| `05eea63` | Final canonical matrix: 12/12 cells and 486/486 checks passed. | Non-root control daemons; real Linux Firecracker and runc actors; macOS and Linux creating clients. No failed, skipped, or unavailable cell in this selected matrix. |
| `3738a3e`, separate host without `/dev/kvm` | 5/5 selected runc cells and 201/201 checks passed. | Non-root Linux creating client and control account. Client execution, continuity, actual actor loss, actor-file loss, and native executor-file loss passed. |

The macOS journeys observed the invoking account, HOME, platform, mapped cwd, mounted actor writes, and routed command writes and exits.
They also observed separate invocation holders, multiple logical executors, and continued execution after the other invocation stopped.
The final rerun includes the cleanup fixes and review patches.

The continuity cells retain the original driving PID, shell variable, and single start marker through repeated attach/detach and a SIGKILL of the viewer.
They then terminate control with SIGTERM and SIGKILL, confirm a new control instance, and reattach to the original actor resource and driving process.
The dispatched native process keeps its original PID and OS identity, and the trace records one dispatch across both control gaps.
Foreground Ctrl-C interrupts the child without deleting the invocation.
Explicit stop removes the original resources and ends the native process.

The two earlier failures were harness defects, each recorded before its correction.
The PTY HUP-sentinel regression fails with the old crash probe and passes with direct SIGKILL.
Production signal forwarding remains unchanged.

Raw records and private host inventories remain outside the repository. This note retains only role, OS, source, and outcome information.

## Verified matrix and limits

The final live matrix passed in full:

| Journey | Passed cells | Passed checks |
| --- | --- | --- |
| macOS client with Linux Firecracker and runc actors | 2 | 84 |
| Linux-local client execution with both drivers | 2 | 84 |
| Actor continuity and actual actor loss with both drivers | 4 | 112 |
| Actor-files and native executor-files runtime loss with both drivers | 4 | 206 |

The [matrix](../../../../xtask/src/e2e/matrix.rs),
[client journey](../../../../xtask/src/e2e/client/journey.rs), and
[lifecycle journey](../../../../xtask/src/e2e/lifecycle.rs) define those checks.
SSH authentication and signing resumed after the owner unlocked the existing agent.
No authentication or signing policy was disabled.
Every final cell checked for owned processes, containers, mounts, and sockets after cleanup; none remained.

The additional no-KVM host exposed no `/dev/kvm` and allowed FUSE access under the test account.
Its required static binaries and actor archive came from the canonical export, with matching SHA-256 digests on both hosts.
The first complete-export transfer timed out before tests started.
The retry delivered the required artifacts; transfer of the unused executor-container image was stopped.
All five selected cells use native executors and ran unchanged through the static harness with `--no-prepare`.
No selected cell was skipped, and no runtime privilege check was bypassed.
This record does not test a macOS creating client against that additional host or a container executor there.

This selected matrix does not test actual holder-process loss, a whole-host reboot,
or a native service-manager restart while an actor is running.
It does not establish macOS actor-driver or Windows executor interoperability support.

Automatic Windows client holders remain unsupported because canonical private-file ACL support is deliberately deferred.
A native Windows build cannot remove that boundary.

An abrupt control crash retains the last snapshot of dynamic chaos state.
Active gateway connections can reset during a control gap.
Actual holder, runtime, or host loss does not restore process memory.
End-only mirror-out does not guarantee durable output after abrupt loss.
Existing trace records and terminal replay do not restore a lost process.

Main remains clean. Implementation, harness fixes, and evidence are kept on the integration branch.
This slice made no new GitHub publication or deployment.
