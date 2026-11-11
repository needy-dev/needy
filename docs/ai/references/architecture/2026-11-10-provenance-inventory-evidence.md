# Provenance inventory

> **Migration notice (2026-11-10)**: *session* is now *run* in the code, the
> API, the CLI and the trace (slice 2 of
> [Runners and runs](2026-11-10-runner-and-run-design.md)). This note keeps
> the word it was written with: read *session* as *run*.

**Status**: evidence, 2026-11-10. The audit that applied the "Provenance of
Hard-coded and Copied Data" rules in [`AGENTS.md`](../../../../AGENTS.md) to
the existing code. Every item below now carries its source at its definition;
this note lists them in one place and names what still needs the owner.

Method: each value was compared against the source named in the table. Every
permalink was fetched (HTTP 200) and its line anchors were read on 2026-11-10.
Kernel images were checked by downloading the Ubuntu package and decompressing
its `vmlinuz`. The Firecracker patch was applied with BSD patch and GNU patch
2.8 to the v1.17.0 tree; the result is byte-identical to the patch before the
notes were added. No value or behavior was changed.

Status:

- **cited**: the definition carries a permalink and a reference.
- **TODO(provenance)**: no source was found; the owner must say where the
  value came from.
- **REVIEW**: the value disagrees with its source; the evidence is at the
  definition. Nothing was fixed.
- **fixed**: was REVIEW or TODO(provenance); a later fix made the value
  match its source, and the definition is cited. The row keeps the original
  finding.
- **needy's choice**: the value is needy's own; the definition says so and
  cites what constrains it.

Counts: 46 cited items (the second table), 9 `TODO(provenance)` markers and
18 `REVIEW` markers (the first table, 26 rows).

After the identity fixes (`9e1e669` to `d873c23`, 2026-11-10): 16 rows are
fixed; row 5 is still REVIEW and the 9 `TODO(provenance)` rows are unchanged,
as they need the owner. The fixes were run in actors; see "Re-verified after
the identity fixes" in
[`2026-11-09-closed-loop-results-evidence.md`](2026-11-09-closed-loop-results-evidence.md).

