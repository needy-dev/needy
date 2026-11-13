//! Global configuration through the installed command boundary.
#![cfg(unix)]
use std::process::{Command, Output};

fn needy(home: &std::path::Path, args: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_needy"))
    .env_remove("NEEDY_ENDPOINT")
    .env_remove("NEEDY_TOKEN")
    .env_remove("NEEDY_EXECUTION_CONTEXT")
    .env("NEEDY_CONFIG_DIR", home.join("config"))
    .env("NEEDY_STATE_DIR", home.join("state"))
    .env("NEEDY_DATA_DIR", home.join("data"))
    .env("NEEDY_RUNTIME_DIR", home.join("run"))
    .args(args)
    .output()
    .unwrap()
}

// ROOT CAUSE:
//
// Context checks treated every endpoint as a control plane, so a healthy
// standalone executor was reported as unavailable. Inspection must identify
// its role without requiring the control-only caller API.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_context_check_reports_standalone_executor_without_control_identity() {
  use needy_api_client::proto::v1;
  use tonic::{Request, Response, Status};
  struct Executor;
  #[tonic::async_trait]
  impl v1::health_service_server::HealthService for Executor {
    async fn check(&self, _: Request<v1::CheckRequest>) -> Result<Response<v1::CheckResponse>, Status> {
      Ok(Response::new(v1::CheckResponse {
        instance_id: "standalone-instance".into(),
        role: "executor".into(),
        ..Default::default()
      }))
    }
  }
  let home = tempfile::tempdir().unwrap();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(
    tonic::transport::Server::builder()
      .add_service(v1::health_service_server::HealthServiceServer::new(Executor))
      .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
  );
  let result = needy(
    home.path(),
    &[
      "--address",
      &address,
      "config",
      "context",
      "check",
      "-o",
      "json",
    ],
  );
  server.abort();
  assert!(result.status.success(), "{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
  let rows: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
  assert_eq!(rows[0]["name"], "direct");
  let finding = rows[0]["finding"].as_str().unwrap();
  assert!(finding.contains("role=executor"), "{finding}");
  assert!(finding.contains("instance=standalone-instance"), "{finding}");
  assert!(finding.contains("unknown: caller inspection unavailable for executor"), "{finding}");
  assert!(!home.path().join("config/config.yaml").exists());
}

#[test]
fn global_configuration_defaults_to_local_and_mutates_only_typed_keys() {
  let home = tempfile::tempdir().unwrap();
  let initial = needy(home.path(), &["config", "get", "current_context", "-o", "json"]);
  assert!(initial.status.success(), "{}", String::from_utf8_lossy(&initial.stderr));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&initial.stdout).unwrap(), "local");
  assert!(needy(home.path(), &["config", "set", "daemon.heartbeat_interval", "30s"]).status.success());
  let read = needy(home.path(), &["config", "get", "daemon.heartbeat_interval", "-o", "json"]);
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&read.stdout).unwrap(), "30s");
  let before = std::fs::read(home.path().join("config/config.yaml")).unwrap();
  for args in [
    ["config", "set", "current_context", "other"],
    ["config", "set", "contexts.remote.credential", "secret"],
    ["config", "set", "daemon.heartbeat_interval", "0s"],
    ["config", "set", "paths.state", "relative"],
  ] {
    assert!(!needy(home.path(), &args).status.success());
    assert_eq!(std::fs::read(home.path().join("config/config.yaml")).unwrap(), before);
  }
}

