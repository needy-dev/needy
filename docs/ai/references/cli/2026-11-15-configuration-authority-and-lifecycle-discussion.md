# Configuration authority and when changes take effect

Status: decision rationale aligned on 2026-11-15 with the approved
[client execution and actor lifecycle contract](../architecture/2026-11-15-client-execution-and-actor-lifecycle-spec.md).
The interview is complete for this slice. The contract is approved;
implementation and acceptance remain pending. “Current” below identifies
source observations, not behavior proven across all platforms.

## Accepted decisions

- The creating client edits and submits a complete profile. Each creation
  reads the latest local file, including local inheritance and recipe inputs.
- `needy run create -f PATH` is the approved alias for profile input. Current
  source accepts `needy run create --profile PATH`.
- Temporary actor-image, workspace, environment-entry and entry-command
  overrides affect only the submitted snapshot. They do not rewrite the file.
  Existing arguments after `--` retain their append meaning.
- Client execution is an explicit selection, distinct from a named runner.
  Missing `runner:` keeps its control-host meaning.
- The supplied client execution capability uses the invoking OS account and
  stays bound to that invocation. Later attach and context changes do not
  replace it. A same-account holder continues automatically after viewer exit.
- Detach, viewer loss and control restart preserve surviving actors. Explicit
  stop and actual runtime loss have separate, truthful outcomes.
- Authenticated clients retain daemon-wide visibility and existing grants.
  No human-account model, creator ACL or fine-grained RBAC is added.
- Mesh describes the application graph of actors, executors and their bindings.
  One run still records one actor invocation, not each child exec or viewer.

These decisions replace the earlier pending-decision lists. Environment and
secret policy, history formats, image preparation, pool sharing, RBAC and
migration redesign were explicitly deferred; keep current behavior in those
areas. They are not unanswered prerequisites for this contract.

## Full profiles remain client-controlled

The client can change actor image, entry program, environment, workspace,
routing and file mappings without logging into the actor. Editing affects a
new creation, not an already running program.

```text
Client reads profile, inheritance and recipe
  → applies explicit temporary overrides
  → validates the final profile before remote side effects
  → submits resolved text
  → daemon validates admission and resources, saves the run snapshot
  → actor and executor resources are created with accepted bindings
  → child commands follow that run's rules
```

The current client already performs local resolution and submits text. The
`-f` alias, temporary overrides and client-execution binding are approved
additions, not current command examples. Sources:
[client creation](../../../../crates/cli/src/commands/run/mod.rs),
[profile resolution](../../../../crates/config/src/profile/mod.rs) and
[daemon run creation](../../../../crates/daemon/src/run/mod.rs).

A requested profile does not grant host authority. Writing an execution
selector or numeric UID cannot make a different host account available. Keep
the current authentication and grants while implementing the accepted
execution binding; do not infer a new account or resource-permission system
from this distinction.

## Protocol identity and OS account

Needy authenticates a client, actor or runner. The destination process's
permissions come from the actual OS account and environment on its execution
machine. A remote numeric UID does not establish account equivalence.
“Same-account holder” means the invoking OS account, not a new Needy user.

The current access model is recorded here as source evidence:

| Role | Current scope | Boundary retained in this slice |
| --- | --- | --- |
| Local file editor | Edit local profile/context files and perform static checks. | Remote creation still needs accepted authentication; editing a context cannot grant server permissions. |
| Paired client with `runs` grant | Create, inspect and stop runs; process operations, logs, reports and live chaos operations. | Daemon-wide visibility, without a new creator-only ACL. |
| Paired client with `admin` grant | Run operations plus client invitations/revocation and runner registration management. | Management authority is distinct from an execution process's OS account. |
| Local operator | Control through the protected local management entrypoint. | Local admission checks owner/root; it is not anonymous remote access. |
| Actor | Requests associated with its run through the guest channel. | Does not gain run-management or registry-administration authority. |
| Runner credential | Heartbeats and its runner's control/task connections. | Runner enrollment alone does not grant run-management access. |

Sources: [authentication and local admission](../../../../crates/api-server/src/middleware/authentication.rs),
[run protocol](../../../../crates/api-server/src/protocol/grpc/v1/run.rs),
[process protocol](../../../../crates/api-server/src/protocol/grpc/v1/process.rs),
[client management](../../../../crates/api-server/src/protocol/grpc/v1/client.rs)
and [runner management](../../../../crates/api-server/src/protocol/grpc/v1/runner.rs).

`created_by` records who requested a run; it is not an owner ACL. Binding
correctness still prevents accidental use of another invocation's capability
or substitution of a privileged account. This does not claim multitenant
isolation or defense against a malicious admitted control administrator.

## Who configures what

| Object | Who edits or supplies it | When it is read | Result |
| --- | --- | --- | --- |
| Profile file | The creating client. | Each creation, on that client. | The daemon receives resolved content, not a requirement to access the client's file. |
| Connection context | The invoking client. | When a CLI connects. | Selects endpoint and credential; does not move a run or workspace. |
| `daemon.*` settings | An account able to configure the service host. | Service startup or explicit replacement. | Editing client-side settings does not remotely rewrite another host's daemon. |
| Persistent runner enrollment | An authorized invitation issuer and the execution host. | Join saves credentials; serve establishes the work connection. | The hosting service's OS account remains its execution account. |
| Client execution capability | The creating invocation's automatically managed holder. | Before actor work starts. | Explicitly selected logical executors use that capability without manual runner enrollment. |
| Existing run configuration | The daemon's accepted snapshot. | During the run. | Editing the original file does not hot-reload it. |

Sources: [global configuration schema](../../../../crates/config/src/global.rs),
[configuration contract](2026-11-14-cli-configuration-and-access-spec.md) and
[client-execution workflow](2026-11-14-client-bound-run-usage-discussion.md).

