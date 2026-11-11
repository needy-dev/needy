//! A recipe's identity as the files a container actor sees in place of the
//! host's: `/proc/cpuinfo`, the DMI attributes under `/sys/class/dmi/id` and
//! the hwmon sensors under `/sys/class/hwmon`. A VM gets the same identity
//! from its patched Firecracker and guest kernel; a container shares the
//! host's kernel, so the runtime bind-mounts these files over the host's.
//!
//! Pure text: the runtime reads the host's `/proc/cpuinfo` and writes the
//! results.

use super::{CpuView, SensorSpec, Smbios};

/// `/proc/cpuinfo` for `vcpus` processors of the recipe's CPU, `cpu`, built
/// from the host's `/proc/cpuinfo`, `host`: its first processor's block,
/// with the recipe's brand, signature and topology, the recipe's hidden
/// flags removed, and the hypervisor flag as the recipe shows it. What the
/// recipe does not say (microcode, frequency, cache, bugs) stays the host's.
// Reference: the fields, their order and padding are the x86
// `show_cpuinfo`, https://github.com/torvalds/linux/blob/v6.18/arch/x86/kernel/cpu/proc.c#L63-L142
// (`physical id`, `siblings`, `core id`, `cpu cores`, `apicid` and `initial
// apicid` from `show_cpuinfo_core`, #L19-L30), and the flag names are the
// `X86_FEATURE_*` comments of
// https://github.com/torvalds/linux/blob/v6.18/arch/x86/include/asm/cpufeatures.h#L135-L136
// (`hypervisor` is word 4 bit 31, right after `rdrand`, word 4 bit 30, and
// flags print in bit order).
pub fn cpuinfo(host: &str, cpu: &CpuView, vcpus: u32) -> String {
  // Each line as its key with the kernel's padding (`processor\t`,
  // `model\t\t`, `power management`), and its value.
  let block: Vec<(&str, &str)> =
    host.lines().take_while(|line| !line.trim().is_empty()).map(|line| line.split_once(':').unwrap_or((line, ""))).collect();
  let vcpus = vcpus.max(1);
  let threads = cpu.threads_per_core.max(1);
  let cores = (vcpus / threads).max(1);
  let flags = |host_flags: &str| {
    let mut flags: Vec<&str> = host_flags.split_whitespace().filter(|flag| !cpu.hide.iter().any(|hidden| hidden == flag)).collect();
    match cpu.hypervisor {
      Some(false) => flags.retain(|flag| *flag != "hypervisor"),
      Some(true) if !flags.contains(&"hypervisor") => {
        let at = flags.iter().position(|flag| *flag == "rdrand").map_or(flags.len(), |i| i + 1);
        flags.insert(at, "hypervisor");
      }
      _ => {}
    }
    flags.join(" ")
  };
  let mut out = String::new();
  for processor in 0..vcpus {
    for (padded, value) in &block {
      let value = value.trim_start();
      let value = match padded.trim_end() {
        "processor" => processor.to_string(),
        "model name" => cpu.brand.clone().unwrap_or_else(|| value.to_string()),
        "cpu family" => cpu.signature.map_or_else(|| value.to_string(), |(family, _, _)| family.to_string()),
        "model" => cpu.signature.map_or_else(|| value.to_string(), |(_, model, _)| model.to_string()),
        "stepping" => cpu.signature.map_or_else(|| value.to_string(), |(_, _, stepping)| stepping.to_string()),
        "physical id" => "0".to_string(),
        "siblings" => vcpus.to_string(),
        "core id" => (processor / threads).to_string(),
        "cpu cores" => cores.to_string(),
        "apicid" | "initial apicid" => processor.to_string(),
        "flags" => flags(value),
        _ => value.to_string(),
      };
      if value.is_empty() {
        out.push_str(&format!("{padded}:\n"));
      } else {
        out.push_str(&format!("{padded}: {value}\n"));
      }
    }
    out.push('\n');
  }
  out
}

