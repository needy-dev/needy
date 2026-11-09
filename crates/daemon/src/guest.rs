//! The actor's side of its session, through `api-server`'s `Guest` trait:
//! the configuration its init asks for at boot, the events it observes, the
//! driving program's exit, and the archives of its copy-in and mirror-out
//! mappings.

use needy_api_server::control::{ControlError, Guest, GuestConfig, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestUser};
use needy_common::process::Exit;
use needy_config::profile::{FaultKind, FileMode, Side};
use needy_core::event::{ActorConfig, ActorExit, Event};
use needy_core::rules;
use needy_core::types::SessionState;

use crate::actor::GATEWAY_PORT;
use crate::session::Session;
use crate::{Daemon, files};

/// The workspace mounts of `session`: executor directories mounted into the
/// actor over SFTP.
fn mounts(session: &Session) -> Vec<GuestMount> {
  let mut mounts = Vec::new();
  for (name, mapping) in &session.profile.files.mappings {
    if !files::selected(session, name) || mapping.mode != FileMode::Mount {
      continue;
    }
    let (Side::Executor { name: exec, path }, Side::Actor(target)) = (&mapping.from, &mapping.to) else {
      continue;
    };
    let Some(instance) = session.executor(exec) else {
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

#[async_trait::async_trait]
impl Guest for Daemon {
  async fn config(&self, session: &str) -> Result<GuestConfig, ControlError> {
    let session = self.sessions.find(session)?;
    let profile = &session.profile;
    let user = &profile.user;
    let home = format!("/home/{}", user.name);
    let mounts = mounts(&session);
    let mut copy_in = Vec::new();
    let mut mirror_out = Vec::new();
    for (name, mapping) in &profile.files.mappings {
      if !files::selected(&session, name) {
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
    let hwmon_faults = session
      .injector
      .faults()
      .iter()
      .filter(|f| f.on == FaultKind::Hwmon)
      .map(|f| GuestFault {
        at_ms: f.at.map(|d| d.as_millis() as i64).unwrap_or(0),
        temp_mc: f.set.temp_c.map(|c| (c * 1000.0) as i32).unwrap_or(100_000),
      })
      .collect();
    let recipe = session.recipe.as_ref();
    let routed = rules::route_names(profile);
    session.emit(&Event::ActorConfig(ActorConfig {
      route_names: routed.clone(),
      mounts: mounts.iter().map(|m| m.target.clone()).collect(),
    }));
    // `*` asks the kernel to route every exec.
    let route_names = routed.unwrap_or_else(|| vec!["*".into()]);
    Ok(GuestConfig {
      session: session.id.clone(),
      user: GuestUser {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
      },
      route_names,
      hostname: recipe.and_then(|r| r.hostname.clone()).unwrap_or_default(),
      uptime_offset_secs: recipe.map(|r| r.uptime_offset_secs).unwrap_or(0),
      mounts,
      command: session.actor_command.clone(),
      env: profile.actor.env.clone().into_iter().collect(),
      cwd: profile.actor.cwd.clone().unwrap_or_else(|| format!("{home}/workspace")),
      gateway_port: GATEWAY_PORT,
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
    })
  }

  async fn event(&self, session: &str, time_ms: i64, event: Event) -> Result<(), ControlError> {
    let session = self.sessions.find(session)?;
    session.trace.emit_at(time_ms, event.kind().as_str(), event.raw());
    Ok(())
  }

  async fn exit(&self, session: &str, exit: Exit) -> Result<(), ControlError> {
    let session = self.sessions.find(session)?;
    session.emit(&Event::ActorExit(ActorExit {
      code: exit.code,
      signal: (!exit.signal.is_empty()).then_some(exit.signal),
    }));
    if let Err(refused) = session.state.advance(SessionState::Finished) {
      tracing::debug!("session {}: {refused}", session.id);
    }
    Ok(())
  }

  async fn fetch_files(&self, session: &str, name: &str) -> Result<Vec<u8>, ControlError> {
    let session = self.sessions.find(session)?;
    files::fetch(&session, name).await
  }

  async fn push_files(&self, session: &str, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
    let session = self.sessions.find(session)?;
    files::push(&session, name, archive).await
  }
}
