//! rclone-style ordered include and exclude filters.

use glob_match::glob_match;

/// rclone-style filter: ordered `+ PATTERN` and `- PATTERN` rules, first match
/// wins. A leading `/` anchors at the mapping root; otherwise a pattern may
/// match at any directory level. A trailing `/` matches a directory and its
/// contents. Paths that match no rule are included.
pub fn filter_allows(filter: &[String], rel_path: &str) -> bool {
  let rel = rel_path.trim_start_matches('/');
  for rule in filter {
    let rule = rule.trim();
    let (include, pattern) = match rule.split_at_checked(1) {
      Some(("+", rest)) => (true, rest.trim()),
      Some(("-", rest)) => (false, rest.trim()),
      _ => continue,
    };
    let mut pattern = pattern.to_string();
    if pattern.ends_with('/') {
      pattern.push_str("**");
    }
    let matched = if let Some(anchored) = pattern.strip_prefix('/') {
      glob_match(anchored, rel)
    } else {
      glob_match(&pattern, rel) || rel.match_indices('/').any(|(i, _)| glob_match(&pattern, &rel[i + 1..]))
    };
    if matched {
      return include;
    }
  }
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn filters_follow_rclone() {
    let f: Vec<String> = ["- /auth.json", "- /sessions/**", "- *.log", "+ **"].iter().map(|s| s.to_string()).collect();
    assert!(!filter_allows(&f, "auth.json"));
    assert!(filter_allows(&f, "nested/auth.json"));
    assert!(!filter_allows(&f, "sessions/2026/a.jsonl"));
    assert!(!filter_allows(&f, "deep/dir/x.log"));
    assert!(filter_allows(&f, "config.toml"));
    let dir: Vec<String> = vec!["- node_modules/".into()];
    assert!(!filter_allows(&dir, "pkg/node_modules/x/y.js"));
    assert!(filter_allows(&[], "anything"));
  }
}
