# CLI installation and enrollment feedback

Scope: a bug fix for Docker-compatible image pull progress, plus the
owner-requested service installation and runner onboarding output, stdin token
input, and prefix-free runner invitations. Service selection, runtime
permissions, and privilege elevation behavior stay as implemented.

## Image pull

The engine collected and discarded all `create_image` updates. The CLI printed
only the initial image reference until the pull finished.

`Engine::pull_progress` streams the engine's typed updates to the CLI. The CLI
prints the layer, status, and current/total bytes on stderr during both actor
and executor pulls. Cached-layer and completion statuses remain visible.
An engine stream error still fails the pull. The actor base ABI check remains
in place, and an actor pull failure does not skip the executor pull.

`Engine::pull` consumes the same stream for callers that need only completion,
such as automatic driver pulls. It no longer buffers every update in a vector.

## Local target and service scopes

`local` is the built-in default context. It discovers the private management
endpoint on this machine; no saved remote connection or pairing creates it.
An additional TCP listener does not redefine the target.

The service scope and runtime identity are independent:

| Choice | Definition on Linux | Manager command | Runtime account |
| --- | --- | --- | --- |
| Default user scope | `~/.config/systemd/user/needy.service` | `systemctl --user` | Installing user, including root |
| `--scope system --run-as USER` | `/etc/systemd/system/needy.service` | `systemctl` | Explicit `USER` |

A system installation requires root, but its runtime account can be non-root.
The binary and its parent directories must be accessible to that account.
The installer does not create accounts, elevate privileges, grant Docker/KVM
access, or enable linger.

The current Firecracker driver uses vsock, with no TAP interface in its VM
configuration. It checks read/write access to `/dev/kvm`. Docker-compatible
drivers use the configured engine endpoint and its permissions. A non-root
needy process using a rootful Docker socket still has that socket's authority.

References:

- [systemd 256: user and system managers](https://www.freedesktop.org/software/systemd/man/256/systemd.html)
- [systemd 256: runtime users](https://www.freedesktop.org/software/systemd/man/256/systemd.exec.html#User=)
- [systemd 256: linger](https://www.freedesktop.org/software/systemd/man/256/loginctl.html#enable-linger%20%5BUSER%E2%80%A6%5D)
- [Docker socket permissions](https://docs.docker.com/engine/install/linux-postinstall/)
- [Firecracker v1.17.0 KVM requirements](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/getting-started.md#kvm)

## Installation output

The installer prints the detected manager, selected scope, and runtime account.
It identifies each configuration snapshot, service definition, and registration
file before writing it. It prints native status/log commands before enabling
and starting the service, so they remain available if startup fails.

The scope determines the native command: user systemd services include
`--user`; system services do not. launchd hints include the user or system
domain and the `dev.needy.control` label. Log paths use shell quoting.

The snapshot contains daemon settings and resolved runtime paths, rather than
saved remote credentials. User installations keep snapshots and the registration
under the configuration directory's `services/user/`. System installations use
`/var/lib/needy/service` for snapshots on Linux and keep the registration under
`services/system/` in the installing client's configuration directory.

## Runner onboarding

The human invitation output groups aligned metadata, a runner-side command,
and the token shown once. The control daemon cannot infer the address reachable
from a different machine, so the command names `<control-address>` explicitly.
The joining machine can use a direct address or its local SSH tunnel endpoint.
The CLI uses `--address`; retired `--endpoint` and token-valued argv flags are
not reintroduced.

Interactive joining explains the control-host command before the hidden
`Joining token:` prompt. It includes the requested role in that command.
`--code-file FILE` reads a private file; `--code-file -` reads one token from
stdin and skips the prompt. Multiline input is refused. For example, on the
control host, with a tunnel already available on the runner machine:

```sh
needy runner invite --roles executor -o json | jq -r '.code' | \
  ssh user@example.com 'needy --address tcp://127.0.0.1:7400 runner join example-runner --roles executor --code-file -'
```

`--output json` and `-o json` retain the existing JSON schema, including `.code`.
Human instructions do not enter the JSON stream.

Successful joining prints a green confirmation on terminals, with the runner
name, control transport, and local capabilities. Redirected output stays plain
text; `NO_COLOR` disables styling. Connector transports are labeled without
printing their argv. The joining token and saved credential are not echoed.

Joining only enrolls and saves credentials. The output names `needy runner serve`
as the next step on the runner, `needy runner ls` on the control host, and the
control service's status/log commands. It does not claim a persistent work
connection exists before serving starts. This follows kubeadm's approach of
showing the next machine's joining steps; see the
[Kubernetes v1.35 setup guide](https://v1-35.docs.kubernetes.io/docs/setup/production-environment/tools/kubeadm/create-cluster-kubeadm/).

### Invitation format boundary

New runner invitations use `<id>.<secret>`, without `needy-join.`. The row ID is
unchanged, and the secret still contains 32 random bytes encoded as 64 hex digits.
The database still stores only the hash of the complete code. Expiry, role
bounds, revocation, single-use consumption, and constant-time hash comparison
are unchanged.

The reader accepts the prefixed public invitation format issued through v0.0.4,
recorded in the [released implementation](https://github.com/needy-dev/needy/blob/v0.0.4/crates/daemon/src/store/secret.rs).
It checks the hash of the original complete string and applies the same usage
limits. This is explicit read compatibility for already-issued invitations;
no database rewrite or prefix reconstruction occurs. Retiring that reader needs
an owner-approved end to support for those issued invitations. Saved runner and
client credentials and client pairing codes keep their existing formats.

## Verification

Evidence level: automated CLI integration and unit tests against temporary
Docker HTTP and OS-manager process fixtures. No live service is installed and
no real image is pulled by these checks.

- `image_pull_shows_layer_progress_before_the_download_finishes` holds the
  engine response open until progress appears on the CLI's stderr. It also
  checks cached layers, byte counts, and actor/executor completion output.
- `image_pull_keeps_progress_and_fails_on_an_engine_stream_error` checks live
  progress followed by an error, a failing exit code, and the executor attempt.
- Both progress tests failed against the previous buffering implementation.
- `service_install_snapshots_settings_without_starting_and_refuses_foreign_definition`
  checks the written artifacts and the installation output against a stand-in
  OS manager. Its new output checks failed against the previous implementation.
- Native command tests cover user/system systemd and launchd hints, including
  a quoted log path containing spaces.

- Runner onboarding tests cover aligned human instructions, pure JSON token
  extraction, stdin joining without a prompt, saved enrollment, status/serve
  guidance, and terminal-only green confirmation.
- Store tests cover prefix-free issuance, the original hash-only storage and
  entropy, and acceptance/use consumption of an invitation issued by v0.0.4.

Final automated results on a macOS host:

- `cargo test --locked --workspace --no-fail-fast`: 1,038 passed, 0 failed.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

Builds used an isolated temporary Cargo target directory. Tests used temporary
state/configuration directories and stand-in OS manager commands; no installed
binary, live service, or real image was added to the cleaned environments.
The lifecycle fixtures use canonical executable identity and wait for private
discovery publication before issuing local-context requests.
