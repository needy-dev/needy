# Client execution and actor lifecycle

Status: approved feature contract synthesized from the owner discussion on
2026-11-15. Implementation and evidence are tracked separately. This document
supersedes unresolved questions in the four preceding discussion notes where
they conflict with the decisions below. It does not claim shipped support.

## Problem Statement

A person should be able to edit a profile on a laptop, create an actor on a
Linux host, and route the actor's commands back to that laptop. Today the
setup encourages fixed runner names and a separately started runner service.
That obscures which account starts a command, and can accidentally give a
client the authority of a privileged service. Changing a connection context
also makes the meaning of local paths and executor selection hard to follow.

The management daemon additionally owns resources whose lifetimes should
outlast a management connection. Its current shutdown and reconciliation
terminate actors instead of preserving them across a control restart.
Attachment, process lifetime, and historical records must have distinct
meanings.

## Solution

Keep one daemon coordinating multiple independent actor invocations and their
executor bindings. Submit a locally resolved profile for each creation, with
optional temporary command-line overrides. A profile can explicitly select
the creating client's execution host without enrolling a persistent shared
runner. Needy automatically keeps the required execution holder running as
the invoking OS account. Persistent runners remain useful for deliberately
provided remote execution resources.

Detach and connection loss end a viewer connection, not the actor. Separate
control management from the process holder so restarting control can recover
the same surviving work. Explicit stop or actual runtime loss still ends the
invocation. Keep truthful execution outcomes and existing persisted history;
do not imply that history can restore a lost process.

## User Stories

1. As a CLI operator, I want to create from a profile file on my computer, so that I can change configuration without copying files to an actor host.
2. As a CLI operator, I want each creation to read the latest file contents, so that edits take effect immediately.
3. As a CLI operator, I want local inheritance and recipe references resolved before submission, so that a remote daemon needs no access to my configuration files.
4. As a CLI operator, I want temporary image, workspace, environment, and entry-command overrides, so that one invocation can differ without rewriting my profile.
5. As a CLI operator, I want an explicit client executor selector, so that the same profile works from different computers without hardcoded runner names.
6. As a CLI operator, I want client execution to use my invoking OS account, so that a privileged runner cannot silently supply my execution authority.
7. As a CLI operator, I want client execution to start automatically, so that normal interactive use needs no manual runner serve command.
8. As a CLI operator, I want multiple logical executors in one profile, so that different commands can target different environments.
9. As a CLI operator, I want explicit named runner selection to keep working, so that existing remote execution arrangements remain useful.
10. As a CLI operator, I want missing client execution to fail clearly, so that it never falls back to the daemon's account.
11. As a CLI operator, I want changing my connection context to change only the control connection, so that it does not silently move execution or files.
12. As a terminal user, I want repeated attach and detach, so that I can leave and return to the same driving program.
13. As a terminal user, I want a CLI crash or network disconnect to preserve work, so that losing a viewer does not kill an actor.
14. As an operator, I want control restart to reconnect to surviving work, so that a management restart does not become an actor failure.
15. As an operator, I want explicit stop and kill to end the intended work, so that resources are reclaimed without affecting unrelated invocations.
16. As a terminal user, I want ordinary foreground signal behavior, so that interrupting a child command need not destroy an interactive shell.
17. As an actor program, I want temporary executor unavailability reported as an execution failure, so that I can handle it without the infrastructure destroying my actor.
18. As an actor program, I want infrastructure diagnostics prefixed with the logical executor name, so that I know which routing target failed.
19. As an operator, I want unknown execution outcomes preserved without automatic retry, so that reconnection cannot duplicate side effects.
20. As an operator, I want actual actor or host loss distinguished from control disconnection, so that status reflects what is known.
21. As an operator, I want historical records retained after an invocation ends, so that I can inspect its configuration and result through existing reports.
22. As a profile author, I want workspace errors to identify the real source and execution side, so that I can prepare the correct directory.
23. As an authenticated client, I want daemon-wide run visibility under existing grants, so that this change does not introduce a new human-account or creator-ACL model.
24. As an operator, I want documentation to identify where each command runs, so that client, actor host, and executor instructions are unambiguous.

## Implementation Decisions

### Domain and responsibility

- One run records one actor invocation and its bindings, not each routed exec
  or client connection. Multiple runs mean independent actor invocations.
- Control owns admission, scheduling, bindings, state, routing, and cleanup
  coordination. Actor drivers own local isolation. Executor services own
  local process and file operations. Logical separation need not introduce
  multiple manually managed services.
- Mesh means the application graph between actors and executors. Retain the
  existing outbound runner task transport. Needy does not implement a VPN,
  NAT traversal, or automatic tunnel provisioning.
- Keep the existing actor placement and executor driver support limits. This
  contract does not add arbitrary remote actor orchestration.

### Profile submission and authority

- Keep run create and add -f as an alias for its profile input. Resolve the
  file, inheritance, and recipe on the creating client and submit text.
- Provide explicit temporary overrides for actor image, actor workspace,
  environment entries, and entry command. Existing trailing arguments retain
  their append meaning. Validate the final profile before remote side effects.
  Overrides affect only the submitted snapshot and never write the source.
- Add an explicit client executor selection alongside existing named runner
  selection. An omitted runner retains its current control-host meaning.
  Client selection must not be implemented by guessing a registered name.
- Bind client execution to the creation's supplied execution capability, not
  to the last viewer or the daemon's local runner. Attach does not rebind it.
