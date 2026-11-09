use std::time::Duration;

use serde::{Deserialize, Deserializer};

pub use glob_match::glob_match;

/// Parses a duration such as `500ms`, `2s`, `10m` or `3d4h`; a bare number
/// means seconds.
pub fn parse_duration(text: &str) -> anyhow::Result<Duration> {
  let text = text.trim();
  if let Ok(secs) = text.parse::<f64>() {
    return Ok(Duration::from_secs_f64(secs));
  }
  Ok(humantime::parse_duration(text)?)
}

pub fn de_opt_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
  let value = Option::<serde_yaml::Value>::deserialize(d)?;
  match value {
    None | Some(serde_yaml::Value::Null) => Ok(None),
    Some(serde_yaml::Value::Number(n)) => {
      n.as_f64().map(|s| Some(Duration::from_secs_f64(s))).ok_or_else(|| serde::de::Error::custom("invalid duration"))
    }
    Some(serde_yaml::Value::String(s)) => parse_duration(&s).map(Some).map_err(serde::de::Error::custom),
    Some(_) => Err(serde::de::Error::custom("invalid duration")),
  }
}

/// splitmix64: a small mixer for deriving values from a seed. It is kept here
/// rather than taken from `rand` because its output must never change between
/// versions; replays depend on it.
pub fn mix(seed: u64, salt: u64) -> u64 {
  let mut z = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
  z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// Expands a leading `~` to `/home/<user>`, the profile user's home on every
/// side.
pub fn expand_home(path: &str, user: &str) -> String {
  let home = format!("/home/{user}");
  shellexpand::tilde_with_context(path, || Some(home.as_str())).into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn globs() {
    assert!(glob_match("**/pi/dist/cli.js", "/opt/pi/dist/cli.js"));
    assert!(glob_match("**/pi/dist/cli.js", "pi/dist/cli.js"));
    assert!(glob_match("/workspace/**", "/workspace/a/b"));
    assert!(glob_match("/workspace/**/*.json", "/workspace/package.json"));
    assert!(glob_match("*.js", "a.js"));
    assert!(!glob_match("*.js", "dir/a.js"));
    assert!(glob_match("registry.*.org", "registry.npmjs.org"));
    assert!(!glob_match("a?c", "a/c"));
    assert!(glob_match("{git,hg}", "git"));
  }

  #[test]
  fn durations() {
    assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
    assert_eq!(parse_duration("3d4h").unwrap(), Duration::from_secs(3 * 86400 + 4 * 3600));
    assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
    assert!(parse_duration("3x").is_err());
    assert!(parse_duration("").is_err());
  }

  #[test]
  fn home() {
    assert_eq!(expand_home("~/x", "agent"), "/home/agent/x");
    assert_eq!(expand_home("~", "agent"), "/home/agent");
    assert_eq!(expand_home("./x", "agent"), "./x");
  }
}