#[test]
fn retired_credentials_are_left_untouched_and_never_imported() {
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("config")).unwrap();
  let path = home.path().join("config/credentials.json");
  let original = b"retired format deliberately not parsed";
  std::fs::write(&path, original).unwrap();
  let list = needy(home.path(), &["config", "context", "ls", "-o", "json"]);
  assert!(list.status.success());
  let rows: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
  assert_eq!(rows.as_array().unwrap().len(), 1);
  assert_eq!(rows[0]["name"], "local");
  assert!(!home.path().join("config/config.yaml").exists());
  assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn profile_display_keeps_environment_keys_and_masks_values_and_known_uri_userinfo() {
  let home = tempfile::tempdir().unwrap();
  let profile = home.path().join("profile.yaml");
  std::fs::write(
    &profile,
    r#"
actor:
  env: { ACTOR_KEY: actor-private }
executors:
  native:
    env: { EXECUTOR_KEY: executor-private }
rules:
  - env: { set: { RULE_KEY: rule-private }, default: { DEFAULT_KEY: default-private } }
presets:
  private:
    env: { append: { PRESET_KEY: preset-private } }
service: https://user:uri-private@example.com/
"#,
  )
  .unwrap();
  let output = needy(
    home.path(),
    &[
      "config",
      "profile",
      "show",
      profile.to_str().unwrap(),
      "-o",
      "json",
    ],
  );
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let text = String::from_utf8(output.stdout).unwrap();
  for secret in [
    "actor-private",
    "executor-private",
    "rule-private",
    "default-private",
    "preset-private",
    "uri-private",
  ] {
    assert!(!text.contains(secret), "leaked {secret}");
  }
  let shown: serde_json::Value = serde_json::from_str(&text).unwrap();
  assert_eq!(shown["actor"]["env"]["ACTOR_KEY"], "<redacted>");
  assert_eq!(shown["rules"][0]["env"]["set"]["RULE_KEY"], "<redacted>");
  assert_eq!(shown["presets"]["private"]["env"]["append"]["PRESET_KEY"], "<redacted>");
}

