# Apple `container` actor: evidence

> **NOTICE (2026-11-12, migration):** the
> [drivers design](2026-11-12-drivers-and-extension-points-design.md) renamed what this note describes, without
> compatibility: `actor.runtime` is `actor.driver` (`firecracker` is
> `linux-firecracker`, `container` is `linux-container`); an executor's
> `provider` is its `driver` (`docker` is `linux-container`) and its `docker:`
> options are `container:`; the `ActorRuntime` trait is the actor `Driver`,
> the executor `Provider` is the executor `Driver`; capabilities read
> `actor(linux-firecracker) executor(linux-container,native)`. The text below
> keeps the names of its time, and its links to `examples/container` point to
> what is now `examples/linux-container`.

**Status**: evidence, 2026-11-12. Step 1 is an experiment with an actor wired
by hand; step 2 is the `macos-apple-container` actor runtime built from it
(`core-actor::host::macos::apple_container`), run end to end. The design is in the
[container actor runtime design](2026-11-10-container-actor-runtime-design.md#as-built-macos-apple-container).

NOTICE: the runs below used the runtime's first name, `apple-container`
(profile value, `actor(apple-container)`, `needy build --apple-container`,
`examples/apple-container`). The owner renamed it to `macos-apple-container`
the same day, and this note uses the new name throughout, also in recorded
commands and output; nothing else in them changed.

Machine: an Apple silicon laptop (M5 Max, 18 cores), macOS 27.2, Apple's
`container` CLI 1.7.1 from Homebrew (services running), and OrbStack as the
Docker API that builds the image. Every container ran on this machine.
Paths below use `/Users/user`; run IDs are the ones the runs got.

Pinned upstream sources (cloned for reading, not built):

| Repo | Revision |
| --- | --- |
| apple/container | tag 1.7.1, `a15b46097aa4c2325da13c1fb5192b9274aab083` |
| apple/containerization | tag 0.54.0, `baf212e5a8e432e539beee325050aa26443a154d` |

## Summary

- **It works.** `examples/smoke` and `examples/chaos`, as
  [`examples/macos-apple-container`](../../../../examples/macos-apple-container/profile.yaml)
  has them, close the loop through `needy run create` in an actor that Apple's
  `container` runs in its own VM: routed commands reach the executor, the
  workspace is mounted, the gateway's allowlist holds, copy-in, mirror-out,
  file and exec faults apply, and `-it`, detach and `run attach` work.
- **Time to ready**: from `actor.start` to `actor.config` (init asked the
  daemon for its configuration) 737 to 919 ms over five runs (median 857 ms);
  the whole run of `true` 0.94 to 1.13 s. Firecracker on the Linux KVM host
  takes 160 to 190 ms to the same point.
- **Transport**: Unix sockets, not loopback TCP. The guest and gateway
  sockets are mounted one file each; the process socket is published.
- **Identity**: the recipe's hostname (init sets it). No DMI, no hwmon, an
  arm64 `/proc/cpuinfo`, Apple's default kernel, and one CPU more than asked.
- Two failures found on the way, both fixed in the runtime: a published
  socket must be on the container's root file system (so the root stays
  writable), and the host side of a published socket accepts before init
  serves it (so the daemon checks a terminal with a request).

## Step 1: by hand

### Image

`images/Dockerfile` built the needy binary for `x86_64-unknown-linux-musl`
only. The `needy` stage now picks the Rust target from BuildKit's
`TARGETARCH`, and `container-root` picks opencode's `arm64` or `x64` asset:

```sh
docker build --platform linux/arm64 -f images/Dockerfile --target actor-container -t needy-actor:arm64 .
docker save needy-actor:arm64 -o needy-actor-arm64.tar      # 2.5 s, 453 MB
container image load -i needy-actor-arm64.tar               # 3.8 s
```

- The first build failed: `host::detect` read KVM's CPUID with
  `kvm_bindings::KVM_MAX_CPUID_ENTRIES` and `Kvm::get_supported_cpuid`, which
  exist on x86 only. Gated to `x86_64`.
- The second built in 87 s (warm BuildKit cache); with `sshfs fuse3` added
  for in-actor mounts, 360 s.
- The Rosetta shortcut (the x86 image with `--rosetta`) was not needed.

### Probes with `alpine:3.22`

