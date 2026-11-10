//! Where needy keeps its files: XDG base directories through `etcetera` on
//! Linux and macOS, known folders on Windows. A `NEEDY_*` variable overrides
//! each directory. The CLI resolves them and hands them to the daemon.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use etcetera::BaseStrategy;

// TODO(config): the config directory (`$XDG_CONFIG_HOME/needy`) and the build
// cache (`$XDG_CACHE_HOME/needy`) have no reader yet. Add them when the config
// slice (2) reads user configuration or `needy build` keeps a cache.
#[derive(Debug, Clone)]
pub struct Paths {
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
    // Windows has no state directory; its data directory is per user too.
    let state = dir("NEEDY_STATE_DIR").unwrap_or_else(|| base.state_dir().unwrap_or_else(|| base.data_dir()).join("needy"));
    let runtime = dir("NEEDY_RUNTIME_DIR").or_else(|| base.runtime_dir().map(|d| d.join("needy"))).unwrap_or_else(|| state.clone());
    Ok(Self {
      state,
      data,
      runtime,
    })
  }

  /// The control daemon's socket, which the CLI reaches by default.
  pub fn socket(&self) -> PathBuf {
    self.runtime.join("daemon.sock")
  }

  /// The image `needy build` writes and the daemon boots by default.
  pub fn default_image(&self) -> PathBuf {
    self.data.join("images").join("default")
  }
}

/// An override from the environment; an empty value counts as unset.
fn dir(name: &str) -> Option<PathBuf> {
  std::env::var_os(name).filter(|v| !v.is_empty()).map(|v: OsString| PathBuf::from(v))
}