After owner decisions 14 and 15 (2026-11-11): row 5 is fixed (the GCE
built-in is now an n2-standard-2) and the 9 `TODO(provenance)` rows are
cited, fixed or marked as needy's choice; no `TODO(provenance)` marker is
left. Every new permalink was fetched (HTTP 200) on 2026-11-11. The new
sources are two public data sets pinned to a commit: Spare Cores Inspector
captures from real GCE VMs (`SC` below,
`https://github.com/SpareCores/sc-inspector-data/blob/80b48a950aa66a70b8710797dc5763dc28f255d4/data/gcp`;
each capture's date is in the `meta.json` next to it) and linuxhw `sensors`
logs of OptiPlex 7000 desktops (`LS` below,
`https://github.com/linuxhw/Sensors/blob/2b4210eb2e8686d7841e8919b019c7e34a1bdb6c/Desktop/Dell/OptiPlex/OptiPlex%207000`;
all four logs: `LT`,
`https://github.com/linuxhw/Sensors/tree/2b4210eb2e8686d7841e8919b019c7e34a1bdb6c/Desktop/Dell/OptiPlex/OptiPlex%207000`).
The definitions cite these files; no serial, UUID or MAC from them was
copied.

Shorthands: `K` = `https://github.com/torvalds/linux/blob/v6.18`, `FC` =
`https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58`
(v1.17.0), `WB` = `https://web.archive.org/web`.

## Items that need the owner

| # | Item | File | Status | Evidence |
| --- | --- | --- | --- | --- |
| 1 | `sse3` feature name | `crates/core/src/recipe/cpu.rs:49` | fixed | Linux shows `pni`: `K/arch/x86/include/asm/cpufeatures.h#L106`; the table now says `pni`; test `sse3_is_named_pni_as_in_proc_cpuinfo` on the captured `testdata/cpuinfo-raptor-lake.txt` |
| 2 | `hybrid_cpu` feature name | `crates/core/src/recipe/cpu.rs:111` | fixed | no quoted name, never in `/proc/cpuinfo`: `K/arch/x86/include/asm/cpufeatures.h#L425`; KVM never passes the bit to a guest, `K/arch/x86/kvm/cpuid.c#L967-L985` (bit 15 clear in `KVM_GET_SUPPORTED_CPUID` on the hybrid host). The entry and its `newer` checks were removed; test `hybrid_cpu_is_not_a_recipe_feature` |
| 3 | `hybrid_cpu` in the Raptor Lake test flags | `crates/core/src/recipe/mod_test.rs:14` | fixed | the hand-written flags were replaced by the captured `testdata/cpuinfo-raptor-lake.txt` |
| 4 | `hybrid_cpu` in the Broadwell hide list | `examples/opencode/gce.recipe.yaml:13` | fixed | removed from the hide list, as item 2 |
| 5 | GCE CPU, Cascade Lake @ 2.20GHz | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:16` | fixed | only A2 and G2 GPU types: `WB/20240216002137/https://cloud.google.com/compute/docs/cpu-platforms`. Now an n2-standard-2 (owner decision 14): `Intel(R) Xeon(R) CPU @ 2.80GHz`, 6/85/7, 1 core of 2 threads, `SC/n2-standard-2/0/lscpu/stdout` (2025-01-10); 2 vCPUs and 8 GB, Cascade Lake the default up to 80 vCPUs, `WB/20241231005450/https://cloud.google.com/compute/docs/general-purpose-machines`; every feature table entry its flags lack is hidden (pku, ospke, umip, rdpid, sha_ni and the post-Cascade-Lake ones) and avx512_vnni is required. TSX (hle, rtm) is not required and the actor has no SMT; both are `TODO` at the recipe. Tests `the_gce_builtin_is_the_cited_n2_standard_2` and `the_gce_builtin_on_a_raptor_lake_host_needs_avx512_and_hides_the_rest` |
| 6 | GCE kernel date and build host | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:29` | fixed | image says `Wed May  1 02:03:24 UTC 2024`, `lcy02-amd64-115`: <https://launchpad.net/~canonical-kernel-team/+archive/ubuntu/ppa2/+build/28152459>; the recipe now says the same; test `the_gce_builtin_kernel_is_the_cited_ubuntu_build` |
| 7 | GCE hostname prefix `instance-` | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:36` | cited | Google documents no default; a public Console-labelled (`goog-ec-src=vm_add-gcloud`) create command names the VM `instance-` and its creation time as `YYYYMMDD-HHMMSS`: <https://github.com/pradeepgadde/blog/blob/13557403fdef8335afe2833c8150ac7e602ada3c/_posts/2024-05-17-gcp-gce-windows.md?plain=1#L36-L38>. The timestamp suffix is a `TODO(hostname-time)`: recipes have no time placeholder |
| 8 | GCE chassis type Other | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:47` | cited | `dmidecode` type 3 `Type: Other` on an n2-standard-2: `SC/n2-standard-2/0/dmidecode/stdout` (2026-07-31); test `the_gce_builtin_is_the_cited_n2_standard_2` |
| 9 | GCE serial `GoogleCloud-{hash}` | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:50` | fixed | real serials carry the 32-digit UUID: <https://github.com/sosreport/sos/issues/2699#issuecomment-925894041>; now `GoogleCloud-{uuid_hex}` and `Board-GoogleCloud-{uuid_hex}`; test `the_gce_builtin_serials_carry_the_system_uuid` |
| 10 | GCE MAC `{seq}` may be 00 or 01 | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:71` | fixed | network and gateway of 10.128.0.0/20: `WB/20240105143455/https://cloud.google.com/vpc/docs/subnets`; `{seq}` is now 02 to ff; test `the_gce_builtin_mac_is_never_the_subnets_network_or_gateway` |
| 11 | Dell kernel build host | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:24` | fixed | image says `lcy02-amd64-028`: <https://launchpad.net/~canonical-kernel-team/+archive/ubuntu/ppa2/+build/29233125>; the recipe now says the same; test `the_dell_builtin_matches_its_cited_probe_and_kernel_build` |
| 12 | Dell BIOS 1.21.0, 06/11/2024 | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:37` | fixed | LVFS has 1.20.0 then 1.22.0: `WB/20250620094041/https://fwupd.org/lvfs/devices/com.dell.uefi896e9574.firmware`; now 1.7.3, 10/27/2022, the cited probe 089EBFAFFD4C, which LVFS also lists; same test |
| 13 | Dell board `0C8MHW`, chassis tower | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:43` | fixed | 13 probes: boards 0VC8RJ, 0FG47G, 0NNFGG, 0VNM11, ...; all chassis Desktop: <https://github.com/linuxhw/DMI/tree/57b21bc6fdd863380e99c228c22aafcd1509528c/Desktop/Dell/OptiPlex/OptiPlex%207000>; now 0VC8RJ and desktop, the cited probe; same test |
| 14 | Dell sensor readings | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:56` | fixed | the Ubuntu 22.04 OptiPlex 7000 log `LS/7241B617E4E3/UBUNTU-22.04/5.17.0-051700-GENERIC/X86_64/6AAE7C23AD`: cores 32 to 37 C (Core 4 34 C), nvme 32.9 C; none of the 4 logs has acpitz (`LT` tree). coretemp 34 C kept, nvme now 32.9 C, acpitz removed; load gain 6 and noise 1 are the fake's own model (was 7 and 1.5, which the fake never used). Only coretemp's idle value reaches the guest: `TODO(sensor-model)`. Test `the_dell_builtin_sensors_are_the_cited_optiplex_7000_log` |
| 15 | Broadwell stepping 0 | `examples/opencode/gce.recipe.yaml:10` | cited | `lscpu` on an e2-medium: `Intel(R) Xeon(R) CPU @ 2.20GHz`, model 79, stepping 0, `SC/e2-medium/0/lscpu/stdout` (2024-06-20). The example now sets `require: []`, as the built-in's avx512_vnni is no Broadwell feature |
| 16 | AWS MAC prefix `0e:` | `crates/core/src/recipe/identity.rs:155` | fixed | AWS examples use `02:`: <https://github.com/awsdocs/amazon-ec2-user-guide/blob/e3707c431483ec0ffffcabebd36cf799ba682cd0/doc_source/instancedata-data-retrieval.md#L400>; the check now takes `02:`; test `an_aws_mac_from_awss_documentation_disagrees_with_a_gce_identity` |
| 17 | SMBIOS keys `serial` and `uuid` never reach the guest | `crates/core-actor/src/host/vm.rs:191` and `images/firecracker/needy.patch:63` | fixed | the patch reads `product_serial` and `product_uuid`; needy now sends those, `chassis_vendor` and `board_serial` under the `/sys/class/dmi/id` names, `K/drivers/firmware/dmi-id.c#L41-L59`; test `every_smbios_key_needy_sends_is_one_the_patched_firecracker_reads` |
| 18 | Patch board serial, SMBIOS 3.0, extension byte 2 | `images/firecracker/needy.patch:57` | fixed | GCE shows 32-digit serials, SMBIOS 2.4 and "targeted content distribution" (bit 2); the patch sets bit 3 (UEFI), DSP0134 3.7.0 Table 9. Now a 32-digit default board serial, a 2.4 `_SM_` entry point (UUID stored in the order written, as 2.4 requires, `K/drivers/firmware/dmi_scan.c#L254-L263`) and bit 2; test `the_patchs_default_serials_carry_its_default_uuid_as_gce_does`, `dmidecode` in an actor. BIOS revision 1.0 and the board asset tag are left as a `TODO` |
| 19 | Patch default serial and UUID values | `images/firecracker/needy.patch:65` | needy's choice | made up so no real VM's identity is copied (Private Data); the format is the dumps' (sos issue, `SC/n2-standard-2/0/dmidecode/stdout`), whose UUIDs carry no RFC 9562 version (nibbles 5 and f), so the default's nibble 9 is as plausible |
| 20 | fake coretemp idle, load and noise model | `images/kernel/fake_coretemp.c:18` | fixed | the 38 C default is the package reading in `LS/7241B617E4E3/...`; readings are now whole degrees, as the real driver's are (`K/drivers/hwmon/coretemp.c#L408`) and every log shows, with under 1 C of noise (was up to 1.5 C in millidegrees); 6 C per unit of load (at most 50 C) and 0.7 C per core are needy's choice, bounded below crit 100 C for 8 cores |
| 21 | fake coretemp max 80 C, crit 100 C | `images/kernel/fake_coretemp.c:48` | cited | the real driver reads TjMax: `K/drivers/hwmon/coretemp.c#L268-L328`; all 4 OptiPlex 7000 logs show `high = +80.0 C, crit = +100.0 C` (`LT`) |
| 22 | fake coretemp file numbering | `images/kernel/fake_coretemp.c:59` | fixed | real Core N is `temp(N+2)`: `K/drivers/hwmon/coretemp.c#L427-L436`; the fake now puts Core N in channel N+1 (`temp(N+2)`) and hides the channels between; checked in an actor with `ls /sys/class/hwmon/*/` and `sensors` |
| 23 | fake coretemp core IDs step by 4 | `images/kernel/fake_coretemp.c:62` | cited | the i7-12700's performance cores are Core 0 to Core 28 in steps of 4 in 3 of the 4 logs (`LT`); the fourth, a 6-core CPU, numbers 0 to 5 |
| 24 | `--device host:mode` | `crates/core-executor/src/provider/docker.rs:248` | fixed | <https://github.com/docker/cli/blob/v28.5.1/cli/command/container/opts.go#L1005-L1016>; `device` follows `validateLinuxPath` (`#L1092-L1130`) and `parseLinuxDevice` (`#L999-L1033`); test `a_device_parses_as_docker_does` |
| 25 | `ENDED = 255` reserved status | `crates/core-executor/src/signals.rs:53` | needy's choice | not reserved anywhere; constrained by the 8 bits of a wait status (POSIX.1-2024 `exit`, <https://pubs.opengroup.org/onlinepubs/9799919799/functions/exit.html>) and the precedent of ssh(1) exiting 255 on its own failure (<https://github.com/openssh/openssh-portable/blob/V_10_6_P2/ssh.1#L1578-L1581>); the comment no longer claims a shell reserves 255 |
| 26 | "no exit maps to 255" | `crates/core-executor/src/signals.rs:57` | fixed | `shell_code` passes `exit 255` through; the comment now says so (no behavior change, no test) |

## Cited items

| Item | File | Source permalink | Reference |
| --- | --- | --- | --- |
| CPUID feature table (leaf, register, bit, name) | `crates/core/src/recipe/cpu.rs` FEATURES | `K/arch/x86/include/asm/cpufeatures.h#L105-L136`, `#L232-L263`, `#L311-L327`, `#L386-L408`, `#L415-L439` | Intel SDM Vol. 2A 253666-088US |
| AVX-VNNI and AVX512-BF16 subleaf 1 | `crates/core/src/recipe/cpu.rs` FEATURES | SDM Vol. 2A 253666-088US Table 3-17 (cited before this audit) | Intel SDM page |
| CPUID leaf 1 EAX encoding | `crates/core/src/recipe/cpu.rs` `encode_signature` | SDM Vol. 2A 253666-088US Figure 3-6, p. 3-229 | Intel SDM page |
| Firecracker CPU template rules | `crates/core/src/recipe/cpu.rs` `cpu_template` | `FC/src/vmm/src/cpu_config/x86_64/mod.rs#L43-L75` (cited before this audit) | KVM API, `KVM_GET_SUPPORTED_CPUID` |
| CPU model numbers | `crates/core/src/recipe/cpu.rs` MODELS | `K/arch/x86/include/asm/intel-family.h#L89-L142` | QEMU v10.1.0 `docs/system/cpu-models-x86.rst.inc` |
| CPU model features | `crates/core/src/recipe/cpu.rs` MODELS | `https://github.com/qemu/qemu/blob/v10.1.0/target/i386/cpu.c`, `https://github.com/llvm/llvm-project/blob/llvmorg-21.1.0/llvm/lib/Target/X86/X86.td` | as above |
| `/proc/cpuinfo` line format | `crates/core/src/recipe/host.rs` | `K/arch/x86/kernel/cpu/proc.c#L70-L79`, `#L104-L107` | proc_cpuinfo(5) |
| cpuinfo test fixture (hand-written) | `crates/core-actor/src/host/testdata/SOURCES.md` | `K/arch/x86/kernel/cpu/proc.c#L70-L130` | proc_cpuinfo(5) |
| KVM CPUID fixtures | `crates/core/src/recipe/testdata/SOURCES.md` | capture command and machine roles (before this audit) | KVM API |
| Raptor Lake cpuinfo fixture (captured) | `crates/core/src/recipe/testdata/SOURCES.md` | capture command, machine role and date; `K/arch/x86/kernel/cpu/proc.c#L70-L130` | proc_cpuinfo(5) |
| SMBIOS chassis type numbers | `crates/core/src/recipe/identity.rs` `chassis_number` | DSP0134 3.7.0 section 7.4.1, Table 17 | DSP0134 3.8.0 |
| UUID version 4 bits | `crates/core/src/recipe/identity.rs` `session_uuid` | RFC 9562 sections 4.1, 4.2, 5.4 | RFC 9562 |
| DMI vendor strings | `crates/core/src/recipe/identity.rs` `vendor_of_dmi` | `https://github.com/systemd/systemd/blob/v257/src/basic/virt.c#L172-L190` | public `dmidecode` output |
| Azure MAC OUIs | `crates/core/src/recipe/identity.rs` `vendor_of_mac` | `https://gitlab.com/wireshark/wireshark/-/blob/v4.4.0/epan/manuf-data.c#L3748`, `#L9137`, `#L21843` | IEEE OUI registry via Wireshark |
| Ubuntu kernel flavours | `crates/core/src/recipe/identity.rs` `vendor_of_kernel` | Launchpad linux-gcp, linux-aws, linux-azure and linux version pages | Launchpad |
| GCE DMI strings | `gce-ubuntu-22.04.yaml` | cloud-init `c9b9eef3` `DataSourceGCE.py#L323-L335` | `WB/20240225130331/https://cloud.google.com/compute/docs/instances/detect-compute-engine` |
| GCE CPU signature 85/7 | `gce-ubuntu-22.04.yaml` | Intel spec update 338848 Rev 028, p. 12 | archived CPU platforms page |
| GCE kernel release | `gce-ubuntu-22.04.yaml` | Launchpad linux-gcp-6.5 6.5.0-1020.20~22.04.1 | Launchpad build 28152459 |
| GCE PCI IDs and revisions | `gce-ubuntu-22.04.yaml` | `https://github.com/pciutils/pciids/blob/2393ed647fe52d620f2840e24af5222889dca5ac/pci.ids#L33582`, `#L39850` | archived GCE `lspci` output |
| GCE `ens4` and MAC layout | `gce-ubuntu-22.04.yaml` | systemd v257 `man/systemd.net-naming-scheme.xml#L161-L188` | archived GCE `ip` output, VPC subnets page |
| Dell CPU signature 151/2 | `dell-optiplex-ubuntu-24.04.yaml` | Intel datasheet 655258 Rev 011, Table 100 | linuxhw probe 089EBFAFFD4C |
| Dell DMI vendor and product | `dell-optiplex-ubuntu-24.04.yaml` | linuxhw/DMI `57b21bc6` probes | as left |
| Dell kernel release and version | `dell-optiplex-ubuntu-24.04.yaml` | Launchpad linux 6.8.0-49.49 | Launchpad build 29233125 |
| Dell host bridge 8086:4668:02 | `dell-optiplex-ubuntu-24.04.yaml` | pci.ids `#L39110`, datasheet 655258 Table 102 | archived `lspci` output |
| Dell `enp0s31f6`, OUI b0:7b:25 | `dell-optiplex-ubuntu-24.04.yaml` | systemd v257 `systemd.net-naming-scheme.xml#L596-L602`, Wireshark v4.4.0 `manuf-data.c#L29209` | as left |
| Broadwell GCE identity | `examples/opencode/gce.recipe.yaml` | `K/arch/x86/include/asm/intel-family.h#L89` | archived CPU platforms page |
| Guest kernel base config | `images/kernel/needy.config`, `images/Dockerfile` | `FC/resources/guest_configs/microvm-kernel-ci-x86_64-6.18.config` (byte-identical copy) | `FC/docs/rootfs-and-kernel-setup.md` |
| Fake coretemp names, labels, attributes | `images/kernel/fake_coretemp.c` | `K/drivers/hwmon/coretemp.c#L32`, `#L335-L345`, `#L44-L53` | `K/Documentation/ABI/testing/sysfs-class-hwmon` |
| Load average shift | `images/kernel/fake_coretemp.c` | `K/include/linux/sched/loadavg.h#L15-L23`, `#L43-L44` | as left |
| binfmt_needy ordering and interpreter swap | `images/kernel/binfmt_needy.c` | `K/fs/binfmt_script.c#L121-L138`, `K/fs/exec.c#L91-L99`, `K/fs/binfmt_elf.c#L275` | kernel binfmt-misc admin guide 6.18 |
| Firecracker patch base and hunks | `images/firecracker/needy.patch` | FC v1.17.0, per-hunk upstream lines | release v1.17.0 |
| SMBIOS structures in the patch | `images/firecracker/needy.patch` | DSP0134 3.7.0 Tables 2, 6-9, 11, 12, 14, 15, 17-19 | as left |
| vsock host CID 2, guest CID 3 | `crates/core-actor/src/host/vm.rs`, `crates/core-actor/src/guest/mod.rs`, `crates/core-actor/src/guest/init/network.rs` | `K/include/uapi/linux/vm_sockets.h#L126-L131` | `FC/docs/vsock.md#L114-L126` |
| Guest boot arguments | `crates/core-actor/src/host/vm.rs` | `FC/docs/getting-started.md#L260`, `FC/FAQ.md#L181-L204` | kernel-parameters 6.18 |
| `vm.memfd_noexec = 2` | `crates/core-actor/src/guest/init/mod.rs` | `K/include/linux/pid_namespace.h#L21-L23`, `K/mm/memfd.c#L345-L366` | `K/Documentation/userspace-api/mfd_noexec.rst` |
| `timens_offsets` format | `crates/core-actor/src/guest/init/identity.rs` | `K/fs/proc/base.c#L1671-L1684`, `K/kernel/time/namespace.c#L383-L461` | time_namespaces(7) |
| `/sys/module/.../parameters` | `crates/core-actor/src/guest/init/identity.rs` | `K/Documentation/ABI/stable/sysfs-module` | as left |
| Proxy port 3128 | `crates/core-actor/src/guest/init/network.rs` | `https://github.com/squid-cache/squid/blob/SQUID_7_8/src/Common.am#L14` | Squid `cf.data.pre` |
| Default PATH | `crates/core-actor/src/guest/init/stubs.rs` | moby v28.5.1 `oci/defaults.go#L19`, Debian base-files 13.8+deb13u7 | systemd v257 `path-util.h#L23` |
| Alternate-screen switches | `crates/core-executor/src/relay/terminal.rs` | `https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt` | xterm ctlseqs |
| Restore sequences, kitty keyboard reset | `crates/core-executor/src/relay/terminal.rs` | xterm-412 `ctlseqs.txt#L1068-L1185`, kitty v0.50.0 `keyboard-protocol.rst#L279-L300`, crossterm 0.29 | xterm ctlseqs, kitty docs |
| Fallback `TERM` | `crates/core-executor/src/process/mod.rs` | ncurses-snapshots v6_5_20251227 `misc/terminfo.src#L5486-L5487` | terminfo(5) |
| Windows signal numbers | `crates/core-executor/src/signals.rs` | POSIX.1-2024 `kill` | bash manual "Exit Status" |
| Reset signal list | `crates/core-executor/src/process/platform/unix.rs` | wezterm `f8921727` `pty/src/unix.rs#L228-L255` (portable-pty 0.9.0) | POSIX.1-2024 `exec`, Shell 2.12 |
| Typed `docker:` fields to the container body (2026-11-11) | `crates/core-executor/src/provider/docker.rs` | docker/cli v29.3.0 `opts.go#L604-L722`, `opts/gpus.go#L18-L99`; v28.5.1 `opts.go#L999-L1071` (devices); bollard-stubs 1.53.1-rc.29.3.1 `models.rs` | moby v28.5.1 `api/swagger.yaml` |
| SFTP packet types | `crates/core/src/chaos/sftp.rs` | openssh-portable V_10_6_P2 `sftp.h#L36`, `#L58` | draft-ietf-secsh-filexfer-02 sections 3, 6.3, 7 |
| SFTP status codes and errno mapping | `crates/core/src/chaos/sftp.rs` | sshfs-3.7.6 `sshfs.c#L1766-L1779`, `sftp.h#L89-L95` | as left |

## Not sourced on purpose

needy's own choices carry no external source: vsock ports 7000 and 7001,
queue and buffer sizes, the report's identity command list, and the session
and table layouts. Example profiles reach the model at `127.0.0.1:11434`,
Ollama's default port
(<https://github.com/ollama/ollama/blob/v0.12.0/envconfig/config.go#L18-L20>);
the example files were left as they are.

## Follow-up candidates

- The n2-standard-2 `dmidecode` (`SC`, 2026-07-31) shows a BIOS date of
  07/16/2026 and chassis security status Unknown, where the recipe and patch
  give 01/01/2011 (the sos issue's date) and None; GCE's BIOS date appears to
  move with its firmware, so which one to show needs the owner.
- The actor never sets Firecracker's `smt`, so `threads_per_core` changes
  only the vCPU count (`TODO(actor-smt)` in the GCE built-in).
- The patch's BIOS revision (GCE shows 1.0) and board asset tag (GCE shows
  the UUID) are marked `TODO` in `images/firecracker/needy.patch`.
- `hybrid_cpu` was removed rather than read from CPUID: KVM does not pass
  the bit to a guest, so there is nothing to hide.
