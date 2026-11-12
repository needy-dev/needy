//! The harness's output: progress lines, the human summary, the JSON
//! document, and the exit code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::checks::{Check, Checks};
use super::host::Host;
use super::matrix::{Cell, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
  Pass,
  Fail,
  /// Should run on this host, but something it needs is missing.
  NotCovered,
  /// Runs on another platform only.
  Elsewhere,
  /// `--list`: would run.
  Planned,
}

impl Status {
  pub fn name(self) -> &'static str {
    match self {
      Status::Pass => "pass",
      Status::Fail => "FAIL",
      Status::NotCovered => "not covered",
      Status::Elsewhere => "elsewhere",
      Status::Planned => "would run",
    }
  }
}

/// What became of one cell.
#[derive(Debug)]
pub struct Outcome {
  pub cell: Cell,
  pub status: Status,
  /// Why it did not run, for a skipped cell.
  pub reason: String,
  pub checks: Vec<Check>,
  pub duration: Duration,
  pub artifacts: Option<PathBuf>,
}

impl Outcome {
  pub fn skipped(cell: Cell, status: Status, reason: String) -> Self {
    Self {
      cell,
      status,
      reason,
      checks: Vec::new(),
      duration: Duration::ZERO,
      artifacts: None,
    }
  }

  pub fn ran(cell: Cell, checks: Checks, duration: Duration, artifacts: Option<PathBuf>) -> Self {
    Self {
      cell,
      status: if checks.passed() {
        Status::Pass
      } else {
        Status::Fail
      },
      reason: String::new(),
      checks: checks.list,
      duration,
      artifacts,
    }
  }

  fn failed(&self) -> impl Iterator<Item = &Check> {
    self.checks.iter().filter(|c| !c.ok)
  }
}

pub struct Report<'a> {
  pub platform: Platform,
  pub host: &'a Host,
  pub notes: &'a [String],
  pub outcomes: &'a [Outcome],
  pub results: &'a Path,
}

/// One line on stderr per finished cell.
pub fn progress(done: usize, total: usize, outcome: &Outcome) {
  eprintln!("e2e: [{done}/{total}] {:<4} {} ({:.1}s)", outcome.status.name(), outcome.cell.id(), outcome.duration.as_secs_f64());
  for check in outcome.failed().take(3) {
    eprintln!("e2e:        - {}: {}", check.name, short(&check.detail, 200));
  }
}

fn short(text: &str, max: usize) -> String {
  let text = text.replace('\n', " ");
  match text.char_indices().nth(max) {
    Some((at, _)) => format!("{}…", &text[..at]),
    None => text,
  }
}
/// The human summary of this host's run on stdout.
pub fn human(report: &Report) {
  print_human("this host", &json(report));
}

/// The human summary of a JSON document from [`json`], from this host or a
/// remote one (`label` names it): a table of cells, then each failure with
/// every check that did not hold, then what was not covered and why.
pub fn print_human(label: &str, doc: &Value) {
  let str_of = |v: &Value| v.as_str().unwrap_or("").to_string();
  println!(
    "e2e on {label}: {} {} as uid {}{}",
    str_of(&doc["platform"]),
    str_of(&doc["arch"]),
    doc["euid"],
    if doc["euid"] == 0 {
      " (root)"
    } else {
      " (not root)"
    }
  );
  for note in doc["notes"].as_array().into_iter().flatten() {
    println!("note: {}", str_of(note));
  }
  let cells: Vec<&Value> = doc["cells"].as_array().map(|c| c.iter().collect()).unwrap_or_default();
  println!();
  println!("{:<12} {:>7}  CELL", "STATUS", "TIME");
  let status_name = |s: &str| {
    match s {
      "pass" => "pass",
      "fail" => "FAIL",
      "not-covered" => "not covered",
      "planned" => "would run",
      other => other,
    }
    .to_string()
  };
  for c in cells.iter().filter(|c| c["status"] != "elsewhere") {
    let ms = c["duration_ms"].as_u64().unwrap_or(0);
    let time = if ms == 0 {
      String::new()
    } else {
      format!("{:.1}s", ms as f64 / 1000.0)
    };
    let reason = str_of(&c["reason"]);
    let reason = if reason.is_empty() {
      reason
    } else {
      format!("  ({})", short(&reason, 120))
    };
    println!("{:<12} {:>7}  {}{reason}", status_name(c["status"].as_str().unwrap_or("")), time, str_of(&c["id"]));
  }
  let failed: Vec<&&Value> = cells.iter().filter(|c| c["status"] == "fail").collect();
  if !failed.is_empty() {
    println!("\nFailures:");
    for c in &failed {
      println!("  {}", str_of(&c["id"]));
      let reason = str_of(&c["reason"]);
      if !reason.is_empty() {
        println!("    - {reason}");
      }
      for check in c["checks"].as_array().into_iter().flatten().filter(|k| k["ok"] == false) {
        println!("    - {}: {}", str_of(&check["name"]), short(&str_of(&check["detail"]), 600));
      }
      if let Some(dir) = c["artifacts"].as_str() {
        println!("    logs: {dir}");
      }
    }
  }
  let mut groups: BTreeMap<(String, String), usize> = BTreeMap::new();
  for c in cells.iter().filter(|c| c["status"] == "elsewhere") {
    *groups.entry((str_of(&c["dimensions"]["driver"]), str_of(&c["reason"]))).or_default() += 1;
  }
  if !groups.is_empty() {
    println!("\nNot covered on this host, other platforms:");
    for ((driver, reason), n) in groups {
      println!("  {driver}: {n} cells ({reason})");
    }
  }
  let t = &doc["totals"];
  println!(
    "\n{} passed, {} failed, {} not covered on this host, {} for other platforms{}",
    t["pass"],
    t["fail"],
    t["not_covered"],
    t["elsewhere"],
    if t["planned"].as_u64().unwrap_or(0) > 0 {
      format!(", {} would run", t["planned"])
    } else {
      String::new()
    }
  );
}

