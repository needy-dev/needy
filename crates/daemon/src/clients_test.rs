use std::time::Duration;

use needy_api_client::Client;
use needy_api_client::proto::v1::{
  CreatePairingCodeRequest, CreateRunRequest, ExecRequest, ExecStart, GetRunEventsRequest, GetRunLogsRequest, KillProcessRequest,
  ListClientsRequest, ListRunsRequest, Origin, PairRequest, RevokeClientRequest, StopRunRequest, exec_request,
};
use needy_core::event::Event;
use tonic::Code;

use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";

const PROFILE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

async fn served() -> Served {
  testing::serve(testing::daemon().await, Some(CONTROL_TOKEN)).await
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

/// A client named `name` paired through `served`, holding its own token.
async fn paired(served: &Served, name: &str) -> (Client, String) {
  let request = CreatePairingCodeRequest {
    ttl_ms: 60_000,
    name: name.into(),
  };
  let code = operator(served).clients().create_pairing_code(request).await.unwrap().into_inner().code;
  let paired = served.client.clients().pair(PairRequest { code }).await.unwrap().into_inner();
  let id = paired.client.unwrap().id;
  (served.client.clone().with_token(&paired.token).unwrap(), id)
}

fn create(served: &Served) -> CreateRunRequest {
  let resolved = needy_config::profile::resolve(&needy_config::Source::text(PROFILE, served.dir())).unwrap();
  CreateRunRequest {
    profile_yaml: resolved.yaml,
    profile_name: "test".into(),
    ..Default::default()
  }
}

/// The decoded trace events of `run`, read through the control plane.
async fn events(client: &Client, run: &str) -> Vec<Event> {
  let mut stream = client.runs().get_run_events(GetRunEventsRequest { run_id: run.into() }).await.unwrap().into_inner();
  let mut events = Vec::new();
  while let Some(message) = stream.message().await.unwrap() {
    assert_eq!(message.run_id, run);
    for stored in message.events {
      events.push(Event::decode(&stored.kind, stored.version, &stored.payload).unwrap());
    }
  }
  events
}

#[tokio::test]
async fn a_paired_client_works_until_it_is_revoked() {
  let served = served().await;
  let (client, id) = paired(&served, "laptop").await;

  client.runs().list_runs(ListRunsRequest {}).await.unwrap();
  let listed = operator(&served).clients().list_clients(ListClientsRequest {}).await.unwrap().into_inner().clients;
  operator(&served).clients().revoke_client(RevokeClientRequest { id: id.clone() }).await.unwrap();
  let refused = client.runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].id.as_str(), listed[0].name.as_str()), (id.as_str(), "laptop"));
  assert!(listed[0].last_used_at_ms > 0, "the list call used the token");
  assert_eq!(refused.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn a_pairing_code_pairs_one_client() {
  let served = served().await;
  let request = CreatePairingCodeRequest {
    ttl_ms: 60_000,
    name: "laptop".into(),
  };
  let code = operator(&served).clients().create_pairing_code(request).await.unwrap().into_inner().code;

  served.client.clients().pair(PairRequest { code: code.clone() }).await.unwrap();
  let again = served.client.clients().pair(PairRequest { code }).await.unwrap_err();

  assert_eq!(again.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn the_trace_names_the_client_that_created_and_the_operator_that_stopped_a_run() {
  let served = served().await;
  let (client, _) = paired(&served, "laptop").await;

  let run = client.runs().create_run(create(&served)).await.unwrap().into_inner().run.unwrap().run_id;
  let start = ExecRequest {
    payload: Some(exec_request::Payload::Start(ExecStart {
      argv: vec!["sleep".into(), "30".into()],
      run_id: run.clone(),
      origin: Origin::Cli as i32,
      detach: true,
      no_stdin: true,
      ..Default::default()
    })),
  };
  let mut outputs = client.processes().exec(tokio_stream::iter([start])).await.unwrap().into_inner();
  let process = loop {
    let message = outputs.message().await.unwrap().expect("the exec starts");
    if let Some(needy_api_client::proto::v1::exec_response::Payload::Started(started)) = message.payload {
      break started.process_id;
    }
  };
  let kill = KillProcessRequest {
    process_id: process.clone(),
    signal: "TERM".into(),
  };
  client.processes().kill_process(kill).await.unwrap();
  operator(&served)
    .runs()
    .stop_run(StopRunRequest {
      run_id: run.clone(),
      ..Default::default()
    })
    .await
    .unwrap();

  let events = events(&client, &run).await;
  let by = |event: &Event| match event {
    Event::RunCreate(e) => e.by.as_ref().map(|by| format!("run.create {by}")),
    Event::RunStop(e) => e.by.as_ref().map(|by| format!("run.stop {by}")),
    Event::ExecStart(e) => e.by.as_ref().map(|by| format!("exec.start {by}")),
    Event::ExecKill(e) => e.by.as_ref().map(|by| format!("exec.kill {} {by}", e.signal)),
    _ => None,
  };
  let attributed: Vec<String> = events.iter().filter_map(by).collect();
  assert_eq!(
    attributed,
    [
      "run.create client:laptop",
      "exec.start client:laptop",
      "exec.kill TERM client:laptop",
      "run.stop operator",
    ]
  );
}

#[tokio::test]
async fn run_events_of_an_unknown_run_are_not_found() {
  let served = served().await;

  let missing = operator(&served)
    .runs()
    .get_run_events(GetRunEventsRequest {
      run_id: "nobody".into(),
    })
    .await
    .unwrap_err();

  assert_eq!(missing.code(), Code::NotFound);
}

#[tokio::test]
async fn followed_logs_carry_what_the_actor_writes_until_the_run_stops() {
  // An actor that writes one line now and one later.
  let started = testing::daemon().await;
  let script =
    r#"dir=$(dirname "$3"); touch "$dir/v.sock"; echo first; sleep 0.6; echo second; while kill -0 $PPID 2>/dev/null; do sleep 0.05; done"#;
  testing::fake_image(&started.dir.path().join("image"), script);
  let served = testing::serve(started, Some(CONTROL_TOKEN)).await;
  let (client, _) = paired(&served, "laptop").await;
  let run = client.runs().create_run(create(&served)).await.unwrap().into_inner().run.unwrap().run_id;

  let request = GetRunLogsRequest {
    run_id: run.clone(),
    follow: true,
  };
  let mut stream = client.runs().get_run_logs(request).await.unwrap().into_inner();
  let mut console = String::new();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !console.contains("second") {
    let chunk = tokio::time::timeout_at(deadline, stream.message()).await.expect("the second line arrives").unwrap().unwrap();
    console.push_str(&String::from_utf8_lossy(&chunk.data));
  }
  operator(&served)
    .runs()
    .stop_run(StopRunRequest {
      run_id: run,
      ..Default::default()
    })
    .await
    .unwrap();
  let end = tokio::time::timeout(Duration::from_secs(5), async { while stream.message().await.unwrap().is_some() {} }).await;

  assert!(console.contains("first\n"), "{console}");
  assert!(end.is_ok(), "the stream ends with the run");
}
