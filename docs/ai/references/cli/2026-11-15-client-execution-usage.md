# Create from a local profile with client execution

Date: 2026-11-15. Scope: local profile submission, temporary overrides, and
execution as the creating client's OS account.

Evidence level: automated CLI and process-service tests, plus real non-root
Linux Firecracker/runc actor journeys with macOS and Linux creating clients.
The selected matrix passed 12 cells and 486 checks at `05eea63`.
See the [integration evidence and limits](../architecture/2026-11-15-client-lifecycle-integration-evidence.md)
and the [approved contract](../architecture/2026-11-15-client-execution-and-actor-lifecycle-spec.md).

## Locations and account

The client reads the profile, its inheritance chain, and its recipe files.
The control daemon receives their resolved text. Each creation reads the files
again. A connection context selects the control endpoint and credential.

An executor with `runner: {client: true}` uses an automatic execution holder
on the creating client's machine. The holder inherits the CLI's OS account.
The profile's `user.uid` and `user.gid` do not authorize a different native
account. No manual `runner serve` command is required for this selection.

The holder belongs to one invocation. Attach does not replace its account or
binding. Two invocations keep separate holders, including profiles with the
same executor pool key. Several logical executors in one invocation can use
its holder.

An omitted `runner` keeps its control-host meaning. A scalar such as
`runner: workstation` selects an explicitly enrolled persistent runner.
The scalar `runner: client` also names a persistent runner; it is not client
selection. Private holders do not appear in `runner ls` or shared selection.

## Profile on the creating client

This example places a Linux container actor on the control host. Its native
executor runs on the creating client, such as a Mac.

```yaml
actor:
  driver: linux-container
  container: { oci_runtime: runc }
  command: bash
  workspace:
    path: /workspace
    from: executor:workstation:~/project
    writable: true
executors:
  workstation:
    driver: native
    runner: { client: true }
    workspace: ~/project
commands:
  - { match: '**', to: workstation }
```

Prepare `~/project` on the client under the account that will invoke the CLI.
The workspace source belongs to that client. `/workspace` is its destination
inside the actor. A `host:` source continues to refer to the control host.
This feature does not copy client files to a control-host `host:` path.

Save the profile as `client.yaml` on the creating client. Select a configured
control context and create the run:

```sh
needy --context actor-host run create -it -f ./client.yaml
```

The existing actor image and host prerequisites still apply. Client execution
supports the native executor driver. Remote container creation remains outside
the existing driver support limits. Automatic Windows client holders fail
before reservation because private startup-file ACL support remains deferred.

## Temporary changes

`-f` is an alias for `--profile`. These flags affect only the submitted snapshot:

| Flag | Effect |
| --- | --- |
| `--image REF` | Replace `actor.image`. |
| `--workspace PATH` | Replace the workspace path inside the actor. |
| `--workspace-from SIDE` | Replace its `executor:NAME:PATH` or `host:PATH` source. |
| `--workspace-writable` | Allow writes to its source. |
| `--env KEY=VALUE` | Set an actor environment entry; repeat for several entries. |
| `--entry-command 'PROGRAM ARGS'` | Replace the driving program; shell quoting splits its arguments. |

Trailing arguments after `--` still append to the driving program.

```sh
needy --context actor-host run create -it -f ./client.yaml \
  --env MODE=trial --entry-command 'bash --noprofile' -- -l
```

Final profile validation precedes remote side effects. The CLI does not write
the source file. Relative host paths in overrides use the profile directory,
as existing profile paths do.

## Detach, stop, and uncertain replies

The default detach sequence is Ctrl-]. To return, use:

```sh
needy --context actor-host run attach RUN
```

Detach, viewer termination, and connection loss preserve the holder. A lost
create reply also preserves a holder whose invocation control already admitted.
An unused reservation expires after two minutes when control can check it.
A bound holder preserves processes through control disconnection and reconnects
with the same capability. Control recovery requires the separate actor lifecycle
implementation; history alone cannot restore a lost process.

Use `needy --context actor-host run stop RUN` to end the invocation. Use
`--now` to skip the actor's graceful stop and mirror-out. Run completion or
explicit stop invalidates holder admission and reclaims its local helper.
Missing client execution never selects the daemon's account or another client.

## Executor capacity

The process host owns each `max_procs` permit until the actual process exits.
Viewer loss and control restart do not free that permit. Attach does not take
another execution slot. Recovery retains the acquired instance's original limit
and wait/reject policy, including shared instances with different profile settings.

Limited commands use a distinct process-owner request. An older executor refuses
that request before execution instead of ignoring its limit. Upgrade executor
binaries and rebuild executor images together with the control binary.

Automated evidence covers [daemon recovery and viewer loss](../../../../crates/daemon/src/relay/mod_test.rs),
[wait/reject behavior](../../../../crates/daemon/src/pool_test.rs), and
[process-service admission, cancellation, and old-reader refusal](../../../../crates/core-executor/src/service_test.rs).

## Setup guide

The setup guide's executor-computer launch uses explicit client selection.
It pairs a control client, saves the profile there, and calls `run create -f`.
It does not enroll or serve a persistent runner for that path. Its static
profile check predicts routing; creation supplies the execution capability.
Control-host launches retain the named persistent-runner procedure.

The guide hides the unused runner name for client selection. It blocks Windows
automatic execution with the private startup-file ACL limitation. Location
labels identify the invoking CLI account in English and Chinese.

## Automated evidence

- [Setup guide tests](../../../../apps/docs/src/setup/guide.test.ts): client
  versus named selection, omitted enrollment steps, local profile creation,
  account labels, workspace quoting, and the Windows limitation.

- [CLI parsing](../../../../crates/cli/src/commands/run/run_test.rs): `-f`,
  temporary flags, and unchanged trailing argument parsing.
- [CLI journeys](../../../../crates/cli/tests/cli.rs): fresh inheritance,
  snapshot-only overrides, invoking UID, hidden holder registration, helper
  cleanup, and independent holders for the same group pool key.
- [Lost create reply](../../../../crates/cli/tests/client_execution/lost_create_reply.rs):
  the real daemon admits the run, the response fails, and the holder still
  executes through that run.
- [Daemon domain tests](../../../../crates/daemon/src/run/mod_test.rs): missing
  capability refusal and rejection of capability reuse.
- [Process-service tests](../../../../crates/core-executor/src/service_test.rs):
  process continuity after control leaves and rejection of late starts after
  invocation termination.
