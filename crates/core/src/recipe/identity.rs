//! Machine identity: placeholders filled from the seed, SMBIOS, PCI IDs,
//! chassis numbers, and the check that the identity names one vendor.

use std::collections::BTreeSet;
use std::fmt;

use needy_common::seed::mix;
use needy_config::recipe::{ChassisType, Dmi, Recipe};
use needy_config::validate::Finding;

/// SMBIOS tables of the VM. Every value is already filled from the seed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Smbios {
  pub sys_vendor: Option<String>,
  pub product_name: Option<String>,
  pub product_version: Option<String>,
  pub serial: Option<String>,
  pub board_vendor: Option<String>,
  pub board_name: Option<String>,
  pub board_serial: Option<String>,
  /// The chassis manufacturer: the system's, as on GCE and Dell machines.
  pub chassis_vendor: Option<String>,
  pub bios_vendor: Option<String>,
  pub bios_version: Option<String>,
  pub bios_date: Option<String>,
  /// A version 4 UUID derived from the seed.
  pub uuid: String,
  /// The SMBIOS chassis type number.
  pub chassis_type: Option<u32>,
}

/// A PCI vendor, device and revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciId {
  pub vendor: u16,
  pub device: u16,
  pub revision: u8,
}

/// `vvvv:dddd:rr`, lower-case hex.
impl fmt::Display for PciId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:04x}:{:04x}:{:02x}", self.vendor, self.device, self.revision)
  }
}

/// `{hash}` becomes 8 hex digits and `{seq}` 2 hex digits from 02 to ff,
/// derived from the seed.
// NOTICE: `{seq}` skips 00 and 01 because it fills the host octet of a MAC
// that encodes an IPv4 address (GCE's `42:01:` and the address), and a
// subnet's first two addresses are its network and gateway, which no VM gets:
// https://web.archive.org/web/20240105143455/https://cloud.google.com/vpc/docs/subnets#reserved_ip_addresses_in_every_subnet
pub(super) fn fill_placeholders(text: &str, seed: u64) -> String {
  let seq = 2 + mix(seed, 2) % 254;
  text.replace("{hash}", &format!("{:08x}", mix(seed, 1) as u32)).replace("{seq}", &format!("{seq:02x}"))
}

// Version 4 bits in octet 6 and variant 10 in octet 8: RFC 9562 sections 4.1,
// 4.2 and 5.4, https://www.rfc-editor.org/rfc/rfc9562.html#section-5.4
fn session_uuid(seed: u64) -> String {
  let a = mix(seed, 10);
  let b = mix(seed, 11);
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&a.to_be_bytes());
  bytes[8..].copy_from_slice(&b.to_be_bytes());
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
  format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

// Source: DMTF SMBIOS DSP0134 3.7.0, section 7.4.1, Table 17 "System
// Enclosure or Chassis Types" (01h Other ... 23h Mini PC),
// https://www.dmtf.org/sites/default/files/standards/documents/DSP0134_3.7.0.pdf
// Reference: https://www.dmtf.org/sites/default/files/standards/documents/DSP0134_3.8.0.pdf (same table)
fn chassis_number(value: &ChassisType) -> Option<u32> {
  let name = match value {
    ChassisType::Code(n) => return Some(*n),
    ChassisType::Name(name) => name,
  };
  Some(match name.as_str() {
    "other" => 1,
    "unknown" => 2,
    "desktop" => 3,
    "low-profile-desktop" => 4,
    "tower" => 7,
    "portable" => 8,
    "laptop" => 9,
    "notebook" => 10,
    "all-in-one" => 13,
    "rack" => 23,
    "mini-pc" => 35,
    _ => return None,
  })
}

/// The SMBIOS tables for a recipe's DMI; board fields fall back to the
/// system's, and the chassis vendor is the system's.
pub(super) fn smbios(dmi: &Dmi, seed: u64, findings: &mut Vec<Finding>) -> Smbios {
  let uuid = session_uuid(seed);
  let uuid_hex = uuid.replace('-', "").to_ascii_uppercase();
  let fill = |value: Option<&str>| value.map(|text| fill_placeholders(text, seed).replace("{uuid_hex}", &uuid_hex));
  let chassis_type = dmi.chassis_type.as_ref().and_then(|value| {
    let number = chassis_number(value);
    if number.is_none() {
      findings.push(Finding::warning(format!("unknown chassis_type {value:?}")));
    }
    number
  });
  Smbios {
    sys_vendor: fill(dmi.sys_vendor.as_deref()),
    product_name: fill(dmi.product_name.as_deref()),
    product_version: fill(dmi.product_version.as_deref()),
    serial: fill(dmi.serial.as_ref().map(|t| t.as_str())),
    board_vendor: fill(dmi.board_vendor.as_deref().or(dmi.sys_vendor.as_deref())),
    board_name: fill(dmi.board_name.as_deref().or(dmi.product_name.as_deref())),
    board_serial: fill(dmi.board_serial.as_ref().map(|t| t.as_str())),
    chassis_vendor: fill(dmi.sys_vendor.as_deref()),
    bios_vendor: fill(dmi.bios.vendor.as_deref()),
    bios_version: fill(dmi.bios.version.as_deref()),
    bios_date: fill(dmi.bios.date.as_deref()),
    uuid,
    chassis_type,
  }
}