#[test]
fn transport_changes_preserve_identity_and_conflicts_precede_invitation_input() {
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("config")).unwrap();
  write_context(home.path(), "client-id", "test-client", "private-token");
  let selected = needy(home.path(), &["config", "use", "local"]);
  assert!(selected.status.success(), "{}", String::from_utf8_lossy(&selected.stderr));
  let path = home.path().join("config/config.yaml");
  let original: serde_json::Value = serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  assert!(
    needy(
      home.path(),
      &[
        "config",
        "context",
        "set",
        "development",
        "--address",
        "https://new.example.com"
      ]
    )
    .status
    .success()
  );
  let changed: serde_json::Value = serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  assert_eq!(changed["contexts"]["development"]["identity"], original["contexts"]["development"]["identity"]);
  assert_eq!(changed["contexts"]["development"]["credential"], "private-token");
  assert_eq!(changed["current_context"], "local");
  let before = std::fs::read(&path).unwrap();
  for args in [
    vec![
      "config",
      "context",
      "pair",
      "development",
      "--address",
      "https://example.com",
      "--code-file",
      "/missing-invitation",
    ],
    vec![
      "config",
      "context",
      "pair",
      "absent",
      "--replace",
      "--address",
      "https://example.com",
      "--code-file",
      "/missing-invitation",
    ],
  ] {
    let result = needy(home.path(), &args);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--replace"), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read(&path).unwrap(), before);
  }
  let shown = needy(home.path(), &["config", "show", "-o", "json"]);
  assert!(!String::from_utf8_lossy(&shown.stdout).contains("private-token"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_reports_issued_but_unsaved_and_preserves_the_old_identity() {
  use needy_api_client::proto::v1;
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
  };
  use tonic::{Request, Response, Status};
  // The documented persistence boundary is a non-root CLI account. Root can
  // bypass directory write permissions, so cannot prove this OS denial.
  if unsafe { libc::geteuid() } == 0 {
    return;
  }
  #[derive(Clone)]
  struct Issuer {
    directory: std::path::PathBuf,
    calls: Arc<AtomicUsize>,
  }
  #[tonic::async_trait]
  impl v1::client_service_server::ClientService for Issuer {
    async fn pair(&self, _: Request<v1::PairRequest>) -> Result<Response<v1::PairResponse>, Status> {
      self.calls.fetch_add(1, Ordering::SeqCst);
      std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o500)).unwrap();
      Ok(Response::new(v1::PairResponse {
        token: "new-private-bearer".into(),
        client: Some(v1::Client {
          id: "issued-public-id".into(),
          name: "issued-client".into(),
          access: v1::ClientAccess::Runs as i32,
          ..Default::default()
        }),
      }))
    }
    async fn get_caller(&self, _: Request<v1::GetCallerRequest>) -> Result<Response<v1::GetCallerResponse>, Status> {
      Err(Status::unimplemented("unused"))
    }
    async fn create_pairing_code(
      &self,
      _: Request<v1::CreatePairingCodeRequest>,
    ) -> Result<Response<v1::CreatePairingCodeResponse>, Status> {
      Err(Status::unimplemented("unused"))
    }
    async fn list_clients(&self, _: Request<v1::ListClientsRequest>) -> Result<Response<v1::ListClientsResponse>, Status> {
      Err(Status::unimplemented("unused"))
    }
    async fn revoke_client(&self, _: Request<v1::RevokeClientRequest>) -> Result<Response<v1::RevokeClientResponse>, Status> {
      Err(Status::unimplemented("unused"))
    }
  }
  let home = tempfile::tempdir().unwrap();
  let directory = home.path().join("config");
  std::fs::create_dir(&directory).unwrap();
  write_context(home.path(), "old-id", "old-client", "old-private-bearer");
  let selected = needy(home.path(), &["config", "use", "local"]);
  assert!(selected.status.success(), "{}", String::from_utf8_lossy(&selected.stderr));
  let original = std::fs::read(directory.join("config.yaml")).unwrap();
  let invitation = home.path().join("invitation");
  std::fs::write(&invitation, "one-use-invitation").unwrap();
  std::fs::set_permissions(&invitation, std::fs::Permissions::from_mode(0o600)).unwrap();
  let calls = Arc::new(AtomicUsize::new(0));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = format!("http://{}", listener.local_addr().unwrap());
  let service = Issuer {
    directory: directory.clone(),
    calls: calls.clone(),
  };
  let server = tokio::spawn(
    tonic::transport::Server::builder()
      .add_service(v1::client_service_server::ClientServiceServer::new(service))
      .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
  );
  let result = needy(
    home.path(),
    &[
      "config",
      "context",
      "pair",
      "development",
      "--replace",
      "--address",
      &address,
      "--code-file",
      invitation.to_str().unwrap(),
    ],
  );
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  server.abort();
  assert!(!result.status.success());
  let stderr = String::from_utf8_lossy(&result.stderr);
  assert!(stderr.contains("issued but not saved"), "{stderr}");
  assert!(stderr.contains("issued-public-id"), "{stderr}");
  assert!(!stderr.contains("new-private-bearer"));
  assert_eq!(calls.load(Ordering::SeqCst), 1);
  assert_eq!(std::fs::read(directory.join("config.yaml")).unwrap(), original);
}

#[test]
fn malformed_and_unsupported_documents_report_typed_reasons_without_replacing_bytes() {
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("config")).unwrap();
  let path = home.path().join("config/config.yaml");
  for (document, reason) in [
    ("version: 99\n", "config.version_unsupported"),
    ("version: [\n", "config.invalid_yaml"),
    ("daemon:\n  hearbeat_interval: 15s\n", "config.invalid_field"),
  ] {
    std::fs::write(&path, document).unwrap();
    let result = needy(home.path(), &["--diagnostic-format", "json", "config", "show"]);
    assert!(!result.status.success());
    let diagnostic: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(diagnostic["reason"], reason, "{diagnostic}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), document);
  }
}

