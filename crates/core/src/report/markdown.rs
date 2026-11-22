//! The report as Markdown, rendered from a [`Summary`].

use std::fmt::Write;

use super::{Finding, FindingKind, SHIM_ROUTING, Summary, actor_stop, capitalize, duration, ms, truncate};
use crate::event::ActorExit;
use crate::types::{Capture, Network, RunState};

pub(super) fn render(s: &Summary) -> String {
  let mut out = String::new();
  // NOTICE: writing to a `String` cannot fail.
  write(&mut out, s).expect("formatting into a String");
  out
}

fn write(out: &mut String, s: &Summary) -> std::fmt::Result {
  let driver = match &s.actor_exit {
    None => "-".to_string(),
    Some(ActorExit {
      signal: Some(signal),
      ..
    }) => signal.clone(),
    Some(ActorExit { code, .. }) => format!("exit {}", code.map_or("-".to_string(), |c| c.to_string())),
  };

  writeln!(out, "# Run report: {} (profile: {})\n", s.run, s.profile.as_deref().unwrap_or("-"))?;
  if s.dropped_events > 0 {
    writeln!(
      out,
      "> ⚠️ The trace is incomplete: {} events were dropped when the trace queue was full, so the counts below may be low.\n",
      s.dropped_events
    )?;
  }
  writeln!(out, "## Overview\n")?;
  if let Some(snapshot) = &s.snapshot {
    writeln!(out, "Snapshot: `{snapshot}`.\n")?;
  }
  if s.started_by.is_some() || s.stopped_by.is_some() {
    write!(out, "Started by `{}`, stopped by `{}`", s.started_by.as_deref().unwrap_or("-"), s.stopped_by.as_deref().unwrap_or("-"))?;
    if let Some(how) = &s.actor_stop {
      write!(out, " ({})", actor_stop(how))?;
    }
    writeln!(out, ".\n")?;
  }

  let c = &s.commands;
  let routed: usize = c.routed.iter().map(|r| r.commands).sum();
  writeln!(
    out,
    "| Duration | State | Driving program | Routed to executor | Kept on actor | CLI commands | Failed | Model calls (actor gateway) |"
  )?;
  writeln!(out, "| --- | --- | --- | --- | --- | --- | --- | --- |")?;
  writeln!(
    out,
    "| {} | {} | {driver} | {routed} | {} | {} | {} | {} |\n",
    duration(s.duration_ms),
    s.state.map_or("-", RunState::as_str),
    c.kept_on_actor,
    c.cli,
    c.failed,
    c.model_calls,
  )?;

  writeln!(out, "## Decisions\n")?;
  writeln!(out, "| ↪️ Routed to executor | 🏠 Kept on actor | 🚫 Denied | 💥 Fault injected |")?;
  writeln!(out, "| --- | --- | --- | --- |")?;
  writeln!(out, "| {routed} | {} | {} | {} |\n", c.kept_on_actor, c.denied, c.faults)?;
  match &s.route_names {
    None => writeln!(out, "🏠 No actor ran in this run.\n")?,
    Some(names) if names.iter().any(|n| n == "*") => {
      writeln!(out, "🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy.\n")?
    }
    Some(names) if names.is_empty() => {
      writeln!(out, "🏠 The actor routes no command names to needy, so commands it ran are not counted as kept on actor.\n")?
    }
    Some(names) => {
      let names = names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
      writeln!(
        out,
        "🏠 Kept on actor counts only commands the actor's kernel routed to needy ({names}); other commands ran on the actor without a trace event.\n"
      )?;
    }
  }
  if let Some(driver) = &s.actor_driver {
    writeln!(out, "The actor ran in `{driver}`.\n")?;
  }
  if s.actor_capture == Some(Capture::Shims) {
    writeln!(out, "{SHIM_ROUTING}\n")?;
  }

  writeln!(out, "## Findings\n")?;
  if s.findings.is_empty() {
    writeln!(out, "- No findings.")?;
  } else {
    for finding in &s.findings {
      writeln!(out, "- {}", format_finding(finding))?;
    }
  }

  writeln!(out, "\n## Top activity\n")?;
  let executor_cmds = s.top_commands.iter().map(|t| format!("{} on {} ({})", t.command, t.executor, t.count)).collect::<Vec<_>>().join(", ");
  let net_hosts = |network: Network| {
    s.network.iter().filter(|h| h.network == network).map(|h| format!("{} ({})", h.host, h.count)).collect::<Vec<_>>().join(", ")
  };
  let slowest = s.slowest_commands.iter().map(|c| format!("`{}` ({} ms)", truncate(&c.command, 60), c.ms)).collect::<Vec<_>>().join(", ");
  let lines = [
    ("Executor commands", executor_cmds),
    ("Executor network", net_hosts(Network::Executor)),
    ("Actor network", net_hosts(Network::Actor)),
    ("Slowest commands", slowest),
  ];
  let mut any = false;
  for (label, value) in lines.iter().filter(|(_, value)| !value.is_empty()) {
    any = true;
    writeln!(out, "- {label}: {value}")?;
  }
  if !any {
    writeln!(out, "- No activity.")?;
  }

  if !s.synced.is_empty() {
    writeln!(out, "\n## Synced files\n")?;
    writeln!(out, "| Mapping | Merge | Taken | Resolved | Conflicts | Store |")?;
    writeln!(out, "| --- | --- | --- | --- | --- | --- |")?;
    for synced in &s.synced {
      writeln!(
        out,
        "| `{}` | {} | {} | {} | {} | `{}` (control host, account `{}`) |",
        synced.name,
        synced.merge.as_str(),
        synced.taken,
        synced.resolved,
        synced.conflicts.len(),
        synced.store,
        synced.account
      )?;
    }
  }

  if !s.latency.is_empty() {
    writeln!(out, "\n## Latency\n")?;
    writeln!(out, "| Hook | Count | Mean | Min | Max |")?;
    writeln!(out, "| --- | --- | --- | --- | --- |")?;
    for h in &s.latency {
      writeln!(out, "| `{}` | {} | {} | {} | {} |", h.hook, h.count, ms(h.mean_us), ms(h.min_us), ms(h.max_us))?;
    }
  }

  if let Some(events) = &s.events {
    writeln!(out, "\n## Event timeline\n")?;
    writeln!(out, "| +ms | Event | Details |")?;
    writeln!(out, "| --- | --- | --- |")?;
    for e in events {
      let details = truncate(&e.raw_payload, 160).replace('|', "\\|");
      writeln!(out, "| {} | {} | `{details}` |", e.offset_ms, e.kind)?;
    }
  }
  Ok(())
}

fn format_finding(finding: &Finding) -> String {
  let msg = &finding.message;
  match finding.kind {
    FindingKind::RunFailed => format!("❌ {}", capitalize(msg)),
    FindingKind::FilesFailed => {
      if let Some((head, reason)) = msg.split_once(" failed: ")
        && let Some((transfer, name)) = head.split_once(' ')
      {
        format!("❌ {} `{name}` failed: {reason}", capitalize(transfer))
      } else {
        format!("❌ {}", capitalize(msg))
      }
    }
    FindingKind::Probing | FindingKind::SyncConflict | FindingKind::MergePending => format!("⚠️ {}", capitalize(msg)),
    FindingKind::DeniedCommand => {
      if let Some(rest) = msg.strip_prefix("denied command: ")
        && let Some((argv, reason)) = rest.split_once(": ")
      {
        format!("🚫 Denied command: `{argv}`: {reason}")
      } else {
        format!("🚫 {}", capitalize(msg))
      }
    }
    FindingKind::DeniedNetwork => format!("🚫 {}", capitalize(msg)),
    FindingKind::Faults | FindingKind::ChaosChanged => format!("ℹ️ {}", capitalize(msg)),
  }
}
