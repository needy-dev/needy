use needy_api_proto::v1::{
  Ancestor, CheckProfileRequest, CreateRunRequest, DecideRequest, FindingSeverity, GetRunEventsRequest, GetRunLogsRequest, GetRunRequest,
  ListRunsRequest, StopRunRequest,
};
use tonic::Code;

use crate::control::Caller;
use crate::testing::serve;

fn actor() -> Caller {
  Caller::Actor {
    run: "s-actor".into(),
  }
}

fn create() -> CreateRunRequest {
  CreateRunRequest {
    profile_yaml: "{}".into(),
    profile_name: "local".into(),
    name: "mine".into(),
    ..Default::default()
  }
}

#[tokio::test]
async fn the_cli_creates_a_run_and_reads_its_state_as_an_enum() {
  let served = serve(Caller::Operator).await;

  let run = served.client.runs().create_run(create()).await.unwrap().into_inner().run.unwrap();

  assert_eq!(run.run_id, "s1");
  assert_eq!(run.name, "mine");
  assert_eq!(run.state(), needy_api_proto::v1::RunState::Running);
  assert_eq!(served.fake.calls(), ["create local"]);
}

#[tokio::test]
async fn a_create_that_names_no_profile_is_invalid() {
  let served = serve(Caller::Operator).await;
  let request = CreateRunRequest {
    profile_name: String::new(),
    ..create()
  };

  let status = served.client.runs().create_run(request).await.unwrap_err();

  assert_eq!(status.code(), Code::InvalidArgument);
  assert_eq!(status.message(), "the request names no profile");
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn an_actor_cannot_create_stop_or_read_the_logs_of_runs() {
  let served = serve(actor()).await;
  let runs = served.client.runs();

  let create = runs.clone().create_run(create()).await.unwrap_err();
  let stop = runs
    .clone()
    .stop_run(StopRunRequest {
      run_id: "s-actor".into(),
      ..Default::default()
    })
    .await
    .unwrap_err();
  let logs = runs
    .clone()
    .get_run_logs(GetRunLogsRequest {
      run_id: "s-actor".into(),
      ..Default::default()
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
  assert_eq!(create.message(), "actors cannot create runs");
  assert!(served.fake.calls().is_empty(), "{:?}", served.fake.calls());
}

#[tokio::test]
async fn a_missing_run_is_not_found() {
  let served = serve(Caller::Operator).await;

  let status = served
    .client
    .runs()
    .get_run(GetRunRequest {
      run_id: "nope".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(status.code(), Code::NotFound);
  assert_eq!(status.message(), "no such run");
}

#[tokio::test]
async fn runs_are_listed_and_stopped() {
  let served = serve(Caller::Operator).await;

  let list = served.client.runs().list_runs(ListRunsRequest {}).await.unwrap().into_inner().runs;
  served
    .client
    .runs()
    .stop_run(StopRunRequest {
      run_id: "s2".into(),
      ..Default::default()
    })
    .await
    .unwrap();

  assert_eq!(list.iter().map(|s| s.run_id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
  assert_eq!(served.fake.calls(), ["stop s2 by operator"]);
}

#[tokio::test]
async fn a_stop_carries_its_grace_period_or_now_and_says_how_the_actor_ended() {
  let served = serve(Caller::Operator).await;
  let mut runs = served.client.runs();

  let graceful = runs
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
      grace_ms: Some(2500),
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  let now = runs
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
      now: true,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  let both = runs
    .stop_run(StopRunRequest {
      run_id: "s1".into(),
      now: true,
      grace_ms: Some(1),
    })
    .await
    .unwrap_err();

  assert_eq!((graceful.graceful, graceful.reason.as_str()), (Some(true), ""));
  assert_eq!((now.graceful, now.reason.as_str()), (Some(false), "asked to stop at once"));
  assert_eq!(both.code(), Code::InvalidArgument);
  assert_eq!(
    served.fake.calls(),
    [
      "stop s1 by operator Graceful { grace: 2.5s }",
      "stop s1 by operator Now"
    ]
  );
}

#[tokio::test]
async fn logs_arrive_in_chunks_that_add_up() {
  let served = serve(Caller::Operator).await;

  let mut stream = served
    .client
    .runs()
    .get_run_logs(GetRunLogsRequest {
      run_id: "s1".into(),
      ..Default::default()
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

fn decide(run: &str, argv: &[&str]) -> DecideRequest {
  DecideRequest {
    run_id: run.into(),
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: "/home/agent".into(),
    ancestors: vec![Ancestor {
      argv: vec!["bash".into(), "-c".into(), "python3".into()],
    }],
    ..Default::default()
  }
}

#[tokio::test]
async fn an_actor_decides_within_its_own_run_whatever_the_request_names() {
  let served = serve(actor()).await;

  let response = served.client.runs().decide(decide("someone-else", &["python3"])).await.unwrap().into_inner();

  assert_eq!(response.target, "on-s-actor");
  assert_eq!(response.cwd, "/home/agent");
  assert_eq!(served.fake.calls(), ["decide s-actor"]);
  let contexts = served.fake.contexts.lock().unwrap();
  assert_eq!(contexts[0].ancestors, [vec!["bash", "-c", "python3"]]);
}

#[tokio::test]
async fn the_cli_decides_within_the_run_it_names() {
  let served = serve(Caller::Operator).await;

  let response = served.client.runs().decide(decide("s1", &["python3"])).await.unwrap().into_inner();

  assert_eq!(response.target, "on-s1");
}

#[tokio::test]
async fn a_refused_or_local_route_maps_to_the_wire_shape() {
  let served = serve(actor()).await;

  let refused = served.client.runs().decide(decide("", &["rm", "-rf", "/"])).await.unwrap().into_inner();
  let local = served.client.runs().decide(decide("", &["ls"])).await.unwrap().into_inner();

  assert_eq!((refused.target.as_str(), refused.deny.as_str()), ("", "denied: no deleting"));
  assert_eq!((local.target.as_str(), local.deny.as_str()), ("actor", ""));
  assert_eq!(local.env.get("SET").map(String::as_str), Some("1"));
}

#[tokio::test]
async fn the_cli_checks_a_profile_and_reads_placements_and_findings() {
  let served = serve(Caller::Operator).await;
  let request = CheckProfileRequest {
    profile_yaml: "{}".into(),
    recipe_yaml: String::new(),
  };

  let check = served.client.runs().check_profile(request).await.unwrap().into_inner();

  assert_eq!(check.actor.unwrap().runner, "kvm");
  assert_eq!(check.executors["mac"].runner, "mac");
  // The control host is an empty runner name.
  assert_eq!(check.executors["build"].runner, "");
  assert_eq!(check.actor_cpu, "Fake CPU");
  assert_eq!((check.actor_image.as_str(), check.actor_base_abi.as_str()), ("needy-custom-jq", "1"));
  assert_eq!(check.findings.len(), 1);
  assert_eq!(check.findings[0].severity(), FindingSeverity::Error);
  assert_eq!(check.findings[0].message, "recipe: avx512f is missing");
  assert_eq!(served.fake.calls(), ["check {}"]);
}

#[tokio::test]
async fn an_actor_cannot_check_profiles() {
  let served = serve(actor()).await;

  let status = served.client.runs().check_profile(CheckProfileRequest::default()).await.unwrap_err();

  assert_eq!(status.code(), Code::PermissionDenied);
  assert!(served.fake.calls().is_empty());
}

#[tokio::test]
async fn followed_logs_stream_each_read_and_end_with_the_run() {
  let served = serve(Caller::Operator).await;

  let request = GetRunLogsRequest {
    run_id: "s1".into(),
    follow: true,
  };
  let mut stream = served.client.runs().get_run_logs(request).await.unwrap().into_inner();
  let mut reads = Vec::new();
  while let Some(chunk) = stream.message().await.unwrap() {
    reads.push(String::from_utf8(chunk.data).unwrap());
  }

  assert_eq!(reads, ["boot\n", "later\n"]);
  assert_eq!(served.fake.calls(), ["logs s1 follow=true"]);
}

#[tokio::test]
async fn run_events_arrive_in_order_in_messages_that_name_the_full_run() {
  let served = serve(Caller::Operator).await;

  let request = GetRunEventsRequest { run_id: "s".into() };
  let mut stream = served.client.runs().get_run_events(request).await.unwrap().into_inner();
  let mut sizes = Vec::new();
  let mut seqs = Vec::new();
  while let Some(message) = stream.message().await.unwrap() {
    assert_eq!(message.run_id, "s1");
    sizes.push(message.events.len());
    seqs.extend(message.events.iter().map(|e| e.seq));
  }

  assert_eq!(sizes, [1000, 1000, 500]);
  assert_eq!(seqs, (0..2500).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_run_without_events_is_one_message_that_names_it() {
  let served = serve(Caller::Operator).await;

  let request = GetRunEventsRequest {
    run_id: "empty".into(),
  };
  let mut stream = served.client.runs().get_run_events(request).await.unwrap().into_inner();
  let first = stream.message().await.unwrap().unwrap();

  assert_eq!((first.run_id.as_str(), first.events.len()), ("empty", 0));
  assert!(stream.message().await.unwrap().is_none());
}

#[tokio::test]
async fn an_actor_cannot_read_run_events() {
  let served = serve(actor()).await;

  let refused = served
    .client
    .runs()
    .get_run_events(GetRunEventsRequest {
      run_id: "s1".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(refused.code(), Code::PermissionDenied);
}
