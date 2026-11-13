//! `cargo xtask tui`: drives a program on a pseudo-terminal from a scenario
//! and records what a person at a terminal would see: the screen at named
//! points, the terminal queries the program sent and whether they were
//! answered, and the modes it left on when it exited.
//!
//! The harness is the outer terminal. It answers queries as a modern
//! terminal emulator does ([`emulator`]), encodes keys, pastes and mouse
//! events for the modes the program turned on ([`input`]), and runs any
//! command: a TUI directly (`docker run -it IMAGE pi`), or through needy
//! (`needy run create -it`, `needy run attach`, `needy run exec -t`), so two
//! reports of the same scenario can be compared with `tui diff`.
//!
//! A scenario is one step per line; `#` starts a comment:
//!
//! ```text
//! wait TEXT [SECS]      wait until the screen shows TEXT (default 30 s)
//! gone TEXT [SECS]      wait until the screen no longer shows TEXT
//! quiet MS [SECS]       wait until the program wrote, then nothing for MS ms
//! type TEXT             type TEXT (escapes: \r \n \t \e \xNN \\)
//! send TEXT             write TEXT's bytes as they are
//! key NAME...           enter, esc, up, ctrl-c, alt-x, ... (see `input::key`)
//! paste TEXT            paste TEXT, bracketed if the program asked
//! mouse ACTION ROW COL  click, scroll-up or scroll-down at a 1-based cell
//! focus in|out          a focus event, if the program asked for them
//! resize ROWS COLS      change the window size
//! snap NAME             record the screen as NAME
//! await NAME [SECS]     wait until the followed session snapped NAME
//!                       (default 120 s)
//! sleep MS              keep following the program for MS ms
//! exit [SECS]           wait for the program to exit
//! ```
//!
//! A step that times out is recorded as failed and the scenario goes on.
//!
//! Two sessions snap the same moments with `--marks` and `--follow`: each
//! `snap NAME` leaves a mark NAME in the session's own `--marks` directory
//! and waits up to 30 s for the same mark in its `--follow` directory. A
//! read-only viewer `await`s each of the typing client's snaps, then snaps.

mod emulator;
mod input;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};

use emulator::{Answers, Emulator};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Run COMMAND on a pseudo-terminal through a scenario and write a JSON
  /// report.
  Run(Run),
  /// Compare two reports of the same scenario: the screens at each snap,
  /// with digits and long hexadecimal IDs masked, the queries and the modes
  /// left on.
  Diff { a: PathBuf, b: PathBuf },
  /// Feed a raw output file (`run --raw`) to the terminal and print the
  /// modes it was left in, as `run` reports them.
  Replay {
    raw: PathBuf,
    #[arg(long, default_value_t = 30)]
    rows: u16,
    #[arg(long, default_value_t = 100)]
    cols: u16,
  },
}

#[derive(clap::Args)]
struct Run {
  /// The scenario file.
  #[arg(long)]
  scenario: PathBuf,
  /// Where to write the JSON report; stdout by default.
  #[arg(long)]
  out: Option<PathBuf>,
  /// A directory where each `snap` leaves a mark that another session's
  /// `await`, or its `snap`, waits for.
  #[arg(long)]
  marks: Option<PathBuf>,
  /// The other session's `--marks`: `await` waits for its marks, and each
  /// `snap` waits for the other session to snap the same name.
  #[arg(long)]
  follow: Option<PathBuf>,
  /// Where to write the program's raw output, for reading it afterwards.
  #[arg(long)]
  raw: Option<PathBuf>,
  #[arg(long, default_value_t = 30)]
  rows: u16,
  #[arg(long, default_value_t = 100)]
  cols: u16,
  /// Which queries the terminal answers.
  #[arg(long, value_enum, default_value = "all")]
  answers: Answers,
  /// Set an environment variable, KEY=VALUE. TERM is xterm-256color and
  /// COLORTERM truecolor unless set here.
  #[arg(short = 'e', long = "env")]
  env: Vec<String>,
  #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
  command: Vec<String>,
  /// The program's working directory; the harness's own by default.
  #[arg(skip)]
  cwd: Option<PathBuf>,
  /// Leave out the line per step on stderr, for a caller that reports the
  /// steps itself (the e2e harness).
  #[arg(skip)]
  silent: bool,
}

pub fn main(args: Args) -> Result<()> {
  match args.command {
    Command::Run(run) => run_scenario(&run),
    Command::Diff { a, b } => diff(&a, &b),
    Command::Replay { raw, rows, cols } => {
      let bytes = std::fs::read(&raw).with_context(|| format!("cannot read {}", raw.display()))?;
      let mut emulator = Emulator::new(rows, cols, Answers::None);
      emulator.process(&bytes);
      let report = json!({ "left_on": emulator.leftover_modes(), "final": emulator.snapshot() });
      println!("{}", serde_json::to_string_pretty(&report)?);
      Ok(())
    }
  }
}

