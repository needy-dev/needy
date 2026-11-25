//! Actor runner bridge: executes a `StartActor` task dispatched on
//! `RunnerService.Control`.
//!
//! Boots a local `linux-firecracker` microVM for the assigned run, bridges its
//! local vsock channels (`v.sock_7000` guest API, `v.sock` port `7002`
//! reverse terminal) outbound to the remote `needy-daemon` using the
//! runner's own credential plus `x-needy-run-id: <run_id>` (Node
//! Authorizer), and streams Firecracker's `console.log` back to the control
//! plane.
//!
//! The actor's gateway (`v.sock_7001`) runs here, next to the actor, when
//! the control plane agrees (`BridgeAttached.local_gateway`): the actor's
//! traffic leaves from this host, under the policy the control plane sent,
//! and what the gateway records goes back on the bridge to the run's trace.
//! A control plane that does not know a runner's gateway leaves it unset,
//! and the channel is proxied to its shared gateway listener instead. See
//! `docs/ai/references/architecture/2026-11-26-runner-gateway-design.md`.
//!
//! The runner's credential is held exclusively by the host-side runner process
//! and is NEVER injected into the Firecracker guest VM.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use needy_api_client::Client;
use needy_api_client::proto::v1::{
  BridgeActivity, BridgeAttach, BridgeAttached, BridgeEvents, BridgeExit, BridgePrepared, BridgeRequest, BridgeStarted, BridgeTaskResult,
  BridgeTerminalReady, BridgeTerminalTask, FetchLayerRequest, TerminalTaskRequest, TraceEvent, bridge_request, bridge_response,
  bridge_task_result, bridge_terminal_task, terminal_task_request,
};
use needy_api_server::middleware::authentication::RUN_ID_HEADER;
use needy_core_actor::contract::{GATEWAY_PORT, GUEST_PORT, PROCESS_PORT};
use needy_core_actor::driver::{Driver as _, Host, Running, Start, linux_firecracker};
use needy_core_actor::image::store::{Digest, LayerDisk, Store};
use needy_core_actor::sidecar::gateway::{IpCidr, Upstream};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

const VM_STOP_GRACE: Duration = Duration::from_secs(3);
const CONSOLE_POLL: Duration = Duration::from_millis(100);
const TERMINAL_POLL: Duration = Duration::from_millis(200);
const TERMINAL_WAIT: Duration = Duration::from_secs(60);
/// Events the local gateway's trace queue holds before it drops them and
/// records a `trace.gap`, as the daemon's run traces do when full.
const GATEWAY_TRACE_QUEUE: usize = 1024;
/// Events handed from the trace's sink to the bridge forwarder at once.
const GATEWAY_EVENTS_QUEUE: usize = 256;
/// Events in one `BridgeEvents`.
const GATEWAY_EVENTS_BATCH: usize = 64;
/// How often the runner reports the gateway's activity, at most.
// NOTICE: activity feeds the run's `last_activity_ms`, which idle checks
// read in seconds; one report a second keeps that current without a message
// per request.
const GATEWAY_ACTIVITY_INTERVAL: Duration = Duration::from_secs(1);
/// How long the end of a run waits for the gateway's last events to be
/// handed to the bridge before it reports the actor's exit.
const GATEWAY_FLUSH_WAIT: Duration = Duration::from_secs(3);

/// Host-side configuration used by a runner to execute `StartActor` tasks.
#[derive(Debug, Clone)]
pub struct ActorConfig {
  /// Control-plane shared TCP gateway address (`HOST:PORT` or `tcp://HOST:PORT`),
  /// which the actor's gateway channel is proxied to when the control plane
  /// does not let this runner run the gateway itself (an older daemon).
  pub gateway: Option<String>,
  /// Path to the base actor VM image directory.
  pub actor_image: PathBuf,
  /// Where guest disks live (`paths.guest_disks()`).
  pub guest_disks: PathBuf,
  /// The runner's layer store: the layers it fetched of its runs' images
  /// and the disks it made of them, reused by later runs on this host.
  pub layer_store: Store,
  /// Path to the guest `needy` binary injected into the VM.
  pub guest_needy: Option<PathBuf>,
  /// Path to the KVM device (defaults to `/dev/kvm`).
  pub kvm: PathBuf,
  /// Directory under which each run gets its own directory for temporary
  /// VM files (`vm.json`, `v.sock`, `console.log`); uses a temp dir if omitted.
  pub work_dir: Option<PathBuf>,
  /// How the actor's gateway on this runner reaches its upstreams
  /// (`--gateway-upstream`): one proxy per actor when the runner serves one
  /// actor (an ephemeral runner Pod). Applies only to a gateway this runner
  /// runs; an actor whose control plane keeps the gateway is refused.
  pub gateway_upstream: Upstream,
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
        local_gateway: true,
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

