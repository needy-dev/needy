//! Protected local control discovery. A descriptor locates a listener; peer
//! credentials, rather than this file, authorize its caller.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// A discovery failure with a stable producer-owned diagnostic reason.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
  reason: &'static str,
  message: &'static str,
  #[source]
  source: anyhow::Error,
}
impl Error {
  pub fn reason(&self) -> &'static str {
    self.reason
  }
  fn new(reason: &'static str, message: &'static str, source: impl Into<anyhow::Error>) -> Self {
    Self {
      reason,
      message,
      source: source.into(),
    }
  }
}
fn invalid(error: anyhow::Error) -> Error {
  let missing = error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
  if missing {
    Error::new("discovery.not_found", "No local control daemon was discovered", error)
  } else {
    Error::new("discovery.invalid_record", "The local control descriptor or its permissions are invalid", error)
  }
}

/// One live control process, correlated with health and an OS service manager.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
  pub version: u32,
  pub instance_id: String,
  pub pid: u32,
  pub role: String,
  pub address: String,
}

/// The cleanup outcome written before a control instance releases its lock.
/// A missing or mismatched record leaves cleanup unknown, never successful.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExitRecord {
  pub version: u32,
  pub instance_id: String,
  pub pid: u32,
  pub complete: bool,
}

pub fn read_exit(runtime: &Path) -> Result<ExitRecord> {
  check_directory(runtime).map_err(invalid)?;
  let file = private_file(&runtime.join("control-exit.json"), false).map_err(invalid)?;
  let record: ExitRecord = serde_json::from_reader(file).map_err(|error| invalid(error.into()))?;
  if record.version != 1 || record.instance_id.is_empty() || record.pid == 0 {
    return Err(invalid(anyhow::anyhow!("unsupported local shutdown result")).into());
  }
  Ok(record)
}

/// Reads only a private, regular descriptor in a private owned runtime directory.
/// A stale record is deliberately left intact for the caller's health check.
pub fn read(runtime: &Path) -> Result<Record> {
  check_directory(runtime).map_err(invalid)?;
  let path = runtime.join("control.json");
  let file = private_file(&path, false).map_err(invalid)?;
  let record: Record = serde_json::from_reader(file).map_err(|error| invalid(error.into()))?;
  if record.version != 1
    || record.role != "control"
    || record.instance_id.is_empty()
    || record.pid == 0
    || !record.address.starts_with("unix:")
  {
    return Err(invalid(anyhow::anyhow!("unsupported local control descriptor")).into());
  }
  Ok(record)
}

/// Holds the exclusive publication lock until the daemon has fully shut down.
pub(crate) struct Publisher {
  runtime: PathBuf,
  instance: String,
  lock: std::sync::Mutex<Option<File>>,
}

impl Publisher {
  pub(crate) fn acquire(runtime: &Path, instance: &str) -> Result<Self> {
    needy_api_server::create_private_dir(runtime)?;
    check_directory(runtime)?;
    let lock = private_file(&runtime.join("control.lock"), true)?;
    // A restart can reach here while the previous instance's release is
    // still in flight; only a live owner keeps the lock past a brief wait.
    for attempt in 0..50 {
      match lock.try_lock() {
        Ok(()) => break,
        Err(std::fs::TryLockError::WouldBlock) if attempt < 49 => {
          std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(error) => {
          return Err(Error::new("discovery.instance_conflict", "Another control daemon owns this runtime directory", error).into());
        }
      }
    }
    Ok(Self {
      runtime: runtime.to_path_buf(),
      instance: instance.to_string(),
      lock: std::sync::Mutex::new(Some(lock)),
    })
  }

  pub(crate) fn finish(&self, complete: bool) -> Result<()> {
    let mut lock = self.lock.lock().unwrap_or_else(|error| error.into_inner());
    if lock.is_none() {
      return Ok(());
    }
    let result = self.write(
      "control-exit.json",
      &ExitRecord {
        version: 1,
        instance_id: self.instance.clone(),
        pid: std::process::id(),
        complete,
      },
    );
    self.remove_owned();
    lock.take();
    result
  }

  pub(crate) fn retract(&self) {
    let lock = self.lock.lock().unwrap_or_else(|error| error.into_inner());
    if lock.is_some() {
      self.remove_owned();
    }
  }

  fn remove_owned(&self) {
    // The publication lock remains held through this check and removal.
    if read(&self.runtime).is_ok_and(|record| record.instance_id == self.instance)
      && let Err(error) = std::fs::remove_file(self.runtime.join("control.json"))
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove local control discovery: {error}");
    }
  }

  pub(crate) fn publish(&self, address: &str) -> Result<()> {
    let lock = self.lock.lock().unwrap_or_else(|error| error.into_inner());
    if lock.is_none() {
      bail!("discovery publication attempted after shutdown");
    }
    let record = Record {
      version: 1,
      instance_id: self.instance.clone(),
      pid: std::process::id(),
      role: "control".into(),
      address: address.to_string(),
    };
    self.write("control.json", &record)
  }

  fn write(&self, name: &str, record: &impl Serialize) -> Result<()> {
    let temporary = self.runtime.join(format!(".{name}-{}", self.instance));
    let result = (|| -> Result<()> {
      let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&temporary)?;
      use std::os::unix::fs::PermissionsExt as _;
      file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
      serde_json::to_writer(&mut file, record)?;
      file.write_all(b"\n")?;
      file.sync_all()?;
      std::fs::rename(&temporary, self.runtime.join(name))?;
      File::open(&self.runtime)?.sync_all()?;
      Ok(())
    })();
    if result.is_err()
      && let Err(error) = std::fs::remove_file(&temporary)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove discovery temporary file: {error}");
    }
    result.context("cannot publish local control discovery")
  }
}

impl Drop for Publisher {
  fn drop(&mut self) {
    self.retract();
  }
}

fn check_directory(path: &Path) -> Result<()> {
  let metadata = std::fs::symlink_metadata(path).context("discovery.not_found: local runtime directory is absent")?;
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  let uid = unsafe { libc::geteuid() };
  if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || (uid != 0 && metadata.uid() != uid) {
    bail!("discovery.invalid_record: runtime directory must be private and owned by the current account");
  }
  Ok(())
}

fn private_file(path: &Path, create: bool) -> Result<File> {
  let file = OpenOptions::new().read(true).write(create).create(create).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
  let metadata = file.metadata()?;
  // SAFETY: geteuid has no preconditions and does not mutate process state.
  let uid = unsafe { libc::geteuid() };
  if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 || (uid != 0 && metadata.uid() != uid) {
    bail!("discovery.invalid_record: descriptor and lock must be private regular files owned by the current account");
  }
  if create {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
  }
  Ok(file)
}
