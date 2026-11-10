# Provenance inventory

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
- **fixed**: was REVIEW; the identity fixes of 2026-11-10 made the value
  match its source, and the definition is cited. The row keeps the original
  finding.

Counts: 46 cited items (the second table), 9 `TODO(provenance)` markers and
18 `REVIEW` markers (the first table, 26 rows).

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
| 5 | GCE CPU, Cascade Lake @ 2.20GHz | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:16` | REVIEW | only A2 and G2 GPU types: `WB/20240216002137/https://cloud.google.com/compute/docs/cpu-platforms` |
| 6 | GCE kernel date and build host | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:29` | REVIEW | image says `Wed May  1 02:03:24 UTC 2024`, `lcy02-amd64-115`: <https://launchpad.net/~canonical-kernel-team/+archive/ubuntu/ppa2/+build/28152459> |
| 7 | GCE hostname prefix `instance-` | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:36` | TODO(provenance) | none found |
| 8 | GCE chassis type Other | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:47` | TODO(provenance) | no public type 3 dump |
| 9 | GCE serial `GoogleCloud-{hash}` | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:50` | REVIEW | real serials carry the 32-digit UUID: <https://github.com/sosreport/sos/issues/2699#issuecomment-925894041> |
| 10 | GCE MAC `{seq}` may be 00 or 01 | `crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml:71` | REVIEW | network and gateway of 10.128.0.0/20: `WB/20240105143455/https://cloud.google.com/vpc/docs/subnets` |
| 11 | Dell kernel build host | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:24` | REVIEW | image says `lcy02-amd64-028`: <https://launchpad.net/~canonical-kernel-team/+archive/ubuntu/ppa2/+build/29233125> |
| 12 | Dell BIOS 1.21.0, 06/11/2024 | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:37` | REVIEW | LVFS has 1.20.0 then 1.22.0: `WB/20250620094041/https://fwupd.org/lvfs/devices/com.dell.uefi896e9574.firmware` |
| 13 | Dell board `0C8MHW`, chassis tower | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:43` | REVIEW | 13 probes: boards 0VC8RJ, 0FG47G, 0NNFGG, 0VNM11, ...; all chassis Desktop: <https://github.com/linuxhw/DMI/tree/57b21bc6fdd863380e99c228c22aafcd1509528c/Desktop/Dell/OptiPlex/OptiPlex%207000> |
| 14 | Dell sensor readings | `crates/config/src/builtin/recipes/dell-optiplex-ubuntu-24.04.yaml:56` | TODO(provenance) | none found |
| 15 | Broadwell stepping 0 | `examples/opencode/gce.recipe.yaml:10` | TODO(provenance) | only an unpinned issue page |
| 16 | AWS MAC prefix `0e:` | `crates/core/src/recipe/identity.rs:155` | REVIEW | AWS examples use `02:`: <https://github.com/awsdocs/amazon-ec2-user-guide/blob/e3707c431483ec0ffffcabebd36cf799ba682cd0/doc_source/instancedata-data-retrieval.md#L400> |
| 17 | SMBIOS keys `serial` and `uuid` never reach the guest | `crates/core-actor/src/host/vm.rs:191` and `images/firecracker/needy.patch:63` | fixed | the patch reads `product_serial` and `product_uuid`; needy now sends those, `chassis_vendor` and `board_serial` under the `/sys/class/dmi/id` names, `K/drivers/firmware/dmi-id.c#L41-L59`; test `every_smbios_key_needy_sends_is_one_the_patched_firecracker_reads` |
| 18 | Patch board serial, SMBIOS 3.0, extension byte 2 | `images/firecracker/needy.patch:57` | REVIEW | GCE shows 32-digit serials, SMBIOS 2.4 and "targeted content distribution" (bit 2); the patch sets bit 3 (UEFI), DSP0134 3.7.0 Table 9 |
| 19 | Patch default serial and UUID values | `images/firecracker/needy.patch:65` | TODO(provenance) | none found |
| 20 | fake coretemp idle, load and noise model | `images/kernel/fake_coretemp.c:18` | TODO(provenance) | none found |
| 21 | fake coretemp max 80 C, crit 100 C | `images/kernel/fake_coretemp.c:48` | TODO(provenance) | the real driver reads TjMax: `K/drivers/hwmon/coretemp.c#L268-L328` |
| 22 | fake coretemp file numbering | `images/kernel/fake_coretemp.c:59` | fixed | real Core N is `temp(N+2)`: `K/drivers/hwmon/coretemp.c#L427-L436`; the fake now puts Core N in channel N+1 (`temp(N+2)`) and hides the channels between; checked in an actor with `ls /sys/class/hwmon/*/` and `sensors` |
| 23 | fake coretemp core IDs step by 4 | `images/kernel/fake_coretemp.c:62` | TODO(provenance) | none found |
| 24 | `--device host:mode` | `crates/core-executor/src/provider/docker.rs:248` | fixed | <https://github.com/docker/cli/blob/v28.5.1/cli/command/container/opts.go#L1005-L1016>; `device` follows `validateLinuxPath` (`#L1092-L1130`) and `parseLinuxDevice` (`#L999-L1033`); test `a_device_parses_as_docker_does` |
| 25 | `ENDED = 255` reserved status | `crates/core-executor/src/signals.rs:53` | TODO(provenance) | only the non-normative Advanced Bash-Scripting Guide |
| 26 | "no exit maps to 255" | `crates/core-executor/src/signals.rs:57` | REVIEW | `shell_code` passes `exit 255` through |

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
| `docker run` option mapping | `crates/core-executor/src/provider/docker.rs` | docker/cli v28.5.1 `opts.go#L600-L712`, go-units v0.5.0 `size.go#L86-L88` | moby v28.5.1 `api/swagger.yaml` |
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

- Fix the items marked REVIEW above; each needs an owner decision on the
  correct value.
- `hybrid_cpu` was removed rather than read from CPUID: KVM does not pass
  the bit to a guest, so there is nothing to hide.
