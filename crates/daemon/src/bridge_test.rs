//! Unit and regression tests for remote actor runners (`StartActor` over
//! `RunnerService.Control`), Node Authorizer enforcement, outbound bridge
//! exclusivity, console log delivery, abrupt disconnect cleanup, shared TCP
//! gateway authentication, `--ephemeral` runner exit, and per-run
//! `last_activity_ms` tracking.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_client::Client;
use needy_api_client::proto::v1::{
  BridgeAttach, BridgeExit, BridgeRequest, BridgeStarted, BridgeTerminalReady, ControlRequest, CreateRunRequest, GetConfigRequest,
  GetRunRequest, HeartbeatRequest, RunnerReport, bridge_request, bridge_response, bridge_terminal_task, control_request, control_response,
  runner_task,
};
use needy_api_server::Listen;
use needy_api_server::control::{ControlError, ExecOpen, ExecSpec, Guest, MintRunnerCredential, Processes, Runners, Runs, WindowSize};
use needy_api_server::protocol::domain::proto_capabilities;
use needy_core::intercept::exec::ExecContext;
use needy_core::types::{ActorState, Capabilities, Origin, RunState, RunnerRole};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::testing::{self, new_run};

const REMOTE_PROFILE: &str = "\
actor:
  driver: linux-firecracker
  runner: actor-node
  command: [sh, -c, 'echo hi']
  network:
    allow: [example.com]
executors:
  here:
    driver: native
commands:
  - match: echo **
    to: here
";

pub(crate) struct ConnectedActorRunner {
  pub(crate) credential: String,
  _ctrl_tx: mpsc::Sender<ControlRequest>,
  pub(crate) ctrl_stream: tonic::Streaming<needy_api_client::proto::v1::ControlResponse>,
}

/// Mints a short-lived actor runner credential, sends a heartbeat with
/// `actor(linux-firecracker)`, and opens its `RunnerService.Control` stream
/// on `addr`.
pub(crate) async fn connect_actor_runner(daemon: &crate::Daemon, addr: &str, name: &str) -> ConnectedActorRunner {
  let minted = Runners::mint_credential(
    daemon,
    MintRunnerCredential {
      name: name.into(),
      ttl_ms: 60_000,
      roles: [RunnerRole::Actor].into(),
      labels: Default::default(),
    },
  )
  .await
  .unwrap();

  let client = Client::connect(&format!("http://{addr}")).await.unwrap().with_token(&minted.credential).unwrap();
  let caps = Capabilities {
    actor: [needy_config::profile::ActorDriver::LinuxFirecracker].into(),
    executor: Default::default(),
  };
  client
    .runners()
    .heartbeat(HeartbeatRequest {
      capabilities: Some(proto_capabilities(&caps)),
      ..Default::default()
    })
    .await
    .unwrap();

  let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlRequest>(16);
  ctrl_tx
    .send(ControlRequest {
      payload: Some(control_request::Payload::Report(RunnerReport::default())),
    })
    .await
    .unwrap();
  let ctrl_stream = client.runners().control(ReceiverStream::new(ctrl_rx)).await.unwrap().into_inner();
  while daemon.links.get(name).is_none() {
    tokio::time::sleep(Duration::from_millis(10)).await;
  }

  ConnectedActorRunner {
    credential: minted.credential,
    _ctrl_tx: ctrl_tx,
    ctrl_stream,
  }
}

