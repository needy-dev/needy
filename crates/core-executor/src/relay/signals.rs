//! Forwards local signals to the remote process: SIGINT, SIGTERM, SIGHUP and
//! SIGQUIT, and window changes (SIGWINCH) as resize requests on a terminal.
//! In raw mode Ctrl-C and Ctrl-Z travel as bytes instead.

use needy_api_proto::v1::{ExecRequest, exec_request};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
pub async fn forward(tx: mpsc::Sender<ExecRequest>, tty: bool, cancel: CancellationToken) {
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
    let payload = tokio::select! {
      _ = int.recv() => exec_request::Payload::Signal("SIGINT".into()),
      _ = term.recv() => exec_request::Payload::Signal("SIGTERM".into()),
      _ = hup.recv() => exec_request::Payload::Signal("SIGHUP".into()),
      _ = quit.recv() => exec_request::Payload::Signal("SIGQUIT".into()),
      _ = winch.recv() => {
        if !tty {
          continue;
        }
        exec_request::Payload::Resize(super::window())
      }
      () = cancel.cancelled() => return,
    };
    if tx
      .send(ExecRequest {
        payload: Some(payload),
      })
      .await
      .is_err()
    {
      return;
    }
  }
}

// TODO(windows): Ctrl-C and Ctrl-Break (`tokio::signal::windows`) are not
// forwarded from a Windows client; terminals on Windows are a known gap.
#[cfg(not(unix))]
pub async fn forward(_tx: mpsc::Sender<ExecRequest>, _tty: bool, _cancel: CancellationToken) {}
