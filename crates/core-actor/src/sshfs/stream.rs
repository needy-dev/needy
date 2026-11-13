//! A workspace mount's SFTP stream, kept across lost connections. sshfs in
//! passive mode speaks SFTP on its stdin and stdout and cannot reconnect by
//! itself (its `reconnect` option needs the ssh it starts). When the exec
//! stream to `sftp-server` is lost (a runner's tunnel dropped, the control
//! plane lost the runner), this bridge starts a new `sftp-server`, replays
//! sshfs's `SSH_FXP_INIT` to it and keeps its `SSH_FXP_VERSION` from sshfs,
//! so the mount, and every process's working directory in it, stays.
//!
//! What a new `sftp-server` cannot know fails cleanly instead of hanging:
//! the requests in flight when the stream was lost are answered with
//! `SSH_FX_CONNECTION_LOST` (sshfs reports EIO), and a file handle the old
//! server opened is unknown to the new one, which fails its requests too.
//! Paths work again at once. A stream that does not come back within the
//! window ends the mount: sshfs exits, and the mount point reports that
//! its transport is not connected.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use needy_api_client::Executor;
use needy_api_client::exec::{Event, ExecStream, Input};
use needy_api_proto::v1::ExecStart;
use needy_common::sftp::{SSH_FX_CONNECTION_LOST, SSH_FXP_INIT, SSH_FXP_VERSION, packet_type, request_id, status_reply, take_packets};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

/// How long a mount waits for its SFTP stream to come back.
// NOTICE: 2 minutes, as a runner keeps a run's processes for 2 minutes after
// it lost the control plane (`needy_daemon::runner::DISCONNECT_GRACE`, which
// this crate cannot name): a runner that comes back within it reconnects
// the mount; one that does not has ended the run's processes anyway.
pub(crate) const RECONNECT_WINDOW: Duration = Duration::from_secs(120);

// NOTICE: retries start soon (a tunnel that reconnects at once) and back
// off to every 5 s, so a runner that is back is found within 5 s.
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);

// NOTICE: 256 KiB holds several of sshfs's largest SFTP writes (64 KiB
// each by default).
const READ: usize = 256 * 1024;

/// What both directions share.
#[derive(Default)]
struct Session {
  /// sshfs's `SSH_FXP_INIT`, replayed to every new server.
  init: Option<Vec<u8>>,
  /// Requests sent to the current server and not answered yet.
  pending: HashSet<u32>,
  /// Counts lost streams: a request sent on a stream of an older
  /// generation is answered as lost.
  generation: u64,
  /// Set after a replayed INIT: the new server's VERSION is not sshfs's.
  swallow_version: bool,
}

/// The current server's input and its generation; `None` while
/// reconnecting.
type Current = (Option<Input>, u64);

/// Carries SFTP between sshfs (`from_sshfs`, `to_sshfs`) and `sftp-server`
/// on the executor, starting with `first`, which `start` started; on a lost
/// stream it starts `start` again for up to `window`. Returns when sshfs
/// exits, the server exits, or the window passes; `to_sshfs` is then
/// dropped, so sshfs sees its input end.
pub(crate) async fn bridge<R, W>(
  executor: Arc<dyn Executor>,
  start: ExecStart,
  first: ExecStream,
  from_sshfs: R,
  to_sshfs: W,
  target: String,
  window: Duration,
) where
  R: AsyncRead + Unpin + Send + 'static,
  W: AsyncWrite + Unpin + Send + 'static,
{
  let session = Arc::new(Mutex::new(Session::default()));
  let (current_tx, current) = watch::channel::<Current>((Some(first.input()), 0));
  // One writer to sshfs, so replies and answers for lost requests never
  // interleave inside a packet.
  let (replies, mut to_write) = mpsc::channel::<Vec<u8>>(64);
  let writer = {
    let target = target.clone();
    tokio::spawn(async move {
      let mut to_sshfs = to_sshfs;
      while let Some(bytes) = to_write.recv().await {
        if let Err(e) = async {
          to_sshfs.write_all(&bytes).await?;
          to_sshfs.flush().await
        }
        .await
        {
          tracing::warn!("mount {target}: writing to sshfs: {e}");
          break;
        }
      }
    })
  };
  let outbound = tokio::spawn(outbound(from_sshfs, current, session.clone(), replies.clone(), target.clone()));
  inbound(executor, start, first, current_tx, session, replies, &target, window).await;
  // The server side is over: sshfs gets no more replies.
  outbound.abort();
  if let Err(e) = writer.await
    && !e.is_cancelled()
  {
    tracing::warn!("mount {target}: the writer to sshfs failed: {e}");
  }
}

