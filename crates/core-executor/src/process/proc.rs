//! One process: input, output fan-out to attached clients, the replay buffer,
//! the terminal-state model of a process on a terminal, and the exit.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use needy_common::process::Exit;
use needy_common::time::now_ms;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::{mpsc, watch};

use bytes::Bytes;

use super::screen::Screen;
use super::{Input, Output, Spec, TermFallback, platform};

/// Output kept per process for clients that attach later.
// NOTICE: the CLI note documents 256 KiB; a client attaching after more
// output than this sees only the most recent part.
const REPLAY_BYTES: usize = 256 * 1024;

/// Output events queued per subscriber: the owner's before the process
/// waits for it, a viewer's before it skips output (a terminal) or is
/// dropped (pipes).
const SUBSCRIBER_QUEUE: usize = 64;

pub struct Proc {
  pub id: String,
  pub argv: Vec<String>,
  pub pid: i64,
  pub tty: bool,
  pub started_at_ms: i64,
  pub run_id: String,
  /// The terminal type the executor set instead of the client's.
  pub term_fallback: Option<TermFallback>,
  input: mpsc::UnboundedSender<Input>,
  /// The replay buffer, the terminal model and the subscribers, under one
  /// lock so a new subscriber's snapshot and its first live event never
  /// overlap. Shared with each terminal viewer's [`Live`], which takes its
  /// pending redraw from it.
  fanout: Arc<Mutex<Fanout>>,
  /// Orders each event's delivery to the owner; held across the owner's
  /// send, never across a viewer's.
  owner_order: tokio::sync::Mutex<()>,
  exited: watch::Receiver<Option<Exit>>,
  finished_at: OnceLock<Instant>,
  /// The first signal a client sent the process.
  signalled: OnceLock<String>,
  /// Who may type and whose window size the terminal has.
  seats: Mutex<Seats>,
}

/// The clients of a process, for arbitration: the client that last attached
/// or sent input owns the terminal's size; when it leaves, the next most
/// recent takes over. A read-only client never owns it and cannot type.
// TODO(terminal-arbitration): an exclusive writer (`--takeover`, herdr's
// direct attach) is not offered; the terminal note's open questions keep it
// until an owner asks for it.
struct Seats {
  next: u64,
  /// Advances at each attach and input, ordering the clients by activity.
  clock: u64,
  /// The size the terminal has now; `None` for pipes.
  window: Option<(u16, u16)>,
  clients: Vec<Seat>,
  /// The answers to queries the program sent while a client terminal could
  /// answer them, if no client typed since: the executor's own answers,
  /// which it sends if the last such client leaves first.
  unanswered: Vec<u8>,
}

struct Seat {
  id: u64,
  window: Option<(u16, u16)>,
  active: u64,
  read_only: bool,
}

impl Seats {
  /// The size the terminal should have now, when it differs from the one it
  /// has: that of the most recently active client that has one.
  fn resize(&mut self) -> Option<(u16, u16)> {
    self.window?;
    let owner = self.clients.iter().filter(|c| !c.read_only && c.window.is_some()).max_by_key(|c| c.active)?;
    let window = owner.window.filter(|w| Some(*w) != self.window)?;
    self.window = Some(window);
    Some(window)
  }

  /// Whether a client terminal that can type, and so answers the program's
  /// terminal queries, is attached.
  fn answering(&self) -> bool {
    self.clients.iter().any(|c| !c.read_only && c.window.is_some())
  }
}

/// One client of a process: its input goes through [`Client::send`], which
/// arbitrates the window size. Dropping it leaves the process to the other
/// clients.
pub struct Client {
  proc: Arc<Proc>,
  id: u64,
  read_only: bool,
}

