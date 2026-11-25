//! Actor runner bridge: executes a `StartActor` task dispatched on
//! `RunnerService.Control`.
//!
//! Boots a local `linux-firecracker` microVM for the assigned run, bridges its
//! local vsock channels (`v.sock_7000` guest API, `v.sock_7001` gateway,
//! `v.sock` port `7002` reverse terminal) outbound to the remote `needy-daemon`
//! using the runner's own credential plus `x-needy-run-id: <run_id>` (Node
//! Authorizer), and streams Firecracker's `console.log` back to the control
//! plane.
//!
//! The runner's credential is held exclusively by the host-side runner process
//! and is NEVER injected into the Firecracker guest VM.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use needy_api_client::Client;
use needy_api_client::proto::v1::{
  BridgeAttach, BridgeExit, BridgeRequest, BridgeStarted, BridgeTaskResult, BridgeTerminalReady, BridgeTerminalTask, TerminalTaskRequest,
  bridge_request, bridge_response, bridge_task_result, bridge_terminal_task, terminal_task_request,
};
use needy_api_server::middleware::authentication::RUN_ID_HEADER;
use needy_core_actor::contract::{GATEWAY_PORT, GUEST_PORT, PROCESS_PORT};
use needy_core_actor::driver::{Driver as _, Host, Running, Start, linux_firecracker};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

const VM_STOP_GRACE: Duration = Duration::from_secs(3);
const CONSOLE_POLL: Duration = Duration::from_millis(100);
const TERMINAL_POLL: Duration = Duration::from_millis(200);
const TERMINAL_WAIT: Duration = Duration::from_secs(60);

/// Host-side configuration used by a runner to execute `StartActor` tasks.
#[derive(Debug, Clone)]
pub struct ActorConfig {
  /// Control-plane shared TCP gateway address (`HOST:PORT` or `tcp://HOST:PORT`).
  pub gateway: Option<String>,
  /// Path to the base actor VM image directory.
  pub actor_image: PathBuf,
  /// Where built VM images live (`paths.images()`).
  pub vm_images: PathBuf,
  /// Where guest disks live (`paths.guest_disks()`).
  pub guest_disks: PathBuf,
  /// Path to the guest `needy` binary injected into the VM.
  pub guest_needy: Option<PathBuf>,
  /// Path to the KVM device (defaults to `/dev/kvm`).
  pub kvm: PathBuf,
  /// Working directory for temporary VM files (`vm.json`, `v.sock`, `console.log`); uses a temp dir if omitted.
  pub work_dir: Option<PathBuf>,
}

