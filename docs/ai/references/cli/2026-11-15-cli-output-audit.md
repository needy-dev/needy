# CLI output audit at v0.0.5

Date: 2026-11-15. Change classification: docs-only.

This audit records the public command surface at commit
`8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c`, the v0.0.5 baseline.
Later onboarding changes do not change the observations recorded here.
See the [working-branch changes and regression evidence](2026-11-15-client-onboarding-output-evidence.md) for the current fixes.
The improvement list separates the active approved slice from proposed follow-up work.

## Evidence and coverage

Evidence levels in this document:

- **Help probe:** Read-only execution of `needy --version` and recursive `needy ... --help` on the installed release binary.
- **Source audit:** Inspection of the command handlers and shared output code at the baseline commit.
- **Proposed:** A candidate improvement, without an implementation or behavior claim.

The help probe returned `needy 0.0.5`.
It found eight public top-level groups and 55 operational leaf commands.
Generated `help`, `--help`, and `--version` are additional presentation surfaces.
Hidden internal commands, guest dispatch, and `cargo xtask` are outside this public CLI inventory.

| Group | Public operational leaves |
| --- | ---: |
| `config` | 16 |
| `daemon` | 9 |
| `client` | 3 |
| `runner` | 8 |
| `run` | 10 |
| `proc` | 4 |
| `fs` | 1 |
| `image` | 4 |
| Total | 55 |

Discovery started at `needy --help`, followed each public `Commands:` entry,
and stopped at commands without further public children.
The audit did not start services, consume invitations, create runs, transfer files, or change images.
It did not connect through SSH or install software.
Except for help/version, the matrices describe source-backed behavior, not live verification or platform support.
Exact external program output, remote error messages, timing, and terminal escape bytes remain runtime-dependent.

## Shared presentation and exit rules

The [root dispatcher](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/cli.rs)
and [output module](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/output/mod.rs)
define the common rules.

| Surface or condition | Output and exit behavior |
| --- | --- |
| Successful ordinary operation | Exit 0, unless its matrix row specifies another result. “None” means no command-owned output on that stream. |
| `--help`, `-h`, or generated `help [COMMAND...]` | Clap prints help to stdout and exits 0. Long help can include additional descriptions. No command handler runs. |
| Root `--version` or `-V` | Version line on stdout, exit 0. |
| Argument parse error | Clap owns the diagnostic, usage, and exit 2. This occurs before the infrastructure diagnostic renderer. |
| Unhandled command/configuration/connection/filesystem/API error | Infrastructure diagnostic on stderr, exit 1. Partial stdout from earlier steps can remain. |
| Closed local stdout pipe | Exit 141 without an infrastructure diagnostic. A broken remote API connection does not count as closed stdout. |
| Row output | Default borderless table with a header, including an empty result. `-o json` prints a pretty JSON array, including `[]`. |
| Document output | Default YAML or pretty JSON object/value, according to the command. This is distinct from row output. |
| Informational messages and handled warnings | Usually plain `needy: ...` on stderr. They do not pass through the infrastructure diagnostic renderer. |
| Verbosity | Default process logs on stderr are off. `-v` enables info, `-vv` adds Needy debug, and `-vvv` enables trace for all targets. Logs can coexist with data and diagnostics. |
| Daemon process logs | Daemon commands also initialize daily log files in the resolved state directory. These differ from the product trace and OS service logs. |

The [diagnostic renderer](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/diagnostics.rs)
selects advice from typed reasons and API error kinds.
Human errors start with `needy: SUMMARY [reason]`, then facts and `Check:` lines.
Some checks include `client: needy ...` commands with the relevant selection arguments.
Known credentials, URL user information, and terminal control bytes receive redaction or escaping at this boundary.
Unknown errors can retain the `internal.unknown` reason.

