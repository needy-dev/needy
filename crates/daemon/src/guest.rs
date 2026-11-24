//! The actor's side of its run, through `api-server`'s `Guest` trait:
//! the configuration its init asks for at boot, the events it observes, the
//! driving program's exit, the archives of its copy-in and mirror-out
//! mappings, and the upper layer a session keeps.

use std::time::Duration;

use futures_util::StreamExt as _;
use needy_api_server::control::{
  Chunks, ControlError, FromRunner, Guest, GuestConfig, GuestControl, GuestControls, GuestCopy, GuestFault, GuestMount, GuestSensor,
  GuestUser, ToRunner,
};
use needy_common::process::Exit;
use needy_config::profile::{FaultDomain, FileMode, Side};
use needy_core::event::{ActorConfig, ActorExit, Event};
use needy_core::types::ActorState;
use needy_core_actor::driver::Side as Presenter;
use needy_core_actor::image::store::Digest;
use tokio_stream::wrappers::WatchStream;

use crate::run::{Run, route_list};
use crate::{Daemon, actor, files};

/// The workspace mounts of `run` and its per-run copies, as its actor's
/// init is told them.
fn mounts(daemon: &Daemon, run: &Run) -> Vec<GuestMount> {
  actor::mounts(run)
    .into_iter()
    .chain(crate::sync::mounts(daemon, run))
    .map(|m| GuestMount {
      target: m.target,
      executor: m.executor,
      source: m.source,
      writable: m.writable,
      sftp_command: m.sftp_command,
    })
    .collect()
}

/// How long `Guest::config` waits for a remote actor's runner to report the
/// layer disks it made (`BridgePrepared`).
// NOTICE: the runner reports them on the bridge before it starts the VM
// whose init asks for its configuration on another stream, so the wait only
// covers that race between the two streams, not the layers' fetch.
const REMOTE_LAYERS_WAIT: Duration = Duration::from_secs(30);

/// The layer disks a remote actor's runner made of `run`'s snapshot, once
/// it reported them, lowest first.
// NOTICE: open question 1 of `2026-11-25-remote-actor-images-spec.md` is
// settled this way: `GetConfig` waits for `BridgePrepared` rather than the
// runner waiting for an acknowledgement, which keeps the runner's start
// sequence one-way.
async fn remote_layers(run: &Run) -> Result<Vec<String>, ControlError> {
  let mut reported = run.remote_layers.subscribe();
  match tokio::time::timeout(REMOTE_LAYERS_WAIT, reported.wait_for(Option::is_some)).await {
    Ok(Ok(layers)) => Ok(layers.clone().unwrap_or_default()),
    // The sender lives as long as the run, which the caller holds.
    Ok(Err(_)) => Err(ControlError::Unavailable(format!("run {} ended before its actor runner reported its layer disks", run.id))),
    Err(_) => Err(ControlError::Unavailable(format!(
      "run {}: its actor runner did not report the layer disks it made within {}s",
      run.id,
      REMOTE_LAYERS_WAIT.as_secs()
    ))),
  }
}

/// The user and group this daemon runs as: a container actor's init gives
/// its process socket to them, so a daemon that is not root can connect.
fn daemon_user() -> (u32, u32) {
  // SAFETY: `geteuid` and `getegid` cannot fail.
  unsafe { (libc::geteuid(), libc::getegid()) }
}

