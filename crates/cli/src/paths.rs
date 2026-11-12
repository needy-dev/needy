//! Where needy keeps its files: XDG base directories through `etcetera` on
//! Linux and macOS, known folders on Windows. A `NEEDY_*` variable overrides
//! each directory. The CLI resolves them and hands them to the daemon.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use etcetera::BaseStrategy;

// TODO(config): the build cache (`$XDG_CACHE_HOME/needy`) has no reader yet.
// Add it when `needy image build` keeps a cache. The config directory holds the
// client tokens `needy pair` stores (`credentials`) and profiles by name
// (`profiles`); other user configuration waits for the config slice.
#[derive(Debug, Clone)]
pub struct Paths {
  /// Client tokens by endpoint and profiles by name:
  /// `$XDG_CONFIG_HOME/needy`, or `NEEDY_CONFIG_DIR`.
  pub config: PathBuf,
  /// `trace.db`, runs and the daemon log: `$XDG_STATE_HOME/needy`, or
  /// `NEEDY_STATE_DIR`.
  pub state: PathBuf,
  /// Images: `$XDG_DATA_HOME/needy`, or `NEEDY_DATA_DIR`.
  pub data: PathBuf,
  /// Sockets: `$XDG_RUNTIME_DIR/needy`, the state directory where there is
  /// none (macOS), or `NEEDY_RUNTIME_DIR`.
  pub runtime: PathBuf,
}

impl Paths {
  pub fn resolve() -> Result<Self> {
    let base = etcetera::choose_base_strategy().context("cannot find the home directory; set NEEDY_STATE_DIR and NEEDY_DATA_DIR")?;
    let data = dir("NEEDY_DATA_DIR").unwrap_or_else(|| base.data_dir().join("needy"));
    let config = dir("NEEDY_CONFIG_DIR").unwrap_or_else(|| base.config_dir().join("needy"));
    // Windows has no state directory; its data directory is per user too.
    let state = dir("NEEDY_STATE_DIR").unwrap_or_else(|| base.state_dir().unwrap_or_else(|| base.data_dir()).join("needy"));
    let runtime = dir("NEEDY_RUNTIME_DIR").or_else(|| base.runtime_dir().map(|d| d.join("needy"))).unwrap_or_else(|| state.clone());
    Ok(Self {
      config,
      state,
      data,
      runtime,
    })
  }

  /// The control daemon's socket, which the CLI reaches by default.
  pub fn socket(&self) -> PathBuf {
    self.runtime.join("daemon.sock")
  }

  /// The client tokens `needy pair` stores, by endpoint.
  pub fn credentials(&self) -> PathBuf {
    self.config.join("credentials.json")
  }

  /// Profiles `--profile NAME` finds by name, as `NAME.yaml`.
  pub fn profiles(&self) -> PathBuf {
    self.config.join("profiles")
  }

  /// The base VM image of this host's VM driver, which `needy image pull`
  /// and `needy image build` write and the daemon boots by default.
  pub fn base_image(&self) -> PathBuf {
    needy_core_actor::image::vm::base_dir(&self.images(), needy_core_actor::image::vm::host_driver())
  }

  /// needy's VM images, a directory per driver: its base
  /// (`needy_core_actor::image::vm::base_dir`) and those `needy image build
  /// --from` makes (`needy_core_actor::image::vm::dir_of`).
  pub fn images(&self) -> PathBuf {
    self.data.join("images")
  }

  /// The guest disks the VM drivers make from the guest needy (the control
  /// daemon, Unix only), which `needy image rm --guest-disks` cleans.
  pub fn guest_disks(&self) -> PathBuf {
    self.data.join("guest")
  }
}

/// An override from the environment; an empty value counts as unset.
fn dir(name: &str) -> Option<PathBuf> {
  std::env::var_os(name).filter(|v| !v.is_empty()).map(|v: OsString| PathBuf::from(v))
}
