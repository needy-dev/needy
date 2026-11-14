# Actor holder and control recovery

Status: implementation evidence for the approved actor lifecycle slice.
Evidence level: automated domain tests with a stand-in Firecracker and a real native process host.
Real actor continuity across a separate control process restart requires the e2e harness evidence.

## Ownership boundary

The control daemon owns admission, authentication, scheduling, routing, bindings, and persisted run state.
A private local holder owns the actor driver handles and the native process host.
The holder starts automatically as the same OS account that starts control.
It accepts no management requests and makes no placement or routing decisions.

The holder keeps the actor-facing guest and gateway sockets at their original paths.
Replacement control binds separate backend sockets and restores the saved executor handles.
It reconnects to the original actor without calling the actor driver start operation again.
The actor keeps its driving program, terminal process service, and replay state.

Guest lifecycle streams reconnect after control returns.
Init retains the real driving-program exit until control acknowledges its idempotent exit report.
Control saves that result before the acknowledgement.
Host sensor schedules resume from the original actor start time.
A recorded sensor change does not run again.

The snapshot retains executor pool keys and handles, including private client binding keys.
Pool restoration retains key sharing and reference counts.
Container restoration connects to the saved process service and does not create a replacement container.

## Protected recovery

The runtime directory has mode `0700` and belongs to the holder account.
The holder descriptor, launch configuration, and invocation snapshot have mode `0600`.
The descriptor and lock must be regular files with one link.
Reads reject symlinks through `O_NOFOLLOW`.
The Unix peer UID, private credential, protocol version, and stable holder identity must agree.
A PID supplies diagnostic metadata only.

A live holder keeps an exclusive lock.
If its descriptor cannot authenticate, control fails startup and leaves retained work untouched.
A new holder receives a new identity, which cannot adopt an old invocation.
Existing cleanup probes verified leftover resources and retains failed or lost history.

Control shutdown closes management and its own tasks.
Explicit run stop closes native process admission before process cleanup, stops the actor, and releases its executor references.
Standalone exec calls retain their existing client-disconnect behavior.
Run-owned native processes survive viewer and control disconnection.

The holder keeps a control lease.
After the last control lease closes, an idle holder exits when no actor or native process remains alive.
Generated systemd units use `KillMode=process`.
Generated launchd definitions use `AbandonProcessGroup=true`.
These definitions let holder lifetime outlast control service lifetime.

## Automated evidence

The focused daemon tests cover these outcomes:

- The original native PID survives control restart
- The trace contains one original `actor.start` and no replacement start
- An attached native process survives the lost control stream
- Explicit stop ends the original process
- Trace sequence numbers continue after recovery
- An acknowledged driving-program exit survives control restart before actor exit
- A damaged holder credential fails closed and leaves the original process alive
- Runtime and snapshot permissions remain private
- An idle holder releases its socket after control disconnects

The service-definition test checks the generated manager directives.
The engine test closes a container wait connection, then reports the same container alive.
The wait probes that resource and obtains its later real exit code.
This prevents a runtime service disconnect from becoming proof of actor loss.

The in-process tests use an in-process holder at the process-launch boundary.
They do not prove survival across an OS process crash.
The production path launches a separate hidden holder process.

## Limits and remaining acceptance

Active exec and network streams can end during a control gap.
Their loss does not restart the actor or automatically replay a command.
The surviving program must handle the interrupted stream or connection.

Graceful control detach saves current dynamic faults and counters.
An abrupt control crash retains the last snapshot of this transient policy state.
Process continuity does not promise exact recovery of chaos counters.
If a crash follows a sensor write before its queued trace record, adoption can repeat the idempotent temperature write.

End-only mirror-out retains its existing abrupt-loss limits.
Stored traces and terminal replay do not restore a lost process.
Cross-machine real actor tests, actual holder loss, and native service-manager restart remain e2e acceptance requirements.

## Validation record

- `cargo test -p needy-daemon --lib`: 236 passed on macOS
- Service-definition and Docker wait-disconnect regressions passed
- `cargo fmt --all` and `git diff --check` passed
- Linux cross-check stopped before project compilation because `aarch64-linux-musl-gcc` was unavailable

This record does not count the unavailable cross-check as a passed check.
