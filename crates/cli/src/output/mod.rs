//! Output formats for people (borderless tables) and tools (JSON). Rows are
//! typed structs; each command defines its own.

pub(crate) mod progress;
mod table;

pub use progress::Progress;

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::Serialize;

static QUIET: AtomicBool = AtomicBool::new(false);

/// Enables or disables `--quiet` mode, suppressing non-error status messages
/// and animated progress on stderr.
pub(crate) fn set_quiet(quiet: bool) {
  QUIET.store(quiet, Ordering::Relaxed);
}

/// Whether `--quiet` mode is active.
pub(crate) fn is_quiet() -> bool {
  QUIET.load(Ordering::Relaxed)
}

/// Whether `NO_COLOR` is unset or empty (<https://no-color.org>).
fn color_allowed() -> bool {
  std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

/// Whether `TERM` is unset or not `"dumb"`.
fn term_capable() -> bool {
  std::env::var_os("TERM").is_none_or(|value| value != "dumb")
}

/// Whether stdout is an interactive terminal that supports ANSI styling.
pub(crate) fn stdout_color() -> bool {
  std::io::stdout().is_terminal() && color_allowed() && term_capable()
}

/// Whether stderr is an interactive terminal that supports ANSI styling.
pub(crate) fn stderr_color() -> bool {
  std::io::stderr().is_terminal() && color_allowed() && term_capable()
}

/// Whether stderr can render live animated progress (spinners and bars).
pub(crate) fn stderr_animated() -> bool {
  !is_quiet() && !crate::logging::stderr_active() && stderr_color()
}

/// Formats an epoch millisecond timestamp as RFC 3339 seconds.
pub(crate) fn time(ms: i64) -> String {
  let at = SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0));
  humantime::format_rfc3339_seconds(at).to_string()
}

/// Formats how long ago `ms` was on this machine's clock, to the second.
pub(crate) fn ago(ms: i64) -> String {
  let seconds = u64::try_from(needy_common::time::now_ms().saturating_sub(ms) / 1000).unwrap_or(0);
  format!("{} ago", humantime::format_duration(Duration::from_secs(seconds)))
}

/// Formats a byte count for people, in binary units.
pub(crate) fn bytes(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
  let mut value = bytes as f64;
  let mut unit = 0;
  while value >= 1024.0 && unit < UNITS.len() - 1 {
    value /= 1024.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{bytes} B")
  } else {
    format!("{value:.1} {}", UNITS[unit])
  }
}

/// As `print!`, but returns the write's error instead of panicking, so a
/// closed stdout ends the command through [`crate::cli::run`].
macro_rules! out {
  ($($arg:tt)*) => {
    $crate::output::write(format_args!($($arg)*))
  };
}

/// As `println!`, but returns the write's error instead of panicking.
macro_rules! outln {
  () => {
    $crate::output::write(format_args!("\n"))
  };
  ($($arg:tt)*) => {
    $crate::output::write(format_args!("{}\n", format_args!($($arg)*)))
  };
}

pub(crate) use {out, outln};

/// Writes to stdout. Every command prints through here (`out!`, `outln!`),
/// never with `print!`, which panics when the reader is gone.
pub fn write(args: std::fmt::Arguments) -> std::io::Result<()> {
  std::io::stdout().lock().write_fmt(args)
}

/// A successful interactive step on stdout. Redirected output stays plain text,
/// and `NO_COLOR` has the same meaning as for rendered run reports.
pub fn success(message: &str) -> std::io::Result<()> {
  let style = if stdout_color() {
    anstyle::AnsiColor::Green.on_default()
  } else {
    anstyle::Style::new()
  };
  outln!("{style}{message}{style:#}")
}

/// Confirms a mutation on stderr without contaminating script/data stdout.
/// Suppressed in `--quiet` mode.
pub fn acknowledge(message: &str) -> std::io::Result<()> {
  if is_quiet() {
    return Ok(());
  }
  let style = if stderr_color() {
    anstyle::AnsiColor::Green.on_default()
  } else {
    anstyle::Style::new()
  };
  writeln!(std::io::stderr().lock(), "{style}{message}{style:#}")
}

/// Writes an unprefixed human detail line to stderr. Suppressed in `--quiet`
/// mode.
pub fn detail(args: std::fmt::Arguments) -> std::io::Result<()> {
  if is_quiet() {
    return Ok(());
  }
  writeln!(std::io::stderr().lock(), "{args}")
}

/// Writes an interactive prompt or guidance block to stderr (not suppressed by
/// `--quiet`).
pub fn stderr_ln(args: std::fmt::Arguments) {
  let _ = writeln!(std::io::stderr().lock(), "{args}");
}

/// An infrastructure error label on stderr. Redirected diagnostics and
/// NO_COLOR stay plain; child-process output does not pass through here.
pub fn error(message: impl std::fmt::Display) {
  let style = if stderr_color() {
    anstyle::AnsiColor::Red.on_default().bold()
  } else {
    anstyle::Style::new()
  };
  eprintln!("{style}error:{style:#} {message}");
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum OutputFormat {
  #[default]
  Table,
  Json,
}

/// A row of a table: its header and its cells, in the same order.
pub trait Row: Serialize {
  const HEADER: &'static [&'static str];
  fn cells(&self) -> Vec<String>;
}

/// Prints rows as a table, or as a JSON array of objects.
pub fn print<R: Row>(format: OutputFormat, rows: &[R]) -> Result<()> {
  match format {
    OutputFormat::Table => out!("{}", table::render(R::HEADER, rows.iter().map(Row::cells)))?,
    OutputFormat::Json => outln!("{}", serde_json::to_string_pretty(rows)?)?,
  }
  Ok(())
}

/// Prints a table of rows without a JSON form, for sections of a text report.
pub fn print_table<R: Row>(rows: &[R]) -> std::io::Result<()> {
  out!("{}", table::render(R::HEADER, rows.iter().map(Row::cells)))
}

/// Prints a general message on stderr, such as progress, a warning, or a
/// handled outcome. Infrastructure errors use [`error`] instead. Suppressed in
/// `--quiet` mode.
pub fn message(message: impl std::fmt::Display) {
  if is_quiet() {
    return;
  }
  eprintln!("needy: {message}");
}

/// Prints a message after a terminal session the user left, such as a
/// detach. It starts on a line of its own.
pub fn after_terminal(message: impl std::fmt::Display) {
  eprint!("{}", after_terminal_line(message));
}

/// The bytes of [`after_terminal`]: the program's prompt may have left the
/// cursor mid-line, and the terminal is out of raw mode by now, so `\r\n`
/// starts the message at the first column of the next line.
fn after_terminal_line(message: impl std::fmt::Display) -> String {
  format!("\r\nneedy: {message}\n")
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
