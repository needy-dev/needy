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
| `client invite` | Unaligned metadata and code, without pairing instructions. | Aligned metadata, client-machine command, reachable-address guidance, selection/check commands, and file/stdin guidance. |
| Interactive pair input | Only a hidden code prompt. | Explains how the operator issues a client invitation and how to automate input. |
| `config context pair --code-file -` | Attempts to open a file named `-`. | Reads one code from stdin. Private file permission checks remain in effect for other paths. |
| Pair completion | Stderr message starts with `needy:` and says selection is unchanged. | Green `Paired successfully!` on a terminal, plain success when redirected. Shows saved context, client ID, actual current context, and selection/check commands. No secret is echoed. |
| `config context check` | One generic row combines address, role, instance, caller and numeric access in `finding`. | Separate `NAME ADDRESS STATUS ROLE INSTANCE CLIENT ACCESS` columns. Access names are `runs`, `admin`, or `unknown`. |
| Infrastructure errors | Plain `needy:` label. | Bold red `error:` label on terminal stderr. Redirected stderr and `NO_COLOR` stay plain. JSON diagnostics retain their schema. |
| `config profile check -f FILE` | Parser rejects the file flag. | Accepts `-f/--file` alongside the existing positional spelling. Providing both is an error. |

Pairing still saves a connection without selecting it.
It does not grant additional access, enroll a runner, or start an executor.
Invitations keep their existing TTL, single-use semantics, credential format, and authorization checks.

The health `instance` value identifies one service lifetime.
The control daemon generates it on startup; it is not a client ID, actor ID, or permanent machine name.
Source: [`Daemon::start`](../../../../crates/daemon/src/lib.rs).

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

## Verification

Evidence level: automated CLI integration tests with temporary homes, a local test daemon, and real PTYs.
No live installation, remote service, project profile, or user connection was changed.

The following regressions failed before their production fixes:

- `client_pair_accepts_the_guides_global_address_and_explains_the_next_step`
- `client_invite_shows_pairing_instructions_and_stdin_pairing_does_not_echo_secrets`
- `context_check_reports_service_and_identity_as_separate_fields`
- `infrastructure_errors_use_a_red_error_label_only_on_a_terminal`
- `profile_check_accepts_a_file_flag_and_keeps_positional_profiles`
- `pairing_next_steps_preserve_an_explicit_configuration_file`
- `a_failed_context_check_names_its_actual_target_in_json_diagnostics`

The pairing test covers both address positions, authenticated use, context transport updates, and terminal success styling.
The stdin test checks that explicit code input does not open a prompt or echo the invitation.
The profile test compares structured results for positional, short-file, and long-file inputs, including saved named profiles.
Printed pairing hints retain an explicitly selected configuration file, including paths with spaces.
Failed check diagnostics retain the checked context even when the saved default points at a different healthy service.

Final automated results on macOS:

- `cargo test --locked --workspace --no-fail-fast -- --test-threads=4`: 1097 passed, 0 failed.
- The CLI integration target contains 116 passing tests.
- CLI Clippy with all targets and `-D warnings`: passed.
- Formatting and whitespace checks: passed.
- The pairing and error-label tests also passed with `NO_COLOR=1`.

An earlier default-parallel run failed the unchanged
`process::tests::a_terminal_process_sees_a_terminal_and_its_signal_death` test because it collected empty PTY output.
The isolated test and final workspace run passed. This is an intermittent-test follow-up candidate; no executor implementation was changed.

## Open work

Docker-style image progress is an accepted presentation direction; this branch does not change image pull.
The foreground run viewer still has no automatic reattachment after a transport failure.
An existing run can be attached again explicitly. Runner/holder backoff is a separate mechanism.
The reported h2 connection-body failure has not been reproduced by this onboarding test suite.
Recovery must target the existing attachment and must not repeat a run creation, process start, or previously sent input.