#[test]
fn concurrent_typed_updates_preserve_each_independent_value_and_private_storage() {
  use std::os::unix::fs::PermissionsExt as _;
  let home = tempfile::tempdir().unwrap();
  let updates = [
    ("daemon.executor_image", "example/image:v1"),
    ("daemon.local_runner", "example-runner"),
    ("daemon.heartbeat_interval", "41s"),
  ];
  let mut children = Vec::new();
  for (key, value) in updates {
    children.push(
      Command::new(env!("CARGO_BIN_EXE_needy"))
        .env("NEEDY_CONFIG_DIR", home.path().join("config"))
        .args(["config", "set", key, value])
        .spawn()
        .unwrap(),
    );
  }
  for child in &mut children {
    assert!(child.wait().unwrap().success());
  }
  for (key, value) in updates {
    let result = needy(home.path(), &["config", "get", key, "-o", "json"]);
    assert!(result.status.success());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(), value);
  }
  assert_eq!(std::fs::metadata(home.path().join("config")).unwrap().permissions().mode() & 0o777, 0o700);
  assert_eq!(std::fs::metadata(home.path().join("config/config.yaml")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn an_explicit_document_selects_its_profiles_without_relocating_runtime_storage() {
  let home = tempfile::tempdir().unwrap();
  let selected = home.path().join("selected");
  std::fs::create_dir_all(selected.join("profiles")).unwrap();
  std::fs::write(selected.join("profiles/good.yaml"), "actor: { driver: linux-firecracker }\nexecutors: { here: { driver: native } }\n")
    .unwrap();
  std::fs::write(selected.join("profiles/broken.yaml"), "actor: [\n").unwrap();
  let file = selected.join("settings.yaml");
  let listed = needy(
    home.path(),
    &[
      "--config",
      file.to_str().unwrap(),
      "config",
      "profile",
      "ls",
      "-o",
      "json",
    ],
  );
  assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
  let rows: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
  assert_eq!(rows.len(), 2);
  assert_eq!(rows[0]["name"], "broken");
  assert!(!rows[0]["finding"].as_str().unwrap().is_empty());
  assert_eq!(rows[1]["name"], "good");
  assert_eq!(rows[1]["finding"], "");
  let shown = needy(
    home.path(),
    &[
      "--config",
      file.to_str().unwrap(),
      "config",
      "profile",
      "show",
      "good",
      "-o",
      "json",
    ],
  );
  assert!(shown.status.success());
  let paths = needy(
    home.path(),
    &[
      "--config",
      file.to_str().unwrap(),
      "config",
      "paths",
      "-o",
      "json",
    ],
  );
  let paths: Vec<serde_json::Value> = serde_json::from_slice(&paths.stdout).unwrap();
  let entry = |name: &str| paths.iter().find(|row| row["name"] == name).unwrap();
  assert_eq!(entry("config_file")["source"], file.display().to_string());
  assert_eq!(entry("config_file")["finding"], "--config");
  assert_eq!(entry("profiles")["source"], selected.join("profiles").display().to_string());
  for (name, directory) in [("state", "state"), ("data", "data"), ("runtime", "run")] {
    assert_eq!(entry(name)["source"], home.path().join(directory).display().to_string());
  }
}

#[test]
fn credential_documents_require_private_regular_files_and_preserve_invalid_sources() {
  use std::os::unix::fs::{PermissionsExt as _, symlink};
  let home = tempfile::tempdir().unwrap();
  std::fs::create_dir(home.path().join("config")).unwrap();
  let path = home.path().join("config/config.yaml");
  let document = "contexts:\n  development:\n    address: https://example.com\n    identity: { client_id: public-id, name: example-client, access: runs, paired_at_ms: 1 }\n    credential: synthetic-private-bearer\n";
  std::fs::write(&path, document).unwrap();
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
  let insecure = needy(home.path(), &["config", "show"]);
  assert!(!insecure.status.success());
  assert!(!String::from_utf8_lossy(&insecure.stderr).contains("synthetic-private-bearer"));
  assert_eq!(std::fs::read_to_string(&path).unwrap(), document);
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
  assert!(needy(home.path(), &["config", "show"]).status.success());
  let moved = home.path().join("private.yaml");
  std::fs::rename(&path, &moved).unwrap();
  symlink(&moved, &path).unwrap();
  assert!(!needy(home.path(), &["config", "show"]).status.success());
  assert_eq!(std::fs::read_to_string(&moved).unwrap(), document);
}

// A listed named profile must be addressable by its listed name, regardless
// of which supported YAML extension stores it.
#[test]
fn named_profiles_resolve_yml_and_reject_duplicate_extensions() {
  let home = tempfile::tempdir().unwrap();
  let profiles = home.path().join("config/profiles");
  std::fs::create_dir_all(&profiles).unwrap();
  let text = "actor: { driver: linux-firecracker }\nexecutors: { here: { driver: native } }\n";
  std::fs::write(profiles.join("task.yml"), text).unwrap();
  let shown = needy(home.path(), &["config", "profile", "show", "task", "-o", "json"]);
  assert!(shown.status.success(), "{}", String::from_utf8_lossy(&shown.stderr));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&shown.stdout).unwrap()["actor"]["driver"], "linux-firecracker");
  std::fs::write(profiles.join("task.yaml"), text).unwrap();
  let ambiguous = needy(
    home.path(),
    &[
      "--diagnostic-format",
      "json",
      "config",
      "profile",
      "show",
      "task",
    ],
  );
  assert!(!ambiguous.status.success());
  let diagnostic: serde_json::Value = serde_json::from_slice(&ambiguous.stderr).unwrap();
  assert_eq!(diagnostic["reason"], "config.invalid_reference");
  let explicit = needy(
    home.path(),
    &[
      "config",
      "profile",
      "show",
      profiles.join("task.yml").to_str().unwrap(),
    ],
  );
  assert!(explicit.status.success());
}