  // NOTICE: every run gets a fresh directory, also under a fixed
  // `--work-dir`. Firecracker leaves its vsock device socket (`v.sock`)
  // behind when it exits, and the next VM booted in the same directory
  // fails with `Vsock device error ... UnixBind AddrInUse`; `console.log`
  // would also start with the previous run's console. The directory is
  // removed when this function returns.
  let run_dir = match &config.work_dir {
    Some(dir) => {
      std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
      tempfile::Builder::new()
        .prefix(&format!("{}-", attached.run_id))
        .tempdir_in(dir)
        .with_context(|| format!("cannot create the run's directory in {}", dir.display()))?
    }
    None => tempfile::tempdir().context("cannot create actor work directory")?,
  };
  let work_dir = run_dir.path().to_path_buf();

  let cancel = CancellationToken::new();
  let tasks = tokio_util::task::TaskTracker::new();

  // Bind local vsock UDS endpoints before booting Firecracker.
  // Neither proxy ever exposes the runner credential to the guest VM.
  let guest_uds = linux_firecracker::host_port(&work_dir, GUEST_PORT);
  spawn_guest_grpc_proxy(&guest_uds, control_client.channel(), credential, &attached.run_id, &tasks, &cancel)?;

  let gateway_uds = linux_firecracker::host_port(&work_dir, GATEWAY_PORT);
  // Ends the local gateway's connections and tunnels; cancelled first when
  // the bridge breaks, so a lost link to the control plane cuts the actor's
  // egress before the VM is stopped.
  let gateway_stop = cancel.child_token();
  // The local gateway's trace, flushed before the actor's exit is reported.
  let mut gateway_trace: Option<needy_trace::Dispatch> = None;
  if !attached.local_gateway
    && let Some(gateway_addr) = &config.gateway
  {
    // A control plane that runs the gateway itself (it did not set
    // `local_gateway`): proxy the channel to it, as before S1.
    let gateway_target = gateway_addr.strip_prefix("tcp://").unwrap_or(gateway_addr).to_string();
    spawn_gateway_proxy(&gateway_uds, gateway_target, credential.to_string(), attached.run_id.clone(), &tasks, &cancel)?;
  }

