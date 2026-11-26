use std::time::Duration;

use needy_api_client::Client;
use needy_api_client::proto::v1::{
  CreateJoinTokenRequest, ExecutorDriver, HeartbeatRequest, JoinRequest, JoinTokenState, ListJoinTokensRequest, ListRunnersRequest,
  RemoveRunnerRequest, RevokeJoinTokenRequest, RotateRunnerRequest, RunnerCapabilities, RunnerStatus,
};
use tonic::Code;

use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";

async fn served(heartbeat: Duration) -> Served {
  testing::serve(testing::daemon_with_heartbeat(heartbeat).await, Some(CONTROL_TOKEN)).await
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

fn as_runner(served: &Served, credential: &str) -> Client {
  served.client.clone().with_token(credential).unwrap()
}

async fn create_token(served: &Served, uses: u32) -> String {
  let request = CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses,
    ..Default::default()
  };
  operator(served).runners().create_join_token(request).await.unwrap().into_inner().token
}

fn join(token: &str, name: &str) -> JoinRequest {
  JoinRequest {
    join_token: token.into(),
    name: name.into(),
    labels: [("os".to_string(), "darwin".to_string())].into(),
    capabilities: native(),
  }
}

fn native() -> Option<RunnerCapabilities> {
  Some(RunnerCapabilities {
    executor: vec![ExecutorDriver::Native as i32],
    ..Default::default()
  })
}

/// A heartbeat reports the runner's capabilities and labels whole.
fn heartbeat() -> HeartbeatRequest {
  HeartbeatRequest {
    labels: [("os".to_string(), "darwin".to_string())].into(),
    host: None,
    capabilities: native(),
  }
}

#[tokio::test]
async fn a_runner_joins_beats_rotates_and_is_refused_once_removed() {
  let served = served(Duration::from_secs(15)).await;
  let token = create_token(&served, 1).await;

  // Join, then a heartbeat with the credential.
  let joined = served.client.runners().join(join(&token, "mac")).await.unwrap().into_inner();
  assert_eq!(joined.heartbeat_interval_ms, 15_000);
  let old = joined.credential;
  let beat = as_runner(&served, &old).runners().heartbeat(heartbeat()).await.unwrap().into_inner();
  assert_eq!(beat.credential, "", "no rotation was asked for");
  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].name.as_str(), listed[0].status()), ("mac", RunnerStatus::Online));
  assert_eq!(listed[0].labels["os"], "darwin");

  // Rotate: the next heartbeat hands out a new credential, and the old one
  // works until the new one is used.
  operator(&served).runners().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap();
  let new = as_runner(&served, &old).runners().heartbeat(heartbeat()).await.unwrap().into_inner().credential;
  assert!(!new.is_empty() && new != old);
  as_runner(&served, &new).runners().heartbeat(heartbeat()).await.unwrap();
  let refused = as_runner(&served, &old).runners().heartbeat(heartbeat()).await.unwrap_err();
  assert_eq!(refused.code(), Code::Unauthenticated);
  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert!(!listed[0].rotating);

  // Remove: the credential is refused and the runner is listed as removed.
  operator(&served).runners().remove_runner(RemoveRunnerRequest { name: "mac".into() }).await.unwrap();
  let refused = as_runner(&served, &new).runners().heartbeat(heartbeat()).await.unwrap_err();
  assert_eq!(refused.code(), Code::Unauthenticated);
  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(listed[0].status(), RunnerStatus::Removed);
}

