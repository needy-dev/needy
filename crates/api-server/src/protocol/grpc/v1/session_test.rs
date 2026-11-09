use needy_api_proto::v1::{
  Ancestor, CreateSessionRequest, DecideRequest, GetSessionLogsRequest, GetSessionRequest, ListSessionsRequest, StopSessionRequest,
};
use tonic::Code;

use crate::control::Caller;
use crate::testing::serve;

fn actor() -> Caller {
  Caller::Actor {
    session: "s-actor".into(),
  }
}

fn create() -> CreateSessionRequest {
  CreateSessionRequest {
    profile_yaml: "{}".into(),
    profile_name: "local".into(),
    name: "mine".into(),
    ..Default::default()
  }
}

#[tokio::test]
async fn the_cli_creates_a_session_and_reads_its_state_as_an_enum() {
  let served = serve(Caller::Cli).await;

  let session = served.client.sessions().create_session(create()).await.unwrap().into_inner().session.unwrap();

  assert_eq!(session.session_id, "s1");
  assert_eq!(session.name, "mine");
  assert_eq!(session.state(), needy_api_proto::v1::SessionState::Running);
  assert_eq!(served.fake.calls(), ["create local"]);
}

#[tokio::test]
async fn a_create_that_names_no_profile_is_invalid() {
  let served = serve(Caller::Cli).await;
  let request = CreateSessionRequest {
    profile_name: String::new(),
    ..create()
  };

  let status = served.client.sessions().create_session(request).await.unwrap_err();

  assert_eq!(status.code(), Code::InvalidArgument);
  assert_eq!(status.message(), "the request names no profile");
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn an_actor_cannot_create_stop_or_read_the_logs_of_sessions() {
  let served = serve(actor()).await;
  let sessions = served.client.sessions();

  let create = sessions.clone().create_session(create()).await.unwrap_err();
  let stop = sessions
    .clone()
    .stop_session(StopSessionRequest {
      session_id: "s-actor".into(),
    })
    .await
    .unwrap_err();
  let logs = sessions
    .clone()
    .get_session_logs(GetSessionLogsRequest {
      session_id: "s-actor".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(
    [create.code(), stop.code(), logs.code()],
    [
      Code::PermissionDenied,
      Code::PermissionDenied,
      Code::PermissionDenied
    ]
  );
  assert_eq!(create.message(), "actors cannot create sessions");
  assert!(served.fake.calls().is_empty(), "{:?}", served.fake.calls());
}

#[tokio::test]
async fn a_missing_session_is_not_found() {
  let served = serve(Caller::Cli).await;

  let status = served
    .client
    .sessions()
    .get_session(GetSessionRequest {
      session_id: "nope".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(status.code(), Code::NotFound);
  assert_eq!(status.message(), "no such session");
}

#[tokio::test]
async fn sessions_are_listed_and_stopped() {
  let served = serve(Caller::Cli).await;

  let list = served.client.sessions().list_sessions(ListSessionsRequest {}).await.unwrap().into_inner().sessions;
  served
    .client
    .sessions()
    .stop_session(StopSessionRequest {
      session_id: "s2".into(),
    })
    .await
    .unwrap();

  assert_eq!(list.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
  assert_eq!(served.fake.calls(), ["stop s2"]);
}

#[tokio::test]
async fn logs_arrive_in_chunks_that_add_up() {
  let served = serve(Caller::Cli).await;

  let mut stream = served
    .client
    .sessions()
    .get_session_logs(GetSessionLogsRequest {
      session_id: "s1".into(),
    })
    .await
    .unwrap()
    .into_inner();
  let mut chunks = Vec::new();
  while let Some(chunk) = stream.message().await.unwrap() {
    chunks.push(chunk.data.len());
  }

  assert_eq!(chunks, [1 << 20, 1 << 20, 1 << 20]);
}

fn decide(session: &str, argv: &[&str]) -> DecideRequest {
  DecideRequest {
    session_id: session.into(),
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: "/home/agent".into(),
    ancestors: vec![Ancestor {
      argv: vec!["bash".into(), "-c".into(), "python3".into()],
    }],
    ..Default::default()
  }
}

#[tokio::test]
async fn an_actor_decides_within_its_own_session_whatever_the_request_names() {
  let served = serve(actor()).await;

  let response = served.client.sessions().decide(decide("someone-else", &["python3"])).await.unwrap().into_inner();

  assert_eq!(response.target, "on-s-actor");
  assert_eq!(response.cwd, "/home/agent");
  assert_eq!(served.fake.calls(), ["decide s-actor"]);
  let contexts = served.fake.contexts.lock().unwrap();
  assert_eq!(contexts[0].ancestors, [vec!["bash", "-c", "python3"]]);
}

#[tokio::test]
async fn the_cli_decides_within_the_session_it_names() {
  let served = serve(Caller::Cli).await;

  let response = served.client.sessions().decide(decide("s1", &["python3"])).await.unwrap().into_inner();

  assert_eq!(response.target, "on-s1");
}

#[tokio::test]
async fn a_refused_or_local_route_maps_to_the_wire_shape() {
  let served = serve(actor()).await;

  let refused = served.client.sessions().decide(decide("", &["rm", "-rf", "/"])).await.unwrap().into_inner();
  let local = served.client.sessions().decide(decide("", &["ls"])).await.unwrap().into_inner();

  assert_eq!((refused.target.as_str(), refused.deny.as_str()), ("", "denied: no deleting"));
  assert_eq!((local.target.as_str(), local.deny.as_str()), ("actor", ""));
  assert_eq!(local.env.get("SET").map(String::as_str), Some("1"));
}