  let host = Host {
    image: config.actor_image.clone(),
    kvm: config.kvm.clone(),
    guest_disks: config.guest_disks.clone(),
  };
  let driver = linux_firecracker::LinuxFirecracker::on(&host, None);
  // What fails before the VM runs is reported as the actor's exit, so the
  // run fails with its reason rather than as a lost bridge.
  let booted = async {
    // An older control plane runs the gateway itself, where this runner's
    // upstream does not apply: the actor would leave directly from the
    // control host. Fail closed instead.
    if !attached.local_gateway && config.gateway_upstream != Upstream::Direct {
      bail!(
        "this runner has a gateway upstream (--gateway-upstream), but the control plane runs the actor's gateway itself (it is older than this runner); refusing to start the actor without its upstream"
      );
    }
    let layer_disks = prepare_layers(&control_client, &config.layer_store, &config.actor_image, &attached, &work_dir).await?;
    // TODO(remote-actor-images): the time the fetch took belongs in
    // `actor.start` (`layers_fetched_ms`, D6 of
    // 2026-11-25-remote-actor-images-spec.md); it needs a trace schema
    // version, since `ActorStart` refuses unknown fields. Reopen with that
    // schema change.
    bridge_tx
      .send(BridgeRequest {
        payload: Some(bridge_request::Payload::Prepared(BridgePrepared {
          layer_disks: layer_disks.iter().map(|disk| disk.digest.to_string()).collect(),
        })),
      })
      .await
      .context("control plane bridge closed before the VM started")?;

    let recipe: Option<needy_core::recipe::CompiledRecipe> = if attached.recipe_json.is_empty() {
      None
    } else {
      Some(serde_json::from_str(&attached.recipe_json).context("cannot decode compiled recipe from control plane")?)
    };
    let exe = std::env::current_exe().context("cannot locate current executable")?;
    let guest_needy = needy_core_actor::image::guest::find(config.guest_needy.as_deref(), &exe, needy_core_actor::image::guest::arch())
      .context("cannot locate guest needy binary")?;

    let profile = needy_config::profile::load(&needy_config::Source::text(attached.profile_yaml.clone(), PathBuf::new()))
      .context("cannot decode profile from control plane")?;
    if attached.local_gateway {
      gateway_trace = Some(serve_gateway(
        &gateway_uds,
        &profile,
        &attached,
        &config.gateway_upstream,
        bridge_tx.clone(),
        &tasks,
        gateway_stop.clone(),
      )?);
    }

    let layer_disks: Vec<PathBuf> = layer_disks.into_iter().map(|disk| disk.path).collect();
    driver
      .start(&Start {
        run: &attached.run_id,
        dir: &work_dir,
        user: &profile.user,
        recipe: recipe.as_ref(),
        mounts: &[],
        guest: &guest_needy,
        layer_disks: &layer_disks,
        snapshot_image: None,
      })
      .await
      .context("cannot start local Firecracker microVM")
  }
  .await;
  let mut vm: Box<dyn Running> = match booted {
    Ok(vm) => vm,
    Err(e) => {
      let exit = BridgeExit {
        code: None,
        signal: String::new(),
        error: format!("{e:#}"),
      };
      // The control plane may be gone already; the error is returned either way.
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Exit(exit)),
        })
        .await;
      cancel.cancel();
      // The gateway's event forwarder ends once its trace is dropped.
      drop(gateway_trace);
      drop(bridge_tx);
      tasks.close();
      let _ = tokio::time::timeout(Duration::from_secs(3), tasks.wait()).await;
      return Err(e);
    }
  };

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
          gateway_stop.cancel();
          bridge_broken = true;
          let _ = vm.stop(VM_STOP_GRACE).await;
          break None;
        }
        Err(status) => {
          tracing::warn!("run {}: actor bridge error: {status}; stopping microVM", attached.run_id);
          gateway_stop.cancel();
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
  // The actor is gone: its gateway ends, and what it recorded reaches the
  // run's trace before the actor's exit does.
  gateway_stop.cancel();
  if !bridge_broken {
    flush_console(&console_path, final_offset, &bridge_tx).await;
    if let Some(trace) = &gateway_trace
      && tokio::time::timeout(GATEWAY_FLUSH_WAIT, trace.flush()).await.is_err()
    {
      tracing::warn!("run {}: the gateway's last events did not reach the control plane in time", attached.run_id);
    }
    if let Some(exit) = vm_exit {
      let _ = bridge_tx
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Exit(exit)),
        })
        .await;
    }
  }

  cancel.cancel();
  // The gateway's event forwarder ends once its trace is dropped.
  drop(gateway_trace);
  drop(bridge_tx);
  tasks.close();
  let _ = tokio::time::timeout(Duration::from_secs(3), tasks.wait()).await;

  if bridge_broken {
    bail!("control plane bridge disconnected");
  }
  Ok(())
}

/// Fetches the layers of the run's snapshot (`attached.layers`) this
/// runner's `store` lacks from the control plane, checks the root they were
/// built on against the base it boots from `base`, and makes the disks to
/// attach, lowest first. No layers, no disks: the actor boots the base.
async fn prepare_layers(control: &Client, store: &Store, base: &Path, attached: &BridgeAttached, scratch: &Path) -> Result<Vec<LayerDisk>> {
  if attached.layers.is_empty() {
    return Ok(Vec::new());
  }
  let base_dir = base.to_path_buf();
  let root = attached.root.clone();
  tokio::task::spawn_blocking(move || check_root(&base_dir, &root)).await.context("checking the base VM image panicked")??;
  let layers = attached
    .layers
    .iter()
    .map(|layer| layer.parse::<Digest>().with_context(|| format!("the control plane named an invalid layer digest {layer:?}")))
    .collect::<Result<Vec<_>>>()?;
  for layer in &layers {
    if !store.has_layer(layer) {
      fetch_layer(control, store, layer, scratch).await?;
    }
  }
  let store = store.clone();
  tokio::task::spawn_blocking(move || -> Result<Vec<LayerDisk>> {
    let snapshot = store.snapshot(&layers)?;
    Ok(store.materialize(&snapshot)?)
  })
  .await
  .context("materializing the run's layers panicked")?
  .context("cannot make the run's layer disks")
}

