# needy through a Cloudflare Tunnel

**Status**: evidence, 2026-11-13. What a control plane behind a Cloudflare
Tunnel with Access does to runners, paired clients, routed commands and
workspace mounts; what broke and was fixed; latency against `ssh -L` and
with added delay; and how needy should handle high latency. Host names,
accounts and IDs are replaced: the tunnel's hostname is
`needy-e2e.example.com`.

Related notes:

- [Runners and runs](2026-11-10-runner-and-run-design.md): dial-out, the
  control stream, task streams, join tokens and paired clients.
- [End-to-end harness](2026-11-13-e2e-harness-design.md#transports): the
  transport dimension this work added.

## Setup

One remotely managed named tunnel with two public hostnames: a TCP one
(`tcp://localhost:7400`) and a proxied HTTPS one (`http://localhost:7400`,
`http2Origin: true`). One self-hosted Access application covers both, with
one Service Auth policy that admits one service token, and both ingress
rules carry `originRequest.access` (required, the application's audience
tag), so the connector itself refuses traffic without a valid Access
token. `cloudflared` 2026.9.0 everywhere; the Linux copies are user binaries
from the release, checked against its published SHA-256. Static `needy`
(x86_64, musl) was built on the Linux build host from `git archive HEAD`;
the Mac ran a debug build of the same commit. Every daemon ran as a normal
user.

Two layouts were run:

| Layout | Control plane, actor (`linux-firecracker`) and tunnel connector | Runners (`executor(native)`) |
| --- | --- | --- |
| A: cloud | a cloud VM, 3 vCPUs, nested KVM, far from the Mac (about 280 ms round trip through Cloudflare) | an Apple silicon Mac on a home connection, and the Linux build host |
| B: KVM host | the Linux build host (Intel, 20 threads), on the Mac's network | the Mac |

Layout A was the first target. Partway through, the VM's ssh jump host
needed a key the session's ssh agent no longer held, so the VM could not
be reached; layout B replaced it for the rest (latency injection, agents),
and **layout A's latency injection and agent runs are pending** until the
VM is reachable again. A's other results below were complete before that.
A Windows runner was not added (building `needy.exe` there was not cheap
enough for this pass).

In both layouts the Mac's uplink (about 0.5 to 1 MB/s) bounds every stream
that leaves the Mac, through Cloudflare or not.

## Simulation first

Stand-ins on the connector host's `localhost:7400` before any needy: a
`socat` echo for TCP, nothing for HTTP (the connector's error shows Access
let the request through).

| Check | Result |
| --- | --- |
| `cloudflared access tcp --hostname needy-e2e.example.com` with the service token, from the Mac and the build host | echo round trip; first byte after 1.3 to 4.2 s (websocket and Access), then 280 to 340 ms per round trip to layout A |
| the same without the token | refused: `websocket: bad handshake` |
| `originRequest.access` on the TCP hostname | does not block `access tcp` with a service token |
| one connection idle 130 s, another idle 300 s and then 900 s, zero bytes | every round trip after the idle worked, from both machines |
| HTTPS hostname, no headers | `403` from Access |
| HTTPS hostname, `CF-Access-Client-Id`/`Secret` headers | `502` from the connector (the stand-in speaks no HTTP): Access let it through |
| gRPC (`content-type: application/grpc`) to the HTTPS hostname, with or without the headers | `403 text/html` from Cloudflare: the zone's gRPC switch is off |

## Proxied HTTPS (gRPC through Cloudflare): not viable

needy over `https://needy-e2e.example.com` fails at once: `protocol error:
received message with invalid compression flag: 60 … status: 403
Forbidden`, an HTML page where gRPC frames were expected. Cloudflare
answers gRPC with 403 while the zone's gRPC setting is off, and it stayed
off: Cloudflare documents that Tunnel public hostnames do not carry gRPC,
and that Access ignores gRPC once the setting is on, which would open the
zone's other Access-protected hostnames to gRPC without Access. The
connector's `originRequest.access` would still refuse such requests for
lack of an Access token, so this transport cannot work behind Access. The
setting is not in the zone settings API either (`Undefined zone setting:
grpc`).

- Reference: <https://developers.cloudflare.com/network/grpc-connections/>
  (page dated 2026-08-14, retrieved 2026-11-13: "Cloudflare Tunnel supports
  gRPC traffic via private subnet routing. Public hostname deployments are
  not currently supported"; "gRPC traffic will be ignored by Access if gRPC
  is enabled").

## Cloudflare Access TCP

Runners and clients use `exec:cloudflared access tcp --hostname
needy-e2e.example.com` as their endpoint, with the service token in
`TUNNEL_SERVICE_TOKEN_ID` and `TUNNEL_SERVICE_TOKEN_SECRET`: nothing is
started by hand, and each HTTP/2 connection is one `cloudflared` process.

| Check | A: cloud | B: KVM host |
| --- | --- | --- |
| runner join and control stream | both runners joined in 1.3 to 1.5 s | joined in 1.5 s |
| heartbeats | `runner ls` every 30 s for 33 minutes: always `online`; one control stream per runner from start to end, no reconnect | `runner ls` every 30 s for 20 minutes: always `online`, one control stream, no reconnect |
| `needy pair`, `runner ls`, `exec --runner`, `report` from the paired client | work; `report` names `client:NAME` | work |
| `-t`, `run attach`, typing, `ctrl-]` | prompt redrawn, typed commands (local, routed, on the mount) ran, `ctrl-]` detached (exit 0) | - |
| the workspace mounted from the Mac after 5 minutes idle | read 624 ms, write 974 ms, routed command 703 ms | read 562 ms, write 846 ms, routed command 374 ms |
| after 15 minutes idle | read 619 ms, write 1554 ms, routed command 665 ms | read 343 ms, write 2008 ms, routed command 335 ms |
| copy-in from the control host; mirror-out from the actor and from the Mac's executor at a graceful `run stop` | `SIGTERM` reached the program; both mirror-outs on the control host, without the files their filters exclude; the copy-in without its excluded file | the same |
| a 200 MiB file through the mount, written and read back | 52.7 s and 216.6 s, checksums equal | 529 s and 440 s (the connector and the Mac share one home uplink, crossed both ways), checksums equal |
| `fs cp` of 41.8 MB to the control host | 38 s, checksums equal | - |
| opencode with the mock model | the loop ran: one tool call `bash -c "uname -a; ls"` routed to the Mac, its output in the final answer, exit 0 | the same (below) |
| pi with the mock model | failed until the gateway fix (below) | the loop ran, exit 0 (below) |

## `ssh -L` (baseline)

Layout A: the Mac forwards `127.0.0.1:47400` to the VM's `7400` through an
ssh jump host; the build host reaches it with `ssh -R` back to that forward
(two hops). A third client used `exec:ssh -W 127.0.0.1:7400 VM` as its
endpoint, which works the same way. Layout B: one `ssh -L` from the Mac to
the build host.

| Check | A: cloud | B: KVM host |
| --- | --- | --- |
| runner join, heartbeats | online in every sample | online in every sample |
| the workspace after 5 minutes idle | read 543 ms, write 1369 ms, routed 663 ms | read 34 ms, write 84 ms, routed 27 ms |
| after 15 minutes idle | read 548 ms, write 1351 ms, routed 588 ms | read 24 ms, write 80 ms, routed 30 ms |
| graceful `run stop`, copy-in, mirror-outs | as with Cloudflare | `SIGTERM` reached the program; both filtered mirror-outs on the control host |

## Measurements

Timed inside the actor (`date +%s%N` around each step); the workspace and
routed commands on the Mac.

| Hop | A, Cloudflare | A, `ssh -L` (jump host) | B, Cloudflare | B, `ssh -L` (same network) |
| --- | --- | --- | --- | --- |
| routed `uname`, start to exit (5 runs) | 631 to 697 ms | 524 to 605 ms | 331 to 380 ms | 45 to 71 ms |
| routed command, first byte (it sleeps 1 s after) | 1671 ms | 1558 ms | 1478 ms | 1085 ms |
| 50 MiB of routed stdout to the actor | 45.5 s | 369 s | 93.9 s | 2.2 s |
| 20 `stat` of one file (cached attributes) | 436 ms | 340 ms | 432 ms | 68 ms |
| 20 small write and read pairs | 38.6 s | 31.4 s | 26.8 s | 2.4 s |
| `git status`, 476 files, in the actor (cold, then again) | 698 s, 324 s | - | - | - |
| `git status` routed to the Mac (cold, then again) | - | - | 357 ms, 501 ms | 61 ms, 98 ms |
| 200 MiB write, read back | 52.7 s, 216.6 s | - | 529 s, 440 s | 17.0 s, 16.9 s |
| `exec.decide` (report, `trace.timing`) | next to the actor | | 23 to 63 µs | 17 to 62 µs |

What they say:

- A routed command costs about two round trips of the actor-to-runner path
  (the control plane announces the task on the control stream; the runner
  opens a task stream and starts the process), plus the process.
- In layout B, Cloudflare turns a same-network round trip into one through
  a Cloudflare edge and back (about 160 ms here): six times slower for a
  routed command than `ssh -L` on the same network. In layout A, where the
  path is long anyway, Cloudflare was within 20% of `ssh -L` for request
  and reply and far faster for streams than an ssh jump host.
- Mount operations pay one round trip each, sequentially. `git status` in
  the actor on a repository made on the executor is the worst case: the
  index's inode and device numbers do not match what sshfs shows, so git
  re-reads every file over the mount (698 s). Routed to the executor, the
  same `git status` took 357 ms.

### Added latency

Layout B, `ssh -L`, with `cargo xtask link-proxy --delay D` between the
runner and the control plane, so only the runner's hop is slower (D each
way; the round trip gains 2 D). The control plane, the actor and the
client stayed where they were.

| Added round trip | `exec --runner` from the client | routed `uname` in the actor | first byte | 5 MiB of stdout | first `stat` | 10 small writes | 10 small reads | `ls -la` | report: `exec.start` mean |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| +0 ms | 69 to 191 ms | 37 to 68 ms | 1089 ms | 0.40 s | 61 ms | 0.99 s | 0.41 s | 33 ms | 55 ms |
| +50 ms | 175 to 252 ms | 146 to 206 ms | 1197 ms | 0.58 s | 130 ms | 1.92 s | 2.06 s | 208 ms | 167 ms |
| +150 ms | 373 to 452 ms | 331 to 379 ms | 1390 ms | 1.47 s | 198 ms | 5.14 s | 5.24 s | 322 ms | 348 ms |
| +300 ms | 666 to 709 ms | 625 to 729 ms | 1641 ms | 2.69 s | 333 ms | 9.43 s | 9.76 s | 622 ms | 650 ms |

- Routed commands and `exec.start` grow by two added round trips, and
  nothing else: the model holds.
- A small file write or read through the mount grows by about three round
  trips (`open`, the read or write, `close`); a tool that touches many small
  files on the mount degrades fastest.
- A stream grows slower than its round trip: 5 MiB took 2.7 s at +300 ms,
  as HTTP/2 flow control windows meet the round trip.
- Nothing timed out or failed at +300 ms.

### One agent tool-call turn

Layout B, through Cloudflare from the Mac's client, the mock model as an
actor network service, one tool call each (`trace: { timing: true }`):

| Agent | Shell on the Mac (Cloudflare) | Shell on the control host |
| --- | --- | --- |
| pi | run 9.1 s; `exec.start` mean 347 ms | run 1.3 s; `exec.start` mean 75 ms |
| opencode | run 53.9 s; `exec.start` mean 3.2 s; 15 gateway CONNECTs, mean 646 ms | run 2.5 s; `exec.start` mean 4.9 ms |

The opencode run through Cloudflare is one sample: its gateway CONNECTs are
opencode's own background requests (its plugin install), not the tunnel,
and why its `exec.start` (the workspace's SFTP server and the routed shell)
took 3.2 s on average was not found in this pass.

## What broke, and the fixes

| Found | Cause | Fix | Test |
| --- | --- | --- | --- |
| gRPC through the proxied HTTPS hostname | Cloudflare (above) | none in needy: use a TCP route | - |
| Runners and clients needed a hand-run `cloudflared access tcp --url` per machine | needy could only dial a socket address | `exec:COMMAND` endpoints (`7026dfb`): each connection is a process whose stdio carries it, as ssh's `ProxyCommand` | `command_test.rs`: a call through a `bash /dev/tcp` relay, the command ends with its connection, a missing program is named, malformed commands are invalid |
| A workspace mount died for good when its exec stream was lost (a tunnel or connector restart, a runner that reconnects) | sshfs in passive mode cannot reconnect, and requests in flight never got an answer | the mount keeps its stream (`d3b0bea`): requests in flight are answered `SSH_FX_CONNECTION_LOST` (EIO), a new `sftp-server` is started with sshfs's `INIT` replayed and its `VERSION` kept from sshfs, for up to 2 minutes, then the mount ends | `stream_test.rs` (a fake executor that drops, ends and refuses streams); e2e `runner/at=local,transport=idle-proxy`, which cuts every connection: it passes, and fails (`after-cut=fail`) on a tree whose reconnect window is zero |
| pi could not reach the mock model service | pi's `fetch` (undici 6's `ProxyAgent`) tunnels plain HTTP with `CONNECT mock.needy:80`; the gateway tunnelled to the name itself and answered 502 | a CONNECT to a service's host tunnels to the service's URL (`438232c`) | `a_connect_to_a_service_s_host_tunnels_to_its_url`; pi with the mock passes in layout B |
| a slow hop was invisible in `needy report` | `trace.timing` was recorded and never shown | the report shows each hook's latency (`exec.decide`, `exec.start`, `net.*`) when the profile sets `trace: { timing: true }` (`02b71fd`) | `report` tests; the tables above read it |

The harness's `cloudflare` transport cell
(`linux-firecracker/runner/at=local,transport=cloudflare`) ran on the
Linux KVM host through this tunnel and passes: a runner joined through
`exec:cloudflared access tcp`, the workspace from it stayed idle past 30 s,
the tunnel connector was ended and started again, and the mount and routed
commands came back. Its first run failed because the runner joined before
the cell's connector registered (`websocket: bad handshake`, and a failed
first join ends a runner); the harness now waits for the registration
(`fa22458`).