/// The DMI attributes of `/sys/class/dmi/id` for `smbios`, each file's
/// name, content and mode: the value and a newline, as the kernel shows it,
/// an empty line for a value the recipe leaves out, and root-only serial
/// numbers and UUID.
// Source: the attribute names and modes are `dmi-id.c`'s,
// https://github.com/torvalds/linux/blob/v6.18/drivers/firmware/dmi-id.c#L41-L62
// and `sys_dmi_field_show` prints `%s\n`, #L22-L31.
pub fn dmi(smbios: &Smbios) -> Vec<(&'static str, String, u32)> {
  let chassis_type = smbios.chassis_type.map(|n| n.to_string());
  [
    ("bios_date", smbios.bios_date.as_deref()),
    ("bios_vendor", smbios.bios_vendor.as_deref()),
    ("bios_version", smbios.bios_version.as_deref()),
    ("board_name", smbios.board_name.as_deref()),
    ("board_serial", smbios.board_serial.as_deref()),
    ("board_vendor", smbios.board_vendor.as_deref()),
    ("chassis_type", chassis_type.as_deref()),
    ("chassis_vendor", smbios.chassis_vendor.as_deref()),
    ("product_name", smbios.product_name.as_deref()),
    ("product_serial", smbios.serial.as_deref()),
    ("product_uuid", Some(smbios.uuid.as_str())),
    ("product_version", smbios.product_version.as_deref()),
    ("sys_vendor", smbios.sys_vendor.as_deref()),
  ]
  .into_iter()
  .map(|(name, value)| {
    let mode = if matches!(name, "board_serial" | "product_serial" | "product_uuid") {
      0o400
    } else {
      0o444
    };
    (name, format!("{}\n", value.unwrap_or_default()), mode)
  })
  .collect()
}

/// The coretemp sensors for `vcpus` cores as files under one hwmon device
/// directory (`hwmon0`), each path relative to `/sys/class/hwmon` and its
/// content, idle at the `coretemp` entry of `sensors` or, without one, at the
/// guest kernel's default. `override_mc` sets every reading, as the hwmon
/// fault does. Every actor has them, as every VM has the fake coretemp
/// built into its kernel.
///
/// The names, labels, channels, limits and the 700 m°C step per sensor are
/// the guest kernel's fake coretemp (`images/kernel/fake_coretemp.c`, itself
/// from the real driver), so `sensors` reads the same in both actors; the
/// VM's load and noise model is not reproduced: a file's reading is the
/// idle one until a fault sets it.
// Source: `images/kernel/fake_coretemp.c`: one core per online CPU, the
// package in channel 0 (`temp1`) and core `i` in channel `1 + 4 i`, labelled
// `Core {4 i}` (`CORE_ID_STEP`), whole degrees (`reading`), max 80 °C and
// crit 100 °C (`read`); the sysfs names are
// https://github.com/torvalds/linux/blob/v6.18/Documentation/ABI/testing/sysfs-class-hwmon?plain=1#L385-L391
// and #L440-L464, #L536-L544, as that file cites.
pub fn hwmon(sensors: &[SensorSpec], vcpus: u32, override_mc: Option<i32>) -> Vec<(String, String)> {
  const CORE_ID_STEP: u32 = 4;
  // `base_mc`'s default in `images/kernel/fake_coretemp.c`.
  const IDLE_MC: i32 = 38_000;
  let idle_mc = sensors.iter().find(|s| s.chip == "coretemp").map_or(IDLE_MC, |s| s.idle_mc);
  let mut files = vec![("hwmon0/name".to_string(), "coretemp\n".to_string())];
  for sensor in 0..=vcpus.clamp(1, 64) {
    let reading = override_mc.unwrap_or(idle_mc + sensor as i32 * 700) / 1000 * 1000;
    let (channel, label) = if sensor == 0 {
      (0, "Package id 0".to_string())
    } else {
      let core = (sensor - 1) * CORE_ID_STEP;
      (core + 1, format!("Core {core}"))
    };
    let n = channel + 1;
    files.push((format!("hwmon0/temp{n}_label"), format!("{label}\n")));
    files.push((format!("hwmon0/temp{n}_input"), format!("{reading}\n")));
    files.push((format!("hwmon0/temp{n}_max"), "80000\n".into()));
    files.push((format!("hwmon0/temp{n}_crit"), "100000\n".into()));
    files.push((format!("hwmon0/temp{n}_crit_alarm"), format!("{}\n", u8::from(reading >= 100_000))));
  }
  files
}

#[cfg(test)]
#[path = "files_test.rs"]
mod tests;
