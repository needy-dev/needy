use super::*;

fn context_yaml(extra: &str) -> String {
  format!(
    "version: 1\ncurrent_context: remote\ncontexts:\n  remote:\n    address: https://needy.example.com\n{extra}    identity:\n      client_id: cli_123\n      name: laptop\n      access: runs\n      paired_at_ms: 1700000000000\n    credential: tok_secret\n"
  )
}

#[test]
fn context_headers_accept_valid_ascii_headers_and_omit_when_empty() {
  let without_headers: Config = serde_yaml::from_str(&context_yaml("")).unwrap();
  without_headers.validate().unwrap();
  assert!(without_headers.contexts["remote"].headers.is_empty());
  let serialized = serde_yaml::to_string(&without_headers).unwrap();
  assert!(!serialized.contains("headers:"), "empty headers should be omitted: {serialized}");

  let with_headers: Config =
    serde_yaml::from_str(&context_yaml("    headers:\n      CF-Access-Client-Id: id-123.access\n      CF-Access-Client-Secret: sec-456\n"))
      .unwrap();
  with_headers.validate().unwrap();
  assert_eq!(with_headers.contexts["remote"].headers["CF-Access-Client-Id"], "id-123.access");
  assert_eq!(with_headers.contexts["remote"].headers["CF-Access-Client-Secret"], "sec-456");
}

#[test]
fn context_headers_reject_authorization_override() {
  let config: Config = serde_yaml::from_str(&context_yaml("    headers:\n      Authorization: Bearer other\n")).unwrap();
  let error = config.validate().unwrap_err();
  assert_eq!(error.field(), "contexts.remote.headers.Authorization");
  assert!(error.to_string().contains("authorization"));
}

#[test]
fn context_headers_reject_case_insensitive_duplicates() {
  let config: Config =
    serde_yaml::from_str(&context_yaml("    headers:\n      CF-Access-Client-Id: one\n      cf-access-client-id: two\n")).unwrap();
  let error = config.validate().unwrap_err();
  assert!(error.to_string().contains("duplicate"));
}

#[test]
fn context_headers_reject_invalid_names_and_values() {
  for yaml in [
    "    headers:\n      \"\": value\n",
    "    headers:\n      bad-bin: value\n",
    "    headers:\n      \"bad name\": value\n",
    "    headers:\n      X-Test: \"\"\n",
    "    headers:\n      X-Test: \"line1\\nline2\"\n",
    "    headers:\n      X-Test: \"non-ascii-é\"\n",
  ] {
    let config: Config = serde_yaml::from_str(&context_yaml(yaml)).unwrap();
    assert!(config.validate().is_err(), "expected validation failure for:\n{yaml}");
  }
}