#[tokio::test]
async fn a_spent_or_revoked_join_token_is_refused_as_unauthenticated() {
  let served = served(Duration::from_secs(15)).await;
  let token = create_token(&served, 1).await;
  served.client.runners().join(join(&token, "one")).await.unwrap();
  let revoked = create_token(&served, 1).await;
  let tokens = operator(&served).runners().list_join_tokens(ListJoinTokensRequest {}).await.unwrap().into_inner().join_tokens;
  let id = tokens[1].id.clone();
  operator(&served).runners().revoke_join_token(RevokeJoinTokenRequest { id }).await.unwrap();

  let spent = served.client.runners().join(join(&token, "two")).await.unwrap_err();
  let revoked = served.client.runners().join(join(&revoked, "three")).await.unwrap_err();

  assert_eq!(needy_api_client::diagnostic::from_status(&spent).unwrap().reason, "bootstrap.used");
  assert_eq!(needy_api_client::diagnostic::from_status(&revoked).unwrap().reason, "bootstrap.revoked");
  assert_eq!(spent.code(), Code::Unauthenticated);
  assert!(spent.message().ends_with("has no uses left"), "{}", spent.message());
  assert_eq!(revoked.code(), Code::Unauthenticated);
  assert!(revoked.message().ends_with("was revoked"), "{}", revoked.message());
  let tokens = operator(&served).runners().list_join_tokens(ListJoinTokensRequest {}).await.unwrap().into_inner().join_tokens;
  let states: Vec<_> = tokens.iter().map(|t| t.state()).collect();
  assert_eq!(states, [JoinTokenState::UsedUp, JoinTokenState::Revoked]);
}

#[tokio::test]
async fn a_runner_is_offline_after_three_missed_heartbeats() {
  let served = served(Duration::from_millis(100)).await;
  let token = create_token(&served, 1).await;
  served.client.runners().join(join(&token, "mac")).await.unwrap();

  tokio::time::sleep(Duration::from_millis(350)).await;

  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(listed[0].status(), RunnerStatus::Offline);
}

// ROOT CAUSE:
//
// If the control plane restarted, a runner opened its control stream again
// at once, but `runner ls` showed the last seen time from before the
// restart, and an offline status, until the runner's next heartbeat,
// because only a heartbeat recorded that the runner was seen.
//
// Before the fix, opening the control stream did not touch `last_seen_ms`.
// The fix records the runner as seen when its control stream opens.
#[tokio::test]
async fn a_runner_that_opens_its_control_stream_is_seen_at_once() {
  let served = served(Duration::from_millis(100)).await;
  let token = create_token(&served, 1).await;
  let credential = served.client.runners().join(join(&token, "mac")).await.unwrap().into_inner().credential;
  tokio::time::sleep(Duration::from_millis(350)).await;
  let before = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(before[0].status(), RunnerStatus::Offline);

  let (_reports, rx) = tokio::sync::mpsc::channel(1);
  let _control =
    as_runner(&served, &credential).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();

  let after = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(after[0].status(), RunnerStatus::Online);
  assert!(after[0].last_seen_at_ms >= before[0].last_seen_at_ms + 350, "{} then {}", before[0].last_seen_at_ms, after[0].last_seen_at_ms);
}

