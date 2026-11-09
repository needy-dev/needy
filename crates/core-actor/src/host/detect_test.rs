use std::path::Path;

use super::*;

#[test]
fn a_cpuinfo_file_reports_the_first_processor() {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/host/testdata/cpuinfo-two-cores.txt");

  let info = cpuinfo(&path).unwrap();

  assert_eq!(info.vendor, "GenuineIntel");
  assert_eq!(info.model_name, "Intel(R) Xeon(R) CPU @ 2.20GHz");
  assert!(info.flags.contains("avx2"), "{:?}", info.flags);
  assert!(!info.flags.contains("avx512f"), "only the first processor's flags count: {:?}", info.flags);
}

#[test]
fn a_missing_cpuinfo_file_is_an_error() {
  let dir = tempfile::tempdir().unwrap();

  assert!(cpuinfo(&dir.path().join("cpuinfo")).is_err());
}
