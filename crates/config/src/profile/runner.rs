//! Runner selectors: which runner an actor or an executor runs on.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer};

/// Which runner an actor or an executor runs on: `runner: NAME`, or
/// `runner: { labels: [KEY, KEY=VALUE] }` for the one connected runner that
/// carries every label. The control plane resolves it when a run is
/// created (`needy_daemon`'s scheduler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerSelector {
  Name(String),
  /// Never empty.
  Labels(Vec<LabelMatch>),
}

/// One label a runner must carry: `KEY` with any value, or `KEY=VALUE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMatch {
  pub key: String,
  pub value: Option<String>,
}

impl LabelMatch {
  /// Whether a runner with `labels` carries this label.
  pub fn matches(&self, labels: &std::collections::BTreeMap<String, String>) -> bool {
    match (&self.value, labels.get(&self.key)) {
      (_, None) => false,
      (None, Some(_)) => true,
      (Some(want), Some(have)) => want == have,
    }
  }
}

impl fmt::Display for LabelMatch {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.value {
      Some(value) => write!(f, "{}={value}", self.key),
      None => f.write_str(&self.key),
    }
  }
}

impl fmt::Display for RunnerSelector {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Name(name) => write!(f, "runner {name}"),
      Self::Labels(labels) => {
        let labels: Vec<String> = labels.iter().map(LabelMatch::to_string).collect();
        write!(f, "runner with labels {}", labels.join(","))
      }
    }
  }
}

impl std::str::FromStr for LabelMatch {
  type Err = String;

  fn from_str(text: &str) -> Result<Self, String> {
    let (key, value) = match text.split_once('=') {
      Some((key, value)) => (key, Some(value.to_string())),
      None => (text, None),
    };
    if key.is_empty() {
      return Err(format!("a label is KEY or KEY=VALUE, not {text:?}"));
    }
    Ok(Self {
      key: key.to_string(),
      value,
    })
  }
}

/// The two written forms; a mapping takes `labels` only.
#[derive(Deserialize)]
#[serde(untagged)]
enum Written {
  Name(String),
  Labels(Labels),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Labels {
  labels: Vec<String>,
}

impl<'de> Deserialize<'de> for RunnerSelector {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    // NOTICE: an untagged enum reports only "did not match any variant", so
    // the value is read as YAML first and each form gets its own message.
    let value = serde_yaml::Value::deserialize(deserializer)?;
    let written: Written = serde_yaml::from_value(value)
      .map_err(|_| de::Error::custom("a runner selector is a runner's name or { labels: [KEY, KEY=VALUE, ...] }"))?;
    match written {
      Written::Name(name) if name.is_empty() => Err(de::Error::custom("a runner's name is empty")),
      Written::Name(name) => Ok(Self::Name(name)),
      Written::Labels(Labels { labels }) if labels.is_empty() => {
        Err(de::Error::custom("`labels` selects no runner when empty; name at least one label"))
      }
      Written::Labels(Labels { labels }) => {
        let labels = labels.iter().map(|text| text.parse()).collect::<Result<Vec<LabelMatch>, String>>().map_err(de::Error::custom)?;
        Ok(Self::Labels(labels))
      }
    }
  }
}
