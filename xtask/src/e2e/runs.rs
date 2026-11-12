//! Runs as the CLI sees them: create, wait, end each way a run can end,
//! and read back `run ls`, the console and the report.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::checks::{Checks, Console};
use super::env::CellEnv;
use super::matrix::End;

const CLI: Duration = Duration::from_secs(120);
/// How long an actor may take from `run create` to the program's marker:
/// a cold image pull or a first VM boot included.
const READY: Duration = Duration::from_secs(180);

/// Creates a run of `profile` (no terminal) and returns its ID.
pub fn create(env: &mut CellEnv, profile: &Path, extra: &[&str]) -> Result<String> {
  let profile = profile.display().to_string();
  let mut args = vec!["run", "create", "--profile", profile.as_str()];
  args.extend_from_slice(extra);
  let out = env.needy(&args, READY)?;
  if !out.ok() {
    bail!("run create failed: {}", out.brief());
  }
  let id = out.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  if id.is_empty() || id.contains(' ') {
    bail!("run create printed no run ID: {}", out.brief());
  }
  env.runs.push(id.clone());
  Ok(id)
}

/// The run's row in `run ls -o json`.
pub fn row(env: &CellEnv, id: &str) -> Result<Value> {
  let out = env.needy(&["run", "ls", "-o", "json"], CLI)?;
  if !out.ok() {
    bail!("run ls failed: {}", out.brief());
  }
  let rows: Value = serde_json::from_str(&out.stdout).with_context(|| format!("run ls printed no JSON: {}", out.brief()))?;
  rows
    .as_array()
    .and_then(|rows| rows.iter().find(|r| r["id"] == id || r["name"] == id))
    .cloned()
    .with_context(|| format!("run ls has no run {id}"))
}

pub fn state(row: &Value) -> &str {
  row["state"].as_str().unwrap_or("")
}

fn ended(state: &str) -> bool {
  matches!(state, "finished" | "stopped" | "failed")
}

/// Waits until the run has ended, and returns its row.
pub fn wait_end(env: &CellEnv, id: &str, timeout: Duration) -> Result<Value> {
  let deadline = Instant::now() + timeout;
  loop {
    let first = row(env, id)?;
    if ended(state(&first)) {
      // The row a moment after the end, so a state the daemon writes just
      // after the run's (the actor's) is not judged mid-update.
      std::thread::sleep(Duration::from_secs(1));
      return row(env, id);
    }
    if Instant::now() > deadline {
      bail!("run {id} did not end within {timeout:?}: {first}");
    }
    std::thread::sleep(Duration::from_millis(300));
  }
}

/// Waits until the actor's state in `run ls` is one of `states`.
pub fn wait_actor(env: &CellEnv, id: &str, states: &[&str], timeout: Duration) -> Result<Value> {
  let deadline = Instant::now() + timeout;
  loop {
    let row = row(env, id)?;
    if row["actor"].as_str().is_some_and(|a| states.contains(&a)) {
      return Ok(row);
    }
    if ended(state(&row)) {
      bail!("run {id} ended while waiting for its actor to be {}: {row}", states.join(" or "));
    }
    if Instant::now() > deadline {
      bail!("the actor of run {id} was not {} within {timeout:?}: {row}", states.join(" or "));
    }
    std::thread::sleep(Duration::from_millis(200));
  }
}

/// The actor's console so far.
pub fn console(env: &CellEnv, id: &str) -> Result<Console> {
  let out = env.needy(&["run", "logs", id], CLI)?;
  if !out.ok() {
    bail!("run logs failed: {}", out.brief());
  }
  Ok(Console::new(out.stdout))
}

/// Waits until the console shows `marker`.
pub fn wait_marker(env: &CellEnv, id: &str, marker: &str) -> Result<Console> {
  let deadline = Instant::now() + READY;
  loop {
    if let Ok(console) = console(env, id)
      && console.contains(marker)
    {
      return Ok(console);
    }
    let row = row(env, id)?;
    if ended(state(&row)) {
      let console = console(env, id).unwrap_or_default();
      bail!("run {id} ended before printing {marker}: {row}; console: {}", tail(&console.text));
    }
    if Instant::now() > deadline {
      bail!("run {id} did not print {marker} within {READY:?}");
    }
    std::thread::sleep(Duration::from_millis(300));
  }
}

