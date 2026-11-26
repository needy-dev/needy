# Multi-host pass, v0.0.19 to v0.0.22

Status: **evidence**, 2026-11-26. The test pass that ran through releases
v0.0.19 to v0.0.22 on several hosts, and what each failure became.
Evidence level: **e2e** (`cargo xtask e2e`, non-root daemons) for the
matrix counts, **probe** for the cross-host remote runs, the SOCKS upstream,
the Windows image build and the holder upgrade. A probe is not a support
claim.

Related notes:

- [Deployment guide](../../../guides/deployment.md), which cites this note
- [e2e harness design](2026-11-13-e2e-harness-design.md) (cells, counts and
  "not covered")
- [S1: the runner gateway](2026-11-26-runner-gateway-design.md)
- [Actor holder and control recovery](2026-11-15-actor-holder-implementation-evidence.md)

Counts are **pass / fail / not covered**, as the harness reports them.

## Hosts

| Host | Hardware and setup |
| --- | --- |
| KVM workstation | Linux x86_64, 20 threads, KVM and Docker |
| A | Linux x86_64 cloud VM, Docker, no KVM |
| B | Linux x86_64 with KVM and Docker. It reaches the internet through a fake-IP proxy: `example.com` resolves into `198.18.0.0/15` and `ghcr.io` is unreachable |
| C | Linux x86_64 cloud VM, Docker with gVisor (`runsc`) registered, no KVM |
| Windows | A Windows 11 machine, CLI only |

## v0.0.19

**KVM workstation, non-root harness: 56 / 20 / 48**, the same in both
rounds. The itemized failures are fixed since:

| Failures | Cause | Fixed in |
| --- | --- | --- |
| 10 | `linux-container` executors started with a command the CLI no longer parsed (`needy daemon --mode executor`) and exited 2 | [#122](https://github.com/needy-dev/needy/pull/122), v0.0.20 |
| 6 | Routed commands exited 127: a routed path was mapped wrongly | [#123](https://github.com/needy-dev/needy/pull/123), v0.0.20 |
| 1 | The remote-actor-host cell's expectations drifted from the schema | #123, [#128](https://github.com/needy-dev/needy/pull/128) |
| 1 | The `runc` image cell expected the lockfile to be gone after a rollback, but a `linux-container` build keeps an implicit `builtin:sshfs` feature pin | #128 |
| 2 | Not itemized in the pass report | — |

Two more cells (`runc/executor-files/executor=native`, `end=stop` and
`end=runtime-kill`) hung on a `linux-container` workspace mount that
contained itself and were counted as passes only after they were unmounted
by hand; the mount moved out of the workspace in
[#132](https://github.com/needy-dev/needy/pull/132), v0.0.22, and walks of a
cell no longer cross a mount boundary (#128).

**Cross-host remote actor** (control on a host without KVM, actor runner on
a KVM host), probe:

- S1: the actor's egress left from the runner's host, not the control host.
- 7 of 31 runs on one of the two paths failed: the bridge closed before the
  actor's exit was delivered. Fixed in
  [#124](https://github.com/needy-dev/needy/pull/124), v0.0.20.
- Per-actor SOCKS upstream: `via` recorded on the proxied `net.*` events; a
  failing proxy failed closed (no direct connection).

**Windows**, probe:

- `needy image build --from REF` produced the same snapshot digest as on
  Linux.
- The Windows CLI could not authenticate to the control daemon: saving a
  paired context and reading a private credential file need Windows ACL
  support (`TODO(windows-acl)` in `crates/cli/src/configuration.rs`).

## v0.0.20 re-verification

- **KVM workstation: 69-70 / 4-5 / 48** over the rounds. The remaining
  failures were harness drift, fixed in #128.
- **Remote runs: 62 of 62 finished** (probe), on both paths, including runs
  whose console carried 7.2 MB.
- **Remote console streaming is latency-bound** (probe): about 10 KB per
  round trip, so 7.2 MB took about 370 s at 0.5 s RTT and about 28 s at
  0.25 s RTT. Recorded as a known gap in the deployment guide.

## #128 live (v0.0.21 code)

**KVM workstation: 75 / 1 / 48.** The one failure was the daemon's drain on
SIGTERM: the shared gateway's accept loop did not stop with control. Fixed
in [#129](https://github.com/needy-dev/needy/pull/129), v0.0.21.

## v0.0.21 non-root pass on A, B and C

The harness ran as a dedicated non-root account (`needy-e2e`), two rounds.

| Host | Round 1 | Round 2 | Failures |
| --- | --- | --- | --- |
| A | 39 / 0 / 85 | | none; not covered for want of KVM and `runsc` |
| B | 70 / 6 / 48 | | 5 from the host's network (`network/allow` ×2, the remote-actor-host cell, `image-feature` ×2: fake-IP answers and no `ghcr.io`), and the sensor cell: B has no `/sys/class/hwmon` |
| C | 73 / 1 / 50 | 72 / 2 / 50 | the sensor cell, and once a terminal-drain flake in `runsc/image` |

The harness now skips the sensor cell on hosts without hwmon and drains the
terminal before reading it ([#134](https://github.com/needy-dev/needy/pull/134)).
The network failures on B are the host's, not needy's.

## Holder adoption across v0.0.21 to v0.0.22

Probe, KVM workstation. With a local run live under a v0.0.21 daemon, only
the daemon was killed and a v0.0.22 daemon started in its place. It adopted
the running v0.0.21 runtime holder; the run finished normally, and the old
holder cleaned up its mount at the old location (the location #132 moved).
This covers one upgrade with one run; it is not a general compatibility
promise between holder and daemon versions.
