//! A pseudo-terminal the harness types into and reads, as a person at
//! `needy run create -it` or `needy run attach` would.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const ROWS: u16 = 30;
const COLS: u16 = 100;
/// How long [`Term::wait`] waits, after the program exits, for the reader
/// to read the terminal to its end. Reading what is left takes moments;
/// the bound is for a descendant that keeps the terminal open after the
/// program exits, which would otherwise hold `wait` up forever.
const DRAIN: Duration = Duration::from_secs(5);

/// A program on a pseudo-terminal, its output kept raw and as a screen.
pub struct Term {
  child: Box<dyn Child + Send + Sync>,
  writer: Box<dyn Write + Send>,
  // Kept so the terminal stays open while the child runs.
  _master: Box<dyn MasterPty + Send>,
  output: Arc<Mutex<Output>>,
  /// Set, and notified, once the reader has read the terminal to its end.
  drained: Arc<(Mutex<bool>, Condvar)>,
}

struct Output {
  raw: Vec<u8>,
  screen: vt100::Parser,
}

impl Term {
  /// Starts `program args` on a new 30x100 terminal, with `env` added to
  /// the harness's environment.
  pub fn spawn(program: &Path, args: &[&str], env: &[(String, String)], cwd: &Path) -> Result<Self> {
    let pty = native_pty_system()
      .openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
      })
      .map_err(|e| anyhow!("cannot open a pseudo-terminal: {e}"))?;
    let mut command = CommandBuilder::new(program);
    command.args(args);
    command.cwd(cwd);
    for (key, value) in env {
      command.env(key, value);
    }
    command.env("TERM", "xterm-256color");
    let child = pty.slave.spawn_command(command).map_err(|e| anyhow!("cannot start {}: {e}", program.display()))?;
    // The child holds the slave side now; the parent's copy would keep the
    // terminal open after the child exits.
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
    let writer = pty.master.take_writer().map_err(|e| anyhow!("{e}"))?;
    let output = Arc::new(Mutex::new(Output {
      raw: Vec::new(),
      screen: vt100::Parser::new(std::num::NonZeroU16::new(ROWS).expect("rows"), std::num::NonZeroU16::new(COLS).expect("cols"), 0),
    }));
    let sink = output.clone();
    let drained = Arc::new((Mutex::new(false), Condvar::new()));
    let done = drained.clone();
    thread::spawn(move || {
      let mut buf = [0u8; 4096];
      // EOF or EIO (the child closed the terminal) ends the reader.
      while let Ok(n) = reader.read(&mut buf) {
        if n == 0 {
          break;
        }
        let mut out = sink.lock().unwrap_or_else(|e| e.into_inner());
        out.raw.extend_from_slice(&buf[..n]);
        out.screen.process(&buf[..n]);
      }
      let (flag, ready) = &*done;
      *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
      ready.notify_all();
    });
    Ok(Self {
      child,
      writer,
      _master: pty.master,
      output,
      drained,
    })
  }

  /// Everything the program wrote, escape sequences removed only as far as
  /// a substring search needs: as text.
  pub fn transcript(&self) -> String {
    let out = self.output.lock().unwrap_or_else(|e| e.into_inner());
    String::from_utf8_lossy(&out.raw).into_owned()
  }

  /// The screen as the terminal shows it now.
  pub fn screen(&self) -> String {
    let out = self.output.lock().unwrap_or_else(|e| e.into_inner());
    out.screen.screen().contents()
  }

  /// Waits until the transcript or the screen has `needle` at or after
  /// byte `from` of the transcript; returns where it ended.
  pub fn expect_from(&self, from: usize, needle: &str, timeout: Duration) -> Result<usize> {
    let deadline = Instant::now() + timeout;
    loop {
      let transcript = self.transcript();
      let tail = transcript.get(from..).unwrap_or("");
      if let Some(at) = tail.find(needle) {
        return Ok(from + at + needle.len());
      }
      if from == 0 && self.screen().contains(needle) {
        return Ok(transcript.len());
      }
      if Instant::now() >= deadline {
        bail!("{needle:?} did not appear within {timeout:?}; the screen:\n{}", self.screen().trim_end());
      }
      thread::sleep(Duration::from_millis(50));
    }
  }

  /// Waits for `needle` anywhere in the output.
  pub fn expect(&self, needle: &str, timeout: Duration) -> Result<usize> {
    self.expect_from(0, needle, timeout)
  }

  /// The transcript's length now: a mark for [`Self::expect_from`].
  pub fn mark(&self) -> usize {
    self.output.lock().unwrap_or_else(|e| e.into_inner()).raw.len()
  }

  /// Types `text`.
  pub fn send(&mut self, text: &str) -> Result<()> {
    self.writer.write_all(text.as_bytes()).context("cannot write to the terminal")?;
    self.writer.flush().context("cannot write to the terminal")
  }

  /// Terminates this viewer process, as a CLI crash would. The actor and
  /// its driving program are outside this pseudo-terminal's child lifetime.
  pub fn kill(&mut self) -> Result<()> {
    anyhow::ensure!(self.child.try_wait()?.is_none(), "the viewer exited before the requested termination");
    let pid =
      i32::try_from(self.child.process_id().context("the viewer has no process ID")?).context("the viewer process ID is out of range")?;
    anyhow::ensure!(pid > 0, "the viewer process ID must be positive");
    // Source: https://github.com/wezterm/wezterm/blob/f8921727a11b9f8b073e8c24821d72fd41283500/pty/src/lib.rs#L340-L372
    // Reference: https://docs.rs/portable-pty/0.9.0/portable_pty/trait.ChildKiller.html#tymethod.kill
    // NOTICE: portable-pty sends SIGHUP first, which the CLI forwards to the
    // driving program. A crash must end this viewer without forwarding a signal.
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).context("cannot terminate the viewer")
  }

  /// Waits for the program to exit and returns its exit code; kills it
  /// after `timeout`. Once it has exited, also waits (up to [`DRAIN`]) for
  /// the reader to read the terminal to its end, so the transcript holds
  /// everything the program wrote.
  pub fn wait(&mut self, timeout: Duration) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    loop {
      if let Some(status) = self.child.try_wait().context("cannot wait for the terminal's program")? {
        self.drain(DRAIN);
        return Ok(status.exit_code());
      }
      if Instant::now() >= deadline {
        // Gone already is as good.
        let _ = self.child.kill();
        bail!("the program did not exit within {timeout:?}; the screen:\n{}", self.screen().trim_end());
      }
      thread::sleep(Duration::from_millis(50));
    }
  }
}

