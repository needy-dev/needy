//! Console log following and trace loading for runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use needy_api_server::control::{ControlError, Logs};
use needy_core::types::RunState;

use super::Run;

/// How often a followed console is read again for what the actor wrote.
// NOTICE: polling, not a file watcher: the console is one file that
// Firecracker appends to, and a quarter second is as quick as a person
// reading `run logs -f` needs; a watcher (`notify`) would add a dependency
// and a platform backend for no visible gain.
const FOLLOW_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The console at `path` from its start, then what the actor writes
/// next, until `run` is no longer running or the caller leaves.
pub(super) fn follow_console(run: Arc<Run>, path: PathBuf) -> Logs {
  use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
  let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, ControlError>>(16);
  let cancel = run.cancel.clone();
  let tasks = run.tasks.clone();
  tasks.spawn(async move {
    let mut offset = 0u64;
    loop {
      // The state is read before the file, so the read after the run
      // stopped running is the last and catches everything it wrote.
      let running = run.state.get() == RunState::Running && !cancel.is_cancelled();
      let read = async {
        let mut file = tokio::fs::File::open(&path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
      };
      match read.await {
        Ok(bytes) if !bytes.is_empty() => {
          offset += bytes.len() as u64;
          if tx.send(Ok(bytes)).await.is_err() {
            return;
          }
        }
        Ok(_) => {}
        // The VM has not created its console yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
          // The caller may be gone; there is nobody else to tell.
          let _ = tx.send(Err(ControlError::Internal(format!("cannot read {}: {e}", path.display())))).await;
          return;
        }
      }
      if !running {
        return;
      }
      tokio::select! {
        () = tokio::time::sleep(FOLLOW_POLL) => {}
        () = cancel.cancelled() => {}
        () = tx.closed() => return,
      }
    }
  });
  Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// The trace events of `run` in `trace.db`, as a control error.
pub(super) async fn load_trace(db: &Path, run: &str) -> Result<needy_trace::sink::sqlite::Loaded, ControlError> {
  needy_trace::sink::sqlite::load(db, run).await.map_err(|e| match e {
    needy_trace::Error::NoRun(_) | needy_trace::Error::NoDatabase(_) => ControlError::NotFound(format!("no trace of a run matching {run}")),
    needy_trace::Error::AmbiguousRun { prefix, candidates } => {
      ControlError::Invalid(format!("run {prefix} is ambiguous: {} runs match", candidates.len())).with_diagnostic(
        "run.ambiguous",
        [
          ("run".into(), prefix),
          ("candidates".into(), candidates.join(", ")),
        ],
      )
    }
    e => {
      ControlError::Internal(format!("{}: {e}", db.display())).with_diagnostic("state.read_failed", [("store".into(), "trace.db".into())])
    }
  })
}
