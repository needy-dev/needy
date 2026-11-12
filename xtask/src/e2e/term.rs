//! A pseudo-terminal the harness types into and reads, as a person at
//! `needy run create -it` or `needy run attach` would.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const ROWS: u16 = 30;
const COLS: u16 = 100;

/// A program on a pseudo-terminal, its output kept raw and as a screen.
pub struct Term {
  child: Box<dyn Child + Send + Sync>,
  writer: Box<dyn Write + Send>,
  // Kept so the terminal stays open while the child runs.
  _master: Box<dyn MasterPty + Send>,
  output: Arc<Mutex<Output>>,
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
    });
    Ok(Self {
      child,
      writer,
      _master: pty.master,
      output,
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

  /// Waits for the program to exit and returns its exit code; kills it
  /// after `timeout`.
  pub fn wait(&mut self, timeout: Duration) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    loop {
      if let Some(status) = self.child.try_wait().context("cannot wait for the terminal's program")? {
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

impl Drop for Term {
  fn drop(&mut self) {
    // A program still running when its cell gives up is killed with it.
    if matches!(self.child.try_wait(), Ok(None)) {
      let _ = self.child.kill();
    }
  }
}
