//! Command rules, presets and launcher resolvers as users write them. How they
//! apply is `needy-core`'s `rules`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde_yaml::{Mapping, Value};

use super::chaos::Fault;
use crate::value::{Duration, Glob, Words};

/// A command rule: a pattern, conditions and actions.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
  /// Written like the command it matches: `git **`, `"{node,bun} {script} **"`.
  pub matcher: String,
  pub when: When,
  pub actions: Actions,
}

// NOTICE: a rule's actions sit beside `match` and `when` in one mapping.
// `#[serde(flatten)]` would read them, but serde ignores
// `deny_unknown_fields` on flattened structs, so a misspelt action would be
// dropped silently. The rule reads `match` and `when` itself and types the
// remaining keys as `Actions`.
impl<'de> Deserialize<'de> for Rule {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = Rule;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a command rule")
      }

      fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Rule, A::Error> {
        let mut matcher = None;
        let mut when = When::default();
        let mut actions = Mapping::new();
        while let Some(key) = map.next_key::<String>()? {
          match key.as_str() {
            "match" => matcher = Some(map.next_value::<String>()?),
            "when" => when = map.next_value::<Option<When>>()?.unwrap_or_default(),
            _ => {
              let value = map.next_value::<Value>()?;
              actions.insert(Value::String(key), value);
            }
          }
        }
        let matcher = matcher.ok_or_else(|| de::Error::missing_field("match"))?;
        let actions = serde_path_to_error::deserialize(Value::Mapping(actions)).map_err(|err| {
          let path = err.path().to_string();
          let err = err.into_inner();
          if path == "." {
            de::Error::custom(err)
          } else {
            de::Error::custom(format_args!("{path}: {err}"))
          }
        })?;
        Ok(Rule {
          matcher,
          when,
          actions,
        })
      }
    }
    d.deserialize_map(V)
  }
}

/// Conditions that are not positional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct When {
  /// Any of these flags anywhere in the arguments.
  pub has: Vec<String>,
  pub cwd: Option<Glob>,
  pub env: BTreeMap<String, String>,
  /// A command pattern that an ancestor process matches.
  pub under: Option<String>,
  pub tty: Option<bool>,
  /// Globs on the arguments that `{name}` tokens in `match` captured.
  pub captures: BTreeMap<String, Glob>,
}

/// Actions of a command rule or preset.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Actions {
  /// `actor`, an executor name, or `executor` together with `executor:`.
  pub to: Option<String>,
  pub executor: Option<String>,
  pub deny: Option<String>,
  pub timeout: Option<Duration>,
  pub cwd: Option<String>,
  pub args: ArgOps,
  /// Words around the whole command, outermost first.
  pub wrap: Words,
  /// Replaces the command; may use captures.
  pub exec: Option<Words>,
  pub env: EnvOps,
  /// File mappings added to the run's for this command.
  pub files: Vec<String>,
  pub faults: Vec<Fault>,
  /// Presets whose actions apply as if written here.
  #[serde(rename = "use")]
  pub uses: Vec<String>,
  /// Stops evaluation after this rule.
  #[serde(rename = "final")]
  pub stop: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArgOps {
  /// After the command name, before the user's arguments.
  pub prepend: Words,
  /// After the user's arguments.
  pub append: Words,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnvOps {
  pub set: BTreeMap<String, String>,
  /// Set only when unset.
  pub default: BTreeMap<String, String>,
  /// Prepended to list variables such as `PATH`, joined with `:`.
  pub prepend: BTreeMap<String, String>,
  pub append: BTreeMap<String, String>,
  pub unset: Vec<String>,
  /// Start from `pass` only, instead of the inherited environment.
  pub clear: bool,
  pub pass: Vec<String>,
}

/// A user launcher resolver.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolver {
  #[serde(rename = "match")]
  pub matcher: String,
  /// The command to match again after rewriting.
  #[serde(default)]
  pub effective: Option<String>,
  /// Early identity from arguments.
  #[serde(default)]
  pub identity: BTreeMap<String, String>,
  /// Children inherit the placement.
  #[serde(default)]
  pub sticky: bool,
}

/// Placement for launchers whose effective command cannot be known statically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UnknownPolicy {
  #[default]
  Sticky,
  Actor,
  Executor(String),
}

impl<'de> Deserialize<'de> for UnknownPolicy {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let text = String::deserialize(d)?;
    Ok(match text.as_str() {
      "sticky" => Self::Sticky,
      "actor" => Self::Actor,
      _ => Self::Executor(text),
    })
  }
}
