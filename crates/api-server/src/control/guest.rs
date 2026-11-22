//! Domain types and trait for `GuestService`.

use std::collections::HashMap;
use std::pin::Pin;

use needy_common::process::Exit;
use needy_core::event::Event;
use tokio_stream::Stream;

use super::{Result, WindowSize};

/// The actor's configuration, which its init asks for at boot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestConfig {
  pub run: String,
  pub user: GuestUser,
  /// Command names the kernel routes; `*` routes every exec.
  pub route_names: Vec<String>,
  pub hostname: String,
  pub uptime_offset_secs: i64,
  pub mounts: Vec<GuestMount>,
  /// The driving program.
  pub command: Vec<String>,
  pub env: HashMap<String, String>,
  pub cwd: String,
  pub gateway_port: u32,
  pub sensors: Vec<GuestSensor>,
  pub copy_in: Vec<GuestCopy>,
  pub mirror_out: Vec<GuestCopy>,
  pub sensor_faults: Vec<GuestFault>,
  /// The driving program's terminal; `None` for none.
  pub tty: Option<WindowSize>,
  /// The vsock port the guest serves `ProcessService` on when the driving
  /// program has a terminal.
  pub process_port: u32,
  /// Subscribe the first terminal client before starting the program.
  pub wait_for_attach: bool,
  /// The user and group the daemon runs as, who connect to the process
  /// service a container actor's init binds on a Unix socket.
  pub daemon_uid: u32,
  pub daemon_gid: u32,
  /// The digests (`sha256:<hex>`) of the layers init stacks over the base,
  /// lowest first; each layer disk's marker names one. Empty boots the
  /// base alone.
  pub layers: Vec<String>,
  /// The image's `PATH`, kept out of [`GuestConfig::env`]: it goes between
  /// the trampolines and the route stubs rather than replacing the driving
  /// program's `PATH`. Empty for none.
  pub image_path: String,
  pub session: GuestSession,
  /// Shell commands run in order as the run's user in `cwd` after setup and
  /// routing are enabled, before the driving program starts. Empty for build
  /// and edit sessions.
  pub start_hooks: Vec<String>,
  /// Shell commands run in order as the run's user in `cwd` after the driving
  /// program exits (or on graceful stop), before mirror-out and sync merges.
  /// Empty for build and edit sessions.
  pub stop_hooks: Vec<String>,
}

/// The settings of the session an actor is: a run, a build step or an edit
/// session (provisional). The default is a run's.
///
/// NOTICE: a session's ending (commit or discard) is the daemon's, not the
/// guest's: the guest streams a kept upper layer either way (see
/// `GuestSession` in `proto/needy/v1/guest.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestSession {
  pub upper: UpperLayer,
  /// Off: init hands no exec to the proxy, whatever the route list says.
  pub routing: bool,
}

impl Default for GuestSession {
  fn default() -> Self {
    Self {
      upper: UpperLayer::Disposable,
      routing: true,
    }
  }
}

/// What happens to a session's upper layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpperLayer {
  /// Gone with the actor.
  #[default]
  Disposable,
  /// Archived and streamed with [`Guest::push_layer`] when the session
  /// ends.
  Kept,
}

/// The parts of an archive as the guest streams them; an error ends the
/// stream.
pub type Chunks = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestUser {
  pub name: String,
  pub uid: u32,
  pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMount {
  pub target: String,
  pub executor: String,
  pub source: String,
  pub writable: bool,
  pub sftp_command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSensor {
  pub chip: String,
  pub idle_mc: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestCopy {
  pub name: String,
  pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFault {
  /// The fault's name.
  pub name: String,
  pub at_ms: i64,
  pub temp_mc: i32,
}

/// What the daemon tells a running actor's init (`GuestService.Control`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestControl {
  /// The route list from now on, as [`GuestConfig::route_names`].
  Routes(Vec<String>),
  /// `run stop` asks the actor to end gracefully: the driving program gets
  /// SIGTERM, then SIGKILL after `grace`.
  Stop { grace: std::time::Duration },
}

/// The messages of [`Guest::control`], until the run ends.
pub type GuestControls = Pin<Box<dyn Stream<Item = GuestControl> + Send>>;

/// The actor's side of its run. Every call names the run the
/// actor's listener is bound to.
#[async_trait::async_trait]
pub trait Guest: Send + Sync + 'static {
  async fn config(&self, run: &str) -> Result<GuestConfig>;
  /// An event observed inside the actor, at `time_ms`.
  async fn event(&self, run: &str, time_ms: i64, event: Event) -> Result<()>;
  /// The driving program's exit.
  async fn exit(&self, run: &str, exit: Exit) -> Result<()>;
  /// A copy-in mapping's archive.
  async fn fetch_files(&self, run: &str, name: &str) -> Result<Vec<u8>>;
  /// A mirror-out mapping's archive.
  async fn push_files(&self, run: &str, name: &str, archive: Vec<u8>) -> Result<()>;
  /// The session's upper layer, a tar archive streamed as it is read, for
  /// a session whose upper layer is [`UpperLayer::Kept`].
  async fn push_layer(&self, run: &str, archive: Chunks) -> Result<()>;
  /// What init is told while the actor runs: the route list now and at
  /// each change, and a graceful stop when `run stop` asks for one. Init
  /// holds it for the actor's life; it ends when the run or the actor does.
  async fn control(&self, run: &str) -> Result<GuestControls>;
}
