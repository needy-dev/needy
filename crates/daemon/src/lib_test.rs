use std::collections::HashMap;
use std::time::Duration;

use needy_api_server::control::{ControlError, ExecOpen, ExecOutput, ExecSpec, Processes, Runs};
use needy_core::types::{Origin, RunState};
use tokio_stream::StreamExt as _;

use crate::testing::{self, new_run, running};

const PROFILE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

#[tokio::test]
async fn shutdown_stops_runs_and_the_local_executors_processes() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let first = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let second = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let spec = ExecSpec {
    argv: vec!["sleep".into(), "30".into()],
    cwd: String::new(),
    env: HashMap::new(),
    clear_env: false,
    tty: None,
    detach: true,
    run: String::new(),
    executor: String::new(),
    runner: String::new(),
    no_stdin: true,
    origin: Origin::Cli,
    search_path_if_missing: false,
    by: None,
  };
  let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
  let pid = loop {
    if let ExecOutput::Started(started) = outputs.next().await.unwrap().unwrap() {
      break started.pid;
    }
  };
  // A detached process outlives its caller.
  drop(outputs);
  assert!(running(pid));

  tokio::time::timeout(Duration::from_secs(20), daemon.shutdown()).await.expect("shutdown waits for every task");

  let states: Vec<_> = Runs::list(daemon).await.unwrap().into_iter().map(|s| (s.id, s.state)).collect();
  assert_eq!(
    states,
    [
      (first.id, RunState::Stopped),
      (second.id, RunState::Stopped)
    ]
  );
  assert!(!running(pid), "the local executor's processes are killed");
}

#[tokio::test]
async fn shutdown_ends_open_streams_saying_the_daemon_is_shutting_down() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  let run = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let mut streams = Vec::new();
  for run in [String::new(), run.id] {
    let spec = ExecSpec {
      argv: vec!["sleep".into(), "30".into()],
      cwd: String::new(),
      env: HashMap::new(),
      clear_env: false,
      tty: None,
      detach: false,
      run,
      executor: String::new(),
      runner: String::new(),
      no_stdin: true,
      origin: Origin::Cli,
      search_path_if_missing: false,
      by: None,
    };
    let mut outputs = daemon.exec(ExecOpen::Start(spec), Box::pin(tokio_stream::pending())).await.unwrap();
    while !matches!(outputs.next().await.unwrap().unwrap(), ExecOutput::Started(_)) {}
    streams.push(outputs);
  }

  tokio::time::timeout(Duration::from_secs(20), daemon.shutdown()).await.expect("shutdown waits for every task");

  // A run's stream and one outside any run say the same.
  for outputs in streams {
    let rest: Vec<_> = outputs.collect().await;
    assert!(matches!(rest.as_slice(), [Ok(ExecOutput::Ended(reason))] if reason == "daemon shutting down"), "{rest:?}");
  }
}

// A socket path longer than `sun_path` used to make the control daemon wait
// 10 s for its local executor before it failed without naming the path.
// This keeps start-up failing at once with the path in the error.
#[tokio::test]
async fn a_runtime_directory_too_long_for_a_socket_fails_start_up_at_once() {
  let dir = tempfile::tempdir().unwrap();
  let runtime_dir = dir.path().join("r".repeat(120));
  let config = crate::Config {
    state_dir: dir.path().join("state"),
    runtime_dir: runtime_dir.clone(),
    actor_image: dir.path().join("image"),
    vm_images: dir.path().join("images"),
    kvm: dir.path().join("kvm"),
    guest_needy: None,
    guest_disks: dir.path().join("guest"),
    executor_image: "needy-executor:latest".into(),
    heartbeat_interval: Duration::from_secs(15),
    local_runner: None,
    report_wait: crate::REPORT_WAIT,
    mirror_out_wait: crate::MIRROR_OUT_WAIT,
  };

  let started = tokio::time::timeout(Duration::from_secs(2), crate::Daemon::start(config)).await.expect("start-up fails at once");

  let Err(error) = started else {
    panic!("a daemon started on a socket path over sun_path");
  };
  let socket = runtime_dir.join("local.sock");
  assert!(format!("{error:#}").contains(&socket.display().to_string()), "{error:#}");
}

