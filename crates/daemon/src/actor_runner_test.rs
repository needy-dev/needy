//! Remote actors with image definitions: the control daemon prepares the
//! image, the bridge names the snapshot, the runner fetches, materializes and
//! reports the layer disks, and checks the root they were built on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_client::Client;
use needy_api_client::proto::v1::{
  BridgeAttach, BridgeAttached, BridgePrepared, BridgeRequest, FetchLayerRequest, bridge_request, bridge_response,
};
use needy_api_server::Listen;
use needy_api_server::control::{Guest, MintRunnerCredential, Runners, Runs, StopMode};
use needy_core::types::{Capabilities, Principal, RunState, RunnerRole};
use needy_core_actor::image::store::Digest;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::bridge::tests::connect_actor_runner;
use crate::image::tests::{BuildBehavior, session_driver_script, spawn_build_responder};
use crate::testing::{self, Started, fake_guest, fake_image, new_run};

const REMOTE_IMAGE_PROFILE: &str = r#"
actor:
  driver: linux-firecracker
  runner: actor-node
  command: [sh, -c, 'echo hi']
  image:
    root: needy-actor:latest
    env:
      IMAGE_ONLY: "1"
    layers:
      - run: echo step-one
      - run: echo step-two
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

/// A control daemon serving on TCP whose build sessions a responder drives,
/// as `image_test` does for local runs.
struct Control {
  started: Started,
  addr: String,
  sessions: Arc<AtomicUsize>,
  stop: CancellationToken,
  server: tokio::task::JoinHandle<Result<(), needy_api_server::server::Error>>,
  responder: tokio::task::JoinHandle<()>,
}

impl Control {
  async fn start(adjust: impl FnOnce(&mut crate::Config)) -> Self {
    let started = testing::daemon_with(adjust).await;
    let started_log = started.dir.path().join("started.log");
    fake_image(&started.dir.path().join("image"), &session_driver_script(&started_log));
    let stop = CancellationToken::new();
    let behavior = Arc::new(std::sync::Mutex::new(BuildBehavior::default()));
    let (sessions, _configs, responder) = spawn_build_responder(started_log, behavior, stop.clone());
    let bound = started.daemon.server(None).bind(&[Listen::Tcp("127.0.0.1:0".into())]).await.unwrap();
    let Listen::Tcp(addr) = &bound.addresses()[0] else {
      unreachable!();
    };
    let addr = addr.clone();
    let server = tokio::spawn(bound.serve(stop.clone()));
    Self {
      started,
      addr,
      sessions,
      stop,
      server,
      responder,
    }
  }

  fn daemon(&self) -> &crate::Daemon {
    &self.started.daemon
  }

  async fn create(&self, profile: &str) -> anyhow::Result<needy_api_server::control::RunInfo> {
    let mut request = new_run(profile, self.started.dir.path());
    request.wait_for_runner_ms = 5_000;
    let created = tokio::time::timeout(Duration::from_secs(20), self.daemon().create(request)).await.expect("run create timed out");
    created.map_err(|e| anyhow::anyhow!("{e}"))
  }

  fn snapshot_layers(&self, run: &str) -> Vec<Digest> {
    self.daemon().runs.find(run).unwrap().boot().snapshot.expect("the run has a snapshot").layers
  }

  async fn end(self) {
    self.stop.cancel();
    self.responder.await.unwrap();
    let _ = self.server.await;
    self.started.daemon.shutdown().await;
  }
}

/// An actor runner `actor-node` running in process against `control`, whose
/// base VM image in `dir` has `rootfs` as its root file system, and whose
/// stand-in Firecracker runs until its run stops it.
async fn start_runner(control: &Control, dir: &Path, rootfs: &str) -> tokio::task::JoinHandle<anyhow::Result<()>> {
  let minted = Runners::mint_credential(
    control.daemon(),
    MintRunnerCredential {
      name: "actor-node".into(),
      ttl_ms: 60_000,
      roles: [RunnerRole::Actor].into(),
      labels: Default::default(),
    },
  )
  .await
  .unwrap();
  let image = dir.join("image");
  fake_image(&image, testing::PARKED);
  std::fs::write(image.join("rootfs.ext4"), rootfs).unwrap();
  let kvm = dir.join("kvm");
  std::fs::write(&kvm, "").unwrap();
  let guest = dir.join("needy-linux");
  fake_guest(&guest);
  let config = crate::runner::Config {
    state_dir: dir.join("state"),
    control: Some(format!("http://{}", control.addr)),
    join_token: None,
    credential: Some(minted.credential),
    name: "actor-node".into(),
    capabilities: Capabilities {
      actor: [needy_config::profile::ActorDriver::LinuxFirecracker].into(),
      executor: Default::default(),
    },
    labels: Default::default(),
    grace: Duration::from_secs(5),
    activity_hook: None,
    ephemeral: false,
    actor: Some(crate::runner::ActorConfig {
      gateway: None,
      actor_image: image,
      vm_images: dir.join("images"),
      guest_disks: dir.join("guest-disks"),
      layer_store: crate::runner::layer_store(&dir.join("cache"), &dir.join("data")),
      guest_needy: Some(guest),
      kvm,
      work_dir: Some(dir.join("work")),
    }),
  };
  let stop = control.stop.clone();
  tokio::spawn(async move { crate::runner::Runner::start(config).await?.serve(stop).await })
}

