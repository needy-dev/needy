use needy_api_client::Client;
use needy_api_proto::v1::{
  CreateJoinTokenRequest, HeartbeatRequest, JoinRequest, JoinTokenState, ListRunnersRequest, ListRunsRequest, RotateRunnerRequest,
  RunnerRole, RunnerStatus,
};
use tonic::Code;

use crate::control::Caller;
use crate::testing::{MAC_CREDENTIAL, REVOKED_CREDENTIAL, serve_with};

const CONTROL_TOKEN: &str = "control-secret";

fn join() -> JoinRequest {
  JoinRequest {
    join_token: "needy-join.t1.secret".into(),
    name: "mac".into(),
    roles: vec![RunnerRole::Executor as i32],
    ..Default::default()
  }
}

fn heartbeat() -> HeartbeatRequest {
  HeartbeatRequest {
    roles: vec![RunnerRole::Executor as i32],
    ..Default::default()
  }
}

fn with(client: &Client, token: &str) -> Client {
  client.clone().with_token(token).unwrap()
}

#[tokio::test]
async fn an_operator_creates_a_join_token_whose_secret_is_in_the_response() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;
  let request = CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 2,
    roles: vec![RunnerRole::Executor as i32],
    labels: [("os".to_string(), "darwin".to_string())].into(),
  };

  let created = with(&served.client, CONTROL_TOKEN).runners().create_join_token(request).await.unwrap().into_inner();

  assert_eq!(created.token, "needy-join.t1.secret");
  let token = created.join_token.unwrap();
  assert_eq!((token.id.as_str(), token.uses_left, token.state()), ("t1", 2, JoinTokenState::Valid));
  assert_eq!(served.fake.calls(), [r#"create token ttl=60000 uses=2 roles={Executor} labels={"os": "darwin"}"#]);
}

#[tokio::test]
async fn joining_needs_no_bearer_token_even_where_the_control_token_is_required() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;

  let joined = served.client.runners().join(join()).await.unwrap().into_inner();
  let wrong = JoinRequest {
    join_token: "needy-join.t1.guess".into(),
    ..join()
  };
  let status = served.client.runners().join(wrong).await.unwrap_err();

  assert_eq!(joined.credential, MAC_CREDENTIAL);
  assert_eq!(joined.heartbeat_interval_ms, 15_000);
  assert_eq!(joined.runner.unwrap().status(), RunnerStatus::Online);
  assert_eq!((status.code(), status.message()), (Code::Unauthenticated, "the join token is not valid"));
}

#[tokio::test]
async fn a_join_with_an_unspecified_role_is_invalid() {
  let served = serve_with(Caller::Cli, None).await;
  let request = JoinRequest {
    roles: vec![RunnerRole::Unspecified as i32],
    ..join()
  };

  let status = served.client.runners().join(request).await.unwrap_err();

  assert_eq!((status.code(), status.message()), (Code::InvalidArgument, "the runner role is unspecified"));
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn a_heartbeat_with_a_runner_credential_names_that_runner_and_carries_a_rotated_credential() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;

  let beat = with(&served.client, MAC_CREDENTIAL).runners().heartbeat(heartbeat()).await.unwrap().into_inner();

  assert_eq!(beat.credential, "needy-runner.mac.new");
  assert_eq!(served.fake.calls(), ["heartbeat mac {Executor}"]);
}

#[tokio::test]
async fn an_operator_cannot_send_heartbeats() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;

  let status = with(&served.client, CONTROL_TOKEN).runners().heartbeat(heartbeat()).await.unwrap_err();

  assert_eq!((status.code(), status.message()), (Code::PermissionDenied, "only runners send heartbeats"));
}

#[tokio::test]
async fn a_runner_credential_reaches_heartbeats_only() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;
  let runner = with(&served.client, MAC_CREDENTIAL);

  let list = runner.runners().list_runners(ListRunnersRequest {}).await.unwrap_err();
  let rotate = runner.runners().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap_err();
  let runs = runner.runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!([list.code(), rotate.code(), runs.code()], [Code::Unauthenticated; 3]);
  assert!(served.fake.calls().is_empty(), "{:?}", served.fake.calls());
}

#[tokio::test]
async fn a_revoked_runner_credential_is_refused_even_on_a_listener_without_a_token() {
  let served = serve_with(Caller::Cli, None).await;

  let status = with(&served.client, REVOKED_CREDENTIAL).runners().heartbeat(heartbeat()).await.unwrap_err();
  let open = served.client.runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner();

  assert_eq!(status.code(), Code::Unauthenticated);
  assert_eq!(open.runners[0].name, "mac", "a listener without a token still serves operators");
}

#[tokio::test]
async fn an_operator_lists_rotates_and_removes_runners() {
  let served = serve_with(Caller::Cli, Some(CONTROL_TOKEN)).await;
  let runners = with(&served.client, CONTROL_TOKEN).runners();

  let listed = runners.clone().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  runners.clone().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap();
  runners.clone().remove_runner(needy_api_proto::v1::RemoveRunnerRequest { name: "mac".into() }).await.unwrap();

  assert_eq!(listed[0].roles().collect::<Vec<_>>(), [RunnerRole::Executor]);
  assert_eq!(listed[0].labels["os"], "darwin");
  assert_eq!(served.fake.calls(), ["list runners", "rotate mac", "remove mac"]);
}
