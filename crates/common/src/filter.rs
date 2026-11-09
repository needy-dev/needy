//! rclone-style ordered include and exclude filters.

use crate::util::glob_match;

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
