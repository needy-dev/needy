# Host detection test fixtures

## `cpuinfo-two-cores.txt`

Written by hand for `detect_test.rs`, not captured from a machine. It follows
the line format of Linux's `show_cpuinfo` and keeps only the fields the parser
reads, with a shortened `flags` line:

- Source: <https://github.com/torvalds/linux/blob/v6.18/arch/x86/kernel/cpu/proc.c#L70-L130>
  (Linux v6.18: `vendor_id`, `cpu family`, `model`, `model name`, `stepping`,
  `flags`, `bogomips`).
- Reference: <https://man7.org/linux/man-pages/man5/proc_cpuinfo.5.html>.

The values imitate a Google Compute Engine N1 (Broadwell, family 6, model 79)
vCPU, the identity of `examples/opencode/gce.recipe.yaml`. Processor 1 lists
`avx512f`, which no Broadwell CPU has, on purpose: the test checks that only
the first processor's flags count.
