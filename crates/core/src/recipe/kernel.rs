//! The guest kernel's identity as make variables for its build
//! (`images/Dockerfile`, stage `kernel`).
//!
//! `uname -r`, `uname -v`, `/proc/version` and
//! `/proc/sys/kernel/{osrelease,version}` all read `init_uts_ns`, whose
//! `release` and `version` are the compile-time `UTS_RELEASE` and
//! `UTS_VERSION`, so setting those at build time keeps the five consistent.
//! Source: https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/init/version-timestamp.c#L10-L31
//! (v6.18.62), https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/fs/proc/version.c#L10-L17
//! and https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/kernel/utsname_sysctl.c#L93-L106
//! Reference: https://docs.kernel.org/kbuild/kbuild.html#kbuild-build-timestamp

use std::collections::BTreeMap;

use needy_config::recipe::Kernel;
use needy_config::validate::Finding;

/// `__NEW_UTS_LEN`: the kernel refuses a longer release and cuts a longer
/// version.
/// Source: https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/Makefile#L1340-L1348
/// and https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/init/Makefile#L35-L38
const UTS_LEN: usize = 64;

/// The make variables that give the guest kernel the recipe's `uname`
/// identity:
///
/// - `KERNELRELEASE` from `os.kernel.release`. Given on the make command
///   line, it replaces `scripts/setlocalversion` as the source of
///   `UTS_RELEASE`
///   (https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/Makefile#L1279-L1283).
///   `LOCALVERSION` would only append to `6.18.62`.
/// - `KBUILD_BUILD_VERSION` and `KBUILD_BUILD_TIMESTAMP` from
///   `os.kernel.version`. The kernel writes `UTS_VERSION` as
///   `#<KBUILD_BUILD_VERSION> <SMP> <PREEMPT flag> <KBUILD_BUILD_TIMESTAMP>`
///   with the flags from its own configuration
///   (https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/init/Makefile#L27-L38),
///   so `KBUILD_BUILD_VERSION` carries the recipe's build number and flags
///   and the Dockerfile empties the configuration's flags.
/// - `KBUILD_BUILD_USER` and `KBUILD_BUILD_HOST`, and `KBUILD_BUILD_TIMESTAMP`
///   from `os.kernel.build.timestamp` when the recipe has no version.
pub(super) fn build_env(kernel: &Kernel, findings: &mut Vec<Finding>) -> BTreeMap<String, String> {
  let mut env = BTreeMap::new();
  if let Some(release) = &kernel.release {
    // The Makefile writes the release unquoted into a shell command and a C
    // string, and modules look it up as a directory name.
    let printable = release.chars().all(|c| c.is_ascii_alphanumeric() || "._+~-".contains(c));
    if release.len() > UTS_LEN || !printable {
      findings.push(Finding::error(format!(
        "os.kernel.release {release:?} cannot be a kernel release: it must be at most {UTS_LEN} letters, digits and ._+~-"
      )));
    } else {
      env.insert("KERNELRELEASE".to_string(), release.clone());
    }
  }
  if let Some(version) = &kernel.version {
    match split_version(version) {
      Some((build_version, timestamp)) => {
        if version.len() > UTS_LEN {
          findings.push(Finding::warning(format!("os.kernel.version is {} bytes; the kernel keeps the first {UTS_LEN}", version.len())));
        }
        if let Some(other) = kernel.build.timestamp.as_ref().filter(|t| *t != timestamp) {
          findings.push(Finding::warning(format!(
            "os.kernel.build.timestamp {other:?} differs from the date in os.kernel.version; using {timestamp:?}"
          )));
        }
        env.insert("KBUILD_BUILD_VERSION".to_string(), build_version.to_string());
        env.insert("KBUILD_BUILD_TIMESTAMP".to_string(), timestamp.to_string());
      }
      None => findings.push(Finding::warning(format!(
        "os.kernel.version {version:?} is not `#<build> [flags] <date>` as `uname -v` prints it; the kernel keeps its own"
      ))),
    }
  }
  for (key, value) in [
    ("KBUILD_BUILD_USER", &kernel.build.user),
    ("KBUILD_BUILD_HOST", &kernel.build.host),
    ("KBUILD_BUILD_TIMESTAMP", &kernel.build.timestamp),
  ] {
    if let Some(value) = value {
      env.entry(key.to_string()).or_insert_with(|| value.clone());
    }
  }
  env
}

/// Splits a `uname -v` string such as `#20~22.04.1-Ubuntu SMP Wed May  1
/// 02:03:24 UTC 2024` into the build number with its flags
/// (`20~22.04.1-Ubuntu SMP`) and the date. The date is what `LC_ALL=C date`
/// prints, which starts with an English weekday; it keeps its spacing.
fn split_version(version: &str) -> Option<(&str, &str)> {
  const WEEKDAYS: [&str; 7] = ["Mon ", "Tue ", "Wed ", "Thu ", "Fri ", "Sat ", "Sun "];
  let rest = version.strip_prefix('#')?;
  let (at, _) = rest.match_indices(' ').find(|(at, _)| WEEKDAYS.iter().any(|day| rest[at + 1..].starts_with(day)))?;
  let build_version = &rest[..at];
  // `filechk_uts_version` puts the build version and the date in
  // double-quoted shell words.
  if build_version.is_empty() || rest.contains(['"', '$', '`', '\\']) {
    return None;
  }
  Some((build_version, &rest[at + 1..]))
}
