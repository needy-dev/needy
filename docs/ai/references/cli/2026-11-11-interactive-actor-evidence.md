# Interactive actor: implementation and evidence

Status: verified, 2026-11-11. This completes the approved slice 8 in
[Runners and runs](../architecture/2026-11-10-runner-and-run-design.md).

## Contract

- `run create -t` gives the driving program a PTY and returns its run ID.
- `run create -it` attaches immediately and returns the program's exit code.
- `run attach RUN` redraws the current screen, forwards input and window
  changes, and returns the program's exit code.
- The default detach keys are `ctrl-]`. Detaching keeps the run alive;
  `run attach --read-only` watches without sending input, signals or resize.
- Init keeps reading the PTY into the serial console independently of
  clients, preserving `run logs` while detached.

## Implementation and regressions

The resumed tree did not compile: the CLI had not supplied `ExecAttach.run_id`
or `CreateRunRequest.tty`. A Linux build also found the proxy still calling
the previous relay API and sshfs missing the new `Detached` event. Both guest
consumers now use the current relay contract. The proxy leaves detach-key
handling to the outer run client.

Two daemon regressions reproduced a run's terminal attach outliving its run,
and a stopped run leaving a vsock handshake pending. Each failed with a
two-second timeout before the fix. Attaches now belong to the run's task
tracker, and cancellation/deadline cover the connection handshake itself.

A real VM revealed a third race: `sh -c "printf 'short-run\\n'; exit 9"`
could exit and reboot before `-it` subscribed. The run recorded exit 9, but
the client waited for a terminal that was gone. The Linux regression failed
against the pre-fix guest with a 15-second timeout, then passed with output
`short-run` and exit 9 against the rebuilt guest in 0.37 s.

For an immediately attached create, init's child now waits on a separate
local socket. Init releases it after the first terminal subscription is
installed. Input stays on the PTY. On exit, init lets existing gRPC streams
drain for up to three seconds before rebooting; an abandoned viewer cannot
keep the VM alive indefinitely. A `-t` create without `-i` never waits for
a client. Rebuild host binaries and guest images together.

## Automated evidence

The focused cases are in
[`relay/mod_test.rs`](../../../../crates/daemon/src/relay/mod_test.rs),
[`domain_test.rs`](../../../../crates/api-server/src/protocol/domain_test.rs)
and [`cli.rs`](../../../../crates/cli/tests/cli.rs).

| Case | Evidence level | Result |
| --- | --- | --- |
| `run_create_on_a_terminal_can_be_attached_later` | CLI test, stand-in VM | Pass: the run's JSON carries `tty: true` |
| `run_attach_explains_when_the_driving_program_has_no_terminal` | CLI test, stand-in VM | Pass: identifies the missing terminal and suggests `run create -t` |
| `run_create_interactive_requires_a_terminal` | CLI test | Pass: `-i` without `-t` is refused |
| `waiting_for_a_terminal_client_requires_a_terminal` | API conversion test | Pass: a malformed wire request is refused |
| `an_interactive_create_tells_the_guest_to_wait_for_its_client` | Daemon trait test, stand-in VM | Pass: the guest gets terminal size, start synchronization and its process port |
| `attaching_to_a_runs_driving_program_forwards_input_and_its_exit` | Daemon API, real PTY and Unix transport standing in for vsock | Pass: input produces `received:hello`, exit 7 |
| `stopping_a_run_ends_its_driving_programs_attach` | Daemon API, real PTY | Pass after reproducing the timeout: the stream ends with `run stopped` |
| `stopping_a_run_interrupts_an_unanswered_terminal_handshake` | Daemon API, unanswered Unix peer | Pass after reproducing the timeout: stop interrupts connection setup |
| `an_immediately_exiting_interactive_actor_returns_its_output_and_exit` | CLI regression, real Linux KVM VM | Pass after failing against the pre-fix guest: preserves output and exit 9 |

The real-VM regression is intentionally excluded from the ordinary suite;
run it on a Linux KVM host against an image built from this source:

```sh
NEEDY_TEST_ACTOR_IMAGE=/home/user/needy-image \
  cargo test --locked -p needy-cli --test cli \
  an_immediately_exiting_interactive_actor_returns_its_output_and_exit \
  -- --ignored --nocapture
```

## Live probes

Environment: a Mac client, SSH local forwarding only, an isolated control
daemon on a Linux KVM host, and a new actor image from the working source.
The probe drove the real CLI through local pseudo-terminals; the driving
program was Bash. These are live probes of this setup, not a claim about
every terminal emulator or arbitrary TUI.
This round did not re-test a Windows executor or other Linux deployment hosts.

| Journey | Observed result |
| --- | --- |
| `run create -it`, send a command, inspect `stty size` | Command output received; initial size 27 rows by 93 columns |
| Default `ctrl-]`, then `run attach` | Client returns 0, run stays alive, previous screen is redrawn |
| Resize the active client | `stty size` becomes 33 by 101 |
| Draw an ANSI alternate screen, detach, reattach | The held full-screen marker is restored |
| Attach a read-only viewer, send input and resize it | Viewer sees live output; its input has no effect and active size stays 33 by 101 |
| Detach every client while a background command writes | `run logs` contains the new output |
| Reattach and `exit 7` | Client returns 7 |
| Immediate program from `run create -it` | Output retained, client returns 9 |
| `run create -t` with an immediate program and no viewer | Program finishes with exit 6 and its output appears in `run logs` |

Raw local probe output is retained under `.tmp/interactive-actor/`; remote
build and test artifacts remain in the isolated scratch tree. Test daemons,
VMs and the SSH forward are stopped after verification.

## Quality checks and boundaries

Formatting and workspace Clippy pass on the Mac. Workspace Clippy and the
full workspace test suite pass on Linux; the Mac workspace test suite also
passes (544 tests on the Mac; 552 on Linux). Linux's ordinary suite excludes
the real-VM case, which passed separately as recorded above. Buf lint,
formatting, generation and breaking checks against `main`
pass. The Linux run initially encountered AppleDouble metadata introduced
by copying the source from macOS; moving that transfer metadata out of the
source tree removed the fixture error without changing product code.

No container actor runtime, remote actor scheduling or terminal-output trace
recording is added. The existing `TODO(run-ux)` retains the explicit `create`
subcommand; re-opening the shorthand needs an owner-approved parsing grammar.
Existing `state.db` records do not preserve the terminal flag for ended runs;
the code marks that boundary, and ended actors cannot be attached.
