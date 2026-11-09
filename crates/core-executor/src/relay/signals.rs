//! Forwards local signals to the remote process: SIGINT, SIGTERM, SIGHUP and
//! SIGQUIT, and window changes (SIGWINCH) as resize requests on a terminal.
//! In raw mode Ctrl-C and Ctrl-Z travel as bytes instead.

use needy_api_client::exec::Input;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
pub async fn forward(input: Input, tty: bool, cancel: CancellationToken) {
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
  loop {
    let sent = tokio::select! {
      _ = int.recv() => input.signal("SIGINT").await,
      _ = term.recv() => input.signal("SIGTERM").await,
      _ = hup.recv() => input.signal("SIGHUP").await,
      _ = quit.recv() => input.signal("SIGQUIT").await,
      _ = winch.recv() => {
        if !tty {
          continue;
        }
        input.resize(super::window()).await
      }
      () = cancel.cancelled() => return,
    };
    // A closed stream has ended; there is nothing left to signal.
    if sent.is_err() {
      return;
    }
  }
}

// TODO(windows): Ctrl-C and Ctrl-Break (`tokio::signal::windows`) are not
// forwarded from a Windows client; terminals on Windows are a known gap.
#[cfg(not(unix))]
pub async fn forward(_input: Input, _tty: bool, _cancel: CancellationToken) {}