/// Runs one remote actor VM for `run_id` and bridges all its channels outbound
/// to `control` until the VM exits, the control plane sends `BridgeStop`, the
/// bridge stream disconnects, or `stop` is cancelled.
pub(crate) async fn run_actor(
  control: &str,
  credential: &str,
  run_id: &str,
  driver_name: &str,
  config: &ActorConfig,
  stop: CancellationToken,
) -> Result<()> {
  if !driver_name.is_empty() && driver_name != "linux-firecracker" {
    bail!("remote actor runner supports linux-firecracker, not {driver_name}");
  }

  let control_client = Client::connect_kept_alive(control)
    .await
    .with_context(|| format!("cannot connect to control plane at {control}"))?
    .with_token(credential)
    .context("invalid runner credential format")?
    .with_run_id(run_id)
    .context("invalid run ID header")?;

  // Open the outbound lifecycle/console/terminal bridge stream first so auth
  // rejection or duplicate bridge attachment fails fast before booting a VM.
  let (bridge_tx, bridge_rx) = mpsc::channel::<BridgeRequest>(64);
  bridge_tx
    .send(BridgeRequest {
      payload: Some(bridge_request::Payload::Attach(BridgeAttach {
        pid: 0,
        vcpus: 0,
        mem_mib: 0,
        driver: String::new(),
        capture: String::new(),
      })),
    })
    .await
    .map_err(|_| anyhow::anyhow!("bridge request channel closed"))?;

  let mut bridge_stream =
    control_client.guest().bridge(ReceiverStream::new(bridge_rx)).await.context("control plane rejected actor bridge")?.into_inner();

  let attached = match bridge_stream.message().await.context("actor bridge failed before attach acknowledgement")? {
    Some(msg) => match msg.payload {
      Some(bridge_response::Payload::Attached(attached)) => attached,
      other => bail!("unexpected first message from control plane on actor bridge: {other:?}"),
    },
    None => bail!("control plane closed actor bridge before attach acknowledgement"),
  };

  tracing::info!("actor bridge attached to run {}", attached.run_id);

  let temp_dir = if config.work_dir.is_none() {
    Some(tempfile::tempdir().context("cannot create actor work directory")?)
  } else {
    None
  };
  let work_dir = match (&config.work_dir, &temp_dir) {
    (Some(dir), _) => {
      std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
      dir.clone()
    }
    (None, Some(tmp)) => tmp.path().to_path_buf(),
    (None, None) => unreachable!(),
  };

  let cancel = CancellationToken::new();
  let tasks = tokio_util::task::TaskTracker::new();

  // Bind local vsock UDS endpoints before booting Firecracker.
  // Neither proxy ever exposes the runner credential to the guest VM.
  let guest_uds = linux_firecracker::host_port(&work_dir, GUEST_PORT);
  spawn_guest_grpc_proxy(&guest_uds, control_client.channel(), credential, &attached.run_id, &tasks, &cancel)?;

  if let Some(gateway_addr) = &config.gateway {
    let gateway_uds = linux_firecracker::host_port(&work_dir, GATEWAY_PORT);
    let gateway_target = gateway_addr.strip_prefix("tcp://").unwrap_or(gateway_addr).to_string();
    spawn_gateway_proxy(&gateway_uds, gateway_target, credential.to_string(), attached.run_id.clone(), &tasks, &cancel)?;
  }

  let recipe: Option<needy_core::recipe::CompiledRecipe> = if attached.recipe_json.is_empty() {
    None
  } else {
    Some(serde_json::from_str(&attached.recipe_json).context("cannot decode compiled recipe from control plane")?)
  };

  let host = Host {
    image: config.actor_image.clone(),
    images: config.vm_images.clone(),
    kvm: config.kvm.clone(),
    guest_disks: config.guest_disks.clone(),
  };
  let driver = linux_firecracker::LinuxFirecracker::on(&host, None);
  let exe = std::env::current_exe().context("cannot locate current executable")?;
  let guest_needy = needy_core_actor::image::guest::find(config.guest_needy.as_deref(), &exe, needy_core_actor::image::guest::arch())
    .context("cannot locate guest needy binary")?;

  let profile = needy_config::profile::load(&needy_config::Source::text(attached.profile_yaml.clone(), PathBuf::new()))
    .context("cannot decode profile from control plane")?;

  let mut vm: Box<dyn Running> = driver
    .start(&Start {
      run: &attached.run_id,
      dir: &work_dir,
      user: &profile.user,
      recipe: recipe.as_ref(),
      mounts: &[],
      guest: &guest_needy,
      layer_disks: &[],
      snapshot_image: None,
    })
    .await
    .context("cannot start local Firecracker microVM")?;

  bridge_tx
    .send(BridgeRequest {
      payload: Some(bridge_request::Payload::Started(BridgeStarted {
        pid: vm.pid(),
        vcpus: vm.vcpus(),
        mem_mib: vm.mem_mib(),
        driver: driver.label(),
        capture: "kernel".into(),
      })),
    })
    .await
    .context("control plane bridge closed right after VM start")?;

  // Stream Firecracker's console.log back to the control plane.
  let console_path = linux_firecracker::console(&work_dir);
  let console_cancel = cancel.child_token();
  let console_handle = tokio::spawn(tail_console(console_path.clone(), bridge_tx.clone(), console_cancel.clone()));

  // If the run has a terminal, probe vsock port 7002 and notify the daemon.
  let vsock_path = linux_firecracker::vsock(&work_dir);
  if attached.tty {
    let tx = bridge_tx.clone();
    let vsock = vsock_path.clone();
    let probe_stop = cancel.clone();
    tasks.spawn(async move {
      if wait_for_terminal(&vsock, &probe_stop).await {
        let _ = tx
          .send(BridgeRequest {
            payload: Some(bridge_request::Payload::TerminalReady(BridgeTerminalReady {})),
          })
          .await;
      }
    });
  }

  let mut bridge_broken = false;

  let vm_exit: Option<BridgeExit> = loop {
    tokio::select! {
      waited = vm.wait() => {
        break Some(exit_from_wait(waited));
      }
      msg = bridge_stream.message() => match msg {
        Ok(Some(resp)) => match resp.payload {
          Some(bridge_response::Payload::Stop(stop_req)) => {
            let grace = Duration::from_millis(stop_req.grace_ms.max(0) as u64).max(Duration::from_millis(500));
            let waited = vm.stop(grace).await;
            break Some(exit_from_wait(waited));
          }
          Some(bridge_response::Payload::TerminalTask(task)) => {
            let client = control_client.clone();
            let tx = bridge_tx.clone();
            let vsock = vsock_path.clone();
            let task_stop = cancel.clone();
            let task_tracker = tasks.clone();
            tasks.spawn(async move {
              let _ = task_stop
                .run_until_cancelled(handle_terminal_task(task, client, tx, vsock, task_tracker, task_stop.clone()))
                .await;
            });
          }
          Some(bridge_response::Payload::Attached(_)) | None => {}
        },
        Ok(None) => {
          tracing::warn!("run {}: control plane closed actor bridge unexpectedly; stopping microVM", attached.run_id);
          bridge_broken = true;
          let _ = vm.stop(VM_STOP_GRACE).await;
          break None;
        }
        Err(status) => {
          tracing::warn!("run {}: actor bridge error: {status}; stopping microVM", attached.run_id);
          bridge_broken = true;
          let _ = vm.stop(VM_STOP_GRACE).await;
          break None;
        }
      },
      () = stop.cancelled() => {
        tracing::info!("run {}: runner shutting down; stopping microVM gracefully", attached.run_id);
        let waited = vm.stop(VM_STOP_GRACE).await;
        break Some(exit_from_wait(waited));
      }
    }
  };

  // Stop console tailer and flush any remaining console bytes before sending exit.
  console_cancel.cancel();
  let final_offset = console_handle.await.unwrap_or(0);
  if !bridge_broken {
    flush_console(&console_path, final_offset, &bridge_tx).await;
    if let Some(exit) = vm_exit {
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Exit(exit)),
        })
        .await;
    }
  }

  cancel.cancel();
  drop(bridge_tx);
  tasks.close();
  let _ = tokio::time::timeout(Duration::from_secs(3), tasks.wait()).await;

  if bridge_broken {
    bail!("control plane bridge disconnected");
  }
  Ok(())
}

