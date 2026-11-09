//! Turns a request to stop the process into cancellation: SIGINT, SIGTERM
//! and SIGHUP on Unix, Ctrl-C on Windows.

use tokio_util::sync::CancellationToken;

/// A token cancelled on the first termination signal. The handlers are
/// installed before this returns, so a signal right after is not lost.
pub fn token() -> std::io::Result<CancellationToken> {
  let token = CancellationToken::new();
  let cancel = token.clone();
  let signalled = signalled()?;
  tokio::spawn(async move {
    let name = signalled.await;
    tracing::info!("{name}: stopping");
    cancel.cancel();
  });
  Ok(token)
}

#[cfg(unix)]
fn signalled() -> std::io::Result<impl Future<Output = &'static str>> {
  use tokio::signal::unix::{SignalKind, signal};
  let mut int = signal(SignalKind::interrupt())?;
  let mut term = signal(SignalKind::terminate())?;
  let mut hup = signal(SignalKind::hangup())?;
  Ok(async move {
    tokio::select! {
      _ = int.recv() => "SIGINT",
      _ = term.recv() => "SIGTERM",
      _ = hup.recv() => "SIGHUP",
    }
  })
}

#[cfg(windows)]
fn signalled() -> std::io::Result<impl Future<Output = &'static str>> {
  let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
  Ok(async move {
    ctrl_c.recv().await;
    "Ctrl-C"
  })
}
