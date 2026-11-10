//! A process on a pseudo-terminal: one output stream, window size changes,
//! and end of input as the EOF character when the terminal reads by lines.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::proc::{Ends, Proc, lost};
use super::{Error, FALLBACK_TERM, Input, Output, Spec, TermFallback, platform};

/// Bytes read from the terminal at once.
const READ_CHUNK: usize = 256 * 1024;

/// How long output may keep draining after the child exits before the
/// terminal is closed.
// NOTICE: on Unix the reader ends by itself once every process closes the
// terminal, which a descendant in its own session may never do, so the
// reader is stopped after this grace period; ConPTY on Windows ends it only
// when the pseudo console closes, so the master is dropped then too.
const DRAIN: Duration = Duration::from_millis(500);

fn size(rows: u16, cols: u16) -> PtySize {
  PtySize {
    rows,
    cols,
    pixel_width: 0,
    pixel_height: 0,
  }
}

pub(super) fn spawn(spec: &Spec, (rows, cols): (u16, u16), tasks: &TaskTracker, cancel: &CancellationToken) -> Result<Arc<Proc>, Error> {
  let program = &spec.argv[0];
  let pair = native_pty_system().openpty(size(rows, cols)).map_err(|e| Error::Terminal(format!("{e:#}")))?;
  let mut command = CommandBuilder::new(program);
  command.args(&spec.argv[1..]);
  if !spec.cwd.is_empty() {
    command.cwd(&spec.cwd);
  }
  if spec.clear_env {
    command.env_clear();
  }
  for (k, v) in &spec.env {
    command.env(k, v);
  }
  // A program cannot draw for a terminal type this machine does not know.
  let term_fallback = spec.env.iter().rev().find(|(k, _)| k == "TERM").and_then(|(_, term)| {
    (!platform::has_terminfo(term)).then(|| TermFallback {
      requested: term.clone(),
      used: FALLBACK_TERM.into(),
    })
  });
  if let Some(fallback) = &term_fallback {
    command.env("TERM", &fallback.used);
  }
  let mut child = pair.slave.spawn_command(command).map_err(|e| Error::StartTerminal {
    program: program.clone(),
    message: format!("{e:#}"),
  })?;
  drop(pair.slave);
  let pid = child.process_id().map(i64::from).unwrap_or(-1);
  let master = pair.master;
  let mut reader = master.try_clone_reader().map_err(|e| Error::Terminal(format!("{e:#}")))?;
  let readable = platform::Readable::new(master.as_ref()).map_err(|e| Error::Terminal(format!("{e:#}")))?;
  // Set when the child exited and the output had its grace period.
  let stop_reading = Arc::new(AtomicBool::new(false));
  let reader_stop = stop_reading.clone();
  let mut writer = master.take_writer().map_err(|e| Error::Terminal(format!("{e:#}")))?;
  let (proc, ends) = Proc::new(spec, pid, true, term_fallback);
  let Ends {
    mut input,
    exit: exit_tx,
  } = ends;

  // portable-pty's reader and writer block, so each gets a blocking thread.
  let (out_tx, mut out_rx) = mpsc::channel::<Bytes>(64);
  let mut read_thread = tasks.spawn_blocking(move || {
    let mut buf = vec![0u8; READ_CHUNK];
    // A read blocks until there is output; the reader waits for output
    // first, so it can stop while a descendant keeps the terminal open.
    while readable.wait(&reader_stop) {
      match reader.read(&mut buf) {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          if out_tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
            break;
          }
        }
      }
    }
  });
  let out_proc = proc.clone();
  let pump = tasks.spawn(async move {
    while let Some(bytes) = out_rx.recv().await {
      out_proc.emit(Output::Stdout(bytes)).await;
    }
  });
  let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
  tasks.spawn_blocking(move || {
    while let Ok(bytes) = in_rx.recv() {
      if let Err(e) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
        tracing::debug!("process {pid}: terminal input: {e}");
        break;
      }
    }
  });
  // The master is dropped after the child exits: ConPTY on Windows only ends
  // the reader once its pseudo console closes.
  let master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> = Arc::new(Mutex::new(Some(master)));
  let wait_master = master.clone();
  let cancel = cancel.clone();
  let input_proc = proc.clone();
  tasks.spawn(async move {
    // After end of input in raw mode, input is not forwarded any more.
    let mut ended = false;
    loop {
      // Input ends with the process or the host; dropping `in_tx` ends the
      // writer thread.
      let next = tokio::select! {
        input = input.recv() => input,
        _ = input_proc.wait() => None,
        () = cancel.cancelled() => None,
      };
      let bytes = match next {
        Some(Input::Stdin(_)) if ended => continue,
        Some(Input::Stdin(bytes)) => bytes,
        // A terminal has no half-close: a line-reading program gets the EOF
        // character; a raw one, which would read it as Ctrl-D, nothing.
        Some(Input::Eof) => {
          let bytes = match master.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
            Some(master) => platform::end_of_input(master.as_ref()),
            None => Vec::new(),
          };
          if bytes.is_empty() {
            ended = true;
            continue;
          }
          bytes
        }
        Some(Input::Resize(rows, cols)) => {
          input_proc.resize_screen(rows, cols);
          if let Some(master) = master.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref()
            && let Err(e) = master.resize(size(rows, cols))
          {
            tracing::debug!("process {pid}: resize: {e:#}");
          }
          continue;
        }
        // `Proc::send` delivers signals.
        Some(Input::Signal(_)) => continue,
        None => break,
      };
      if in_tx.send(bytes).is_err() {
        // The writer stopped because the terminal closed.
        break;
      }
    }
  });
  let wait_proc = proc.clone();
  let wait_tasks = tasks.clone();
  tasks.spawn(async move {
    let exit = match wait_tasks.spawn_blocking(move || platform::wait_terminal(&mut child, pid)).await {
      Ok(exit) => exit,
      Err(e) => {
        tracing::warn!("process {pid}: cannot wait: {e}");
        lost()
      }
    };
    let drained = tokio::time::timeout(DRAIN, &mut read_thread).await.is_ok();
    // A descendant that left the session, such as `setsid sleep 600 &`,
    // keeps the terminal open, and the reader would never see its end.
    stop_reading.store(true, Ordering::SeqCst);
    wait_master.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    if !drained && let Err(e) = read_thread.await {
      tracing::warn!("process {pid}: terminal reader failed: {e}");
    }
    if let Err(e) = pump.await {
      tracing::warn!("process {pid}: output pump failed: {e}");
    }
    wait_proc.finish(&exit_tx, exit).await;
  });
  Ok(proc)
}
