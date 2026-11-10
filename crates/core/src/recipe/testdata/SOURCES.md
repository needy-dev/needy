# Recipe test fixtures

## `kvm-cpuid-*.txt`

The CPUID entries, one `leaf subleaf` pair per line, that a host's KVM
reports through `KVM_GET_SUPPORTED_CPUID`. Firecracker builds the guest's CPUID
from exactly these entries and refuses a CPU template that changes any other
one, so they decide which hide modifiers a recipe may emit on that host.

- Reference: the KVM API, `KVM_GET_SUPPORTED_CPUID`,
  <https://www.kernel.org/doc/html/v6.12/virt/kvm/api.html#kvm-get-supported-cpuid>.
- Captured on 2026-11-10 with a Python script that opens `/dev/kvm` and issues
  `KVM_GET_SUPPORTED_CPUID` (`_IOWR(0xAE, 0x05, struct kvm_cpuid2)`, request
  `0xc008ae05`) with room for 256 entries, then prints each entry's
  `function` and `index` as `0x%x 0x%x`. Only the leaf and subleaf are kept;
  the register values, which carry the host's feature bits, were dropped.
  Nothing else was removed or changed.

| File | Machine |
| --- | --- |
| `kvm-cpuid-cascade-lake.txt` | a 3-vCPU Intel Xeon Gold 6252 (Cascade Lake) cloud VM with nested KVM, Debian 13, kernel 6.12 |
| `kvm-cpuid-raptor-lake.txt` | a Linux KVM host with an Intel Core i5-13600KF (Raptor Lake), Debian 13, kernel 7.1 |

Cascade Lake has no CPUID leaf 07H, subleaf 1 (its leaf 07H, subleaf 0 reports
0 in EAX, the highest valid subleaf), so KVM lists none; Raptor Lake has it.
