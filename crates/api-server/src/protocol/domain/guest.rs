//! Proto <-> domain mappings for `GuestService`.

use needy_api_proto::v1;

use crate::control::{GuestConfig, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestSession, GuestUser, UpperLayer};

impl From<GuestUser> for v1::GuestUser {
  fn from(user: GuestUser) -> Self {
    Self {
      name: user.name,
      uid: user.uid,
      gid: user.gid,
    }
  }
}

impl From<GuestMount> for v1::GuestMount {
  fn from(m: GuestMount) -> Self {
    Self {
      target: m.target,
      executor: m.executor,
      source: m.source,
      writable: m.writable,
      sftp_command: m.sftp_command,
    }
  }
}

impl From<GuestSensor> for v1::GuestSensor {
  fn from(s: GuestSensor) -> Self {
    Self {
      chip: s.chip,
      idle_mc: s.idle_mc,
    }
  }
}

impl From<GuestCopy> for v1::GuestCopy {
  fn from(c: GuestCopy) -> Self {
    Self {
      name: c.name,
      path: c.path,
    }
  }
}

impl From<GuestFault> for v1::GuestFault {
  fn from(f: GuestFault) -> Self {
    Self {
      at_ms: f.at_ms,
      temp_mc: f.temp_mc,
      name: f.name,
    }
  }
}

impl From<GuestSession> for v1::GuestSession {
  fn from(session: GuestSession) -> Self {
    Self {
      upper: match session.upper {
        UpperLayer::Disposable => v1::UpperLayer::Disposable,
        UpperLayer::Kept => v1::UpperLayer::Kept,
      }
      .into(),
      routing: session.routing,
    }
  }
}

impl From<GuestConfig> for v1::GetConfigResponse {
  fn from(config: GuestConfig) -> Self {
    Self {
      tty: config.tty.map(Into::into),
      process_port: config.process_port,
      wait_for_attach: config.wait_for_attach,
      daemon_uid: config.daemon_uid,
      daemon_gid: config.daemon_gid,
      run_id: config.run,
      user: Some(config.user.into()),
      route_names: config.route_names,
      hostname: config.hostname,
      uptime_offset_secs: config.uptime_offset_secs,
      mounts: config.mounts.into_iter().map(Into::into).collect(),
      command: config.command,
      env: config.env,
      cwd: config.cwd,
      gateway_port: config.gateway_port,
      sensors: config.sensors.into_iter().map(Into::into).collect(),
      copy_in: config.copy_in.into_iter().map(Into::into).collect(),
      mirror_out: config.mirror_out.into_iter().map(Into::into).collect(),
      sensor_faults: config.sensor_faults.into_iter().map(Into::into).collect(),
      layers: config.layers.into_iter().map(|digest| v1::GuestLayer { digest }).collect(),
      image_path: config.image_path,
      session: Some(config.session.into()),
      start_hooks: config.start_hooks,
      stop_hooks: config.stop_hooks,
    }
  }
}

pub fn proto_guest_config(config: GuestConfig) -> v1::GetConfigResponse {
  config.into()
}
