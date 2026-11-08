use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Deserializer};

/// Glob match: `**` matches any characters including `/`, `*` matches within
/// one path segment, `?` matches one non-`/` character. `**/` also matches no
/// directories at all.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob(&p, &t)
}

fn glob(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            let rest = &p[2..];
            if rest.first() == Some(&'/') && glob(&rest[1..], t) {
                return true;
            }
            (0..=t.len()).any(|i| glob(rest, &t[i..]))
        }
        Some('*') => {
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if glob(rest, &t[i..]) {
                    return true;
                }
                if i == t.len() || t[i] == '/' {
                    return false;
                }
                i += 1;
            }
        }
        Some('?') => !t.is_empty() && t[0] != '/' && glob(&p[1..], &t[1..]),
        Some(c) => !t.is_empty() && t[0] == *c && glob(&p[1..], &t[1..]),
    }
}

/// Parses `500ms`, `2s`, `10m`, `3d4h`; a bare number means seconds.
pub(crate) fn parse_duration(text: &str) -> anyhow::Result<Duration> {
    let text = text.trim();
    if let Ok(secs) = text.parse::<f64>() {
        return Ok(Duration::from_secs_f64(secs));
    }
    let mut total = Duration::ZERO;
    let mut number = String::new();
    let mut chars = text.chars().peekable();
    let mut seen = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let mut unit = c.to_string();
        while let Some(&n) = chars.peek() {
            if n.is_ascii_alphabetic() {
                unit.push(n);
                chars.next();
            } else {
                break;
            }
        }
        let value: f64 = number
            .parse()
            .with_context(|| format!("invalid duration {text:?}"))?;
        number.clear();
        let secs = match unit.as_str() {
            "ms" => value / 1000.0,
            "s" => value,
            "m" => value * 60.0,
            "h" => value * 3600.0,
            "d" => value * 86400.0,
            "w" => value * 604800.0,
            _ => bail!("invalid duration unit {unit:?} in {text:?}"),
        };
        total += Duration::from_secs_f64(secs);
        seen = true;
    }
    if !number.is_empty() || !seen {
        bail!("invalid duration {text:?}");
    }
    Ok(total)
}

pub(crate) fn de_opt_duration<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Duration>, D::Error> {
    let value = Option::<serde_yaml::Value>::deserialize(d)?;
    match value {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(serde_yaml::Value::Number(n)) => n
            .as_f64()
            .map(|s| Some(Duration::from_secs_f64(s)))
            .ok_or_else(|| serde::de::Error::custom("invalid duration")),
        Some(serde_yaml::Value::String(s)) => parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(_) => Err(serde::de::Error::custom("invalid duration")),
    }
}

/// splitmix64: a small, stable mixer for deriving values from a seed.
pub(crate) fn mix(seed: u64, salt: u64) -> u64 {
    let mut z = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Expands a leading `~` to `/home/<user>`.
pub(crate) fn expand_home(path: &str, user: &str) -> String {
    if path == "~" {
        format!("/home/{user}")
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("/home/{user}/{rest}")
    } else {
        path.to_string()
    }
}

pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("**/pi/dist/cli.js", "/opt/pi/dist/cli.js"));
        assert!(glob_match("**/pi/dist/cli.js", "pi/dist/cli.js"));
        assert!(glob_match("/workspace/**", "/workspace/a/b"));
        assert!(glob_match(
            "/workspace/**/*.json",
            "/workspace/package.json"
        ));
        assert!(glob_match("*.js", "a.js"));
        assert!(!glob_match("*.js", "dir/a.js"));
        assert!(glob_match("registry.*.org", "registry.npmjs.org"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(
            parse_duration("3d4h").unwrap(),
            Duration::from_secs(3 * 86400 + 4 * 3600)
        );
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