`--diagnostic-format json` changes infrastructure errors on stderr to compact JSON.
Its fields are `version`, `reason`, `summary`, `facts`, `completion`, and `checks`.
Each check has `summary`, `location`, and optional `argv`.
`completion` can identify `unknown` or `not_started` outcomes.
The flag does not make all stderr JSON: prompts, process logs, progress messages, warnings, and child stderr retain their own formats.
It also does not select the command's successful `-o json` format.

At the baseline, infrastructure human diagnostics have no explicit semantic color styling.
The runner enrollment success line is green only on terminal stdout with an absent or empty `NO_COLOR`.
Human run reports use the same terminal/`NO_COLOR` condition and disable color for `--file`.
Tables and ordinary `output::message` lines do not add semantic color.
Logging and child programs control their own styling.

### Root selection flags

| Input | Meaning and visible consequences |
| --- | --- |
| `--config FILE` | Selects the client configuration document and adjacent `profiles/`. It also affects resolved paths and commands printed in diagnostic advice. |
| `--context NAME` | Selects a saved transport and identity for this invocation. A context name does not grant access. |
| `--address URI` | Selects direct TCP, HTTPS, or Unix transport. It conflicts with global `--context`. |
| `--credential-file FILE` | Supplies the private outgoing bearer for direct `--address`. Clap requires an address. |
| No explicit connection selector | Resolution uses the inherited execution reference, then the saved current context. Built-in `local` uses private local discovery and the OS account. |
| Failed connection | Resolution reports the selected target's failure. It does not switch to another daemon or identity. |

These rules come from the
[connection boundary](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/connection.rs).
Inherited selection carries references, not credential bytes.
A direct bearer file does not create a saved identity reference for later child invocations.
Legacy endpoint/token environment selectors can produce `config.selection_conflict` with explicit migration advice.

Daemon and image operations are local and reject global connection selectors.
Most configuration operations also reject them.
Context pair/set/check and profile checks in control mode have specialized selection behavior.
`runner serve` uses saved local enrollment and rejects global connection selectors.
At the baseline, pair/set also declare a local `--address` with the same name as the global flag.
The help probe shows that spelling, but does not prove that the direct-address pairing path parses correctly.
That parser collision belongs to the active onboarding fix.

## Configuration matrix

Sources: [configuration commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/config.rs)
and [profile commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/profile.rs).
All command names in the matrices have the `needy` prefix.
Common error and exit rules apply to every row.

