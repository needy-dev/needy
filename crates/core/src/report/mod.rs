//! `needy report`: a fixed-format markdown summary of one session, rendered
//! from its typed events. No IO; the caller reads the events.

use std::collections::BTreeMap;
use std::fmt::Write;

use needy_config::profile::FaultKind;

use crate::event::{ActorExit, Event, Recorded};
use crate::pattern::basename;
use crate::types::{Network, Origin, Outcome, Placement, SessionState};

/// How much the report shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
  /// About one page.
  Summary,
  /// Adds the event timeline.
  Detailed,
}

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

/// How many entries a top-activity line lists.
const TOP: usize = 8;

/// Execs that run a user's or the actor's command, as opposed to needy's own
/// file transfers and mounts.
fn is_command(origin: Origin) -> bool {
  matches!(origin, Origin::Cli | Origin::ActorProxy)
}

fn command_name(argv: &[String]) -> &str {
  argv.first().map(|word| basename(word)).unwrap_or_default()
}

fn top(counts: &BTreeMap<String, usize>, n: usize) -> String {
  let mut v: Vec<_> = counts.iter().collect();
  v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
  v.iter().take(n).map(|(k, c)| format!("{k} ({c})")).collect::<Vec<_>>().join(", ")
}

fn truncate(s: &str, n: usize) -> String {
  if s.chars().count() <= n {
    return s.to_string();
  }
  let mut t: String = s.chars().take(n).collect();
  t.push('…');
  t
}

/// What the report counts, gathered in one pass over the events.
#[derive(Default)]
struct Tally {
  profile: Option<String>,
  created: Option<i64>,
  /// The first end: an actor exit or a stop.
  ended: Option<i64>,
  state: Option<SessionState>,
  actor_exit: Option<ActorExit>,
  /// Why the session failed to start.
  failure: Option<String>,
  /// Events the trace dropped.
  dropped: u64,
  /// The actor's route names; `Some(None)` routes every exec.
  route_names: Option<Option<Vec<String>>>,
  routed: usize,
  kept: usize,
  denied: usize,
  faults: usize,
  cli: usize,
  failed: usize,
  model_calls: usize,
  probes: BTreeMap<String, usize>,
  fault_kinds: BTreeMap<String, usize>,
  executor_cmds: BTreeMap<String, usize>,
  actor_net: BTreeMap<String, usize>,
  executor_net: BTreeMap<String, usize>,
  denials: Vec<String>,
  net_denials: Vec<String>,
  slowest: Vec<(u64, String)>,
}

impl Tally {
  fn add(&mut self, time_ms: i64, event: &Event) {
    match event {
      Event::SessionCreate(create) => {
        self.profile = Some(create.profile.clone());
        self.created = Some(time_ms);
        self.state = Some(SessionState::Running);
      }
      Event::SessionStop(_) => {
        self.ended.get_or_insert(time_ms);
        self.state = Some(SessionState::Stopped);
      }
      Event::SessionFail(fail) => {
        self.ended.get_or_insert(time_ms);
        self.state = Some(SessionState::Failed);
        self.failure = Some(fail.reason.clone());
      }
      Event::TraceGap(gap) => self.dropped += gap.dropped,
      Event::ActorExit(exit) => {
        self.ended.get_or_insert(time_ms);
        self.state = Some(SessionState::Finished);
        self.actor_exit = Some(exit.clone());
      }
      Event::ActorConfig(config) => self.route_names = Some(config.route_names.clone()),
      Event::ExecDecide(decide) => {
        match &decide.outcome {
          // NOTICE: a command that needy did not route never reaches the
          // trace, so only commands the actor asked about count as kept.
          Outcome::Run(Placement::Actor) if decide.origin == Origin::ActorProxy => self.kept += 1,
          Outcome::Run(Placement::Actor) => {}
          Outcome::Run(Placement::Executor(_)) => self.routed += 1,
          Outcome::Deny(message) => {
            self.denied += 1;
            self.denials.push(format!("`{}`: {message}", decide.argv.join(" ")));
          }
          // Counted from its `fault.injected` event.
          Outcome::Fault(_) => {}
        }
        let line = decide.argv.join(" ");
        let name = command_name(&decide.argv);
        if IDENTITY.iter().any(|p| *p == name || line.contains(p)) {
          *self.probes.entry(name.to_string()).or_default() += 1;
        }
      }
      Event::ExecStart(start) => {
        if start.origin == Origin::Cli {
          self.cli += 1;
        }
        if is_command(start.origin) {
          *self.executor_cmds.entry(format!("{} on {}", command_name(&start.argv), start.executor)).or_default() += 1;
        }
      }
      Event::ExecExit(exit) => {
        if exit.code.is_some_and(|code| code != 0) || exit.signal.is_some() {
          self.failed += 1;
        }
        if is_command(exit.origin) {
          self.slowest.push((exit.ms, exit.argv.join(" ")));
        }
      }
      Event::FaultInjected(fired) => {
        self.faults += 1;
        *self.fault_kinds.entry(fired.kind.to_string()).or_default() += 1;
        // An exec fault with an errno fails the command before it starts,
        // so no `exec.exit` counts it.
        if fired.kind == FaultKind::Exec && fired.effect.errno.is_some() {
          self.failed += 1;
        }
      }
      Event::NetRequest(request) => {
        if request.network == Network::Actor && (request.path.contains("chat/completions") || request.path.contains("/v1/")) {
          self.model_calls += 1;
        }
        self.count_host(request.network, &request.host);
      }
      Event::NetConnect(connect) => self.count_host(connect.network, &connect.host),
      Event::NetDeny(deny) => self.net_denials.push(format!("{} ({} network)", deny.host, network_name(deny.network))),
      Event::ActorStart(_)
      | Event::ActorVmExit(_)
      | Event::FilesCopyIn(_)
      | Event::FilesMirrorOut(_)
      | Event::NetError(_)
      | Event::TraceTiming(_) => {}
    }
  }