## Image, directory and environment scopes

The following are current schema fields, not newly invented YAML. They explain
where a value applies. Source paths must retain their actual execution side.

| Requested change | Current configuration entry | Scope and condition |
| --- | --- | --- |
| Actor image | `actor.image` | The actor driver must accept its ABI and image format; VM drivers need the prepared VM image. |
| Actor entry program | `actor.command` | New actor's driving program; current trailing creation args append to it. |
| Actor environment | `actor.env` | New driving program; does not imply copying the client's entire environment to executors. |
| Actor initial cwd | `actor.workspace.path` | Driving-program cwd, also used for workspace mounting and path mapping; there is no separate `actor.cwd`. |
| Workspace source | `actor.workspace.from` | `executor:NAME:PATH` is on that executor; `host:PATH` is on the control host. |
| Native executor default directory | `executors.NAME.workspace` | That executor's default cwd, subject to derived workspace values and matching rules. |
| Container executor image and defaults | `executors.NAME.container.image`, `.env`, `.workdir` | Container driver only; does not configure the native host's image or account. |
| Routed command cwd/environment | Rule `cwd` and `env.set/unset/clear/pass` | Commands matched by the rule chain, with path mapping. |
| Explicit new exec cwd/environment | `run exec` or `proc exec` options `--cwd`, `-e KEY=VALUE`, `--clear-env` | This new process only; does not alter an already running program. |
| Actor/container user | Profile `user.name/uid/gid` | Actor and container executor environment; does not change native host UID/GID. |
| Routing target | `commands`, `executors` and runner selectors | That run's scheduling and routing, subject to its accepted execution binding. |

Sources: [actor schema](../../../../crates/config/src/profile/actor.rs),
[executor schema](../../../../crates/config/src/profile/executor.rs),
[rule schema](../../../../crates/config/src/profile/rules.rs),
[explicit exec arguments](../../../../crates/cli/src/commands/proc/exec.rs)
and [native identity](../../../../crates/core-executor/src/driver/native.rs).

The accepted temporary overrides operate on the resolved profile for this
creation. Their exact additional flag spellings belong with implementation;
they do not replace complete-profile submission. No new current-directory
default or independent `actor.cwd` is introduced by this note.

Selecting an image does not transfer it. An image in one machine's local
engine does not automatically appear on the actor host. Keep the current
pull/build and VM-conversion requirements. Workspace failures must name the
real source and execution side so the operator can prepare the correct
machine.

NOTICE: image preparation, host resource preparation, environment inheritance,
secret transport and retention remain unchanged at the owner's request.
They require a separately approved slice before redesign.

## When changes take effect

| Event | Current behavior | Approved addition or retained boundary |
| --- | --- | --- |
| Edit a client profile | Changes the local file only. | Each later creation reads the latest contents. |
| Create a run | Resolve and submit profile/recipe; save a snapshot and create resources. | Add `-f`, explicit temporary overrides and client execution; validate before remote side effects. |
| Edit YAML after creation | Does not update the live run. | The accepted snapshot stays authoritative. |
| Start a new exec | Uses its arguments and the run's rule chain. | New cwd/env values do not mutate existing processes. |
| A running program changes cwd or environment | Affects that program and its subsequent work. | Does not edit daemon configuration or grants. |
| Replace actor image, entry or whole workspace | No live run-update API; create another run. | A replacement after loss is a new invocation, not process restoration. |
| Change live chaos | Dedicated add/remove operations record events. | Does not imply arbitrary profile hot reload. |
| Restart control | Current shutdown/reconciliation stops or fails runs. | The approved contract changes this to recovery of surviving work, with verified identity. |

Sources: [run API](../../../../proto/needy/v1/run.proto),
[run creation](../../../../crates/daemon/src/run/mod.rs),
[snapshot storage](../../../../crates/daemon/src/store/runs.rs) and
[current reconciliation](../../../../crates/daemon/src/reconcile.rs).

## Automatic behavior and explicit boundaries

Needy already handles client-side inheritance, workspace-derived mounts and
path maps, guest configuration and internal channels. The approved slice adds
automatic same-account execution holders and recovery of surviving work across
control restart. Users should not manage internal socket paths or guest
credentials for normal creation.

Automatic management cannot select another account, replace an established
executor with a new viewer or retry execution with an unknown outcome. Before
dispatch, an unavailable executor has a bounded wait; confirmed non-dispatch
returns 125. A dispatched request with unknown outcome returns 255. Execution
diagnostics use the logical executor name once known, while target stderr is
preserved. The [lifecycle design](../architecture/2026-11-15-actor-survival-and-history-design.md)
explains the dispatch and recovery invariants.

Common sources of confusion remain explicit:

- `user` configures actor/container identity; native identity comes from its
  hosting process.
- `host:` means the control host, not whichever computer invokes the CLI.
- `runner:` names a registered resource; the new client selection is explicit.
- Cwd exists at actor, executor, container, rule and individual-process scopes.
- `unknown` handles statically unresolved launcher commands, not authorization.
- A schema field describes a request, not a grant to a host account.

Sources: [workspace derivation](../../../../crates/config/src/profile/workspace.rs),
[guest configuration](../../../../crates/daemon/src/guest.rs) and
[workflow evidence boundaries](2026-11-14-client-bound-run-usage-discussion.md#required-acceptance).

NOTICE: independent cwd fields, pool-key/sharing changes, history
synchronization, protected-variable policy, resource scopes and migration
compatibility are deliberately omitted. Existing behavior stays; reopen only
when the owner names a separate slice. The approved contract's acceptance
requirements cover the current work without expanding into these deferrals.
