//! Filesystem helpers.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically writes `bytes` to `path` by writing a sibling temporary file and
/// renaming it into place. On Unix, when `mode` is `Some`, applies those file
/// permissions before renaming.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
  let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
  let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
  let tmp: PathBuf = parent.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));

  let write_result = (|| -> std::io::Result<()> {
    let mut file = open_temp(&tmp, mode)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    if let Some(mode) = mode {
      use std::os::unix::fs::PermissionsExt as _;
      // The umask may have narrowed the mode passed to `open`; enforce exact bits.
      std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
  })();

  if write_result.is_err() {
    let _ = std::fs::remove_file(&tmp);
  }
  write_result
}

fn open_temp(path: &Path, mode: Option<u32>) -> std::io::Result<std::fs::File> {
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  if let Some(mode) = mode {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(mode);
  }
  #[cfg(not(unix))]
  let _ = mode;
  options.open(path)
}

#[cfg(test)]
mod tests {
  use super::write_atomic;

  #[test]
  fn write_atomic_creates_and_overwrites_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");

    write_atomic(&path, b"first", None).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");

    write_atomic(&path, b"second", None).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");

    // No leftover temp files.
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(entries.len(), 1);
  }

  #[cfg(unix)]
  #[test]
  fn write_atomic_applies_unix_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.key");

    write_atomic(&path, b"secret", Some(0o600)).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
  }
}