/// Parses `vvvv:dddd[:rr]`; `revision` applies when the text has none.
pub(super) fn pci_id(text: &str, revision: Option<&str>) -> Result<PciId, String> {
  let invalid = || format!("invalid PCI id {text:?}");
  let parts: Vec<&str> = text.split(':').collect();
  let (vendor, device, rev) = match parts.as_slice() {
    [v, d] => (*v, *d, revision.unwrap_or("00")),
    [v, d, r] => (*v, *d, *r),
    _ => return Err(invalid()),
  };
  let hex = |s: &str, digits: usize| s.len() == digits && s.chars().all(|c| c.is_ascii_hexdigit());
  if !hex(vendor, 4) || !hex(device, 4) || !hex(rev, 2) {
    return Err(invalid());
  }
  Ok(PciId {
    vendor: u16::from_str_radix(vendor, 16).map_err(|_| invalid())?,
    device: u16::from_str_radix(device, 16).map_err(|_| invalid())?,
    revision: u8::from_str_radix(rev, 16).map_err(|_| invalid())?,
  })
}

// Real DMI vendors are `Google`, `Amazon EC2` and `Microsoft Corporation`;
// systemd matches similar strings,
// https://github.com/systemd/systemd/blob/v257/src/basic/virt.c#L172-L190
fn vendor_of_dmi(vendor: &str) -> &'static str {
  let v = vendor.to_ascii_lowercase();
  if v.contains("google") {
    "gce"
  } else if v.contains("amazon") {
    "aws"
  } else if v.contains("microsoft") {
    "azure"
  } else {
    "other"
  }
}

// Source: Azure's OUIs are Microsoft's in
// https://gitlab.com/wireshark/wireshark/-/blob/v4.4.0/epan/manuf-data.c#L3748
// (00:0D:3A), #L9137 (00:22:48) and #L21843 (60:45:BD). GCE's 42:01 is a
// locally administered prefix seen on GCE NICs (see
// crates/config/src/builtin/recipes/gce-ubuntu-22.04.yaml), not an OUI.
// REVIEW: AWS ENI MACs are locally administered with varying first octets;
// AWS's own examples start with 02:,
// https://github.com/awsdocs/amazon-ec2-user-guide/blob/e3707c431483ec0ffffcabebd36cf799ba682cd0/doc_source/instancedata-data-retrieval.md#L400
// so `0e:` names only some of them.
fn vendor_of_mac(mac: &str) -> Option<&'static str> {
  let m = mac.to_ascii_lowercase();
  if m.starts_with("42:01:") {
    Some("gce")
  } else if m.starts_with("0e:") {
    Some("aws")
  } else if ["00:0d:3a:", "00:22:48:", "60:45:bd:"].iter().any(|p| m.starts_with(p)) {
    Some("azure")
  } else {
    None
  }
}

// Ubuntu's kernel flavours: https://launchpad.net/ubuntu/+source/linux-gcp/6.8.0-1073.81,
// https://launchpad.net/ubuntu/+source/linux-aws/6.8.0-1069.74,
// https://launchpad.net/ubuntu/+source/linux-azure/6.8.0-1073.82 and
// https://launchpad.net/ubuntu/+source/linux/6.8.0-49.49 (`-generic`).
fn vendor_of_kernel(release: &str) -> Option<&'static str> {
  if release.ends_with("-gcp") {
    Some("gce")
  } else if release.ends_with("-aws") {
    Some("aws")
  } else if release.ends_with("-azure") {
    Some("azure")
  } else if release.ends_with("-generic") {
    Some("other")
  } else {
    None
  }
}

fn vendor_of_provider(provider: &str) -> &'static str {
  match provider.to_ascii_lowercase().as_str() {
    "gce" | "gcp" | "google" => "gce",
    "aws" | "ec2" | "amazon" => "aws",
    "azure" | "microsoft" => "azure",
    _ => "other",
  }
}

/// Warns when DMI vendor, MAC prefix, metadata provider and kernel flavour
/// name different clouds or machines. `mac` is the filled MAC address.
pub(super) fn check_vendor(recipe: &Recipe, mac: Option<&str>) -> Vec<Finding> {
  let mut sources: Vec<(&str, &'static str)> = Vec::new();
  if let Some(vendor) = recipe.firmware.dmi.as_ref().and_then(|d| d.sys_vendor.as_deref()) {
    sources.push(("DMI vendor", vendor_of_dmi(vendor)));
  }
  if let Some(v) = mac.and_then(vendor_of_mac) {
    sources.push(("MAC prefix", v));
  }
  if let Some(provider) = recipe.network.metadata.as_ref().and_then(|m| m.provider.as_deref()) {
    sources.push(("metadata provider", vendor_of_provider(provider)));
  }
  if let Some(v) = recipe.os.kernel.release.as_deref().and_then(vendor_of_kernel) {
    sources.push(("kernel release", v));
  }
  let distinct: BTreeSet<&str> = sources.iter().map(|(_, v)| *v).collect();
  if distinct.len() > 1 {
    let detail = sources.iter().map(|(what, v)| format!("{what}={v}")).collect::<Vec<_>>().join(", ");
    vec![Finding::warning(format!(
      "machine identity disagrees: {detail}"
    ))]
  } else {
    Vec::new()
  }
}
