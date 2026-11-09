//! `needy report`: a fixed-format markdown summary of one session.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use anyhow::Result;

use crate::trace;

/// Commands whose only purpose is to learn about the machine.
const IDENTITY: &[&str] = &[
  "uname",
  "lscpu",
  "dmidecode",
  "systemd-detect-virt",
  "hostnamectl",
  "sw_vers",
  "sysctl",
  "lspci",
  "sensors",
  "nproc",
  "free",
  "uptime",
  "hostname",
  "cat /proc/cpuinfo",
  "cat /proc/meminfo",
];

fn base(argv: &str) -> String {
  let first = argv.split_whitespace().next().unwrap_or_default();
  Path::new(first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn top(counts: &BTreeMap<String, usize>, n: usize) -> String {
  let mut v: Vec<_> = counts.iter().collect();
  v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
  v.iter().take(n).map(|(k, c)| format!("{k} ({c})")).collect::<Vec<_>>().join(", ")
}

pub async fn render(db: &Path, session: &str, detailed: bool) -> Result<String> {
  let loaded = trace::load(db, session).await?;
  let info = &loaded.session;
  let id = info["id"].as_str().unwrap_or_default().to_string();
  let created = info["created_ms"].as_i64().unwrap_or(0);
  let last = loaded.events.last().map(|e| e.0).unwrap_or(created);
  let ended = info["ended_ms"].as_i64().unwrap_or(last);
  let secs = ((ended - created).max(0)) / 1000;

  let mut routed = 0;
  let mut kept = 0;
  let mut denied = 0;
  let mut faults = 0;
  let mut cli = 0;
  let mut failed = 0;
  let mut model_calls = 0;
  let mut probes: BTreeMap<String, usize> = BTreeMap::new();
  let mut executor_cmds: BTreeMap<String, usize> = BTreeMap::new();
  let mut actor_net: BTreeMap<String, usize> = BTreeMap::new();
  let mut executor_net: BTreeMap<String, usize> = BTreeMap::new();
  let mut denials = Vec::new();
  let mut net_denials = Vec::new();
  let mut actor_exit = String::new();
  let mut slowest: Vec<(i64, String)> = Vec::new();
  for (_, kind, attrs) in &loaded.events {
    let get = |k: &str| attrs.get(k).cloned().unwrap_or_default();
    match kind.as_str() {
      "exec.decide" => {
        match get("target").as_str() {
          "actor" => kept += 1,
          "deny" => {
            denied += 1;
            denials.push(format!("`{}`: {}", get("argv"), get("deny")));
          }
          _ => routed += 1,
        }
        let argv = get("argv");
        let b = base(&argv);
        if IDENTITY.iter().any(|p| *p == b || argv.contains(p)) {
          *probes.entry(b).or_default() += 1;
        }
      }
      "exec.start" => {
        if get("origin") == "cli" {
          cli += 1;
        }
        if get("origin") != "actor-mount" {
          *executor_cmds.entry(format!("{} on {}", base(&get("argv")), get("executor"))).or_default() += 1;
        }
      }
      "exec.exit" => {
        let code = get("code");
        if (!code.is_empty() && code != "0") || !get("signal").is_empty() {
          failed += 1;
        }
        if let Ok(ms) = get("ms").parse::<i64>() {
          slowest.push((ms, get("argv")));
        }
      }
      "fault.injected" => faults += 1,
      "net.request" | "net.connect" => {
        let host = get("host");
        if get("network") == "actor" {
          if get("path").contains("chat/completions") || get("path").contains("/v1/") {
            model_calls += 1;
          }
          *actor_net.entry(host).or_default() += 1;
        } else {
          *executor_net.entry(host).or_default() += 1;
        }
      }
      "net.deny" => net_denials.push(format!("{} ({} network)", get("host"), get("network"))),
      "actor.exit" => {
        actor_exit = match get("signal").as_str() {
          "" => format!("exit {}", get("code")),
          s => s.to_string(),
        }
      }
      _ => {}
    }
  }

  let mut out = String::new();
  writeln!(out, "# Session report: {id} (profile: {})\n", info["profile"].as_str().unwrap_or("-"))?;
  writeln!(out, "## Overview\n")?;
  writeln!(
    out,
    "| Duration | State | Driving program | Routed to executor | Kept on actor | CLI commands | Failed | Model calls (actor gateway) |"
  )?;
  writeln!(out, "| --- | --- | --- | --- | --- | --- | --- | --- |")?;
  writeln!(
    out,
    "| {}m {:02}s | {} | {} | {routed} | {kept} | {cli} | {failed} | {model_calls} |\n",
    secs / 60,
    secs % 60,
    info["state"].as_str().unwrap_or("-"),
    if actor_exit.is_empty() {
      "-".into()
    } else {
      actor_exit
    },
  )?;
  writeln!(out, "## Decisions\n")?;
  writeln!(out, "| ↪️ Routed to executor | 🏠 Kept on actor | 🚫 Denied | 💥 Fault injected |")?;
  writeln!(out, "| --- | --- | --- | --- |")?;
  writeln!(out, "| {routed} | {kept} | {denied} | {faults} |\n")?;
  writeln!(out, "## Findings\n")?;
  let mut any = false;
  if !probes.is_empty() {
    any = true;
    writeln!(out, "- ⚠️ Environment probing: {} identity commands ({})", probes.values().sum::<usize>(), top(&probes, 8))?;
  }
  for d in &denials {
    any = true;
    writeln!(out, "- 🚫 Denied command: {d}")?;
  }
  for d in &net_denials {
    any = true;
    writeln!(out, "- 🚫 Network denied: {d}")?;
  }
  if faults > 0 {
    any = true;
    writeln!(out, "- ℹ️ Injected faults: {faults}")?;
  }
  if !any {
    writeln!(out, "- No findings.")?;
  }
  writeln!(out, "\n## Top activity\n")?;
  writeln!(out, "- Executor commands: {}", top(&executor_cmds, 8))?;
  writeln!(out, "- Executor network: {}", top(&executor_net, 8))?;
  writeln!(out, "- Actor network: {}", top(&actor_net, 8))?;
  slowest.sort_by_key(|s| std::cmp::Reverse(s.0));
  if !slowest.is_empty() {
    writeln!(
      out,
      "- Slowest routed commands: {}",
      slowest.iter().take(3).map(|(ms, argv)| format!("`{}` ({ms} ms)", truncate(argv, 60))).collect::<Vec<_>>().join(", ")
    )?;
  }
  if detailed {
    writeln!(out, "\n## Event timeline\n")?;
    writeln!(out, "| +ms | Event | Details |")?;
    writeln!(out, "| --- | --- | --- |")?;
    for (time, kind, attrs) in &loaded.events {
      let details = attrs
        .iter()
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .map(|(k, v)| format!("{k}={}", truncate(v, 80)))
        .collect::<Vec<_>>()
        .join(" ");
      writeln!(out, "| {} | {kind} | {} |", time - created, details.replace('|', "\\|"))?;
    }
  }
  Ok(out)
}

fn truncate(s: &str, n: usize) -> String {
  if s.chars().count() <= n {
    return s.to_string();
  }
  let mut t: String = s.chars().take(n).collect();
  t.push('…');
  t
}
