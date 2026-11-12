//! A container actor's identity from the host: the recipe's `/proc/cpuinfo`,
//! DMI and hwmon files written into the actor's directory and bind-mounted
//! over the host's, lxcfs's resource views mounted where lxcfs runs, and
//! what of the recipe a container cannot present.
//!
//! The text is `needy_core::recipe::files`'s; this module reads the host and
//! writes files. `root` is the host's root directory (`/`), which tests
//! replace.
//!
//! Under gVisor (`runsc`) the sandbox's kernel synthesizes `/proc` and
//! `/sys` itself: its `/sys/class` has `dmi` and takes a mount at `hwmon`
//! whatever the host has, its `/proc/meminfo`, `/proc/stat`,
//! `/proc/uptime` and `/proc/loadavg` already follow the container, and it
//! has no `/proc/diskstats` or `/proc/swaps` to mount over. So under
//! `runsc` no lxcfs view is mounted. Observed on a Debian 12 cloud VM with
//! runsc `release-20261102.0` (2026-11-11), see the closed-loop results.
//!
//! See the Identity table of
//! `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::path::{Path, PathBuf};

use needy_config::profile::OciRuntime;
use needy_config::recipe::IdentityField;
use needy_core::recipe::{CompiledRecipe, files};

use super::driver::{Error, Gap};

/// Where lxcfs mounts its file system, as its systemd unit starts it.
// Source: https://github.com/lxc/lxcfs/blob/v6.0.4/config/init/systemd/lxcfs.service.in#L10
// (`ExecStart=/usr/bin/lxcfs {{LXCFSTARGETDIR}}`) and
// https://github.com/lxc/lxcfs/blob/v6.0.4/meson.build#L67 (`LXCFSTARGETDIR`
// is `<localstatedir>/lib/lxcfs`, `/var/lib/lxcfs` for distribution builds).
pub const LXCFS: &str = "/var/lib/lxcfs";

/// The resource views lxcfs computes from the container's cgroup, mounted
/// over the host's. Never `/proc/cpuinfo`: the recipe's file covers it.
/// Each is mounted only where lxcfs offers its file; `/proc/loadavg` follows
/// the container only where lxcfs runs with `--enable-loadavg` (`-l`).
// Source: https://github.com/lxc/lxcfs/blob/v6.0.4/README.md?plain=1#L8-L21
// (the files lxcfs virtualizes) and its Docker example,
// https://github.com/lxc/lxcfs/blob/v6.0.4/README.md?plain=1#L100-L112. runc
// allows bind mounts over these `/proc` files only,
// https://github.com/opencontainers/runc/blob/v1.3.0/libcontainer/rootfs_linux.go#L821-L833
// (`checkProcMount`'s `validProcMounts`).
pub const LXCFS_VIEWS: &[&str] = &[
  "/proc/meminfo",
  "/proc/stat",
  "/proc/uptime",
  "/proc/loadavg",
  "/proc/diskstats",
  "/proc/swaps",
  "/sys/devices/system/cpu/online",
];

/// The identity files' directory in the actor's directory `dir`.
fn identity_dir(dir: &Path) -> PathBuf {
  dir.join("identity")
}

/// Where the hwmon files are; the hwmon fault rewrites them.
fn hwmon_dir(dir: &Path) -> PathBuf {
  identity_dir(dir).join("hwmon")
}

/// What of the host the identity files need: where it has DMI, hwmon and
/// lxcfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
  /// `/sys/class/dmi/id` exists, so a directory can be mounted over it; a
  /// container cannot create one in sysfs.
  pub dmi: bool,
  /// `/sys/class/hwmon` exists, for the same reason.
  pub hwmon: bool,
  /// The lxcfs views this host offers.
  pub lxcfs: Vec<&'static str>,
  root: PathBuf,
}

impl Host {
  /// The host whose root directory is `root`.
  pub fn read(root: &Path) -> Self {
    let under = |path: &str| root.join(path.trim_start_matches('/'));
    Self {
      dmi: under("/sys/class/dmi/id").is_dir(),
      hwmon: under("/sys/class/hwmon").is_dir(),
      lxcfs: LXCFS_VIEWS.iter().copied().filter(|view| under(&format!("{LXCFS}{view}")).exists()).collect(),
      root: root.to_path_buf(),
    }
  }

  fn path(&self, path: &str) -> PathBuf {
    self.root.join(path.trim_start_matches('/'))
  }
}

