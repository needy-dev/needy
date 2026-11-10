use std::collections::BTreeMap;
use std::time::Duration;

use needy_api_client::Client;
use needy_api_client::proto::v1::{
  CreateJoinTokenRequest, ListRunnersRequest, RemoveRunnerRequest, RotateRunnerRequest, Runner as Listed, RunnerStatus,
};
use needy_core::types::RunnerRole;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";

async fn control(heartbeat: Duration) -> Served {
  testing::serve(testing::daemon_with_heartbeat(heartbeat).await, Some(CONTROL_TOKEN)).await
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

async fn join_token(served: &Served) -> String {
  let request = CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 1,
    ..Default::default()
  };
  operator(served).runners().create_join_token(request).await.unwrap().into_inner().token
}

async fn listed(served: &Served) -> Vec<Listed> {
  operator(served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners
}

fn config(served: &Served, state_dir: &std::path::Path, join_token: Option<String>) -> Config {
  Config {
    state_dir: state_dir.to_path_buf(),
    control: Some(served.endpoint.clone()),
    join_token,
    name: "mac".into(),
    roles: [RunnerRole::Executor].into(),
    labels: BTreeMap::from([("os".to_string(), "darwin".to_string())]),
    grace: crate::runner::DISCONNECT_GRACE,
  }
}

/// Waits until `check` holds, for at most 10 s.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !check().await {
    assert!(tokio::time::Instant::now() < deadline, "{what}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

#[tokio::test]
async fn a_runner_joins_once_keeps_its_credential_private_and_reuses_it_after_a_restart() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  let token = join_token(&served).await;

  let mut first = Runner::start(config(&served, state.path(), Some(token))).await.unwrap();
  first.beat().await.unwrap();
  let stored = std::fs::read_to_string(state.path().join("runner.json")).unwrap();
  let before = listed(&served).await[0].last_seen_at_ms;
  tokio::time::sleep(Duration::from_millis(5)).await;
  // A restart has no join token; the one it joined with is spent anyway.
  let mut second = Runner::start(config(&served, state.path(), None)).await.unwrap();
  second.beat().await.unwrap();

  assert!(stored.contains("needy-runner.mac."), "the credential is stored");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(state.path().join("runner.json")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
  }
  let runners = listed(&served).await;
  assert_eq!(runners.len(), 1);
  assert_eq!((runners[0].name.as_str(), runners[0].status()), ("mac", RunnerStatus::Online));
  assert!(runners[0].last_seen_at_ms > before, "the restarted runner's heartbeat arrived");
}

#[tokio::test]
async fn a_first_start_without_a_join_token_says_what_it_needs() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();

  let error = Runner::start(config(&served, state.path(), None)).await.err().unwrap();

  assert_eq!(error.to_string(), "this runner has not joined yet: give it a join token with --join-token-env or --join-token-file");
}

#[tokio::test]
async fn a_restart_under_another_name_is_refused() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  Runner::start(config(&served, state.path(), Some(join_token(&served).await))).await.unwrap();
  let renamed = Config {
    name: "other".into(),
    ..config(&served, state.path(), None)
  };

  let error = Runner::start(renamed).await.err().unwrap();

  assert!(error.to_string().starts_with("this runner joined as mac, not other"), "{error}");
}

#[tokio::test]
async fn heartbeats_pick_up_a_rotated_credential_and_end_once_the_runner_is_removed() {
  let served = control(Duration::from_millis(100)).await;
  let state = tempfile::tempdir().unwrap();
  let runner = Runner::start(config(&served, state.path(), Some(join_token(&served).await))).await.unwrap();
  let path = state.path().join("runner.json");
  let old = std::fs::read_to_string(&path).unwrap();
  let stop = CancellationToken::new();
  let beating = tokio::spawn(runner.run(stop.clone()));

  operator(&served).runners().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap();
  eventually("the rotated credential is stored and used", async || {
    std::fs::read_to_string(&path).unwrap() != old && !listed(&served).await[0].rotating
  })
  .await;
  operator(&served).runners().remove_runner(RemoveRunnerRequest { name: "mac".into() }).await.unwrap();
  let ended = tokio::time::timeout(Duration::from_secs(10), beating).await.expect("heartbeats end").unwrap();

  let error = ended.unwrap_err().to_string();
  assert!(error.starts_with("the control plane refused this runner's credential"), "{error}");
  assert_eq!(listed(&served).await[0].status(), RunnerStatus::Removed);
}