/// A program on a pseudo-terminal, as the harness's terminal shows it.
struct Session {
  emulator: Emulator,
  writer: Box<dyn Write + Send>,
  output: mpsc::Receiver<Vec<u8>>,
  child: Box<dyn portable_pty::Child + Send + Sync>,
  master: Box<dyn portable_pty::MasterPty + Send>,
  raw: Option<std::fs::File>,
  marks: Option<PathBuf>,
  follow: Option<PathBuf>,
  /// When the program last wrote; `None` before its first output.
  last_output: Option<Instant>,
  /// The exit code once the program exited, `-1` for a signal.
  exit: Option<i64>,
  /// The terminal's output reached its end.
  closed: bool,
}

impl Session {
  fn start(run: &Run) -> Result<Self> {
    let pair = native_pty_system()
      .openpty(PtySize {
        rows: run.rows,
        cols: run.cols,
        pixel_width: 0,
        pixel_height: 0,
      })
      .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    // portable-pty looks a program up in PATH unless its path is absolute.
    let program = PathBuf::from(&run.command[0]);
    let program = if program.components().count() > 1 && program.is_relative() {
      std::env::current_dir()?.join(program)
    } else {
      program
    };
    let mut command = CommandBuilder::new(program);
    command.args(&run.command[1..]);
    command.cwd(match &run.cwd {
      Some(cwd) => cwd.clone(),
      None => std::env::current_dir()?,
    });
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    for pair in &run.env {
      let (k, v) = pair.split_once('=').with_context(|| format!("{pair:?} is not KEY=VALUE"))?;
      command.env(k, v);
    }
    let child = pair.slave.spawn_command(command).map_err(|e| anyhow::anyhow!("cannot start {:?}: {e:#}", run.command[0]))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let writer = pair.master.take_writer().map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let (tx, output) = mpsc::channel();
    std::thread::spawn(move || {
      let mut buf = vec![0u8; 64 * 1024];
      loop {
        match reader.read(&mut buf) {
          Ok(0) | Err(_) => break,
          Ok(n) => {
            if tx.send(buf[..n].to_vec()).is_err() {
              break;
            }
          }
        }
      }
    });
    let raw = run.raw.as_ref().map(std::fs::File::create).transpose().context("cannot create the raw output file")?;
    Ok(Self {
      emulator: Emulator::new(run.rows, run.cols, run.answers),
      writer,
      output,
      child,
      master: pair.master,
      raw,
      marks: run.marks.clone(),
      follow: run.follow.clone(),
      last_output: None,
      exit: None,
      closed: false,
    })
  }

  /// Follows the program for up to `wait`; returns after the first output.
  fn poll(&mut self, wait: Duration) -> Result<()> {
    if self.exit.is_none()
      && let Some(status) = self.child.try_wait()?
    {
      self.exit = Some(if status.signal().is_some() {
        -1
      } else {
        i64::from(status.exit_code())
      });
    }
    if self.closed {
      std::thread::sleep(wait.min(Duration::from_millis(20)));
      return Ok(());
    }
    match self.output.recv_timeout(wait) {
      Ok(bytes) => {
        self.last_output = Some(Instant::now());
        if let Some(raw) = self.raw.as_mut() {
          raw.write_all(&bytes)?;
        }
        let replies = self.emulator.process(&bytes);
        if !replies.is_empty() {
          self.write(&replies)?;
        }
      }
      Err(mpsc::RecvTimeoutError::Timeout) => {}
      Err(mpsc::RecvTimeoutError::Disconnected) => self.closed = true,
    }
    Ok(())
  }

  fn write(&mut self, bytes: &[u8]) -> Result<()> {
    // A program that exited, or is exiting, no longer reads: the terminal
    // answers EIO once its other end is closed. The input is dropped and
    // the steps that wait for the exit tell what happened.
    if let Err(e) = self.writer.write_all(bytes).and_then(|()| self.writer.flush()) {
      eprintln!("tui: input dropped: {e}");
    }
    Ok(())
  }

