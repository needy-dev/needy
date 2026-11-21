# Slice 1: snapshots and hand edits on VM drivers

Evidence for slice 1 of the
[images and run data spec](2026-11-17-images-and-run-data-spec.md#slices)
and the [slice 1 plan](2026-11-17-images-slice-1-plan.md).
Recorded 2026-11-21.

## Scope & Setup

- **Base revision**: `6723724` (`main` after slice 1 steps 1–8), plus the
  `image` e2e scenario and the `root_digest` caching fix on branch
  `feat/e2e-image-slice-1`.
- **Hosts tested live**:
  - An Apple silicon Mac (`arm64`, macOS 27.2), `vfkit v0.6.4`, non-root daemon
    (`uid != 0`) for `macos-vz/image` and VM measurements.
  - A Linux x86_64 KVM host, non-root daemon (`uid != 0`), tested at `c0ecc6c`
    for `linux-firecracker/image`.

## E2E `image` scenario

Added `Scenario::Image` (`macos-vz/image` and `linux-firecracker/image`) to
`cargo xtask e2e` (`xtask/src/e2e/image.rs`). The cell exercises:

1. An image definition file (`test-image.yaml`) with a `run:` layer that
   installs an executable in `/opt/e2e-bin/e2e-layer-tool` and writes
   `/etc/e2e-layer.txt`, plus `env.PATH: "/opt/e2e-bin:${PATH}"` and
   `env.E2E_LAYER_ENV: "from-image"`.
2. `needy image build --profile`: builds the layer once, returns a
   `sha256:` snapshot digest, and hits cache on a second invocation with the
   same digest.
3. First `needy run create`: verifies `e2e-layer-tool` is found on `PATH`
   at `/opt/e2e-bin/e2e-layer-tool`, `/etc/e2e-layer.txt` and
   `E2E_LAYER_ENV` are visible, and `needy run report -o json --level detailed`
   records the exact `snapshot` digest.
4. `needy image edit --profile` on a pseudo-terminal: runs commands inside
   the writable root shell creating `/etc/e2e-edited.txt` and
   `/usr/local/bin/e2e-edit-tool`, exits cleanly, and verifies the lockfile
   (`test-image.yaml.lock`) records `commit.layer` and `commit.base` and the
   CLI prints a pasteable `- run: |` snippet containing the executed commands.
5. Second `needy run create`: verifies both the declared `run:` layer and
   the committed edit are visible and the report records a new `snapshot`
   digest.
6. `needy image rollback --profile`: clears the lockfile commit entry.
7. Third `needy run create`: verifies the committed edit is gone, the
   declared `run:` layer and `env`/`PATH` remain, and the report restores
   the original `snapshot` digest.
8. Standard harness invariants (private sockets, discovery descriptor,
   clean daemon shutdown on SIGTERM, zero leftover VMs/processes/mounts/sockets).

### Results

| Cell | Host | Command | Result | Duration |
| --- | --- | --- | --- | --- |
| `macos-vz/image` | Apple silicon Mac (`arm64`, macOS 27.2) | `cargo xtask e2e --only image` | **pass** | 37.5 s (after `root_digest` cache fix; 125.7 s before) |
| `linux-firecracker/image` | Linux x86_64 KVM host (non-root) | `cargo xtask e2e --remote <KVM_HOST> --only /image` (at `c0ecc6c`) | **pass** | 28.4 s |

## Bug uncovered during live measurements

While measuring `macos-vz` boot times with 0, 1, and 8 layer disks, cached
1-layer and 8-layer runs showed ~19.0 s wall time despite a ~1.6 s reported
run duration.

- **Root cause**: `needy_daemon::image::root_digest` hashed the entire
  locally built `rootfs.ext4` (~573 MiB, which has no `manifest.sha256`) on
  every `run create`, `image build`, and `image edit`, adding ~17.4 s of
  unoptimized SHA-256 work per call in debug builds.
- **Fix**: cached the computed `root_digest` in `Images` keyed by
  `(rootfs_path, len, modified)` with a unit regression test
  (`root_digest_caches_unhashed_rootfs_and_invalidates_on_change` in
  `crates/daemon/src/image_test.rs`).
- **Effect**: `macos-vz/image` e2e runtime dropped from 125.7 s to 37.5 s,
  and cached 1-layer / 8-layer `run create` wall times dropped from ~19.0 s
  to ~1.7–1.8 s.

## Measurements on `macos-vz`

Collected on an Apple silicon Mac (`arm64`, macOS 27.2, `vfkit v0.6.4`),
default VM size (2 vCPU, 2 GiB RAM), debug `needy` binary, 3 consecutive
cached runs per configuration:

### Boot time by layer disk count

| Layer disks | Wall time (`run create` -> `finished`, ms) | Median wall | Report `duration_ms` | Median report | Guest layer mount time (init log) |
| --- | --- | --- | --- | --- | --- |
| 0 (base only) | 1689, 1693, 1686 | **1689 ms** | 1553, 1530, 1545 | **1545 ms** | — |
| 1 (`/dev/vdc`) | 1689, 1679, 1718 | **1689 ms** | 1543, 1543, 1575 | **1543 ms** | ~0.2 ms (`0.0214s` -> `0.0216s`) |
| 8 (`/dev/vdc`..`/dev/vdj`) | 1817, 1819, 1831 | **1819 ms** | 1637, 1628, 1629 | **1629 ms** | ~3.0 ms (`0.0217s` -> `0.0247s`, ~0.38 ms/disk) |

Adding 8 SquashFS layer disks added **+84 ms** to median reported run
duration (+130 ms wall time) compared to booting the base image alone.

### Edit session upper-layer tmpfs use

Inside an interactive `needy image edit` session on the default 2 GiB
`macos-vz` VM, `/run/needy/overlay` is mounted as a Linux `tmpfs` (default
50% of guest RAM):

- **Capacity (`statvfs` / `df -B1 /run/needy/overlay`)**: `1,037,004,800`
  bytes (~989 MiB).
- **Empty upper layer (at boot)**: `0` bytes used (0%).
- **After writing a 32 MiB binary + 200 files in `/opt/pkg`**:
  `34,373,632` bytes (~32.8 MiB, 4% of tmpfs) used; cleanly archived,
  streamed to the daemon via `PushLayer`, and committed as a SquashFS layer
  with all external commands recorded in the printed `run:` snippet.

As noted in the spec (`TODO(images)`), edit/build sessions whose writes
exceed ~50% of guest RAM (~989 MiB on a 2 GiB VM) will need a host-backed
scratch disk in a later slice; init currently logs a warning at 80% tmpfs
utilization.

### Firecracker x86_64 block-device limit

- **Source-derived enforced limit** (`crates/core-actor/src/driver/linux_firecracker/config.rs`,
  `max_layer_disks`):
  - **Without PCI (`pci=off`, default)**: 19 x86_64 legacy MMIO IRQs
    (GSIs 5..=23) minus 3 base virtio devices (`rootfs`, `needy` guest disk,
    `vsock`) = **16 layer disks** (so 8 layer disks + guest disk = 9 disks
    fit with 7 slots to spare).
  - **With `--enable-pci`**: 32 PCI slots minus root bridge (and optional
    ISA bridge) minus 3 base virtio devices = **27–28 layer disks**.
- **Live x86_64 KVM verification**: **not checked** in this run (pending
  remote KVM host SSH access).

## Remaining / Not checked

- Live boot-time (0, 1, 8 layer disks), 8-layer + guest-disk device-limit
  confirmation, and edit-session tmpfs measurements on `linux-firecracker`.
