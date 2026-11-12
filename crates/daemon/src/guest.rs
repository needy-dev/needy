//! The actor's side of its run, through `api-server`'s `Guest` trait:
//! the configuration its init asks for at boot, the events it observes, the
//! driving program's exit, and the archives of its copy-in and mirror-out
//! mappings.

use needy_api_server::control::{ControlError, Guest, GuestConfig, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestUser};
use needy_common::process::Exit;
use needy_config::profile::{FaultDomain, FileMode, Side};
use needy_core::event::{ActorConfig, ActorExit, Event};
use needy_core::rules;
use needy_core::types::{ActorState, RunState};
use needy_core_actor::driver::Side as Presenter;

use crate::run::Run;
use crate::{Daemon, actor, files};

/// The workspace mounts of `run` as its actor's init is told them.
fn mounts(run: &Run) -> Vec<GuestMount> {
  actor::mounts(run)
    .into_iter()
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
    let mounts = mounts(&run);
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
    let routed = rules::route_names(profile);
    actor::advance(&run, ActorState::Configured);
    run.emit(&Event::ActorConfig(ActorConfig {
      route_names: routed.clone(),
      mounts: mounts.iter().map(|m| m.target.clone()).collect(),
    }));
    // `*` asks the kernel to route every exec.
    let route_names = routed.unwrap_or_else(|| vec!["*".into()]);
    // Init is told only what its driver presents in the guest: a driver
    // that presents something from the host (the workspace mounts, the
    // hostname, the sensors and their faults of a linux-container actor)
    // made it itself, and what a driver does not present at all (Apple's
    // `container`'s sensors and uptime offset) is a gap `profile check`
    // reports.
    let mut config = GuestConfig {
      run: run.id.clone(),
      user: GuestUser {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
      },
      route_names,
      hostname: recipe.and_then(|r| r.hostname.clone()).unwrap_or_default(),
      uptime_offset_secs: recipe.map(|r| r.uptime_offset_secs).unwrap_or(0),
      mounts,
      command: run.actor_command.clone(),
      env: profile.actor.env.clone().into_iter().collect(),
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
    run.set_actor_exit(exit.clone());
    run.emit(&Event::ActorExit(ActorExit {
      code: exit.code,
      signal: (!exit.signal.is_empty()).then_some(exit.signal),
    }));
    if let Err(refused) = run.state.advance(RunState::Finished) {
      tracing::debug!("run {}: {refused}", run.id);
    }
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

  async fn wait_stop(&self, run: &str) -> Result<std::time::Duration, ControlError> {
    let run = self.runs.find(run)?;
    // An actor that exits, or a run whose stop kills it, ends the wait: the
    // guest listener's shutdown waits for its calls.
    tokio::select! {
      grace = run.stop_request.wait() => Ok(grace),
      () = run.actor.exited() => Err(ControlError::Unavailable(format!("the actor of run {} exited", run.id))),
      () = run.cancel.cancelled() => Err(ControlError::Unavailable(format!("run {} is stopping", run.id))),
    }
  }
}

#[cfg(test)]
#[path = "guest_test.rs"]
mod tests;
