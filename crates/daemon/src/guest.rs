//! The actor's side of its run, through `api-server`'s `Guest` trait:
//! the configuration its init asks for at boot, the events it observes, the
//! driving program's exit, the archives of its copy-in and mirror-out
//! mappings, and the upper layer a session keeps.

use futures_util::StreamExt as _;
use needy_api_server::control::{
  Chunks, ControlError, Guest, GuestConfig, GuestControl, GuestControls, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestUser,
  UpperLayer,
};
use needy_common::process::Exit;
use needy_config::profile::{FaultDomain, FileMode, Side};
use needy_core::event::{ActorConfig, ActorExit, Event};
use needy_core::types::ActorState;
use needy_core_actor::driver::Side as Presenter;
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
      layers: if boot.snapshot_image.is_some() {
        Vec::new()
      } else if !boot.layer_disks.is_empty() {
        boot.layer_disks.iter().map(|disk| disk.digest.to_string()).collect()
      } else {
        boot.snapshot.map(|s| s.layers.iter().map(ToString::to_string).collect()).unwrap_or_default()
      },
      image_path: boot.path,
      session: boot.session,
    };
    let in_guest = Some(Presenter::Guest);
    if facets.workspace != Presenter::Guest {
      config.mounts.clear();
    }
    if facets.identity.hostname != in_guest {
      config.hostname.clear();
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
    run.trace.emit_at(time_ms, event.kind().as_str(), event.raw());
    Ok(())
  }

  async fn exit(&self, run: &str, exit: Exit) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
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
    files::fetch(&run, name).await
  }

  async fn push_files(&self, run: &str, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    files::push(&run, name, archive).await
  }

  /// Keeps the session's upper layer as `upper.tar` in the run's directory
  /// for the layer store, written as it arrives.
  async fn push_layer(&self, run: &str, archive: Chunks) -> Result<(), ControlError> {
    let run = self.runs.find(run)?;
    if run.boot().session.upper != UpperLayer::Kept {
      return Err(ControlError::Failed(format!("run {}'s session keeps no upper layer", run.id)));
    }
    // TODO(images): slice 1, step 7 puts the archive in the layer store
    // (`needy_core_actor::image::store::Store::put` or `commit`) when the
    // session ends, and removes this file.
    keep_upper(&run.dir.join(UPPER_LAYER), archive)
      .await
      .map_err(|e| ControlError::Failed(format!("run {}: keeping the upper layer: {e:#}", run.id)))
  }

  /// The route list now and each change, and a graceful stop once `run
  /// stop` asks for one, until the run or its actor ends: init holds it for
  /// the actor's life.
  // NOTICE: the stream ends when the run is cancelled or the actor exits, so
  // a stopping guest listener does not wait its grace period for it.
  async fn control(&self, run: &str) -> Result<GuestControls, ControlError> {
    let run = self.runs.find(run)?;
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
