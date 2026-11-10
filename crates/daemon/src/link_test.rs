//! The control plane and a runner in one process: the runner dials the
//! control plane's socket (through a proxy a test can cut), and a client
//! runs commands on the runner through the control plane.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use needy_api_client::exec::{Event, ExecStream};
use needy_api_client::proto::v1::{
  CreateJoinTokenRequest, ExecStart, KillProcessRequest, ListProcessesRequest, Origin, ProcessInfo, RemoveRunnerRequest, WindowSize,
};
use needy_api_client::{Client, Error, Kind};
use needy_core::types::RunnerRole;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runner::{Config, Runner};
use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";

/// A Unix socket that forwards every connection to `target`, until cut.
struct Proxy {
  endpoint: String,
  connections: std::sync::Arc<std::sync::Mutex<Vec<JoinHandle<()>>>>,
  _accept: JoinHandle<()>,
}

impl Proxy {
  fn start(dir: &Path, target: &str) -> Self {
    let socket = dir.join("proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let target = target.strip_prefix("unix:").unwrap().to_string();
    let connections: std::sync::Arc<std::sync::Mutex<Vec<JoinHandle<()>>>> = Default::default();
    let open = connections.clone();
    let accept = tokio::spawn(async move {
      while let Ok((mut inbound, _)) = listener.accept().await {
        let target = target.clone();
        let forward = tokio::spawn(async move {
          if let Ok(mut outbound) = tokio::net::UnixStream::connect(&target).await {
            // Either side closing ends the connection.
            let _copied = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
          }
        });
        open.lock().unwrap().push(forward);
      }
    });
    Self {
      endpoint: format!("unix:{}", socket.display()),
      connections,
      _accept: accept,
    }
  }

  /// Drops every open connection at once, as a dying tunnel does.
  fn cut(&self) {
    for connection in self.connections.lock().unwrap().drain(..) {
      connection.abort();
    }
  }
}

/// A control plane, a runner `mac` joined to it through a proxy, and an
/// operator's client. The runner stops when `stop` is cancelled.
struct Setup {
  served: Served,
  proxy: Proxy,
  stop: CancellationToken,
  runner: JoinHandle<anyhow::Result<()>>,
  _runner_state: tempfile::TempDir,
}

impl Setup {
  async fn start() -> Self {
    let served = testing::serve(testing::daemon().await, Some(CONTROL_TOKEN)).await;
    let proxy = Proxy::start(served.dir(), &served.endpoint);
    let state = tempfile::tempdir().unwrap();
    let request = CreateJoinTokenRequest {
      ttl_ms: 60_000,
      uses: 1,
      ..Default::default()
    };
    let token = operator(&served).runners().create_join_token(request).await.unwrap().into_inner().token;
    let config = Config {
      state_dir: state.path().to_path_buf(),
      control: Some(proxy.endpoint.clone()),
      join_token: Some(token),
      name: "mac".into(),
      roles: [RunnerRole::Executor].into(),
      labels: BTreeMap::new(),
    };
    let runner = Runner::start(config).await.unwrap();
    let stop = CancellationToken::new();
    let runner = tokio::spawn(runner.serve(stop.clone()));
    let setup = Self {
      served,
      proxy,
      stop,
      runner,
      _runner_state: state,
    };
    setup.connected().await;
    setup
  }

  fn client(&self) -> Client {
    operator(&self.served)
  }

