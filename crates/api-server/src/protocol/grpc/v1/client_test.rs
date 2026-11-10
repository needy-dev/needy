use needy_api_client::Client;
use needy_api_proto::v1::{
  CreatePairingCodeRequest, CreateRunRequest, ExecRequest, ExecStart, HeartbeatRequest, KillProcessRequest, ListClientsRequest,
  ListRunsRequest, Origin, PairRequest, RevokeClientRequest, StopRunRequest, exec_request,
};
use tonic::Code;

use crate::control::Caller;
use crate::testing::{CLIENT_TOKEN, PAIRING_CODE, REVOKED_CLIENT_TOKEN, serve, serve_with};

const CONTROL_TOKEN: &str = "control-secret";

fn with(client: &Client, token: &str) -> Client {
  client.clone().with_token(token).unwrap()
}

#[tokio::test]
async fn an_operator_creates_a_pairing_code_whose_secret_is_in_the_response() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let request = CreatePairingCodeRequest {
    ttl_ms: 600_000,
    name: "laptop".into(),
  };

  let created = with(&served.client, CONTROL_TOKEN).clients().create_pairing_code(request).await.unwrap().into_inner();

  assert_eq!((created.id.as_str(), created.name.as_str(), created.code.as_str()), ("k1", "laptop", PAIRING_CODE));
  assert_eq!(served.fake.calls(), ["create pairing code ttl=600000 name=laptop"]);
}

#[tokio::test]
async fn pairing_needs_no_bearer_token_even_where_the_control_token_is_required() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;

  let paired = served
    .client
    .clients()
    .pair(PairRequest {
      code: PAIRING_CODE.into(),
    })
    .await
    .unwrap()
    .into_inner();
  let wrong = served
    .client
    .clients()
    .pair(PairRequest {
      code: "needy-pair.k1.guess".into(),
    })
    .await
    .unwrap_err();
  let empty = served.client.clients().pair(PairRequest::default()).await.unwrap_err();

  assert_eq!(paired.token, CLIENT_TOKEN);
  assert_eq!(paired.client.unwrap().name, "laptop");
  assert_eq!(wrong.code(), Code::Unauthenticated);
  assert_eq!(empty.code(), Code::InvalidArgument);
  // Every other method still needs a token.
  let list = served.client.clients().list_clients(ListClientsRequest {}).await.unwrap_err();
  assert_eq!(list.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn a_client_token_reaches_operator_methods_and_is_named_as_the_principal() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;
  let client = with(&served.client, CLIENT_TOKEN);
  let create = CreateRunRequest {
    profile_yaml: "{}".into(),
    profile_name: "local".into(),
    ..Default::default()
  };

  client.runs().create_run(create).await.unwrap();
  client.runs().list_runs(ListRunsRequest {}).await.unwrap();
  client
    .runs()
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
    })
    .await
    .unwrap();
  client
    .processes()
    .kill_process(KillProcessRequest {
      process_id: "p1".into(),
      signal: "TERM".into(),
    })
    .await
    .unwrap();
  let start = ExecRequest {
    payload: Some(exec_request::Payload::Start(ExecStart {
      argv: vec!["true".into()],
      run_id: "s1".into(),
      origin: Origin::Cli as i32,
      ..Default::default()
    })),
  };
  let mut outputs = client.processes().exec(tokio_stream::iter([start])).await.unwrap().into_inner();
  while outputs.message().await.unwrap().is_some() {}
  client.clients().revoke_client(RevokeClientRequest { id: "c2".into() }).await.unwrap();

  assert_eq!(
    served.fake.calls(),
    [
      "create local by client:laptop",
      "stop s1 by client:laptop",
      "kill p1 TERM by client:laptop",
      r#"exec true in "s1" from Cli by client:laptop"#,
      "revoke client c2",
    ]
  );
}

#[tokio::test]
async fn a_client_token_is_named_even_on_a_listener_without_a_control_token() {
  let served = serve(Caller::Operator).await;

  with(&served.client, CLIENT_TOKEN)
    .runs()
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
    })
    .await
    .unwrap();
  served
    .client
    .runs()
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
    })
    .await
    .unwrap();

  assert_eq!(served.fake.calls(), ["stop s1 by client:laptop", "stop s1 by operator"]);
}

#[tokio::test]
async fn a_revoked_client_token_is_refused_even_without_a_control_token() {
  let served = serve(Caller::Operator).await;

  let refused = with(&served.client, REVOKED_CLIENT_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(refused.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn a_client_token_does_not_reach_the_runner_methods() {
  let served = serve_with(Caller::Operator, Some(CONTROL_TOKEN)).await;

  let refused = with(&served.client, CLIENT_TOKEN).runners().heartbeat(HeartbeatRequest::default()).await.unwrap_err();

  assert_eq!(refused.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn an_actor_cannot_manage_clients() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;

  let refused = served.client.clients().create_pairing_code(CreatePairingCodeRequest::default()).await.unwrap_err();

  assert_eq!(refused.code(), Code::PermissionDenied);
  assert!(served.fake.calls().is_empty());
}
