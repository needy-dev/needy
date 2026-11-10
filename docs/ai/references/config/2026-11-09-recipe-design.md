# Recipes

**Status**: draft; implemented as described unless the [closed-loop results](../architecture/2026-11-09-closed-loop-results-evidence.md#not-yet-verified) list it as missing. Terms follow the
[actor and executor separation](../architecture/2026-11-09-actor-executor-separation-design.md)
note. Recipes are referenced from
[profiles](2026-11-09-profile-and-rules-design.md).

A **recipe** describes one machine: image, kernel, CPU, memory, firmware,
devices and network identity. It replaces the earlier word *persona*. A recipe
may describe a truthful machine, as an integration test usually wants, or one
that presents a chosen identity, as an evaluation may want.

| | Recipe | Profile |
| --- | --- | --- |
| Describes | One machine | One session: which recipe the actor and each executor use, routing rules, both networks, chaos, file mappings |
| Used by | `needy build --recipe NAME`; outputs are cached by the hash of their inputs | `needy session create --profile NAME` |
| Reuse | One recipe can serve the actor and several executors | References recipes |

When the actor and an executor use the same recipe, an in-process `uname()` in
the actor and a routed `uname` command on the executor agree without any extra
mechanism.

## Example

```yaml
# recipes/gce-e2-standard-4.yaml
extends: builtin:gce-ubuntu-22.04      # start from a checked built-in recipe

cpu:
  brand: "Intel(R) Xeon(R) CPU @ 2.20GHz"
  signature: { family: 6, model: 85, stepping: 7 }
  hypervisor: hidden                   # visible | hidden
  features: { hide: [avx_vnni, serialize] }   # Linux /proc/cpuinfo flag names; hide only, never add
  topology: { cores: 2, threads_per_core: 2 }

memory: 16GiB

os:
  image: ubuntu:22.04                  # rootfs source
  kernel:
    release: 6.5.0-1020-gcp            # uname -r
    version: "#20~22.04.1-Ubuntu SMP Fri Mar  1 10:04:32 UTC 2024"   # uname -v
    build: { user: buildd, host: lcy02-amd64-033 }                   # /proc/version
  hostname: "instance-{hash}"
  uptime: 3d4h

firmware:
  dmi:
    sys_vendor: Google
    product_name: Google Compute Engine
    bios: { vendor: Google, version: Google, date: 01/01/2011 }
    chassis_type: other
    serial: "GoogleCloud-{hash}"

devices:
  pci:
    host_bridge: "8086:1237"           # 440FX
    extra:
      - { id: "8086:7110", class: isa-bridge }
  sensors: []                          # cloud VMs have none

network:
  interface: ens4
  mac: "42:01:0a:80:00:{seq}"
  metadata: { provider: gce, file: ./gce-metadata.json }
```

A laptop recipe would declare sensors instead:

```yaml
devices:
  sensors:
    - { chip: coretemp, per_core: true, idle_c: 38, load_gain_c: 6, noise_c: 1.5 }
    - { chip: nvme, idle_c: 35 }
    - { chip: acpitz, idle_c: 27 }
```

`{hash}` and `{seq}` derive from the session seed. A replayed session gets the
same values; different sessions get different serial numbers and MAC
addresses.

Built-in recipes are referenced as `builtin:NAME`: `gce-ubuntu-22.04` and
`dell-optiplex-ubuntu-24.04`, embedded in `needy-config`. *Preset* stays
reserved for reusable rule blocks inside profiles.

## Extends

`extends` names a parent recipe: `builtin:NAME` or a path relative to the
extending file. A profile's `actor.recipe` and `needy build --recipe` take the
same references. A chain is at most 16 deep and may not cycle. Recipes use
the profiles' merge model (see
[Extends](2026-11-09-profile-and-rules-design.md#extends)):

- Maps merge key by key: `os: { hostname: "x-{hash}" }` keeps the parent's
  `os.image` and `os.kernel`.
- A scalar or list replaces the parent's: `cpu.features.hide: [avx_vnni]`
  replaces the parent's list, and `devices.pci.extra: []` removes its extra
  devices.
- `null` removes the parent's value. Recipes have no prepend lists.

Unknown keys are errors. Values are checked when the recipe loads: `memory`
(`512MiB`, `16GiB`, or a number of MiB), `os.uptime` (a duration) and
`cpu.hypervisor` (`visible` or `hidden`). A relative path such as
`network.metadata.file` resolves against the directory of the recipe that was
loaded, including paths a parent wrote.

## How each field is applied

| Field | Mechanism | When | State |
| --- | --- | --- | --- |
| `cpu.brand`, `cpu.hypervisor` | Patched Firecracker CPUID normalization | VM start | Verified in the proof of concept |
| `cpu.signature` | CPUID leaf 1 EAX modifier | VM start | Verified |
| KVM signature | Clear leaf `0x40000000` | VM start | Verified |
| `cpu.features.hide` | Linux `/proc/cpuinfo` flag names (from `cpufeatures.h`) mapped to CPUID leaf and bit, then cleared; a feature in a CPUID entry the host's KVM does not support is left out with a warning, since the guest cannot see it | VM start | Needs the name-to-bit table |
| `cpu.topology` | vCPU count and topology leaves | VM start | Partly built into Firecracker |
| `memory` | `mem_size_mib`; real | VM start | Native |
| `os.image` | OCI image to ext4 rootfs | Build | Verified |
| `os.kernel.*` | `LOCALVERSION`, `KBUILD_BUILD_USER`, `KBUILD_BUILD_HOST`, `KBUILD_BUILD_TIMESTAMP` | Build | Straightforward |
| `os.hostname` | Rootfs overlay or kernel command line | Build or start | Straightforward |
| `os.uptime` | Time namespace offset for driving processes, plus logs dated to match | Run time | To do |
| `firmware.dmi` | Built-in fake DMI driven by module parameters, or an SMBIOS table added to Firecracker | Build or start | To do |
| `devices.pci.host_bridge` | Firecracker patch for the bridge ID | VM start | Small change |
| `devices.pci.extra` | Config-space-only devices in Firecracker | VM start | Larger work |
| `devices.sensors` | Built-in fake hwmon driven by module parameters; chaos can override readings | VM start; chaos at run time | coretemp verified |
| `network.mac` | Firecracker `guest_mac` | VM start | Native |
| `network.interface` | Kernel command line or udev rule | VM start | Straightforward |
| `network.metadata` | Firecracker MMDS | VM start | Native |

## Raw overrides

Not implemented: `needy-config` rejects a `firecracker:` key until the merge
with the compiled recipe is decided. Each runtime would accept raw
configuration, merged after the recipe is compiled:

```yaml
firecracker:
  cpu_template: ./custom-cpuid.json    # passed to Firecracker as is
  kernel:
    config_fragments: [./extra.config]
    cmdline: "mitigations=off"
```

Order: compile the recipe, apply raw overrides, then run the consistency check.

## Consistency check

A recipe fails more often on contradictions than on single values. The build
reports:

- **Errors** for CPU features the recipe claims but the host lacks, for example
  an AVX-512 Xeon on a 13th-generation Core i5 host.
- **Warnings** for a CPU model that disagrees with its features (the proof of
  concept's Cascade Lake signature still exposed `avx_vnni`).
- Cloud vendor, DMI, MAC prefix, metadata service and interface name that do not
  agree.
- A kernel release that does not belong to the distribution.
- Uptime that does not match log dates.

Built-in recipes are combinations that pass the check. Starting from one and
overriding a few fields keeps a recipe consistent.

## Decisions and open questions

Decided: feature names follow Linux's `/proc/cpuinfo` flags; Arm hosts use
MIDR and ID register fields instead of `cpu.brand` and `cpu.signature`; the
Firecracker patch is maintained in this project for now.

- How hard DMI and extra PCI devices are; being implemented in the proof of
  concept.
