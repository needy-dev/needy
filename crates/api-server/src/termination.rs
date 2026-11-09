//! Turns requests to stop the process into cancellation: SIGINT, SIGTERM
//! and SIGHUP on Unix, Ctrl-C on Windows. The first one asks for a graceful
//! shutdown; a second one forces the process to exit.

use std::future::Future;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The termination signals the process has received.
pub struct Termination {
  stop: CancellationToken,
  force: CancellationToken,
}

impl Termination {
  /// Listens for termination signals. The handlers are installed before
  /// this returns, so a signal right after is not lost.
  pub fn listen() -> std::io::Result<Self> {
    let mut signals = Signals::new()?;
    let (send, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
      // Ends once the watch stops listening after the second signal.
      while send.send(signals.recv().await).is_ok() {}
    });
    Ok(Self::watch(received))
  }

  /// Cancels `stop` on the first signal `received` gets and `force` on the
  /// second, each signal named.
  fn watch(mut received: mpsc::UnboundedReceiver<&'static str>) -> Self {
    let stop = CancellationToken::new();
    let force = CancellationToken::new();
    let (first, second) = (stop.clone(), force.clone());
    tokio::spawn(async move {
      let Some(name) = received.recv().await else {
        return;
      };
      tracing::info!("{name}: stopping; send it again to exit at once");
      first.cancel();
      let Some(name) = received.recv().await else {
        return;
      };
      tracing::warn!("{name} again: exiting without finishing the shutdown");
      second.cancel();
    });
    Self { stop, force }
  }

  /// A token cancelled on the first signal.
  pub fn stop(&self) -> CancellationToken {
    self.stop.clone()
  }

  /// Runs `graceful` to its end, or returns `None` when a second signal
  /// comes first; `graceful` is then dropped where it was.
  pub async fn unless_forced<T>(&self, graceful: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
      // A shutdown that already ended is not forced.
      biased;
      done = graceful => Some(done),
      () = self.force.cancelled() => None,
    }
  }
}

#[cfg(unix)]
struct Signals {
  int: tokio::signal::unix::Signal,
  term: tokio::signal::unix::Signal,
  hup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
  fn new() -> std::io::Result<Self> {
    use tokio::signal::unix::{SignalKind, signal};
    Ok(Self {
      int: signal(SignalKind::interrupt())?,
      term: signal(SignalKind::terminate())?,
      hup: signal(SignalKind::hangup())?,
    })
  }

  async fn recv(&mut self) -> &'static str {
    tokio::select! {
      _ = self.int.recv() => "SIGINT",
      _ = self.term.recv() => "SIGTERM",
      _ = self.hup.recv() => "SIGHUP",
    }
  }
}

#[cfg(windows)]
struct Signals(tokio::signal::windows::CtrlC);

#[cfg(windows)]
impl Signals {
  fn new() -> std::io::Result<Self> {
    Ok(Self(tokio::signal::windows::ctrl_c()?))
  }

  async fn recv(&mut self) -> &'static str {
    self.0.recv().await;
    "Ctrl-C"
  }
}

#[cfg(test)]
#[path = "termination_test.rs"]
mod tests;