#[tokio::test]
async fn remote_actor_runner_skips_local_kvm_dispatches_start_actor_and_enforces_node_authorizer() {
  let started = testing::daemon_with(|config| {
    // Control daemon has no /dev/kvm, no base image, and no guest needy.
    config.kvm = config.state_dir.join("nonexistent-kvm");
    config.actor_image = config.state_dir.join("nonexistent-image");
    config.guest_needy = Some(config.state_dir.join("nonexistent-guest-needy"));
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));

  let mut actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;
  let other_runner = connect_actor_runner(&started.daemon, &addr, "other-node").await;

  let created =
    started.daemon.create(new_run(REMOTE_PROFILE, started.dir.path())).await.expect("remote actor create succeeds without local KVM");
  assert_eq!(created.state, RunState::Running);
  assert_eq!(created.actor, Some(ActorState::Starting));

  // Control plane must dispatch `StartActor { run_id, driver }` on the bound runner's control stream.
  let task_msg = tokio::time::timeout(Duration::from_secs(3), actor_runner.ctrl_stream.message())
    .await
    .expect("timed out waiting for StartActor")
    .unwrap()
    .expect("control stream ended before StartActor");
  match task_msg.payload {
    Some(control_response::Payload::Task(task)) => match task.kind {
      Some(runner_task::Kind::StartActor(start)) => {
        assert_eq!(start.run_id, created.id);
        assert_eq!(start.driver, "linux-firecracker");
      }
      other => panic!("expected StartActor task, got {other:?}"),
    },
    other => panic!("expected Task payload, got {other:?}"),
  }

  // Bound actor runner with x-needy-run-id reaches GuestService.GetConfig.
  let bound_client = Client::connect(&format!("http://{addr}"))
    .await
    .unwrap()
    .with_token(&actor_runner.credential)
    .unwrap()
    .with_run_id(&created.id)
    .unwrap();
  let cfg = bound_client.guest().get_config(GetConfigRequest {}).await.unwrap().into_inner();
  assert_eq!(cfg.run_id, created.id);

  // Bound actor runner is still denied non-actor methods (e.g. GetRun).
  let denied = bound_client
    .runs()
    .get_run(GetRunRequest {
      run_id: created.id.clone(),
    })
    .await
    .unwrap_err();
  assert_eq!(denied.code(), tonic::Code::PermissionDenied);

  // Cross-node runner with the same run ID is denied.
  let other_client = Client::connect(&format!("http://{addr}"))
    .await
    .unwrap()
    .with_token(&other_runner.credential)
    .unwrap()
    .with_run_id(&created.id)
    .unwrap();
  assert_eq!(other_client.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), tonic::Code::PermissionDenied);

  // Plain runner without x-needy-run-id cannot access ACTOR_METHODS.
  let plain_client = Client::connect(&format!("http://{addr}")).await.unwrap().with_token(&actor_runner.credential).unwrap();
  assert_eq!(plain_client.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), tonic::Code::Unauthenticated);

  // Stopping the run immediately revokes Node Authorizer access.
  started.daemon.stop(&created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  assert_eq!(bound_client.guest().get_config(GetConfigRequest {}).await.unwrap_err().code(), tonic::Code::PermissionDenied);

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn bridge_enforces_single_exclusivity_streams_console_and_handles_abrupt_drop() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started.daemon.create(new_run(REMOTE_PROFILE, started.dir.path())).await.unwrap();

  let (tx1, rx1) = mpsc::channel(16);
  tx1
    .send(Ok(BridgeRequest {
      payload: Some(bridge_request::Payload::Attach(BridgeAttach {
        pid: 4242,
        vcpus: 2,
        mem_mib: 512,
        driver: "linux-firecracker".into(),
        capture: "kernel".into(),
      })),
    }))
    .await
    .unwrap();
  let mut out1 = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx1))).await.unwrap();
  let first = out1.next().await.expect("BridgeAttached");
  assert!(matches!(first.payload, Some(bridge_response::Payload::Attached(ref a)) if a.run_id == created.id));

  // Second concurrent bridge on the same run must be rejected.
  let (tx2, rx2) = mpsc::channel(4);
  tx2
    .send(Ok(BridgeRequest {
      payload: Some(bridge_request::Payload::Attach(BridgeAttach {
        pid: 9999,
        vcpus: 1,
        mem_mib: 256,
        driver: "linux-firecracker".into(),
        capture: "kernel".into(),
      })),
    }))
    .await
    .unwrap();
  let second = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx2))).await;
  assert!(matches!(second, Err(ControlError::Failed(msg)) if msg.contains("already has an actor bridge")));

  // Stream console bytes and verify `Runs::logs` reads them back.
  tx1
    .send(Ok(BridgeRequest {
      payload: Some(bridge_request::Payload::Console(b"booting remote actor\n".to_vec())),
    }))
    .await
    .unwrap();
  tx1
    .send(Ok(BridgeRequest {
      payload: Some(bridge_request::Payload::Console(b"ready\n".to_vec())),
    }))
    .await
    .unwrap();

  // Wait briefly for the bridge reader task to flush console.log.
  let mut logs_text = String::new();
  for _ in 0..40 {
    if let Ok(mut stream) = started.daemon.logs(&created.id, false).await {
      let mut all = Vec::new();
      while let Some(chunk) = stream.next().await {
        all.extend(chunk.unwrap());
      }
      logs_text = String::from_utf8_lossy(&all).into_owned();
      if logs_text.contains("ready\n") {
        break;
      }
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  assert_eq!(logs_text, "booting remote actor\nready\n");

  // Abruptly drop the bridge stream without sending BridgeExit -> RunState::Failed + pool release + Node Authorizer revoked.
  drop(tx1);
  drop(out1);

  let mut final_info = started.daemon.get(&created.id).await.unwrap();
  for _ in 0..80 {
    final_info = started.daemon.get(&created.id).await.unwrap();
    if final_info.state == RunState::Failed {
      break;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  assert_eq!(final_info.state, RunState::Failed);
  assert!(final_info.executors.is_empty(), "executors must be released on abrupt bridge drop");
  assert_eq!(final_info.released.len(), 1);
  assert!(!started.daemon.authorize_actor_runner("actor-node", &created.id).await.unwrap());

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn last_activity_ms_advances_on_guest_rpc_decide_exec_and_tcp_gateway() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;
  let other_runner = connect_actor_runner(&started.daemon, &addr, "other-node").await;

  let created = started.daemon.create(new_run(REMOTE_PROFILE, started.dir.path())).await.unwrap();
  let cred = &actor_runner.credential;
  let t0 = created.last_activity_ms;
  assert_eq!(t0, created.created_ms);

  // 1. Guest RPC advances last_activity_ms.
  tokio::time::sleep(Duration::from_millis(15)).await;
  started.daemon.config(&created.id).await.unwrap();
  let t1 = started.daemon.get(&created.id).await.unwrap().last_activity_ms;
  assert!(t1 > t0, "expected guest config RPC to advance last_activity_ms ({t1} > {t0})");

  // 2. RunService/Decide advances last_activity_ms.
  tokio::time::sleep(Duration::from_millis(15)).await;
  let ctx = ExecContext {
    argv: vec!["echo".into(), "hi".into()],
    cwd: "/tmp".into(),
    env: Default::default(),
    tty: false,
    ancestors: Vec::new(),
  };
  started.daemon.decide(&created.id, ctx).await.unwrap();
  let t2 = started.daemon.get(&created.id).await.unwrap().last_activity_ms;
  assert!(t2 > t1, "expected decide RPC to advance last_activity_ms ({t2} > {t1})");

  // 3. Shared TCP Gateway authenticates via Node Authorizer (rejecting wrong runner with 407) and advances last_activity_ms.
  let bound_gw = started.daemon.bind_gateway_tcp("127.0.0.1:0").await.unwrap();
  let gw_addr = bound_gw.local_addr().unwrap();
  let gw_stop = CancellationToken::new();
  tokio::spawn(bound_gw.serve(gw_stop.clone()));

  // Non-bound runner via HTTP Proxy-Authorization + X-Needy-Run-Id -> 407 Proxy Authentication Required.
  let mut bad = tokio::net::TcpStream::connect(gw_addr).await.unwrap();
  bad
    .write_all(
      format!(
        "GET http://blocked.invalid/ HTTP/1.1\r\nHost: blocked.invalid\r\nProxy-Authorization: Bearer {}\r\nX-Needy-Run-Id: {}\r\nConnection: close\r\n\r\n",
        other_runner.credential, created.id
      )
      .as_bytes(),
    )
    .await
    .unwrap();
  let mut resp = String::new();
  bad.read_to_string(&mut resp).await.unwrap();
  assert!(resp.starts_with("HTTP/1.1 407 "), "expected 407 for wrong runner, got: {resp}");

  // Bound runner via HTTP Proxy-Authorization + X-Needy-Run-Id -> reaches run's Gateway (403 denied by allowlist) and advances last_activity_ms.
  tokio::time::sleep(Duration::from_millis(15)).await;
  let mut good_http = tokio::net::TcpStream::connect(gw_addr).await.unwrap();
  good_http
    .write_all(
      format!(
        "GET http://blocked.invalid/ HTTP/1.1\r\nHost: blocked.invalid\r\nProxy-Authorization: Bearer {cred}\r\nX-Needy-Run-Id: {}\r\nConnection: close\r\n\r\n",
        created.id
      )
      .as_bytes(),
    )
    .await
    .unwrap();
  let mut resp = String::new();
  good_http.read_to_string(&mut resp).await.unwrap();
  assert!(resp.starts_with("HTTP/1.1 403 "), "expected 403 from run gateway allowlist, got: {resp}");
  let t3 = started.daemon.get(&created.id).await.unwrap().last_activity_ms;
  assert!(t3 > t2, "expected TCP gateway HTTP request to advance last_activity_ms ({t3} > {t2})");

  // Bound runner via NEEDY-GATEWAY <cred> <run_id> preamble -> reaches run's Gateway without Proxy-Authorization header.
  tokio::time::sleep(Duration::from_millis(15)).await;
  let mut good_preamble = tokio::net::TcpStream::connect(gw_addr).await.unwrap();
  good_preamble
    .write_all(
      format!(
        "{}{cred} {}\nGET http://blocked.invalid/ HTTP/1.1\r\nHost: blocked.invalid\r\nConnection: close\r\n\r\n",
        crate::gateway_tcp::PREAMBLE_PREFIX,
        created.id
      )
      .as_bytes(),
    )
    .await
    .unwrap();
  let mut resp = String::new();
  good_preamble.read_to_string(&mut resp).await.unwrap();
  assert!(resp.starts_with("HTTP/1.1 403 "), "expected 403 via bridge preamble, got: {resp}");
  let t4 = started.daemon.get(&created.id).await.unwrap().last_activity_ms;
  assert!(t4 > t3, "expected TCP gateway preamble request to advance last_activity_ms ({t4} > {t3})");

  // 4. ProcessService/Exec advances last_activity_ms.
  tokio::time::sleep(Duration::from_millis(15)).await;
  let open = ExecOpen::Start(ExecSpec {
    argv: vec!["echo".into(), "activity-check".into()],
    cwd: "/tmp".into(),
    env: Default::default(),
    clear_env: false,
    tty: None,
    detach: false,
    run: created.id.clone(),
    executor: String::new(),
    runner: String::new(),
    no_stdin: true,
    origin: Origin::ActorProxy,
    search_path_if_missing: false,
    by: None,
  });
  let mut exec_out = started.daemon.exec(open, Box::pin(tokio_stream::empty())).await.unwrap();
  while exec_out.next().await.is_some() {}
  let t5 = started.daemon.get(&created.id).await.unwrap().last_activity_ms;
  assert!(t5 > t4, "expected exec to advance last_activity_ms ({t5} > {t4})");

  gw_stop.cancel();
  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn bridge_clean_exit_after_actor_exit_finishes_run() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started.daemon.create(new_run(REMOTE_PROFILE, started.dir.path())).await.unwrap();

  let (tx, rx) = mpsc::channel(16);
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Attach(BridgeAttach {
      pid: 1001,
      vcpus: 1,
      mem_mib: 256,
      driver: "linux-firecracker".into(),
      capture: "kernel".into(),
    })),
  }))
  .await
  .unwrap();
  let mut out = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx))).await.unwrap();
  let _ = out.next().await.unwrap();

  // Guest reports driving program exit 0, then bridge reports VM exit 0.
  started
    .daemon
    .exit(
      &created.id,
      needy_common::process::Exit {
        code: Some(0),
        signal: String::new(),
      },
    )
    .await
    .unwrap();
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Exit(BridgeExit {
      code: Some(0),
      signal: String::new(),
      error: String::new(),
    })),
  }))
  .await
  .unwrap();
  drop(tx);

  let mut info = started.daemon.get(&created.id).await.unwrap();
  for _ in 0..80 {
    info = started.daemon.get(&created.id).await.unwrap();
    if info.state == RunState::Finished {
      break;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  assert_eq!(info.state, RunState::Finished);
  assert_eq!(info.actor, Some(ActorState::Exited));

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

// ROOT CAUSE:
//
// If an attach to a remote actor's driving program started before the actor
// runner reported `BridgeTerminalReady` (always so for `needy run create
// -it`), the attach timed out after a minute because the attach initialized
// `run.terminal` while waiting for the bridge, and the bridge's `set` on a
// `OnceCell` being initialized failed and was dropped.
//
// Before the fix, the attach never reached the actor's terminal.
// The fix keeps the bridge's terminal apart from `run.terminal`, so an attach
// waiting for it gets it as soon as the runner reports it.
#[tokio::test]
async fn an_attach_waiting_before_terminal_ready_reaches_the_remote_terminal() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started
    .daemon
    .create(needy_api_server::control::NewRun {
      tty: Some(WindowSize { rows: 24, cols: 80 }),
      ..new_run(REMOTE_PROFILE, started.dir.path())
    })
    .await
    .unwrap();

  let (tx, rx) = mpsc::channel(16);
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Attach(BridgeAttach {
      pid: 1001,
      vcpus: 1,
      mem_mib: 256,
      driver: "linux-firecracker".into(),
      capture: "kernel".into(),
    })),
  }))
  .await
  .unwrap();
  let mut out = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx))).await.unwrap();
  let _ = out.next().await.unwrap();

  // The attach starts before the guest's terminal answers, as `run create
  // -it` does while the VM boots.
  let daemon = started.daemon.clone();
  let run = created.id.clone();
  let attach = tokio::spawn(async move {
    daemon
      .exec(
        ExecOpen::Attach {
          process: String::new(),
          run,
          replay: false,
          window: None,
          read_only: true,
        },
        Box::pin(tokio_stream::empty()),
      )
      .await
  });
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert!(!attach.is_finished(), "the attach waits for the actor's terminal");

  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::TerminalReady(BridgeTerminalReady {})),
  }))
  .await
  .unwrap();

  // The attach looks up the driving program on the actor's terminal, which
  // reaches the runner as a terminal task on the bridge.
  let task = tokio::time::timeout(Duration::from_secs(3), out.next())
    .await
    .expect("the waiting attach reaches the actor's terminal after BridgeTerminalReady")
    .expect("bridge stream ended");
  let Some(bridge_response::Payload::TerminalTask(task)) = task.payload else {
    panic!("expected a terminal task, got {:?}", task.payload);
  };
  assert!(matches!(task.kind, Some(bridge_terminal_task::Kind::ListProcesses(_))), "{:?}", task.kind);

  attach.abort();
  drop(tx);
  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