  /// Follows the program until `done` or `timeout`; whether `done` held.
  fn until(&mut self, timeout: Duration, mut done: impl FnMut(&Self) -> bool) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
      if done(self) {
        return Ok(true);
      }
      let now = Instant::now();
      if now >= deadline {
        return Ok(false);
      }
      self.poll((deadline - now).min(Duration::from_millis(50)))?;
    }
  }

  fn shows(&self, text: &str) -> bool {
    self.emulator.screen().contents().contains(text)
  }

  fn step(&mut self, words: &[String], snaps: &mut BTreeMap<String, Value>) -> Result<bool> {
    let secs = |at: usize, default: u64| -> Result<Duration> {
      Ok(Duration::from_secs_f64(words.get(at).map(|s| s.parse::<f64>()).transpose()?.unwrap_or(default as f64)))
    };
    let arg = |at: usize| -> Result<&String> { words.get(at).with_context(|| format!("{} needs an argument", words[0])) };
    let ok = match words[0].as_str() {
      "wait" => {
        let text = arg(1)?.clone();
        self.until(secs(2, 30)?, |s| s.shows(&text))?
      }
      "gone" => {
        let text = arg(1)?.clone();
        self.until(secs(2, 30)?, |s| !s.shows(&text))?
      }
      "quiet" => {
        let quiet = Duration::from_millis(arg(1)?.parse()?);
        self.until(secs(2, 30)?, |s| s.last_output.is_some_and(|at| at.elapsed() >= quiet) || s.closed)?
      }
      "sleep" => {
        let until = Duration::from_millis(arg(1)?.parse()?);
        self.until(until, |s| s.closed && s.exit.is_some())?;
        true
      }
      "type" => {
        let text = String::from_utf8(input::unescape(arg(1)?)?)?;
        let flags = self.emulator.keyboard_flags();
        // One key at a time, as a person types.
        for c in text.chars() {
          self.write(&input::text(&c.to_string(), flags))?;
          self.until(Duration::from_millis(15), |_| false)?;
        }
        true
      }
      "send" => {
        let bytes = input::unescape(arg(1)?)?;
        self.write(&bytes)?;
        true
      }
      "key" => {
        for name in &words[1..] {
          let screen = self.emulator.screen();
          let bytes = input::key(name, screen.application_cursor(), self.emulator.keyboard_flags())?;
          self.write(&bytes)?;
          self.until(Duration::from_millis(80), |_| false)?;
        }
        true
      }
      "paste" => {
        let text = String::from_utf8(input::unescape(arg(1)?)?)?;
        let bytes = input::paste(&text, self.emulator.screen().bracketed_paste());
        self.write(&bytes)?;
        true
      }
      "mouse" => {
        let (row, col) = (arg(2)?.parse()?, arg(3)?.parse()?);
        match input::mouse(arg(1)?, row, col, self.emulator.screen())? {
          Some(bytes) => {
            self.write(&bytes)?;
            true
          }
          // The program did not ask for the mouse.
          None => false,
        }
      }
      "focus" => {
        if self.emulator.focus() {
          let bytes: &[u8] = if arg(1)? == "in" {
            b"\x1b[I"
          } else {
            b"\x1b[O"
          };
          self.write(bytes)?;
          true
        } else {
          false
        }
      }
      "resize" => {
        let (rows, cols): (u16, u16) = (arg(1)?.parse()?, arg(2)?.parse()?);
        self.emulator.resize(rows, cols);
        self
          .master
          .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
          })
          .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        true
      }
      "snap" => {
        let name = arg(1)?;
        snaps.insert(name.clone(), self.emulator.snapshot());
        if let Some(marks) = &self.marks {
          std::fs::write(marks.join(name), "").with_context(|| format!("cannot mark {name}"))?;
        }
        // The other session snaps the same moment before this one goes on.
        match self.follow.clone() {
          Some(follow) => self.until(Duration::from_secs(30), |_| follow.join(name).exists())?,
          None => true,
        }
      }
      "await" => {
        let follow = self.follow.clone().context("await needs --follow")?;
        let mark = follow.join(arg(1)?);
        self.until(secs(2, 120)?, |_| mark.exists())?
      }
      "exit" => {
        let timeout = secs(1, 30)?;
        let exited = self.until(timeout, |s| s.exit.is_some())?;
        // What the program wrote last, such as its terminal reset.
        self.until(Duration::from_millis(500), |s| s.closed)?;
        exited
      }
      other => bail!("unknown step {other:?}"),
    };
    Ok(ok)
  }
}

fn run_scenario(run: &Run) -> Result<()> {
  let scenario = std::fs::read_to_string(&run.scenario).with_context(|| format!("cannot read {}", run.scenario.display()))?;
  let report = play(run, &scenario)?;
  let text = serde_json::to_string_pretty(&report)?;
  match &run.out {
    Some(path) => std::fs::write(path, text + "\n").with_context(|| format!("cannot write {}", path.display()))?,
    None => println!("{text}"),
  }
  Ok(())
}

