//! Dispatch: one session's bounded event queue and the worker that drains it
//! into the session's sinks.
//!
//! Producers never wait. [`Dispatch::emit`] puts the event on the queue or,
//! when the queue is full, drops it and counts it. When the queue has room
//! again, one `trace.gap { dropped }` event takes the dropped events' place,
//! so a reader knows the trace is incomplete and where.

use needy_common::time::now_ms;
use serde_json::value::RawValue;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{mpsc, oneshot};

use crate::{Event, Sink, event};

/// How a session's Dispatch behaves.
#[derive(Debug, Clone, Copy)]
pub struct Options {
  /// Events the queue holds before it drops.
  pub capacity: usize,
  /// The payload schema version stamped on every event, including the
  /// `trace.gap` and `trace.timing` events Dispatch writes itself.
  pub version: u32,
  /// Collect per-hook latencies and write them as `trace.timing`.
  pub timing: bool,
}

/// A session's event pipeline. Clones share one queue; the worker ends when
/// the last clone is dropped and the queue is drained.
#[derive(Clone)]
pub struct Dispatch {
  inner: Arc<Inner>,
}

struct Inner {
  session: String,
  version: u32,
  tx: mpsc::Sender<Message>,
  shared: Arc<Shared>,
}

/// What producers and the worker both change.
struct Shared {
  /// Producers enqueue while holding this lock, so the worker can tell an
  /// empty queue from one about to fill.
  counters: Mutex<Counters>,
  #[cfg(feature = "timing")]
  timing: Option<Mutex<crate::timing::Timing>>,
}

#[derive(Default)]
struct Counters {
  /// The next event's number.
  seq: u64,
  /// Events dropped since the last gap.
  dropped: u64,
}