fn exit_from_wait(waited: std::io::Result<std::process::ExitStatus>) -> BridgeExit {
  use std::os::unix::process::ExitStatusExt as _;
  match waited {
    Ok(status) => BridgeExit {
      code: status.code(),
      signal: status.signal().map(needy_core_executor::signals::name).unwrap_or_default(),
      error: String::new(),
    },
    Err(e) => BridgeExit {
      code: None,
      signal: String::new(),
      error: e.to_string(),
    },
  }
}

/// Proxies guest HTTP/2 gRPC calls from Firecracker's `v.sock_7000` to the
/// remote control plane, injecting `authorization: Bearer <runner_credential>`
/// and `x-needy-run-id: <run_id>`.
fn spawn_guest_grpc_proxy(
  uds_path: &Path,
  channel: tonic::transport::Channel,
  credential: &str,
  run_id: &str,
  tasks: &tokio_util::task::TaskTracker,
  cancel: &CancellationToken,
) -> Result<()> {
  if let Err(e) = std::fs::remove_file(uds_path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", uds_path.display());
  }
  let listener = tokio::net::UnixListener::bind(uds_path).with_context(|| format!("cannot bind {}", uds_path.display()))?;
  let auth_header = http::HeaderValue::from_str(&format!("Bearer {credential}")).context("invalid runner credential header")?;
  let run_id_header = http::HeaderValue::from_str(run_id).context("invalid run ID header")?;
  let stop = cancel.clone();
  let sub_tasks = tasks.clone();
  tasks.spawn(async move {
    loop {
      tokio::select! {
        () = stop.cancelled() => break,
        accepted = listener.accept() => match accepted {
          Ok((stream, _)) => {
            let channel = channel.clone();
            let auth_header = auth_header.clone();
            let run_id_header = run_id_header.clone();
            let conn_stop = stop.clone();
            sub_tasks.spawn(async move {
              let _ = conn_stop.run_until_cancelled(proxy_grpc_connection(stream, channel, auth_header, run_id_header)).await;
            });
          }
          Err(e) => {
            tracing::warn!("guest vsock proxy accept error: {e}");
            break;
          }
        }
      }
    }
  });
  Ok(())
}

