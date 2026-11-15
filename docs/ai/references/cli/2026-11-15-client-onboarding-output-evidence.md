# Client onboarding output changes

Date: 2026-11-15. Change classification: bug fix and owner-requested CLI presentation changes.

This note describes the working branch after the v0.0.5 baseline.
It does not describe the installed v0.0.5 binary.
The [complete command output audit](2026-11-15-cli-output-audit.md) records all 55 public operational commands at that baseline.

## Before and after

| Surface | v0.0.5 | Working branch |
| --- | --- | --- |
| `needy --address tcp://127.0.0.1:7400 config context pair control` | Parser rejects the provided address and asks for address/connector arguments. | Root address works before or after the subcommand. The duplicate local argument is removed. |
| `config context set NAME` | The same address shadowing applies. | Uses the same global address flag as pairing. |
| `client invite` | Unaligned metadata and code, without pairing instructions. | A copyable client-machine command includes `--token`; the invitation is shown once. JSON `.code` stays unchanged. |
| Pair input | Hidden prompt or private file. | Accepts one-time `--token TOKEN`, private file, stdin, or the hidden prompt. Token and file input conflict before consuming an invitation. |
| `config context pair --code-file -` | Attempts to open a file named `-`. | Reads one code from stdin. Private file permission checks remain in effect for other paths. |
| Pair completion | Stderr message starts with `needy:` and says selection is unchanged. | Green `Paired successfully!`, followed by `Updated:` and the actual configuration file on stderr. Stdout stays empty; no secret or identity metadata is echoed. |
| `config context check` | One generic row combines address, role, instance, caller and numeric access in `finding`. | Separate `NAME ADDRESS STATUS SERVICE INSTANCE CLIENT CLIENT ROLE` columns. There are two client roles, `runs` and `admin`; `unknown` is an observation limit. |
| Infrastructure errors | Plain `needy:` label. | Bold red `error:` label on terminal stderr. Redirected stderr and `NO_COLOR` stay plain. JSON diagnostics retain their schema. |
| `config profile check -f FILE` | Parser rejects the file flag. | Accepts `-f/--file` alongside the existing positional spelling. Providing both is an error. |

Pairing still saves a connection without selecting it.
It does not grant additional access, enroll a runner, or start an executor.
Invitations keep their existing TTL, single-use semantics, credential format, and authorization checks.

The health `instance` value identifies one service lifetime.
The control daemon generates it on startup; it is not a client ID, actor ID, or permanent machine name.
Source: [`Daemon::start`](../../../../crates/daemon/src/lib.rs).

## Loading and completion feedback

Scoped indicatif renderers replace repeated progress lines on an interactive terminal.
Each guard clears on success, failure, or cancellation before subsequent output.
They do not cover child execution or persistent serving.

Implemented waiting boundaries include:

- Client invitations, pairing, connection checks, and remote registry operations.
- Run creation, selection/attachment setup, stopping, and log acquisition.
- Process stream opening, listing, and signal requests; control-mode profile checks.
- File transfers and runner enrollment/management.
- Service status, readiness, and verified stop polling.
- Image downloads, verification, extraction, installation, and reported build stages.

Container pulls use stable per-layer rows and real byte totals when available.
VM downloads use HTTP byte totals or an indeterminate spinner.
Existing redirected image progress text and empty stdout remain intact.
Default terminal image completion shows the actual image reference or installed directory.

Animation is disabled for redirected/dumb terminals, nonempty `NO_COLOR`, JSON diagnostics, and verbose stderr logs.
Command connectors retain ownership of stderr for prompts and diagnostics.
Bare mutation confirmations use stderr so that scripts can still compose stdout as data.
Existing invitation, enrollment, service-installation, ID, JSON/NDJSON, and child-output contracts remain separately documented.

## Structured-output changes

`client invite -o json` retains its existing object and `.code` field.
Pair confirmation has no JSON mode, as before.

