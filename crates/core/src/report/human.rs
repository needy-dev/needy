//! The report for people: plain text in sections of aligned `key  value`
//! lines, like `kubectl describe`. Sections with nothing to say are left
//! out. With `color`, failures are red and faults yellow.

use std::fmt::Write;

use anstyle::{AnsiColor, Style};

use super::{FindingKind, Summary, duration, network_name, truncate};
use crate::event::ActorExit;
use crate::types::RunState;

const FAILURE: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
const FAULT: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));

/// Paints `text` in `style` when colors are on.
fn paint(text: &str, style: Style, color: bool) -> String {
  if color {
    format!("{style}{text}{style:#}")
  } else {
    text.to_string()
  }
}

/// Lines of `label  value`, the labels padded to one width.
fn aligned(out: &mut String, indent: &str, rows: &[(String, String)]) {
  let width = rows.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
  for (label, value) in rows {
    let line = format!("{indent}{label:<width$}  {value}");
    out.push_str(line.trim_end());
    out.push('\n');
  }
}

/// The driving program's end, as it follows the state.
fn driver(exit: &ActorExit) -> String {
  match (&exit.signal, exit.code) {
    (Some(signal), _) => format!("driving program killed by {signal}"),
    (None, Some(code)) => format!("driving program exit {code}"),
    (None, None) => "driving program ended".into(),
  }
}

pub(super) fn render(s: &Summary, color: bool) -> String {
  let mut out = String::new();
  // NOTICE: writing to a `String` cannot fail.
  write(&mut out, s, color).expect("formatting into a String");
  out
}

fn write(out: &mut String, s: &Summary, color: bool) -> std::fmt::Result {
  let run = match &s.profile {
    Some(profile) => format!("{}  (profile {profile})", s.run),
    None => s.run.clone(),
  };
  let mut state = match s.state {
    Some(RunState::Failed) => paint("failed", FAILURE, color),
    Some(state) => state.to_string(),
    None => "unknown".into(),
  };
  if let Some(exit) = &s.actor_exit {
    let text = driver(exit);
    let failed = exit.signal.is_some() || exit.code.is_some_and(|code| code != 0);
    state = format!(
      "{state}, {}",
      if failed {
        paint(&text, FAILURE, color)
      } else {
        text
      }
    );
  }
  let mut head = vec![
    ("Run".to_string(), run),
    ("State".into(), state),
    ("Duration".into(), duration(s.duration_ms)),
  ];
  if let Some(runtime) = &s.actor_runtime {
    head.push(("Actor".into(), runtime.clone()));
  }
  if let Some(by) = &s.started_by {
    head.push(("Started by".into(), by.clone()));
  }
  if let Some(by) = &s.stopped_by {
    head.push(("Stopped by".into(), by.clone()));
  }
  if s.dropped_events > 0 {
    head.push(("Warning".into(), format!("the trace is incomplete: {} events were dropped, so the counts may be low", s.dropped_events)));
  }
  aligned(out, "", &head);

  let c = &s.commands;
  let actor = s.route_names.is_some();
  if actor || !c.routed.is_empty() || c.cli + c.denied + c.faults + c.model_calls > 0 {
    let mut rows: Vec<(String, usize, String)> = Vec::new();
    for routed in &c.routed {
      let failed = if routed.failed > 0 {
        paint(&format!("({} failed)", routed.failed), FAILURE, color)
      } else {
        String::new()
      };
      rows.push((format!("routed to {}", routed.executor), routed.commands, failed));
    }
    if c.routed.is_empty() {
      rows.push(("routed to executors".into(), 0, String::new()));
    }
    if actor {
      rows.push(("kept on actor".into(), c.kept_on_actor, String::new()));
    }
    if c.cli > 0 {
      rows.push(("from needy exec".into(), c.cli, String::new()));
    }
    rows.push(("denied".into(), c.denied, String::new()));
    rows.push(("faults".into(), c.faults, String::new()));
    if c.model_calls > 0 {
      rows.push(("model calls".into(), c.model_calls, String::new()));
    }
    let width = rows.iter().map(|(_, count, _)| count.to_string().len()).max().unwrap_or(1);
    let rows: Vec<(String, String)> = rows.into_iter().map(|(label, count, note)| (label, format!("{count:<width$}  {note}"))).collect();
    writeln!(out, "\nCommands")?;
    aligned(out, "  ", &rows);
    // Commands the actor's kernel does not route never reach needy, so
    // "kept on actor" undercounts them; say so only then.
    match s.route_names.as_ref().filter(|names| !names.iter().any(|n| n == "*")) {
      Some(names) if names.is_empty() => {
        writeln!(out, "  (the actor routes no command names, so kept on actor counts none of its commands)")?
      }
      Some(names) => writeln!(out, "  (kept on actor counts only the names the actor routes: {})", names.join(", "))?,
      None => {}
    }
    if s.actor_runtime.as_deref().is_some_and(|runtime| runtime.starts_with("container")) {
      writeln!(out, "  ({})", super::CONTAINER_ROUTING)?;
    }
  }

  if !s.top_commands.is_empty() {
    let width = s.top_commands.iter().map(|t| t.count.to_string().len()).max().unwrap_or(1);
    let rows: Vec<(String, String)> = s
      .top_commands
      .iter()
      .map(|t| {
        let slowest = t.slowest_ms.map(|ms| format!("slowest {ms} ms")).unwrap_or_default();
        (format!("{} on {}", t.command, t.executor), format!("{:<width$}  {slowest}", t.count))
      })
      .collect();
    writeln!(out, "\nTop commands")?;
    aligned(out, "  ", &rows);
  }

  if !s.network.is_empty() {
    let host_width = s.network.iter().map(|h| h.host.chars().count()).max().unwrap_or(0);
    let rows: Vec<(String, String)> =
      s.network.iter().map(|h| (network_name(h.network).to_string(), format!("{:<host_width$}  {}", h.host, h.count))).collect();
    writeln!(out, "\nNetwork")?;
    aligned(out, "  ", &rows);
  }

  if s.findings.is_empty() {
    writeln!(out, "\nNo findings.")?;
  } else {
    writeln!(out, "\nFindings")?;
    for finding in &s.findings {
      let message = match finding.kind {
        FindingKind::RunFailed | FindingKind::FilesFailed => paint(&finding.message, FAILURE, color),
        FindingKind::Faults => paint(&finding.message, FAULT, color),
        FindingKind::Probing | FindingKind::DeniedCommand | FindingKind::DeniedNetwork => finding.message.clone(),
      };
      writeln!(out, "  {message}")?;
    }
  }

  if let Some(events) = &s.events {
    writeln!(out, "\nEvents")?;
    let width = events.iter().map(|e| e.offset_ms.to_string().len() + 1).max().unwrap_or(0);
    let kind_width = events.iter().map(|e| e.kind.len()).max().unwrap_or(0);
    for e in events {
      let offset = format!("+{}", e.offset_ms);
      writeln!(out, "  {offset:>width$}ms  {:<kind_width$}  {}", e.kind, truncate(&e.payload.to_string(), 160))?;
    }
  }
  Ok(())
}
