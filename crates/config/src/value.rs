//! Values users write in profiles and recipes: durations, sizes, word lists,
//! globs and templates. Each one checks its form when the document loads, so
//! a typo fails at load time instead of when a run uses it.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

/// A value that does not have the form its type needs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValueError(String);

/// A span of time: `500ms`, `2s`, `10m` or `3d4h`. A bare number is seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration(std::time::Duration);

impl Duration {
  pub const fn from_secs(secs: u64) -> Self {
    Self(std::time::Duration::from_secs(secs))
  }

  fn from_secs_f64(secs: f64) -> Result<Self, ValueError> {
    std::time::Duration::try_from_secs_f64(secs).map(Self).map_err(|_| ValueError(format!("invalid duration {secs}")))
  }
}

impl Deref for Duration {
  type Target = std::time::Duration;

  fn deref(&self) -> &std::time::Duration {
    &self.0
  }
}

impl From<Duration> for std::time::Duration {
  fn from(value: Duration) -> Self {
    value.0
  }
}

impl FromStr for Duration {
  type Err = ValueError;

  fn from_str(text: &str) -> Result<Self, ValueError> {
    let text = text.trim();
    if let Ok(secs) = text.parse::<f64>() {
      return Self::from_secs_f64(secs);
    }
    humantime::parse_duration(text).map(Self).map_err(|err| ValueError(format!("invalid duration {text:?}: {err}")))
  }
}

impl<'de> Deserialize<'de> for Duration {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl Visitor<'_> for V {
      type Value = Duration;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a duration such as 500ms, 2s, 10m or 3d4h, or a number of seconds")
      }

      fn visit_u64<E: de::Error>(self, secs: u64) -> Result<Duration, E> {
        Ok(Duration::from_secs(secs))
      }

      fn visit_i64<E: de::Error>(self, secs: i64) -> Result<Duration, E> {
        u64::try_from(secs).map(Duration::from_secs).map_err(|_| E::custom(format!("invalid duration {secs}")))
      }

      fn visit_f64<E: de::Error>(self, secs: f64) -> Result<Duration, E> {
        Duration::from_secs_f64(secs).map_err(E::custom)
      }

      fn visit_str<E: de::Error>(self, text: &str) -> Result<Duration, E> {
        text.parse().map_err(E::custom)
      }
    }
    d.deserialize_any(V)
  }
}

/// A memory size: `512MiB`, `16GiB`, `2G`. A bare number is MiB.
///
/// Written by hand because the size crates read a bare number as bytes and
/// `G` as 10^9; recipes have always meant MiB and GiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize {
  bytes: u64,
}

const MIB: u64 = 1 << 20;

impl ByteSize {
  pub const fn from_mib(mib: u64) -> Self {
    Self { bytes: mib * MIB }
  }

  pub const fn bytes(&self) -> u64 {
    self.bytes
  }

  /// The size in whole MiB, rounded to the nearest.
  pub const fn mib(&self) -> u64 {
    (self.bytes + MIB / 2) / MIB
  }
}

impl FromStr for ByteSize {
  type Err = ValueError;

  fn from_str(text: &str) -> Result<Self, ValueError> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().map_err(|_| ValueError(format!("invalid size {text:?}")))?;
    let unit_bytes = match unit.trim() {
      "" | "M" | "MiB" | "MB" => MIB,
      "G" | "GiB" | "GB" => 1 << 30,
      "T" | "TiB" | "TB" => 1 << 40,
      other => return Err(ValueError(format!("invalid size unit {other:?} in {text:?}; use MiB, GiB or TiB"))),
    };
    Ok(Self {
      bytes: (number * unit_bytes as f64).round() as u64,
    })
  }
}

impl<'de> Deserialize<'de> for ByteSize {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl Visitor<'_> for V {
      type Value = ByteSize;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a size such as 512MiB or 16GiB, or a number of MiB")
      }

      fn visit_u64<E: de::Error>(self, mib: u64) -> Result<ByteSize, E> {
        Ok(ByteSize::from_mib(mib))
      }

      fn visit_str<E: de::Error>(self, text: &str) -> Result<ByteSize, E> {
        text.parse().map_err(E::custom)
      }
    }
    d.deserialize_any(V)
  }
}

