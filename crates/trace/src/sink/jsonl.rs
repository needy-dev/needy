//! A sink that appends one JSON envelope per line to a file, for export.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Error, Event, Sink};

pub struct Jsonl {
  path: PathBuf,
  file: std::fs::File,
}

impl Jsonl {
  /// Opens `path` for appending, creating it and its directory.
  pub fn create(path: &Path) -> Result<Self, Error> {
    let io = |source| Error::Io {
      path: path.to_path_buf(),
      source,
    };
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(io)?;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(io)?;
    Ok(Self {
      path: path.to_path_buf(),
      file,
    })
  }
}

#[async_trait::async_trait]
impl Sink for Jsonl {
  async fn write(&mut self, event: &Event) -> Result<(), Error> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    // NOTICE: one small append per event, without a buffer, so a crash loses
    // at most the line being written. The write blocks the worker, never a
    // producer.
    self.file.write_all(&line).map_err(|source| Error::Io {
      path: self.path.clone(),
      source,
    })
  }
}

#[cfg(test)]
#[path = "jsonl_test.rs"]
mod tests;