  /// Waits until the runner's control stream is open.
  async fn connected(&self) {
    let client = self.client();
    eventually("the runner connects", async || list(&client, "mac").await.is_ok()).await;
  }
}

impl Drop for Setup {
  fn drop(&mut self) {
    self.stop.cancel();
  }
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

async fn list(client: &Client, runner: &str) -> Result<Vec<ProcessInfo>, tonic::Status> {
  let request = ListProcessesRequest {
    runner: runner.into(),
    ..Default::default()
  };
  Ok(client.processes().list_processes(request).await?.into_inner().processes)
}

/// Waits until `check` holds, for at most 10 s.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !check().await {
    assert!(tokio::time::Instant::now() < deadline, "{what}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

fn on_runner(runner: &str, argv: &[&str]) -> ExecStart {
  ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    runner: runner.into(),
    origin: Origin::Cli.into(),
    ..Default::default()
  }
}

/// Reads events until the process exits or the stream fails.
async fn drain(stream: &mut ExecStream) -> (Vec<Event>, Option<Error>) {
  let mut events = Vec::new();
  loop {
    match stream.next().await {
      Ok(Some(event)) => {
        let exited = matches!(event, Event::Exited(_));
        events.push(event);
        if exited {
          return (events, None);
        }
      }
      Ok(None) => return (events, None),
      Err(e) => return (events, Some(e)),
    }
  }
}

fn output(events: &[Event]) -> String {
  let bytes: Vec<u8> = events
    .iter()
    .flat_map(|e| match e {
      Event::Stdout(b) => b.clone(),
      _ => Vec::new(),
    })
    .collect();
  String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn a_command_on_a_runner_streams_its_output_exit_code_and_signal() {
  let setup = Setup::start().await;
  let client = setup.client();

  let out =
    ExecStream::start(&client, on_runner("mac", &["sh", "-c", "echo out; echo err >&2; exit 3"])).await.unwrap().run(None).await.unwrap();
  let mut killed = ExecStream::start(&client, on_runner("mac", &["sh", "-c", "kill -TERM $$"])).await.unwrap();
  let (events, _) = drain(&mut killed).await;

  assert_eq!((out.stdout.as_slice(), out.stderr.as_slice(), out.exit.code), (&b"out\n"[..], &b"err\n"[..], Some(3)));
  let Some(Event::Started(started)) = events.first() else {
    panic!("{events:?}")
  };
  assert_eq!(started.executor, "mac");
  let Some(Event::Exited(exit)) = events.last() else {
    panic!("{events:?}")
  };
  assert_eq!((exit.code, exit.signal.as_str()), (None, "SIGTERM"));
}

#[tokio::test]
async fn a_terminal_on_a_runner_gets_its_size_input_and_resizes() {
  let setup = Setup::start().await;
  let start = ExecStart {
    tty: Some(WindowSize {
      rows: 30,
      cols: 100,
    }),
    ..on_runner(
      "mac",
      &[
        "sh",
        "-c",
        "stty size; read line; stty size; echo got $line",
      ],
    )
  };

  let mut stream = ExecStream::start(&setup.client(), start).await.unwrap();
  let input = stream.input();
  let mut seen = String::new();
  while !seen.contains("30 100") {
    match stream.next().await.unwrap() {
      Some(Event::Stdout(bytes)) => seen.push_str(&String::from_utf8_lossy(&bytes)),
      Some(_) => {}
      None => panic!("the stream ended: {seen:?}"),
    }
  }
  input
    .resize(WindowSize {
      rows: 40,
      cols: 120,
    })
    .await
    .unwrap();
  input.stdin(b"hello\n".to_vec()).await.unwrap();
  let (events, error) = drain(&mut stream).await;

  assert!(error.is_none(), "{error:?}");
  let rest = output(&events);
  assert!(rest.contains("40 120") && rest.contains("got hello"), "{rest:?}");
}

#[tokio::test]
async fn a_detached_process_on_a_runner_is_listed_attached_and_killed() {
  let setup = Setup::start().await;
  let client = setup.client();
  let start = ExecStart {
    detach: true,
    no_stdin: true,
    ..on_runner("mac", &["sh", "-c", "echo ready; exec sleep 600"])
  };

  let mut started = ExecStream::start(&client, start).await.unwrap();
  let Some(Event::Started(process)) = started.next().await.unwrap() else {
    panic!("no start")
  };
  drop(started);
  let listed = list(&client, "mac").await.unwrap();
  let mut attached = ExecStream::attach(&client, &process.process_id, true, None).await.unwrap();
  let mut replayed = String::new();
  while !replayed.contains("ready") {
    if let Some(Event::Stdout(bytes)) = attached.next().await.unwrap() {
      replayed.push_str(&String::from_utf8_lossy(&bytes));
    }
  }
  let kill = KillProcessRequest {
    process_id: process.process_id.clone(),
    signal: "SIGKILL".into(),
  };
  client.processes().kill_process(kill).await.unwrap();
  let (events, _) = drain(&mut attached).await;

  let listed = listed.iter().find(|p| p.process_id == process.process_id).expect("listed");
  assert!(listed.running);
  assert_eq!(listed.executor, "mac");
  let Some(Event::Exited(exit)) = events.last() else {
    panic!("{events:?}")
  };
  assert_eq!(exit.signal, "SIGKILL");
}

#[tokio::test]
async fn a_command_a_runner_cannot_start_is_refused_as_on_any_executor() {
  let setup = Setup::start().await;

  let error = ExecStream::start(&setup.client(), on_runner("mac", &["needy-no-such-command"])).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Failed, "{error}");
}

#[tokio::test]
async fn a_runner_that_is_not_connected_fails_at_once() {
  let setup = Setup::start().await;
  let client = setup.client();
  let started = tokio::time::Instant::now();

  let exec = ExecStream::start(&client, on_runner("ghost", &["true"])).await.err().unwrap();
  let listed = list(&client, "ghost").await.unwrap_err();

  assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
  assert_eq!((exec.kind(), exec.to_string().as_str()), (Kind::Unavailable, "runner ghost is not connected"));
  assert_eq!((listed.code(), listed.message()), (tonic::Code::Unavailable, "runner ghost is not connected"));
}

#[tokio::test]
async fn a_command_names_a_run_or_a_runner_not_both() {
  let setup = Setup::start().await;
  let start = ExecStart {
    run_id: "some-run".into(),
    ..on_runner("mac", &["true"])
  };

  let error = ExecStream::start(&setup.client(), start).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Invalid, "{error}");
}

#[tokio::test]
async fn a_runner_connects_again_after_its_connection_drops_and_execs_in_flight_end() {
  let setup = Setup::start().await;
  let client = setup.client();
  let mut running = ExecStream::start(&client, on_runner("mac", &["sh", "-c", "echo up; exec sleep 600"])).await.unwrap();
  while !matches!(running.next().await.unwrap(), Some(Event::Stdout(_))) {}
  let detached = ExecStart {
    detach: true,
    no_stdin: true,
    ..on_runner("mac", &["sleep", "600"])
  };
  let mut stream = ExecStream::start(&client, detached).await.unwrap();
  let Some(Event::Started(survivor)) = stream.next().await.unwrap() else {
    panic!("no start")
  };
  drop(stream);

  setup.proxy.cut();
  let (_, ended) = drain(&mut running).await;
  setup.connected().await;
  let again = ExecStream::start(&client, on_runner("mac", &["echo", "back"])).await.unwrap().run(None).await.unwrap();

  let ended = ended.expect("the stream failed");
  assert_eq!(ended.kind(), Kind::Ended, "{ended}");
  assert_eq!(ended.to_string(), "the connection ended before the command exited: runner mac disconnected");
  assert_eq!(again.stdout, b"back\n");
  let listed = list(&client, "mac").await.unwrap();
  assert!(listed.iter().any(|p| p.process_id == survivor.process_id && p.running), "the detached process keeps running: {listed:?}");
}

#[tokio::test]
async fn a_removed_runner_loses_its_control_stream_and_stops() {
  let mut setup = Setup::start().await;
  let client = setup.client();

  client.runners().remove_runner(RemoveRunnerRequest { name: "mac".into() }).await.unwrap();
  let ended = tokio::time::timeout(Duration::from_secs(10), &mut setup.runner).await.expect("the runner stops").unwrap();
  let exec = ExecStream::start(&client, on_runner("mac", &["true"])).await.err().unwrap();

  let error = ended.unwrap_err().to_string();
  assert!(error.starts_with("the control plane refused this runner's credential"), "{error}");
  assert_eq!(exec.to_string(), "runner mac is not connected");
}
