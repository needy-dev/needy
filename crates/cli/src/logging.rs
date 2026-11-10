//! Process logs with `tracing`: stderr is off unless `-v` is given, and the
//! daemon also writes a daily file, `daemon.YYYY-MM-DD.log`, in its state
//! directory.
//!
//! Process logs are not the product trace (runs' events); those go
//! through the daemon's trace store.

use std::path::Path;

use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

/// Keeps the file writer flushing; drop it last.
pub struct Guard(#[allow(dead_code, reason = "held for its Drop")] Option<WorkerGuard>);

/// Installs the global subscriber. `verbosity` is the number of `-v` flags;
/// `log_dir` adds a daily file for the daemon.
pub fn init(verbosity: u8, log_dir: Option<&Path>) -> Result<Guard> {
  let stderr = fmt::layer().with_writer(std::io::stderr).with_target(verbosity > 1).with_filter(stderr_filter(verbosity));
  let mut file_error = None;
  let (file, guard) = match log_dir.map(file_appender) {
    Some(Ok(appender)) => {
      let (writer, guard) = tracing_appender::non_blocking(appender);
      (Some(fmt::layer().with_writer(writer).with_ansi(false).with_filter(LevelFilter::INFO)), Some(guard))
    }
    Some(Err(error)) => {
      file_error = Some(error);
      (None, None)
    }
    None => (None, None),
  };
  tracing_subscriber::registry().with(stderr).with(file).try_init()?;
  if let Some(error) = file_error {
    // A read-only home, as in some containers, must not stop the daemon.
    tracing::warn!("no log file: {error:#}");
  }
  Ok(Guard(guard))
}

fn stderr_filter(verbosity: u8) -> EnvFilter {
  // `-vv` turns on debug for needy's crates only; dependencies such as h2
  // are noisy at debug.
  let directives = match verbosity {
    0 => "off",
    1 => "info",
    2 => {
      "info,needy=debug,needy_cli=debug,needy_daemon=debug,needy_core=debug,needy_core_executor=debug,needy_core_actor=debug,needy_api_client=debug,needy_api_server=debug"
    }
    _ => "trace",
  };
  EnvFilter::new(directives)
}

fn file_appender(dir: &Path) -> Result<RollingFileAppender> {
  Ok(RollingFileAppender::builder().rotation(Rotation::DAILY).filename_prefix("daemon").filename_suffix("log").build(dir)?)
}
