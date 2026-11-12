use needy_api_client::Client;
use needy_api_proto::v1::{ControlRequest, TaskRequest, control_response, exec_request, task_request};
use needy_api_proto::v1::{
  CreateJoinTokenRequest, ExecutorDriver, HeartbeatRequest, JoinRequest, JoinTokenState, ListRunnersRequest, ListRunsRequest,
  RotateRunnerRequest, RunnerCapabilities, RunnerRole, RunnerStatus,
};
use tonic::Code;

use crate::control::Caller;
use crate::testing::{MAC_CREDENTIAL, REVOKED_CREDENTIAL, serve_with};

const CONTROL_TOKEN: &str = "control-secret";

fn native() -> Option<RunnerCapabilities> {
  Some(RunnerCapabilities {
    executor: vec![ExecutorDriver::Native as i32],
    ..Default::default()
  })
}

fn join() -> JoinRequest {
  JoinRequest {
    join_token: "needy-join.t1.secret".into(),
    name: "mac".into(),
    capabilities: native(),
    ..Default::default()
  }
}

fn heartbeat() -> HeartbeatRequest {
  HeartbeatRequest {
    capabilities: native(),
    ..Default::default()
  }
}

fn with(client: &Client, token: &str) -> Client {
  client.clone().with_token(token).unwrap()
}

#[tokio::test]
async fn an_operator_creates_a_join_token_whose_secret_is_in_the_response() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
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
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;

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
async fn a_join_with_an_unspecified_executor_driver_is_invalid() {
  let served = serve_with(Caller::Operator, None).await;
  let request = JoinRequest {
    capabilities: Some(RunnerCapabilities {
      executor: vec![ExecutorDriver::Unspecified as i32],
      ..Default::default()
    }),
    ..join()
  };

  let status = served.client.runners().join(request).await.unwrap_err();

  assert_eq!((status.code(), status.message()), (Code::InvalidArgument, "the executor driver is unspecified"));
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn a_heartbeat_with_a_runner_credential_names_that_runner_and_carries_a_rotated_credential() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;

  let beat = with(&served.client, MAC_CREDENTIAL).runners().heartbeat(heartbeat()).await.unwrap().into_inner();

  assert_eq!(beat.credential, "needy-runner.mac.new");
  assert_eq!(served.fake.calls(), ["heartbeat mac executor(native)"]);
}

#[tokio::test]
async fn an_operator_cannot_send_heartbeats() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;

  let status = with(&served.client, CONTROL_TOKEN).runners().heartbeat(heartbeat()).await.unwrap_err();

  assert_eq!((status.code(), status.message()), (Code::PermissionDenied, "only runners send heartbeats"));
}

#[tokio::test]
async fn a_runner_credential_reaches_heartbeats_only() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let runner = with(&served.client, MAC_CREDENTIAL);

  let list = runner.runners().list_runners(ListRunnersRequest {}).await.unwrap_err();
  let rotate = runner.runners().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap_err();
  let runs = runner.runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!([list.code(), rotate.code(), runs.code()], [Code::Unauthenticated; 3]);
  assert!(served.fake.calls().is_empty(), "{:?}", served.fake.calls());
}

#[tokio::test]
async fn a_revoked_runner_credential_is_refused_even_on_a_listener_without_a_token() {
  let served = serve_with(Caller::Operator, None).await;

  let status = with(&served.client, REVOKED_CREDENTIAL).runners().heartbeat(heartbeat()).await.unwrap_err();
  let open = served.client.runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner();

  assert_eq!(status.code(), Code::Unauthenticated);
  assert_eq!(open.runners[0].name, "mac", "a listener without a token still serves operators");
}

#[tokio::test]
async fn an_operator_lists_rotates_and_removes_runners() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let runners = with(&served.client, CONTROL_TOKEN).runners();

  let listed = runners.clone().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners;
  runners.clone().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap();
  runners.clone().remove_runner(needy_api_proto::v1::RemoveRunnerRequest { name: "mac".into() }).await.unwrap();

  assert_eq!(listed[0].capabilities.as_ref().unwrap().executor().collect::<Vec<_>>(), [ExecutorDriver::Native]);
  assert_eq!(listed[0].labels["os"], "darwin");
  assert_eq!(served.fake.calls(), ["list runners", "rotate mac", "remove mac"]);
}

#[tokio::test]
async fn a_runner_opens_its_control_stream_and_gets_tasks() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let (_results, rx) = tokio::sync::mpsc::channel::<ControlRequest>(1);

  let mut tasks =
    with(&served.client, MAC_CREDENTIAL).runners().control(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  let first = tasks.message().await.unwrap().unwrap();

  let Some(control_response::Payload::Task(task)) = first.payload else {
    panic!("{first:?}")
  };
  assert_eq!(task.id, 1);
  assert_eq!(served.fake.calls(), ["control mac"]);
}

#[tokio::test]
async fn a_task_stream_names_its_task_first() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let runner = with(&served.client, MAC_CREDENTIAL);
  let named = TaskRequest {
    payload: Some(task_request::Payload::TaskId(7)),
  };
  let unnamed = TaskRequest {
    payload: Some(task_request::Payload::Exec(Default::default())),
  };

  let mut requests = runner.runners().task(tokio_stream::iter([named])).await.unwrap().into_inner();
  let first = requests.message().await.unwrap().unwrap();
  let status = runner.runners().task(tokio_stream::iter([unnamed])).await.unwrap_err();

  assert_eq!(first.exec.unwrap().payload, Some(exec_request::Payload::Stdin(b"hi".to_vec())));
  assert_eq!((status.code(), status.message()), (Code::InvalidArgument, "the first request of a task stream must name its task"));
  assert_eq!(served.fake.calls(), ["task mac 7"]);
}

#[tokio::test]
async fn only_runners_open_control_and_task_streams() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let operator = with(&served.client, CONTROL_TOKEN);

  let control = operator.runners().control(tokio_stream::iter(Vec::<ControlRequest>::new())).await.unwrap_err();
  let task = operator.runners().task(tokio_stream::iter(Vec::<TaskRequest>::new())).await.unwrap_err();

  assert_eq!((control.code(), control.message()), (Code::PermissionDenied, "only runners open a control stream"));
  assert_eq!((task.code(), task.message()), (Code::PermissionDenied, "only runners open task streams"));
  assert!(served.fake.calls().is_empty());
}