/// Runs `command` (the program and its arguments) on a pseudo-terminal of
/// 30 by 100 cells, through `scenario` (the steps, one per line), with
/// `env` (`KEY=VALUE`) added and `cwd` as its working directory, the raw
/// output written to `raw` when given, and returns the report `tui run`
/// writes. For callers in this crate: the e2e harness's TUI cells.
pub(crate) fn report(scenario: &str, command: Vec<String>, env: Vec<String>, cwd: &std::path::Path, raw: Option<PathBuf>) -> Result<Value> {
  let run = Run {
    scenario: PathBuf::new(),
    out: None,
    marks: None,
    follow: None,
    raw,
    rows: 30,
    cols: 100,
    answers: Answers::All,
    env,
    command,
    cwd: Some(cwd.to_path_buf()),
    silent: true,
  };
  play(&run, scenario)
}

/// Runs `run`'s command through the steps of `scenario` and returns the
/// report.
fn play(run: &Run, scenario: &str) -> Result<Value> {
  let mut session = Session::start(run)?;
  let started = Instant::now();
  let mut steps = Vec::new();
  let mut snaps = BTreeMap::new();
  for (n, line) in scenario.lines().enumerate() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let words = shell_words::split(line).with_context(|| format!("line {}: {line}", n + 1))?;
    let at = started.elapsed().as_millis();
    let ok = session.step(&words, &mut snaps).with_context(|| format!("line {}: {line}", n + 1))?;
    if !run.silent {
      eprintln!("tui: {} {line}", if ok { "ok  " } else { "FAIL" });
    }
    steps.push(json!({ "line": n + 1, "step": line, "ok": ok, "at_ms": at }));
  }
  let exited = session.exit.is_some();
  if !exited {
    // NOTICE: a program still running at the end is killed so the harness
    // never leaves one behind; the report says it did not exit.
    if let Err(e) = session.child.kill() {
      eprintln!("tui: cannot kill the program: {e}");
    }
  }
  let report = json!({
    "command": run.command,
    "size": [run.rows, run.cols],
    "answers": format!("{:?}", run.answers),
    "steps": steps,
    "failed": steps.iter().filter(|s| s["ok"] == false).count(),
    "snaps": snaps,
    "terminal": session.emulator.report(),
    "exited": exited,
    "exit": session.exit,
    "final": session.emulator.snapshot(),
    // Meaningful once the program exited: what it left on for the shell.
    "left_on": session.emulator.leftover_modes(),
  });
  Ok(report)
}

/// `line` with each run of digits, and each word of eight or more
/// hexadecimal digits, masked as `#`: times, counts, sizes and IDs.
fn mask(line: &str) -> String {
  let mut out = String::new();
  for word in line.split_inclusive(|c: char| !c.is_ascii_hexdigit()) {
    let (body, end) = match word.char_indices().last() {
      Some((at, c)) if !c.is_ascii_hexdigit() => (&word[..at], &word[at..]),
      _ => (word, ""),
    };
    if body.len() >= 8 && body.chars().any(|c| c.is_ascii_digit()) {
      out.push('#');
    } else {
      let mut digits = false;
      for c in body.chars() {
        if c.is_ascii_digit() {
          if !digits {
            out.push('#');
          }
          digits = true;
        } else {
          digits = false;
          out.push(c);
        }
      }
    }
    out.push_str(end);
  }
  out
}

fn diff(a: &PathBuf, b: &PathBuf) -> Result<()> {
  let read = |p: &PathBuf| -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(p).with_context(|| format!("cannot read {}", p.display()))?)?)
  };
  let out = compare(&read(a)?, &read(b)?);
  println!("{}", serde_json::to_string_pretty(&out)?);
  Ok(())
}

