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

#[test]
fn gateway_blocked_cidrs_accepts_valid_v4_and_v6_and_rejects_invalid() {
  let mut cfg = Config::default();
  cfg.daemon.gateway_blocked_cidrs = vec![
    "10.0.0.0/8".into(),
    "192.168.0.0/16".into(),
    "fc00::/7".into(),
  ];
  cfg.validate().expect("valid CIDRs should pass");

  for bad in [
    "10.0.0.1",
    "10.0.0.0/33",
    "fc00::/129",
    "not-an-ip/8",
    "10.0.0.0/abc",
  ] {
    let mut cfg = Config::default();
    cfg.daemon.gateway_blocked_cidrs = vec![bad.into()];
    let err = cfg.validate().expect_err(&format!("{bad} must be rejected"));
    assert_eq!(err.field(), "daemon.gateway_blocked_cidrs");
  }
}

#[test]
fn layer_registry_reads_from_the_settings_file_and_refuses_an_empty_value() {
  let cfg: Config = serde_yaml::from_str("version: 1\ndaemon:\n  layer_registry: registry.example.com/needy/layers\n").unwrap();
  cfg.validate().unwrap();
  assert_eq!(cfg.daemon.layer_registry.as_deref(), Some("registry.example.com/needy/layers"));

  let mut cfg = Config::default();
  cfg.daemon.layer_registry = Some(String::new());
  let err = cfg.validate().expect_err("an empty layer registry must be rejected");
  assert_eq!(err.field(), "daemon.layer_registry");
}