`config context check -o json` changes its array elements from generic
`kind/name/source/finding` entries to typed check results:

```json
[
  {
    "name": "control",
    "address": "tcp://127.0.0.1:7400",
    "status": "ok",
    "role": "control",
    "instance": "example-instance",
    "client": "example-client",
    "client_id": "example-client-id",
    "access": "runs",
    "error": null
  }
]
```

Scripts must read these fields instead of parsing `finding`.
Failed probes emit `status: error`, unknown facts as null, and an error description.
They exit unsuccessfully and also use the selected stderr diagnostic format.
The JSON error envelope and Protobuf messages do not change.
Human `SERVICE` and `CLIENT ROLE` headings do not rename JSON or persisted `role`/`access` fields.
The new token flag changes invitation input only; TTL, single use, and authority remain server-enforced.

## Verification

Evidence level: automated CLI integration tests with temporary homes, a local test daemon, and real PTYs.
No live installation, remote service, project profile, or user connection was changed.

The initial parsing and output regressions were observed failing before their fixes.
The current tests include:

- `client_pair_accepts_the_guides_global_address_and_reports_the_written_file`
- `client_invite_shows_pairing_instructions_and_stdin_pairing_does_not_echo_secrets`
- `context_check_reports_service_and_identity_as_separate_fields`
- `infrastructure_errors_use_a_red_error_label_only_on_a_terminal`
- `profile_check_accepts_a_file_flag_and_keeps_positional_profiles`
- `pairing_reports_an_explicit_configuration_file`
- `a_failed_context_check_names_its_actual_target_in_json_diagnostics`
- `the_invitation_command_pairs_with_a_one_time_token_argument`
- `a_token_argument_conflicts_with_file_input_before_consuming_the_invitation`
- `pairing_does_not_animate_over_verbose_logs_or_connector_stderr`
- `command_execution_leaves_connector_stderr_free_of_progress`

The pairing test covers both address positions, authenticated use, context transport updates, and terminal success styling.
The stdin test checks that explicit code input does not open a prompt or echo the invitation.
The profile test compares structured results for positional, short-file, and long-file inputs, including saved named profiles.
Pair completion names the actual selected configuration file, including paths with spaces, while preserving the saved default selection.
Failed check diagnostics retain the checked context even when the saved default points at a different healthy service.

Final automated results on macOS:

- `cargo test --locked --workspace --no-fail-fast -- --test-threads=4`: 1107 passed, 0 failed.
- The CLI integration target contains 120 passing tests.
- CLI Clippy with all targets and `-D warnings`: passed.
- Formatting and whitespace checks: passed.
- The pairing and error-label tests also pass with `NO_COLOR=1`.
- In-memory terminal tests cover spinner animation, updates, cleanup, image layers, byte totals, and error cleanup.
- Existing image stream fixtures still verify progress before completion, failure propagation, and the executor pull after actor-pull failure.

The existing execution-selection test caught stdout contamination from a newly added configuration confirmation.
Mutation acknowledgements now use stderr; the unchanged child-JSON assertion passes.
No hardware-backed actor, container image, native service-manager operation, or real image build was run for these UI changes.

An earlier default-parallel run failed the unchanged
`process::tests::a_terminal_process_sees_a_terminal_and_its_signal_death` test because it collected empty PTY output.
The isolated test and final workspace run passed. This is an intermittent-test follow-up candidate; no executor implementation was changed.

## Open work

Persistent runner serving has no connection-state callback for a scoped waiting indicator.
Control/executor listener readiness and additional transcript wording remain explicit presentation proposals.
The code does not invent a connected/ready outcome to complete an animation.
The foreground run viewer still has no automatic reattachment after a transport failure.
An existing run can be attached again explicitly. Runner/holder backoff is a separate mechanism.
The reported h2 connection-body failure has not been reproduced by this onboarding test suite.
Recovery must target the existing attachment and must not repeat a run creation, process start, or previously sent input.
