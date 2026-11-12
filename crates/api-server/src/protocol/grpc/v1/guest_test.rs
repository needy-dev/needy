use needy_api_proto::v1::guest_service_control_response::Message;
use needy_api_proto::v1::{
  FetchFilesRequest, GetConfigRequest, GracefulStop, GuestServiceControlRequest, PushEventsRequest, PushFilesRequest, ReportExitRequest,
  RouteList, TraceEvent,
};
use needy_core::event::{Event, RunStop, SCHEMA_VERSION};
use tonic::Code;

use crate::control::Caller;
use crate::testing::serve;

fn actor() -> Caller {
  Caller::Actor {
    run: "s-actor".into(),
  }
}

#[tokio::test]
async fn the_control_socket_serves_no_guest() {
  let served = serve(Caller::Operator).await;

  let status = served.client.guest().get_config(GetConfigRequest {}).await.unwrap_err();

  assert_eq!(status.code(), Code::PermissionDenied);
  assert_eq!(status.message(), "not an actor listener");
}

#[tokio::test]
async fn the_actor_gets_its_run_s_configuration() {
  let served = serve(actor()).await;

  let config = served.client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();

  assert_eq!(config.run_id, "s-actor");
  assert_eq!(config.user.map(|u| (u.name, u.uid)), Some(("agent".into(), 1000)));
  assert_eq!(config.mounts[0].target, "/home/agent/workspace");
  assert_eq!(config.gateway_port, 7001);
}

fn pushed(version: u32) -> PushEventsRequest {
  let event = Event::RunStop(RunStop::default());
  PushEventsRequest {
    event: Some(TraceEvent {
      time_ms: 99,
      kind: event.kind().as_str().into(),
      payload: event.payload(),
      version,
    }),
  }
}

#[tokio::test]
async fn guest_events_are_decoded_into_the_run() {
  let served = serve(actor()).await;

  served.client.guest().push_events(tokio_stream::iter([pushed(SCHEMA_VERSION)])).await.unwrap();

  let events = served.fake.events.lock().unwrap();
  assert_eq!(*events, [("s-actor".to_string(), 99, Event::RunStop(RunStop::default()))]);
}

#[tokio::test]
async fn a_guest_event_of_another_schema_version_is_invalid() {
  let served = serve(actor()).await;

  let status = served.client.guest().push_events(tokio_stream::iter([pushed(SCHEMA_VERSION + 1)])).await.unwrap_err();

  assert_eq!(status.code(), Code::InvalidArgument);
  assert!(status.message().starts_with("guest event run.stop: "), "{}", status.message());
  assert!(served.fake.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_driving_program_s_exit_is_reported() {
  let served = serve(actor()).await;

  served
    .client
    .guest()
    .report_exit(ReportExitRequest {
      code: None,
      signal: "SIGKILL".into(),
    })
    .await
    .unwrap();

  let exits = served.fake.exits.lock().unwrap();
  assert_eq!(exits[0].0, "s-actor");
  assert_eq!((exits[0].1.code, exits[0].1.signal.as_str()), (None, "SIGKILL"));
}

#[tokio::test]
async fn copy_in_arrives_in_chunks_and_a_missing_mapping_is_not_found() {
  let served = serve(actor()).await;

  let mut stream = served.client.guest().fetch_files(FetchFilesRequest { name: "big".into() }).await.unwrap().into_inner();
  let mut total = 0;
  let mut largest = 0;
  while let Some(chunk) = stream.message().await.unwrap() {
    total += chunk.data.len();
    largest = largest.max(chunk.data.len());
  }
  let missing = served
    .client
    .guest()
    .fetch_files(FetchFilesRequest {
      name: "nope".into(),
    })
    .await
    .unwrap_err();

  assert_eq!((total, largest), (3 << 20, 1 << 20));
  assert_eq!(missing.code(), Code::NotFound);
}

#[tokio::test]
async fn mirror_out_chunks_are_joined_under_the_first_name() {
  let served = serve(actor()).await;
  let chunks = [
    PushFilesRequest {
      name: "out".into(),
      data: b"ab".to_vec(),
    },
    PushFilesRequest {
      name: String::new(),
      data: b"cd".to_vec(),
    },
  ];

  served.client.guest().push_files(tokio_stream::iter(chunks)).await.unwrap();

  let pushed = served.fake.pushed.lock().unwrap();
  assert_eq!(*pushed, [("s-actor".to_string(), "out".to_string(), b"abcd".to_vec())]);
}

#[tokio::test]
async fn the_actor_s_control_stream_carries_its_run_s_route_lists_and_a_stop() {
  let served = serve(actor()).await;

  let mut stream = served.client.guest().control(GuestServiceControlRequest {}).await.unwrap().into_inner();
  let mut messages = Vec::new();
  while let Some(message) = stream.message().await.unwrap() {
    messages.push(message.message);
  }

  let routes = |names: &[&str]| {
    Some(Message::Routes(RouteList {
      names: names.iter().map(|n| n.to_string()).collect(),
    }))
  };
  let stop = Some(Message::Stop(GracefulStop { grace_ms: 1500 }));
  assert_eq!(
    messages,
    [
      routes(&["s-actor:git"]),
      routes(&["s-actor:curl", "s-actor:git"]),
      stop
    ]
  );
}

#[tokio::test]
async fn the_control_socket_serves_no_control_stream() {
  let served = serve(Caller::Operator).await;

  let status = served.client.guest().control(GuestServiceControlRequest {}).await.unwrap_err();

  assert_eq!(status.code(), Code::PermissionDenied);
}
