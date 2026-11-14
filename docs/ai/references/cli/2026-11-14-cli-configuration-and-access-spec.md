# CLI configuration, access, lifecycle, and diagnostics

Status: approved implementation contract, 2026-11-14; implementation and acceptance are in progress.
Owner amendment, 2026-11-14: no legacy CLI or credential-import compatibility is required.
Old credential files remain untouched, old environment selectors do not grant
access, and unspecified access grants are rejected. Database schema upgrades
remain necessary to preserve stored state.

The command tree and access changes are present in the current worktree.
The [lifecycle evidence](2026-11-14-cli-lifecycle-evidence.md) records the tested
local discovery, shutdown and macOS user-service slice and its remaining limits.
The source inventory below describes commit `cfc6f8e9a09cf3e5264887d51679a72743f5db0a`.
Historical observations and design reviews retain their own dates and evidence limits.

Navigation: [Problem](#problem-statement), [commands and behavior](#solution),
[user stories](#user-stories), [implementation/API](#implementation-decisions),
[testing](#testing-decisions), [scope](#out-of-scope), and
[inventory, sources, history, reviews](#further-notes).

This document consolidates six uncommitted onboarding, command-surface, access,
service, and diagnostic notes. It follows the `to-spec` structure. It remains a
local repository document; publishing an issue is outside this request.

## Problem Statement

An operator cannot reliably tell which control plane a command will reach or
which credential it will present. Starting a TCP listener can remove the
expected local socket. Pairing changes the default target. A saved client
credential can then prevent an administrator from removing a runner on the
control host itself.

Configuration management is split across environment variables, stored client
tokens, profile files, and command-specific defaults. Named profiles can be
loaded but cannot be listed. Client pairing and server-side authorization share
one command group. Run operations and process operations have similar names
but different targets. Background daemons require manual process management.

Many failures end with an error chain. The output often omits the selection
source, verified caller, execution machine, and next useful check. Some typed
causes are flattened before they reach the CLI. A formatting-only change cannot
recover those facts.

The desired result is a complete CLI contract: one local configuration entry,
automatic local access, explicit remote selection, managed services, preserved
execution behavior, and diagnostics that lead to a valid next action.

## Solution

### Product rules

1. Local control administration works through a private management socket.
   It needs no pairing or manually supplied bearer.
2. A connection record contains transport and credential information.
   Local configuration management owns these records and named task profiles.
3. Pairing saves a connection. Only an explicit selection changes the default.
4. Each remote client has its own revocable credential and server-assigned access.
   A runner has a separate credential for runner protocols.
5. Discovery locates the local management entry. It never scans remote ports,
   grants authority, or selects another control plane after failure.
6. System service management owns background processes. Foreground serving
   remains available for development, containers, and external supervisors.
7. Run, process, runner, configuration, and service operations name their objects.
   A profile remains a task definition, not a connection record.
8. Errors preserve known causes and provide applicable checks. Unknown causes
   remain unknown. Commands never retry a possibly completed execution silently.

### Scope and platform matrix

| Capability | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Existing control daemon | Unix control path; driver requirements still apply | Unix control path; actor drivers require Apple silicon where specified | Rejected by current code |
| Existing native runner | Implemented; specific behavior has e2e evidence | Implemented; specific behavior has e2e evidence | Implemented; specific behavior has e2e evidence |
| Target control service management | systemd adapter, to implement and test | launchd adapter, to implement and test | Deferred; fail before installation writes |
| Target local management discovery | Private Unix socket and OS admission | Private Unix socket and OS admission | No Windows control-host discovery in this slice |
| Target client configuration and remote use | Same user-facing contract | Same user-facing contract | Preserve remote client/runner behavior; Windows ACL work needs its own acceptance |

This matrix describes existing roles and proposed work separately. It is not a
new platform support claim. A Linux service host needs a supported systemd
manager; a Linux distribution without one is not covered by that adapter.
Windows messages name the unsupported Needy role. They do not claim that
Windows itself lacks container support. Use “not supported yet” or “planned.”
Use “implementation in progress” only after implementation work starts.

### Canonical command tree

All entries below describe the target interface. There are eight top-level
command groups. Do not retain parallel top-level `context`, `ctx`, `conn`,
`pair`, or `profile` interfaces. No compatibility aliases are retained.

```text
needy
├── config                              # This client's persistent configuration
│   ├── ls                              # Configuration/profile/connection overview
│   ├── show                            # Global configuration, credentials redacted
│   ├── get KEY
│   ├── set KEY VALUE
│   ├── unset KEY
│   ├── paths                           # Resolved config/state/data/runtime paths
│   ├── use NAME                        # Persist the selected connection only
│   ├── context                         # Saved control-plane connections/identities
│   │   ├── ls
│   │   ├── show NAME
│   │   ├── pair NAME [-- PROGRAM ARG…] # Read an invitation code; save credentials
│   │   ├── set NAME                    # Change one saved transport explicitly
│   │   ├── check [NAME]                # Bounded read-only connection/access checks
│   │   └── rm NAME                     # Local removal, not server-side revocation
│   └── profile                         # Local named YAML or an explicit file
│       ├── ls
│       ├── show PROFILE
│       └── check PROFILE               # Static by default; runtime mode is explicit
├── daemon                              # This machine's service, never a remote context
│   ├── serve                           # Foreground control daemon
│   ├── executor                        # Existing standalone executor role
│   └── service
│       ├── install
│       ├── uninstall
│       ├── start
│       ├── stop
│       ├── restart
│       ├── status
│       └── logs
├── client                              # Authorization on the selected control plane
│   ├── invite                          # One-use code; optional management access
│   ├── ls
│   └── revoke CLIENT_ID
├── runner                              # Registration/placement machines
│   ├── invite                          # Time-limited runner join code
│   ├── join NAME [-- PROGRAM ARG…]     # This machine joins once; save runner state
│   ├── serve                           # This machine reconnects with its credential
│   ├── ls
│   ├── rm NAME                         # Revoke runner identity; retain removed record
│   ├── rotate NAME
│   └── token
│       ├── ls                          # Join-code metadata, never its secret again
│       └── revoke ID
├── run                                 # Actor + bound executors + routing
│   ├── create
│   ├── ls
│   ├── stop [RUN]
│   ├── attach [RUN]                    # Driving program terminal, not an exec PID
│   ├── logs [RUN]                      # Actor console
│   ├── exec [RUN] -- COMMAND ARG…      # Route inside this run
│   ├── report [RUN]
│   └── chaos
│       ├── add NAME
│       ├── ls
│       └── rm NAME
├── proc                                # Single executor processes
│   ├── exec -- COMMAND ARG…            # Outside any run; explicit host/runner/pooled-executor target
│   ├── ls
│   ├── attach PROCESS_ID
│   └── kill PROCESS_ID
├── fs
│   └── cp SOURCE DESTINATION           # Explicit run/runner/control-host selection
└── image                               # Local image stores, not remote resources
    ├── pull
    ├── ls
    ├── build
    └── rm IMAGE…
```

`proc` is the proposed canonical short name for process operations. This spelling
is part of the proposed contract, not an implemented rename. Built-in recipes
remain accessible through profile references. A recipe management command is
not added in this slice.

### Shared argument contracts

Brackets indicate optional arguments in this specification. `DURATION` uses the
existing duration parser. `RUN` accepts a full ID, unique prefix, name, or
`latest`, except offline report, whose restricted selectors are defined below.
A command argument after `--` is passed without shell re-parsing.

| Group | Arguments and behavior |
| --- | --- |
| Global | `--config FILE` selects the client configuration file; `-v`, `-vv`, `-vvv` retain current tracing levels; `-h/--help` and `--version` retain normal CLI behavior |
| Control selection | `--context NAME` selects one saved connection for this invocation; `--address URI` selects a direct address instead; the two conflict |
| Direct credential | `--credential-file FILE` is permitted only with a direct address. Read a private bearer file; do not put a secret in argv. Do not borrow a saved context's credential for a different address |
| Diagnostic rendering | `--diagnostic-format human|json`, default `human`, selects infrastructure diagnostics on stderr. It never changes raw child stdout or the child's exit result |
| Data output, abbreviated D below | `-o/--output table|json`, default `table`; valid only for the commands listed with D |
| Document output, abbreviated Y | `-o/--output yaml|json`, default `yaml`; secrets remain redacted |
| Exec output, abbreviated E | `-o/--output human|json|ndjson`, default `human`; preserve the existing exec schemas until an explicit versioned migration |
| Exec process arguments, abbreviated X | `-t` force PTY, `-T` force pipes, `-n` no stdin, `-d` detach, `--detach-keys KEYS`, `--cwd PATH`, repeatable `-e/--env KEY=VALUE`, `--clear-env`, E |
| Attach arguments, abbreviated A | `--no-replay`, `--read-only`, `--detach-keys KEYS`; read-only sends no input, resize, or signals |
| Bind addresses | `tcp://HOST:PORT` and `unix:///absolute/path` for local listeners. Remote client addresses also accept HTTPS with normal certificate validation. A TCP URI does not imply TLS |
| Connector transport | Mutually exclusive with `--address`; trailing `-- PROGRAM ARG…` is an argv vector. Its stdout is protocol data, not log output |

Control-selection options are accepted only by operations that use a control
plane, explicit connection checks, or profile control checks. Explicit remote
selection on local service/image/config mutation commands is an error. It is
not silently ignored. Persisted remote selection does not change a local
operation's scope. `--config` does not relocate state/data/runtime implicitly.

The actor-driver values are `linux-firecracker`, `linux-container`, `macos-vz`,
and `macos-apple-container`. Runner-role values are `actor` and `executor`.
The shared service `--scope` accepts `user|system` on every service verb.
Detach keys default to `ctrl-]`; an empty value disables terminal detaching.
Listing output uses `table|json`, not the exec/report `human` enum.

The proposed startup wait is 30 seconds. The proposed service-stop budget is
60 seconds. The proposed context-check deadline is 5 seconds. These are product
policy defaults, not measured support limits. Each deadline belongs to its own
operation and needs e2e validation; do not implement one shared timeout constant.

### Configuration command parameters

| Command | Parameters | Behavior and result |
| --- | --- | --- |
| `config ls` | D | List local global configuration, named connections, and named profiles with kind and source. No daemon required |
| `config show` | Y | Show the global document with credential values redacted |
| `config get` | Required `KEY`; Y | Read a schema-defined global key. No opaque path guessing into profile files |
| `config set` | Required `KEY VALUE` | Parse VALUE as the key's typed YAML value, validate the complete candidate, then write atomically. Refuse credential values on argv and current-context mutation through this generic path |
| `config unset` | Required `KEY` | Restore the schema default or remove an optional value; refuse required/managed fields |
| `config paths` | D | Display resolved configuration/profile/state/data/runtime paths and override sources |
| `config use` | Required `NAME` | Select an existing connection or built-in `local`. Do not contact the server or change a default run/profile |
| `config context ls` | D | Show local aliases, current selection, sanitized transport, and recorded identity. Recorded identity is not a live permission claim |
| `config context show` | Required `NAME`; Y | Show one connection with a redacted credential; no `--show-token` shortcut |
| `config context pair` | Required `NAME`; exactly one `--address URI` or connector argv; optional `--code-file FILE`; `--replace` for an existing local name | Read code from hidden input by default. Pair once, validate returned identity/access, and save. Preserve the current selection. Refuse `--replace` without an existing record |
| `config context set` | Required `NAME`; exactly one new address or connector argv | Change transport only; preserve its identity record. No implicit re-pair, permission upgrade, or switch |
| `config context check` | Optional `NAME`, default current selection; `--timeout DURATION` default 5s; D | Report config source, sanitized transport, health/instance result, and authenticated caller where supported. Mark unavailable checks unknown; do not create runs or repair state |
| `config context rm` | Required `NAME` | Remove the local record only. Require an explicit switch before removing the current connection. Built-in `local` cannot be removed |
| `config profile ls` | D | List named YAML files and source paths. A malformed file is a finding for that row, not a reason to hide all other rows |
| `config profile show` | Required `PROFILE`; `--resolved`; Y | PROFILE is a local name or explicit YAML path. Default shows redacted source data; resolved mode uses the existing extends/merge contract |
| `config profile check` | Required `PROFILE`; `--mode static|host|control`, default `static`; repeatable `--cmd STRING`; D | Static loads schema/references and evaluates available routing facts. Host adds this machine's capability/recipe checks. Control checks placement against the selected control plane. Unresolved runtime facts are reported, not invented |

Named profiles remain local files. An explicit project file is not automatically
registered. Keep the existing relative-path and extends semantics. Do not merge
context credentials into a task profile. A generic config mutation must not
become an alternate credential or permission-management API.

The writable global-key surface is bounded:

| Key | Type / default | Writer and validation |
| --- | --- | --- |
| `version` | Schema version 1 | Read-only; unsupported versions are rejected |
| `current_context` | Connection name, default local | Only config use writes it |
| `contexts` and child transport/credential/identity fields | Named records | Context commands own changes; generic set/unset rejects them |
| `paths.state` / `paths.data` / `paths.runtime` | Optional absolute paths | Global set/unset; absent uses existing platform defaults. Do not change any other path implicitly |
| `daemon.listen` | URI list, default empty extras | Additional network/custom listeners; management listener still exists. Validate schemes and duplicate addresses |
| `daemon.actor_image` | Optional image directory | Absent uses the existing host-driver base-image resolution |
| `daemon.local_runner` | Optional registered runner name | Validate the existing name contract; do not create registration by setting a string |
| `daemon.guest_needy` | Optional executable path | Preserve version/architecture/helper validation |
| `daemon.executor_image` | Optional image reference | Absent uses the current release/source-build default |
| `daemon.heartbeat_interval` | Positive duration, default 15s | Global set/unset; preserve the runner scheduling/liveness meaning |
| `daemon.engine_host` | Optional engine address | Explicit Docker-compatible engine boundary; absent resolves the existing engine default/environment. Service install records the effective address |
| `daemon.process_path` | Optional ordered executable-search path entries | Validate entries. Absent foreground serving uses its process PATH; installation records the effective PATH explicitly |

All other generic keys are rejected with valid-key guidance. There is no
arbitrary environment map, arbitrary profile mutation path, or generic credential
setter in the first version. Named profiles and their extends chains remain
edited as files. Changes to global daemon settings take effect on the next
foreground serve or an explicit service replace/restart; they are not live patches.

Profile display masks every actor/executor/rule/preset environment value and
schema-known credential field by default. Preserve keys so the view remains
useful. Redact recognized URI userinfo and exact known secrets before rendering.
Arbitrary user-authored argv, free text, or vendor URI query values can embed
unknown secrets; there is no claim of universal detection. Document this limit,
never log full profiles automatically, and test the defined masking rules.

### Daemon and service parameters

The local control daemon and standalone executor share the existing serving
mechanisms. Their roles remain explicit in the CLI and discovery record.

| Command | Parameters | Behavior |
| --- | --- | --- |
| `daemon serve` | Repeatable `--listen ADDRESS`; `--actor-image DIR`; `--local-runner NAME`; `--guest-needy PATH`; `--executor-image REF`; `--heartbeat-interval DURATION` default 15s | Run the control role in the foreground. Always add the private management listener; extra listeners do not replace it. Runtime settings are independent of client selection |
| `daemon executor` | One `--listen ADDRESS`; `--name NAME` default `local`; optional `--token-file FILE`; Unix `--socket-group GID` | Preserve the standalone process-service role, including container use. Do not publish it as the default control management endpoint. The token file is the incoming server credential, distinct from the outgoing client `--credential-file`. Network serving requires its explicit credential policy |
| `daemon service install` | `--now`; `--scope user|system`, default `user`; `--run-as USER` required for system scope; `--replace`; `--timeout DURATION` for `--now` | Register and enable the service. Do not start without `--now`. Store stable executable, account, directories, and explicit daemon options. Refuse foreign/conflicting definitions |
| `daemon service uninstall` | `--scope`; `--timeout DURATION` default 60s | Stop the owned service, wait, disable autostart, remove owned definition. Preserve configuration, credentials, state, traces, images, and logs |
| `daemon service start` | `--scope`; `--timeout DURATION` default 30s | Start an installed service and require the expected instance and control role to answer successfully |
| `daemon service stop` | `--scope`; `--timeout DURATION` default 60s | Ask the manager to stop its owned instance. Wait for controlled shutdown. An already stopped service succeeds |
| `daemon service restart` | `--scope`; `--timeout DURATION` default 60s | Use one stated total budget for stop then start. Do not overlap instances |
| `daemon service status` | `--scope`; D | Distinguish not installed, stopped, starting, ready, failed, and API-unreachable. Show manager identity, account, instance, and log location |
| `daemon service logs` | `--scope`; `-f/--follow`; `--tail N` default 100 | Read the owned service's log source. Do not infer a process by name |

The daemon runtime options move into a typed local daemon configuration section.
Serve flags override that section for foreground use. Installation snapshots
its effective non-secret settings; `--replace` is the explicit service-update
operation. A service invocation does not depend on the installing shell's cwd,
rc files, full environment, or current remote context.

Hidden test controls retain their existing purpose: a stand-in KVM device for
control/runner tests and a shorter runner disconnect grace for test scenarios.
They are not advertised as normal setup options. Remove neither test seam
without replacing its observable coverage.

### Authorization and runner parameters

| Command | Parameters | Behavior |
| --- | --- | --- |
| `client invite` | `--name NAME`; `--ttl DURATION` default 10m; `--admin`; D | Omitted name requests a generated unique client name. Reusing an active server name reports replacement when consumed. Create a one-use client code. Default access is run/process use; management access is explicit. Output its secret once, outside diagnostic/log output |
| `client ls` | D | List identities, granted access, activity, and revocation state; no credential values |
| `client revoke` | Required `CLIENT_ID` | Revoke that server-side identity only. Preserve its audit record |
| `runner invite` | `--ttl DURATION` default 1h; `--uses N` default 1; comma-separated `--roles`; repeatable `--labels KEY=VALUE,...`; D | Issue a purpose-limited runner join code. Omitted roles preserve the existing allowed-role contract |
| `runner join` | Required `NAME`; one `--address URI`, connector argv, or `--context NAME` transport; `--code-file FILE`; comma-separated `--roles`; repeatable `--labels KEY=VALUE,...`; `--actor-image DIR` | Detect capabilities, exchange the join code, save runner-owned transport/credential/state. A client context contributes transport only, never its admin bearer |
| `runner serve` | `--actor-image DIR`; comma-separated `--roles`; repeatable `--labels KEY=VALUE,...` | Foreground reconnect from saved runner state. No new code required. Omitted values use saved join settings; roles can narrow the saved grant, not expand it. Explicit label overrides update saved runner settings after validation. Do not silently join another control plane |
| `runner ls` | D | List online, offline, and removed runners, host platform, roles, and capabilities |
| `runner rm` | Required `NAME` | Revoke the runner credential; retain the record as removed. Do not delete client contexts |
| `runner rotate` | Required `NAME` | Preserve heartbeat-based credential rotation; old credential expires after adoption |
| `runner token ls` | D | Show metadata for valid, expired, spent, and revoked join codes, without their secrets |
| `runner token revoke` | Required `ID` | Revoke the join code. Existing joined runners keep their independent credentials |

Invitation table output prints public metadata plus one clearly marked secret
code, once. JSON creation output is a versioned object with public ID/name,
granted access or roles, expiry, and the once-only code. A later list never
returns that code. Diagnostic JSON is separate stderr output.

Runner join persists the selected transport, own credential, registered name,
roles/labels, image override and resolved state location. Serve re-detects actual
capabilities within that grant. A role or label change is validated before
storage/heartbeat; an unsupported role is reported, not silently advertised.

Invitation and join secrets use hidden input or a private file on consumers.
Their deliberate one-time creation output is not copied into logs, diagnostics,
or a service definition. A local alias and a server client/runner name are
separate identifiers. Reusing a server client name can revoke its old identity;
report that replacement explicitly before local persistence.

### Run, process, file, and report parameters

| Command | Parameters | Behavior |
| --- | --- | --- |
| `run create` | Required `--profile PROFILE`; `-t`; `-i` requires `-t`; `--detach-keys`; `--group GROUP`; `--name NAME`; appended actor args after `--` | Preserve creation, first-terminal subscription, and driving-program exit behavior. No bare-run shorthand in this slice |
| `run ls` | D | List live and ended runs, actor state, executor release/loss, profile, and result |
| `run stop` | Optional `RUN`; `--grace DURATION` default current 10s; `--now` conflicts with grace | Graceful stop runs mirror-out; now skips grace and actor mirror-out. Report forced/failed completion |
| `run attach` | Optional `RUN`; A | Attach the actor driving program's terminal. Do not resolve a process ID implicitly |
| `run logs` | Optional `RUN`; `-f/--follow` | Stream actor console bytes. Keep log bytes separate from diagnostics |
| `run exec` | Optional `RUN`; optional `--executor NAME`; X; required command argv | Use this run's rules by default; explicit executor preserves the existing override. Never become a run-outside execution because RUN is missing |
| `run report` | Optional `RUN`; `--level summary|detailed`, default summary; `-o human|markdown|json`; `--file PATH`; `--offline` | Default reads selected control-plane events. Offline reads the resolved local trace store and never contacts another target |
| `proc exec` | Exactly one `--runner NAME`, `--host`, or `--executor NAME`; X; required command argv | Always outside a run. Host means the selected control/executor daemon's process host, not necessarily the CLI machine. A pooled executor name must resolve uniquely. A saved run variable cannot redirect it |
| `proc ls` | Optional mutually exclusive `--run RUN` or `--runner NAME`; `-a/--all`; D | Default lists visible processes on the selected control plane. Include internal Needy processes only with all |
| `proc attach` | Required `PROCESS_ID`; A | Replay then attach one executor process; preserve its terminal and exit semantics |
| `proc kill` | Required `PROCESS_ID`; `-s/--signal SIGNAL`, default SIGTERM | Send a signal, not a run stop. Preserve platform signal conversion |
| `fs cp` | Required SOURCE and DESTINATION; one target form: `--run RUN --executor NAME`, `--runner NAME`, `--host`, or `--executor NAME` outside a run | One side is local and one is selected remote. Runner/host mode needs no embedded name. Run mode requires a named executor; standalone pooled-executor selection must be unique. No remote-to-remote expansion |

Require the `--` separator for run/proc exec. In `run exec -- echo hi`, both
words after the separator belong to argv; `echo` cannot be consumed as RUN.
In `run exec demo -- echo hi`, demo is RUN. Add parser-level regressions for
both forms, flags after the separator, and command names that match subcommands.

Omitted RUN resolves from a valid inherited execution context, else the only
live run on the selected control plane. Ambiguity reports candidates. Online report retains an explicit `latest` fallback and resolves a supplied
run name through the existing run-list boundary before requesting events.
Offline report accepts a full ID, unique ID prefix, or `latest`, not a run name;
the trace store has no durable run-name index. Current reports select the latest
prefix match without resolving names. Rejecting ambiguous report prefixes is
an explicit target behavior change and needs a trace-reader regression plus
migration notice, not an unnoticed parser rename. A supplied empty RUN is an error in the target grammar, not a hidden escape
from an environment variable. An ended run can be reported; attach/exec explain
why it is no longer executable.

For file copy, use `remote:PATH` as the one reserved remote-side marker. The
runner/executor identity comes from the explicit target selector, not from that
marker. Preserve Windows drive-letter parsing. A file path containing a colon
needs its platform path parsing, not a generic split at the first colon.
For example, target syntax can be tested with `fs cp --runner workstation
./input remote:./project` and the reverse direction. This intentionally migrates
the current `NAME:path` contract; do not keep both meanings silently.

### Chaos parameters

All chaos actions move under `run chaos`. They retain optional `--run RUN` so
that fault NAME remains unambiguous. They use the same run resolver as other run
operations. The fault schema and seeded behavior do not change.

| Command/group | Parameters and validation |
| --- | --- |
| `run chaos add NAME` | Exactly one domain selector: `--exec PATTERN`, `--open GLOB`, `--connect HOST`, or `--request HOST`; alternatively `--file YAML` |
| Exec effects | `--exit CODE`, `--signal SIGNAL`, `--stderr TEXT`, `--hang`, `--delay DURATION`; existing compatible-effect constraints remain |
| File effects | `--errno NAME`, `--delay DURATION`; preserve supported errno values and archive/file-domain behavior |
| Network effects | `--refuse`, `--reset`, `--status HTTP_STATUS`, `--hang`, `--delay DURATION`; request-only status constraints remain |
| Trigger arguments | `--probability FLOAT`, `--nth N`, `--times N`, `--after DURATION`, `--until DURATION`; retain ranges, conflicts, and ordering |
| YAML input | `--file` supplies the complete fault. Reject effect/trigger flags alongside it instead of choosing one source silently. Sensor faults remain representable through YAML |
| `run chaos ls` | Optional `--run RUN`; D; include source, match count, and fire count |
| `run chaos rm NAME` | Optional `--run RUN`; remove a profile or added live fault and retain trace recording |

### Image parameters

Image operations remain local. A selected remote context does not redirect the
local Docker-compatible engine, VM store, or Apple's container image store.

| Command | Parameters and validation |
| --- | --- |
| `image pull` | Repeatable `--driver DRIVER`; `--version VERSION`; `--file ARCHIVE` conflicts with version and imports one VM archive; preserve host detection, checksums, and ABI checks |
| `image ls` | D; preserve VM, container, custom image, and executor rows |
| `image build` | `--recipe REFERENCE`, `--out DIR`, `--executor`, `--linux-container`, `--macos-apple-container`, `--from OCI_REF`, `--base DIR` |
| Build constraints | `--from` conflicts with recipe/executor/container-base flags; `--base` requires from; Apple container base conflicts with recipe/out. Preserve the current compatible multi-target behavior |
| `image rm` | IMAGE arguments; `--driver DRIVER`; `--container`; `--guest-disks`; require at least images or cleanup intent. Never delete container assets implicitly |

### Normal workflows

On a control host, install a service or run foreground serving. These are
alternatives, not two processes to start together:

```sh
needy daemon service install --now
needy daemon service status
needy runner ls
needy run create -it --profile demo
```

Issue management access on that control host. On the client, consume the code
through hidden input, then select the saved connection:

```sh
# Control host
needy client invite --name workstation --admin

# Client, through an existing local TCP forward
needy config context pair development --address tcp://127.0.0.1:17400
needy config use development
needy runner ls
```

For Cloudflare Access, save a command transport without a local listening port:

```sh
needy config context pair production -- cloudflared access tcp --hostname prod.example.com
needy config use production
needy --context development run ls
```

For a runner, use a separate join code and identity:

```sh
# Operator terminal
needy runner invite --roles executor

# Execution machine, not the control host's SSH session
needy runner join workstation --address tcp://127.0.0.1:17400
needy runner serve
```

### Diagnostic presentation and coverage

A diagnostic states the failed operation, stable reason, relevant known facts,
and one or two applicable checks/actions. It identifies the selected context
and source when relevant. It does not claim an authenticated identity before
successful authentication.

Target human output:

```text
needy: [auth.admin_required] Cannot remove runner "workstation".
  Context: development (selected in config.yaml)
  Caller: client "workstation"; access: runs

  Next: Inspect available connections: needy config context ls
        Use a connection with management access, or request a new invitation.
```

For a missing local service, suggest install or start based on the known service
state. For a remote forward failure, identify the configured client-side port.
Do not suggest starting an unrelated local control daemon. Do not suggest local
access to repair a remote permission failure unless the operator is on that
control host and wants its local control plane.

JSON diagnostics on stderr have a version, reason code, summary, sanitized
selection/object facts, completion state, and structured checks/actions.
Commands in actions are argv arrays plus an execution location, not shell text.
Human rendering uses the maintained shell-quoting helper and escapes control
characters in untrusted facts; it does not concatenate them into executable text.
Unknown or inapplicable facts are absent. Default output is short; verbose output
adds safe underlying causes. No diagnostic contains token values, bootstrap
codes, URL userinfo, unfiltered environment values, or protocol stdout.

The catalogue below is the target handled set. A slash-separated group means
separate reason codes, not English-message matching. “Rewrite” is the target
summary pattern; object and location facts are supplied separately.

| Reason code(s) | Rewrite and known cause | Applicable next action |
| --- | --- | --- |
| `config.read_failed` / `config.write_failed` | “Cannot read/write the local configuration.” Preserve the OS cause and source location | Inspect permissions/parent path and `config paths`; never discard credentials automatically |
| `config.invalid_yaml` / `config.invalid_field` / `config.invalid_reference` | “The configuration cannot be loaded.” Include field/location/extends facts that exist | Run the relevant offline profile check or correct the named global key |
| `config.version_unsupported` | “This configuration version is not supported.” State supported versions | Use a binary that supports the file version; preserve the file |
| `config.context_not_found` / `config.context_conflict` | “The saved connection does not exist/already exists.” | List connections; use a different name or explicit replace |
| `config.selection_conflict` | “These target selections conflict.” | Supply one context/address or one run/runner/host selector |
| `discovery.not_found` | “No local control daemon was discovered.” | Inspect installed service state; start it, install it, or serve foreground as appropriate |
| `discovery.invalid_record` / `discovery.stale_record` / `discovery.instance_conflict` | “The local daemon record is invalid/stale/owned by another instance.” | Inspect service status/logs and ownership. Never delete a live instance's socket blindly |
| `connection.invalid_address` | “The connection address is invalid.” | Show accepted schemes and the named configuration field |
| `connection.unreachable` / `connection.timeout` | “Cannot reach the selected control plane” / “The selected service did not become ready in time.” | Distinguish local service, remote forward, and startup wait; inspect only the matching target |
| `connection.wrong_instance` / `connection.wrong_role` | “The responding service is not the expected daemon.” | Inspect the selected address/service registration; do not send management calls to an executor |
| `connector.not_found` / `connector.start_failed` | “Cannot start the connector.” Preserve executable and OS cause | Inspect the configured executable/PATH on the client |
| `connector.failed` | “The connector ended before the connection was established.” | Use known exit facts and bounded safe external detail; do not label every failure as Cloudflare login expiry |
| `auth.credential_rejected` | “The selected credential was rejected.” | Inspect its source/connection and request current authorization if appropriate. Do not assert expiry or revocation without verified facts |
| `auth.backend_unavailable` | “The control plane could not verify credentials.” | Inspect control-plane logs/state access. Do not tell the client to discard a possibly valid credential |
| `auth.admin_required` | “This operation requires management access.” | Show verified caller/access; use an authorized context or obtain an admin invitation |
| `auth.role_denied` | “This caller cannot use this API.” | Explain client/actor/runner protocol boundary. Changing a local context alias does not grant authority |
| `bootstrap.invalid_code` | “The authorization code is not valid.” | Obtain a current code for the right purpose; keep unknown/wrong secrets generic |
| `bootstrap.save_failed` | “Authorization succeeded, but the new credential could not be saved.” | Preserve the old local record; show public identity and local storage cause. Request explicit revocation/new authorization rather than retrying a consumed code |
| `bootstrap.expired` / `bootstrap.used` / `bootstrap.revoked` / `bootstrap.scope_denied` | State the condition only when the store proves it | Obtain a new matching invitation; do not broaden roles automatically |
| `client.invitation_open` / `client.not_found` | “An invitation already exists” / “The client does not exist.” | Preserve current useful invitation guidance; inspect clients without printing secrets |
| `runner.name_in_use` / `runner.not_found` | “This runner name is already registered/not present.” | Show the selected control plane and status/candidates; no automatic removal |
| `runner.offline` / `runner.not_connected` | “The runner is not available for execution.” | Inspect that runner on its own machine and its control route |
| `runner.capability_missing` / `runner.selector_no_match` / `runner.selector_ambiguous` | Name requested driver/roles and known candidates | Choose a compatible runner or refine the selector |
| `run.not_found` / `run.ambiguous` / `run.ended` | State selected scope and lifecycle state/candidates | List runs, choose explicitly, or inspect the ended run; never search another control plane and execute there |
| `run.no_terminal` / `run.no_console` | “This run has no attachable terminal/console.” | Use available report/process inspection, or create a terminal-enabled run |
| `process.not_found` / `process.internal_attach_denied` | “The process is not available” / “This internal process cannot be attached.” | List matching processes; do not offer an admin-token bypass for protected mounts/transfers |
| `process.start_failed` / `process.cwd_invalid` / `process.terminal_failed` | Name program, actual execution target, and OS/PTY/cwd cause | Inspect tools and workspace/path maps on the executor, not on the client |
| `process.outcome_unknown` | “The connection ended before the process result was received.” | Include known IDs; inspect or attach before retrying |
| `policy.exec_denied` / `fault.exec_injected` | Preserve the routing decision or injected-fault cause and events | Inspect the matching rule/fault and report; do not recommend re-pairing or blanket permissions |
| `capacity.exhausted` | “The execution capacity limit was reached.” | Show known limit/target; wait or explicitly adjust the relevant policy |
| `executor.authentication_failed` | “The executor rejected the control plane's credential.” | Inspect operator-managed executor configuration. Do not ask the end client to re-pair |
| `image.missing` / `image.abi_mismatch` / `image.architecture_mismatch` | Name the exact missing component or mismatch | Give the matching release pull or source build path for that driver |
| `image.integrity_failed` | “Image verification failed; this image was not installed.” | Report successful earlier artifacts in a multi-driver pull. Retry a trusted source; never bypass checksum/archive checks |
| `files.invalid_target` / `files.unsafe_archive` | Identify invalid copy side or refused extraction condition | Correct target/path or archive; never suggest disabling extraction safeguards |
| `service.not_installed` / `service.manager_unavailable` / `service.definition_conflict` | State the actual local registration/manager problem | Inspect the relevant scope; install/update explicitly, or serve foreground |
| `service.start_failed` / `service.stop_timeout` / `service.cleanup_failed` | State startup or controlled-stop failure | Show manager/API state and log path. Forced exit is not graceful success |
| `platform.unsupported` | “The requested capability is not supported on this platform yet.” Include actual platform and capability; Windows control/service is one example | Give the applicable supported host/role alternative. For Windows control/service, use a Linux/macOS control host and preserve Windows runner use |
| `state.read_failed` / `state.write_failed` / `protocol.invalid_response` | Preserve store or protocol cause and known field/version | Inspect state access or peer versions; do not classify these as profile syntax errors |
| `internal.unknown` | “The operation failed.” Add the safe original cause | Give the relevant log/check location. Do not invent an exact repair |

Bootstrap codes are deliberately generic when the presented secret is unknown.
Do not create an oracle by exposing another identity's revocation history.
Permission, routing policy, injected fault, and backend availability are separate
reasons even when their current gRPC status categories overlap.

## User Stories

1. As a local operator, I want to start a control daemon without creating a token, so that initial use needs no credential ceremony.
2. As a local operator, I want TCP serving to retain local management access, so that a network setting cannot lock me out.
3. As an operator, I want the CLI to discover the actual local socket, so that I do not copy runtime paths.
4. As an operator, I want stale discovery to produce a precise check, so that I do not delete another instance's socket.
5. As a client, I want one configuration entry for global values, connections, and profiles, so that I can find persistent inputs.
6. As a client, I want to list named profiles without a daemon, so that I can select an existing task.
7. As a client, I want malformed profiles to remain visible in listings, so that one bad file does not hide other tasks.
8. As a client, I want source and resolved profile views, so that I can understand inheritance without reimplementing merge rules.
9. As a client, I want static, host, and control checks to be explicit, so that I know which machine a finding describes.
10. As a client, I want arbitrary project YAML paths to remain valid, so that local registration is optional.
11. As a client, I want to inspect resolved directories and their sources, so that overrides are understandable.
12. As a client, I want to save several paired control planes, so that pairing another one does not overwrite my setup.
13. As a client, I want two identities on one address to have separate local aliases, so that transport does not define identity.
14. As a client, I want only explicit use to change my default target, so that authorization has no hidden selection effect.
15. As a client, I want per-command target selection, so that one operation does not change other terminals.
16. As a process launched by Needy, I want a fixed inherited selection, so that another terminal cannot redirect my next call.
17. As a client, I want an unavailable target to fail in place, so that my operation cannot run on another control plane.
18. As an operator, I want independently revocable client credentials, so that I do not rotate a shared administrator secret.
19. As an operator, I want management access to be an explicit grant, so that existing clients keep their current authority.
20. As an operator, I want traces to name the remote administrator, so that management access does not erase identity.
21. As a client, I want a verified caller check, so that local configuration cannot misrepresent my authority.
22. As a client, I want hidden code input or private-file input, so that bootstrap secrets are not command-line arguments.
23. As a client, I want connection removal to be local, so that it does not revoke a shared server identity unexpectedly.
24. As an operator, I want revocation to act on one server identity, so that other clients remain connected.
25. As a client using a local forward, I want one saved address, so that I do not repeat a temporary port on every command.
26. As a client using Cloudflare Access, I want a saved connector argv, so that I need no competing local listener.
27. As a client, I want connector failures distinguished from Needy authorization, so that I repair the correct layer.
28. As an operator, I want service install and uninstall, so that I do not maintain unit or plist files by hand.
29. As an operator, I want start, stop, restart, status, and logs, so that I never need process-name searches for normal management.
30. As an operator, I want service readiness to identify the expected instance, so that a running PID cannot produce false success.
31. As an operator, I want service stop to complete run cleanup within a stated budget, so that saved state is not silently lost.
32. As an operator, I want uninstall to preserve data and credentials, so that service removal is not a data purge.
33. As an operator, I want local service actions to ignore a persisted remote default, so that their machine scope stays predictable.
34. As an operator, I want system and user service scopes explicit, so that installation cannot change the runtime account silently.
35. As a runner operator, I want join and serve separated, so that restart reuses my runner credential.
36. As a runner operator, I want capability and role checks before joining, so that unsupported roles are explained.
37. As an operator, I want runner removal and rotation preserved, so that machine access remains manageable.
38. As a client, I want run execution and direct process execution distinct, so that routing does not depend on hidden ambient state.
39. As a client, I want direct host, runner, and unique pooled-executor targets, so that the redesign preserves current execution reach.
40. As a client, I want actor-terminal attach and process attach to name their objects, so that I reconnect to the correct program.
41. As a client, I want child exit codes and terminal behavior preserved, so that scripts and interactive tools keep working.
42. As a client, I want unknown completion reported explicitly, so that I inspect before repeating a side effect.
43. As a client, I want offline reports, so that I can inspect local traces after the control daemon stops.
44. As an operator, I want live fault controls and their trace events preserved, so that replay remains meaningful.
45. As a client, I want file copy selectors with one fixed remote marker, so that a path prefix cannot change its object type.
46. As a client, I want Windows paths preserved, so that drive letters are not mistaken for resource selectors.
47. As an image maintainer, I want release pinning, archive import, custom OCI builds, and explicit cleanup preserved, so that CLI cleanup does not remove build capabilities.
48. As a client, I want reasons, selection facts, and applicable actions in errors, so that failure tells me what to do next.
49. As an API consumer, I want stable structured reasons without losing refusal events, so that diagnostics do not break existing exec consumers.
50. As an operator, I want authentication backend failures distinguished from rejected credentials, so that I do not re-pair during a store outage.
51. As a Windows user, I want unsupported control/service operations rejected before writes, so that I can still use supported runner capabilities.
52. As a new user, I want wizard choices reflected in the complete profile and command sequence, so that examples do not discard my machine choices.
53. As a new user, I want machine-specific copy blocks and one shared connection setup, so that I do not start a runner on the wrong machine.
54. As a developer, I want explicit configuration-version and package boundaries, so that implementation follows the existing code order.

## Implementation Decisions

### Ownership and code order

| Module/crate | Responsibility in this change | Constraint |
| --- | --- | --- |
| CLI command modules | Own each public Args and run entry, help, selector validation, and unsupported-input messages | Keep one module per canonical top-level command; group leaf code within that boundary |
| CLI connection resolution | Resolve configuration, explicit selection, inherited selection, discovery, and credential source once | Return one resolved target/identity-source value; callers do not repeat precedence rules |
| CLI paths and private storage | Resolve platform paths and perform locked atomic writes | Use existing path conventions; do not move process or socket code into config |
| CLI output/diagnostics | Render safe human/JSON diagnostics and version-correct actions | Domain producers supply facts, not shell instructions |
| CLI service boundary | Common service contract with systemd and launchd implementations | Implementations do not depend on each other; share actual components, not forwarding wrappers |
| Config crate | Typed global/client/daemon configuration, version handling, pure validation | No process, socket, gRPC, OS-manager, or environment-dependent service code |
| API client | Existing transports, URI normalization, readiness expectations, diagnostic decoding, caller lookup | No implicit user configuration or bearer environment reads inside the library |
| API server | Per-listener admission, caller/access projection, protocol adapters, one status/diagnostic conversion | Daemon policy never constructs raw gRPC statuses |
| Daemon | Registry grants, run/process scheduling, discovery lifecycle coordination, controlled shutdown | Long-lived tasks remain owned by daemon/run TaskTrackers |
| Daemon state store | Additive client and invitation access migration | Existing client/code rows become runs-only, never administrators |
| Core actor/executor | Preserve capture, execution, relay, typed startup causes, and cleanup boundaries | No service-manager dependencies in pure domain/core modules |
| Trace/core events | Preserve named client principals and current event/report vocabulary | Granting admin access does not rename the client to anonymous operator |
| Xtask | Existing end-to-end host/transport/platform matrix | Add cells/invariants; do not write replacement remote shell e2e scripts |

Implementation follows Rust 2024 and the repository's two-space formatting.
Use action-first names within modules and keep APIs small. Use anyhow context
for composition and typed thiserror errors when callers branch on failure.
Prefer maintained parsers and platform libraries. Any hand-written mechanism
needs a reason near its implementation.

Use dependency injection at actual external boundaries: filesystem, OS service
manager, process/connector, network, clock, and engine. Do not add internal
Dependencies bags or alternate production paths solely for tests. Preserve
existing comments when moving code. Apply extension-point directory/trait/table
rules to the service backends; platform implementations cannot import each other.

### Selection and storage

The global document has a version, current connection, connection records,
explicit directory overrides, and typed daemon runtime settings. A task profile
remains a separate document. A client-side configuration file override selects
that document and its named-profile directory; state, data, and runtime remain
separately resolved and displayed. No current run is persisted globally.

Selection order is an explicit context or direct address, a valid inherited
execution selection, the saved current connection, then built-in local. Direct
selection and inherited credential references must belong to the same resolved
transport. No failure causes fallback to another target or stronger identity.

Before consuming a pairing code, validate and lock its destination and inspect
local name conflicts. If remote pairing succeeds but persistence fails, report
that the new credential was issued but not saved. Preserve the old local record;
do not retry the consumed code or print the bearer. Give the operator the
public client ID for explicit revocation and fresh authorization.

A connection record stores address or connector argv, client identity metadata,
and its own credential. An execution context stores only non-secret stable
references and a resolved run/target selection. The actor uses its bound guest
channel, not a copy of management credentials. Same-OS-account native execution
is a trusted privileged environment by the owner's explicit decision.

Private writes use a lock, restrictive creation, a same-directory temporary,
and atomic replacement. Unix directory/file defaults are 0700/0600. Windows
client persistence needs explicit ACL coverage; Unix modes are not an ACL
implementation. Discovery records contain instance/role/address facts, not secrets.

The global document is the only client-configuration source. Do not import the
old endpoint-keyed `credentials.json`, infer its default, or convert its tokens.
Leave that file untouched. Users establish new named identities with explicit
invitations and `config context pair`. No shared-token upgrade command or
legacy command aliases are required by this owner-approved slice.

`NEEDY_ENDPOINT` and `NEEDY_TOKEN` are unsupported. Without an explicit new
selection, their presence produces a selection diagnostic rather than quietly
using local operator access. Explicit context/address selection uses only its
own credential source. `NEEDY_RUN` is no longer consumed; pass RUN or inherit a
versioned execution selection. Directory overrides and source-build repository
selection remain advanced inputs. Daemon binding never reads client target
variables. Internal capture ABI and Docker-compatible engine variables remain
unchanged.

Normal database schema migrations preserve existing state and give old client
records only runs access. This is a storage upgrade, not a legacy client API or
credential-file import path. Unspecified and unknown wire access values fail
closed; the CLI explicitly requests runs or admin access.

### Local discovery and per-listener admission

Create one process instance ID for the control daemon. Use that same ID in its
health response and protected descriptor. The descriptor also records its
publisher PID as correlation data, not as process ownership authority. Hold an exclusive publication lock.
Publish only after reconciliation and a successful expected-role/instance
health call. Shutdown removes the record only when it still belongs to that
instance. Never treat the instance ID as a durable or cryptographic server identity.

Apply OS admission only to the designated management listener. The default
Unix policy admits the daemon owner and root through a private management
socket. It does not add group sharing. Existing actor guest/vsock/Unix channels
keep their bound run identity; standalone executor sockets retain their own policy.

An explicit bearer is validated as presented. A restricted or invalid bearer
cannot fall back to local owner authority. Network listeners require registered
client/runner credentials; loopback reachability alone is not authority. Only
health and purpose-specific bootstrap exchanges remain bearer-free where their
services are actually exposed.

Connection resolution is purpose-aware. Control operations require a control
service. Direct proc/fs host operations may use a standalone executor and do
not require ClientService.GetCaller. A context check reports that role and marks
control-only identity inspection unavailable. Pairing requires the control role.
Do not make a universal control-only probe a prerequisite for every connection.

The stricter readiness helper requires a successful health response plus the
expected role and instance. Keep the current generic reachability helper for
consumers that intentionally use its weaker contract until they are migrated.
For a managed service, correlate the descriptor PID with the manager's actual
service PID. Require manager PID = descriptor PID, descriptor instance = health
instance, and health role = control. Recheck manager state/process identity
across the bounded probe. A matching healthy manual instance is not service
readiness. Never signal or adopt a process based only on the descriptor PID.

Service status reports manager state, API identity/readiness, and admission as
separate facts. Readiness does not prove authenticated management permission.

### Authorization and authenticated inspection

Introduce server-granted client access with two concrete levels: runs and admin.
Unspecified and unknown request access values fail closed. Existing stored rows
receive runs-only access through the schema upgrade. Add access to the domain caller and credential result while preserving
client identity and the existing client trace principal.

Admin clients may manage clients, runners, and invitations over authenticated
network connections. Replace the current local-only restrictions for client and
join-code metadata listing with the same explicit management policy. OS-local
operator authority remains the bootstrap path. Actor and runner protocol bounds
are unchanged. This is not a general role-policy language.

Invitation creation returns the granted access. Pairing returns it through the
client record. Before an admin invitation, check support through authenticated
caller inspection. Treat an unimplemented inspection RPC or missing access
contract as unsupported before issuing the invitation. An older server or a
downgraded response must not appear to have granted admin access. Never print an
unusable or downgraded secret as a
successful administrator invitation.

Credential lookup returns a result that distinguishes invalid credentials from
backend lookup failures. Unknown/wrong secrets remain generic. A registry outage
maps to internal/unavailable diagnostics, not a false re-pair suggestion.

### Protocol changes: minimum additive contract

CLI grouping does not rename RPCs. Local config/profile listing, discovery, and
OS service lifecycle need no new RPC. The existing client, runner, run, process,
and guest services remain the protocol owners.

| Contract | Proposed additive change | Compatibility behavior |
| --- | --- | --- |
| Client access | Add ClientAccess with unspecified=0, runs=1, admin=2. Add access to CreatePairingCodeRequest at field 3, CreatePairingCodeResponse at field 6, and Client at field 6 | Unspecified and unknown wire values reject. Existing stored rows remain runs-only. PairResponse already carries Client |
| Caller inspection | Add authenticated ClientService.GetCaller with an empty request. GetCallerResponse has kind at field 1, client ID at field 2, name at field 3, and access at field 4. Kind values are unspecified=0, local_operator=1, client=2 | Derive from middleware caller. Reject unsupported caller kinds. Return no secret/hash. An old server's UNIMPLEMENTED means access unknown, not administrator |
| Diagnostic value | Add ErrorDiagnostic with reason string at field 1 and bounded safe string facts at field 2 | Unknown reasons retain ordinary error behavior; facts are not executable instructions |
| Unary/status diagnostic | Encode that value in dedicated binary metadata named needy-diagnostic-v1-bin | Preserve gRPC code, message, and existing raw status details exactly |
| Exec stream diagnostic | Add a normal, non-oneof diagnostic field 9 to ExecResponse | Keep existing error string field 5 and ended payload. Old consumers still receive their payload |
| Runner error bridge | Add details bytes at TaskError field 3 and diagnostic at field 4 | Preserve refusal events and known diagnostics through both runner bridge directions |
| Runtime findings | Add optional diagnostic at ProfileFinding field 3 | Keep severity/message fields and existing finding semantics |
| Health readiness | Reuse instance ID, version, role, and platform | No serving-state or lifecycle RPC is required in the initial slice |

The existing PERMISSION_DENIED details contain raw ExecRefusal bytes. Do not
replace them with a generic diagnostic envelope. Preserve refusal-event decoding
and NDJSON replay input. Add diagnostic metadata alongside those bytes instead.

The authenticated caller adapter reads its middleware-established identity.
It does not query the entire client registry or need a daemon service method
merely to echo that identity. Use a separate diagnostic schema dependency for
the shared error value, avoiding circular process/run/runner schema imports.

The runner bridge currently rebuilds status from only code/message. Updating
only the direct API path would lose diagnostics and refusal details on runner
execution. Add bounded conversion at the API boundary and preserve both fields.
In-band exec errors also need structured facts; metadata alone is insufficient.

Bound the diagnostic record to 8 KiB, at most 16 fact entries, and 1 KiB per
fact value. These are proposed protocol limits, not platform measurements.
Validate known fact keys by reason and omit untrusted/secret values. Reject a
malformed diagnostic as unusable detail, not as a replacement for the original
status. Report safe protocol-version detail in verbose output. Forward only
this known diagnostic and existing refusal details, not arbitrary metadata.

Keep wire conversion in API server/client helpers. Daemon methods return domain
errors and facts. The daemon runner composition calls those helpers instead of
adding new ad hoc status tables. Update misleading existing client-proto
comments about paired clients having every operator right. Correct the exec
selector comment: runner is exclusive with run/executor; run plus executor is
a supported override, not an invalid pair.

Change canonical schemas and regenerate the API-proto crate through the existing
Buf/make workflow. Never hand-edit generated code. Use Buf lint and FILE-level
breaking checks against an explicit baseline. A clean breaking check does not
prove explicit-grant enforcement or unknown-enum handling; test those.

### Diagnostic implementation and execution semantics

Add typed causes at the producer or at the composition boundary that still has
the cause. Do not infer reason codes by matching an English string. Preserve
configuration paths/locations, scheduling candidates, actual cwd, and upstream
execution machine while they are known.

Replace the current working-directory substring hint with a typed startup
reason. Preserve the distinction between caller authentication failure and an
executor rejecting the daemon's credential. The latter remains an upstream
availability/configuration error from the end client's perspective.

Connector spawning currently inherits stderr and does not capture a complete
classified failure. Add a bounded observation boundary before promising
connector exit facts. Keep protocol stdout untouched. Preserve interactive
connector stderr behavior; captured summaries may redact known secrets but
cannot claim to sanitize arbitrary external output perfectly.

Current exec-start exit semantics are deliberate: denied starts return 126,
failed-precondition starts return 127, ordinary infrastructure failures return 1,
and unknown stream completion returns 255. Completed child and signal results
retain their existing conversion. A numeric 255 alone does not imply unknown
completion, because a child can exit 255. Include completion-known state in
structured output. Changing these mappings requires a separate versioned contract.

Global diagnostic JSON remains on stderr. Existing exec JSON/NDJSON stdout
records keep their schemas and refusal event ordering. Existing data output
formats remain command-specific: table/JSON for listings, human/JSON/NDJSON for
exec, and human/Markdown/JSON for report. A closed stdout pipeline keeps the
existing SIGPIPE exit behavior without an extra friendly diagnostic.

### Service lifecycle and shutdown

Use one common service boundary with independent systemd and launchd backends.
The manager owns the process, restart policy, and stop request. Discovery owns
connection location. Do not use a PID file or process-name scan as ownership.
A hand-started instance occupying the same state/runtime is a conflict, not a
process to kill or adopt.

User scope is the default. System scope requires an explicit runtime account.
A missing user manager or launchd domain produces a useful scope diagnostic.
Do not change linger, elevate privileges, or change runtime accounts implicitly.
Windows control/service installation fails before any service-definition write.
Runner service installation is a later role-specific consumer of the same mechanism.

Controlled daemon shutdown stops new work, handles runs being created, stops
active runs using normal cleanup/mirror-out, then closes executors, links, and
owned discovery. The current immediate-stop loop is an intentional historical
deferral, not proof that SIGTERM already saves state. Replace it in an approved
shutdown slice with a finite global budget and explicit incomplete-cleanup result.
Preserve the second-signal and explicit run-stop-now force paths.

### Delivery order

| Slice | Change and dependency | Acceptance result |
| --- | --- | --- |
| 1. Configuration contract | Typed global schema, local inventory, profile modes, command parser tree and explicit selectors | All proposed leaf usage/help is enumerated; local config works without a daemon |
| 2. Local access | Owned management listener, discovery publication, strict readiness, safe selection diagnostics | Local runner management works without bearer, even with network listening and old remote credentials |
| 3. Remote authority | Additive client access/GetCaller, middleware/domain/store migration, context pairing | Named admin grants, denial, revocation, explicit-grant validation, and named trace identity pass |
| 4. Diagnostic transport | Reason/facts and status/stream/finding/runner preservation | Direct and runner paths return the same cause/action inputs without losing refusal events |
| 5. Managed lifecycle | Service definitions, manager backends, finite controlled shutdown, status/logs | Install/start/stop/restart/uninstall need no manual PID; cleanup and retained data are observed |
| 6. Execution grammar | Run/proc/fs split, exact target validation, execution context | All old execution/copy targets and exit/terminal semantics remain available |
| 7. Assets and documentation | Preserve image/chaos/report options; update examples, wizard, glossary and help | The four user workflows run from documented commands on applicable hosts |

Implement one reviewed behavioral slice at a time. Write a regression at the
existing external seam before changing its producer. A spec or deferred marker
is not permission to implement unrelated future work.

## Testing Decisions

### Existing seams and what constitutes a good test

Use the existing CLI integration boundary as the primary product seam. Run the
real binary against isolated configuration directories and a test control plane.
Assert the selected server, actual output/exit result, stored record, and observed
side effect. Do not test private function call order or mirror implementation
branches with assertions that cannot catch the reported failure.

Use the existing API-server domain-trait and in-process daemon tests for protocol,
authority, and runner forwarding cases that need multiple actors or peers. Use
the repository's non-root end-to-end harness for actual actor drivers, service
managers, and host/transport behavior. The owner confirmed these three existing
seams during drafting. That decision does not assert that any proposed product
behavior or test has run.

| Surface | Required observations |
| --- | --- |
| CLI/config | Every command/flag/conflict in the catalogue parses or fails before mutation. Config/profile lists work offline. Selection and source facts match the target. Private writes preserve contents and identity; retired credential files are never imported |
| CLI diagnostics | Human and JSON errors identify the correct object and next action. Absent service versus stopped service produce different advice. Local versus remote suggestions differ. Redaction and stderr/stdout boundaries hold |
| API authentication | Owner/root management admission, restricted/invalid explicit bearer without owner fallback, authenticated network admin/runs clients, runner/actor bounds, backend-error distinction |
| Registry/store | Legacy rows and outstanding codes become runs-only. Admin grants and revocation persist. Same-name replacement is explicit. Unknown access values fail closed |
| Protocol round trip | New metadata with old raw refusal bytes, unknown/missing diagnostics, old server caller inspection, in-band error and finding fields, and both runner TaskError conversions |
| Readiness/discovery | A failed health response is not ready. Wrong role/instance is rejected. Competing publisher cannot replace a live owner. Old shutdown cannot remove new state. Stale/malformed discovery never triggers remote fallback |
| Execution | Routed run exec, explicit run executor, host/standalone executor, unique/ambiguous pooled executor, native runner, terminal replay/read-only/detach, Windows exit conversion |
| Files | Upload/download for run executor, pooled executor, runner, and host; Windows drive paths, unsafe archive refusal, one remote side, no remote-to-remote expansion |
| Run/report/chaos | Appended actor argv, graceful versus now stop, ended-run reports, offline report without a daemon, fault file conflicts, matching/fire counts and trace events |
| Service platform | Install/enable versus start, actual manager ownership, expected API instance, user/system scope, repeat operations, conflict with manual serve, controlled stop/mirror-out, uninstall retaining data |
| Platform support | Unsupported Windows control/service returns a specific nonzero diagnostic before writes. Existing runner behavior is not disabled |
| Documentation | Relative links/anchors, command examples versus the proposed catalogue, wizard selection/profile consistency, installation-source distinctions, and visible evidence levels |

An exit code alone is not enough for an execution test. Assert completed output,
refusal events, process ID, persisted state, or completion-unknown status as
appropriate. Static unit/plist validation is useful but does not prove a service
can start, stop, or survive its intended session lifetime.

### Required regression scenarios

1. Configure network listening, retain a local management socket, and remove a
   runner locally without exporting any credential.
2. Pair another control plane and confirm that the local default and existing
   client identities remain unchanged.
3. Select a remote connection, fail its transport, and prove that no request was
   sent to a different control plane.
4. Grant remote admin, exercise runner/client management, observe its named
   principal in a run trace, revoke it, and prove other clients still work.
5. Run the same failure through a direct executor and a runner. Preserve refusal
   events, diagnostic reason, actual cwd/target, and exit semantics in both.
6. Make credential lookup fail independently of a bad secret. Only the first
   case reports an authentication backend failure.
7. Stop a managed daemon with pending run work and a mirror-out artifact.
   Observe cleanup or an explicit bounded failure, never silent success.
8. Change the saved default in another terminal while a related process runs.
   Prove its inherited target remains fixed.
9. Try each unsupported/conflicting service operation and inspect the filesystem
   and manager to prove that no unrelated service or record changed.
10. Exercise the Linux-actor/Mac-executor onboarding path, including shared files
    and the selected entrypoint. Report authenticated tool calls separately.

### Validation commands and provenance

When implementing Rust changes, use the repository's normal build, formatting,
check, and test targets. Extend existing CLI/API/daemon tests and the xtask e2e
matrix. When implementing schema changes, read the repository's Buf skills,
change canonical schemas, run generation/lint/breaking checks, and commit the
generated API-proto output. Do not call a generated-only change a protocol test.

The docs consolidation itself requires link/anchor checks, private-data checks,
command/argument coverage review, and independent design review. It does not
require rerunning unrelated actor or frontend product tests. Historical frontend
and live-probe results below are not new verification of this specification.

## Out of Scope

- Windows control hosting and Windows service installation in this delivery.
  Preserve supported Windows runner/native execution behavior.
- A general RBAC language, arbitrary custom roles, or client-side role claims.
- New capture drivers, runtime shim coverage, actor networking expansion, or
  agent-specific configuration concepts.
- Isolation between an administrator account and native processes running as
  that same account. The owner accepts the privileged trust boundary.
- System secret stores, multi-file configuration merge, automatic project
  profile registration, or recursive profile discovery.
- A global doctor subsystem, automatic permission loosening, speculative
  vendor-error parsing, or automatic destructive repairs.
- Remote image-store management, remote OS service control, run survival after
  a control-plane crash, or unrelated persistence TODO implementation.
- A second detached-process supervisor, automatic linger changes, or implicit
  privilege elevation.
- Renaming existing RPCs merely to match CLI words, changing trace vocabulary
  for admin clients, or editing generated API files by hand.
- Bare `needy run` shorthand with ambiguous trailing argv. The documented parser
  deferral remains until its own grammar is approved.
- Publishing issues, changing live machine configuration, committing, or pushing
  as part of this documentation task.

TODO: Windows service adapters, secret-store integration, multi-file merge,
background service installation for the runner role, and crash-survival work
need their own owner-named slices and acceptance. Do not infer authorization
from this list. Legacy command/token compatibility is intentionally omitted by the owner; no deprecation bridge is required.

## Further Notes

### Current command and behavior inventory

Source inspection confirms 13 roots and 30 public leaf operations, excluding
help/version and hidden harness controls. This inventory explains the target
mapping; it is not a statement that the new tree already exists.

| Current root / leaves | Target mapping | Behavior that must survive or change explicitly |
| --- | --- | --- |
| `daemon` (one parser leaf with three modes) | `daemon serve`, `daemon executor`, `runner join/serve`, service group | All role-specific options, standalone executor, name/socket group, runner saved state, image/helper/engine settings; standalone executor mode is explicit |
| `exec` | `run exec` or `proc exec` | Run rules/override, control host, standalone executor, pooled executor, runner, PTY/pipes/stdin/detach/env/cwd/output and shell exits |
| `ps` | `proc ls` | Run/runner filters, internal-process visibility, table/JSON |
| `attach` | `proc attach` | Single-process replay/read-only/terminal controls |
| `kill` | `proc kill` | Process signal, default SIGTERM; never substitute run stop |
| `fs cp` | `fs cp` with explicit selectors and fixed remote marker | Upload/download, host/pooled/run/runner targets, archive safeguards, Windows paths |
| `run create/ls/stop/attach/logs` | Same run leaves | Profile names/paths, appended actor argv, group/name, terminal subscription, grace/now, console follow |
| `runner ls/rm/rotate`, `token create/ls/revoke` | Registry leaves, `runner invite`, token metadata leaves | Roles, labels, TTL/uses, removed records, heartbeat rotation, purpose-limited code |
| `pair CODE`, `pair create/ls/revoke` | `config context pair`, `client invite/ls/revoke` | Bearer-free code exchange, same-name replacement, separate local alias, once-only secret, explicit new admin capability |
| `profile check` | `config profile check` plus inventory/show | Repeatable command decisions, static versus host versus control checks, source/resolved recipe handling |
| `report` | `run report` | Offline trace read, existing ID/prefix/latest selection, new online name resolution, summary/detailed, human/Markdown/JSON, output file |
| `chaos add/ls/rm` | `run chaos` | All match/effect/trigger arguments, YAML sensor cases, trace/replay, live removal |
| `image pull/ls/build/rm` | Same image leaves | Four drivers, archive/version, OCI/base/out/kernel recipe builds, container/executor bases, explicit cleanup |

The nine identified interaction classes are configuration fragmentation,
pairing/authorization side effects, daemon/runner role split, run/process object
confusion, overloaded exec placement, run-related report/chaos roots, copy-target
ambiguity, endpoint/listen/local terminology coupling, and inconsistent local
versus selected-control-plane scope. Different domain objects still need
separate operations; grouping alone does not solve these behaviors.

### API and package impact inventory

| Current service/message | Reuse or modification | Why |
| --- | --- | --- |
| ClientService.CreatePairingCode / Pair | Reuse names; additive access fields | Client invite and context pair are CLI workflows over the same bootstrap exchange |
| ClientService.ListClients / RevokeClient | Reuse names; authority-aware policy | Admin clients manage identities; runs-only clients retain their restrictions |
| ClientService.GetCaller | Add authenticated RPC | Context check needs verified caller/access without listing unrelated clients |
| RunnerService.CreateJoinToken / Join / Heartbeat / RotateRunner / RemoveRunner | Reuse | CLI invite/join/serve regrouping does not require alternate registration protocols |
| RunnerService.ListJoinTokens / RevokeJoinToken / ListRunners | Reuse; management policy where applicable | Preserve existing metadata/status and per-identity revocation |
| RunnerService.Control / Task and TaskError | Preserve framing; additive error details/diagnostic | Runner forwarding currently loses typed remote diagnostic/refusal data |
| RunService.CreateRun / GetRun / ListRuns / StopRun / GetRunLogs / GetRunEvents | Reuse | Renaming CLI paths and controlled local service stop do not require a run-lifecycle API clone |
| RunService.Decide / CheckProfile / fault RPCs | Reuse; optional finding diagnostic | Preserve routing and fault behavior, add facts only where currently lost |
| ProcessService.Exec / ListProcesses / KillProcess | Reuse | Explicit CLI targets map to existing run/executor/runner selectors |
| ExecResponse / ExecRefusal | Add non-oneof diagnostic, preserve raw refusal details | Maintain existing terminal/event/old-client behavior |
| HealthService.Check | Reuse fields | Strict expected-instance readiness is a client/lifecycle behavior change |
| GuestService | No new management/config authority | Actor channels remain bound to their run; command vocabulary is not a guest-protocol rename |
| Profile listing / local config / service manager / discovery | No RPC | These operate on the CLI/control host's local resources and OS manager |

No generated package or crate rename is required. The API-proto crate changes
only through regeneration. API-client and API-server do need implementation
changes for access, diagnostics, caller inspection, and forwarding. The daemon
and its state store need access migration and shutdown changes. Config and CLI
need typed schemas and local mechanisms. Trace/core principal vocabulary can
remain unchanged.

### Error producers that are scattered or not yet structured

This is an explicit residual inventory. The reason catalogue names desired
handling; the rows below identify facts that the current implementation cannot
reliably supply. They must not be hidden by optimistic formatter text.

| Producer / present state | Missing distinction | Required handling and boundary |
| --- | --- | --- |
| Credential registries log database failures and return Refused; Authenticator returns an optional caller | Bad credential versus failed registry | Domain result carries backend failure; middleware maps it. Unknown secrets stay generic |
| ControlError Denied serves permission, policy, fault, and protected-process cases | Actionable subreason and object facts | Attach a producer reason; do not infer it from PermissionDenied alone |
| Executor start errors have typed cwd/PTY/start causes, then become strings | Actual startup cause and target | Preserve typed facts through executor API, runner TaskError, daemon relay, client |
| Relay adds a cwd hint by substring matching | A message can change independently of its cause | Replace that one heuristic after the typed producer path exists; test the actual routed failure |
| ProfileFinding has only severity/message | Stable reason/location and scheduler candidates | Add the optional diagnostic only for known producer facts; keep other findings readable |
| Config loader has structured read/YAML/schema/reference errors | CLI renderers often expose only the chained text | Adapt existing variants; retain source, field, YAML span and inheritance facts where present |
| Connector stderr is inherited and child status is not exposed as diagnostic | Failure before spawn versus exit/Access/protocol failure | Observe the external process boundary; classify only known facts; do not parse arbitrary vendor prose |
| Driver/engine/PTY/vendor errors carry driver/message or anyhow sources | Reliable fine-grained external cause | Keep a generic driver/process family and safe original cause; no speculative universal taxonomy |
| API conversion/event/manifest decoding errors are distributed | Bad local input versus malformed peer data/version | Distinguish local schema, protocol, and stored-artifact errors at the decoding boundary |
| State/trace I/O and migration failures use broad errors | Availability, permissions, schema or corruption facts | Preserve store and real cause, propose inspection/migration rather than reset |
| Discovery/context/service are new subsystems | No existing failure producers | Implement their typed errors and tests with the subsystem, not a fictional current-error rewrite |
| Process origin after restart has a recorded deferral | Verified origin may be unknown | Preserve unknown; do not invent ownership or add unrelated persistence in this slice |
| Shutdown intentionally uses immediate stop for active runs | Mirror-out completion is not guaranteed | Approved controlled-shutdown slice with a finite budget; service text cannot fix it |
| Invitation secrets are intentionally printed once | Generic redaction could destroy required creation output | Keep explicit secret-creation stdout separate from safe logs/diagnostics |
| Stream/child exit 255 shares one numeric value | Completion known versus unknown | Preserve typed completion state; never infer from the number alone |

Unknown or unstructured errors remain visible as internal/driver/state/protocol
families with a real cause and safe inspection path. There is no promise to
classify every operating-system or vendor string. The implementation review
must check residual rows again before claiming full actionable-error coverage.

### Source map for implementation and review

These links identify current producer/consumer seams. Implementation decisions
above use module responsibilities rather than locking the spec to file layouts.

- [CLI root, selection, and error exit](../../../../crates/cli/src/cli.rs),
  [paths](../../../../crates/cli/src/paths.rs),
  [configuration](../../../../crates/cli/src/configuration.rs),
  [run resolution](../../../../crates/cli/src/runs.rs), and
  [output](../../../../crates/cli/src/output/mod.rs).
- [Command modules](../../../../crates/cli/src/commands/mod.rs), including
  [exec](../../../../crates/cli/src/commands/proc/exec.rs),
  [files](../../../../crates/cli/src/commands/fs.rs),
  [profile](../../../../crates/cli/src/commands/profile.rs), and
  [image](../../../../crates/cli/src/commands/image/mod.rs).
- [API domain errors, callers, and traits](../../../../crates/api-server/src/control.rs),
  [authentication](../../../../crates/api-server/src/middleware/authentication.rs),
  [listener binding](../../../../crates/api-server/src/server.rs), and
  [status mapping](../../../../crates/api-server/src/protocol/status.rs).
- [API client error kinds](../../../../crates/api-client/src/error.rs),
  [exec refusal decoding](../../../../crates/api-client/src/exec.rs),
  [readiness](../../../../crates/api-client/src/ready.rs), and
  [command transport](../../../../crates/api-client/src/command.rs).
- [Client registry](../../../../crates/daemon/src/clients.rs),
  [runner registry](../../../../crates/daemon/src/runners.rs),
  [relay](../../../../crates/daemon/src/relay/mod.rs),
  [runner bridge](../../../../crates/daemon/src/runner.rs),
  [link bridge](../../../../crates/daemon/src/link.rs), and
  [store entities](../../../../crates/daemon/src/store/entity.rs).
- [Client schema](../../../../proto/needy/v1/client.proto),
  [health schema](../../../../proto/needy/v1/health.proto),
  [process schema](../../../../proto/needy/v1/process.proto),
  [run schema](../../../../proto/needy/v1/run.proto),
  [runner schema](../../../../proto/needy/v1/runner.proto), and
  [guest schema](../../../../proto/needy/v1/guest.proto).
- [CLI integration tests](../../../../crates/cli/tests/cli.rs),
  [API status tests](../../../../crates/api-server/src/protocol/status_test.rs),
  [authentication tests](../../../../crates/api-server/src/middleware/authentication_test.rs),
  [relay tests](../../../../crates/daemon/src/relay/mod_test.rs), and
  [e2e design and evidence](../architecture/2026-11-13-e2e-harness-design.md).

### External design references

The inspected auv revision is `e57aaec65e88d3b714918c89bcf6680331f50b12`:

- [Serve contract](https://github.com/moeru-ai/auv/blob/e57aaec65e88d3b714918c89bcf6680331f50b12/crates/auv-cli/src/commands/serve.rs): separate local and remote listeners, CLI OS-user admission by default.
- [Discovery reader](https://github.com/moeru-ai/auv/blob/e57aaec65e88d3b714918c89bcf6680331f50b12/crates/auv/src/discovery.rs) and [publisher](https://github.com/moeru-ai/auv/blob/e57aaec65e88d3b714918c89bcf6680331f50b12/crates/auv-daemon/src/discovery.rs): protected descriptor, lock, atomic publication, and instance-owned cleanup.
- [Authentication](https://github.com/moeru-ai/auv/blob/e57aaec65e88d3b714918c89bcf6680331f50b12/crates/auv-api-server/src/authentication.rs): principal-to-authority mapping and separate local/remote methods. The library also supports stricter launcher-token mode; not all local auv configurations are token-free.
- [Execution context](https://github.com/moeru-ai/auv/blob/e57aaec65e88d3b714918c89bcf6680331f50b12/crates/auv/src/context.rs): non-secret target references, with credentials resolved separately.

Platform references are [systemctl 256](https://www.freedesktop.org/software/systemd/man/256/systemctl.html)
and [Apple's archived launchd guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html).
They establish manager semantics, not Needy service acceptance. The original
service review read them on 2026-11-14 and inspected local launchctl help only;
it did not register a service. A future Windows service needs the
[SCM service entry/control contract](https://learn.microsoft.com/en-us/windows/win32/services/service-entry-point),
not just registration of a console executable.

[Docker socket access](https://docs.docker.com/reference/cli/dockerd/) and
[kubeconfig contexts](https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/)
illustrate OS-protected local access and saved connection/identity selection.
[kubeadm administrator configuration](https://kubernetes.io/docs/setup/production-environment/tools/kubeadm/create-cluster-kubeadm/)
contains actual credentials; discovery alone is not authorization.

The existing [Cloudflare transport evidence](../architecture/2026-11-13-cloudflare-transport-evidence.md#cloudflare-access-tcp)
covers the current stdio command transport. It does not prove the new context
commands or replace Cloudflare's own Access authentication.

<a id="onboarding-review"></a>

### Historical onboarding findings — 2026-11-13

This subsection preserves the merged onboarding review as a historical snapshot.
Some findings were addressed by the uncommitted documentation/wizard draft.
They are not claims that the target CLI or access redesign has shipped.

| Priority / finding | Observed problem | Required onboarding behavior |
| --- | --- | --- |
| P1, TCP address continuity | TCP-only daemon setup omitted endpoint/credential on later commands. The local-actor/remote-executor wizard branch could bypass pairing and still invoke later commands | One consistent listener-to-client route and identity, with shell-local versus saved values distinguished |
| P1, copy block crosses machines | Pairing/join snippets mixed control and executor commands; a Mac used as client and runner could open the same tunnel twice | One copy action per machine/terminal/credential role; one reused route; install Needy on each participating machine |
| P1, selected environment discarded | Final agent step used a fixed Claude/Firecracker/container example despite earlier driver/runner answers | Preserve selected driving program, actor driver, executor, network, state, and project path throughout the generated walkthrough |
| P1, workspace incomplete | Executor workspace alone did not supply actor mount and path map; a zero-exit profile check could still warn about cwd mapping | Use actor workspace derivation; create executor directory first; explain each machine's home and prove shared visibility |
| P1, generic OpenCode entry used an experiment | Local-model setup and fixed prompt appeared as generic interactive onboarding | Keep the model-service experiment separate and state prerequisites; use the generic scenario by default |
| P2, stale release instructions | README/wizard said source-only although release v0.0.3 assets existed | Separate release installation from source builds; keep guest/image ABI and image names consistent |
| P2, route rule description | “First match wins” hid ordered combination of single values/collections and final | Explain actual rule assembly and capture limits next to the selected task |
| P2, undefined finish state | Steps ended after profile/run commands without machine, shared-file, login, or return/stop success criteria | Define phase-specific evidence, login, attach/detach, graceful stop, and restart expectations |

Historical source/live observations:

- Source inspection traced endpoint priority, profile paths, rule combination,
  workspace derivation, and the wizard/Studio example selection.
- A CLI invocation with empty temporary config/state/runtime and no endpoint or
  token variables returned exit 1 for the absent default socket. It did not
  start or stop a daemon.
- A workspace-only profile check returned 0 while warning about unmapped actor
  cwd. A passing check did not establish shared project access.
- A GitHub release check on 2026-11-13 found
  [v0.0.3](https://github.com/needy-dev/needy/releases/tag/v0.0.3) binaries and
  VM archives. Asset presence did not verify every install channel.
- Live probes used a Linux x86_64 control host and Apple-silicon native runner.
  They observed Linux entrypoint, Mac child shell, shared file access, tool
  version queries, and Claude/OpenCode interface startup. They did not verify
  provider login or authenticated model-driven tool calls.

Capture and permission explanations remain required: the driving program starts
without exec routing; shell builtins stay in their shell; container shims do not
capture arbitrary newly installed programs. A routed child shell executes its
whole script on the executor; that is not a separate actor interception for
every inner command. Built-in editor/file actions need shared workspace access.
HTTP/HTTPS hostname allowance, native-host authority, and an application's own
approval policy are distinct. HTTP hostname allowance is not arbitrary-protocol
network access.

The intended tutorial path chooses program/machines/project, installs each role,
checks runtime/guest/images, establishes one route, optionally pairs the client,
joins/verifies the runner host OS, prepares workspace/state, generates the
matching profile, validates routing/files, and then tests entry/login/return/stop.
README remains short. Detailed tutorials live in explanations. Examples own the
executable profiles, and the website/Studio reuse them. Source build, releases,
mock models, and authenticated tools have distinct evidence levels.

<a id="onboarding-evidence"></a>

### Historical documentation draft evidence — 2026-11-13

The uncommitted documentation draft added bash/Claude/OpenCode entry links,
folded older README reference material, a Linux-to-Mac walkthrough, canonical
profiles, and Studio defaults. It changed no CLI, daemon, capture, or live-host
configuration. The public examples allowed HTTP/HTTPS hosts and preserved
application approval policies.

The first web draft replaced the environment questionnaire with a fixed flow.
Owner feedback restored local/SSH host, Linux/Apple-silicon macOS, package
manager, KVM, Mac runtime, executor placement, and Docker choices. Remote choices
also included runner OS/name/project path. Routing could cover captured children
or child shells. The compact chips, two-column layout, and small selected accent
were restored. Global palette/theme/style files were unchanged.

| Historical check | Recorded result | Limit |
| --- | --- | --- |
| First draft frontend tests | 216 passed | Entry selection, topology, token-input context, one-tunnel regressions |
| Restored wizard frontend tests | 254 passed | Includes 36 entrypoint/driver/executor combinations through Studio schema/reference checks |
| Generated profile local CLI checks | All 36 passed | Configuration validation only, not fresh installs on all host combinations |
| Three canonical public YAML checks | All three exited 0 | Installed CLI, no live runner/model authentication |
| Frontend type check and production build | Passed | Existing large-chunk warning remained |
| Relative Markdown links | Passed | File existence; external URLs and all fragments were not exhaustively checked |
| Desktop Chrome | Initial program/role/steps view and direct start fragment observed; restored dark two-column questionnaire observed | Narrow viewport and full interactive switching were not completed |
| Live topology | Equivalent Linux-container/Mac-native probes observed | Exact public YAMLs did not replace the working private configuration |

The desktop view exposed an initial-fragment timing issue. The draft resolved
the section after mount and font readiness. No browser captures, host aliases,
private paths, tokens, raw run records, or machine logs were committed.

Fresh non-root installation, provider login, authenticated tool calls, exact
walkthrough stop/start persistence, narrow layout, and the restored combinations
still need their stated acceptance. A Codex entrypoint and moving folded
reference chapters into dedicated pages remain separate candidates. Historical
frontend/static successes are not acceptance of this CLI specification.

### Independent review and disposition

Three independent read-only sub-agent reviews covered command/argument
preservation, diagnostics/error producers, and protocol/package impact.
Their source-backed findings are incorporated in this document:

| Review finding | Resolution in this specification |
| --- | --- |
| Run-outside host/standalone/pooled executor reach could be lost | Preserve explicit host, runner, and pooled-executor selectors, with ambiguity errors |
| Offline report and host recipe checks could be flattened | Keep offline reporting and separate static/host/control profile modes |
| Image/chaos options and standalone executor could disappear | Enumerate those leaf options and preserve purpose-specific behavior |
| Service settings lacked a persistent runtime source | Use typed daemon settings and a fixed service snapshot, independent of current client context |
| Client admin is a new API/storage grant, not a rename | Add server-granted access, legacy runs-only migration, and named audit identity |
| Raw refusal details and runner relay could break | Keep raw ExecRefusal, use additive metadata/stream fields, preserve TaskError details |
| Auth backend failures were indistinguishable from bad credentials | Return a domain/backend result through authentication and render the correct cause |
| Existing readiness accepts error responses | Add expected-instance/role success checks without a new lifecycle RPC |
| Connector details cannot be promised from inherited stderr | Require an actual observation boundary; preserve terminal/protocol output contracts |
| Current shutdown explicitly skips mirror-out | Treat controlled shutdown as its own tested behavior slice |
| Platform status wording could overstate progress | Parameterize unsupported capability/platform; Windows control/service is unsupported/planned, not “in progress” without work evidence |
| Service readiness could match an unrelated manual daemon | Correlate descriptor PID and instance with manager PID and health; recheck manager identity |
| Global client credential and executor server token used the same flag | Reserve outgoing credential-file for clients; standalone executor uses token-file |
| Global writable keys and runner overrides were vague | Enumerate the bounded global schema and exact persisted runner role/label override rules |
| Reports incorrectly claimed existing run-name support | State current ID/prefix/latest behavior; define new online names and restricted offline selectors |
| Generic control probes could reject standalone executors | Resolve admission and health by operation purpose |
| Integrity wording ignored earlier successful images | Scope non-installation to the failed artifact and retain partial results |
| Profile display implied perfect secret detection | Use conservative env/credential masking and a clear arbitrary-content limitation |

After the fixes above, all three reviewers rechecked their changed contracts.
They reported no remaining blocking or major issues in those sections. Their
review did not run the proposed product behavior or acceptance tests.

NOTICE: These are design reviews, not product execution or implementation
acceptance. This document resolves the proposed interface for review. A release
still needs platform acceptance. The owner subsequently approved implementation
and explicitly removed legacy compatibility requirements; see the amendment above.

### Documentation consolidation checks — 2026-11-14

The six uncommitted source notes were merged into this document. Their duplicate
drafts were removed, and the README, tutorial, glossary, reference indexes, and
website evidence link now point here.

Static checks passed for 137 local Markdown targets and fragments across this
specification and its five referring documents. No references to the six removed
draft names remain in the scoped documentation and website sources. The seven
required specification sections and 54 user stories are present. The new
specification contains no Chinese text; a targeted private-data check found no
known machine identifiers or private workspace paths in the added text.

The proposed field numbers are unused in all six affected canonical Protobuf
messages. This is a source check, not a Buf compatibility result. The whitespace
check passed. No Rust, canonical schema, generated API, or runtime configuration
was changed for this consolidation. Product tests and real-platform acceptance
were not run for this docs-only delivery.
