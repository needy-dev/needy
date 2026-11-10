use needy_api_client::Kind;
use needy_api_client::exec::{Event, ExecStream};
use needy_api_proto::v1::{ExecRequest, ExecStart, KillProcessRequest, ListProcessesRequest, Origin, exec_request};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;

use crate::control::Caller;
use crate::testing::{refusal_event, serve};

fn start(argv: &[&str]) -> ExecStart {
  ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    run_id: "s1".into(),
    origin: Origin::Cli.into(),
    ..Default::default()
  }
}

#[tokio::test]
async fn an_exec_carries_input_to_the_daemon_and_output_back() {
  let served = serve(Caller::Operator).await;

  let output = ExecStream::start(&served.client, start(&["cat"])).await.unwrap().run(Some(b"hello".to_vec())).await.unwrap();

  assert_eq!(output.stdout, b"hello");
  assert_eq!(output.exit.code, Some(3));
  assert_eq!(served.fake.calls(), [r#"exec cat in "s1" from Cli"#]);
}

#[tokio::test]
async fn an_actor_execs_within_its_own_run() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;
  let mut stream = ExecStream::start(
    &served.client,
    ExecStart {
      run_id: "someone-else".into(),
      origin: Origin::ActorProxy.into(),
      ..start(&["python3"])
    },
  )
  .await
  .unwrap();

  let Some(Event::Started(started)) = stream.next().await.unwrap() else {
    panic!("no started message");
  };

  assert_eq!(started.executor, "s-actor");
  assert_eq!(served.fake.calls(), [r#"exec python3 in "s-actor" from ActorProxy by actor:s-actor"#]);
}

#[tokio::test]
async fn a_refused_exec_is_denied_with_its_events_in_the_details() {
  let served = serve(Caller::Operator).await;

  let error = ExecStream::start(&served.client, start(&["refuse"])).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Denied);
  assert_eq!(error.to_string(), "denied: refused");
  let events = error.refusal().unwrap();
  assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), [refusal_event().kind().as_str()]);
}

#[tokio::test]
async fn a_full_executor_is_resource_exhausted() {
  let served = serve(Caller::Operator).await;

  let error = ExecStream::start(&served.client, start(&["full"])).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Exhausted);
}

#[tokio::test]
async fn an_unspecified_origin_is_invalid() {
  let served = serve(Caller::Operator).await;
  let request = ExecStart {
    origin: Origin::Unspecified.into(),
    ..start(&["true"])
  };

  let error = ExecStream::start(&served.client, request).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Invalid);
  assert_eq!(error.to_string(), "the exec origin is unspecified");
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn a_stream_must_start_or_attach_first() {
  let served = serve(Caller::Operator).await;
  let (tx, rx) = mpsc::channel(1);
  tx.send(ExecRequest {
    payload: Some(exec_request::Payload::Stdin(b"x".to_vec())),
  })
  .await
  .unwrap();

  let status = served.client.processes().exec(ReceiverStream::new(rx)).await.unwrap_err();

  assert_eq!(status.code(), Code::InvalidArgument);
  assert_eq!(status.message(), "the first request must start or attach");
}

#[tokio::test]
async fn an_attach_names_the_process_and_replay() {
  let served = serve(Caller::Operator).await;

  let stream = ExecStream::attach(&served.client, "p9", true, None).await.unwrap();
  drop(stream);

  assert_eq!(served.fake.calls(), ["attach p9 replay=true"]);
}

#[tokio::test]
async fn an_actor_lists_only_its_own_run() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;

  let listed = served.client.processes().list_processes(ListProcessesRequest::default()).await.unwrap().into_inner().processes;

  assert_eq!(served.fake.calls(), [r#"list "s-actor""#]);
  assert_eq!(listed[0].run_id, "s-actor");
  assert_eq!(listed[0].origin, Origin::Cli as i32);
  assert!(!listed[0].running);
  assert_eq!(listed[0].exited.as_ref().map(|e| e.signal.as_str()), Some("SIGKILL"));
}

#[tokio::test]
async fn killing_a_missing_process_is_not_found() {
  let served = serve(Caller::Operator).await;

  let status = served
    .client
    .processes()
    .kill_process(KillProcessRequest {
      process_id: "gone".into(),
      signal: "SIGTERM".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(status.code(), Code::NotFound);
  assert_eq!(status.message(), "no such process");
}

#[tokio::test]
async fn an_actor_cannot_name_a_runner() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;

  let request = ListProcessesRequest {
    runner: "mac".into(),
    ..Default::default()
  };
  let status = served.client.processes().list_processes(request).await.unwrap_err();

  assert_eq!((status.code(), status.message()), (tonic::Code::PermissionDenied, "an actor cannot name a runner"));
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn a_client_lists_one_runner_s_processes() {
  let served = serve(Caller::Operator).await;

  let request = ListProcessesRequest {
    runner: "mac".into(),
    ..Default::default()
  };
  served.client.processes().list_processes(request).await.unwrap();

  assert_eq!(served.fake.calls(), [r#"list "" on runner mac"#]);
}