// ROOT CAUSE:
//
// If a remote actor's runner took longer than a minute to start the VM
// (fetching image layers of a few hundred megabytes), `needy run create -it`
// failed with "timed out waiting for the terminal" because the attach's
// terminal wait counted from the attach, which starts before the runner has
// fetched anything.
//
// Before the fix, the attach gave up while the runner was still preparing.
// The fix counts the terminal wait from the runner's `BridgeStarted`, as a
// local actor's counts from its boot; before that, the run's own lifecycle
// bounds the wait.
#[tokio::test]
async fn an_attach_waits_for_a_remote_actor_that_is_still_preparing() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started
    .daemon
    .create(needy_api_server::control::NewRun {
      tty: Some(WindowSize { rows: 24, cols: 80 }),
      ..new_run(REMOTE_PROFILE, started.dir.path())
    })
    .await
    .unwrap();

  // A runner attaches before it prepares the VM, without a PID, as
  // `actor_runner::run_actor` does.
  let (tx, rx) = mpsc::channel(16);
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Attach(BridgeAttach {
      pid: 0,
      vcpus: 0,
      mem_mib: 0,
      driver: String::new(),
      capture: String::new(),
    })),
  }))
  .await
  .unwrap();
  let mut out = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx))).await.unwrap();
  let _ = out.next().await.unwrap();

  let daemon = started.daemon.clone();
  let run = created.id.clone();
  let attach = tokio::spawn(async move {
    daemon
      .exec(
        ExecOpen::Attach {
          process: String::new(),
          run,
          replay: false,
          window: None,
          read_only: true,
        },
        Box::pin(tokio_stream::empty()),
      )
      .await
  });
  tokio::time::sleep(Duration::from_millis(200)).await;

  // Longer than a minute passes while the runner fetches layers.
  tokio::time::pause();
  tokio::time::advance(Duration::from_secs(90)).await;
  tokio::time::resume();
  tokio::time::sleep(Duration::from_millis(200)).await;
  if attach.is_finished() {
    let result = attach.await.unwrap().map(|_| ());
    panic!("the attach gave up while the remote actor was still preparing: {result:?}");
  }

  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Started(BridgeStarted {
      pid: 1001,
      vcpus: 1,
      mem_mib: 256,
      driver: "linux-firecracker".into(),
      capture: "kernel".into(),
    })),
  }))
  .await
  .unwrap();
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::TerminalReady(BridgeTerminalReady {})),
  }))
  .await
  .unwrap();

  let task = tokio::time::timeout(Duration::from_secs(3), out.next())
    .await
    .expect("the attach reaches the actor's terminal once the runner started it")
    .expect("bridge stream ended");
  let Some(bridge_response::Payload::TerminalTask(task)) = task.payload else {
    panic!("expected a terminal task, got {:?}", task.payload);
  };
  assert!(matches!(task.kind, Some(bridge_terminal_task::Kind::ListProcesses(_))), "{:?}", task.kind);

  attach.abort();
  drop(tx);
  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn create_run_over_grpc_returns_last_activity_ms() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let client = Client::connect(&format!("http://{addr}")).await.unwrap();
  let resp = client
    .runs()
    .create_run(CreateRunRequest {
      profile_yaml: REMOTE_PROFILE.into(),
      profile_name: "remote".into(),
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  let run = resp.run.expect("run in CreateRunResponse");
  assert_eq!(run.last_activity_ms, run.created_at_ms);

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn wait_for_runner_ms_waits_for_actor_and_executor_runners_to_connect() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let profile = "\
actor:
  driver: linux-firecracker
  runner: delayed-actor
  command: [sh, -c, 'echo hi']
executors:
  remote:
    driver: native
    runner: delayed-exec
commands:
  - match: echo **
    to: remote
";

  let daemon = Arc::new(started.daemon.clone());
  let bound = daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));

  // Mint credentials for both delayed-actor and delayed-exec, then connect them after a short delay.
  let minted_exec = Runners::mint_credential(
    daemon.as_ref(),
    MintRunnerCredential {
      name: "delayed-exec".into(),
      ttl_ms: 60_000,
      roles: [RunnerRole::Executor].into(),
      labels: Default::default(),
    },
  )
  .await
  .unwrap();

  let runner_stop = stop.clone();
  let runner_dir = started.dir.path().join("delayed-runner");
  std::fs::create_dir_all(&runner_dir).unwrap();
  let exec_addr = addr.clone();
  let runner_task = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(120)).await;
    let cfg = crate::runner::Config {
      state_dir: runner_dir,
      control: Some(format!("http://{exec_addr}")),
      join_token: None,
      credential: Some(minted_exec.credential),
      name: "delayed-exec".into(),
      capabilities: Capabilities {
        actor: Default::default(),
        executor: [needy_config::profile::ExecutorDriver::Native].into(),
      },
      labels: Default::default(),
      grace: Duration::from_secs(5),
      activity_hook: None,
      ephemeral: false,
      actor: None,
    };
    let _ = crate::runner::Runner::start(cfg).await.unwrap().serve(runner_stop).await;
  });

  let actor_daemon = daemon.clone();
  let actor_addr = addr.clone();
  let actor_task = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(150)).await;
    connect_actor_runner(&actor_daemon, &actor_addr, "delayed-actor").await
  });

  let mut req = new_run(profile, started.dir.path());
  req.wait_for_runner_ms = 5_000;
  let created = started.daemon.create(req).await.expect("create should succeed once both runners connect");
  assert_eq!(created.state, RunState::Running);

  let _actor_runner = actor_task.await.unwrap();
  stop.cancel();
  let _ = runner_task.await;
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

