use std::time::Duration;

use tokio::sync::mpsc;

use super::*;

/// A `Termination` fed by the returned sender instead of the process's
/// signals.
fn injected() -> (mpsc::UnboundedSender<&'static str>, Termination) {
  let (send, received) = mpsc::unbounded_channel();
  (send, Termination::watch(received))
}

#[tokio::test]
async fn the_first_signal_stops_and_the_second_forces() {
  let (signals, termination) = injected();
  let stop = termination.stop();

  signals.send("SIGTERM").unwrap();
  tokio::time::timeout(Duration::from_secs(5), stop.cancelled()).await.expect("the first signal stops");
  let shutdown = termination.unless_forced(std::future::pending::<()>());
  tokio::pin!(shutdown);
  assert!(tokio::time::timeout(Duration::from_millis(100), &mut shutdown).await.is_err(), "one signal does not force");

  signals.send("SIGTERM").unwrap();
  let forced = tokio::time::timeout(Duration::from_secs(5), shutdown).await.expect("the second signal forces");
  assert_eq!(forced, None);
}

#[tokio::test]
async fn a_shutdown_that_ends_is_not_forced() {
  let (signals, termination) = injected();
  signals.send("SIGINT").unwrap();

  assert_eq!(termination.unless_forced(async { 7 }).await, Some(7));
}
