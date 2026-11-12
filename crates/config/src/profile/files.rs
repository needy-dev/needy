//! File mappings: named copy-in, mount and mirror-out between the host, the
//! actor and executors.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};

use super::User;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Files {
  /// Optional gitignore-style file whose lines become `-` filter rules.
  ///
  /// TODO(files): read but not applied yet; copy-in uses each mapping's
  /// `filter` only. Unlocks when the daemon's copy-in reads it.
  pub ignore_file: Option<String>,
  /// The mappings by name; every key other than `ignore_file`.
  pub mappings: BTreeMap<String, FileMapping>,
}

// NOTICE: written by hand rather than with `#[serde(flatten)]` so that each
// mapping is read through the map's own deserializer, which keeps key paths
// such as `files.workspace.from` in errors.
impl<'de> Deserialize<'de> for Files {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = Files;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("file mappings by name")
      }

      fn visit_unit<E: de::Error>(self) -> Result<Files, E> {
        Ok(Files::default())
      }

      fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Files, A::Error> {
        let mut files = Files::default();
        while let Some(name) = map.next_key::<String>()? {
          if name == "ignore_file" {
            files.ignore_file = map.next_value()?;
          } else {
            let mapping = map.next_value()?;
            files.mappings.insert(name, mapping);
          }
        }
        Ok(files)
      }
    }
    d.deserialize_map(V)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMapping {
  pub mode: FileMode,
  pub from: Side,
  pub to: Side,
  /// rclone-style ordered `+` and `-` rules; first match wins.
  #[serde(default)]
  pub filter: Vec<String>,
  #[serde(default)]
  pub writable: bool,
  #[serde(default)]
  pub when: MirrorWhen,
  /// Propagate deletions to the host.
  #[serde(default)]
  pub delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileMode {
  CopyIn,
  Mount,
  MirrorOut,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MirrorWhen {
  Continuous,
  #[default]
  End,
}

/// One side of a file mapping: `host:./x`, `actor:~/x` or
/// `executor:NAME:~/x`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
  Host(String),
  Actor(String),
  Executor { name: String, path: String },
}

impl Side {
  pub fn parse(text: &str) -> Result<Self, String> {
    if let Some(path) = text.strip_prefix("host:") {
      return Ok(Self::Host(path.to_string()));
    }
    if let Some(path) = text.strip_prefix("actor:") {
      return Ok(Self::Actor(path.to_string()));
    }
    if let Some(rest) = text.strip_prefix("executor:") {
      let (name, path) = rest.split_once(':').ok_or_else(|| format!("executor side needs NAME:PATH: {text:?}"))?;
      if name.is_empty() {
        return Err(format!("executor side needs a name: {text:?}"));
      }
      return Ok(Self::Executor {
        name: name.to_string(),
        path: path.to_string(),
      });
    }
    Err(format!("file side must start with host:, actor: or executor:: {text:?}"))
  }

  /// Expands `~` to the profile user's home on the actor side. A host path
  /// keeps `~` for the control host's home, and an executor path for the
  /// executor's home, which only the daemon knows once the instance exists
  /// (`needy-daemon`'s `pool::Instance::expand_home`).
  pub(super) fn expand_home(self, user: &User) -> Self {
    match self {
      Self::Actor(path) => Self::Actor(user.expand_home(&path)),
      side => side,
    }
  }
}

impl<'de> Deserialize<'de> for Side {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    Side::parse(&String::deserialize(d)?).map_err(de::Error::custom)
  }
}