impl Client {
  /// Sends input as this client: typing makes it the size's owner; a resize
  /// is kept as its size and applied while it owns the size. A read-only
  /// client's input is dropped.
  pub fn send(&self, input: Input) {
    if self.read_only {
      tracing::debug!("process {}: input from a read-only client refused", self.proc.id);
      return;
    }
    let resize = {
      let mut seats = self.proc.lock_seats();
      let clock = seats.clock + 1;
      let Some(seat) = seats.clients.iter_mut().find(|c| c.id == self.id) else {
        return;
      };
      match &input {
        Input::Stdin(_) => seat.active = clock,
        Input::Resize(rows, cols) => seat.window = Some(((*rows).max(1), (*cols).max(1))),
        Input::Eof | Input::Signal(_) => {}
      }
      if matches!(input, Input::Stdin(_)) {
        seats.clock = clock;
        // A terminal answers a query at once, before anything typed after.
        seats.unanswered.clear();
      }
      seats.resize()
    };
    if let Some((rows, cols)) = resize {
      self.proc.send(Input::Resize(rows, cols));
    }
    if !matches!(input, Input::Resize(..)) {
      self.proc.send(input);
    }
  }
}

impl Drop for Client {
  fn drop(&mut self) {
    let (resize, answers) = {
      let mut seats = self.proc.lock_seats();
      seats.clients.retain(|c| c.id != self.id);
      let answers = if seats.answering() {
        Vec::new()
      } else {
        std::mem::take(&mut seats.unanswered)
      };
      (seats.resize(), answers)
    };
    if let Some((rows, cols)) = resize {
      self.proc.send(Input::Resize(rows, cols));
    }
    // The program still waits for the answers this client did not send.
    if !answers.is_empty() {
      self.proc.send(Input::Stdin(answers));
    }
  }
}

struct Fanout {
  events: VecDeque<Output>,
  bytes: usize,
  /// What a process on a terminal shows, fed with the replay buffer's
  /// bytes; `None` for pipes.
  screen: Option<Screen>,
  finished: Option<Exit>,
  /// The client that started the process; it alone may hold the process
  /// back.
  owner: Option<mpsc::Sender<Output>>,
  viewers: Vec<Viewer>,
  next_viewer: u64,
}

struct Viewer {
  id: u64,
  tx: mpsc::Sender<Output>,
  behind: Arc<AtomicBool>,
  /// A terminal viewer whose queue filled: it skips output and has one
  /// redraw pending, which it gets as soon as it can take it, from
  /// [`Live`] once it has read its queue or from [`Proc::emit`] at the next
  /// output that finds room, whichever comes first.
  stale: bool,
}

/// Who subscribes to a process's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscriber {
  /// The client that started the process. A slow owner slows the process
  /// down instead of losing output (the CLI note documents this).
  Owner,
  /// Any other client, such as one that attached. A viewer never holds back
  /// the process or another client. On a terminal, a viewer that cannot
  /// keep up skips output and gets the screen drawn again from the model as
  /// soon as it has read what was queued, whether or not the program writes
  /// again; on pipes it is dropped, [`Subscription::fell_behind`].
  Viewer,
}

/// A subscriber's output: what happened before it subscribed, then what
/// happens after. The two never overlap.
pub struct Subscription {
  pub past: Vec<Output>,
  pub live: Live,
  behind: Arc<AtomicBool>,
}

/// A subscriber's live output. A terminal viewer that skipped output gets
/// its pending redraw here once its queue is empty, so it shows the
/// program's screen again although the program writes nothing more.
// NOTICE: this follows herdr, which gives each client a render lane of one
// frame ("Capacity is one so slow clients cannot build lag"), skips a frame
// a full lane cannot take and marks that client for a full render, and has
// the client's writer report when it takes a frame from the lane, so the
// full render follows "even if pane output stops". needy streams the
// program's bytes, not frames: the queue holds output, a viewer that skips
// some has one redraw pending, and the redraw is drawn when the viewer has
// read the queue, not on a timer. herdr also renders at most every 16 ms;
// needy needs no cadence, since the viewer's own read asks for the redraw.
// Source: https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/client_transport.rs#L121-L127
// (the lanes), #L951-L971 (`client_writer_loop`, `ClientWriterDrained`),
// https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless/render.rs#L963-L991
// (`try_send`; `defer_full_render` when the lane is full), #L561-L569 (an
// observer waiting for its redraw) and
// https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/server/headless.rs#L2651-L2656
// (the drained lane asks for the pending full render);
// https://github.com/ogulcancelik/herdr/blob/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde/src/app/mod.rs#L39
// (`MIN_RENDER_INTERVAL`, 16 ms).
// Reference: the terminal note's "A lagging terminal viewer"
// (`docs/ai/references/cli/2026-11-10-terminal-attach-design.md`).
pub struct Live {
  rx: mpsc::Receiver<Output>,
  /// For a terminal viewer: the fan-out and the viewer's id in it.
  catch_up: Option<(Weak<Mutex<Fanout>>, u64)>,
}