| Command | Successful stdout | Stderr, prompts, progress, and conditions | Machine-readable output and exit behavior |
| --- | --- | --- | --- |
| `config ls` | `KIND NAME SOURCE FINDING` rows for global configuration, built-in/saved contexts, and local profiles. | None normally. Malformed profile findings remain in rows. | `-o json`: entry array. Listing malformed profile rows still exits 0. |
| `config show` | Global configuration document with recognized secrets masked. | None normally. Configuration load/validation errors use shared diagnostics. | `-o yaml` default, or `json`. |
| `config get KEY` | The selected schema-defined value with recognized secrets masked. | Unknown key diagnostic names the field and writable keys. | YAML default or JSON, including scalar/null values. |
| `config set KEY VALUE` | None. | No success confirmation or prompt. Invalid/managed fields give typed diagnostics and preserve the existing configuration. | No successful structured output. Exit 0 after atomic save. |
| `config unset KEY` | None. | No success confirmation or prompt. Same writable-key restrictions as `set`. | No successful structured output. Exit 0 after atomic save. |
| `config paths` | `KIND NAME SOURCE FINDING` rows for configuration, profiles, state, data, and runtime paths. | None normally. Findings identify the flag, environment variable, saved value, or platform default that selected each path. | `-o json`: entry array. |
| `config use NAME` | None. | No success confirmation or connection probe. Unknown context gives list/pair advice. | No successful structured output. Exit 0 means selection was saved. |
| `config context ls` | `NAME CURRENT TRANSPORT RECORDED IDENTITY RECORDED ACCESS` rows, including built-in `local`. | None normally. Local identity is `OS account`, access is `unknown until checked`. Saved identity/access are recorded facts. | `-o json`: context-row array. |
| `config context show NAME` | Saved record with credential masked, or the synthetic local transport/identity document. | Unknown context gives list/pair advice. | YAML default or JSON. |
| `config context pair NAME` | None at baseline. | Hidden `Invitation code: ` prompt unless `--code-file` is supplied. Success: `needy: paired context NAME as client ID; current selection is unchanged`. No onboarding guidance before the prompt. | No successful structured format. Baseline code-file input reads a private file, without a special stdin `-` branch. |
| `config context set NAME` | None. | No success confirmation. Requires one address or connector argv, preserves identity/selection, rejects global context/credential inputs. | No successful structured output. |
| `config context check [NAME]` | One `KIND NAME SOURCE FINDING` row. The finding combines transport, service role, instance, and caller text. | No progress message. Probe errors become an `unknown: ...` finding on stdout. Pre-probe argument/configuration errors still use stderr diagnostics. | `-o json`: the same entry array. Probe success exits 0, error/timeout exits 1. |
| `config context rm NAME` | None. | No prompt or success confirmation. Built-in local cannot be removed. Removing the current context gives switch-first advice. Server revocation is separate. | No successful structured output. |
| `config profile ls` | `NAME SOURCE FINDING` rows for local `.yaml`/`.yml` files. | None normally. A missing profiles directory is an empty listing. Invalid files retain load findings. | `-o json`: profile-row array. Malformed rows do not make the listing fail. |
| `config profile show PROFILE` | YAML source data, or resolved data with `--resolved`. Recognized credentials and environment values are masked. | No prompt. File/YAML/resolve errors use shared diagnostics. Arbitrary argv/free text can contain unrecognized secrets. | YAML default or JSON. Output is serialized data, not original YAML formatting. |
| `config profile check PROFILE` | Static: unresolved runtime/host line and findings. Host: user, executors, routes, optional services/faults/recipe/identity, and findings. Control: placement, optional CPU/image/ABI, routes/recipe, reasons, and findings. Repeated `--cmd` adds decision rows. | No interactive prompt or progress. Schema/recipe/host/API errors use shared diagnostics. | `-o json`: object with mode, findings, placement, recipe, decisions. Error findings exit 1, warnings alone exit 0. |

Pairing validates the context name and replacement condition before consuming a code.
An existing record requires `--replace`, and `--replace` requires an existing record.
After remote credential issuance, save/identity validation failures use `bootstrap.save_failed`.
That diagnostic gives the public client ID and instructs revocation plus a fresh invitation.
It explicitly warns against retrying the consumed code.

Context checks use a five-second default timeout.
An executor endpoint or an older control endpoint can succeed with caller inspection marked `unknown`.
The baseline combines that limitation into free text rather than separate service, identity, and admission fields.

Profile checks default to static mode.
Host mode adds facts about the current machine, while control mode asks the selected control plane about placement.
The JSON form and human form have different detail shapes. JSON recipe facts are limited to compiled vCPU and memory values in host mode.

## Daemon matrix

Sources: [foreground commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/daemon/mod.rs),
[service lifecycle](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/daemon/service/mod.rs),
[systemd adapter](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/daemon/service/systemd.rs),
and [launchd adapter](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/daemon/service/launchd.rs).