/// A client calling as the actor runner holding `credential` on behalf of
/// run `run` (Node Authorizer).
async fn bound_client(addr: &str, credential: &str, run: &str) -> Client {
  Client::connect(&format!("http://{addr}")).await.unwrap().with_token(credential).unwrap().with_run_id(run).unwrap()
}

async fn fetched(client: &Client, digest: &str) -> Result<Vec<u8>, tonic::Status> {
  let mut parts = client
    .guest()
    .fetch_layer(FetchLayerRequest {
      digest: digest.into(),
    })
    .await?
    .into_inner();
  let mut archive = Vec::new();
  while let Some(part) = parts.message().await? {
    archive.extend(part.data);
  }
  Ok(archive)
}

// ROOT CAUSE:
//
// If a run's actor was placed on a remote actor runner, `run create` skipped
// `image::prepare`, so the run had no snapshot and its actor booted the base
// image silently: no feature programs, no image `PATH`, no hooks.
//
// Before the fix, the create succeeded with no snapshot.
// The fix prepares the image on the control daemon for remote actors too.
#[tokio::test]
async fn a_remote_actor_with_an_image_definition_does_not_boot_the_base_silently() {
  let control = Control::start(|_| {}).await;
  let _runner = connect_actor_runner(control.daemon(), &control.addr, "actor-node").await;

  let created = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();

  assert!(created.snapshot.is_some(), "a remote run with an image definition must record its snapshot");
  let boot = control.daemon().runs.find(&created.id).unwrap().boot();
  assert_eq!(boot.snapshot.as_ref().map(|s| s.layers.len()), Some(2));
  assert_eq!(boot.env.get("IMAGE_ONLY").map(String::as_str), Some("1"));
  assert!(boot.root.is_some());
  assert!(boot.layer_disks.is_empty(), "the daemon makes no disks for a remote actor");
  control.end().await;
}

#[tokio::test]
async fn the_bridge_names_the_snapshot_and_its_root_and_the_guest_stacks_the_reported_disks() {
  let control = Control::start(|_| {}).await;
  let runner = connect_actor_runner(control.daemon(), &control.addr, "actor-node").await;
  let created = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  let layers = control.snapshot_layers(&created.id);
  let root = crate::image::root_digest(control.daemon()).await.unwrap();

  let (tx, rx) = mpsc::channel(16);
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Attach(BridgeAttach::default())),
  }))
  .await
  .unwrap();
  let mut out = control.daemon().bridge(&created.id, Box::pin(ReceiverStream::new(rx))).await.unwrap();
  let Some(bridge_response::Payload::Attached(BridgeAttached {
    layers: named,
    root: named_root,
    ..
  })) = out.next().await.unwrap().payload
  else {
    panic!("the bridge did not answer attach with attached");
  };
  assert_eq!(named, layers.iter().map(ToString::to_string).collect::<Vec<_>>());
  assert_eq!(named_root, root.to_string());

  // A layer of the run's snapshot streams as its canonical archive; any
  // other digest is not the runner's to read, even one this daemon holds.
  let client = bound_client(&control.addr, &runner.credential, &created.id).await;
  let archive = fetched(&client, &named[1]).await.unwrap();
  assert_eq!(Digest::of(&archive), layers[1]);
  let outside = fetched(&client, &Digest::of(b"another run's layer").to_string()).await.unwrap_err();
  assert_eq!(outside.code(), tonic::Code::PermissionDenied);
  let invalid = fetched(&client, "sha256:nope").await.unwrap_err();
  assert_eq!(invalid.code(), tonic::Code::InvalidArgument);

  // The guest is told the disks the runner made, which may differ from the
  // snapshot's layers when the runner flattened a lower prefix.
  let flattened = Digest::of(b"a flattened lower prefix").to_string();
  let reported = vec![flattened, named[1].clone()];
  tx.send(Ok(BridgeRequest {
    payload: Some(bridge_request::Payload::Prepared(BridgePrepared {
      layer_disks: reported.clone(),
    })),
  }))
  .await
  .unwrap();
  let config = tokio::time::timeout(Duration::from_secs(5), Guest::config(control.daemon(), &created.id)).await.unwrap().unwrap();
  assert_eq!(config.layers, reported);

  Runs::stop(control.daemon(), &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  drop(tx);
  control.end().await;
}