async fn proxy_grpc_connection(
  stream: tokio::net::UnixStream,
  channel: tonic::transport::Channel,
  auth_header: http::HeaderValue,
  run_id_header: http::HeaderValue,
) {
  let service = hyper::service::service_fn(move |mut req: http::Request<hyper::body::Incoming>| {
    let mut channel = channel.clone();
    let auth_header = auth_header.clone();
    let run_id_header = run_id_header.clone();
    async move {
      req.headers_mut().insert(http::header::AUTHORIZATION, auth_header);
      req.headers_mut().insert(http::header::HeaderName::from_static(RUN_ID_HEADER), run_id_header);
      let req = req.map(tonic::body::Body::new);
      match tower::ServiceExt::oneshot(&mut channel, req).await {
        Ok(resp) => Ok::<_, std::convert::Infallible>(resp),
        Err(err) => Ok(tonic::Status::unavailable(format!("control plane proxy error: {err}")).into_http()),
      }
    }
  });
  let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
    .await;
}

/// Proxies guest gateway connections from Firecracker's `v.sock_7001` to the
/// daemon's shared TCP gateway with the `NEEDY-GATEWAY <runner_credential> <run_id>\n`
/// preamble.
fn spawn_gateway_proxy(
  uds_path: &Path,
  gateway_target: String,
  credential: String,
  run_id: String,
  tasks: &tokio_util::task::TaskTracker,
  cancel: &CancellationToken,
) -> Result<()> {
  if let Err(e) = std::fs::remove_file(uds_path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", uds_path.display());
  }
  let listener = tokio::net::UnixListener::bind(uds_path).with_context(|| format!("cannot bind {}", uds_path.display()))?;
  let stop = cancel.clone();
  let sub_tasks = tasks.clone();
  tasks.spawn(async move {
    loop {
      tokio::select! {
        () = stop.cancelled() => break,
        accepted = listener.accept() => match accepted {
          Ok((mut unix, _)) => {
            let target = gateway_target.clone();
            let preamble = format!("{}{credential} {run_id}\n", crate::gateway_tcp::PREAMBLE_PREFIX);
            let conn_stop = stop.clone();
            sub_tasks.spawn(async move {
              let _ = conn_stop
                .run_until_cancelled(async move {
                  let Ok(mut tcp) = tokio::net::TcpStream::connect(&target).await else {
                    tracing::debug!("cannot connect to remote gateway at {target}");
                    return;
                  };
                  let _ = tcp.set_nodelay(true);
                  if tcp.write_all(preamble.as_bytes()).await.is_err() {
                    return;
                  }
                  let _ = tokio::io::copy_bidirectional(&mut unix, &mut tcp).await;
                })
                .await;
            });
          }
          Err(e) => {
            tracing::warn!("gateway vsock proxy accept error: {e}");
            break;
          }
        }
      }
    }
  });
  Ok(())
}

async fn tail_console(path: PathBuf, tx: mpsc::Sender<BridgeRequest>, cancel: CancellationToken) -> u64 {
  let mut offset = 0u64;
  loop {
    offset = flush_console(&path, offset, &tx).await;
    tokio::select! {
      () = cancel.cancelled() => return offset,
      () = tokio::time::sleep(CONSOLE_POLL) => {}
    }
  }
}

async fn flush_console(path: &Path, mut offset: u64, tx: &mpsc::Sender<BridgeRequest>) -> u64 {
  let Ok(mut file) = tokio::fs::File::open(path).await else {
    return offset;
  };
  if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
    return offset;
  }
  let mut buf = Vec::new();
  if file.read_to_end(&mut buf).await.is_ok() && !buf.is_empty() {
    offset += buf.len() as u64;
    for chunk in buf.chunks(64 * 1024) {
      if tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Console(chunk.to_vec())),
        })
        .await
        .is_err()
      {
        break;
      }
    }
  }
  offset
}