| Command | Successful stdout | Stderr, prompts, progress, and conditions | Machine-readable output and exit behavior |
| --- | --- | --- | --- |
| `daemon serve` | None from the handler. Runs in the foreground. | Missing default guest binary prints a warning that actors cannot start. Explicit guest path errors fail. Startup/runtime logs require verbosity on stderr. | No successful structured output. Exit 0 after serving ends successfully. Windows control hosting gives `platform.unsupported`. |
| `daemon executor` | None from the handler. Runs in the foreground. | Network listeners require `--token-file`. Token/listener/serve errors use shared diagnostics. Process logs follow verbosity. | No successful structured output. Exit 0 after serving ends successfully. |
| `daemon service install` | Detected manager/scope, runtime account, snapshot/definition/registration write steps, native status/log commands, enable step, optional start step, final installed line. | No confirmation prompt. Existing/foreign registration, scope/account, executable access, ownership, manager, readiness, and timeout failures use diagnostics. Earlier progress remains visible. | No successful structured output. `--now` adds readiness wait. `--replace` requires an existing owned installation. |
| `daemon service uninstall` | None. | No prompt or success confirmation. Stops with cleanup verification, disables, and removes owned definition/receipt. Failure gives manager/cleanup/timeout diagnostics. | No successful structured output. Configuration, state, credentials, logs, snapshots, and images remain. |
| `daemon service start` | None. | No progress or success confirmation. Waits for the manager PID and matching ready control instance. Missing install gives install advice. | No successful structured output. Deadline/readiness/manager failures exit 1. |
| `daemon service stop` | None. | No progress or success confirmation. Checks cleanup when stopping a running service. An already stopped service can return silently. | No successful structured output. Unverified cleanup or timeout exits 1. |
| `daemon service restart` | None. | No progress or success confirmation. Stop and start share the total timeout budget. | No successful structured output. Failure in either stage exits 1. |
| `daemon service status` | `STATE MANAGER ACCOUNT PID INSTANCE ADMISSION LOG` row. Missing receipt produces `not-installed`. Running PID without a matching API produces `api-unreachable`. | No normal stderr output. Ownership/manager inspection failures use diagnostics. | `-o json`: one-row array. `not-installed`, `failed`, or `api-unreachable` states still exit 0 when inspection succeeds. Admission remains `unknown`. |
| `daemon service logs` | Inherited output of `journalctl` on Linux or `tail` of the service log on macOS. Default tail is 100 lines. `--follow` continues. | The external log command inherits stderr and can emit its own errors. Needy adds a diagnostic when it fails. | No successful structured output. External nonzero status becomes Needy exit 1. |

Service management chooses the platform before installation writes.
Unsupported platforms give `platform.unsupported`.
User scope is the default. System installation requires `--run-as` and root, without automatic privilege elevation.
The installer prints native manager commands, while subsequent lifecycle commands mostly succeed silently.

## Client and runner matrix

Sources: [client commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/client.rs),
[runner commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/runner.rs),
and [local runner startup](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/daemon/runner.rs).