// ROOT CAUSE:
//
// When `run stop` cancelled `run.cancel`, `drive_bridge` queued `BridgeStop`
// and broke immediately, calling `link.close()`, which cancelled `link.closed`
// and truncated `ReceiverStream::new(rx).take_until(closed)` before `BridgeStop`
// was yielded to the remote actor host.
//
// Before the fix, the remote bridge saw stream EOF instead of `BridgeStop`.
// The fix awaits `outbound.send(BridgeStop)`, drops the senders cleanly on
// exit without `take_until`, and waits briefly for `BridgeExit`.
#[tokio::test]
async fn run_stop_delivers_bridge_stop_before_closing_stream() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let _actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started.daemon.create(new_run(REMOTE_PROFILE, started.dir.path())).await.unwrap();

  let (tx, rx) = mpsc::channel(16);
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Attach(BridgeAttach {
      pid: 2002,
      vcpus: 1,
      mem_mib: 256,
      driver: "linux-firecracker".into(),
      capture: "kernel".into(),
    })),
  }))
  .await
  .unwrap();
  let mut out = started.daemon.bridge(&created.id, Box::pin(ReceiverStream::new(rx))).await.unwrap();
  let attached = out.next().await.expect("BridgeAttached");
  assert!(matches!(attached.payload, Some(bridge_response::Payload::Attached(_))));

  let stop_daemon = started.daemon.clone();
  let run_id = created.id.clone();
  let stop_task = tokio::spawn(async move {
    stop_daemon.stop(&run_id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
  });

  // Remote bridge must receive `BridgeStop` before the stream closes.
  let msg = tokio::time::timeout(Duration::from_secs(3), out.next())
    .await
    .expect("timed out waiting for BridgeStop")
    .expect("bridge stream closed without sending BridgeStop");
  assert!(matches!(msg.payload, Some(bridge_response::Payload::Stop(stop)) if stop.grace_ms > 0), "expected BridgeStop, got {msg:?}");

  // Acknowledge with BridgeExit so stop finishes promptly.
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Exit(BridgeExit {
      code: Some(0),
      signal: String::new(),
      error: String::new(),
    })),
  }))
  .await
  .unwrap();
  drop(tx);

  tokio::time::timeout(Duration::from_secs(3), stop_task).await.expect("stop completes").unwrap();
  assert!(tokio::time::timeout(Duration::from_secs(2), out.next()).await.expect("stream ends").is_none());

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn gateway_blocked_cidrs_rejects_matching_target_and_finish_cancels_keepalive() {
  let profile = "\
actor:
  driver: linux-firecracker
  runner: actor-node
  command: [sh, -c, 'echo hi']
  network:
    allow: ['198.51.100.10']
executors:
  here:
    driver: native
commands:
  - match: echo **
    to: here
";

  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
    config.gateway_blocked_cidrs = vec!["198.51.100.0/24".into()];
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));
  let actor_runner = connect_actor_runner(&started.daemon, &addr, "actor-node").await;

  let created = started.daemon.create(new_run(profile, started.dir.path())).await.unwrap();

  let bound_gw = started.daemon.bind_gateway_tcp("127.0.0.1:0").await.unwrap();
  let gw_addr = bound_gw.local_addr().unwrap();
  let gw_stop = CancellationToken::new();
  tokio::spawn(bound_gw.serve(gw_stop.clone()));

  // Open a keep-alive bridge-preamble connection; target 198.51.100.10 is allowed
  // by profile allowlist but must be blocked by `gateway_blocked_cidrs`.
  let mut conn = tokio::net::TcpStream::connect(gw_addr).await.unwrap();
  conn
    .write_all(
      format!(
        "{}{} {}\nCONNECT 198.51.100.10:443 HTTP/1.1\r\nHost: 198.51.100.10:443\r\n\r\n",
        crate::gateway_tcp::PREAMBLE_PREFIX,
        actor_runner.credential,
        created.id
      )
      .as_bytes(),
    )
    .await
    .unwrap();

  let mut buf = [0u8; 512];
  let n = tokio::time::timeout(Duration::from_secs(2), conn.read(&mut buf)).await.expect("gateway response").unwrap();
  let resp = String::from_utf8_lossy(&buf[..n]);
  assert!(resp.starts_with("HTTP/1.1 403 "), "expected 403 for blocked CIDR, got: {resp}");
  assert!(resp.contains("blocked CIDR 198.51.100.0/24"), "expected blocked CIDR reason, got: {resp}");

  // Closing the run's gateway (as `finish_run` does) must immediately cut off
  // the established keep-alive connection on the remote gateway.
  let run = started.daemon.runs.exact(&created.id).unwrap();
  run.close_gateway();
  let eof = tokio::time::timeout(Duration::from_secs(2), conn.read(&mut buf))
    .await
    .expect("keep-alive connection must close immediately on gateway cancel");
  assert!(matches!(eof, Ok(0) | Err(_)), "expected closed connection, got {eof:?}");

  gw_stop.cancel();
  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}

