//! A process on pipes: stdout and stderr stay separate and stdin can be
//! half-closed.

use std::process::Stdio;
use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::proc::{Ends, Proc, lost};
use super::{Error, Input, Output, Spec, platform};

/// Bytes read from stdout or stderr at once.
const READ_CHUNK: usize = 256 * 1024;

pub(super) fn spawn(spec: &Spec, tasks: &TaskTracker, cancel: &CancellationToken) -> Result<Arc<Proc>, Error> {
  let program = &spec.argv[0];
  let mut command = tokio::process::Command::new(program);
  command.args(&spec.argv[1..]);
  if !spec.cwd.is_empty() {
    command.current_dir(&spec.cwd);
  }
  if spec.clear_env {
    command.env_clear();
  }
  command.envs(spec.env.iter().cloned());
  command.stdin(if spec.no_stdin {
    Stdio::null()
  } else {
    Stdio::piped()
  });
  command.stdout(Stdio::piped());
  command.stderr(Stdio::piped());
  platform::own_group(&mut command);
  platform::default_signals(&mut command);
  let mut child = command.spawn().map_err(|error| Error::Start {
    program: program.clone(),
    error,
  })?;
  let pid = child.id().map(i64::from).unwrap_or(-1);
  let (proc, ends) = Proc::new(spec, pid, false);
  let Ends {
    mut input,
    exit: exit_tx,
  } = ends;

  let mut stdin = child.stdin.take();
  // Both are piped above, so they are present.
  let stdout = child.stdout.take().expect("stdout is piped");
  let stderr = child.stderr.take().expect("stderr is piped");
  let out = tasks.spawn(pump(stdout, proc.clone(), Output::Stdout));
  let err = tasks.spawn(pump(stderr, proc.clone(), Output::Stderr));
  let cancel = cancel.clone();
  let input_proc = proc.clone();
  tasks.spawn(async move {
    loop {
      // Input ends with the process or the host.
      let next = tokio::select! {
        input = input.recv() => input,
        _ = input_proc.wait() => None,
        () = cancel.cancelled() => None,
      };
      match next {
        Some(Input::Stdin(bytes)) => {
          if let Some(s) = stdin.as_mut()
            && let Err(e) = s.write_all(&bytes).await
          {
            // The process closed its stdin; later input is dropped.
            tracing::debug!("process {pid}: stdin: {e}");
            stdin = None;
          }
        }
        Some(Input::Eof) => stdin = None,
        // `Proc::send` delivers signals; a pipe has no window size.
        Some(Input::Signal(_) | Input::Resize(..)) => {}
        None => break,
      }
    }
  });
  let wait_proc = proc.clone();
  tasks.spawn(async move {
    let status = child.wait().await;
    // Output read before the exit reaches subscribers before it.
    for (name, reader) in [("stdout", out), ("stderr", err)] {
      if let Err(e) = reader.await {
        tracing::warn!("process {pid}: {name} reader failed: {e}");
      }
    }
    let exit = match status {
      Ok(status) => platform::exit_from_std(status),
      Err(e) => {
        tracing::warn!("process {pid}: cannot wait: {e}");
        lost()
      }
    };
    wait_proc.finish(&exit_tx, exit).await;
  });
  Ok(proc)
}

/// Fans one output stream out until end of file.
async fn pump(mut reader: impl AsyncRead + Unpin, proc: Arc<Proc>, wrap: fn(Bytes) -> Output) {
  let mut buf = vec![0u8; READ_CHUNK];
  loop {
    match reader.read(&mut buf).await {
      Ok(0) => break,
      Ok(n) => proc.emit(wrap(Bytes::copy_from_slice(&buf[..n]))).await,
      Err(e) => {
        tracing::debug!("process {}: read: {e}", proc.id);
        break;
      }
    }
  }
}
