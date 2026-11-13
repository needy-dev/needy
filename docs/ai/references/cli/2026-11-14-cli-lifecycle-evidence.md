# CLI lifecycle implementation and evidence

Date: 2026-11-14. Evidence level: CLI integration, daemon API-trait regression
tests, non-root macOS and Linux user-service e2e, and Linux Firecracker e2e.
This note records the current implementation. Linux and system-scope acceptance
are listed separately below.

## Implemented behavior

The control daemon keeps its private management socket alongside additional
listeners. It publishes a private `control.json` descriptor after reconciliation
and a successful health call for its control role and process instance.
An exclusive publication lock rejects a competing daemon. Shutdown removes
only the owning instance's descriptor and releases its lock.

The `daemon` command separates foreground `serve`, standalone `executor`, and
local `service` operations. The service boundary selects an independent systemd
or launchd adapter. Installation snapshots daemon configuration, paths, PATH,
and an explicitly configured engine address. The snapshot contains no saved
client contexts or their credentials.

Service readiness compares manager PID, descriptor PID, health role and instance.
It rechecks manager state and the descriptor after the probe. Service status
reports authenticated admission as unknown when no authenticated check occurred.
A healthy manual daemon cannot satisfy readiness for a different manager PID.

Controlled shutdown refuses new work, lets existing run creation unwind,
and uses the normal graceful-stop/mirror-out contract. Run cleanup, listener
drain and executor cleanup share a finite 60-second budget. A second signal
still forces exit. An incomplete cleanup produces a nonzero serving result.

Runner enrollment and serving are separate operations. Enrollment saves its
transport, independent credential, registered name, role bounds, labels and
image override. Serving re-detects capabilities within those bounds. Explicit
label changes persist only after the control plane accepts them.

## Executed checks

| Command or seam | Observed result | Limit |
| --- | --- | --- |
| `cargo test -p needy-cli --test lifecycle` | Three tests passed: private management survives network serving; competing publication is refused; graceful exit removes discovery; installation snapshots configuration without starting; a foreign definition survives rejected replacement; a healthy manual daemon cannot satisfy service readiness for a different manager PID | Real CLI processes; manager installation boundary uses a stand-in manager executable |
| `cargo test -p needy-daemon daemon_shutdown_waits_for_the_actors_mirror_out` | Passed; the guest pushed its archive before actor exit and the host received it | In-process daemon traits and stand-in actor; the same regression failed against immediate stop |
| `cargo test -p needy-daemon a_runs_life_is_written_to_state_db_and_listed_after_a_restart` | Passed after publication-lock release moved into shutdown | Restart through the existing API-trait seam |
| `cargo xtask e2e --services --only none/service/user --no-prepare --needy target/debug/needy -o json` | One cell passed with ten checks on a non-root Apple-silicon macOS host; repeated after configuration/discovery changes | User launchd only; no real actor or provider login |
| Docs tests, type check and build | 254 tests passed; type check and production build passed | Existing large bundle warning remains; no new visual acceptance |

The service cell first checked that the canonical user definition and loaded
job were absent. It then installed without starting, started, checked readiness
and logs, stopped twice, restarted, and uninstalled. The restart had a new
instance ID. Configuration and state survived uninstall. The harness found
no remaining cell processes, sockets, mounts or containers.

`--services` is opt-in. A busy canonical service or unavailable manager reports
not covered and prevents installation. The cell's cleanup guard uses only the
owned registration it created. Normal actor cells also check descriptor
privacy, child PID/health identity and descriptor removal during shutdown.

## Remaining acceptance

Linux user systemd execution passed in the remote acceptance below. Both
platforms' system-scope installations still need dedicated manager/account fixtures. The implementation does not change
linger, elevate privileges, or select a runtime account implicitly.
System scope requires `--run-as USER`.

The user-service cell does not prove mirror-out from a real VM during a manager
stop. The daemon regression proves the cleanup contract with a stand-in actor.
Real actors still require the existing non-root e2e matrix on suitable hosts.
Windows control hosting and service installation remain unsupported and fail
before installation writes. This work does not extend Windows credential-file
ACL acceptance.

## Platform references