/// What `tui diff` prints for reports `a` and `b`: each snap of `a`,
/// `"equal"` or how `b`'s differs, then the queries, the modes left on,
/// the exits and the failed steps of both.
pub(crate) fn compare(a: &Value, b: &Value) -> Value {
  let mut summary = serde_json::Map::new();
  let empty = serde_json::Map::new();
  let snaps_a = a["snaps"].as_object().unwrap_or(&empty);
  let snaps_b = b["snaps"].as_object().unwrap_or(&empty);
  for (name, snap_a) in snaps_a {
    let Some(snap_b) = snaps_b.get(name) else {
      summary.insert(name.clone(), json!("missing in b"));
      continue;
    };
    let lines = |s: &Value| -> Vec<String> {
      s["lines"].as_array().map(|l| l.iter().map(|x| mask(x.as_str().unwrap_or_default())).collect()).unwrap_or_default()
    };
    let (la, lb) = (lines(snap_a), lines(snap_b));
    let mut differing = Vec::new();
    for i in 0..la.len().max(lb.len()) {
      let (x, y) = (la.get(i).map_or("", String::as_str), lb.get(i).map_or("", String::as_str));
      if x != y {
        differing.push(json!({ "row": i, "a": x, "b": y }));
      }
    }
    let mut other = Vec::new();
    for key in ["modes", "cursor", "wide_cells", "title"] {
      if snap_a[key] != snap_b[key] {
        other.push(json!({ "what": key, "a": snap_a[key], "b": snap_b[key] }));
      }
    }
    // Truecolor shown on one side only means a program chose fewer colors.
    let truecolor = |s: &Value| s["colors"]["rgb"].as_u64().unwrap_or(0) > 0;
    if truecolor(snap_a) != truecolor(snap_b) {
      other.push(json!({ "what": "truecolor", "a": snap_a["colors"], "b": snap_b["colors"] }));
    }
    summary.insert(
      name.clone(),
      if differing.is_empty() && other.is_empty() {
        json!("equal")
      } else {
        json!({ "rows": differing, "other": other })
      },
    );
  }
  let kinds = |r: &Value| -> Vec<String> {
    let mut kinds: Vec<String> = r["terminal"]["queries"]
      .as_array()
      .map(|q| {
        q.iter()
          .map(|q| {
            format!(
              "{}{}",
              q["kind"].as_str().unwrap_or_default(),
              if q["answered"] == true {
                ""
              } else {
                " (unanswered)"
              }
            )
          })
          .collect()
      })
      .unwrap_or_default();
    kinds.sort();
    kinds.dedup();
    kinds
  };
  json!({
    "snaps": summary,
    "queries": { "a": kinds(a), "b": kinds(b) },
    "left_on": { "a": a["left_on"], "b": b["left_on"] },
    "exit": { "a": a["exit"], "b": b["exit"] },
    "failed_steps": { "a": a["failed"], "b": b["failed"] },
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn masking_hides_numbers_and_ids_but_keeps_words() {
    assert_eq!(mask("run 3f2a9c1b7e00 started at 12:04"), "run # started at #:#");
    assert_eq!(mask("deadbeef cafe"), "deadbeef cafe");
  }

  fn report(script: &str, command: &[&str], answers: Answers) -> Value {
    let dir = std::env::temp_dir().join(format!("needy-tui-test-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let scenario = dir.join("scenario");
    let out = dir.join("report.json");
    std::fs::write(&scenario, script).unwrap();
    let run = Run {
      scenario,
      out: Some(out.clone()),
      raw: None,
      marks: None,
      follow: None,
      rows: 10,
      cols: 40,
      answers,
      env: Vec::new(),
      command: command.iter().map(|s| s.to_string()).collect(),
      cwd: None,
      silent: false,
    };
    run_scenario(&run).unwrap();
    let report = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    report
  }

  // ROOT CAUSE:
  //
  // If two of these tests started in the same clock tick (macOS's clock
  // counts microseconds), they shared a directory and one read the other's
  // scenario (`unknown step ":"`), because the suffix was the time alone.
  //
  // The suffix now also counts the directories this process made.
  fn rand_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{nanos}-{n}")
  }

  // A program that asks for the primary device attributes and prints what
  // it read back gets the harness's answer, and the query is recorded.
  #[test]
  fn queries_are_answered_and_recorded() {
    let program = r#"stty raw -echo; printf '\033[c'; r=$(dd bs=1 count=9 2>/dev/null | od -An -c | tr -d ' \n'); stty sane; echo "got:$r"; printf '\033[?2004h\033[?1004h'"#;
    let report = report("wait got: 10\nexit 10\n", &["sh", "-c", program], Answers::All);
    let screen = report["final"]["lines"].to_string();
    assert!(screen.contains(r"got:033[?62;22c"), "{screen}");
    assert_eq!(report["terminal"]["queries"][0]["kind"], "DA1");
    assert_eq!(report["terminal"]["queries"][0]["answered"], true);
    assert_eq!(report["left_on"], json!(["bracketed_paste", "focus"]));
  }

  #[test]
  fn a_terminal_that_does_not_answer_records_the_query_as_unanswered() {
    let report = report("exit 10\n", &["sh", "-c", r"printf '\033]11;?\007'"], Answers::None);
    assert_eq!(report["terminal"]["queries"][0]["kind"], "OSC 11");
    assert_eq!(report["terminal"]["queries"][0]["answered"], false);
  }
}