- A locally launched execution holder inherits the invoking OS account. A
  profile UID/GID is not proof of authority to become a host user. Never fall
  back to a root runner, daemon host, or another connected client.
- An execution holder may serve multiple logical client-selected executors
  within that invocation. Preserve existing executor instance and pool policy.
  Do not publish this capability as an unrestricted shared runner resource.
- Keep current client authentication and existing role grants. Admitted
  clients have daemon-wide visibility; do not add human users, per-creator
  isolation, scopes, or RBAC. Execution bindings still prevent accidental
  account substitution and cross-invocation capability reuse.
- A connection context chooses endpoint and credential. It does not change
  an existing binding, OS account, or workspace location.

### Lifetime and recovery

- Viewer detach, CLI termination, and connection failure leave the actor and
  its execution holder running. The holder does not require manual enrollment
  or runner serve. Work lifetime determines holder lifetime.
- Control shutdown and control crash must be recoverable without replacing a
  surviving actor or driving program. A local protected holder owns process,
  terminal, driver channels, and required per-run services across that gap.
  Recovery verifies stable identity and protected access; a PID alone is not
  sufficient evidence of ownership or liveness.
- Explicit stop ends the specified invocation and owned processes. Existing
  sharing and reference counts remain authoritative for shared resources.
  Entry-program exit records its real result. Foreground Ctrl-C follows PTY
  semantics; it is not an implicit whole-actor delete operation.
- Actual holder, actor runtime, or host loss may make the original work lost.
  Record failure or unknown outcomes truthfully. Restarting a runtime service
  is not by itself proof that every actor died: probe the actual resource.
- Do not automatically rerun commands or rebuild actors and call that recovery.
  A new actor after loss is a new invocation.
- Preserve existing trace/report storage. History and screen replay are not
  process restoration. Do not collect additional secrets or redesign history
  synchronization as part of this work.
- Actors remain disposable. Durable files belong on executors or configured
  persistent targets. End-only mirror-out offers no guarantee after abrupt
  loss; failed or unknown synchronization must not be reported as success.

### Executor failure contract

- Wait only before dispatch while the selected executor is disconnected,
  using a bounded configurable deadline. No command may execute late after
  returning a confirmed-not-started result.
- Confirmed not dispatched at deadline: proxy exits 125 and identifies the
  logical executor, unavailability, and that the command was not started.
- A dispatched request whose outcome cannot be determined exits 255 and
  reports outcome unknown. Absence of a Started event is not proof of no
  execution. Never retry such work automatically.
- A target's actual exit code or signal passes through. Preserve existing
  denial/start-failure behavior except where required for this distinction.
- Prefix Needy-generated execution diagnostics with the profile's logical
  executor name once known. Preserve target-program stderr byte for byte.
  Errors before a target is resolved may retain the product prefix.

## Testing Decisions

The owner selected non-root, cross-machine acceptance through the existing
e2e harness. Use that highest public CLI/API seam for the complete journeys;
use existing in-process daemon domain tests for focused regressions that are
cheaper and more deterministic. Test externally visible results, identities,
process continuity, and files, rather than helper structure.

- Add a regression before each behavior change at the relevant existing seam.
- Create with a local file, edit it, create again, and verify different
  snapshots. Verify overrides and inheritance without source-file changes.
- Run a Linux actor with a client executor on macOS and on Linux, and a
  Windows native executor where the existing toolchain supports it. Verify
  account, HOME, cwd, file operations, and signal/exit behavior.
- Contrast a privileged persistent service with a non-root creating client:
  client-bound work must report the client's account, never service root.
- Prove independent simultaneous invocations cannot accidentally exchange
  their client execution bindings; verify missing capability fails closed.
- Detach, kill the viewer, reconnect, restart control gracefully, and crash
  control. Compare stable actor/driving-program identity and a stateful
  command marker before and after; ensure no duplicate execution.
- Test actual actor loss, explicit stop, helper cleanup, historical outcome,
  and unavailable executor before versus after dispatch.
- Add cells or invariants to cargo xtask e2e; do not substitute hand-written
  remote acceptance scripts. Hosts unavailable or unsupported cells remain
  unverified, never counted as passed. Keep private host inventories outside
  the repository.
- Run focused tests during implementation, formatting and static checks,
  then the full Rust suite and affected documentation application checks.
  Review standards and spec coverage independently before delivery.

## Out of Scope

- New human users, scope/RBAC, per-creator isolation, or defenses against a
  malicious admitted control administrator.
- VPNs, NAT traversal, automatic external tunnels, direct-path optimization,
  and arbitrary remote actor placement.
- Multi-actor runs, new grouping objects, executor sharing semantics, pool
  key redesign, or cross-actor state redesign.
- Environment inheritance, secret transport, configuration retention, image
  pull/build policy, and host resource preparation policy redesign. Keep the
  current behavior except the explicit per-create overrides above.
- Atuin-style history synchronization, search UI, automatic replay, and
  restoration of process memory after host/runtime loss.
- Compatibility layers for old profiles or old intermediate implementations.
  Update the contract directly and document the new usage.

## Further Notes

The earlier discussion notes provide rationale and source observations. Align
them in English with this contract and remove obsolete requests for decisions.
Update the shared glossary and documentation indexes together. Implementation
notes must distinguish completed behavior, automated evidence, live probes,
and remaining gaps; a build alone is not evidence of process continuity.

The lifetime reference is the separation of tmux clients from its server and
herdr-core's holder from management, rather than layout-only restoration.
Atuin informs historical facts, not replay guarantees. Their pinned references
remain in the accompanying lifecycle design note.