// ROOT CAUSE:
//
// If the daemon shut down while a run was being created, the create
// was not unwound, because shutdown only stopped runs it could move to
// `stopping` and nothing told the create to stop.
//
// Before the fix, the create went on after shutdown returned and ended
// `running`, holding its executors (for Docker, a container) with no
// daemon left to release them.
// The fix runs the create under its run's token, which shutdown
// cancels and waits for: the create unwinds through its rollback and
// records `run.fail`.
//
// NOTICE: the create is held in its copy-in by a runner that is connected
// but never answers. When the stall ends the copy-in fails, so the reason
// recorded is the runner's, not "the daemon shut down" that the next step's
// check would give; an earlier version held the create in a `remote`
// executor that came up late, which runners replaced.
#[tokio::test]
async fn shutdown_unwinds_a_run_being_created() {
  use needy_api_server::control::{Joining, NewJoinToken, RunnerReport, Runners};
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  // Runner `b` joins and opens a control stream, but answers no task.
  let token = Runners::create_join_token(
    daemon,
    NewJoinToken {
      ttl_ms: 60_000,
      uses: 1,
      ..Default::default()
    },
  )
  .await
  .unwrap()
  .token;
  let joining = Joining {
    join_token: token,
    name: "b".into(),
    report: RunnerReport {
      capabilities: crate::testing::capabilities("executor(native)"),
      ..Default::default()
    },
  };
  Runners::join(daemon, joining).await.unwrap();
  let (results, from_runner) = tokio::sync::mpsc::channel(1);
  let mut tasks = Runners::control(daemon, "b", Box::pin(tokio_stream::wrappers::ReceiverStream::new(from_runner))).await.unwrap();
  let seed = started.dir.path().join("seed");
  std::fs::create_dir_all(&seed).unwrap();
  let profile = format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
actor: {{ driver: linux-firecracker }}
executors:
  a: {{ driver: native }}
  b: {{ runner: b }}
commands:
  - {{ match: "**", to: a }}
files:
  seed: {{ mode: copy-in, from: "host:{}", to: "executor:b:/tmp/needy-seed" }}
"#,
    seed.display()
  );
  let creating = tokio::spawn({
    let daemon = daemon.clone();
    let request = new_run(&profile, started.dir.path());
    async move { Runs::create(&daemon, request).await }
  });
  // The copy-in to runner b is under way once its exec task is announced.
  let announced = tokio::time::timeout(Duration::from_secs(20), tokio_stream::StreamExt::next(&mut tasks)).await;
  assert!(matches!(announced, Ok(Some(_))), "the copy-in's task was not announced");

  let shutdown = tokio::spawn({
    let daemon = daemon.clone();
    async move { daemon.shutdown().await }
  });
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert!(!shutdown.is_finished(), "shutdown returned while a create was in progress");
  // The runner's connection ends; its copy-in fails.
  drop(results);
  let created = tokio::time::timeout(Duration::from_secs(20), creating).await.expect("the create ends").unwrap();
  tokio::time::timeout(Duration::from_secs(20), shutdown).await.expect("shutdown ends").unwrap();

  let Err(ControlError::Failed(message)) = created else {
    panic!("a create the daemon's shutdown interrupted must fail, not {created:?}");
  };
  assert!(message.contains("copy-in seed"), "{message}");
  assert!(daemon.pool.instances().await.is_empty(), "the unwound create still holds an executor");
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].state, RunState::Failed);
  let trace = needy_trace::sink::sqlite::load(&started.dir.path().join("state/trace.db"), &listed[0].id).await.unwrap();
  let last = trace.events.last().unwrap();
  assert_eq!(last.kind, "run.fail");
  assert!(last.payload.get().contains("copy-in seed"), "{}", last.payload.get());
}

