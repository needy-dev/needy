//! Interactive image edit sessions, commit collapse, rollback, and commit
//! history queries.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use needy_api_server::control::{
  Caller, CommitHistoryEntry, ControlError, GuestConfig, GuestSession, GuestUser, ImageEditFinished, ImageEditStart, ImageEditStarted,
  ImageLog, ImageLogResult, ImageRollback, ImageRollbackResult, RecordedCommand, UpperLayer, WindowSize,
};
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_common::sync::lock;
use needy_config::profile::{ActorDriver, CONTAINER_IMAGE, OciRuntime, User};
use needy_config::value::Glob;
use needy_core::image::{Commit as PlanCommit, Digest};
use needy_core::network::Policy;
use needy_core_actor::driver;
use needy_core_actor::driver::linux_container::LinuxContainer;
use needy_core_actor::image::store::{Command as StoreCommand, Error as StoreError, extract_top_layer};
use needy_core_executor::engine::Engine;
use needy_trace::Dispatch;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::build::{build_image_plan, select_builder_driver};
use super::container::commit_container_to_file;
use super::session::{
  EditSessionRuns, SESSION_STOP_GRACE, SessionGuest, cleanup_session_dir, daemon_gid, daemon_uid, open_socket_permissions,
  serve_session_gateway, session_profile_yaml,
};
use super::{Images, check_driver_host, parse_definition, parse_driver};
use crate::guest::UPPER_LAYER;
use crate::pool::Instance;
use crate::{Daemon, actor};

/// Result broadcast when an interactive edit session finishes.
type EditOutcomeResult = std::result::Result<ImageEditFinished, String>;

/// A running interactive edit session.
pub(crate) struct EditSession {
  pub id: String,
  pub(super) snapshot: Digest,
  channels: driver::Channels,
  cancel: CancellationToken,
  aborted: Arc<AtomicBool>,
  terminal: tokio::sync::OnceCell<Arc<Instance>>,
  result_rx: watch::Receiver<Option<EditOutcomeResult>>,
}

impl EditSession {
  /// Connects to the edit session actor's process service for terminal attach.
  pub(crate) async fn terminal(&self) -> Result<Arc<Instance>, ControlError> {
    self
      .terminal
      .get_or_try_init(|| async {
        let client = actor::connect_terminal(&self.channels, &self.cancel, &format!("edit session {}", self.id)).await?;
        Ok(Instance::actor(&self.id, Arc::new(client)))
      })
      .await
      .cloned()
  }
}

/// Releases an image's per-image edit lock when dropped.
struct EditLockGuard {
  images: Arc<Images>,
  image_name: String,
}

impl Drop for EditLockGuard {
  fn drop(&mut self) {
    lock(&self.images.active_edits).remove(&self.image_name);
  }
}

impl Images {
  /// Looks up an active interactive edit session by its session ID.
  pub(crate) fn edit_session(&self, id: &str) -> Option<Arc<EditSession>> {
    lock(&self.edits).get(id).cloned()
  }

  /// Acquires the per-image edit lock for `image_name`.
  fn lock_edit(self: &Arc<Self>, image_name: &str) -> Result<EditLockGuard, ControlError> {
    let mut active = lock(&self.active_edits);
    if !active.insert(image_name.to_string()) {
      return Err(ControlError::Failed(format!("an edit session is already running for image {image_name}")));
    }
    Ok(EditLockGuard {
      images: self.clone(),
      image_name: image_name.to_string(),
    })
  }
}