enum Message {
  Event(Event),
  Flush(oneshot::Sender<()>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // A panic while holding the lock leaves plain counters, which stay usable.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Dispatch {
  /// Starts the worker of `session` on the current Tokio runtime.
  pub fn new(session: impl Into<String>, sinks: Vec<Box<dyn Sink>>, options: Options) -> Self {
    let session = session.into();
    let (tx, rx) = mpsc::channel(options.capacity.max(1));
    let shared = Arc::new(Shared {
      counters: Mutex::new(Counters::default()),
      #[cfg(feature = "timing")]
      timing: options.timing.then(Default::default),
    });
    #[cfg(not(feature = "timing"))]
    if options.timing {
      tracing::warn!("session {session}: trace timing is on, but this build has no `timing` feature");
    }
    let worker = Worker {
      session: session.clone(),
      version: options.version,
      rx,
      sinks,
      shared: shared.clone(),
    };
    tokio::spawn(worker.run());
    Self {
      inner: Arc::new(Inner {
        session,
        version: options.version,
        tx,
        shared,
      }),
    }
  }

  pub fn session(&self) -> &str {
    &self.inner.session
  }

  /// Records an event of `kind` now.
  pub fn emit(&self, kind: &str, payload: Box<RawValue>) {
    self.emit_at(now_ms(), kind, payload);
  }

  /// Records an event of `kind` with the time its producer gave it.
  pub fn emit_at(&self, time_ms: i64, kind: &str, payload: Box<RawValue>) {
    let inner = &*self.inner;
    let mut counters = lock(&inner.shared.counters);
    if counters.dropped > 0 {
      // The queue may have room again: the gap goes first. When it has
      // not, this event is dropped too.
      let gap = gap(&inner.session, inner.version, &counters, time_ms);
      if !self.enqueue(&mut counters, gap) {
        return;
      }
      counters.dropped = 0;
    }
    let event = Event {
      session: inner.session.clone(),
      seq: counters.seq,
      time_ms,
      kind: kind.to_string(),
      version: inner.version,
      payload,
    };
    self.enqueue(&mut counters, event);
  }

  /// Puts `event` on the queue, numbered `counters.seq`, or counts it as
  /// dropped. Returns whether it was queued.
  fn enqueue(&self, counters: &mut Counters, event: Event) -> bool {
    match self.inner.tx.try_send(Message::Event(event)) {
      Ok(()) => {
        counters.seq += 1;
        true
      }
      Err(mpsc::error::TrySendError::Full(_)) => {
        counters.dropped += 1;
        false
      }
      Err(mpsc::error::TrySendError::Closed(message)) => {
        // NOTICE: the worker only ends early when the runtime shuts down.
        if let Message::Event(event) = message {
          tracing::warn!("session {}: trace {} after the trace closed", event.session, event.kind);
        }
        false
      }
    }
  }

  /// Waits until every event queued before this call is written, the
  /// pending gap and timing summary included, and the sinks are flushed.
  pub async fn flush(&self) {
    let (done, wait) = oneshot::channel();
    if self.inner.tx.send(Message::Flush(done)).await.is_err() {
      // The worker ended; nothing is left to flush.
      return;
    }
    // A worker that ends first has written everything too.
    let _ = wait.await;
  }

  /// Adds one latency sample of `hook`, when timing is on.
  #[cfg(feature = "timing")]
  pub fn time(&self, hook: &str, elapsed: std::time::Duration) {
    if let Some(timing) = &self.inner.shared.timing {
      lock(timing).add(hook, elapsed);
    }
  }
}

/// The gap event for the events dropped so far, numbered `counters.seq`.
fn gap(session: &str, version: u32, counters: &Counters, time_ms: i64) -> Event {
  let payload = serde_json::value::to_raw_value(&serde_json::json!({ "dropped": counters.dropped }))
    // NOTICE: an object with one integer always serializes.
    .expect("gap payloads serialize to JSON");
  Event {
    session: session.to_string(),
    seq: counters.seq,
    time_ms,
    kind: event::GAP.to_string(),
    version,
    payload,
  }
}

struct Worker {
  session: String,
  version: u32,
  rx: mpsc::Receiver<Message>,
  sinks: Vec<Box<dyn Sink>>,
  shared: Arc<Shared>,
}

impl Worker {
  async fn run(mut self) {
    while let Some(message) = self.rx.recv().await {
      match message {
        Message::Event(event) => {
          self.write(&event).await;
          self.write_gap().await;
        }
        Message::Flush(done) => {
          self.write_gap().await;
          self.close_out().await;
          // The caller may have stopped waiting.
          let _ = done.send(());
        }
      }
    }
    self.write_gap().await;
    self.close_out().await;
  }

  /// Writes the gap of dropped events once the queue is empty, so the gap
  /// follows every event accepted before the drop.
  async fn write_gap(&mut self) {
    let gap = {
      let mut counters = lock(&self.shared.counters);
      (counters.dropped > 0 && self.rx.is_empty()).then(|| {
        let event = gap(&self.session, self.version, &counters, now_ms());
        counters.seq += 1;
        counters.dropped = 0;
        event
      })
    };
    if let Some(gap) = gap {
      self.write(&gap).await;
    }
  }

  /// Writes the timing summary and flushes the sinks.
  async fn close_out(&mut self) {
    #[cfg(feature = "timing")]
    if let Some(timing) = &self.shared.timing {
      let summary = lock(timing).take();
      if let Some(payload) = summary {
        let event = {
          let mut counters = lock(&self.shared.counters);
          let seq = counters.seq;
          counters.seq += 1;
          Event {
            session: self.session.clone(),
            seq,
            time_ms: now_ms(),
            kind: event::TIMING.to_string(),
            version: self.version,
            payload,
          }
        };
        self.write(&event).await;
      }
    }
    for sink in &mut self.sinks {
      if let Err(e) = sink.flush().await {
        tracing::warn!("session {}: trace flush: {e}", self.session);
      }
    }
  }

  async fn write(&mut self, event: &Event) {
    for sink in &mut self.sinks {
      if let Err(e) = sink.write(event).await {
        tracing::warn!("session {}: trace {} #{}: {e}", self.session, event.kind, event.seq);
      }
    }
  }
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