// ROOT CAUSE:
//
// If the control daemon was stopped by a termination signal during a run
// on a runner, the run's processes kept running on the runner until it
// connected to the next control daemon, because serving ended the runners'
// control streams as soon as the signal came, before the shutdown stopped
// the runs and told the runners to end them.
//
// Before the fix, the sleep below outlived the daemon's shutdown.
// The fix stops the runs first, then ends the control streams.
#[tokio::test]
async fn a_daemon_stopped_by_a_signal_ends_its_runs_processes_on_runners_before_it_drops_them() {
  const TOKEN: &str = "control-secret";
  let started = testing::daemon().await;
  let daemon = started.daemon.clone();
  let socket = started.dir.path().join("ctl.sock");
  let endpoint = format!("unix:{}", socket.display());
  let stop = tokio_util::sync::CancellationToken::new();
  let serving = tokio::spawn({
    let (daemon, endpoint, stop) = (daemon.clone(), endpoint.clone(), stop.clone());
    async move {
      daemon.serve_until(std::slice::from_ref(&endpoint), Some(TOKEN.into()), stop).await.unwrap();
      daemon.shutdown().await;
    }
  });
  while std::os::unix::net::UnixStream::connect(&socket).is_err() {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let operator = needy_api_client::Client::connect(&endpoint).await.unwrap().with_token(TOKEN).unwrap();
  let request = needy_api_client::proto::v1::CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 1,
    ..Default::default()
  };
  let token = operator.runners().create_join_token(request).await.unwrap().into_inner().token;
  let runner_state = tempfile::tempdir().unwrap();
  let config = crate::runner::Config {
    state_dir: runner_state.path().to_path_buf(),
    control: Some(endpoint.clone()),
    join_token: Some(token),
    name: "far".into(),
    capabilities: crate::testing::capabilities("executor(native)"),
    labels: Default::default(),
    grace: crate::runner::DISCONNECT_GRACE,
  };
  let runner = crate::runner::Runner::start(config).await.unwrap();
  let runner_stop = tokio_util::sync::CancellationToken::new();
  let _runner_stop = runner_stop.clone().drop_guard();
  tokio::spawn(runner.serve(runner_stop));
  while daemon.links.get("far").is_none() {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let profile = "actor: { driver: linux-firecracker }\nexecutors:\n  far: { runner: far }\ncommands:\n  - { match: \"**\", to: far }\n";
  let created = Runs::create(&daemon, new_run(profile, started.dir.path())).await.unwrap();
  let (_, pid) = testing::detached_sleep(&daemon, &created.id).await;

  stop.cancel();
  tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the daemon shuts down").unwrap();

  assert!(testing::ended(pid).await, "process {pid} outlived the daemon's shutdown");
}

// ROOT CAUSE:
//
// If a termination signal stopped the control daemon while a command
// outside any run was attached, its client could see the stream cut without
// "daemon shutting down", because only the runs' streams ended while the
// server still served; the others waited out the server's grace period and
// were ended by the shutdown after serving, racing the process's exit.
//
// Before the fix, the stream below stayed open after serving stopped.
// The fix ends every open exec stream, with its reason, while the server
// still serves and drains it.
#[tokio::test]
async fn a_stream_outside_a_run_hears_the_daemon_is_shutting_down_while_it_serves() {
  const TOKEN: &str = "control-secret";
  let started = testing::daemon().await;
  let daemon = started.daemon.clone();
  let socket = started.dir.path().join("ctl.sock");
  let endpoint = format!("unix:{}", socket.display());
  let stop = tokio_util::sync::CancellationToken::new();
  // Serving only: the shutdown after it is what the stream must not need.
  let serving = tokio::spawn({
    let (daemon, endpoint, stop) = (daemon.clone(), endpoint.clone(), stop.clone());
    async move { daemon.serve_until(std::slice::from_ref(&endpoint), Some(TOKEN.into()), stop).await.unwrap() }
  });
  while std::os::unix::net::UnixStream::connect(&socket).is_err() {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let client = needy_api_client::Client::connect(&endpoint).await.unwrap().with_token(TOKEN).unwrap();
  let start = needy_api_client::proto::v1::ExecStart {
    argv: vec!["sleep".into(), "300".into()],
    no_stdin: true,
    origin: needy_api_client::proto::v1::Origin::Cli.into(),
    ..Default::default()
  };
  let mut stream = needy_api_client::exec::ExecStream::start(&client, start).await.unwrap();
  while !matches!(stream.next().await.unwrap(), Some(needy_api_client::exec::Event::Started(_))) {}

  stop.cancel();
  let ended = tokio::time::timeout(Duration::from_secs(20), stream.next()).await.expect("the stream ends while the daemon serves");
  tokio::time::timeout(Duration::from_secs(20), serving).await.expect("serving ends").unwrap();
  daemon.shutdown().await;

  match ended {
    Err(needy_api_client::Error::Ended { reason }) => assert_eq!(reason.as_deref(), Some("daemon shutting down")),
    other => panic!("{other:?}"),
  }
}

// Controlled shutdown leaves the API available for guest cleanup, but must
// refuse new work even while an old daemon clone still exists.
#[tokio::test]
async fn shutdown_rejects_new_runs_and_process_starts() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  daemon.shutdown().await;
  let error = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap_err();
  assert!(matches!(error, ControlError::Diagnostic { .. }), "{error:?}");
  assert!(error.to_string().contains("shutting down"));
  let spec = ExecSpec {
    argv: vec!["true".into()],
    cwd: String::new(),
    env: HashMap::new(),
    clear_env: false,
    tty: None,
    detach: false,
    run: String::new(),
    executor: String::new(),
    runner: String::new(),
    no_stdin: true,
    origin: Origin::Cli,
    search_path_if_missing: false,
    by: None,
  };
  let result = Processes::exec(daemon, ExecOpen::Start(spec), Box::pin(tokio_stream::empty())).await;
  let error = result.err().expect("new executor work must be refused during shutdown");
  assert!(matches!(error, ControlError::Diagnostic { .. }), "{error:?}");
  assert!(error.to_string().contains("shutting down"));
}
