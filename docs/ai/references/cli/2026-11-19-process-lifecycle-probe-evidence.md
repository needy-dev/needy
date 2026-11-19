# Process lifecycle cleanup probes

Status: fixes implemented and regression coverage enabled; initial failing results
below are historical. See the fix validation record at the end.

Evidence level: automated CLI integration probes on a macOS Apple silicon
development host, with real control, runtime-holder, client-holder and CLI
processes, a native executor, and a stand-in Firecracker actor. These probes do
not boot a guest or establish a driver support claim.

Baseline checkpoint: `30e6b76` plus the initial opt-in reproduction tests in
`crates/cli/tests/cli/process_leaks.rs` and
`crates/cli/src/commands/run/execution/leak_test.rs`. The initial tracked source changes
only registered those test modules. Fixes and probes were run on 2026-11-19;
the fix validation record describes the implementation and enabled regressions.

## Initial observed results (before fixes)

Results across the focused invocations: 13 integration scenarios and two unit
scenarios; nine passed and six failed their desired cleanup/response invariant.
The initial reproductions were ignored by default. They are now enabled
regression tests, including the previously failing assertions.

| Scenario | Result | Observable evidence |
| --- | --- | --- |
| Drop fixture after native actor starts | FAIL | Runtime holder and actor remained after control and temporary recovery directory were removed; holder parent was PID 1. |
| Drop fixture after client-bound actor starts | FAIL | Runtime holder, client execution holder and actor remained; both holders had parent PID 1. |
| Timeout while waiting for daemon readiness | FAIL | After the 20-second readiness panic, the directly created control child was still alive. |
| Explicit stop, wait for invocation helpers, then drop fixture | PASS | Actor and client helper exited; runtime holder exited after control released its lease. |
| Idle control exit | PASS | Its runtime holder also exited without an admitted run. |
| Three create/stop cycles, with sequential repeated stop | PASS | Both stops succeeded in every cycle; no actor/client helper accumulated and exactly one runtime holder served control. |
| Graceful control exit and restart with a client-bound run | PASS | Original actor and client holder PIDs survived; recovered exec printed the expected output; actor start log contained one start. Final explicit stop cleaned them. |
| Stop, immediately terminate control, then restart | PASS | Restart completed cleanup without starting another actor; another stop succeeded. Recovery state was retained. |
| Unexpected actor termination | PASS | Actor and client helper exited; run history reported `failed`, as expected without a guest driving-program exit report. |
| Stop one of two client-bound invocations | PASS | Distinct client process-host PIDs were observed; stopping the first twice left the second able to execute on its original process host. |
| Rejected creation selecting an unavailable actor runner | PASS | CLI reported `runner.not_found`; client helper exited and final control teardown left no helper. This tests a submitted, failed create, not a never-submitted reservation. |
| Execution helper exits while interactive creating CLI remains alive | FAIL | Real `run create -it` remained in attach while its directly parented execution helper, terminated by SIGKILL, remained in `Z` for two seconds. Reproduced twice. |
| Two overlapping stops of the same run | FAIL response invariant; cleanup PASS | In each of three repeated cycles, one stop returned `operation.failed_precondition`: `a stopping run cannot become stopping`. Both requests completed and invocation helpers were reclaimed before asserting response failure. |
| Unit: kept holder's short-lived child exits while parent stays alive | FAIL | Child remained `ZN` after two seconds. |
| Unit: unkept holder is dropped | PASS | Child was stopped and reaped. |

The rejected-create cleanup completed in roughly 46 ms after the failed CLI
returned in the observed run. This is an observation, not a cleanup latency
guarantee. The probe allows the documented reservation expiry bound.

## Findings and boundaries

### Fixture cleanup does not own the full test lifecycle