Not broken, and why: needy's HTTP/2 pings every 20 s on clients and servers
keep every connection busy; Cloudflare's documented idle limits are longer
(Proxy Idle Timeout 900 s; the 125 s Proxy Read Timeout applies to proxied
HTTP, not to Access TCP); and a connection with no bytes at all survived
900 s through Access TCP. Runners already reconnected with backoff (0.5 s
doubling to 15 s), as the latency runs showed each time the delay proxy
restarted. No keepalive or reconnect change was needed for this transport;
the harness's `idle-proxy` cell keeps it that way.

- Reference: <https://developers.cloudflare.com/fundamentals/reference/connection-limits/>
  (page dated 2026-10-22, retrieved 2026-11-13).

## Interconnect without Cloudflare features

| Mechanism | Size | Verdict |
| --- | --- | --- |
| an endpoint that is a command: `exec:cloudflared access tcp --hostname H`, `exec:ssh -W 127.0.0.1:7400 HOST`, `exec:tailscale nc HOST 7400` | 86 lines with comments (`crates/api-client/src/command.rs`) and help text; tests 98 lines | built and used for every Cloudflare runner and client above: any tunnel that can forward stdio to a TCP port works with no code of its own, and the CLI's paired endpoint and a runner's `runner.json` keep it like any URL |
| extra headers on HTTP(S) endpoints (`NEEDY_ENDPOINT_HEADERS`) | about 25 lines (an interceptor already adds `authorization`) | not built: Access headers help only on Cloudflare's proxied HTTPS, which cannot carry needy's gRPC; a candidate for an HTTP reverse proxy that forwards gRPC and checks a header |