/// Refuses to stack layers built on base `root` (`sha256:<hex>`) on the base
/// VM image in `base`, unless it is the same (D4 in
/// `2026-11-25-remote-actor-images-spec.md`). Blocking.
fn check_root(base: &Path, root: &str) -> Result<()> {
  let built_on: Digest = root.parse().with_context(|| format!("the control plane named the run's layers with an invalid root {root:?}"))?;
  let boots = crate::image::base_root(base).with_context(|| format!("cannot read the base VM image in {}", base.display()))?;
  if built_on != boots {
    bail!(
      "the run's image layers were built on base {built_on}, but this runner boots base {boots} from {}; refusing to stack them \
       on another base: give the control daemon and this runner the same base VM image",
      base.display()
    );
  }
  Ok(())
}

/// Fetches layer `digest` from the control plane into `store` through a file
/// in `scratch`, never holding it whole, and checks that what was stored is
/// the layer asked for.
async fn fetch_layer(control: &Client, store: &Store, digest: &Digest, scratch: &Path) -> Result<()> {
  let mut parts = control
    .guest()
    .fetch_layer(FetchLayerRequest {
      digest: digest.to_string(),
    })
    .await
    .with_context(|| format!("control plane refused layer {digest}"))?
    .into_inner();
  let staged = tempfile::NamedTempFile::new_in(scratch).with_context(|| format!("cannot stage layer {digest} in {}", scratch.display()))?;
  let mut file = tokio::fs::File::from_std(staged.reopen().with_context(|| format!("cannot open {}", staged.path().display()))?);
  while let Some(part) = parts.message().await.with_context(|| format!("fetching layer {digest} broke"))? {
    file.write_all(&part.data).await.with_context(|| format!("cannot write {}", staged.path().display()))?;
  }
  file.flush().await.with_context(|| format!("cannot write {}", staged.path().display()))?;
  drop(file);
  let store = store.clone();
  let stored = tokio::task::spawn_blocking(move || -> Result<Digest> {
    let archive = std::fs::File::open(staged.path()).with_context(|| format!("cannot open {}", staged.path().display()))?;
    Ok(store.put(std::io::BufReader::new(archive))?)
  })
  .await
  .context("storing a fetched layer panicked")??;
  if stored != *digest {
    bail!("the control plane sent layer {stored} for {digest}");
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

/// Serves the actor's gateway on this runner on Firecracker's gateway
/// channel `uds`, until `stop`: the profile's allowlist and services, its
/// network faults with their windows counted from the run's creation on the
/// control plane (`attached.created_ms`), and the control plane's blocked
/// CIDRs, leaving through this runner's `upstream`, built as the daemon
/// builds it (`actor::gateway`). What it records
/// goes to the run's trace over the bridge (`BridgeEvents`), and its
/// activity as `BridgeActivity`. Returns its trace, for the end of the run
/// to flush.
fn serve_gateway(
  uds: &Path,
  profile: &needy_config::profile::Profile,
  attached: &BridgeAttached,
  upstream: &Upstream,
  bridge: mpsc::Sender<BridgeRequest>,
  tasks: &tokio_util::task::TaskTracker,
  stop: CancellationToken,
) -> Result<needy_trace::Dispatch> {
  let blocked = attached
    .gateway_blocked_cidrs
    .iter()
    .map(|cidr| cidr.parse::<IpCidr>().map_err(|e| anyhow::anyhow!("the control plane named an invalid blocked CIDR {cidr:?}: {e}")))
    .collect::<Result<Vec<_>>>()?;
  let injector = Arc::new(needy_core::chaos::Injector::since_ms(&profile.chaos, attached.created_ms));
  let (events, forwarded) = mpsc::channel(GATEWAY_EVENTS_QUEUE);
  let trace = needy_trace::Dispatch::new(
    attached.run_id.clone(),
    vec![Box::new(BridgeSink { events })],
    needy_trace::Options {
      capacity: GATEWAY_TRACE_QUEUE,
      version: needy_core::event::SCHEMA_VERSION,
      timing: false,
    },
  );
  tasks.spawn(forward_events(forwarded, bridge.clone()));
  let active = Arc::new(AtomicBool::new(false));
  tasks.spawn(forward_activity(active.clone(), bridge, stop.clone()));
  let gateway = crate::actor::gateway(
    &profile.actor.network,
    trace.clone(),
    injector,
    blocked,
    upstream.clone(),
    stop,
    Arc::new(move || active.store(true, Ordering::Relaxed)),
  );
  if let Err(e) = std::fs::remove_file(uds)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", uds.display());
  }
  let listener = tokio::net::UnixListener::bind(uds).with_context(|| format!("cannot bind {}", uds.display()))?;
  tasks.spawn(gateway.run(tokio_stream::wrappers::UnixListenerStream::new(listener)));
  Ok(trace)
}

/// What the local gateway's trace sink hands the bridge forwarder.
enum Forward {
  Event(TraceEvent),
  /// Answered once every event before it was handed to the bridge.
  Flush(oneshot::Sender<()>),
}

/// The local gateway's trace sink: each event goes to the run's trace on
/// the control plane, through [`forward_events`]. Waiting for the
/// forwarder fills the trace's queue when the bridge is slow, which then
/// drops and records a `trace.gap`, as a full trace does on the daemon.
struct BridgeSink {
  events: mpsc::Sender<Forward>,
}

#[async_trait::async_trait]
impl needy_trace::Sink for BridgeSink {
  async fn write(&mut self, event: &needy_trace::Event) -> Result<(), needy_trace::Error> {
    let event = TraceEvent {
      time_ms: event.time_ms,
      kind: event.kind.clone(),
      payload: event.payload.get().to_string(),
      version: event.version,
    };
    // A closed forwarder means the bridge is gone, and the run's trace with
    // it: there is nowhere left to write.
    let _ = self.events.send(Forward::Event(event)).await;
    Ok(())
  }

  /// Waits until every event written before is handed to the bridge.
  async fn flush(&mut self) -> Result<(), needy_trace::Error> {
    let (done, handed) = oneshot::channel();
    if self.events.send(Forward::Flush(done)).await.is_ok() {
      // A forwarder that ended dropped `done`: the bridge is gone.
      let _ = handed.await;
    }
    Ok(())
  }
}

/// Sends the local gateway's events on the bridge in order, as many at a
/// time as are waiting (up to [`GATEWAY_EVENTS_BATCH`]), until the trace
/// closes or the bridge does.
async fn forward_events(mut events: mpsc::Receiver<Forward>, bridge: mpsc::Sender<BridgeRequest>) {
  while let Some(first) = events.recv().await {
    let mut batch = Vec::new();
    let mut flushed = None;
    let mut next = Some(first);
    while let Some(item) = next.take() {
      match item {
        Forward::Event(event) => batch.push(event),
        Forward::Flush(done) => {
          flushed = Some(done);
          break;
        }
      }
      if batch.len() >= GATEWAY_EVENTS_BATCH {
        break;
      }
      next = events.try_recv().ok();
    }
    if !batch.is_empty()
      && bridge
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Events(BridgeEvents { events: batch })),
        })
        .await
        .is_err()
    {
      return;
    }
    if let Some(done) = flushed {
      // The flush's caller may have stopped waiting.
      let _ = done.send(());
    }
  }
}

/// Reports that the local gateway saw a request (`active`), at most once
/// per [`GATEWAY_ACTIVITY_INTERVAL`], until `stop` or the bridge closes.
async fn forward_activity(active: Arc<AtomicBool>, bridge: mpsc::Sender<BridgeRequest>, stop: CancellationToken) {
  let mut tick = tokio::time::interval(GATEWAY_ACTIVITY_INTERVAL);
  tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
  loop {
    tokio::select! {
      () = stop.cancelled() => return,
      _ = tick.tick() => {}
    }
    if active.swap(false, Ordering::Relaxed)
      && bridge
        .send(BridgeRequest {
          payload: Some(bridge_request::Payload::Activity(BridgeActivity {})),
        })
        .await
        .is_err()
    {
      return;
    }
  }
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

#[cfg(test)]
#[path = "actor_runner_test.rs"]
mod tests;
