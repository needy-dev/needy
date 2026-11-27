use needy_api_client::Client;
use needy_api_proto::v1::health_service_server::{HealthService, HealthServiceServer};
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Response, Status};

use super::*;

struct Health;

#[tonic::async_trait]
impl HealthService for Health {
  async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
    Ok(Response::new(CheckResponse {
      role: "control".into(),
      ..Default::default()
    }))
  }
}

/// Accepts `secret` only, for every method.
struct Secret;

#[async_trait::async_trait]
impl Authenticator for Secret {
  async fn authenticate(&self, _method: &str, token: &str, _run_id: Option<&str>) -> crate::control::Result<Option<Caller>> {
    Ok((token == "secret").then_some(Caller::Operator))
  }
}

#[tokio::test]
async fn methods_need_a_token() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let url = format!("http://{}", listener.local_addr().unwrap());
  tokio::spawn(
    tonic::transport::Server::builder()
      .layer(AuthLayer::new(Secret))
      .add_service(HealthServiceServer::new(Health))
      .serve_with_incoming(TcpListenerStream::new(listener)),
  );

  let client = Client::connect(&url).await.unwrap();
  let err = client.health().check(CheckRequest {}).await.unwrap_err();
  assert_eq!(err.code(), Code::Unauthenticated);

  let wrong = client.clone().with_token("guess").unwrap();
  let err = wrong.health().check(CheckRequest {}).await.unwrap_err();
  assert_eq!(err.code(), Code::Unauthenticated);

  let client = client.with_token("secret").unwrap();
  let health = client.health().check(CheckRequest {}).await.unwrap();
  assert_eq!(health.into_inner().role, "control");
}

#[test]
fn no_configured_token_accepts_any() {
  assert!(Token(None).accepts(""));
  assert!(Token(None).accepts("anything"));
  assert!(!Token(Some("secret".into())).accepts("secreT"));
  assert!(!Token(Some("secret".into())).accepts("secret2"));
}

#[tokio::test]
async fn node_authorizer_scopes_actor_runner_to_bound_run_and_actor_methods() {
  use needy_api_proto::v1::{
    DecideRequest, GetConfigRequest, GetRunRequest, HeartbeatRequest, KillProcessRequest, ListProcessesRequest, ListRunsRequest,
  };

  use crate::testing::{MAC_CREDENTIAL, OTHER_CREDENTIAL, REVOKED_CREDENTIAL, serve};

  let served = serve(Caller::Operator).await;
  let bound = served.client.clone().with_token(MAC_CREDENTIAL).unwrap().with_run_id("s-actor").unwrap();
  let wrong_run = served.client.clone().with_token(MAC_CREDENTIAL).unwrap().with_run_id("s-ended").unwrap();
  let wrong_runner = served.client.clone().with_token(OTHER_CREDENTIAL).unwrap().with_run_id("s-actor").unwrap();
  let plain_runner = served.client.clone().with_token(MAC_CREDENTIAL).unwrap();
  let revoked_runner = served.client.clone().with_token(REVOKED_CREDENTIAL).unwrap().with_run_id("s-actor").unwrap();

  // Authorized actor runner with x-needy-run-id reaches ACTOR_METHODS and is
  // scoped to its bound run (`s-actor`).
  bound.health().check(CheckRequest {}).await.unwrap();
  let config = bound.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  assert_eq!(config.run_id, "s-actor");
  let decided = bound
    .runs()
    .decide(DecideRequest {
      run_id: "other-run".into(),
      argv: vec!["python3".into()],
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  assert_eq!(decided.target, "on-s-actor");

  // Disallowed methods for an authorized actor runner carrying x-needy-run-id
  // must be rejected with PermissionDenied (including RUNNER_METHODS like Heartbeat).
  assert_eq!(
    bound
      .runs()
      .get_run(GetRunRequest {
        run_id: "s1".into()
      })
      .await
      .unwrap_err()
      .code(),
    Code::PermissionDenied
  );
  assert_eq!(bound.runs().list_runs(ListRunsRequest {}).await.unwrap_err().code(), Code::PermissionDenied);
  assert_eq!(bound.processes().list_processes(ListProcessesRequest::default()).await.unwrap_err().code(), Code::PermissionDenied);
  assert_eq!(
    bound
      .processes()
      .kill_process(KillProcessRequest {
        process_id: "p1".into(),
        signal: "SIGKILL".into(),
      })
      .await
      .unwrap_err()
      .code(),
    Code::PermissionDenied
  );
  assert_eq!(bound.runners().heartbeat(HeartbeatRequest::default()).await.unwrap_err().code(), Code::PermissionDenied);
  assert_eq!(bound.runner_fs_task(7).await.unwrap_err().kind(), needy_api_client::Kind::Denied);

  // Authorized actor runner can open FsService.Session, while plain runner
  // cannot open FsService.Session but can open RunnerService.FsTask.
  let (tx, mut rx) = bound.fs_session().await.unwrap();
  drop(tx);
  assert!(rx.message().await.unwrap().is_none());

  // Non-bound runner or non-active/wrong run must be rejected with PermissionDenied.
  assert_eq!(wrong_run.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), Code::PermissionDenied);
  assert_eq!(wrong_runner.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), Code::PermissionDenied);
  assert_eq!(wrong_run.fs_session().await.unwrap_err().kind(), needy_api_client::Kind::Denied);
  assert_eq!(wrong_runner.fs_session().await.unwrap_err().kind(), needy_api_client::Kind::Denied);

  // Runner WITHOUT x-needy-run-id is rejected on ACTOR_METHODS, and allowed on RUNNER_METHODS.
  assert_eq!(plain_runner.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), Code::Unauthenticated);
  assert_eq!(plain_runner.runs().decide(DecideRequest::default()).await.unwrap_err().code(), Code::Unauthenticated);
  assert_eq!(plain_runner.fs_session().await.unwrap_err().kind(), needy_api_client::Kind::Unauthenticated);
  plain_runner.runners().heartbeat(HeartbeatRequest::default()).await.unwrap();
  let (_tx, mut fs_task) = plain_runner.runner_fs_task(7).await.unwrap();
  assert!(fs_task.message().await.unwrap().is_some());

  // Revoked runner credential fails with Unauthenticated even with x-needy-run-id.
  assert_eq!(revoked_runner.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), Code::Unauthenticated);
  assert_eq!(revoked_runner.fs_session().await.unwrap_err().kind(), needy_api_client::Kind::Unauthenticated);
}