impl Live {
  /// The next event; `None` once the subscription has ended.
  // Cancel safe, as `mpsc::Receiver::recv` is: a pending redraw is taken
  // only in the poll that returns it.
  pub async fn recv(&mut self) -> Option<Output> {
    match self.try_recv() {
      Ok(event) => Some(event),
      Err(TryRecvError::Disconnected) => None,
      Err(TryRecvError::Empty) => self.rx.recv().await,
    }
  }

  /// The next event if there is one now.
  pub fn try_recv(&mut self) -> Result<Output, TryRecvError> {
    match self.rx.try_recv() {
      Err(TryRecvError::Empty) => self.redraw().ok_or(TryRecvError::Empty),
      other => other,
    }
  }

  /// The pending redraw of a viewer that skipped output and has read its
  /// queue. Taken under the fan-out's lock, so no output is queued between
  /// the check and the redraw, and what the program writes next follows it.
  fn redraw(&self) -> Option<Output> {
    let (fanout, id) = self.catch_up.as_ref()?;
    let fanout = fanout.upgrade()?;
    let mut fanout = lock(&fanout);
    let Fanout {
      screen, viewers, ..
    } = &mut *fanout;
    let screen = screen.as_ref()?;
    // A queue that is not empty here filled again since `try_recv` found
    // it empty: what it holds is older than a redraw, so it is read first.
    let viewer = viewers.iter_mut().find(|v| v.id == *id && v.stale && v.tx.capacity() == v.tx.max_capacity())?;
    viewer.stale = false;
    Some(Output::Stdout(Bytes::from(screen.redraw())))
  }
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
    let screen = spec.tty.filter(|_| tty).map(Screen::new);
    let (input, input_rx) = mpsc::unbounded_channel();
    let (exit_tx, exited) = watch::channel(None);
    let proc = Arc::new(Self {
      id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
      argv: spec.argv.clone(),
      pid,
      tty,
      started_at_ms: now_ms(),
      run_id: spec.run_id.clone(),
      term_fallback,
      input,
      fanout: Arc::new(Mutex::new(Fanout {
        events: VecDeque::new(),
        bytes: 0,
        screen,
        finished: None,
        owner: None,
        viewers: Vec::new(),
        next_viewer: 0,
      })),
      owner_order: tokio::sync::Mutex::new(()),
      exited,
      finished_at: OnceLock::new(),
      signalled: OnceLock::new(),
      seats: Mutex::new(Seats {
        next: 0,
        clock: 0,
        window: spec.tty.filter(|_| tty),
        clients: Vec::new(),
        unanswered: Vec::new(),
      }),
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
      // The first signal is the one that ends the process where signals
      // terminate; see `ended`.
      self.signalled.get_or_init(|| name.clone());
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

  /// Subscribes to output. With `replay`, what came before comes first: for
  /// a viewer of a terminal, the screen drawn from the model; otherwise the
  /// buffered output, which the owner, subscribing as the process starts,
  /// gets whole. A process has one [`Subscriber::Owner`]; a later one
  /// replaces it.
  pub fn subscribe(&self, replay: bool, subscriber: Subscriber) -> Subscription {
    let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
    let behind = Arc::new(AtomicBool::new(false));
    let mut catch_up = None;
    let mut fanout = lock(&self.fanout);
    let mut past: Vec<Output> = match (&fanout.screen, replay, subscriber) {
      (_, false, _) => Vec::new(),
      (Some(screen), true, Subscriber::Viewer) => vec![Output::Stdout(Bytes::from(screen.redraw()))],
      (_, true, _) => fanout.events.iter().cloned().collect(),
    };
    if let Some(exit) = &fanout.finished {
      past.push(Output::Exit(exit.clone()));
    } else {
      match subscriber {
        Subscriber::Owner => fanout.owner = Some(tx),
        Subscriber::Viewer => {
          fanout.next_viewer += 1;
          let id = fanout.next_viewer;
          if fanout.screen.is_some() {
            catch_up = Some((Arc::downgrade(&self.fanout), id));
          }
          fanout.viewers.push(Viewer {
            id,
            tx,
            behind: behind.clone(),
            stale: false,
          });
        }
      }
    }
    Subscription {
      past,
      live: Live { rx, catch_up },
      behind,
    }
  }

  /// Records `event` and fans it out: to each viewer without waiting, and to
  /// the owner, whom the process waits for. No lock is held while it waits.
  pub(super) async fn emit(&self, event: Output) {
    let _order = self.owner_order.lock().await;
    let (owner, answers) = {
      let mut fanout = lock(&self.fanout);
      let answers = fanout.record(&event);
      let last = matches!(event, Output::Exit(_));
      let Fanout {
        screen, viewers, ..
      } = &mut *fanout;
      // Drawn once per event, for every stale viewer that has room again.
      let mut redraw: Option<Bytes> = None;
      viewers.retain_mut(|viewer| {
        if viewer.stale {
          // A stale viewer whose queue has room again by the next output
          // gets its pending redraw here, before that output; one whose
          // queue empties first gets it from `Live`. The exit needs room for
          // the redraw and itself.
          let room = viewer.tx.capacity();
          if room < if last { 2 } else { 1 } {
            if last {
              viewer.behind.store(true, Ordering::SeqCst);
            }
            return !last;
          }
          if let Some(screen) = screen.as_ref() {
            let bytes = redraw.get_or_insert_with(|| Bytes::from(screen.redraw()));
            if viewer.tx.try_send(Output::Stdout(bytes.clone())).is_err() {
              return false;
            }
          }
          viewer.stale = false;
          // The redraw already shows this output.
          if !last {
            return true;
          }
        }
        match viewer.tx.try_send(event.clone()) {
          Ok(()) => !last,
          Err(TrySendError::Full(_)) if screen.is_some() && !last => {
            viewer.stale = true;
            true
          }
          Err(TrySendError::Full(_)) => {
            viewer.behind.store(true, Ordering::SeqCst);
            false
          }
          Err(TrySendError::Closed(_)) => false,
        }
      });
      let owner = if last {
        fanout.owner.take()
      } else {
        fanout.owner.clone()
      };
      (owner, answers)
    };
    if !answers.is_empty() {
      self.answer(answers);
    }
    if let Some(owner) = owner
      && owner.send(event).await.is_err()
    {
      // The owner left; the process no longer waits for anyone.
      let mut fanout = lock(&self.fanout);
      if fanout.owner.as_ref().is_some_and(|current| current.same_channel(&owner)) {
        fanout.owner = None;
      }
    }
  }

  /// Answers the program's terminal queries the model can answer, `answers`
  /// (see [`Screen::process`]), unless a client terminal that can type is
  /// attached: that terminal shows the program and answers them itself. The
  /// executor answers only for a program nobody watches, or one only
  /// read-only viewers watch, whose input is dropped.
  // NOTICE: the executor never answers alongside a client terminal, and it
  // does not filter the terminal's answer out of the client's input instead:
  // an answer cannot be told from a key (`CSI 1 ; 2 R` is also Shift+F3,
  // https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1388-L1396),
  // so either way a second answer or a lost key would reach the program.
  // ConPTY asks once when it starts with `PSEUDOCONSOLE_INHERIT_CURSOR` and
  // reads no input until it is answered. portable-pty 0.9.0 always passes
  // that flag
  // (https://docs.rs/crate/portable-pty/0.9.0/source/src/win/psuedocon.rs#83-91),
  // so not asking would need a fork of it; answering also serves any
  // program, on any platform, that asks while nobody watches.
  // Source: https://github.com/microsoft/terminal/blob/v1.25.3131.0/src/winconpty/winconpty.cpp#L448-L460
  // and https://github.com/microsoft/terminal/blob/v1.25.3131.0/src/host/VtIo.cpp#L183-L193
  // Reference: https://learn.microsoft.com/en-us/windows/console/createpseudoconsole
  // (retrieved 2026-11-11; the page has no versions).
  // TODO(terminal-arbitration): two client terminals that can type both
  // answer, as they did before the executor answered at all; the program
  // reads two answers. Only one owner of the terminal's input would fix
  // that; see the terminal note's open questions.
  fn answer(&self, answers: Vec<u8>) {
    {
      let mut seats = self.lock_seats();
      if seats.answering() {
        seats.unanswered.extend_from_slice(&answers);
        return;
      }
    }
    self.send(Input::Stdin(answers));
  }

  /// Publishes the exit to subscribers, then to waiters; from then on the
  /// retention period runs.
  pub(super) async fn finish(&self, ends: &watch::Sender<Option<Exit>>, exit: Exit) {
    let exit = ended(exit, self.signalled.get().map(String::as_str), cfg!(windows));
    self.emit(Output::Exit(exit.clone())).await;
    self.finished_at.get_or_init(Instant::now);
    // `send_replace` stores the exit even when no waiter is subscribed.
    ends.send_replace(Some(exit));
  }

  /// Joins the process as a client with its terminal's `window`, which it
  /// then owns unless it is `read_only`.
  pub fn join(self: &Arc<Self>, window: Option<(u16, u16)>, read_only: bool) -> Client {
    let (id, resize) = {
      let mut seats = self.lock_seats();
      seats.next += 1;
      seats.clock += 1;
      let (id, active) = (seats.next, seats.clock);
      seats.clients.push(Seat {
        id,
        window: window.map(|(rows, cols)| (rows.max(1), cols.max(1))),
        active,
        read_only,
      });
      (id, seats.resize())
    };
    if let Some((rows, cols)) = resize {
      self.send(Input::Resize(rows, cols));
    }
    Client {
      proc: self.clone(),
      id,
      read_only,
    }
  }

  fn lock_seats(&self) -> std::sync::MutexGuard<'_, Seats> {
    // NOTICE: every change under the lock is a push, a remove or a field
    // store, so a panic elsewhere leaves the seats whole.
    self.seats.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Follows a window size change in the terminal model.
  pub(super) fn resize_screen(&self, rows: u16, cols: u16) {
    if let Some(screen) = lock(&self.fanout).screen.as_mut() {
      screen.resize(rows, cols);
    }
  }
}

fn lock(fanout: &Mutex<Fanout>) -> std::sync::MutexGuard<'_, Fanout> {
  // NOTICE: every change under the lock is one push, pop or swap, so a
  // panic elsewhere leaves the buffer whole.
  fanout.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Fanout {
  /// Records `event`; returns the answers to the cursor position queries in
  /// it.
  fn record(&mut self, event: &Output) -> Vec<u8> {
    let mut answers = Vec::new();
    match event {
      Output::Stdout(b) | Output::Stderr(b) => {
        if let Some(screen) = self.screen.as_mut() {
          answers = screen.process(b);
        }
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
    answers
  }
}

/// The exit to report for a process that exited with `exit` after needy sent
/// it `signalled`. Where signals terminate the process instead of being
/// delivered (`terminate`, Windows), the termination is reported as the
/// signal that asked for it, as on Unix, not as the program's own exit code.
// NOTICE: `taskkill /F` ends a process with `TerminateProcess` and exit code
// 1 (`platform/windows.rs`, `signal_group`), so only that code is taken for
// the termination; any other code is the program's, which exited before it
// took effect.
// Reference: https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/taskkill
pub(super) fn ended(exit: Exit, signalled: Option<&str>, terminate: bool) -> Exit {
  match signalled {
    Some(signal) if terminate && exit.code == Some(1) && exit.signal.is_empty() => Exit {
      code: None,
      signal: signal.to_string(),
    },
    _ => exit,
  }
}

/// The exit reported when the process's exit could not be observed.
pub(super) fn lost() -> Exit {
  Exit {
    code: None,
    signal: "SIGKILL".into(),
  }
}