| Command | Successful stdout | Stderr, prompts, progress, and conditions | Machine-readable output and exit behavior |
| --- | --- | --- | --- |
| `client invite` | Baseline vertical fields: ID, NAME, ACCESS, EXPIRES, and CODE (shown once). Defaults: ten-minute TTL, run/process access. | Replacement invitations warn that pairing revokes the previous named client's credential. `--admin` verifies the caller and server-confirmed access. No pairing instructions at baseline. | `-o json`: version 1 object with id, name, access, expires_at_ms, code. Replacement warning stays on stderr. |
| `client ls` | `ID NAME ACCESS STATE PAIRED LAST USED` rows, including revoked clients. | No normal stderr output. Authentication/authorization/API errors use shared diagnostics. | `-o json`: identities with timestamps and optional revocation/last-used values. No credential/code field. |
| `client revoke CLIENT_ID` | None. | No prompt or success confirmation. Retains the audit record. | No successful structured output. |
| `runner invite` | “Runner invitation created”, metadata, runner-side join command, reachable-address/tunnel guidance, one-time joining token, and automation instructions. Defaults: one-hour TTL and one use. | No normal stderr output. Role/label/TTL parsing and management errors follow common rules. | `-o json`: version 1 object with id, roles, expires_at_ms, uses_left, code. Human guidance is omitted. |
| `runner join NAME` | Contacting control daemon line, success line, runner/control/capability summary, `runner serve`, control-side `runner ls`, and service status/log hints. | Without code input: invitation acquisition guidance on stderr, then hidden `Joining token: ` prompt. Capability warnings use stderr. | No successful structured output. `--code-file FILE` skips the prompt. `--code-file -` reads stdin. Exit 0 means enrollment saved, not persistent serving. |
| `runner serve` | None from the handler. Keeps the saved enrollment connected. | Capability warnings on stderr. Process logs require verbosity. Missing enrollment, unavailable explicit roles, or expanded enrolled roles fail. | No successful structured output. Uses saved connection, roles, labels, and actor image unless permitted overrides apply. |
| `runner token ls` | `ID STATE USES LEFT EXPIRES ROLES LABELS` rows, including expired, spent, and revoked invitations. | No normal stderr output. Unknown/unspecified wire states or roles fail rather than invent defaults. | `-o json`: metadata array, including creation/expiry timestamps. No token secret. |
| `runner token revoke ID` | None. | No prompt or success confirmation. Existing runner credentials remain independent. | No successful structured output. |
| `runner ls` | `NAME STATUS CAPABILITIES HOST CPU LABELS LAST SEEN` rows, including removed machines. Rotation adds `(rotating)` to status. Missing host/CPU uses `-`. | No normal stderr output. Unknown wire role/runtime/provider/status values fail. | `-o json`: complete runner rows, including rotation, capabilities, optional host, labels, timestamps, and join-token ID. |
| `runner rotate NAME` | None. | No prompt or success confirmation. Credential rotation takes effect at the next heartbeat. | No successful structured output. Exit 0 acknowledges the request, not completed rotation. |
| `runner rm NAME` | None. | No prompt or success confirmation. Revokes the runner while preserving its registry record. | No successful structured output. |

Runner join requires exactly one explicit address, saved context transport, or connector command.
It rejects `--credential-file` because enrollment uses the invitation.
It validates one nonempty joining token and supports private-file/stdin input.
Its first termination signal allows credential storage to finish.
A second termination signal can produce a warning about uncertain saved enrollment and inspection before retry.

## Runs and processes matrix

Sources: [run commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/run/mod.rs),
[report command](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/run/report.rs),
[chaos commands](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/run/chaos.rs),
[exec](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/proc/exec.rs),
[attach](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/proc/attach.rs),
[process listing](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/proc/ps.rs),
and [process signals](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/proc/kill.rs).