async fn wait_for_terminal(vsock: &Path, cancel: &CancellationToken) -> bool {
  let deadline = tokio::time::Instant::now() + TERMINAL_WAIT;
  loop {
    if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
      return false;
    }
    if let Ok(client) = Client::connect_firecracker_vsock(vsock, PROCESS_PORT).await {
      match client.health().check(needy_api_client::proto::v1::CheckRequest {}).await {
        Ok(_) => return true,
        Err(status) if status.code() == tonic::Code::Unimplemented => return true,
        Err(_) => {}
      }
    }
    tokio::select! {
      () = cancel.cancelled() => return false,
      () = tokio::time::sleep(TERMINAL_POLL) => {}
    }
  }
}

async fn handle_terminal_task(
  task: BridgeTerminalTask,
  control: Client,
  bridge_tx: mpsc::Sender<BridgeRequest>,
  vsock: PathBuf,
  tasks: tokio_util::task::TaskTracker,
  cancel: CancellationToken,
) {
  let id = task.id;
  let Some(kind) = task.kind else {
    return;
  };
  let local = match Client::connect_firecracker_vsock(&vsock, PROCESS_PORT).await {
    Ok(client) => client,
    Err(e) => {
      let err = needy_api_client::diagnostic::task_error(&tonic::Status::unavailable(format!("cannot reach actor terminal: {e}")));
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::TaskResult(BridgeTaskResult {
            task_id: id,
            result: Some(bridge_task_result::Result::Error(err)),
          })),
        })
        .await;
      return;
    }
  };
  match kind {
    bridge_terminal_task::Kind::ListProcesses(req) => {
      let result = match local.processes().list_processes(req).await {
        Ok(resp) => bridge_task_result::Result::ListProcesses(resp.into_inner()),
        Err(status) => bridge_task_result::Result::Error(needy_api_client::diagnostic::task_error(&status)),
      };
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::TaskResult(BridgeTaskResult {
            task_id: id,
            result: Some(result),
          })),
        })
        .await;
    }
    bridge_terminal_task::Kind::KillProcess(req) => {
      let result = match local.processes().kill_process(req).await {
        Ok(resp) => bridge_task_result::Result::KillProcess(resp.into_inner()),
        Err(status) => bridge_task_result::Result::Error(needy_api_client::diagnostic::task_error(&status)),
      };
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::TaskResult(BridgeTaskResult {
            task_id: id,
            result: Some(result),
          })),
        })
        .await;
    }
    bridge_terminal_task::Kind::Exec(_) => {
      let (to_control_tx, to_control_rx) = mpsc::channel::<TerminalTaskRequest>(32);
      if to_control_tx
        .send(TerminalTaskRequest {
          payload: Some(terminal_task_request::Payload::TaskId(id)),
        })
        .await
        .is_err()
      {
        return;
      }
      let Ok(resp) = control.guest().terminal_task(ReceiverStream::new(to_control_rx)).await else {
        return;
      };
      let mut from_control = resp.into_inner();
      let (to_local_tx, to_local_rx) = mpsc::channel(32);
      // Pump control -> local requests BEFORE opening `local.processes().exec(...)`
      // so the initial Start/Attach message is delivered while the guest's
      // ProcessService waits for it before sending response headers.
      let forward_stop = cancel.clone();
      let forward_in = tasks.spawn(async move {
        let _ = forward_stop
          .run_until_cancelled(async move {
            while let Ok(Some(msg)) = from_control.message().await {
              if let Some(exec) = msg.exec
                && to_local_tx.send(exec).await.is_err()
              {
                break;
              }
            }
          })
          .await;
      });
      let local_resp = match local.processes().exec(ReceiverStream::new(to_local_rx)).await {
        Ok(resp) => resp,
        Err(status) => {
          forward_in.abort();
          let _ = to_control_tx
            .send(TerminalTaskRequest {
              payload: Some(terminal_task_request::Payload::Error(needy_api_client::diagnostic::task_error(&status))),
            })
            .await;
          return;
        }
      };
      let mut from_local = local_resp.into_inner();
      while let Ok(Some(msg)) = from_local.message().await {
        if to_control_tx
          .send(TerminalTaskRequest {
            payload: Some(terminal_task_request::Payload::Exec(msg)),
          })
          .await
          .is_err()
        {
          break;
        }
      }
      forward_in.abort();
    }
  }
}