| Probe | Result |
| --- | --- |
| `--read-only --tmpfs /.needy/channels -v host.sock:/.needy/channels/guest.sock` | The socket file shows in the tmpfs (`srw-rw-rw-`); the directory stays writable |
| Exit code of `container run` | The container's (`exit 3` gave 3) |
| `-v cpuinfo:/proc/cpuinfo:ro` (a regular file) | Works: `cat /proc/cpuinfo` shows the file |
| `-v dmi:/sys/class/dmi/id:ro` (a directory) | Fails: `mount failed with errno 1: failed to create directory 'dmi'` (no `CONFIG_DMI`, and sysfs takes no new directory) |
| `--tmpfs /home/agent:mode=755,uid=1000,gid=1000` | Works: `drwxr-xr-x 1000 1000` |
| `hostname` | The container's `--name` |
| `CapEff` (default) | `00000000a80425fb`; `/dev/fuse` exists |

### The actor wired by hand

The daemon ran from this branch (`needy daemon --mode control`, its PID and
log under the checkout's `.tmp/apple-container/`, stopped by that PID). A
throwaway runtime, not committed, served the channels as the container
runtime does and ran this command for each run:

```sh
container run --rm --name "needy-actor-$RUN" \
  --network none \
  --tmpfs /tmp --tmpfs /var/tmp --tmpfs /run --tmpfs /home/agent:mode=755,uid=1000,gid=1000 \
  --cap-add CAP_SYS_ADMIN --cpus 2 --memory 2048M \
  -v "$DIR/channels/guest.sock:/.needy/channels/guest.sock" \
  -v "$DIR/channels/gateway.sock:/.needy/channels/gateway.sock" \
  --publish-socket "$DIR/channels/process.sock:/.needy/channels/process.sock" \
  --entrypoint /.needy/init needy-actor:arm64
```

Init's sshfs mounts and hostname need `CAP_SYS_ADMIN`; the container is alone
in its VM, and the driving program runs as uid 1000 with no capabilities
(`CapEff: 0000000000000000`).

The executor was a `native` one on the Mac. A `docker` executor on this
control host was refused before the actor started: `cannot give
.../executors/<id> to the container's user 1000:1000 ... run the daemon as
root or as that user` (the daemon runs as the Mac's user). That is the Docker
provider's rule, not the actor's.

| Check | Result |
| --- | --- |
| Time to ready (`actor.start` to `actor.config`) | 700 ms (smoke), 1054 ms (chaos), 1199 ms (`-it`) |
| Whole smoke run | 1.0 s |
| `uname -a` | `Linux <recipe hostname> 6.18.56 #1 SMP ... aarch64 GNU/Linux` |
| hostname | the recipe's (`instance-20261111-210604`), set by init |
| `/proc/cpuinfo` | arm64: `CPU implementer 0x61`, `CPU part 0x000`, no `model name`; 3 processors for `--cpus 2` |
| DMI, hwmon | none (`/sys/class/dmi/id`, `/sys/class/hwmon` missing) |
| `free -m` | 2102 MiB total for `--memory 2048M`: the VM's own |
| Routed `python3` | ran on the Mac (`platform.node()` the Mac's), exit 7 relayed, `exec.decide`/`exec.start`/`exec.exit` in the trace |
| Workspace mount (sshfs in the actor) | files written on either side show on the other |
| Gateway, `allow: [example.com]` | `https://example.com/` 200; `example.org` 403 (`net.deny`); without the proxy, `Could not resolve host` (no network) |
| Chaos | copy-in with `- /secret.txt` filtered; `cat broken.json` `Input/output error` (`fault.injected` `file.open` EIO); `curl` `fault injected: ECONNREFUSED`, exit 126; mirror-out wrote `result.txt` |
| hwmon fault | `No sensors found!`; with init applying it (as a VM's does), the fault was recorded as injected although `/sys/module/fake_coretemp` does not exist, so the runtime does not send it to init |

### `-it` failed twice

1. **Root read-only, tmpfs over the channels**: `needy run create -it`
   failed with `transport error`. Dialing `process.sock` on the host
   connected and was closed at once (0 bytes), although init had bound
   `/.needy/channels/process.sock`. `vminitd` reaches a published socket at
   the container's root file system path in the VM
   (`socket.source = rootInGuest.appending(path: socket.source.path)`,
   [`LinuxContainer.swift#L1307-L1335`](https://github.com/apple/containerization/blob/baf212e5a8e432e539beee325050aa26443a154d/Sources/Containerization/LinuxContainer.swift#L1307-L1335)),
   outside the container's own mounts, so it cannot see a socket in a tmpfs
   the container mounted. Without `--read-only` and the tmpfs, init binds on
   the root file system and the same dial got the process service's HTTP/2
   `SETTINGS` back in 58 ms.
2. **The daemon's terminal connected too early**: still `transport error`,
   about 1 s after the create. The host side of a published socket accepts
   at once and closes the connection only when it fails to reach init's
   socket, so the daemon's terminal loop took a connection before init served
   it and the attach on it failed. The daemon now counts a terminal
   connection only once the process service answers a request (a health
   check, which it answers `Unimplemented`). After both fixes: `-it`, `ctrl-]`
   and `run attach` work, and `exit 5` returned 5.

## Step 2: the runtime

`actor: { runtime: macos-apple-container }` (optional `container: { image }`;
`oci_runtime` is refused). `needy build --macos-apple-container` builds the image
for linux/arm64 through Docker and loads it with `container image load`
(140 s here, warm cache). The daemon (from this branch, with `DOCKER_HOST`
pointing at OrbStack for the build only) ran:

```sh
needy profile check examples/macos-apple-container/profile.yaml
needy run create --profile examples/macos-apple-container/profile.yaml
needy run create --profile examples/macos-apple-container/chaos.yaml
needy run create -it --profile examples/macos-apple-container/profile.yaml /bin/bash --norc -i
needy run attach RUN
needy run stop RUN
```

| Check | Result |
| --- | --- |
| Capability | a runner on this Mac reported `actor(macos-apple-container,container) executor(docker,native)` |
| `profile check` | warnings for the shims, the extra CPU, hwmon, cpuinfo, cpuid, dmi, pci and kernel; with a missing image, `error: actor: on the control host: the container actor image needy-actor:missing is not in Apple's container image store; run needy build --macos-apple-container`; `oci_runtime` is an error. On Linux, `Apple's container runs on macOS only` (`check_test.rs`) |
| Smoke | finished, exit 0; recipe hostname; 3 processors and `nproc` 3 for 2 vCPUs; 4047 MiB for the recipe's 4096; routed `python3` printed `Darwin /private/tmp/needy-apple-ws`; workspace files both ways; `routed exit=7`; `example.com 200`, `example.org` 403 |
| Chaos | finished, exit 0: copy-in, EIO on `broken.json`, `ECONNREFUSED` for `curl`, mirror-out `out/<run>/result.txt`; the hwmon fault is not recorded (no sensors), as `profile check` warned |
| `-it`, detach, attach | the shell's `tty` `/dev/pts/0`; routed `python3` saw a terminal on the Mac (`Darwin True`); `ctrl-]` detached; `run attach` and `exit 5` gave 5 |
| Time to ready (5 runs of `true`) | 893, 857, 804, 737, 919 ms |
| `run stop` of `sleep 300` | 3.1 s (the 3 s grace, then the stop), run `stopped`, container deleted |
| Daemon killed with `SIGKILL` while an actor ran | the container kept running; the restarted daemon deleted it (`deleting its actor's container, which a restarted control plane does not adopt`), instance `ended`, run `failed` |
| In the actor | `/tmp` and the home directory run programs; `touch /etc/x` as the user: `Permission denied`; root `/dev/vdb ext4 rw` (writable to root only) |

## Gaps

Each is marked in the code.

- **Identity under Apple's default kernel**: no DMI, hwmon, uptime offset or
  recipe kernel (`TODO(macos-apple-container-kernel)`, the separate approved slice
  that boots a needy-built arm64 kernel with `-k`); `/proc/cpuinfo` is the
  arm64 VM's (`TODO(macos-apple-container)`: a file mount works, but needy's
  generator needs a host processor block and an x86 text contradicts `uname
  -m`); the VM has one CPU more than asked (`cpus + cpuOverhead`,
  [`RuntimeService.swift#L268-L270`](https://github.com/apple/container/blob/a15b46097aa4c2325da13c1fb5192b9274aab083/Sources/Services/RuntimeLinux/Server/RuntimeService.swift#L268-L270)).
- **Writable root file system** (`TODO(macos-apple-container)` in `launch`): see
  the first `-it` failure.
- **Image build needs Docker** (`TODO(macos-apple-container)` in `needy build`):
  Apple's `container build` could build the Dockerfile without it.
- **Docker executors on a Mac control host**: refused by the Docker
  provider's ownership rule; the examples use a `native` executor.
- **Native executor workspace**: `workspace: /tmp/needy-apple-ws` is not
  created by the provider; the examples' directories must exist (the first
  smoke run failed with `working directory /tmp/needy-apple-ws/ does not
  exist`).
- **Unix socket paths**: macOS limits them to 104 bytes. The default state
  directory (`~/.local/state/needy/runs/<id>/channels/process.sock`) fits; a
  state directory deep in a checkout did not, so the experiment used a short
  link to it.