## High latency

Where a round trip is spent today:

| Operation | Round trips of the actor-to-runner path | Notes |
| --- | --- | --- |
| `exec.decide` | 0 | the guest asks the control plane on its own host |
| routed command start | 2 | the task is announced on the control stream; the runner opens a task stream and starts the process |
| routed command output | streamed | bounded by bandwidth and HTTP/2 windows |
| a mount operation | 1 each | sshfs sends one SFTP request per FUSE request; attributes and entries are cached for 1 s |
| copy-in, mirror-out | about 2, plus the stream | tar over one exec |

Ideas, evaluated:

- **Decide in the guest from rules pushed over the control stream.** No gain
  in either layout: the decision is made next to the actor (23 to 63 µs).
  It would matter only for an actor whose control plane is far away, which
  needy does not build (`TODO(runner-actor)`). Not done.
- **One round trip for a routed command instead of two**: carry the exec's
  start in the task announcement, so the runner starts the process and
  opens its task stream at once. Saves one round trip per routed command
  (about 300 ms of 650 ms at +300 ms). A protocol change to the runner
  streams: candidate next slice.
- **Batching or pipelining mount operations**: sshfs already pipelines
  independent requests; what costs is a program's sequential small-file
  access, which no batching below FUSE removes.
- **Mount caching.** Longer `attr_timeout` and `entry_timeout` (1 s today)
  cut repeated `stat`s but let the actor see a file a routed command changed
  that much later, which breaks "build on the executor, read the output in
  the actor". A per-profile setting is the honest form; not done.