#[tokio::test]
async fn the_control_token_is_still_required_for_operators() {
  let served = served(Duration::from_secs(15)).await;

  let status = served.client.runners().list_runners(ListRunnersRequest {}).await.unwrap_err();

  assert_eq!(status.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn static_runners_hot_reload_add_remove_and_forbid_rotate_or_rm() {
  use needy_api_client::proto::v1::RevokeRunnerCredentialRequest;

  let dir = tempfile::tempdir().unwrap();
  let runners_file = dir.path().join("runners.yaml");
  let cred_a = "needy-runner.static-a.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  let cred_b = "needy-runner.static-b.fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
  std::fs::write(
    &runners_file,
    format!("runners:\n  - name: static-a\n    credential: {cred_a}\n    roles: [executor]\n    labels:\n      pool: k8s\n"),
  )
  .unwrap();

  let started = testing::daemon_with(|config| {
    config.heartbeat_interval = Duration::from_secs(15);
    config.runners_file = Some(runners_file.clone());
  })
  .await;
  let served = testing::serve(started, Some(CONTROL_TOKEN)).await;

  // Static runner authenticates, sends heartbeat, and appears in ListRunners and candidates.
  as_runner(&served, cred_a).runners().heartbeat(heartbeat()).await.unwrap();
  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].name, "static-a");
  assert!(listed[0].static_config);
  assert_eq!(listed[0].status(), RunnerStatus::Online);
  assert_eq!(listed[0].labels.get("pool").map(String::as_str), Some("k8s"));

  // Open a control stream for static-a.
  let (_tx, rx) = tokio::sync::mpsc::channel(1);
  let mut control =
    as_runner(&served, cred_a).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  assert!(served.daemon().links.get("static-a").is_some());
  let candidates = served.daemon().candidates().await.unwrap();
  assert!(candidates.iter().any(|c| c.name == "static-a" && c.connected));

  // Rotate, Remove, and Revoke are refused for static runners.
  let rot = operator(&served)
    .runners()
    .rotate_runner(RotateRunnerRequest {
      name: "static-a".into(),
    })
    .await
    .unwrap_err();
  assert_eq!(rot.code(), Code::FailedPrecondition);
  let rm = operator(&served)
    .runners()
    .remove_runner(RemoveRunnerRequest {
      name: "static-a".into(),
    })
    .await
    .unwrap_err();
  assert_eq!(rm.code(), Code::FailedPrecondition);
  let rev = operator(&served)
    .runners()
    .revoke_runner_credential(RevokeRunnerCredentialRequest {
      name: "static-a".into(),
    })
    .await
    .unwrap_err();
  assert_eq!(rev.code(), Code::FailedPrecondition);

  // Hot-reload: replace static-a with static-b (atomic rename ensures inode+mtime change).
  let tmp = dir.path().join("runners.yaml.tmp");
  std::fs::write(&tmp, format!("runners:\n  - name: static-b\n    credential: {cred_b}\n    roles: [executor]\n")).unwrap();
  std::fs::rename(&tmp, &runners_file).unwrap();

  // Trigger reload via an RPC (or background tick): static-b works, static-a is refused and its control stream closes.
  as_runner(&served, cred_b).runners().heartbeat(heartbeat()).await.unwrap();
  let refused = as_runner(&served, cred_a).runners().heartbeat(heartbeat()).await.unwrap_err();
  assert_eq!(refused.code(), Code::Unauthenticated);
  assert!(served.daemon().links.get("static-a").is_none());
  let ended = tokio::time::timeout(Duration::from_secs(2), control.message()).await.expect("control stream closes");
  assert!(matches!(ended, Ok(None) | Err(_)), "expected closed control stream, got {ended:?}");
}

