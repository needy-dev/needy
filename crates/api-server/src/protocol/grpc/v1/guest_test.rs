use needy_api_proto::v1::guest_service_control_response::Message;
use needy_api_proto::v1::{
  FetchFilesRequest, GetConfigRequest, GracefulStop, GuestServiceControlRequest, GuestSession, PushEventsRequest, PushFilesRequest,
  PushLayerRequest, ReportExitRequest, RouteList, TraceEvent, UpperLayer,
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

#[tokio::test]
async fn the_actor_is_told_its_layers_image_path_and_session() {
  let served = serve(actor()).await;

  let config = served.client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();

  let layers: Vec<_> = config.layers.into_iter().map(|l| l.digest).collect();
  assert_eq!(
    layers,
    [
      format!("sha256:{}", "a".repeat(64)),
      format!("sha256:{}", "b".repeat(64))
    ]
  );
  assert_eq!(config.image_path, "/usr/local/bin:/usr/bin");
  assert_eq!(
    config.session,
    Some(GuestSession {
      upper: UpperLayer::Kept.into(),
      routing: false,
    })
  );
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
async fn an_upper_layer_is_streamed_in_parts_of_at_most_one_mebibyte() {
  let served = serve(actor()).await;
  let archive: Vec<u8> = (0..(3 << 20) + 5).map(|i| (i % 251) as u8).collect();
  let ended = served.fake.layer_ended.notified();

  needy_api_client::guest::push_layer(&served.client, std::io::Cursor::new(archive.clone())).await.unwrap();
  ended.await;

  let layers = served.fake.layers.lock().unwrap();
  let (run, parts, error) = &layers[0];
  assert_eq!(run, "s-actor");
  assert_eq!(*error, None);
  assert!(parts.len() >= 4 && parts.iter().all(|p| p.len() <= 1 << 20), "{} parts", parts.len());
  assert_eq!(parts.concat(), archive);
}

/// Reads `good` bytes, then fails.
struct Failing {
  good: usize,
}

impl tokio::io::AsyncRead for Failing {
  fn poll_read(
    mut self: std::pin::Pin<&mut Self>,
    _: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<std::io::Result<()>> {
    if self.good == 0 {
      return std::task::Poll::Ready(Err(std::io::Error::other("disk gone")));
    }
    let n = self.good.min(buf.remaining());
    buf.put_slice(&vec![b'x'; n]);
    self.good -= n;
    std::task::Poll::Ready(Ok(()))
  }
}

// A client stream's end carries no error: when the guest's read fails, its
// request stream just ends. Without the marked last part, the daemon would
// take the truncated archive for a whole one.
#[tokio::test]
async fn an_upper_layer_whose_read_fails_reaches_the_daemon_as_a_broken_upload() {
  let served = serve(actor()).await;
  let ended = served.fake.layer_ended.notified();

  // More than the client buffers, so the call is under way when the read
  // fails.
  let error = needy_api_client::guest::push_layer(&served.client, Failing { good: 8 << 20 }).await.unwrap_err();
  tokio::time::timeout(std::time::Duration::from_secs(10), ended).await.unwrap();

  assert!(matches!(error, needy_api_client::Error::Read(_)), "{error}");
  let layers = served.fake.layers.lock().unwrap();
  let (_, _, error) = &layers[0];
  // Ended, or reset as the call is dropped: broken either way.
  assert!(
    matches!(error.as_deref(), Some(e) if e == "the layer upload ended before its last part" || e.starts_with("the layer upload broke: ")),
    "{error:?}"
  );
}

#[tokio::test]
async fn the_control_socket_serves_no_layer_upload() {
  let served = serve(Caller::Operator).await;

  let parts = [PushLayerRequest {
    data: b"x".to_vec(),
    last: true,
  }];
  let status = served.client.guest().push_layer(tokio_stream::iter(parts)).await.unwrap_err();

  assert_eq!(status.code(), Code::PermissionDenied);
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

#[tokio::test]
async fn an_authorized_actor_runner_reaches_guest_service_on_operator_listener() {
  let served = serve(Caller::Operator).await;
  let actor_client = served.client.clone().with_token(crate::testing::MAC_CREDENTIAL).unwrap().with_run_id("s-actor").unwrap();

  let config = actor_client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  assert_eq!(config.run_id, "s-actor");

  actor_client
    .guest()
    .report_exit(ReportExitRequest {
      code: Some(0),
      signal: String::new(),
    })
    .await
    .unwrap();
  let exits = served.fake.exits.lock().unwrap();
  assert_eq!(exits.len(), 1);
  assert_eq!(exits[0].0, "s-actor");
}