The systemd adapter uses [systemctl 256](https://www.freedesktop.org/software/systemd/man/256/systemctl.html).
The launchd adapter uses Apple's [archived daemon and agent guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html).
The local `launchctl print DOMAIN` inspection established the PID/status/label
row format. `launchctl print gui/UID/dev.needy.control` returned exit 113 for
an absent service on the acceptance host on 2026-11-14. The harness treats
other lookup failures as unavailable, rather than proof of absence.

## Continued implementation checks

The owner removed the legacy compatibility requirement on 2026-11-14.
The CLI no longer imports `credentials.json` or consumes `NEEDY_RUN`.
Unspecified access grants are rejected. Database schema upgrades preserve state
with runs-only access for existing client records.

A focused two-axis review found four issues: recovery commands could lose their
selected connection; connector failures lost their typed reason; local operations
consumed inherited execution selection; and service publication lacked a
transaction lock. The implementation now preserves selection in action argv,
retains connector reason/program/exit facts, resolves inheritance only at the
connection boundary, and locks service mutations across ownership inspection,
manager actions and publication. A follow-up review identified the lock's
receipt-existence race and a missing fault-reason selection branch; both were
corrected before remote acceptance.

| Check | Observed result | Limit |
| --- | --- | --- |
| `cargo test --locked --workspace` | 1,029 passed, none failed or ignored | Local macOS; test binaries include stand-in actor/API/manager boundaries |
| Workspace Clippy, all targets with warnings denied | Passed | Native platform compilation |
| Buf lint, format, generation and FILE breaking check against the parent main revision | Passed | Wire compatibility check, not runtime behavior |
| Frontend tests/typecheck/build | 254 passed; typecheck/build passed | Existing bundle-size warning; no new visual acceptance |
| CLI configuration regressions | `.yml` lookup, duplicate-name rejection, typed mutation failure, redacted YAML numeric types, retired-file non-import and local inheritance isolation passed | Isolated temporary directories |
| CLI execution regressions | Exact argv, colon-containing copy paths, completed child exit 255 versus unknown outcome, explicit legacy-selector rejection and stable inherited connection passed | Native host plus in-process/stand-in peers |
| Service identity regressions | Foreign loaded job rejected; competing installation and stop rejected while the first installation holds its lock | Recorded OS-manager executable boundary; no foreign live service was changed |

The image checksum regression proves that the failed artifact is not installed
and a previously installed base remains intact. Typed image ABI and architecture
errors now retain their producer facts. This does not attest downloaded images
beyond the existing SHA-256 contract.

Automated Windows enrollment is reported as not covered before remote preparation:
private code-file reads require the deferred Windows ACL boundary. Native Windows
runner implementation remains present. No relaxed file permissions or legacy
join-token path is used to bypass that limitation.

## Remote Linux acceptance

A non-root Linux x86_64 KVM host built commit `d2d2646` through the existing
`cargo xtask e2e --remote` source-shipping boundary. Docker built the guest
binary and actor/executor images from that source. Six selected cells passed
with 180 checks and no failed or uncovered cells:

- `none/exec` and `none/fs-cp`: command output/exits and explicit file targets.
- `none/service/user`: actual systemd install, start, readiness, logs, stop,
  restart, uninstall and retained configuration/state.
- `linux-firecracker/routing`: a real actor and routed child execution.
- `linux-firecracker/actor-files/end=stop`: copy-in and graceful mirror-out.
- `linux-firecracker/actor-files/end=stop-now`: immediate-stop behavior,
  including its intentional lack of graceful mirror-out.

The matching macOS run passed three cells with 53 checks: host exec, file copy
and user launchd lifecycle. These are host-role and hardware observations;
private host identifiers, addresses and raw logs are not part of this note.

A final regression extended the service transaction test to two distinct
configuration directories targeting the same canonical manager definition.
It reproduced a race after the first implementation commit. The fix puts the
lock next to that canonical definition so all configuration sources serialize
on the same inode. The inode is retained after uninstall to prevent lock
replacement races. The CLI manager-boundary regressions and actual macOS
launchd lifecycle were rerun after this fix.

These cells do not establish system-scope service support, Windows credential
ACLs, authenticated model calls, or a live managed-service stop with an active
VM mirror-out. Controlled shutdown with mirror-out has separate daemon-trait
regression evidence; the Linux real-VM graceful run-stop cell proves the actor
file path. Those observations remain distinct.

The final service-lock implementation at commit `58f8d69` was then rebuilt
and rerun on the same non-root Linux host: `none/exec`, `none/fs-cp`, and
`none/service/user` all passed (50 checks). The final local Rust suite again
passed all 1,029 tests, and workspace Clippy passed with warnings denied.