#[async_trait::async_trait]
impl Guest for Daemon {
  async fn config(&self, run: &str) -> Result<GuestConfig, ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    let channels = actor::channels(self, &run).map_err(|e| ControlError::Failed(format!("run {}: {e:#}", run.id)))?;
    let facets = actor::facets(&run);
    let profile = &run.profile;
    let user = &profile.user;
    let home = format!("/home/{}", user.name);
    let mounts = mounts(self, &run);
    // The workspace, else the home directory: init mounts a fresh home, so
    // nothing else is sure to exist.
    let cwd = profile.actor.workspace.as_ref().map_or(home, |w| w.path.clone());
    let mut copy_in = Vec::new();
    let mut mirror_out = Vec::new();
    for (name, mapping) in &profile.files.mappings {
      if !files::selected(&run, name) {
        continue;
      }
      match (mapping.mode, &mapping.from, &mapping.to) {
        (FileMode::CopyIn, Side::Host(_), Side::Actor(path)) => copy_in.push(GuestCopy {
          name: name.clone(),
          path: path.clone(),
        }),
        (FileMode::MirrorOut, Side::Actor(path), Side::Host(_)) => mirror_out.push(GuestCopy {
          name: name.clone(),
          path: path.clone(),
        }),
        _ => {}
      }
    }
    let sensor_faults = run
      .injector
      .faults(FaultDomain::Sensor)
      .into_iter()
      .map(|(name, f)| GuestFault {
        at_ms: f.at.map(|d| d.as_millis() as i64).unwrap_or(0),
        temp_mc: needy_core::chaos::sensor_temp_mc(&f),
        name,
      })
      .collect();
    let recipe = run.recipe.as_ref();
    let boot = run.boot();
    // A remote actor stacks the disks its runner made, which may flatten
    // lower layers as `Store::materialize` does here.
    let remote_layers = if run.is_remote_actor() && boot.snapshot.as_ref().is_some_and(|s| !s.layers.is_empty()) {
      Some(remote_layers(&run).await?)
    } else {
      None
    };
    // The run's exec faults now, also those `needy chaos add` added
    // before the actor asked.
    let routed = run.route_names();
    actor::advance(&run, ActorState::Configured);
    run.emit(&Event::ActorConfig(ActorConfig {
      route_names: routed.clone(),
      mounts: mounts.iter().map(|m| m.target.clone()).collect(),
    }));
    // `*` asks the kernel to route every exec.
    let route_names = route_list(routed);
    // Init is told only what its driver presents in the guest: a driver
    // that presents something from the host (the workspace mounts, the
    // hostname, the sensors and their faults of a linux-container actor)
    // made it itself, and what a driver does not present at all (Apple's
    // `container`'s sensors and uptime offset) is a gap `profile check`
    // reports.
    // Image `env` applies first; `profile.actor.env` overrides it.
    let mut env: std::collections::HashMap<String, String> = boot.env.into_iter().collect();
    env.extend(profile.actor.env.clone());
    let mut config = GuestConfig {
      run: run.id.clone(),
      user: GuestUser {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
      },
      route_names,
      // NOTICE: without a recipe hostname a VM kept its kernel's
      // CONFIG_DEFAULT_HOSTNAME, the same in every actor (`sandbox-vm` in
      // images/kernel/containerization-arm64.config, `(none)` in
      // images/kernel/firecracker-6.18.config), so programs that tell hosts
      // apart by name, such as session locks that record pid and hostname,
      // took one actor's process for another's. The run ID is unique.
      hostname: recipe.and_then(|r| r.hostname.clone()).unwrap_or_else(|| format!("needy-{}", run.id)),
      machine_id: recipe
        .and_then(|r| r.machine_id.clone())
        .or_else(|| {
          profile.actor.seed.as_ref().map(|seed| match seed {
            needy_config::profile::Seed::Int(n) => needy_common::seed::machine_id(*n),
            needy_config::profile::Seed::String(s) => needy_common::seed::machine_id(needy_common::seed::from_str(s)),
          })
        })
        .unwrap_or_default(),
      uptime_offset_secs: recipe.map(|r| r.uptime_offset_secs).unwrap_or(0),
      mounts,
      command: run.actor_command.clone(),
      env,
      cwd,
      gateway_port: channels.gateway_port,
      sensors: recipe
        .map(|r| {
          r.sensors
            .iter()
            .map(|s| GuestSensor {
              chip: s.chip.clone(),
              idle_mc: s.idle_mc,
            })
            .collect()
        })
        .unwrap_or_default(),
      copy_in,
      mirror_out,
      sensor_faults,
      tty: run.tty,
      process_port: channels.process_port,
      wait_for_attach: run.wait_for_attach,
      daemon_uid: daemon_user().0,
      daemon_gid: daemon_user().1,
      // Container snapshots are already assembled into the OCI image booted by
      // the container driver, so the guest init does not stack VM layer disks.
      // For VM drivers, use the materialized layer disk digests (which may
      // auto-flatten lower layers above `MAX_UNFLATTENED_LAYERS`) so guest
      // `init`'s `needy-layer` marker matching aligns with attached disks.
      layers: if let Some(layers) = remote_layers {
        layers
      } else if boot.snapshot_image.is_some() {
        Vec::new()
      } else if !boot.layer_disks.is_empty() {
        boot.layer_disks.iter().map(|disk| disk.digest.to_string()).collect()
      } else {
        boot.snapshot.map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default()
      },
      image_path: boot.path,
      session: boot.session,
      start_hooks: boot.start_hooks,
      stop_hooks: boot.stop_hooks,
    };
    let in_guest = Some(Presenter::Guest);
    if facets.workspace != Presenter::Guest {
      config.mounts.clear();
    }
    if facets.identity.hostname != in_guest {
      config.hostname.clear();
      config.machine_id.clear();
    }
    if facets.identity.hwmon != in_guest {
      config.sensors.clear();
      config.sensor_faults.clear();
    }
    if facets.identity.uptime != in_guest {
      config.uptime_offset_secs = 0;
    }
    Ok(config)
  }

  async fn event(&self, run: &str, time_ms: i64, event: Event) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    run.trace.emit_at(time_ms, event.kind().as_str(), event.raw());
    Ok(())
  }

  async fn exit(&self, run: &str, exit: Exit) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    if run.actor_exit().as_ref() == Some(&exit) {
      return Ok(());
    }
    run.set_actor_exit(exit.clone());
    // The guest may retry an idempotent exit report across control loss.
    // Persist before acknowledging: VM shutdown can follow immediately.
    self.save_run(&run).await;
    run.emit(&Event::ActorExit(ActorExit {
      code: exit.code,
      signal: (!exit.signal.is_empty()).then_some(exit.signal),
    }));
    // The run finishes once its actor exited (`actor::watch`), not here:
    // init still mirrors out and shuts down, and an ended run's actor is
    // exited.
    Ok(())
  }

  async fn fetch_files(&self, run: &str, name: &str) -> Result<Vec<u8>, ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    files::fetch(&run, name, &files::HostFilesPolicy::from_config(&self.config)).await
  }

  async fn push_files(&self, run: &str, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    files::push(&run, name, archive, &files::HostFilesPolicy::from_config(&self.config)).await
  }

  /// Streams a layer of the run's snapshot to its remote actor runner.
  /// Only a layer the snapshot names is served, so a runner's credential
  /// bound to one run reaches no other run's layers.
  async fn fetch_layer(&self, run: &str, digest: &str) -> Result<Chunks, ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    let digest: Digest = digest.parse().map_err(|e| ControlError::Invalid(format!("invalid layer digest {digest:?}: {e}")))?;
    if !run.boot().snapshot.is_some_and(|snapshot| snapshot.layers.contains(&digest)) {
      return Err(ControlError::denied(format!("layer {digest} is not in run {}'s image", run.id)));
    }
    let store = self.images.store.clone();
    let file = tokio::task::spawn_blocking(move || store.open_layer(&digest))
      .await
      .map_err(|e| ControlError::Internal(format!("opening layer {digest} panicked: {e}")))?
      .map_err(|e| ControlError::Failed(e.to_string()))?;
    Ok(crate::image::stream_archive(tokio::fs::File::from_std(file)))
  }

  /// Rejects upper-layer uploads on normal runs. Image build and edit sessions
  /// serve their own `Guest` implementation (`image::SessionGuest`) that
  /// consumes and stores `upper.tar`; normal runs never keep an upper layer.
  async fn push_layer(&self, run: &str, _archive: Chunks) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    Err(ControlError::Failed(format!("run {} is not an image session and cannot keep an upper layer", run.id)))
  }

  /// The route list now and each change, and a graceful stop once `run
  /// stop` asks for one, until the run or its actor ends: init holds it for
  /// the actor's life.
  // NOTICE: the stream ends when the run is cancelled or the actor exits, so
  // a stopping guest listener does not wait its grace period for it.
  async fn control(&self, run: &str) -> Result<GuestControls, ControlError> {
    let run = self.runs.find(run)?;
    run.touch_activity();
    let routes = WatchStream::new(run.routes.subscribe()).map(GuestControl::Routes);
    // Polled from the start, so the run counts its guest as waiting for a
    // stop (`StopRequest::wait`) for as long as the stream is open.
    let stop = {
      let run = run.clone();
      futures_util::stream::once(async move {
        GuestControl::Stop {
          grace: run.stop_request.wait().await,
        }
      })
    };
    let ended = async move {
      tokio::select! {
        () = run.actor.exited() => {}
        () = run.cancel.cancelled() => {}
      }
    };
    Ok(Box::pin(futures_util::stream::select(routes, stop).take_until(ended)))
  }

  async fn bridge(
    &self,
    run: &str,
    inbound: FromRunner<needy_api_client::proto::v1::BridgeRequest>,
  ) -> Result<ToRunner<needy_api_client::proto::v1::BridgeResponse>, ControlError> {
    self.attach_bridge(run, inbound).await
  }

  async fn terminal_task(
    &self,
    run: &str,
    task: u64,
    responses: FromRunner<needy_api_client::proto::v1::TerminalTaskRequest>,
  ) -> Result<ToRunner<needy_api_client::proto::v1::TerminalTaskResponse>, ControlError> {
    self.bridge_terminal_task(run, task, responses)
  }
}

/// The upper layer a session kept, in its run's directory.
pub(crate) const UPPER_LAYER: &str = "upper.tar";

/// Writes `archive` to `path` as it arrives. The file appears only once the
/// whole archive is in, so a broken upload leaves the previous one, or
/// none.
pub(crate) async fn keep_upper(path: &std::path::Path, mut archive: Chunks) -> anyhow::Result<()> {
  use tokio::io::AsyncWriteExt as _;
  let partial = path.with_extension("tar.partial");
  let mut file = tokio::fs::File::create(&partial).await?;
  let written = async {
    while let Some(part) = archive.next().await {
      file.write_all(&part?).await?;
    }
    file.sync_all().await?;
    anyhow::Ok(())
  }
  .await;
  drop(file);
  match written {
    Ok(()) => Ok(tokio::fs::rename(&partial, path).await?),
    Err(e) => {
      if let Err(removed) = tokio::fs::remove_file(&partial).await {
        tracing::warn!("cannot remove {}: {removed}", partial.display());
      }
      Err(e)
    }
  }
}

#[cfg(test)]
#[path = "guest_test.rs"]
mod tests;