/// sshfs's requests, packet by packet, to the current server.
async fn outbound<R: AsyncRead + Unpin>(
  mut from_sshfs: R,
  mut current: watch::Receiver<Current>,
  session: Arc<Mutex<Session>>,
  replies: mpsc::Sender<Vec<u8>>,
  target: String,
) {
  let mut buf = vec![0u8; READ];
  let mut framed = Vec::new();
  loop {
    let n = match from_sshfs.read(&mut buf).await {
      Ok(0) => break,
      Ok(n) => n,
      Err(e) => {
        tracing::warn!("mount {target}: reading sshfs: {e}");
        break;
      }
    };
    framed.extend_from_slice(&buf[..n]);
    // Whole packets only: a new server must never get the second half of
    // a packet whose first half went to the old one.
    for packet in take_packets(&mut framed) {
      let id = request_id(&packet);
      // Wait out a reconnect; a closed channel means the bridge is over.
      let Ok(now) = current.wait_for(|(input, _)| input.is_some()).await.map(|c| c.clone()) else {
        return;
      };
      let (Some(input), generation) = now else {
        continue;
      };
      let stale = {
        let mut s = lock(&session);
        if packet_type(&packet) == Some(SSH_FXP_INIT) && s.init.is_none() {
          s.init = Some(packet.clone());
        }
        let stale = s.generation != generation;
        if !stale && let Some(id) = id {
          s.pending.insert(id);
        }
        stale
      };
      if stale {
        // Lost after we looked: answer it as lost, as the reconnect did
        // for those before it.
        if let Some(id) = id
          && replies.send(lost(id)).await.is_err()
        {
          return;
        }
        continue;
      }
      if input.stdin(packet).await.is_err() {
        // The stream is gone. If its loss was handled already, this
        // request missed the answer for lost ones: give it its own.
        let missed = {
          let mut s = lock(&session);
          s.generation != generation && id.is_some_and(|id| s.pending.remove(&id))
        };
        if missed
          && let Some(id) = id
          && replies.send(lost(id)).await.is_err()
        {
          return;
        }
      }
    }
  }
  // sshfs exited (the mount was detached): end the server's input, so it
  // exits and the stream ends with it.
  tracing::debug!("mount {target}: sshfs exited");
  let input = current.borrow().0.clone();
  if let Some(input) = input
    && input.eof().await.is_err()
  {
    tracing::debug!("mount {target}: the SFTP stream had ended already");
  }
}

/// The server's replies to sshfs; on a lost stream, answers the requests in
/// flight and starts a new server.
#[allow(clippy::too_many_arguments)]
async fn inbound(
  executor: Arc<dyn Executor>,
  start: ExecStart,
  first: ExecStream,
  current: watch::Sender<Current>,
  session: Arc<Mutex<Session>>,
  replies: mpsc::Sender<Vec<u8>>,
  target: &str,
  window: Duration,
) {
  let mut stream = first;
  let mut framed = Vec::new();
  loop {
    let lost = match stream.next().await {
      Ok(Some(Event::Stdout(bytes))) => {
        framed.extend_from_slice(&bytes);
        let mut out = Vec::new();
        {
          let mut s = lock(&session);
          for packet in take_packets(&mut framed) {
            if s.swallow_version && packet_type(&packet) == Some(SSH_FXP_VERSION) {
              s.swallow_version = false;
              continue;
            }
            if let Some(id) = request_id(&packet) {
              s.pending.remove(&id);
            }
            out.extend(packet);
          }
        }
        if !out.is_empty() && replies.send(out).await.is_err() {
          return;
        }
        continue;
      }
      Ok(Some(Event::Exited(exit))) => {
        tracing::info!("mount {target}: sftp-server exited ({exit:?}); the mount ends");
        answer_pending(&session, &replies).await;
        return;
      }
      Ok(Some(Event::Detached)) => {
        // Mount streams never request detach; losing their replies ends
        // the mount instead of leaving sshfs waiting for an answer.
        tracing::warn!("mount {target}: the SFTP stream detached unexpectedly");
        answer_pending(&session, &replies).await;
        return;
      }
      Ok(Some(Event::Started(_) | Event::Stderr(_) | Event::Trace(_))) => continue,
      Ok(None) => "the stream closed".to_string(),
      Err(e) => e.to_string(),
    };
    tracing::warn!("mount {target}: the SFTP stream was lost ({lost}); reconnecting for up to {window:?}");
    // Lost: no reply comes for what is in flight.
    let generation = {
      let mut s = lock(&session);
      s.generation += 1;
      s.generation
    };
    current.send_replace((None, generation));
    answer_pending(&session, &replies).await;
    framed.clear();
    let Some(next) = reconnect(executor.as_ref(), &start, window).await else {
      tracing::warn!("mount {target}: the SFTP stream did not come back within {window:?}; the mount ends");
      return;
    };
    let init = {
      let mut s = lock(&session);
      s.swallow_version = s.init.is_some();
      s.init.clone()
    };
    // The new server hears INIT before any request sshfs sends next.
    if let Some(init) = init
      && next.input().stdin(init).await.is_err()
    {
      tracing::debug!("mount {target}: the new SFTP stream closed at once");
    }
    tracing::info!("mount {target}: the SFTP stream is back");
    current.send_replace((Some(next.input()), generation));
    stream = next;
  }
}

/// Starts the server again until it starts or `window` passes.
async fn reconnect(executor: &dyn Executor, start: &ExecStart, window: Duration) -> Option<ExecStream> {
  let deadline = tokio::time::Instant::now() + window;
  let mut delay = RETRY_MIN;
  loop {
    match ExecStream::start(executor, start.clone()).await {
      Ok(stream) => return Some(stream),
      Err(e) => tracing::debug!("mount {}: starting sftp-server again: {e}", start.cwd),
    }
    let now = tokio::time::Instant::now();
    if now >= deadline {
      return None;
    }
    tokio::time::sleep(delay.min(deadline - now)).await;
    delay = (delay * 2).min(RETRY_MAX);
  }
}

/// Answers every request in flight as lost.
async fn answer_pending(session: &Mutex<Session>, replies: &mpsc::Sender<Vec<u8>>) {
  let ids: Vec<u32> = lock(session).pending.drain().collect();
  for id in ids {
    if replies.send(lost(id)).await.is_err() {
      return;
    }
  }
}

fn lost(id: u32) -> Vec<u8> {
  status_reply(id, SSH_FX_CONNECTION_LOST, "needy: the connection to the executor was lost")
}

fn lock(session: &Mutex<Session>) -> std::sync::MutexGuard<'_, Session> {
  session.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;