#[tokio::test]
async fn a_remote_runner_fetches_materializes_and_boots_the_run_s_image_layers() {
  let control = Control::start(|_| {}).await;
  let runner_dir = tempfile::tempdir().unwrap();
  // The same base as the control daemon's stand-in image: an empty rootfs.
  let runner = start_runner(&control, runner_dir.path(), "").await;

  let created = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  let layers = control.snapshot_layers(&created.id);
  assert_eq!(control.sessions.load(Ordering::SeqCst), 2, "the control daemon built both steps");

  // Init's configuration names the disks the runner reported.
  let config = tokio::time::timeout(Duration::from_secs(10), Guest::config(control.daemon(), &created.id)).await.unwrap().unwrap();
  assert_eq!(config.layers, layers.iter().map(ToString::to_string).collect::<Vec<_>>());

  // The runner keeps the layers in its own store and attaches their disks.
  let store = crate::runner::layer_store(&runner_dir.path().join("cache"), &runner_dir.path().join("data"));
  for layer in &layers {
    assert!(store.has_layer(layer), "the runner's store lacks {layer}");
  }
  // The runner reports the disks before it starts the VM.
  let vm_json = runner_dir.path().join("work/vm.json");
  for _ in 0..200 {
    if vm_json.exists() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  let vm_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&vm_json).unwrap()).unwrap();
  let drives: Vec<_> = vm_json["drives"].as_array().unwrap().iter().map(|d| d["drive_id"].as_str().unwrap().to_string()).collect();
  assert!(drives.ends_with(&["layer-0".to_string(), "layer-1".to_string()]), "{drives:?}");
  for drive in &vm_json["drives"].as_array().unwrap()[drives.len() - 2..] {
    let path = PathBuf::from(drive["path_on_host"].as_str().unwrap());
    assert!(path.starts_with(runner_dir.path()), "{}", path.display());
    assert_eq!(&std::fs::read(&path).unwrap()[..4], b"hsqs");
  }

  Runs::stop(control.daemon(), &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  control.end().await;
  let _ = tokio::time::timeout(Duration::from_secs(10), runner).await;
}