pub fn tail(text: &str) -> String {
  let lines: Vec<&str> = text.lines().collect();
  lines[lines.len().saturating_sub(12)..].join(" | ")
}

/// `needy report RUN -o json --level detailed`.
pub fn report(env: &CellEnv, id: &str) -> Result<Value> {
  let out = env.needy(&["report", id, "-o", "json", "--level", "detailed"], CLI)?;
  if !out.ok() {
    bail!("report failed: {}", out.brief());
  }
  serde_json::from_str(&out.stdout).with_context(|| format!("report printed no JSON: {}", out.brief()))
}

/// The kinds of the report's timeline events.
pub fn event_kinds(report: &Value) -> Vec<String> {
  report["events"].as_array().map(|e| e.iter().filter_map(|e| e["kind"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// How many commands the report routed to `executor`.
pub fn routed_to(report: &Value, executor: &str) -> u64 {
  report["commands"]["routed"]
    .as_array()
    .and_then(|r| r.iter().find(|r| r["executor"] == executor))
    .and_then(|r| r["commands"].as_u64())
    .unwrap_or(0)
}

/// Ends run `id` as `end` says, once its program printed `E2E-READY` (for
/// every end but `Exit`), and returns its final row and the console from
/// before the end. Records the live state on the way: the actor's state,
/// and the sockets while the run is up.
pub fn end_run(env: &mut CellEnv, id: &str, end: End, checks: &mut Checks, timeout: Duration) -> Result<(Value, Console)> {
  if end == End::Exit {
    let row = wait_end(env, id, timeout)?;
    let console = console(env, id).unwrap_or_default();
    return Ok((row, console));
  }
  let console = wait_marker(env, id, "E2E-READY")?;
  let live = row(env, id)?;
  checks.eq("run ls while live: state", "running", state(&live));
  // Without a terminal the actor has no process service: `configured` is
  // as far as it gets (TERMS_AND_CONCEPTS.md, Run).
  checks.check(
    "run ls while live: actor configured or ready",
    matches!(live["actor"].as_str(), Some("configured" | "ready")),
    live["actor"].to_string(),
  );
  super::invariants::sockets(env, checks, "while the run is live");
  match end {
    End::Exit => unreachable!(),
    End::Stop | End::StopNow => {
      let mut args = vec!["run", "stop", id];
      if end == End::StopNow {
        args.push("--now");
      }
      let out = env.needy(&args, timeout)?;
      checks.check(format!("`needy {}` succeeds", args.join(" ")), out.ok(), out.brief());
    }
    End::DaemonKill => {
      env.kill_daemon()?;
      env.start_daemon()?;
    }
  }
  let row = wait_end(env, id, timeout)?;
  let console = super::runs::console(env, id).unwrap_or(console);
  Ok((row, console))
}

/// Asserts the run's final row for `end`: its state, its actor and, for a
/// program that exited, its exit code.
pub fn check_ended(checks: &mut Checks, end: End, row: &Value, exit: i64) {
  match end {
    End::Exit => {
      checks.eq("run ls: state", "finished", state(row));
      checks.eq("run ls: exit code", Some(exit), row["exit_code"].as_i64());
    }
    End::Stop | End::StopNow => {
      checks.eq("run ls: state", "stopped", state(row));
    }
    End::DaemonKill => {
      checks.eq("run ls after restart: state", "failed", state(row));
      checks.check(
        "run ls after restart: the failure names the restart",
        row["failure"].as_str().is_some_and(|f| f.contains("restart")),
        row["failure"].to_string(),
      );
    }
  }
  // An ended run has no live actor: `exited`, or none at all (`-` in the
  // table, TERMS_AND_CONCEPTS.md, Run).
  checks.check(
    "run ls: the actor of an ended run is exited or gone",
    matches!(row["actor"].as_str(), None | Some("exited")),
    row["actor"].to_string(),
  );
  // A finished run keeps its executors until it is stopped; a stopped or
  // failed one has released (or lost) them.
  if end != End::Exit {
    checks.check(
      "run ls: every executor of a stopped or failed run is released",
      row["executors"].as_array().is_none_or(|e| e.iter().all(|e| e["released"] == true || e["lost"] == true)),
      row["executors"].to_string(),
    );
  }
}
