# Slice 1 plan: snapshots and hand edits on the VM drivers

Status: plan, 2026-11-17. Implements slice 1 of the
[images and run data spec](2026-11-17-images-and-run-data-spec.md#slices):
the layer store, layer disks, stacking in the guest's init, the session
primitive, `image edit` (with its record and `run:` snippet), `image build`
and `image rollback`, commits in the lockfile; layers of kind `run` only, on
`linux-firecracker` and `macos-vz`. Features, container drivers, hooks and
sync mappings are later slices.

Slice 0's [evidence](2026-11-17-features-on-needy-base-evidence.md) holds
for this plan: the image's `PATH` and the read-only root are what a run of a
built image needs to fix. The supplementary groups fix (#23) lands before
step 5 needs it.

## Steps

Each step is one PR that leaves `main` working: a step whose surface is not
reachable yet is built behind the next step's entry point, not behind a
flag. Each starts with its failing tests at the highest seam that can show
the behavior.

| # | Step | Seam that tests it | Depends on |
| --- | --- | --- | --- |
| 1 | **Image definitions in config**: `actor.image` by syntax (a reference, a path to an image file, an inline map); the image file kind with `root`, `layers` (`run` only; `feature` refused until slice 3, `TODO(images)`), `env`; keyed append along `extends`; recipe `os.image` removed | config loading | nothing |
| 2 | **The build plan** (`needy-core`, no IO): steps with cache keys over the root digest, the user (A3) and each `run` script; the image's `env` and `PATH` | unit, through the plan's interface | 1 |
| 3 | **The layer store**: put a layer from a stream, digests, the snapshot record, commits collapsed with a history, rollback, garbage collection that keeps named layers, materializing SquashFS layer disks with a digest marker, opaque directories expanded to whiteouts | the store's interface, on temporary directories | nothing |
| 4 | **The guest configuration** (Protobuf, with Buf): the layer list, the session settings (upper layer, routing, ending), the image's `PATH`; and a streaming upload of an archive from the guest | daemon in process (`Guest::config`) and the api-client round trip | 2 |
| 5 | **Stacking in the guest's init**: after `GetConfig`, find layer disks by marker, mount them with overlayfs under a tmpfs upper layer, move the earlier mounts, keep `/.needy`; write the image's `PATH` under `/run/needy`; at a session's end, archive the upper layer and stream it | guest tests on Linux (as root, in a container), then e2e | 4, #23 |
| 6 | **VM drivers attach layer disks**, after the guest disk, from the store's materialized snapshot | daemon in process with a stand-in VM driver (the disk list it is given) | 3, 4 |
| 7 | **Sessions and building**: the session primitive in the daemon; `run create` building missing `run` steps and booting the snapshot; one build per cache key; the snapshot digest in the run record and trace | daemon in process; e2e | 2, 3, 5, 6 |
| 8 | **The CLI**: `image build`, `image edit` (every exec decided as local so the trace records it; the commit's `run:` snippet), `image rollback`; commits in the lockfile, written by the CLI | e2e `image` scenario on `macos-vz` | 7 |

Slice 1 ends with the e2e `image` scenario passing on `macos-vz` here and on
`linux-firecracker` through `cargo xtask e2e --remote`, and its evidence
recorded, including the measurements the spec asks for (boot time with 0,
1 and 8 layer disks; Firecracker's device limit; an edit session's tmpfs
use).

## Decisions this plan makes inside the spec

- Steps 1 and 3 have no dependency between them and can be reviewed in
  parallel.
- Step 1 refuses `feature` layers with a message naming slice 3, rather than
  parsing them unused, so nothing accepts input it cannot honor.
- The layer store lives beside the existing VM image and guest disk code in
  `needy-core-actor`'s image component; the build plan in `needy-core`,
  which does no IO.
- The image's `PATH` reaches the guest in its configuration (step 4), not in
  the `env` map, which would replace the trampoline and stub `PATH` whole.

## Risks to watch

- Moving init's earlier mounts into the overlay root without losing the
  never-route rule (`/.needy` must stay where `binfmt_needy` expects it).
- Firecracker on x86_64 may cap block devices below a long image's layer
  count; step 6 measures it before step 7 relies on it.
- An edit session's upper layer is in guest memory; step 5 records a size
  check and the `TODO(images)` for a host-backed scratch disk.
