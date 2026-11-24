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

  let stream = ExecStream::attach(
    &served.client,
    needy_api_proto::v1::ExecAttach {
      process_id: "p9".into(),
      replay: true,
      ..Default::default()
    },
  )
  .await
  .unwrap();
  drop(stream);

  assert_eq!(served.fake.calls(), ["attach p9 replay=true"]);
}

#[tokio::test]
async fn an_actor_cannot_list_or_kill_processes() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;

  let list_err = served.client.processes().list_processes(ListProcessesRequest::default()).await.unwrap_err();
  let kill_err = served
    .client
    .processes()
    .kill_process(KillProcessRequest {
      process_id: "p1".into(),
      signal: "SIGKILL".into(),
    })
    .await
    .unwrap_err();

  assert_eq!([list_err.code(), kill_err.code()], [Code::PermissionDenied, Code::PermissionDenied]);
  assert!(served.fake.calls().is_empty());

  // Defense-in-depth directly at the adapter (without middleware).
  use needy_api_proto::v1::process_service_server::ProcessService as _;
  let actor_adapter = super::Adapter {
    processes: served.fake.clone(),
    caller: Caller::Actor {
      run: "s-actor".into(),
    },
  };
  assert_eq!(
    actor_adapter
      .kill_process(tonic::Request::new(KillProcessRequest {
        process_id: "p1".into(),
        signal: "SIGKILL".into(),
      }))
      .await
      .unwrap_err()
      .code(),
    Code::PermissionDenied
  );
  let runner_adapter = super::Adapter {
    processes: served.fake.clone(),
    caller: Caller::Runner { name: "r1".into() },
  };
  assert_eq!(
    runner_adapter
      .kill_process(tonic::Request::new(KillProcessRequest {
        process_id: "p1".into(),
        signal: "SIGKILL".into(),
      }))
      .await
      .unwrap_err()
      .code(),
    Code::PermissionDenied
  );
}

// ROOT CAUSE:
//
// `ProcessService/Exec(Attach)` with an empty `run_id` previously bypassed the
// actor run-scoping check, allowing `Caller::Actor` to attach to any process on
// the control daemon by `process_id`.
//
// Before the fix, `attach.run_id.is_empty()` skipped `run(&caller, ...)` and
// looked up the process globally.
// The fix rejects `Exec(Attach)` for `Caller::Actor` and `Caller::Runner`.
#[tokio::test]
async fn an_actor_cannot_attach_to_a_process_with_empty_run_id() {
  let served = serve(Caller::Actor {
    run: "s-actor".into(),
  })
  .await;

  let err = ExecStream::attach(
    &served.client,
    needy_api_proto::v1::ExecAttach {
      process_id: "p9".into(),
      run_id: String::new(),
      replay: true,
      ..Default::default()
    },
  )
  .await
  .err()
  .expect("an actor must not attach to processes");

  assert_eq!(err.kind(), Kind::Denied);
  assert!(served.fake.calls().is_empty());
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

  let err = ExecStream::start(
    &served.client,
    ExecStart {
      runner: "mac".into(),
      origin: Origin::ActorProxy.into(),
      ..start(&["python3"])
    },
  )
  .await
  .err()
  .expect("an actor cannot name a runner");

  assert_eq!(err.kind(), Kind::Denied);
  assert_eq!(err.to_string(), "an actor cannot name a runner");
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

#[tokio::test]
async fn a_public_client_cannot_supply_process_owner_capacity_policy() {
  let served = serve(Caller::Operator).await;
  let error = ExecStream::start_limited(
    &served.client,
    needy_api_proto::v1::LimitedExecStart {
      start: Some(start(&["true"])),
      capacity: Some(needy_api_proto::v1::ExecutorCapacity {
        key: "forged".into(),
        max_procs: 100,
        on_full: needy_api_proto::v1::CapacityOnFull::Wait.into(),
      }),
    },
  )
  .await
  .err()
  .expect("the lower-level request cannot cross the public control boundary");
  assert_eq!(error.kind(), Kind::Invalid);
  assert!(served.fake.calls().is_empty(), "public policy must never reach execution");
}
