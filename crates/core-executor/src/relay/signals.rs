//! Forwards local signals to the remote process: SIGINT, SIGTERM, SIGHUP and
//! SIGQUIT, and window changes (SIGWINCH) as resize requests on a terminal.
//! In raw mode Ctrl-C and Ctrl-Z travel as bytes instead.
//!
//! A second SIGTERM or SIGHUP is not forwarded: the process did not end at
//! the first, so the client restores the terminal and leaves (`terminated`).
//! SIGINT and SIGQUIT are always forwarded; a program may handle them and
//! go on.

use needy_api_client::exec::Input;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
pub async fn forward(input: Input, tty: bool, terminated: mpsc::Sender<&'static str>, cancel: CancellationToken) {
  use tokio::signal::unix::{SignalKind, signal};
  let handlers = (
    signal(SignalKind::interrupt()),
    signal(SignalKind::terminate()),
    signal(SignalKind::hangup()),
    signal(SignalKind::quit()),
    signal(SignalKind::window_change()),
  );
  let (Ok(mut int), Ok(mut term), Ok(mut hup), Ok(mut quit), Ok(mut winch)) = handlers else {
    tracing::warn!("relay: cannot install signal handlers; signals are not forwarded");
    return;
  };
  let mut terminating = false;
  loop {
    let signal = tokio::select! {
      _ = int.recv() => "SIGINT",
      _ = term.recv() => "SIGTERM",
      _ = hup.recv() => "SIGHUP",
      _ = quit.recv() => "SIGQUIT",
      _ = winch.recv() => {
        if !tty {
          continue;
        }
        if input.resize(super::window()).await.is_err() {
          return;
        }
        continue;
      }
      () = cancel.cancelled() => return,
    };
    let ends_client = matches!(signal, "SIGTERM" | "SIGHUP");
    if ends_client && terminating {
      super::restore();
      // The relay is waiting for this; a full queue already said it.
      if terminated.try_send(signal).is_err() {
        tracing::debug!("relay: the client is leaving already");
      }
      return;
    }
    terminating |= ends_client;
    let sent = input.signal(signal).await;
    // A closed stream has ended; there is nothing left to signal.
    if sent.is_err() {
      return;
    }
  }
}

// TODO(windows): Ctrl-C and Ctrl-Break (`tokio::signal::windows`) are not
// forwarded from a Windows client; terminals on Windows are a known gap.
#[cfg(not(unix))]
pub async fn forward(_input: Input, _tty: bool, _terminated: mpsc::Sender<&'static str>, _cancel: CancellationToken) {}