#[tokio::test]
async fn minted_runner_credential_disconnects_on_ttl_expiry_and_explicit_revoke() {
  use needy_api_client::proto::v1::{MintRunnerCredentialRequest, RevokeRunnerCredentialRequest};

  let served = served(Duration::from_secs(15)).await;

  // 1. Short TTL expiry automatically closes control stream and rejects new requests.
  let minted = operator(&served)
    .runners()
    .mint_runner_credential(MintRunnerCredentialRequest {
      name: "ephemeral-ttl".into(),
      ttl_ms: 200,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  assert!(minted.expires_at_ms > 0);
  as_runner(&served, &minted.credential).runners().heartbeat(heartbeat()).await.unwrap();
  let (_tx, rx) = tokio::sync::mpsc::channel(1);
  let mut control =
    as_runner(&served, &minted.credential).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  assert!(served.daemon().links.get("ephemeral-ttl").is_some());

  // Wait for TTL expiry to disconnect the control stream.
  let ended = tokio::time::timeout(Duration::from_secs(3), control.message()).await.expect("control stream closes on TTL expiry");
  assert!(matches!(ended, Ok(None) | Err(_)), "expected closed stream, got {ended:?}");
  let refused = as_runner(&served, &minted.credential).runners().heartbeat(heartbeat()).await.unwrap_err();
  assert_eq!(refused.code(), Code::Unauthenticated);

  // 2. Explicit RevokeRunnerCredential immediately closes control stream and rejects auth.
  let minted2 = operator(&served)
    .runners()
    .mint_runner_credential(MintRunnerCredentialRequest {
      name: "ephemeral-revoke".into(),
      ttl_ms: 60_000,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  as_runner(&served, &minted2.credential).runners().heartbeat(heartbeat()).await.unwrap();
  let (_tx2, rx2) = tokio::sync::mpsc::channel(1);
  let mut control2 =
    as_runner(&served, &minted2.credential).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx2)).await.unwrap().into_inner();
  assert!(served.daemon().links.get("ephemeral-revoke").is_some());

  operator(&served)
    .runners()
    .revoke_runner_credential(RevokeRunnerCredentialRequest {
      name: "ephemeral-revoke".into(),
    })
    .await
    .unwrap();
  let ended2 = tokio::time::timeout(Duration::from_secs(2), control2.message()).await.expect("control stream closes on revoke");
  assert!(matches!(ended2, Ok(None) | Err(_)), "expected closed stream, got {ended2:?}");
  let refused2 = as_runner(&served, &minted2.credential).runners().heartbeat(heartbeat()).await.unwrap_err();
  assert_eq!(refused2.code(), Code::Unauthenticated);
}

// ROOT CAUSE:
//
// `schedule_expiry` spawned a timer that closed `links.close(&name)` without
// re-checking `state.check_active`. If a short-TTL runner was revoked and
// re-minted under the same name with a longer TTL, the stale timer woke up and
// killed the new runner's control stream.
//
// Before the fix, the newly minted runner was disconnected when the old TTL
// elapsed. The fix verifies the runner is not static and is currently expired
// in `state.db` before closing its link.
#[tokio::test]
async fn stale_expiry_timer_does_not_disconnect_reminted_runner_with_same_name() {
  use needy_api_client::proto::v1::{MintRunnerCredentialRequest, RevokeRunnerCredentialRequest};

  let served = served(Duration::from_secs(15)).await;

  // Mint with a short 200ms TTL, then immediately revoke and re-mint with 60s TTL.
  let _first = operator(&served)
    .runners()
    .mint_runner_credential(MintRunnerCredentialRequest {
      name: "reused-ephemeral".into(),
      ttl_ms: 200,
      ..Default::default()
    })
    .await
    .unwrap();
  operator(&served)
    .runners()
    .revoke_runner_credential(RevokeRunnerCredentialRequest {
      name: "reused-ephemeral".into(),
    })
    .await
    .unwrap();
  let second = operator(&served)
    .runners()
    .mint_runner_credential(MintRunnerCredentialRequest {
      name: "reused-ephemeral".into(),
      ttl_ms: 60_000,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();

  as_runner(&served, &second.credential).runners().heartbeat(heartbeat()).await.unwrap();
  let (_tx, rx) = tokio::sync::mpsc::channel(1);
  let _control =
    as_runner(&served, &second.credential).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  assert!(served.daemon().links.get("reused-ephemeral").is_some());

  // Wait well past the first credential's 200ms expiry.
  tokio::time::sleep(Duration::from_millis(400)).await;

  assert!(
    served.daemon().links.get("reused-ephemeral").is_some(),
    "stale expiry timer must not close the re-minted runner's control stream"
  );
  as_runner(&served, &second.credential).runners().heartbeat(heartbeat()).await.unwrap();
}

// Unprefixed credentials: plain random secrets an orchestrator generates
// without needy's credential format, found by their hash.
const PLAIN_A: &str = "plain-runner-a-0123456789abcdefghijklmnopqrstuvwxyz";
const PLAIN_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const PLAIN_OTHER: &str = "plain-runner-x-0123456789abcdefghijklmnopqrstuvwxyz";

/// A daemon whose runners file (and clients file, when `clients` is given)
/// starts as the text given, and the runners file's path.
async fn served_with_files(runners: &str, clients: Option<&str>) -> (Served, std::path::PathBuf) {
  let dir = tempfile::tempdir().unwrap();
  let runners_file = dir.path().join("runners.yaml");
  std::fs::write(&runners_file, runners).unwrap();
  let clients_file = clients.map(|text| {
    let file = dir.path().join("clients.yaml");
    std::fs::write(&file, text).unwrap();
    file
  });
  let started = testing::daemon_in(dir, |config| {
    config.heartbeat_interval = Duration::from_secs(15);
    config.runners_file = Some(runners_file.clone());
    config.clients_file = clients_file;
  })
  .await;
  (testing::serve(started, Some(CONTROL_TOKEN)).await, runners_file)
}

/// Replaces a file the way a mounted Secret changes: a new inode.
fn replace(file: &std::path::Path, text: &str) {
  let tmp = file.with_extension("tmp");
  std::fs::write(&tmp, text).unwrap();
  std::fs::rename(&tmp, file).unwrap();
}

#[tokio::test]
async fn an_unprefixed_static_runner_credential_authenticates_as_its_runner() {
  let hash = hash_hex(PLAIN_B);
  let prefixed = "needy-runner.legacy.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  let text = format!(
    "runners:\n  - name: pod-a\n    credential: {PLAIN_A}\n  - name: pod-b\n    credential_hash: sha256:{hash}\n  - name: legacy\n    credential: {prefixed}\n"
  );
  let (served, _file) = served_with_files(&text, None).await;

  as_runner(&served, PLAIN_A).runners().heartbeat(heartbeat()).await.unwrap();
  as_runner(&served, PLAIN_B).runners().heartbeat(heartbeat()).await.unwrap();
  as_runner(&served, prefixed).runners().heartbeat(heartbeat()).await.unwrap();
  let wrong = as_runner(&served, PLAIN_OTHER).runners().heartbeat(heartbeat()).await.unwrap_err();
  let not_operator = as_runner(&served, PLAIN_A).runners().list_runners(ListRunnersRequest {}).await.unwrap_err();
  let listed = operator(&served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;

  let status = |name: &str| listed.iter().find(|runner| runner.name == name).map(|runner| runner.status());
  assert_eq!(status("pod-a"), Some(RunnerStatus::Online));
  assert_eq!(status("pod-b"), Some(RunnerStatus::Online));
  assert_eq!(status("legacy"), Some(RunnerStatus::Online), "a prefixed credential still names its runner");
  assert_eq!(wrong.code(), Code::Unauthenticated);
  assert_eq!(not_operator.code(), Code::Unauthenticated, "a runner credential reaches only runner methods");
}

/// The hex SHA-256 of `secret`, as a runners file writes `credential_hash`.
fn hash_hex(secret: &str) -> String {
  crate::store::hex(&crate::store::hash_secret(secret))
}

#[tokio::test]
async fn an_actor_runner_with_an_unprefixed_credential_acts_for_its_bound_run() {
  use needy_api_client::proto::v1::{ControlRequest, GetConfigRequest, GetRunRequest, RunnerReport, control_request};
  use needy_api_server::control::Runs as _;
  use needy_api_server::protocol::domain::proto_capabilities;

  let text = format!("runners:\n  - name: actor-node\n    credential: {PLAIN_A}\n    roles: [actor]\n");
  let (served, _file) = served_with_files(&text, None).await;
  let runner = as_runner(&served, PLAIN_A);
  let capabilities = needy_core::types::Capabilities {
    actor: [needy_config::profile::ActorDriver::LinuxFirecracker].into(),
    executor: Default::default(),
  };
  let beat = HeartbeatRequest {
    capabilities: Some(proto_capabilities(&capabilities)),
    ..Default::default()
  };
  runner.runners().heartbeat(beat).await.unwrap();
  let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlRequest>(16);
  let report = ControlRequest {
    payload: Some(control_request::Payload::Report(RunnerReport::default())),
  };
  control_tx.send(report).await.unwrap();
  let _control = runner.runners().control(tokio_stream::wrappers::ReceiverStream::new(control_rx)).await.unwrap();
  while served.daemon().links.get("actor-node").is_none() {
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  let profile = "user: { name: agent, uid: 1000, gid: 1000 }\nactor: { driver: linux-firecracker, runner: actor-node }\n";
  let created = served.daemon().create(testing::new_run(profile, served.dir())).await.unwrap();

  let bound = as_runner(&served, PLAIN_A).with_run_id(&created.id).unwrap();
  let config = bound.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  let denied = bound
    .runs()
    .get_run(GetRunRequest {
      run_id: created.id.clone(),
    })
    .await
    .unwrap_err();
  let unbound = as_runner(&served, PLAIN_A).with_run_id("no-such-run").unwrap().guest().get_config(GetConfigRequest {}).await.unwrap_err();

  assert_eq!(config.run_id, created.id);
  assert_eq!(denied.code(), Code::PermissionDenied);
  assert_eq!(unbound.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn a_runners_file_with_a_duplicate_or_weak_credential_loads_no_runners() {
  let hash = hash_hex(PLAIN_A);
  let client_token = "needy-client.static-pod.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  for text in [
    format!("runners:\n  - name: a\n    credential: {PLAIN_A}\n  - name: b\n    credential: {PLAIN_A}\n"),
    format!("runners:\n  - name: a\n    credential: {PLAIN_A}\n  - name: b\n    credential_hash: {hash}\n"),
    format!("runners:\n  - name: a\n    credential: {PLAIN_A}\n  - name: b\n    credential: {}\n", &PLAIN_OTHER[..42]),
    format!("runners:\n  - name: a\n    credential: {PLAIN_A}\n  - name: b\n    credential: {}\n", &PLAIN_B[..63]),
    format!("runners:\n  - name: a\n    credential: {PLAIN_A}\n  - name: b\n    credential: {client_token}\n"),
  ] {
    let (served, _file) = served_with_files(&text, None).await;

    let refused = as_runner(&served, PLAIN_A).runners().heartbeat(heartbeat()).await.unwrap_err();

    assert_eq!(refused.code(), Code::Unauthenticated, "{text}");
  }
}

#[tokio::test]
async fn reloading_the_runners_file_adds_and_removes_unprefixed_credentials() {
  let (served, file) = served_with_files(&format!("runners:\n  - name: pod-a\n    credential: {PLAIN_A}\n"), None).await;
  let (_tx, rx) = tokio::sync::mpsc::channel(1);
  let mut control =
    as_runner(&served, PLAIN_A).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  assert!(served.daemon().links.get("pod-a").is_some());

  replace(&file, &format!("runners:\n  - name: pod-b\n    credential: {PLAIN_B}\n"));
  as_runner(&served, PLAIN_B).runners().heartbeat(heartbeat()).await.unwrap();
  let removed = as_runner(&served, PLAIN_A).runners().heartbeat(heartbeat()).await.unwrap_err();
  let ended = tokio::time::timeout(Duration::from_secs(2), control.message()).await.expect("control stream closes");

  replace(&file, &format!("runners:\n  - name: pod-b\n    credential: {PLAIN_OTHER}\n"));
  as_runner(&served, PLAIN_OTHER).runners().heartbeat(heartbeat()).await.unwrap();
  let rotated_away = as_runner(&served, PLAIN_B).runners().heartbeat(heartbeat()).await.unwrap_err();

  assert_eq!(removed.code(), Code::Unauthenticated);
  assert!(matches!(ended, Ok(None) | Err(_)), "expected closed control stream, got {ended:?}");
  assert_eq!(rotated_away.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn an_unprefixed_secret_in_both_the_runners_and_clients_files_is_refused_for_both() {
  use needy_api_client::proto::v1::{GetCallerRequest, ListRunsRequest};

  let runners = format!("runners:\n  - name: pod-a\n    credential: {PLAIN_A}\n  - name: pod-b\n    credential: {PLAIN_B}\n");
  let clients = format!("clients:\n  - name: controller\n    token: {PLAIN_A}\n  - name: ci\n    token: {PLAIN_OTHER}\n");
  let (served, _file) = served_with_files(&runners, Some(&clients)).await;

  let as_runner_refused = as_runner(&served, PLAIN_A).runners().heartbeat(heartbeat()).await.unwrap_err();
  let as_client_refused = as_runner(&served, PLAIN_A).runs().list_runs(ListRunsRequest {}).await.unwrap_err();
  as_runner(&served, PLAIN_B).runners().heartbeat(heartbeat()).await.unwrap();
  let ci = as_runner(&served, PLAIN_OTHER).clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();

  assert_eq!(as_runner_refused.code(), Code::Unauthenticated);
  assert_eq!(as_client_refused.code(), Code::Unauthenticated);
  assert_eq!(ci.client_id, "static-ci", "the clients file's other entries still authenticate");
}
