//! One process: input, output fan-out to attached clients, the replay buffer
//! and the exit.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use needy_common::process::Exit;
use needy_common::time::now_ms;
use tokio::sync::{mpsc, watch};

use super::{Input, Output, Spec, platform};

/// Output kept per process for clients that attach later.
// NOTICE: the CLI note documents 256 KiB; a client attaching after more
// output than this sees only the most recent part.
const REPLAY_BYTES: usize = 256 * 1024;

/// Output events queued per subscriber before the process waits for it.
const SUBSCRIBER_QUEUE: usize = 64;

pub struct Proc {
  pub id: String,
  pub argv: Vec<String>,
  pub pid: i64,
  pub tty: bool,
  pub started_at_ms: i64,
  pub session_id: String,
  input: mpsc::UnboundedSender<Input>,
  subscribers: tokio::sync::Mutex<Vec<mpsc::Sender<Output>>>,
  replay: Mutex<Replay>,
  exited: watch::Receiver<Option<Exit>>,
  finished_at: OnceLock<Instant>,
}

struct Replay {
  events: VecDeque<Output>,
  bytes: usize,
  finished: Option<Exit>,
}

/// The parts of a new process its tasks own: the input queue and the exit.
pub(super) struct Ends {
  pub input: mpsc::UnboundedReceiver<Input>,
  pub exit: watch::Sender<Option<Exit>>,
}

impl Proc {
  pub(super) fn new(spec: &Spec, pid: i64, tty: bool) -> (Arc<Self>, Ends) {
    let (input, input_rx) = mpsc::unbounded_channel();
    let (exit_tx, exited) = watch::channel(None);
    let proc = Arc::new(Self {
      id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
      argv: spec.argv.clone(),
      pid,
      tty,
      started_at_ms: now_ms(),
      session_id: spec.session_id.clone(),
      input,
      subscribers: tokio::sync::Mutex::new(Vec::new()),
      replay: Mutex::new(Replay {
        events: VecDeque::new(),
        bytes: 0,
        finished: None,
      }),
      exited,
      finished_at: OnceLock::new(),
    });
    let ends = Ends {
      input: input_rx,
      exit: exit_tx,
    };
    (proc, ends)
  }

  /// Sends stdin, end of input or a resize to the process, or delivers a
  /// signal to its process group.
  pub fn send(&self, input: Input) {
    if let Input::Signal(name) = &input {
      platform::signal_group(self.pid, name);
      return;
    }
    if self.input.send(input).is_err() {
      // The input task ends with the process; late input has nowhere to go.
      tracing::debug!("process {}: input after the process ended", self.id);
    }
  }

  pub fn exit(&self) -> Option<Exit> {
    self.exited.borrow().clone()
  }

  pub async fn wait(&self) -> Exit {
    let mut exited = self.exited.clone();
    match exited.wait_for(Option::is_some).await {
      Ok(exit) => exit.clone().unwrap_or_else(lost),
      // The waiter task ended without an exit: the runtime is going away.
      Err(_) => lost(),
    }
  }

  /// How long ago the process finished.
  pub(super) fn finished_for(&self) -> Option<Duration> {
    self.finished_at.get().map(Instant::elapsed)
  }

  /// Subscribes to output. With `replay`, buffered output comes first. The
  /// subscriber lock orders the snapshot before any live event. A slow
  /// subscriber slows the process down instead of losing output.
  pub async fn subscribe(&self, replay: bool) -> (Vec<Output>, mpsc::Receiver<Output>) {
    let mut subscribers = self.subscribers.lock().await;
    let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
    let state = self.replay.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut past: Vec<Output> = if replay {
      state.events.iter().cloned().collect()
    } else {
      Vec::new()
    };
    if let Some(exit) = &state.finished {
      past.push(Output::Exit(exit.clone()));
    } else {
      subscribers.push(tx);
    }
    (past, rx)
  }

  pub(super) async fn emit(&self, event: Output) {
    let mut subscribers = self.subscribers.lock().await;
    self.record(&event);
    let mut alive = Vec::with_capacity(subscribers.len());
    for tx in subscribers.drain(..) {
      if tx.send(event.clone()).await.is_ok() {
        alive.push(tx);
      }
    }
    if !matches!(event, Output::Exit(_)) {
      *subscribers = alive;
    }
  }

  /// Publishes the exit to subscribers, then to waiters; from then on the
  /// retention period runs.
  pub(super) async fn finish(&self, ends: &watch::Sender<Option<Exit>>, exit: Exit) {
    self.emit(Output::Exit(exit.clone())).await;
    self.finished_at.get_or_init(Instant::now);
    // `send_replace` stores the exit even when no waiter is subscribed.
    ends.send_replace(Some(exit));
  }

  fn record(&self, event: &Output) {
    let mut state = self.replay.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match event {
      Output::Stdout(b) | Output::Stderr(b) => {
        state.bytes += b.len();
        state.events.push_back(event.clone());
        while state.bytes > REPLAY_BYTES {
          match state.events.pop_front() {
            Some(Output::Stdout(b) | Output::Stderr(b)) => state.bytes -= b.len(),
            Some(_) => {}
            None => break,
          }
        }
      }
      Output::Exit(exit) => state.finished = Some(exit.clone()),
    }
  }
}

/// The exit reported when the process's exit could not be observed.
pub(super) fn lost() -> Exit {
  Exit {
    code: None,
    signal: "SIGKILL".into(),
  }
}