  fn count_host(&mut self, network: Network, host: &str) {
    let counts = match network {
      Network::Actor => &mut self.actor_net,
      Network::Executor => &mut self.executor_net,
    };
    *counts.entry(host.to_string()).or_default() += 1;
  }
}

fn network_name(network: Network) -> &'static str {
  match network {
    Network::Actor => "actor",
    Network::Executor => "executor",
  }
}

/// Renders the report of session `id` from its events, in recorded order.
pub fn render(id: &str, events: &[Recorded], level: Level) -> String {
  let mut tally = Tally::default();
  for recorded in events {
    tally.add(recorded.time_ms, &recorded.event);
  }
  let mut out = String::new();
  // NOTICE: writing to a `String` cannot fail.
  write_report(&mut out, id, events, &tally, level).expect("formatting into a String");
  out
}

fn write_report(out: &mut String, id: &str, events: &[Recorded], t: &Tally, level: Level) -> std::fmt::Result {
  let first = events.first().map_or(0, |r| r.time_ms);
  let created = t.created.unwrap_or(first);
  let ended = t.ended.or(events.last().map(|r| r.time_ms)).unwrap_or(created);
  let secs = (ended - created).max(0) / 1000;
  let driver = match &t.actor_exit {
    None => "-".to_string(),
    Some(ActorExit {
      signal: Some(signal),
      ..
    }) => signal.clone(),
    Some(ActorExit { code, .. }) => format!("exit {}", code.map_or("-".to_string(), |c| c.to_string())),
  };

  writeln!(out, "# Session report: {id} (profile: {})\n", t.profile.as_deref().unwrap_or("-"))?;
  if t.dropped > 0 {
    writeln!(
      out,
      "> ⚠️ The trace is incomplete: {} events were dropped when the trace queue was full, so the counts below may be low.\n",
      t.dropped
    )?;
  }
  writeln!(out, "## Overview\n")?;
  writeln!(
    out,
    "| Duration | State | Driving program | Routed to executor | Kept on actor | CLI commands | Failed | Model calls (actor gateway) |"
  )?;
  writeln!(out, "| --- | --- | --- | --- | --- | --- | --- | --- |")?;
  writeln!(
    out,
    "| {}m {:02}s | {} | {driver} | {} | {} | {} | {} | {} |\n",
    secs / 60,
    secs % 60,
    t.state.map_or("-", SessionState::as_str),
    t.routed,
    t.kept,
    t.cli,
    t.failed,
    t.model_calls,
  )?;
  writeln!(out, "## Decisions\n")?;
  writeln!(out, "| ↪️ Routed to executor | 🏠 Kept on actor | 🚫 Denied | 💥 Fault injected |")?;
  writeln!(out, "| --- | --- | --- | --- |")?;
  writeln!(out, "| {} | {} | {} | {} |\n", t.routed, t.kept, t.denied, t.faults)?;
  match &t.route_names {
    None => writeln!(out, "🏠 No actor ran in this session.\n")?,
    Some(None) => writeln!(out, "🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy.\n")?,
    Some(Some(names)) => {
      let names = names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
      writeln!(
        out,
        "🏠 Kept on actor counts only commands the actor's kernel routed to needy ({names}); other commands ran on the actor without a trace event.\n"
      )?;
    }
  }

  writeln!(out, "## Findings\n")?;
  let mut any = false;
  if let Some(reason) = &t.failure {
    any = true;
    writeln!(out, "- ❌ Session failed: {reason}")?;
  }
  if !t.probes.is_empty() {
    any = true;
    writeln!(out, "- ⚠️ Environment probing: {} identity commands ({})", t.probes.values().sum::<usize>(), top(&t.probes, TOP))?;
  }
  for d in &t.denials {
    any = true;
    writeln!(out, "- 🚫 Denied command: {d}")?;
  }
  for d in &t.net_denials {
    any = true;
    writeln!(out, "- 🚫 Network denied: {d}")?;
  }
  if t.faults > 0 {
    any = true;
    writeln!(out, "- ℹ️ Injected faults: {} ({})", t.faults, top(&t.fault_kinds, TOP))?;
  }
  if !any {
    writeln!(out, "- No findings.")?;
  }

  writeln!(out, "\n## Top activity\n")?;
  let mut slowest = t.slowest.clone();
  slowest.sort_by_key(|s| std::cmp::Reverse(s.0));
  let slowest = slowest.iter().take(3).map(|(ms, argv)| format!("`{}` ({ms} ms)", truncate(argv, 60))).collect::<Vec<_>>().join(", ");
  let lines = [
    ("Executor commands", top(&t.executor_cmds, TOP)),
    ("Executor network", top(&t.executor_net, TOP)),
    ("Actor network", top(&t.actor_net, TOP)),
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

  if level == Level::Detailed {
    writeln!(out, "\n## Event timeline\n")?;
    writeln!(out, "| +ms | Event | Details |")?;
    writeln!(out, "| --- | --- | --- |")?;
    for recorded in events {
      let details = truncate(&recorded.event.payload(), 160).replace('|', "\\|");
      writeln!(out, "| {} | {} | `{details}` |", recorded.time_ms - created, recorded.event.kind())?;
    }
  }
  Ok(())
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