impl Term {
  /// Waits up to `timeout` for the reader to reach the terminal's end: EOF,
  /// or EIO once no process holds the terminal's other side.
  fn drain(&self, timeout: Duration) {
    let (flag, ready) = &*self.drained;
    let flag = flag.lock().unwrap_or_else(|e| e.into_inner());
    let (done, _) = ready.wait_timeout_while(flag, timeout, |done| !*done).unwrap_or_else(|e| e.into_inner());
    if !*done {
      // NOTICE: a descendant still holds the terminal open, so its end
      // never comes; the transcript is what was read so far, as before
      // `wait` waited for the reader at all.
      eprintln!("e2e: the terminal stayed open {timeout:?} after its program exited; its transcript may be incomplete");
    }
  }
}

impl Drop for Term {
  fn drop(&mut self) {
    // A program still running when its cell gives up is killed with it.
    if matches!(self.child.try_wait(), Ok(None)) {
      let _ = self.child.kill();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Fixture(std::path::PathBuf);

  impl Fixture {
    fn new() -> Self {
      // NOTICE: a counter too: tests run in parallel, and macOS's clock
      // is only microsecond-precise, so two fixtures could share a time.
      static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
      let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
      let path = std::env::temp_dir().join(format!("needy-viewer-crash-{}-{suffix}-{n}", std::process::id()));
      std::fs::create_dir(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      if let Err(error) = std::fs::remove_dir_all(&self.0) {
        eprintln!("cannot remove the viewer-crash test fixture: {error}");
      }
    }
  }

  // ROOT CAUSE:
  //
  // If a viewer was terminated through portable-pty's kill operation, it
  // received SIGHUP before SIGKILL and could forward that signal to the
  // driving program. A viewer crash must give it no chance to signal work.
  #[test]
  fn killing_a_viewer_does_not_deliver_a_catchable_signal() {
    let fixture = Fixture::new();
    let program = "trap 'printf caught > hangup; exit 0' HUP; printf 'READY\\n'; while IFS= read -r line; do :; done";
    let mut term = Term::spawn(Path::new("sh"), &["-c", program], &[], &fixture.0).unwrap();
    term.expect("READY", Duration::from_secs(5)).unwrap();

    term.kill().unwrap();
    term.wait(Duration::from_secs(5)).unwrap();

    assert!(!fixture.0.join("hangup").exists(), "the viewer handled a signal before its simulated crash");
  }

  // ROOT CAUSE:
  //
  // If a program wrote its last output and exited at once, `wait` returned
  // as soon as the child had exited while the reader thread had not yet
  // taken that output in, so the transcript missed the last lines
  // (`linux-container.runsc/image` missed `image edit: committed sha256:`).
  //
  // Before the fix, `wait` watched only the child.
  // The fix waits until the reader has read the terminal to its end.
  #[test]
  fn wait_returns_after_the_last_output_is_read() {
    let fixture = Fixture::new();
    // Output small enough for the terminal's buffer, so the program exits
    // without waiting for the reader.
    let program = "sleep 0.2; printf 'THE-LAST-LINE\\n'";
    let mut term = Term::spawn(Path::new("sh"), &["-c", program], &[], &fixture.0).unwrap();
    // Stall the reader: it reads the output, then waits for the lock this
    // thread holds until well after the program has exited.
    let output = term.output.clone();
    let (held, hold) = std::sync::mpsc::channel();
    let staller = thread::spawn(move || {
      let _guard = output.lock().unwrap();
      held.send(()).unwrap();
      thread::sleep(Duration::from_millis(800));
      Instant::now()
    });
    hold.recv().unwrap();

    assert_eq!(term.wait(Duration::from_secs(10)).unwrap(), 0);
    let returned = Instant::now();
    let released = staller.join().unwrap();

    assert!(returned >= released, "wait returned before the reader could take the program's last output in");
    assert!(term.transcript().contains("THE-LAST-LINE"), "{:?}", term.transcript());
  }
}