| Command | Successful stdout | Stderr, prompts, progress, and conditions | Machine-readable output and exit behavior |
| --- | --- | --- | --- |
| `run exec [RUN] -- COMMAND...` | Human: child output through the selected run's routing, or bound executor override. Detached: process ID. | Child stderr in pipe mode. Terminal mode merges child streams. Infrastructure failures and detach hints follow the execution rules that follow. | `-o human/json/ndjson`, default human. Child/refusal/stream exits apply. |
| `run report [RUN]` | Human report by default, Markdown or JSON on request. Summary is default, detailed adds timeline information. | No progress or prompt. Missing traces, unreadable event versions/payloads, connection, selection, and file-write errors use shared diagnostics. | `-o human/markdown/json`. `--file FILE` writes the report with no stdout confirmation. Exit 0 does not mean the reported run succeeded. |
| `run chaos add NAME` | `added NAME to run ID`. | Server-returned warnings use `needy: warning: ...`. Invalid fault YAML/domain/effect combinations and API errors fail. No prompt. | No successful structured output. |
| `run chaos ls` | `NAME DOMAIN SOURCE MATCHED FIRED FAULT` rows. Fault text uses compact flow-style YAML. | No normal stderr output. | `-o json`: rows with original fault YAML string and counters. |
| `run chaos rm NAME` | `removed NAME from run ID`. | No prompt. API/selection errors use shared diagnostics. | No successful structured output. |
| `run create --profile PROFILE` | Noninteractive: run ID only. `-it`: driving program terminal output, without the run ID mixed into stdout. | Interactive creation prints `needy: run ID` on stderr before attachment. `-i` without `-t` prints a specific use-`-it` message and exits 2. No creation progress summary. | No creation JSON flag. Noninteractive success exits 0. Interactive follows attachment/child exit behavior. |
| `run ls` | `ID NAME STATE ACTOR GROUP EXECUTORS EXIT` rows, including ended runs. Default ID-derived name/group uses `-`. Executors show `(lost)` or `(released)`. | No normal stderr output. Unknown wire states fail. EXIT shows code, signal, or first failure line truncated to 48 characters. | `-o json`: full failure text, actor/exit fields, executor flags, and tty state. |
| `run stop [RUN]` | None. | No prompt or normal success confirmation. Unexpected forced stop prints `run ID: the actor was killed without its mirror-out: REASON`. Explicit `--now` suppresses that warning. | No successful structured output. Successful response exits 0 even with the unexpected forced-stop warning. |
| `run attach [RUN]` | Replayed terminal screen and subsequent driving program output. `--no-replay` omits replay. | Detach prints a new-line `needy: detached from run ID; ... run attach ID ...` hint. Read-only mode sends no input/signals/window changes. | No structured format. Child/stream/detach exit rules apply. |
| `run logs [RUN]` | Raw actor console bytes, including boot messages and driving program terminal output. `--follow` flushes arriving chunks until the run stops. | No progress or prompt. Streaming API errors use shared diagnostics. | No structured format. Successful end of the log stream exits 0, independent of the actor's exit. |
| `proc exec TARGET -- COMMAND...` | Human child output, or process ID with `-d`. Target is exactly one host, runner, or pooled executor. | Same execution rules as `run exec`, without run routing. | `-o human/json/ndjson`, default human. Child/refusal/stream exits apply. |
| `proc ls` | `ID RUN ORIGIN EXECUTOR PID STATE EXIT TTY COMMAND` rows. Missing run/origin uses `-`. Internal transfers/mounts stay hidden unless `--all`. | No normal stderr output. `--run` and `--runner` conflict. | `-o json`: array with separate argv, optional exit/signal/origin, and tty. |
| `proc attach PROCESS_ID` | Recent pipe output or current terminal screen, then subsequent output. `--no-replay` omits replay. | Detach prints a new-line hint with `needy proc attach PROCESS_ID`. Read-only mode prevents input/signals/window changes. | No structured format. Child/stream/detach exit rules apply. |
| `proc kill PROCESS_ID` | None. | No prompt or success confirmation. Unknown signal fails before connection. Default signal is SIGTERM. | No successful structured output. Exit 0 acknowledges the signal request. |

### Execution, terminal, and native child output

Human execution and attachment relay child bytes directly.
A native executor session can show the shell's own prompt, colors, errors, and progress.
Infrastructure formatting does not wrap or recolor these bytes.
The same distinction applies to actor terminal output, raw run logs, and inherited service-log command output.

Execution automatically requests a terminal only for an interactive client in human mode.
`-t` forces a terminal, `-T` forces separate pipes, and structured output always uses pipes.
`-n` disables stdin forwarding. `-d` also disables forwarding and returns after the process starts.
Detach keys apply only to terminal input. The default is `ctrl-]`, and an empty value disables detachment.