#[tokio::test]
async fn ephemeral_runner_exits_cleanly_after_completing_a_single_run() {
  let started = testing::daemon_with(|config| {
    config.kvm = config.state_dir.join("nonexistent-kvm");
  })
  .await;

  let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
  let Listen::Tcp(addr) = &bound.addresses()[0] else {
    unreachable!();
  };
  let addr = addr.clone();
  let stop = CancellationToken::new();
  let server_task = tokio::spawn(bound.serve(stop.clone()));

  // Start an ephemeral actor runner (without a real KVM image, so `StartActor`
  // attaches, fails to boot the nonexistent VM, and `--ephemeral` cleanly exits).
  let minted = Runners::mint_credential(
    &started.daemon,
    MintRunnerCredential {
      name: "ephemeral-actor".into(),
      ttl_ms: 60_000,
      roles: [RunnerRole::Actor].into(),
      labels: Default::default(),
    },
  )
  .await
  .unwrap();

  let runner_dir = started.dir.path().join("ephemeral-runner");
  std::fs::create_dir_all(&runner_dir).unwrap();
  let runner_stop = stop.clone();
  let runner_task = tokio::spawn({
    let addr = addr.clone();
    async move {
      let cfg = crate::runner::Config {
        state_dir: runner_dir.clone(),
        control: Some(format!("http://{addr}")),
        join_token: None,
        credential: Some(minted.credential),
        name: "ephemeral-actor".into(),
        capabilities: Capabilities {
          actor: [needy_config::profile::ActorDriver::LinuxFirecracker].into(),
          executor: Default::default(),
        },
        labels: Default::default(),
        grace: Duration::from_secs(5),
        activity_hook: None,
        ephemeral: true,
        actor: Some(crate::runner::ActorConfig {
          gateway: None,
          actor_image: runner_dir.join("no-image"),
          guest_disks: runner_dir.join("guest-disks"),
          layer_store: crate::runner::layer_store(&runner_dir.join("cache"), &runner_dir.join("data")),
          guest_needy: Some(runner_dir.join("no-guest")),
          kvm: runner_dir.join("no-kvm"),
          work_dir: Some(runner_dir.join("work")),
        }),
      };
      crate::runner::Runner::start(cfg).await.unwrap().serve(runner_stop).await
    }
  });

  // Wait for the ephemeral actor runner to connect and heartbeat its capabilities.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while started.daemon.links.get("ephemeral-actor").is_none() && tokio::time::Instant::now() < deadline {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  let profile = "\
actor:
  driver: linux-firecracker
  runner: ephemeral-actor
  command: [sh, -c, 'echo hi']
";
  let mut req = new_run(profile, started.dir.path());
  req.wait_for_runner_ms = 5_000;
  let _created = started.daemon.create(req).await.unwrap();

  // The ephemeral runner must exit cleanly (`Ok(())`) on its own after handling the single StartActor task.
  let served =
    tokio::time::timeout(Duration::from_secs(10), runner_task).await.expect("ephemeral runner did not exit after single actor run").unwrap();
  assert!(served.is_ok(), "ephemeral runner exited with error: {served:?}");

  stop.cancel();
  let _ = server_task.await;
  started.daemon.shutdown().await;
}
