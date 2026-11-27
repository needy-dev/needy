use needy_api_proto::v1::{FsPing, SessionRequest, session_request, session_response};
use tonic::Code;

use crate::control::Caller;
use crate::testing::{CLIENT_TOKEN, MAC_CREDENTIAL, serve, serve_with};

#[tokio::test]
async fn an_actor_opens_an_fs_session_and_exchanges_messages() {
  let served = serve(Caller::Actor {
    run: "s-local".into(),
  })
  .await;

  let (tx, mut rx) = served.client.fs_session().await.unwrap();
  tx.send(SessionRequest {
    id: 42,
    body: Some(session_request::Body::Ping(FsPing { timestamp_ms: 1234 })),
  })
  .await
  .unwrap();
  drop(tx);

  let reply = rx.message().await.unwrap().unwrap();
  assert_eq!(reply.id, 42);
  let Some(session_response::Body::Pong(pong)) = reply.body else {
    panic!("unexpected reply: {reply:?}");
  };
  assert_eq!(pong.timestamp_ms, 1234);
  assert_eq!(served.fake.calls(), ["fs session Actor { run: \"s-local\" }"]);
}

#[tokio::test]
async fn an_operator_and_client_and_authorized_actor_runner_can_open_fs_sessions() {
  let served = serve_with(Caller::Operator, Some("control-secret")).await;

  let operator = served.client.clone().with_token("control-secret").unwrap();
  let (tx, mut rx) = operator.fs_session().await.unwrap();
  tx.send(SessionRequest {
    id: 1,
    body: Some(session_request::Body::Ping(FsPing { timestamp_ms: 10 })),
  })
  .await
  .unwrap();
  drop(tx);
  assert_eq!(rx.message().await.unwrap().unwrap().id, 1);

  let client = served.client.clone().with_token(CLIENT_TOKEN).unwrap();
  let (tx, mut rx) = client.fs_session().await.unwrap();
  tx.send(SessionRequest {
    id: 2,
    body: Some(session_request::Body::Ping(FsPing { timestamp_ms: 20 })),
  })
  .await
  .unwrap();
  drop(tx);
  assert_eq!(rx.message().await.unwrap().unwrap().id, 2);

  let actor_runner = served.client.clone().with_token(MAC_CREDENTIAL).unwrap().with_run_id("s-actor").unwrap();
  let (tx, mut rx) = actor_runner.fs_session().await.unwrap();
  tx.send(SessionRequest {
    id: 3,
    body: Some(session_request::Body::Ping(FsPing { timestamp_ms: 30 })),
  })
  .await
  .unwrap();
  drop(tx);
  assert_eq!(rx.message().await.unwrap().unwrap().id, 3);

  assert_eq!(
    served.fake.calls(),
    [
      "fs session Operator",
      "fs session Client { id: \"c1\", name: \"laptop\", access: Runs }",
      "fs session Actor { run: \"s-actor\" }",
    ]
  );
}

#[tokio::test]
async fn a_runner_without_run_id_cannot_open_an_fs_session() {
  let served = serve_with(Caller::Operator, Some("control-secret")).await;
  let runner = served.client.clone().with_token(MAC_CREDENTIAL).unwrap();

  let err = runner.fs_session().await.unwrap_err();
  assert_eq!(err.kind(), needy_api_client::Kind::Unauthenticated);
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn a_runner_listener_cannot_open_an_fs_session() {
  let served = serve(Caller::Runner { name: "mac".into() }).await;

  let err = served.client.fs_session().await.unwrap_err();
  let needy_api_client::Error::Status(status) = err else {
    panic!("expected status error, got {err:?}");
  };
  assert_eq!((status.code(), status.message()), (Code::PermissionDenied, "runners cannot open filesystem sessions"));
  assert!(served.fake.calls().is_empty());
}