/// A command's words: a YAML list, or one string split on whitespace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Words(Vec<String>);

impl Words {
  pub fn into_vec(self) -> Vec<String> {
    self.0
  }
}

impl Deref for Words {
  type Target = [String];

  fn deref(&self) -> &[String] {
    &self.0
  }
}

impl Extend<String> for Words {
  fn extend<I: IntoIterator<Item = String>>(&mut self, words: I) {
    self.0.extend(words);
  }
}

impl From<Vec<String>> for Words {
  fn from(words: Vec<String>) -> Self {
    Self(words)
  }
}

impl<'de> Deserialize<'de> for Words {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    /// One list item: a string, or a number or boolean taken as its text.
    struct Word(String);
    impl<'de> Deserialize<'de> for Word {
      fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
          type Value = Word;

          fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a word")
          }

          fn visit_str<E: de::Error>(self, text: &str) -> Result<Word, E> {
            Ok(Word(text.to_string()))
          }

          fn visit_u64<E: de::Error>(self, n: u64) -> Result<Word, E> {
            Ok(Word(n.to_string()))
          }

          fn visit_i64<E: de::Error>(self, n: i64) -> Result<Word, E> {
            Ok(Word(n.to_string()))
          }

          fn visit_f64<E: de::Error>(self, n: f64) -> Result<Word, E> {
            Ok(Word(n.to_string()))
          }

          fn visit_bool<E: de::Error>(self, b: bool) -> Result<Word, E> {
            Ok(Word(b.to_string()))
          }
        }
        d.deserialize_any(V)
      }
    }

    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = Words;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of words or one space-separated string")
      }

      fn visit_unit<E: de::Error>(self) -> Result<Words, E> {
        Ok(Words::default())
      }

      fn visit_str<E: de::Error>(self, text: &str) -> Result<Words, E> {
        Ok(Words(text.split_whitespace().map(str::to_string).collect()))
      }

      fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Words, A::Error> {
        let mut words = Vec::new();
        while let Some(Word(word)) = seq.next_element()? {
          words.push(word);
        }
        Ok(Words(words))
      }
    }
    d.deserialize_any(V)
  }
}

/// A glob pattern in `glob-match` syntax: `*`, `**`, `?`, `{a,b}`, `[ab]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct Glob(String);

impl Glob {
  pub fn new(pattern: impl Into<String>) -> Self {
    Self(pattern.into())
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  pub fn matches(&self, text: &str) -> bool {
    glob_match::glob_match(&self.0, text)
  }

  /// Whether `host` matches this pattern as a host name: case-insensitively
  /// (host names are, RFC 4343), and with `*.example.com` also matching
  /// `example.com` itself. The actor gateway's allowlist and the profile's
  /// `network.allow` both use it, so a profile check and the gateway agree.
  pub fn matches_host(&self, host: &str) -> bool {
    let pattern = self.0.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    pattern.strip_prefix("*.").is_some_and(|domain| domain == host) || glob_match::glob_match(&pattern, &host)
  }
}

impl fmt::Display for Glob {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Text with `{name}` placeholders that are filled in later, such as an
/// executor key `"{run}"` or a hostname `"instance-{hash}"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct Template(String);

impl Template {
  pub fn new(text: impl Into<String>) -> Self {
    Self(text.into())
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// Replaces each `{name}` with its value; unknown placeholders stay as
  /// written.
  pub fn render(&self, values: &[(&str, &str)]) -> String {
    let mut text = self.0.clone();
    for (name, value) in values {
      text = text.replace(&format!("{{{name}}}"), value);
    }
    text
  }
}

impl fmt::Display for Template {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

#[cfg(test)]
#[path = "value_test.rs"]
mod tests;
