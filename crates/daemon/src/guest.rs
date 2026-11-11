//! The actor's side of its run, through `api-server`'s `Guest` trait:
//! the configuration its init asks for at boot, the events it observes, the
//! driving program's exit, and the archives of its copy-in and mirror-out
//! mappings.

use needy_api_server::control::{ControlError, Guest, GuestConfig, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestUser};
use needy_common::process::Exit;
use needy_config::profile::{FaultKind, FileMode, Side};
use needy_core::event::{ActorConfig, ActorExit, Event};
use needy_core::rules;
use needy_core::types::RunState;

use crate::run::Run;
use crate::{Daemon, actor, files};

/// The workspace mounts of `run`: executor directories mounted into the
/// actor over SFTP.
fn mounts(run: &Run) -> Vec<GuestMount> {
  let mut mounts = Vec::new();
  for (name, mapping) in &run.profile.files.mappings {
    if !files::selected(run, name) || mapping.mode != FileMode::Mount {
      continue;
    }
    let (Side::Executor { name: exec, path }, Side::Actor(target)) = (&mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = run.executor(exec) else {
      continue;
    };
    mounts.push(GuestMount {
      target: target.clone(),
      executor: exec.clone(),
      source: path.clone(),
      writable: mapping.writable,
      sftp_command: instance.handle.sftp.clone(),
    });
  }
  mounts
}

/// The driving program's working directory when the profile sets no
/// `actor.cwd`: `~/workspace` when a workspace mount provides it, else the
/// home directory. Init mounts a fresh home, so `~/workspace` exists only
/// when a mount at or below it creates it.
fn default_cwd(home: &str, mounts: &[GuestMount]) -> String {
  let workspace = format!("{home}/workspace");
  let provided = mounts.iter().any(|m| {
    let target = m.target.trim_end_matches('/');
    target == workspace || target.starts_with(&format!("{workspace}/"))
  });
  if provided {
    workspace
  } else {
    home.to_string()
  }
}

#[async_trait::async_trait]
impl Guest for Daemon {
  async fn config(&self, run: &str) -> Result<GuestConfig, ControlError> {
    let run = self.runs.find(run)?;
    let channels = actor::channels(self, &run).map_err(|e| ControlError::Failed(format!("run {}: {e:#}", run.id)))?;
    let profile = &run.profile;
    let user = &profile.user;
    let home = format!("/home/{}", user.name);
    let mounts = mounts(&run);
    let cwd = profile.actor.cwd.clone().unwrap_or_else(|| default_cwd(&home, &mounts));
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
    let hwmon_faults = run
      .injector
      .faults()
      .iter()
      .filter(|f| f.on == FaultKind::Hwmon)
      .map(|f| GuestFault {
        at_ms: f.at.map(|d| d.as_millis() as i64).unwrap_or(0),
        temp_mc: f.set.temp_c.map(|c| (c * 1000.0) as i32).unwrap_or(100_000),
      })
      .collect();
    let recipe = run.recipe.as_ref();
    let routed = rules::route_names(profile);
    run.emit(&Event::ActorConfig(ActorConfig {
      route_names: routed.clone(),
      mounts: mounts.iter().map(|m| m.target.clone()).collect(),
    }));
    // `*` asks the kernel to route every exec.
    let route_names = routed.unwrap_or_else(|| vec!["*".into()]);
    Ok(GuestConfig {
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
      hwmon_faults,
      tty: run.tty,
      process_port: channels.process_port,
      wait_for_attach: run.wait_for_attach,
    })
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
}

#[cfg(test)]
#[path = "guest_test.rs"]
mod tests;