| Execution condition | Output and exit |
| --- | --- |
| Child exits normally | Human: child's streams. JSON: one final object with process_id, executor, exit_code, signal, stdout, stderr. NDJSON: streamed events. Shell exit follows the child code. |
| Child ends by signal | Exit is 128 plus the local signal number, with fallback number 1. Structured output retains the signal. |
| Child code cannot fit the local exit status | Stderr names the actual code and exit 1. JSON preserves the real code. See the [signal mapping](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/core-executor/src/signals.rs). |
| Detach keys | Exit 0 and a stderr reattach hint. This is not evidence that the child exited. |
| `-d` start confirmed | Human process ID, JSON process_id/executor object, or NDJSON started event, then exit 0. No completion output is collected. |
| Start explicitly marked not started | Exit 125 plus infrastructure diagnostic. |
| Start denied | Exit 126 plus infrastructure diagnostic. |
| Start fails a precondition/cannot start | Exit 127 plus infrastructure diagnostic. |
| Refusal with `-o json` | stdout object with exit_code, signal, error, plus the stderr infrastructure diagnostic. |
| Refusal with `-o ndjson` | Available refusal trace events, error event, exit event, plus the stderr infrastructure diagnostic. |
| Exec stream ends before observed exit | Exit 255 and `process.outcome_unknown` diagnostic. JSON includes partial output and error. NDJSON emits error without an invented exit event. |
| Second termination signal handled by relay | Exit 128 plus signal number. |

NDJSON writes one compact object per event on stdout.
Started events include process ID, executor, PID, and argv.
Output chunks use `data` for UTF-8 or `data_base64` for non-UTF-8 bytes.
Trace events use their event kind as `type`. Exit events contain code and signal.
Final JSON decodes captured stdout/stderr with lossy UTF-8 conversion.
Infrastructure JSON diagnostics remain a separate stderr schema.

Run selectors accept ID, unique prefix, name, or `latest`.
Most run operations default to the inherited run, then the only live run.
Reports instead default to the inherited run, then latest, and support offline lookup in the local trace database.
An ambiguous or missing selection produces an infrastructure error rather than a selection prompt.

## Files and images matrix

