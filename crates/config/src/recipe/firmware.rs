//! Firmware identity: DMI tables.

use serde::Deserialize;

use crate::value::Template;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Firmware {
  pub dmi: Option<Dmi>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dmi {
  pub sys_vendor: Option<String>,
  pub product_name: Option<String>,
  pub product_version: Option<String>,
  pub bios: Bios,
  pub board_vendor: Option<String>,
  pub board_name: Option<String>,
  pub chassis_type: Option<ChassisType>,
  /// The system serial. `{hash}` derives from the session seed, and
  /// `{uuid_hex}` is the system UUID's 32 upper-case hex digits.
  pub serial: Option<Template>,
  /// The baseboard serial, with the same placeholders as `serial`.
  pub board_serial: Option<Template>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bios {
  pub vendor: Option<String>,
  pub version: Option<String>,
  pub date: Option<String>,
}

/// An SMBIOS chassis type, as its number or a name such as `tower`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ChassisType {
  Code(u32),
  Name(String),
}