/// The JSON document `-o json` prints.
pub fn json(report: &Report) -> Value {
  let host = report.host;
  let drivers: BTreeMap<String, Value> = host
    .drivers
    .iter()
    .map(|(d, r)| {
      (
        d.name(),
        match r {
          Ok(()) => json!("available"),
          Err(e) => json!(e),
        },
      )
    })
    .collect();
  let cells: Vec<Value> = report
    .outcomes
    .iter()
    .map(|o| {
      let c = &o.cell;
      json!({
        "id": c.id(),
        "status": match o.status { Status::Pass => "pass", Status::Fail => "fail", Status::NotCovered => "not-covered", Status::Elsewhere => "elsewhere", Status::Planned => "planned" },
        "reason": o.reason,
        "duration_ms": o.duration.as_millis() as u64,
        "dimensions": {
          "driver": c.driver.name(),
          "terminal": c.terminal.name(),
          "end": c.end.name(),
          "files": c.files(),
          "chaos": c.chaos(),
          "network": c.network(),
          "user": c.user.name(),
          "executor": c.executor.map(|e| e.name()).unwrap_or("none"),
          "daemon_user": if c.root { "root" } else { "non-root" },
        },
        "checks": o.checks.iter().map(|k| json!({ "name": k.name, "ok": k.ok, "detail": k.detail })).collect::<Vec<_>>(),
        "artifacts": o.artifacts.as_ref().map(|p| p.display().to_string()),
      })
    })
    .collect();
  let count = |s: Status| report.outcomes.iter().filter(|o| o.status == s).count();
  json!({
    "platform": format!("{:?}", report.platform),
    "arch": std::env::consts::ARCH,
    "euid": host.euid,
    "drivers": drivers,
    "container_executor": match &host.container_executor { Ok(()) => json!("available"), Err(e) => json!(e) },
    "host_sshfs": match &host.host_sshfs { Ok(()) => json!("available"), Err(e) => json!(e) },
    "stop_now": host.stop_now,
    "notes": report.notes,
    "results": report.results.display().to_string(),
    "cells": cells,
    "totals": {
      "pass": count(Status::Pass),
      "fail": count(Status::Fail),
      "not_covered": count(Status::NotCovered),
      "elsewhere": count(Status::Elsewhere),
      "planned": count(Status::Planned),
    },
    "exit_code": exit_code(report.outcomes),
  })
}

/// 1 when any cell failed; 2 when none failed but some that belong on this
/// platform did not run; 0 otherwise. Cells of other platforms do not
/// count: another host covers them.
pub fn exit_code(outcomes: &[Outcome]) -> i32 {
  if outcomes.iter().any(|o| o.status == Status::Fail) {
    1
  } else if outcomes.iter().any(|o| o.status == Status::NotCovered) {
    2
  } else {
    0
  }
}