#[tokio::test]
async fn a_remote_runner_refuses_layers_built_on_another_base() {
  let control = Control::start(|_| {}).await;
  let runner_dir = tempfile::tempdir().unwrap();
  let _runner = start_runner(&control, runner_dir.path(), "another base").await;

  let created = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();

  let mut info = control.daemon().get(&created.id).await.unwrap();
  for _ in 0..200 {
    info = control.daemon().get(&created.id).await.unwrap();
    if info.state == RunState::Failed {
      break;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  assert_eq!(info.state, RunState::Failed);
  let failure = info.failure.unwrap_or_default();
  assert!(failure.starts_with("the actor runner could not start the actor: "), "{failure}");
  assert!(failure.contains("refusing to stack them on another base"), "{failure}");
  assert!(!runner_dir.path().join("work/vm.json").exists(), "the runner must not start the VM");
  control.end().await;
}

#[tokio::test]
async fn a_control_daemon_without_kvm_serves_layers_it_holds_and_refuses_ones_it_cannot_build() {
  let control = Control::start(|_| {}).await;
  let mut runner = connect_actor_runner(control.daemon(), &control.addr, "actor-node").await;
  let first = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  assert_eq!(control.sessions.load(Ordering::SeqCst), 2);
  // The runner is dispatched the first run's actor.
  let _start = runner.ctrl_stream.message().await.unwrap().unwrap();

  // The host loses KVM: it can no longer build, but holds both layers.
  std::fs::remove_file(&control.daemon().config.kvm).unwrap();
  let second = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  assert_eq!(second.snapshot, first.snapshot);
  assert_eq!(control.sessions.load(Ordering::SeqCst), 2, "nothing was built");
  let _start = runner.ctrl_stream.message().await.unwrap().unwrap();

  // A step it does not hold fails the create before any actor starts.
  let missing_step = REMOTE_IMAGE_PROFILE.replace("echo step-two", "echo step-three");
  let refused = control.create(&missing_step).await.unwrap_err().to_string();
  assert!(refused.contains("image step `echo step-three`"), "{refused}");
  assert!(refused.contains("this host cannot build it"), "{refused}");
  let dispatched = tokio::time::timeout(Duration::from_millis(300), runner.ctrl_stream.message()).await;
  assert!(dispatched.is_err(), "no actor may start for a refused create: {dispatched:?}");
  assert_eq!(control.sessions.load(Ordering::SeqCst), 2);

  for id in [&first.id, &second.id] {
    Runs::stop(control.daemon(), id, Principal::Operator, StopMode::Now).await.unwrap();
  }
  control.end().await;
}

#[test]
fn a_runner_stacks_layers_only_on_the_base_they_were_built_on() {
  let dir = tempfile::tempdir().unwrap();
  let image = dir.path().join("image");
  fake_image(&image, testing::PARKED);
  std::fs::write(image.join("rootfs.ext4"), "base").unwrap();

  super::check_root(&image, &Digest::of(b"base").to_string()).unwrap();
  let refused = super::check_root(&image, &Digest::of(b"another base").to_string()).unwrap_err();
  assert!(format!("{refused:#}").contains("refusing to stack them on another base"), "{refused:#}");
  let invalid = super::check_root(&image, "").unwrap_err();
  assert!(format!("{invalid:#}").contains("invalid root"), "{invalid:#}");

  // A released image's manifest names its root; the file is not hashed.
  let mut manifest = needy_core_actor::image::manifest::read(&image).unwrap().unwrap();
  manifest.sha256 = Some(Digest::of(b"released").hex().to_string());
  needy_core_actor::image::manifest::write(&image, &manifest).unwrap();
  super::check_root(&image, &Digest::of(b"released").to_string()).unwrap();
}

/// A control daemon with KVM, sharing layers through `repository`, that
/// builds both steps of [`REMOTE_IMAGE_PROFILE`] and pushes them.
async fn build_into_registry(repository: &str) -> Vec<Digest> {
  let repository = repository.to_string();
  let builder = Control::start(move |config| config.layer_registry = Some(repository)).await;
  let _runner = connect_actor_runner(builder.daemon(), &builder.addr, "actor-node").await;
  let created = builder.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  assert_eq!(builder.sessions.load(Ordering::SeqCst), 2, "the builder built both steps");
  let layers = builder.snapshot_layers(&created.id);
  Runs::stop(builder.daemon(), &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  builder.end().await;
  layers
}

/// A control daemon without KVM and with an empty layer store, sharing
/// layers through `repository`.
async fn control_without_kvm(repository: &str) -> Control {
  let repository = repository.to_string();
  let control = Control::start(move |config| config.layer_registry = Some(repository)).await;
  std::fs::remove_file(&control.daemon().config.kvm).unwrap();
  control
}

// ROOT CAUSE:
//
// If a control daemon could not build (no KVM), a remote run's step missing
// from its layer store failed the create even when the layer registry held
// it, because the remote plan took steps from the store only and never
// reached the registry lookup inside the step's single flight.
//
// Before the fix, the create failed with "this host cannot build it".
// The fix tries the store, then the layer registry, and only then refuses.
#[tokio::test]
async fn a_control_daemon_without_kvm_takes_remote_steps_from_the_layer_registry() {
  use needy_core_actor::image::store::registry::testing::TestRegistry;

  let registry = TestRegistry::start().await;
  let repository = registry.repository("needy/layers");
  let built = build_into_registry(&repository).await;

  let control = control_without_kvm(&repository).await;
  let mut runner = connect_actor_runner(control.daemon(), &control.addr, "actor-node").await;
  let created = control.create(REMOTE_IMAGE_PROFILE).await.unwrap();
  assert_eq!(control.snapshot_layers(&created.id), built);
  assert_eq!(control.sessions.load(Ordering::SeqCst), 0, "nothing was built on a host without KVM");
  let _start = runner.ctrl_stream.message().await.unwrap().unwrap();

  // A step neither here nor in the registry is refused, naming the registry.
  let missing_step = REMOTE_IMAGE_PROFILE.replace("echo step-two", "echo step-three");
  let refused = control.create(&missing_step).await.unwrap_err().to_string();
  assert!(refused.contains("image step `echo step-three`"), "{refused}");
  assert!(refused.contains(&format!("nor in the layer registry {repository}")), "{refused}");
  assert!(refused.contains("this host cannot build it"), "{refused}");

  Runs::stop(control.daemon(), &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  control.end().await;
}