Before the fix, `Daemon::drop` in `crates/cli/tests/cli.rs` killed and waited
for control only.
Holder survival across control loss is intentional: see
[runtime holder ownership](../../../TERMS_AND_CONCEPTS.md#runtime-holder) and
[actor holder evidence](../architecture/2026-11-15-actor-holder-implementation-evidence.md).
The fixture deletes recovery state without first stopping its admitted work.
The stand-in actor keeps polling its still-live holder. This establishes a test
teardown leak, not a defect in the intended control-restart contract.

Before the fix, `Daemon::start` constructed its `Daemon` guard only after readiness. A readiness
panic drops an ordinary `std::process::Child` without killing or waiting for it.
The opt-in reproduction owns an independent cleanup guard to avoid accumulating
its own failed-start children.

### Kept CLI execution helpers have no waiter

Before the fix, `Holder::keep()` in `crates/cli/src/commands/run/execution.rs` took and dropped its
`Child` without transferring wait ownership. `run create` calls it before
submitting creation and can then remain alive in interactive attach.

The integration reproduction uses a stand-in actor with no guest terminal to
keep attach waiting, verifies that the holder is the real CLI's direct child,
terminates that holder, and observes its process state while the CLI is alive.
The unit reproduction isolates the same ownership loss. A runtime holder has
its own waiter thread in `crates/daemon/src/holder/mod.rs`; these are separate
child-launch paths.

This evidence does not imply that every normal CLI invocation accumulates
zombies, or that zombies caused the historical shell slowdown. The parent must
still be alive when its child exits. Live polling actors and already-exited
zombies are different resource conditions.

### Sequential stop is accepted; overlapping stop is rejected

`Runs::stop` accepts terminal history after the live run has been removed.
`stop_run` instead calls `run.state.advance(Stopping)` for a live run and rejects
a caller that observes `Stopping`. This explains the response difference.

The overlap probe observed successful resource cleanup despite one failed
response in every repeated cycle. It establishes a concurrency boundary in
stop behavior, not a new leak or evidence of double resource destruction.
The implemented fix makes overlapping requests wait for the accepted cleanup.
The completion guard stays held through resource release, trace flush and
persistence; it also records completion for waiters holding the original run.
Cleanup runs on the daemon TaskTracker independently of the request future.
A caller disconnect therefore does not interrupt a half-completed release.
Two daemon regressions assert one stop trace event, waiting for cleanup, and
continued graceful completion after the original caller is cancelled.
A later `--now` joins the accepted stop and does not escalate its grace;
separate escalation coordination is intentionally deferred at the code site.

### Helper exit is asynchronous

`run stop` returning does not establish that every helper process has exited.
Client helpers can learn revoked admission through their control stream or
heartbeat. An otherwise idle runtime holder remains while control holds its
lease. The passing teardown waits for actor/client-helper exit with control
available, then closes control and observes runtime-holder exit.

## Reproduction commands

```sh
cargo test -p needy-cli --test cli process_leaks:: -- --nocapture --test-threads=1
cargo test -p needy-cli --bin needy commands::run::execution::leak_test:: -- --nocapture --test-threads=1
```

The integration scenarios were run in focused batches as tests were added;
the combined command above selects the resulting set. The rejected-create
probe permits up to 135 seconds for eventual helper cleanup. The initial investigation checked only this scope. The fix validation below
adds full-workspace and affected-target checks.

Final process inspection found zero needy executables from this checkout and
zero system zombies. Fixture cleanup is restricted to unique temporary paths;
the creating CLI and unit children also have direct wait ownership in probe
cleanup. `rustfmt` and `git diff --check` passed.

## Remaining evidence limits

- TODO: Real guest graceful exit, real hypervisor/engine loss, runtime-holder
  loss, and service-manager termination need cells in `cargo xtask e2e` on
  appropriate hosts. They are outside these local stand-in probes; use the
  [existing actor lifecycle evidence limits](../architecture/2026-11-15-actor-holder-implementation-evidence.md#limits-and-remaining-acceptance).
- TODO: Expiry after a CLI disappears before submitting CreateRun requires a
  separate transport-boundary fault probe. Rejected creation and lost replies
  after admission do not establish that earlier window. The reservation's
  intentional two-minute lifetime is described in
  [client execution usage](2026-11-15-client-execution-usage.md).
- These reproductions do not attribute every historical residual process to a
  specific failure path, establish arbitrary-duration resource trends, or
  demonstrate generic PID/socket collisions.


## Implemented fixes and validation

- CLI holder retention transfers `Child` to a waiter thread. It does not kill
  retained work when the creating CLI ends.
- The main CLI fixture owns its child before readiness, notices early exit,
  stops admitted runs and detached native processes, waits for client helper
  processes, and then ends/reaps control. Process observation tolerates a
  startup directory left by a helper killed with SIGKILL. Temporary state is
  retained until fixture cleanup completes.
- Custom control launch paths in the CLI suite now establish guards before
  waiting for readiness. Helpers returning raw children use a startup guard
  until ownership is transferred. Crash/restart tests explicitly terminate and
  reap control to bypass normal fixture cleanup and retain their test boundary.
- Execution and lifecycle fixture wrappers already guarded startup early;
  their wait calls, and the runner wrapper's wait call, now run even if kill
  reports failure. They were checked as adjacent child-ownership cases.
- Stops serialize on a per-run completion guard, run on daemon-owned tasks,
  and return success to retries without repeating completed cleanup. Recovery
  initializes the same guard for reconstructed runs.
- The 13 original CLI scenarios passed after the fixes. An additional regression
  covers fixture teardown of a detached host process outside any run.
- `cargo test --workspace --locked`: 1,135 passed, zero failed in the full run.
  Final fixture refinements and the additional detached-host regression are
  validated in the affected targets below.
- An embedded-control fixture in
  `crates/cli/tests/client_execution/lost_create_reply.rs` bypassed the main
  `Daemon` owner. Process inspection after otherwise-passing suite runs found
  three execution holders with deleted fixture homes. This test now keeps its
  transport serving until helper exit and uses the shared temporary-path
  cleanup guard for assertion failures. The guard is in `cli/processes.rs`.
  The three confirmed fixture remnants were terminated without force.
- Final `cargo test --workspace --locked`, after all fixes: **1,136 passed,
  zero failed, zero ignored** across 33 test groups (including doc tests).
- Final affected-target checks also passed: 134 CLI integration tests, six
  execution tests, four lifecycle tests, and nine daemon stop tests.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check` passed.
- Final process inspection after the last full suite: zero needy executables
  from this checkout and zero system zombies.

These fixes preserve the intended holder continuity across control restarts.
The real-actor evidence limits above still apply.