/// Writes the recipe's identity files for the actor in `dir`, with
/// `vcpus` processors, and returns the binds that put them, and lxcfs's
/// views, over the host's. Without a recipe only the sensors and lxcfs's
/// views apply.
pub fn prepare(dir: &Path, recipe: Option<&CompiledRecipe>, vcpus: u32, host: &Host, oci: OciRuntime) -> Result<Vec<String>, Error> {
  let gvisor = oci == OciRuntime::Runsc;
  let mut binds = Vec::new();
  if let Some(recipe) = recipe {
    let identity = identity_dir(dir);
    let cpuinfo = host.path("/proc/cpuinfo");
    let host_cpuinfo = std::fs::read_to_string(&cpuinfo).map_err(|source| Error::Write {
      path: cpuinfo,
      source,
    })?;
    let path = identity.join("cpuinfo");
    write(&path, &files::cpuinfo(&host_cpuinfo, &recipe.cpu, vcpus), 0o444)?;
    binds.push(format!("{}:/proc/cpuinfo:ro", path.display()));
    if let Some(smbios) = &recipe.identity.smbios
      && (host.dmi || gvisor)
    {
      let dmi = identity.join("dmi");
      for (name, content, mode) in files::dmi(smbios) {
        write(&dmi.join(name), &content, mode)?;
      }
      binds.push(format!("{}:/sys/class/dmi/id:ro", dmi.display()));
    }
  }
  // Every actor has sensors, as every VM has the fake coretemp.
  if host.hwmon || gvisor {
    for (name, content) in files::hwmon(recipe.map_or(&[], |r| &r.sensors), vcpus, None) {
      // Writable by the daemon, which rewrites them for faults; the
      // container mounts them read-only.
      write(&hwmon_dir(dir).join(name), &content, 0o644)?;
    }
    binds.push(format!("{}:/sys/class/hwmon:ro", hwmon_dir(dir).display()));
  }
  // gVisor's own views follow the container already.
  for view in host.lxcfs.iter().filter(|_| !gvisor) {
    binds.push(format!("{}:{view}:ro", host.path(&format!("{LXCFS}{view}")).display()));
  }
  Ok(binds)
}

/// Sets every temperature of the actor in `dir` to `temp_mc`, as the hwmon
/// fault does; the files are rewritten in place, so the container sees the
/// change through its bind mount.
pub fn overheat(dir: &Path, temp_mc: i32) -> Result<(), Error> {
  let device = hwmon_dir(dir).join("hwmon0");
  let entries = std::fs::read_dir(&device).map_err(|source| Error::Write {
    path: device.clone(),
    source,
  })?;
  for entry in entries.flatten() {
    let name = entry.file_name().to_string_lossy().into_owned();
    let content = if name.ends_with("_input") {
      format!("{temp_mc}\n")
    } else if name.ends_with("_crit_alarm") {
      format!("{}\n", u8::from(temp_mc >= 100_000))
    } else {
      continue;
    };
    let path = entry.path();
    std::fs::write(&path, content).map_err(|source| Error::Write { path, source })?;
  }
  Ok(())
}

/// Writes `content` to `path` with `mode`, creating its directories.
fn write(path: &Path, content: &str, mode: u32) -> Result<(), Error> {
  use std::os::unix::fs::PermissionsExt;
  let failed = |source| Error::Write {
    path: path.to_path_buf(),
    source,
  };
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(failed)?;
  }
  // A file the last run left read-only is replaced, not written through.
  match std::fs::remove_file(path) {
    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(failed(e)),
    _ => {}
  }
  std::fs::write(path, content).map_err(failed)?;
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(failed)
}

/// What of `recipe` (or of a plain machine, without one) a container run
/// with `oci` on `host` does not present.
pub fn gaps(recipe: Option<&CompiledRecipe>, oci: OciRuntime, host: &Host) -> Vec<Gap> {
  let gvisor = oci == OciRuntime::Runsc;
  let mut gaps = Vec::new();
  if gvisor {
    gaps.push(Gap {
      field: None,
      message: "gVisor has no /proc/diskstats or /proc/swaps, and shows itself in /proc (/proc/gvisor, \
                /proc/sentry-meminfo) and in dmesg"
        .into(),
    });
  } else if host.lxcfs.is_empty() {
    gaps.push(Gap {
      field: None,
      message: format!(
        "lxcfs is not running on this host ({LXCFS} is empty), so /proc/meminfo, /proc/stat, /proc/uptime and the CPU count \
         show the host's resources, not the actor's limits; install and start lxcfs"
      ),
    });
  }
  if !host.hwmon && !gvisor {
    gaps.push(Gap::of(
      IdentityField::Hwmon,
      "this host has no /sys/class/hwmon to cover, so the actor has no sensors and hwmon faults have no effect",
    ));
  }
  if host.hwmon || gvisor {
    // The sensors are there, only their load model is not: no field is
    // missing.
    gaps.push(Gap {
      field: None,
      message: "sensor readings stay at the idle value until a fault sets them; they do not follow load".into(),
    });
  }
  let Some(recipe) = recipe else {
    return gaps;
  };
  let cpu = &recipe.cpu;
  if cpu.brand.is_some() || cpu.signature.is_some() || !cpu.hide.is_empty() || cpu.hypervisor.is_some() {
    gaps.push(Gap::of(IdentityField::Cpuid, "the CPUID instruction shows the host's CPU; /proc/cpuinfo shows the recipe's"));
  }
  if recipe.identity.has_pci() {
    gaps.push(Gap::of(IdentityField::Pci, "PCI IDs are the host's (lspci and /sys/bus/pci)"));
  }
  if recipe.kernel_build_env.get("KERNELRELEASE").is_some_and(|r| !r.is_empty()) {
    let kernel = if gvisor {
      "gVisor's (4.19.0-gvisor)"
    } else {
      "the host's"
    };
    gaps.push(Gap::of(IdentityField::Kernel, format!("uname and /proc/version show {kernel} kernel")));
  }
  if recipe.uptime_offset_secs != 0 {
    gaps.push(Gap::of(IdentityField::Uptime, "the uptime is the container's, without the recipe's offset"));
  }
  if recipe.identity.smbios.is_some() && !host.dmi && !gvisor {
    gaps.push(Gap::of(IdentityField::Dmi, "this host has no /sys/class/dmi/id to cover, so the actor has no DMI"));
  }
  gaps
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