Sources: [file copy](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/fs.rs),
[image pull](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/image/pull.rs),
[image listing](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/image/ls.rs),
[image build](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/image/build.rs),
[OCI conversion](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/image/build/from.rs),
and [image removal](https://github.com/needy-dev/needy/blob/8a5c79399d4c46a4a0b3a5fa0fd524666f6add1c/crates/cli/src/commands/image/rm.rs).

| Command | Successful stdout | Stderr, prompts, progress, and conditions | Machine-readable output and exit behavior |
| --- | --- | --- | --- |
| `fs cp SOURCE DESTINATION` | None. | No prompt, progress, or success confirmation by default. `-v` shows copied byte count and target. Requires one local path, one literal `remote:PATH`, and one explicit target. Tar/path errors name the target/path. Unsafe downloaded archives fail. | No successful structured output. Exit 0 after transfer/extraction. |
| `image pull` | None. | Plain `needy:` messages describe detected drivers, download URL, periodic MiB progress, checksum verification, destination, container pull/layer progress, and final driver results. Local import reports checksum status and destination. | No successful structured format. Per-driver failures print messages and return 1 after remaining pulls. Early setup/import errors use shared diagnostics. |
| `image ls` | `DRIVER IMAGE VERSION BASE ABI SOURCE SIZE PATH` rows for local VM and available container stores. Missing fields use `-`, size uses binary units. Invalid/missing manifests appear in SOURCE. | Unavailable container-store reasons appear only in info logs with verbosity. VM listing skips unreadable/missing directories and some entries. | `-o json`: complete image rows with numeric bytes and optional path/version/ABI. A partial or empty inventory can still exit 0. |
| `image build` | None from the handler. | BuildKit step-start messages, recipe findings, optional OCI pull notice, optional Apple store load notice, and VM destination success message. Debug verbosity adds build log lines. A failed BuildKit step includes buffered tail output in its diagnostic. | No successful structured format. Recipe error findings, missing checkout/runtime, build/conversion/ABI failures use shared diagnostics and exit 1. Container-only additions have step output without a uniform final summary. |
| `image rm [IMAGE...]` | None. | Plain stderr removal messages per VM image, container store, and obsolete guest disk. Missing images print a message, including `image ls` advice for VM misses. No confirmation prompt. | No successful structured format. Missing targets accumulate exit 1. `--guest-disks` with nothing obsolete can succeed silently. Filesystem errors use shared diagnostics. |

Image pull reports progress approximately every additional 32 MiB for VM downloads.
Container progress includes layer status and available current/total byte counts.
Import without an adjacent checksum file prints the calculated SHA-256 and states that no checksum file exists.
These notices describe different integrity evidence and do not establish release-attestation verification.

The file-copy help describes an `EXECUTOR:PATH` form, while the implementation recognizes only the literal `remote:` marker.
That discrepancy is a source-backed documentation gap, separate from the onboarding slice.

## Prioritized gaps and proposed follow-up

This list records presentation gaps. It does not authorize implementation outside the owner's named slice.
Silence on successful mutations is existing behavior, not automatically a bug.

| Priority | Gap and consequence | Status and bounded next slice |
| --- | --- | --- |
| P0 | Pair/set local `--address` overlaps the global parser field. Help alone does not establish that direct pairing works. | **Active approved slice:** repair and reproduce the parser behavior for pairing/transport updates. |
| P1 | `client invite` exposes a code without client-side pairing guidance. Pairing starts with an unexplained hidden prompt and ends with one stderr line. | **Active approved slice:** invitation, prompt, save outcome, and next-step guidance. Keep secret input separate from argv. |
| P1 | Context check combines transport, instance, caller, and failure into one finding cell. Service reachability and authenticated identity are hard to distinguish. | **Active approved slice:** make the check result and its limits clear. Preserve honest unknown outcomes. |
| P1 | Infrastructure human errors lack semantic colors, despite an existing colored enrollment success line. | **Active approved slice:** terminal-aware diagnostic colors and explicit plain redirected/`NO_COLOR` behavior. Child bytes retain their own styling. |
| P1 | `config profile check` accepts a positional profile but rejects the `-f` file spelling used by `run create`. | **Active approved slice:** accept `-f/--file`, retain positional input, reject both together. |
| P1 | A foreground run viewer returns on transport failure without reconnecting. Runner/holder backoff does not reconnect that viewer. | **Open recovery slice:** reconnect only the existing run attachment; never repeat create, process start, or buffered input. The reported h2 failure has not been reproduced. |
| P2 | `fs cp` help names an executor prefix that the parser does not recognize. | **Proposed docs-only slice:** align command help with literal `remote:PATH` and explicit target flags. |
| P2 | Many mutations have no success confirmation, while chaos operations and enrollment provide explicit outcomes. | **Proposed contract slice:** choose one family at a time and specify interactive confirmation versus stable automation output. Start only after owner selection. |
| P2 | Successful service inspection can show failed/unreachable/not-installed state with exit 0. Image listing can silently omit unavailable stores. | **Proposed documentation slice:** explain observation success versus healthy/complete inventory. A stricter exit mode requires a separate approved contract. |
| P2 | Several progress/warning paths remain plain text under `--diagnostic-format json`. Image pull can return 1 through handled text errors. | **Proposed contract slice:** define whether automation needs structured progress and handled errors for one command family. The existing diagnostic flag covers infrastructure errors only. |
| P3 | Suggested commands vary between native manager commands, generic commands, and context-preserving diagnostic argv. | **Proposed review slice:** inspect one onboarding/lifecycle flow for correct machine location and retained selection. Do not expand every hint in this audit. |
| P3 | Human and structured profile checks carry different recipe/identity detail. Some list schemas expose more than their tables. | **Proposed documentation slice:** document the selected machine-output fields before considering schema additions. |

## Remaining verification limits

The help probe establishes command discovery and advertised flags on one installed v0.0.5 binary.
The source audit establishes intended branches at the pinned commit.
No cross-platform runtime matrix, external service-manager transcript, full TTY session, or remote failure matrix ran for this audit.
Prompt placement follows the password library call and explicit stderr guidance in source, without a captured interactive session.
The exact terminal/log bytes and remotely supplied messages depend on the actual child, endpoint, manager, and runtime state.
Active onboarding changes require their own tests and evidence before this historical baseline can describe the resulting release.
