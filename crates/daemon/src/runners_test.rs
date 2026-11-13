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
