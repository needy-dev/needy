use super::*;

#[test]
fn durations() {
  let d = |text: &str| text.parse::<Duration>().map(std::time::Duration::from);
  assert_eq!(d("2s").unwrap(), std::time::Duration::from_secs(2));
  assert_eq!(d("10m").unwrap(), std::time::Duration::from_secs(600));
  assert_eq!(d("3d4h").unwrap(), std::time::Duration::from_secs(3 * 86400 + 4 * 3600));
  assert_eq!(d("500ms").unwrap(), std::time::Duration::from_millis(500));
  assert_eq!(d("90").unwrap(), std::time::Duration::from_secs(90));
  assert!(d("3x").is_err());
  assert!(d("").is_err());
  assert!(d("-1").is_err());
}

#[test]
fn sizes() {
  let mib = |text: &str| text.parse::<ByteSize>().map(|s| s.mib());
  assert_eq!(mib("512MiB").unwrap(), 512);
  assert_eq!(mib("2G").unwrap(), 2048);
  assert_eq!(mib("16GiB").unwrap(), 16384);
  assert_eq!(mib("1.5GiB").unwrap(), 1536);
  assert_eq!(mib("512").unwrap(), 512);
  let err = mib("2Q").unwrap_err().to_string();
  assert!(err.contains("\"Q\""), "{err}");
}

#[test]
fn globs() {
  let g = |pattern: &str, text: &str| Glob::new(pattern).matches(text);
  assert!(g("**/pi/dist/cli.js", "/opt/pi/dist/cli.js"));
  assert!(g("**/pi/dist/cli.js", "pi/dist/cli.js"));
  assert!(g("/workspace/**", "/workspace/a/b"));
  assert!(g("/workspace/**/*.json", "/workspace/package.json"));
  assert!(g("*.js", "a.js"));
  assert!(!g("*.js", "dir/a.js"));
  assert!(g("registry.*.org", "registry.npmjs.org"));
  assert!(!g("a?c", "a/c"));
  assert!(g("{git,hg}", "git"));
}

#[test]
fn host_globs_ignore_case() {
  let h = |pattern: &str, host: &str| Glob::new(pattern).matches_host(host);
  assert!(h("api.example.com", "API.Example.COM"));
  assert!(h("*.Example.com", "api.example.COM"));
  assert!(!h("api.example.com", "api.example.org"));
}

#[test]
fn a_wildcard_subdomain_host_glob_also_matches_its_domain() {
  let h = |pattern: &str, host: &str| Glob::new(pattern).matches_host(host);
  assert!(h("*.example.com", "example.com"));
  assert!(h("*.example.com", "api.example.com"));
  assert!(h("*.example.com", "a.b.example.com"));
  assert!(!h("*.example.com", "badexample.com"));
  assert!(h("*", "example.com"));
  assert!(h("{api,cdn}.example.com", "cdn.example.com"));
}

#[test]
fn templates() {
  let key = Template::new("{run}:{cwd}");
  assert_eq!(key.render(&[("run", "abc")]), "abc:{cwd}");
}