pub(super) async fn start_edit(daemon: &Daemon, request: ImageEditStart) -> Result<ImageEditStarted, ControlError> {
  daemon.admit_work()?;
  let definition = parse_definition(&request.image_yaml)?;
  let target_driver = parse_driver(&request.driver)?;
  let user = request.user.unwrap_or_default();
  let base_container_image = definition.root.as_deref().unwrap_or(CONTAINER_IMAGE);
  let builder = select_builder_driver(daemon, target_driver).await;
  check_driver_host(daemon, builder, base_container_image).await?;

  // Enforce one edit session per image at a time. The guard stays alive
  // inside the background watcher task until the edit session ends.
  let lock_guard = daemon.images.lock_edit(&request.image_name)?;

  let session_id = format!("e-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
  let trace = Dispatch::new(
    &session_id,
    Vec::new(),
    needy_trace::Options {
      capacity: 256,
      version: needy_core::event::SCHEMA_VERSION,
      timing: false,
    },
  );

  // Build and materialize for the builder driver (VM layer disks, or an OCI
  // image in Docker when falling back to `linux-container` on a Linux host
  // without a VM driver).
  let resolved =
    build_image_plan(daemon, &definition, builder, &user, request.commit.as_ref(), &request.pinned_features, &trace, &daemon.cancel)
      .await
      .map_err(crate::failed)?;

  let base_top = *resolved.plan.top();
  let commit_rebased = resolved.plan.commit_rebased();
  let resolved_features = resolved.resolved_features;
  let parent_layer: Option<Digest> = request.commit.as_ref().map(|c| c.layer);

  let dir = daemon.config.state_dir.join("sessions").join(&session_id);
  needy_api_server::create_private_dir(&dir).with_context(|| format!("cannot create {}", dir.display())).map_err(crate::failed)?;

  let root_user = User {
    name: "root".into(),
    uid: 0,
    gid: 0,
  };
  let guest_bin = actor::guest_needy(&daemon.config).map_err(crate::failed)?;

  let session_cancel = daemon.cancel.child_token();
  let tasks = TaskTracker::new();
  let commands: Arc<Mutex<Vec<StoreCommand>>> = Arc::default();

  let is_container_builder = resolved.builder == ActorDriver::LinuxContainer;
  let (channels, profile_yaml) = if is_container_builder {
    let drv = LinuxContainer::with_engine(OciRuntime::Runc, base_container_image.to_string(), daemon.images.backends().engine.clone());
    (driver::Driver::channels(&drv, &dir), String::new())
  } else {
    let profile_yaml = session_profile_yaml(resolved.builder, &root_user, None);
    let session_profile =
      needy_config::profile::load(&needy_config::Source::text(profile_yaml.clone(), PathBuf::new())).map_err(|e| crate::failed(e.into()))?;
    let actor_driver = driver::new(&actor::host(daemon), &session_profile.actor).map_err(|e| crate::failed(e.into()))?;
    (actor_driver.channels(&dir), profile_yaml)
  };

  // Spec ("Recording an edit session"): route every exec (`*`) and decide
  // every one as local (`Target::Actor`) so each program is traced and
  // recorded with the commit.
  let guest_config = GuestConfig {
    run: session_id.clone(),
    user: GuestUser {
      name: root_user.name.clone(),
      uid: root_user.uid,
      gid: root_user.gid,
    },
    route_names: vec!["*".into()],
    hostname: format!("needy-{session_id}"),
    machine_id: String::new(),
    uptime_offset_secs: 0,
    mounts: Vec::new(),
    command: vec!["/bin/sh".into()],
    env: resolved.plan.env.into_iter().collect(),
    cwd: "/".into(),
    gateway_port: channels.gateway_port,
    sensors: Vec::new(),
    copy_in: Vec::new(),
    mirror_out: Vec::new(),
    sensor_faults: Vec::new(),
    tty: Some(request.tty.unwrap_or(WindowSize { rows: 24, cols: 80 })),
    process_port: channels.process_port,
    wait_for_attach: request.wait_for_attach,
    daemon_uid: daemon_uid(),
    daemon_gid: daemon_gid(),
    layers: if is_container_builder {
      Vec::new()
    } else {
      resolved.layer_disks.iter().map(|disk| disk.digest.to_string()).collect()
    },
    image_path: resolved.plan.path,
    session: GuestSession {
      upper: UpperLayer::Kept,
      routing: true,
    },
    // Lifecycle hooks run only for normal runs, never in edit sessions.
    start_hooks: Vec::new(),
    stop_hooks: Vec::new(),
  };

  let guest_service = Arc::new(SessionGuest {
    config: guest_config,
    copy_in_archives: HashMap::new(),
    expected_mirror_out: HashSet::new(),
    mirror_out_archives: Mutex::default(),
    dir: dir.clone(),
    trace: trace.clone(),
    exit: Mutex::new(None),
    cancel: session_cancel.clone(),
  });
  let runs_service = Arc::new(EditSessionRuns {
    commands: commands.clone(),
    trace: trace.clone(),
  });

  let (guest_sock, gateway_sock) = if is_container_builder {
    let driver::Endpoint::Unix(g) = &channels.guest;
    let driver::Endpoint::Unix(gw) = &channels.gateway;
    if let Some(parent) = g.parent() {
      needy_api_server::create_private_dir(parent).with_context(|| format!("cannot create {}", parent.display())).map_err(crate::failed)?;
    }
    (g.clone(), gw.clone())
  } else {
    (dir.join("cu.sock"), dir.join("cg.sock"))
  };

  // Serve GuestService AND RunService (for Decide).
  let health = Health {
    instance_id: uuid::Uuid::new_v4().to_string(),
    role: "control",
  };
  let caller = Caller::Actor {
    run: session_id.clone(),
  };
  let guest_listener = Server::new(health, caller)
    .guest(guest_service.clone())
    .runs(runs_service)
    .bind(&[Listen::Unix(guest_sock.clone())])
    .await
    .map_err(|e| crate::failed(e.into()))?;
  if is_container_builder {
    open_socket_permissions(&guest_sock).map_err(crate::failed)?;
  }
  let stop_guest = session_cancel.clone();
  let id_for_log = session_id.clone();
  tasks.spawn(async move {
    if let Err(e) = guest_listener.serve(stop_guest).await {
      tracing::error!("edit session {id_for_log}: guest listener: {e:#}");
    }
  });

  let network = Policy {
    allow: vec![Glob::new("*")],
    services: BTreeMap::new(),
  };
  if let Err(e) = serve_session_gateway(&gateway_sock, network, trace, session_cancel.clone(), &tasks, is_container_builder) {
    session_cancel.cancel();
    tasks.close();
    tasks.wait().await;
    cleanup_session_dir(&dir);
    return Err(crate::failed(e));
  }

  enum EditRuntime {
    Vm(Box<dyn driver::Running>),
    Container {
      container: needy_core_actor::driver::linux_container::SessionContainer,
      engine: Arc<dyn Engine>,
    },
  }

  let mut runtime = if is_container_builder {
    let engine = daemon.images.backends().engine.clone();
    let container_driver = LinuxContainer::with_engine(OciRuntime::Runc, base_container_image.to_string(), engine.clone());
    let start_req = driver::Start {
      run: &session_id,
      dir: &dir,
      recipe: None,
      user: &root_user,
      mounts: &[],
      mounts_dir: &daemon.run_mounts_dir(&session_id),
      guest: &guest_bin,
      layer_disks: &[],
      snapshot_image: resolved.snapshot_image.as_deref(),
    };
    match container_driver.start_session(&start_req).await {
      Ok(container) => EditRuntime::Container { container, engine },
      Err(e) => {
        session_cancel.cancel();
        tasks.close();
        tasks.wait().await;
        cleanup_session_dir(&dir);
        return Err(crate::failed(e.into()));
      }
    }
  } else {
    let layer_disks = resolved.layer_disks.into_iter().map(|d| d.path).collect();
    match daemon
      .holder
      .start(crate::holder::Actor {
        run: session_id.clone(),
        dir: dir.clone(),
        host: actor::host(daemon),
        profile_yaml,
        recipe: None,
        mounts: Vec::new(),
        mounts_dir: daemon.run_mounts_dir(&session_id),
        guest: guest_bin,
        layer_disks,
        snapshot_image: None,
      })
      .await
    {
      Ok(vm) => EditRuntime::Vm(vm),
      Err(e) => {
        session_cancel.cancel();
        tasks.close();
        tasks.wait().await;
        cleanup_session_dir(&dir);
        return Err(crate::failed(e));
      }
    }
  };

  let (result_tx, result_rx) = watch::channel(None);
  let aborted = Arc::new(AtomicBool::new(false));
  let session = Arc::new(EditSession {
    id: session_id.clone(),
    snapshot: resolved.snapshot.digest,
    channels,
    cancel: session_cancel.clone(),
    aborted: aborted.clone(),
    terminal: tokio::sync::OnceCell::new(),
    result_rx,
  });
  lock(&daemon.images.edits).insert(session_id.clone(), session);

  let store = daemon.images.store.clone();
  let image_name = request.image_name;
  let id_for_task = session_id.clone();
  daemon.tasks.spawn(async move {
    let _lock_guard = lock_guard;
    let waited = match &mut runtime {
      EditRuntime::Vm(vm) => tokio::select! {
        waited = vm.wait() => Some(waited),
        () = session_cancel.cancelled() => {
          if let Err(e) = vm.stop(SESSION_STOP_GRACE).await {
            tracing::warn!("edit session {id_for_task}: cannot stop VM {}: {e}", vm.pid());
          }
          None
        }
      },
      EditRuntime::Container { container, .. } => tokio::select! {
        waited = container.wait() => Some(waited),
        () = session_cancel.cancelled() => {
          if let Err(e) = container.stop(SESSION_STOP_GRACE).await {
            tracing::warn!("edit session {id_for_task}: cannot stop container {}: {e}", container.pid());
          }
          None
        }
      },
    };

    session_cancel.cancel();
    tasks.close();
    tasks.wait().await;

    let recorded: Vec<StoreCommand> = lock(&commands).clone();
    let recorded_out: Vec<RecordedCommand> = recorded
      .iter()
      .map(|c| RecordedCommand {
        argv: c.argv.clone(),
        cwd: c.cwd.clone(),
      })
      .collect();

    let outcome = if aborted.load(Ordering::SeqCst) {
      if let EditRuntime::Container { container, .. } = &runtime {
        container.remove().await;
      }
      Ok(ImageEditFinished {
        commit: None,
        commands: recorded_out,
        exit: None,
      })
    } else if let Some(Err(e)) = waited {
      if let EditRuntime::Container { container, .. } = &runtime {
        container.remove().await;
      }
      Err(format!("waiting for edit session {id_for_task} failed: {e}"))
    } else {
      let exit = guest_service.exit();
      let succeeded = matches!(&exit, Some(Exit { code: Some(0), signal }) if signal.is_empty());
      match &runtime {
        EditRuntime::Vm(_) => {
          let upper = dir.join(UPPER_LAYER);
          if succeeded && upper.exists() {
            let committed = tokio::task::spawn_blocking(move || -> Result<Digest> {
              let file = std::fs::File::open(&upper).with_context(|| format!("cannot open {}", upper.display()))?;
              Ok(store.commit(&image_name, parent_layer.as_ref(), Some(&base_top), file, recorded)?)
            })
            .await;
            finish_commit(committed, &id_for_task, base_top, recorded_out, exit)
          } else {
            Ok(ImageEditFinished {
              commit: None,
              commands: recorded_out,
              exit,
            })
          }
        }
        EditRuntime::Container { container, engine } => {
          if succeeded {
            let exported = commit_container_to_file(engine, container.container_name(), &dir).await;
            container.remove().await;
            match exported {
              Ok(committed_tar) => {
                let committed = tokio::task::spawn_blocking(move || -> Result<Digest> {
                  let top = extract_top_layer(&committed_tar)?;
                  Ok(store.commit(&image_name, parent_layer.as_ref(), Some(&base_top), top, recorded)?)
                })
                .await;
                finish_commit(committed, &id_for_task, base_top, recorded_out, exit)
              }
              Err(e) => Err(format!("committing edit session {id_for_task}: {e:#}")),
            }
          } else {
            container.remove().await;
            Ok(ImageEditFinished {
              commit: None,
              commands: recorded_out,
              exit,
            })
          }
        }
      }
    };

    cleanup_session_dir(&dir);
    let _ = result_tx.send(Some(outcome));
  });

  Ok(ImageEditStarted {
    session_id,
    commit_rebased,
    resolved_features,
  })
}

pub(super) async fn finish_edit(daemon: &Daemon, session_id: &str, abort: bool) -> Result<ImageEditFinished, ControlError> {
  let session =
    lock(&daemon.images.edits).remove(session_id).ok_or_else(|| ControlError::NotFound(format!("no active edit session {session_id}")))?;
  if abort {
    session.aborted.store(true, Ordering::SeqCst);
    session.cancel.cancel();
  }
  let mut rx = session.result_rx.clone();
  let result = rx
    .wait_for(Option::is_some)
    .await
    .map_err(|_| ControlError::Internal(format!("edit session {session_id} ended without a result")))?
    .clone()
    .expect("wait_for guarantees Some");
  result.map_err(ControlError::Failed)
}

pub(super) async fn rollback(daemon: &Daemon, request: ImageRollback) -> Result<ImageRollbackResult, ControlError> {
  daemon.admit_work()?;
  if lock(&daemon.images.active_edits).contains(&request.image_name) {
    return Err(ControlError::Failed(format!("cannot roll back image {}: an edit session is running", request.image_name)));
  }
  let from: Digest =
    request.from_layer.parse().map_err(|e| ControlError::Invalid(format!("invalid commit layer {}: {e}", request.from_layer)))?;
  let store = daemon.images.store.clone();
  let image_name = request.image_name;
  let steps = request.steps as usize;
  let target = tokio::task::spawn_blocking(move || store.rollback(&image_name, &from, steps))
    .await
    .map_err(|e| ControlError::Internal(format!("rollback panicked: {e}")))?
    .map_err(|e| match e {
      StoreError::UnknownCommit { .. } | StoreError::Missing(_) => ControlError::NotFound(e.to_string()),
      StoreError::TooFar { .. } | StoreError::Invalid(_) => ControlError::Invalid(e.to_string()),
      StoreError::Io { .. } | StoreError::Unexpected { .. } => ControlError::Failed(e.to_string()),
    })?;
  Ok(ImageRollbackResult {
    layer: target.as_ref().map(|c| c.layer.to_string()),
    base: target.and_then(|c| c.base).map(|d| d.to_string()),
  })
}

pub(super) async fn log(daemon: &Daemon, request: ImageLog) -> Result<ImageLogResult, ControlError> {
  daemon.admit_work()?;
  let store = daemon.images.store.clone();
  let image_name = request.image_name;
  let commits = tokio::task::spawn_blocking(move || store.history(&image_name))
    .await
    .map_err(|e| ControlError::Internal(format!("image log panicked: {e}")))?
    .map_err(|e| match e {
      StoreError::Invalid(_) => ControlError::Invalid(e.to_string()),
      StoreError::Missing(_) | StoreError::UnknownCommit { .. } => ControlError::NotFound(e.to_string()),
      StoreError::TooFar { .. } | StoreError::Io { .. } | StoreError::Unexpected { .. } => ControlError::Failed(e.to_string()),
    })?;
  let current = request.current_layer.as_deref();
  let entries = commits
    .into_iter()
    .map(|c| {
      let layer = c.layer.to_string();
      let active = current == Some(layer.as_str());
      CommitHistoryEntry {
        layer,
        parent: c.parent.map(|d| d.to_string()),
        base: c.base.map(|d| d.to_string()),
        commands: c
          .commands
          .into_iter()
          .map(|cmd| RecordedCommand {
            argv: cmd.argv,
            cwd: cmd.cwd,
          })
          .collect(),
        time_ms: c.time_ms,
        active,
      }
    })
    .collect();
  Ok(ImageLogResult { commits: entries })
}

fn finish_commit(
  committed: std::result::Result<Result<Digest>, tokio::task::JoinError>,
  session_id: &str,
  base_top: Digest,
  commands: Vec<RecordedCommand>,
  exit: Option<Exit>,
) -> EditOutcomeResult {
  match committed {
    Ok(Ok(layer)) => Ok(ImageEditFinished {
      commit: Some(PlanCommit {
        layer,
        base: base_top,
      }),
      commands,
      exit,
    }),
    Ok(Err(e)) => Err(format!("committing edit session {session_id}: {e:#}")),
    Err(e) => Err(format!("committing edit session {session_id} panicked: {e}")),
  }
}
