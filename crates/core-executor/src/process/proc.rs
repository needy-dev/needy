//! One process: input, output fan-out to attached clients, the replay buffer
//! and the exit.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use needy_common::process::Exit;
use needy_common::time::now_ms;
use tokio::sync::{mpsc, watch};

use super::{Input, Output, Spec, TermFallback, platform};

/// Output kept per process for clients that attach later.
// NOTICE: the CLI note documents 256 KiB; a client attaching after more
// output than this sees only the most recent part.
const REPLAY_BYTES: usize = 256 * 1024;

/// Output events queued per subscriber: the owner's before the process
/// waits for it, a viewer's before it is dropped.
const SUBSCRIBER_QUEUE: usize = 64;

pub struct Proc {
  pub id: String,
  pub argv: Vec<String>,
  pub pid: i64,
  pub tty: bool,
  pub started_at_ms: i64,
  pub session_id: String,
  /// The terminal type the executor set instead of the client's.
  pub term_fallback: Option<TermFallback>,
  input: mpsc::UnboundedSender<Input>,
  /// The replay buffer and the subscribers, under one lock so a new
  /// subscriber's snapshot and its first live event never overlap.
  fanout: Mutex<Fanout>,
  /// Orders each event's delivery to the owner; held across the owner's
  /// send, never across a viewer's.
  owner_order: tokio::sync::Mutex<()>,
  exited: watch::Receiver<Option<Exit>>,
  finished_at: OnceLock<Instant>,
}

struct Fanout {
  events: VecDeque<Output>,
  bytes: usize,
  finished: Option<Exit>,
  /// The client that started the process; it alone may hold the process
  /// back.
  owner: Option<mpsc::Sender<Output>>,
  viewers: Vec<Viewer>,
}

struct Viewer {
  tx: mpsc::Sender<Output>,
  behind: Arc<AtomicBool>,
}

/// Who subscribes to a process's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscriber {
  /// The client that started the process. A slow owner slows the process
  /// down instead of losing output (the CLI note documents this).
  Owner,
  /// Any other client, such as one that attached. A viewer that cannot keep
  /// up is dropped, [`Subscription::fell_behind`], and never holds back the
  /// process or another client.
  // TODO(terminal-model): the terminal note's G1 turns this into skipping
  // output and a fresh redraw from the terminal model.
  Viewer,
}

/// A subscriber's output: what happened before it subscribed, then what
/// happens after. The two never overlap.
pub struct Subscription {
  pub past: Vec<Output>,
  pub live: mpsc::Receiver<Output>,
  behind: Arc<AtomicBool>,
}

impl Subscription {
  /// Whether `live` ended because this viewer could not keep up.
  pub fn fell_behind(&self) -> bool {
    self.behind.load(Ordering::SeqCst)
  }
}

/// The parts of a new process its tasks own: the input queue and the exit.
pub(super) struct Ends {
  pub input: mpsc::UnboundedReceiver<Input>,
  pub exit: watch::Sender<Option<Exit>>,
}

impl Proc {
  pub(super) fn new(spec: &Spec, pid: i64, tty: bool, term_fallback: Option<TermFallback>) -> (Arc<Self>, Ends) {
    let (input, input_rx) = mpsc::unbounded_channel();
    let (exit_tx, exited) = watch::channel(None);
    let proc = Arc::new(Self {
      id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
      argv: spec.argv.clone(),
      pid,
      tty,
      started_at_ms: now_ms(),
      session_id: spec.session_id.clone(),
      term_fallback,
      input,
      fanout: Mutex::new(Fanout {
        events: VecDeque::new(),
        bytes: 0,
        finished: None,
        owner: None,
        viewers: Vec::new(),
      }),
      owner_order: tokio::sync::Mutex::new(()),
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

  /// Subscribes to output. With `replay`, buffered output comes first. A
  /// process has one [`Subscriber::Owner`]; a later one replaces it.
  pub fn subscribe(&self, replay: bool, subscriber: Subscriber) -> Subscription {
    let (tx, live) = mpsc::channel(SUBSCRIBER_QUEUE);
    let behind = Arc::new(AtomicBool::new(false));
    let mut fanout = self.lock_fanout();
    let mut past: Vec<Output> = if replay {
      fanout.events.iter().cloned().collect()
    } else {
      Vec::new()
    };
    if let Some(exit) = &fanout.finished {
      past.push(Output::Exit(exit.clone()));
    } else {
      match subscriber {
        Subscriber::Owner => fanout.owner = Some(tx),
        Subscriber::Viewer => fanout.viewers.push(Viewer {
          tx,
          behind: behind.clone(),
        }),
      }
    }
    Subscription { past, live, behind }
  }

  /// Records `event` and fans it out: to each viewer without waiting, and to
  /// the owner, whom the process waits for. No lock is held while it waits.
  pub(super) async fn emit(&self, event: Output) {
    let _order = self.owner_order.lock().await;
    let owner = {
      let mut fanout = self.lock_fanout();
      fanout.record(&event);
      let last = matches!(event, Output::Exit(_));
      fanout.viewers.retain(|viewer| match viewer.tx.try_send(event.clone()) {
        Ok(()) => !last,
        Err(mpsc::error::TrySendError::Full(_)) => {
          viewer.behind.store(true, Ordering::SeqCst);
          false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
      });
      if last {
        fanout.owner.take()
      } else {
        fanout.owner.clone()
      }
    };
    if let Some(owner) = owner
      && owner.send(event).await.is_err()
    {
      // The owner left; the process no longer waits for anyone.
      let mut fanout = self.lock_fanout();
      if fanout.owner.as_ref().is_some_and(|current| current.same_channel(&owner)) {
        fanout.owner = None;
      }
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

  fn lock_fanout(&self) -> std::sync::MutexGuard<'_, Fanout> {
    // NOTICE: every change under the lock is one push, pop or swap, so a
    // panic elsewhere leaves the buffer whole.
    self.fanout.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Fanout {
  fn record(&mut self, event: &Output) {
    match event {
      Output::Stdout(b) | Output::Stderr(b) => {
        self.bytes += b.len();
        self.events.push_back(event.clone());
        while self.bytes > REPLAY_BYTES {
          match self.events.pop_front() {
            Some(Output::Stdout(b) | Output::Stderr(b)) => self.bytes -= b.len(),
            Some(_) => {}
            None => break,
          }
        }
      }
      Output::Exit(exit) => self.finished = Some(exit.clone()),
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