- **Placement advice** (from the numbers): keep the control plane next to
  the actor, so decisions stay local; route the workspace's own tools (`git`,
  build tools, package managers) to the executor that holds it; use the
  mount for what the driving program itself reads and writes; and on one
  network, prefer a direct or `ssh` route to a hairpin through Cloudflare.
- **Per-hop latency in `needy report`**: done (`02b71fd`).
- **Timeouts and keepalives for a long round trip**: the 20 s HTTP/2 pings,
  the 10 s wait for a task stream and the 10 s wait for a mount's first
  round trip all held at +300 ms and at layout A's 600 ms paths; nothing
  changed.

## Decisions made on the spot

- **The zone's gRPC setting stays off** (above).
- **A remote runner's `linux-container` executor is not built**
  (`TODO(runner-docker)`): the build host joined with `executor(native)`.
- **One set of Cloudflare resources.** A first set made before the
  coordinator's message was deleted at once; the remaining set, made by the
  coordinating session, was deleted at the end.
- **Layout A's VM, once unreachable over ssh, was not used for new work.**
  Its daemon, connector and files were removed through the paired client
  session that was still open (`needy exec` on the control host), which the
  VM's own cleanup needed. The last step ended that daemon, so the result
  could not be checked from here, and the one-file cleanup script in the
  VM's `/tmp` stays until someone can log in.
- **The baselines' `ssh -L` paths differ**: a jump host in layout A, the
  same network in layout B. Each is compared with Cloudflare in its own
  layout.
- **Interactive checks read the console** (`run logs`), not the attach
  client's redraw, whose escape sequences split long markers.
- **`ssh-l` harness cells are not covered here**: neither machine accepts
  its own key, and changing `authorized_keys` was not this work's to do.

## Follow-up candidates

- One round trip for a routed command start (above).
- `run create` fails at once (`runner NAME is not connected`) while a
  runner is reconnecting, even 60 ms before it is back; `exec --runner`
  waits for the control stream since `5f11cd4`, and the same short wait at
  scheduling would ride out a tunnel restart.
- `needy report RUN_NAME`: `report` takes an ID, a prefix or `latest`, not a
  run's name, unlike `run logs` and `run stop`.
- A relative `host:` path in a profile resolves on the client; with a
  remote control plane it names a path the control host does not have
  (`examples/agents/mock-model/pi.yaml`).
- `exec.start` latency by executor in the report, not only overall.
- Why opencode's run through Cloudflare took 54 s (above).
