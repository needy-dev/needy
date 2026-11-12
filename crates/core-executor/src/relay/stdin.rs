//! Local stdin for a relay, read so that a relay that ends leaves it alone:
//! a process that keeps running, or a parent that shares the terminal, gets
//! the input that comes after.
//!
//! NOTICE: tokio's `stdin` is not used. Its `read` runs on the runtime's
//! blocking pool, where a read of a terminal nobody types at cannot be
//! interrupted: the read outlives the relay, takes the next keystroke for a
//! process that has exited, and a runtime drop waits for it. The guest
//! proxy hung after every routed command until the next keystroke because
//! of this. Stdin is read on a thread of its own instead, which a dropped
//! [`Stop`] ends without reading anything more.
//!
//! O_NONBLOCK is not set on stdin either: it belongs to the open file
//! description, which the parent shell shares, so a relay that died with it
//! set would leave the shell's reads failing with `EAGAIN`.

use tokio::sync::mpsc;

/// What the stdin thread read, in order; an empty chunk is the end of file,
/// and a read error counts as one.
pub(super) type Chunks = mpsc::Receiver<Vec<u8>>;

/// Ends the stdin thread when dropped.
pub(super) struct Stop {
  // Dropping the write end makes the read end readable (end of file),
  // which wakes the thread's `poll`.
  #[cfg(unix)]
  _wake: std::io::PipeWriter,
}

/// Starts reading local stdin on a thread of its own.
pub(super) fn read() -> std::io::Result<(Chunks, Stop)> {
  // NOTICE: 4 chunks of up to 32 KiB bound what is read ahead of the
  // stream; the thread waits for room before it reads more.
  let (chunks_tx, chunks) = mpsc::channel(4);
  #[cfg(unix)]
  {
    let (woken, wake) = std::io::pipe()?;
    std::thread::Builder::new().name("relay-stdin".into()).spawn(move || unix::pump(&woken, &chunks_tx))?;
    Ok((chunks, Stop { _wake: wake }))
  }
  #[cfg(not(unix))]
  {
    std::thread::Builder::new().name("relay-stdin".into()).spawn(move || blocking::pump(&chunks_tx))?;
    Ok((chunks, Stop {}))
  }
}

#[cfg(unix)]
mod unix {
  use std::io::PipeReader;
  use std::os::fd::AsFd;

  use nix::errno::Errno;
  use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
  use tokio::sync::mpsc;

  /// Waits in `poll` for stdin or the stop, and reads stdin only when it
  /// has input: a read then returns at once with what is there, so the
  /// thread is never blocked in a `read` when the stop comes, and input
  /// that arrives after the stop stays in the terminal or pipe.
  pub(super) fn pump(stop: &PipeReader, chunks: &mpsc::Sender<Vec<u8>>) {
    let stdin = std::io::stdin();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
      let mut fds = [
        PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
        PollFd::new(stop.as_fd(), PollFlags::POLLIN),
      ];
      match poll(&mut fds, PollTimeout::NONE) {
        Ok(_) => {}
        Err(Errno::EINTR) => continue,
        Err(e) => {
          tracing::debug!("relay: cannot wait for stdin: {e}");
          // A relay that is gone needs no end of file.
          let _ = chunks.blocking_send(Vec::new());
          return;
        }
      }
      // The stop wins over input that arrived with it: that input is for
      // whoever reads stdin after the relay.
      if fds[1].any().unwrap_or(true) {
        return;
      }
      // NOTICE: macOS `poll` answers POLLNVAL for character devices other
      // than terminals and pseudo-terminals (`/dev/null`; checked on
      // macOS 27.2), and Linux answers POLLHUP or POLLERR at the end of a
      // pipe; any answer is taken as "read now". A device `poll` does not
      // support is then read blocking, as before.
      if !fds[0].any().unwrap_or(true) {
        continue;
      }
      let n = match nix::unistd::read(stdin.as_fd(), &mut buf) {
        Ok(n) => n,
        // Another reader took the input first, or the parent set stdin
        // nonblocking: wait again.
        Err(Errno::EINTR | Errno::EAGAIN) => continue,
        Err(e) => {
          tracing::debug!("relay: stdin: {e}");
          0
        }
      };
      // The relay is gone when nobody receives; the end of file was the
      // last chunk.
      if chunks.blocking_send(buf[..n].to_vec()).is_err() || n == 0 {
        return;
      }
    }
  }
}

#[cfg(not(unix))]
mod blocking {
  use std::io::Read;

  use tokio::sync::mpsc;

  /// Reads stdin in blocking reads until the end of file or until nobody
  /// receives.
  // NOTICE: a Windows console or pipe read cannot wait on a stop as `poll`
  // does, so this thread is left blocked in its read when the relay ends:
  // it is not the runtime's, so nothing waits for it, but it takes the
  // next input if the process lives on. The CLI exits right after a relay.
  // An overlapped read or `CancelSynchronousIo` would stop it; that waits
  // for terminals on Windows (see `terminal::interactive`).
  pub(super) fn pump(chunks: &mpsc::Sender<Vec<u8>>) {
    let mut stdin = std::io::stdin().lock();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
      let n = match stdin.read(&mut buf) {
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
        Err(e) => {
          tracing::debug!("relay: stdin: {e}");
          0
        }
      };
      if chunks.blocking_send(buf[..n].to_vec()).is_err() || n == 0 {
        return;
      }
    }
  }
}