#[test]
fn invalid_typed_mutations_preserve_the_document_and_name_the_field() {
  let home = tempfile::tempdir().unwrap();
  assert!(needy(home.path(), &["config", "set", "daemon.heartbeat_interval", "41s"]).status.success());
  let path = home.path().join("config/config.yaml");
  let original = std::fs::read(&path).unwrap();
  for value in ["[", "[]", "0s"] {
    let output = needy(
      home.path(),
      &[
        "--diagnostic-format",
        "json",
        "config",
        "set",
        "daemon.heartbeat_interval",
        value,
      ],
    );
    assert!(!output.status.success());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["reason"], "config.invalid_field", "{diagnostic}");
    assert_eq!(diagnostic["facts"]["field"], "daemon.heartbeat_interval");
    assert_eq!(std::fs::read(&path).unwrap(), original);
  }
}

fn write_context(home: &std::path::Path, id: &str, name: &str, credential: &str) {
  use std::os::unix::fs::PermissionsExt as _;
  std::fs::set_permissions(home.join("config"), std::fs::Permissions::from_mode(0o700)).unwrap();
  let path = home.join("config/config.yaml");
  let value = serde_json::json!({"version": 1, "current_context": "local", "contexts": {
    "development": {"address":"https://example.com", "credential":credential,
      "identity":{"client_id":id,"name":name,"access":"runs","paired_at_ms":123}}
  }});
  std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn local_configuration_ignores_an_inherited_execution_selection() {
  let home = tempfile::tempdir().unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_needy"))
    .env("NEEDY_CONFIG_DIR", home.path().join("config"))
    .env("NEEDY_EXECUTION_CONTEXT", "invalid inherited selection")
    .args(["config", "show", "-o", "json"])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["current_context"], "local");
}

#[test]
fn redacted_yaml_display_preserves_numeric_scalar_types() {
  let home = tempfile::tempdir().unwrap();
  let output = needy(home.path(), &["config", "show"]);
  assert!(output.status.success());
  let shown: serde_yaml::Value = serde_yaml::from_slice(&output.stdout).unwrap();
  assert_eq!(shown["version"].as_u64(), Some(1));
}
