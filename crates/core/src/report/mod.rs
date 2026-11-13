//! `needy report`: a fixed-format summary of one run, rendered from its
//! typed events in one of three formats: plain text for people (`human`),
//! markdown, and a JSON [`Summary`] for tools. No IO; the caller reads the
//! events and decides whether the text may carry colors.

mod human;

use std::collections::BTreeMap;
use std::fmt::Write;

use needy_config::profile::FaultDomain;
use serde::Serialize;

use crate::event::{ActorExit, ActorStop, Event, HookTiming, Recorded, Transfer};
use crate::pattern::basename;
use crate::types::{Capture, Network, Origin, Outcome, Placement, Principal, RunState};

/// How much the report shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
  /// About one page.
  Summary,
  /// Adds the event timeline.
  Detailed,
}

/// How the report is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
  /// Plain text in aligned sections, like `kubectl describe`; `color` marks
  /// failures and faults with terminal colors.
  Human {
    color: bool,
  },
  Markdown,
  /// A [`Summary`] as JSON.
  Json,
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

/// A run's length in units that fit it: tenths of a second under a
/// minute, then minutes and seconds, then hours and minutes. Each unit is
/// cut, not rounded, so a length never reads longer than it was.
fn duration(ms: i64) -> String {
  let ms = ms.max(0);
  let secs = ms / 1000;
  match secs {
    0..60 => format!("{}.{}s", secs, (ms % 1000) / 100),
    60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
    _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
  }
}

/// How a stop ended the running actor, in a few words.
fn actor_stop(how: &ActorStop) -> String {
  match how {
    ActorStop::Graceful => "graceful: the actor mirrored out and ended".into(),
    ActorStop::Forced { reason } => format!("forced: {reason}"),
  }
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
  /// Per-hook latency from `trace.timing`, merged across its payloads.
  latency: BTreeMap<String, HookTiming>,
  profile: Option<String>,
  created: Option<i64>,
  /// The first end: an actor exit, a stop or a failure.
  ended: Option<i64>,
  /// `Running` once created, then the first terminal state, which stays as
  /// it does in the daemon's run state machine, so the report agrees
  /// with `run ls`.
  // NOTICE: an actor exit that lands while a stop is under way, as a
  // graceful stop's does, is recorded before `run.stop`; a `run.stop` with
  // `actor` says the stop ended the running actor, so it makes the run
  // `stopped` as the daemon says. Without `actor` (an actor that ended
  // first) the first end stays.
  state: Option<RunState>,
  actor_exit: Option<ActorExit>,
  /// Why the run failed.
  failure: Option<String>,
  /// Who created the run, and who stopped it.
  started_by: Option<Principal>,
  stopped_by: Option<Principal>,
  /// How the stop ended the running actor.
  actor_stop: Option<ActorStop>,
  /// Events the trace dropped.
  dropped: u64,
  /// The actor's route names; `Some(None)` routes every exec.
  route_names: Option<Option<Vec<String>>>,
  /// What ran the actor, and how it caught an exec.
  actor_driver: Option<String>,
  actor_capture: Option<Capture>,
  routed: usize,
  kept: usize,
  denied: usize,
  faults: usize,
  cli: usize,
  failed: usize,
  model_calls: usize,
  probes: BTreeMap<String, usize>,
  /// Injected faults by `NAME (DOMAIN)`.
  fault_names: BTreeMap<String, usize>,
  executor_cmds: BTreeMap<String, usize>,
  actor_net: BTreeMap<String, usize>,
  executor_net: BTreeMap<String, usize>,
  /// Commands placed on each executor.
  routed_by: BTreeMap<String, usize>,
  /// Commands that failed on each executor.
  failed_by: BTreeMap<String, usize>,
  /// Per command name and executor: how many ran, and the slowest exit.
  commands_by: BTreeMap<(String, String), (usize, Option<u64>)>,
  /// Denied commands and the rules' messages.
  denials: Vec<(String, String)>,
  /// Denied hosts and their networks.
  net_denials: Vec<(String, Network)>,
  /// Copy-ins and mirror-outs that failed: the mapping's transfer, name
  /// and why.
  files_failed: Vec<(Transfer, String, String)>,
  /// Faults added or removed while the run ran, in order: `+NAME` or
  /// `-NAME`.
  chaos_changes: Vec<String>,
  slowest: Vec<(u64, String)>,
}

impl Tally {
  fn add(&mut self, time_ms: i64, event: &Event) {
    match event {
      Event::RunCreate(create) => {
        self.profile = Some(create.profile.clone());
        self.created = Some(time_ms);
        self.started_by = create.by.clone();
        self.state.get_or_insert(RunState::Running);
      }
      Event::RunStop(stop) => {
        if stop.actor.is_some() {
          // The stop ended the running actor: its exit, recorded first, was
          // the stop's doing.
          self.ended = Some(time_ms);
          self.state = Some(RunState::Stopped);
        } else {
          self.end(time_ms, RunState::Stopped);
        }
        self.stopped_by = stop.by.clone();
        self.actor_stop = stop.actor.clone();
      }
      Event::RunFail(fail) => {
        self.end(time_ms, RunState::Failed);
        self.failure = Some(fail.reason.clone());
      }
      Event::TraceGap(gap) => self.dropped += gap.dropped,
      Event::ActorExit(exit) => {
        self.end(time_ms, RunState::Finished);
        self.actor_exit = Some(exit.clone());
      }
      // A VM exit after the driving program's `actor.exit` ended nothing
      // new; one before it is the failure the daemon records next as
      // `run.fail`, with the reason.
      Event::ActorVmExit(_) => self.end(time_ms, RunState::Failed),
      Event::ActorConfig(config) => self.route_names = Some(config.route_names.clone()),
      Event::ExecDecide(decide) => {
        match &decide.outcome {
          // NOTICE: a command that needy did not route never reaches the
          // trace, so only commands the actor asked about count as kept.
          Outcome::Run(Placement::Actor) if decide.origin == Origin::ActorProxy => self.kept += 1,
          Outcome::Run(Placement::Actor) => {}
          Outcome::Run(Placement::Executor(executor)) => {
            self.routed += 1;
            *self.routed_by.entry(executor.clone()).or_default() += 1;
          }
          Outcome::Deny(message) => {
            self.denied += 1;
            self.denials.push((decide.argv.join(" "), message.clone()));
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
          self.commands_by.entry((command_name(&start.argv).to_string(), start.executor.clone())).or_default().0 += 1;
        }
      }
      Event::ExecExit(exit) => {
        let failed = exit.code.is_some_and(|code| code != 0) || exit.signal.is_some();
        if failed {
          self.failed += 1;
        }
        if is_command(exit.origin) {
          self.slowest.push((exit.ms, exit.argv.join(" ")));
          if failed {
            *self.failed_by.entry(exit.executor.clone()).or_default() += 1;
          }
          let slowest = &mut self.commands_by.entry((command_name(&exit.argv).to_string(), exit.executor.clone())).or_default().1;
          *slowest = Some(slowest.unwrap_or(0).max(exit.ms));
        }
      }
      Event::FaultInjected(fired) => {
        self.faults += 1;
        *self.fault_names.entry(format!("{} ({})", fired.name, fired.domain)).or_default() += 1;
        // An exec fault that exits non-zero or by a signal fails the
        // command without running it, so no `exec.exit` counts it.
        if fired.domain == FaultDomain::Exec && (fired.effect.exit.is_some_and(|code| code != 0) || fired.effect.signal.is_some()) {
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
      Event::NetDeny(deny) => self.net_denials.push((deny.host.clone(), deny.network)),
      Event::FilesFail(fail) => self.files_failed.push((fail.transfer, fail.name.clone(), fail.reason.clone())),
      Event::ChaosAdd(add) => self.chaos_changes.push(format!("+{} ({})", add.name, add.fault.domain())),
      Event::ChaosRemove(remove) => self.chaos_changes.push(format!("-{}", remove.name)),
      Event::ActorStart(start) => {
        self.actor_driver = start.driver.clone();
        self.actor_capture = start.capture;
      }
      Event::ActorReady(_) | Event::ExecKill(_) | Event::FilesCopyIn(_) | Event::FilesMirrorOut(_) | Event::NetError(_) => {}
      Event::TraceTiming(timing) => {
        for (hook, sample) in &timing.hooks {
          let merged = self.latency.entry(hook.clone()).or_insert(HookTiming {
            count: 0,
            total_us: 0,
            min_us: u64::MAX,
            max_us: 0,
          });
          merged.count += sample.count;
          merged.total_us += sample.total_us;
          merged.min_us = merged.min_us.min(sample.min_us);
          merged.max_us = merged.max_us.max(sample.max_us);
        }
      }
    }
  }

  /// Records an end at `time_ms`; only the first sets the state.
  fn end(&mut self, time_ms: i64, state: RunState) {
    if self.ended.is_none() {
      self.ended = Some(time_ms);
      self.state = Some(state);
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

/// Renders the report of run `id` from its events, in recorded order.
pub fn render(id: &str, events: &[Recorded], level: Level, format: Format) -> String {
  let mut tally = Tally::default();
  for recorded in events {
    tally.add(recorded.time_ms, &recorded.event);
  }
  match format {
    Format::Human { color } => human::render(&summarize(id, events, &tally, level), color),
    Format::Markdown => {
      let mut out = String::new();
      // NOTICE: writing to a `String` cannot fail.
      write_report(&mut out, id, events, &tally, level).expect("formatting into a String");
      out
    }
    // NOTICE: a summary is strings, numbers, lists and maps with string
    // keys, which always serialize to JSON.
    Format::Json => serde_json::to_string_pretty(&summarize(id, events, &tally, level)).expect("a summary serializes to JSON") + "\n",
  }
}

/// What shim routing ([`Capture::Shims`], a container actor's) leaves out:
/// its shims are made when its image is built.
pub(crate) const SHIM_ROUTING: &str = "A container actor routes only the programs its image had when it was built; \
  programs created at run time and run directly stayed in the actor without a trace event.";

/// What the human and JSON reports show: one run's outcome and counts.
/// Its JSON form is `needy report -o json`; the CLI note lists its fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
  pub run: String,
  pub profile: Option<String>,
  /// `None` for a trace without a `run.create`.
  pub state: Option<RunState>,
  /// From the create to the first end, or to the last event.
  pub duration_ms: i64,
  /// How the driving program ended, when it reported its exit.
  pub actor_exit: Option<ActorExit>,
  /// Why the run failed.
  pub failure: Option<String>,
  /// Who created the run (`operator`, `client:NAME`, …); `None` when the
  /// trace does not say, such as one written before principals were
  /// recorded.
  pub started_by: Option<String>,
  /// Who stopped the run; `None` when it was not stopped, or the control
  /// plane stopped it on its own.
  pub stopped_by: Option<String>,
  /// How the stop ended the running actor: `graceful` (mirrored out) or
  /// `forced` with the reason; `None` when no stop ended one.
  pub actor_stop: Option<ActorStop>,
  /// Events the trace dropped; the counts are low by about as many.
  pub dropped_events: u64,
  /// The command names the actor routes, `["*"]` for every exec; `None`
  /// when no actor ran.
  pub route_names: Option<Vec<String>>,
  /// What ran the actor (`linux-firecracker`, `linux-container (runc)`,
  /// `macos-apple-container`, …); `None` when no actor ran or the trace
  /// predates it.
  pub actor_driver: Option<String>,
  /// How the actor caught an exec (`kernel`, `shims`), from its driver's
  /// facets; `None` when no actor ran. Under `shims` the report says what
  /// shim routing leaves out.
  pub actor_capture: Option<Capture>,
  pub commands: Commands,
  /// Commands by name and executor, most frequent first.
  pub top_commands: Vec<TopCommand>,
  /// The slowest commands, slowest first.
  pub slowest_commands: Vec<SlowCommand>,
  /// Hosts reached through the gateways, most frequent first.
  pub network: Vec<HostCount>,
  /// Latency of each hook point, by name (`exec.decide`, `net.request`,
  /// ...), from `trace.timing`; empty unless the profile set
  /// `trace.timing`.
  pub latency: Vec<HookLatency>,
  pub findings: Vec<Finding>,
  /// The event timeline, at the detailed level only.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub events: Option<Vec<TimelineEvent>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Commands {
  /// Commands placed on each executor, by name.
  pub routed: Vec<Routed>,
  /// Commands the actor asked about and kept.
  pub kept_on_actor: usize,
  /// Commands `needy exec` ran.
  pub cli: usize,
  pub denied: usize,
  /// Faults chaos injected.
  pub faults: usize,
  /// Commands that exited non-zero or by a signal, or that an injected
  /// exit or signal ended without running.
  pub failed: usize,
  /// Requests to a model API through the actor gateway.
  pub model_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Routed {
  pub executor: String,
  pub commands: usize,
  pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopCommand {
  /// The program's name, without its directory.
  pub command: String,
  pub executor: String,
  pub count: usize,
  /// The slowest run's milliseconds, once one exited.
  pub slowest_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlowCommand {
  pub command: String,
  pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostCount {
  pub network: Network,
  pub host: String,
  pub count: usize,
}

/// One hook point's latency over a run, in microseconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookLatency {
  pub hook: String,
  pub count: u64,
  pub mean_us: u64,
  pub min_us: u64,
  pub max_us: u64,
}

/// The hooks' latency, by hook name.
fn latency(t: &Tally) -> Vec<HookLatency> {
  t.latency
    .iter()
    .filter(|(_, timing)| timing.count > 0)
    .map(|(hook, timing)| HookLatency {
      hook: hook.clone(),
      count: timing.count,
      mean_us: timing.total_us / timing.count,
      min_us: timing.min_us,
      max_us: timing.max_us,
    })
    .collect()
}

/// A latency: microseconds under a millisecond, else milliseconds with one
/// decimal.
fn ms(us: u64) -> String {
  if us < 1000 {
    format!("{us} µs")
  } else {
    format!("{}.{} ms", us / 1000, us % 1000 / 100)
  }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
  pub kind: FindingKind,
  pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
  RunFailed,
  /// A copy-in or mirror-out failed.
  FilesFailed,
  /// Commands that only learn about the machine.
  Probing,
  DeniedCommand,
  DeniedNetwork,
  /// Faults chaos injected.
  Faults,
  /// Faults `needy chaos` added or removed while the run ran.
  ChaosChanged,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEvent {
  /// Milliseconds since the run was created.
  pub offset_ms: i64,
  pub kind: String,
  pub payload: serde_json::Value,
}

/// The first end, or the last event, minus the create (or the first event).
fn span(events: &[Recorded], t: &Tally) -> (i64, i64) {
  let first = events.first().map_or(0, |r| r.time_ms);
  let created = t.created.unwrap_or(first);
  let ended = t.ended.or(events.last().map(|r| r.time_ms)).unwrap_or(created);
  (created, ended)
}

fn summarize(id: &str, events: &[Recorded], t: &Tally, level: Level) -> Summary {
  let (created, ended) = span(events, t);
  let mut findings = Vec::new();
  let mut find = |kind, message: String| findings.push(Finding { kind, message });
  if let Some(reason) = &t.failure {
    find(FindingKind::RunFailed, format!("run failed: {reason}"));
  }
  for (transfer, name, reason) in &t.files_failed {
    find(FindingKind::FilesFailed, format!("{} {name} failed: {reason}", transfer.as_str()));
  }
  if !t.probes.is_empty() {
    let count = t.probes.values().sum::<usize>();
    let noun = if count == 1 { "command" } else { "commands" };
    find(FindingKind::Probing, format!("environment probing: {count} identity {noun} ({})", top(&t.probes, TOP)));
  }
  for (argv, message) in &t.denials {
    find(FindingKind::DeniedCommand, format!("denied command: {argv}: {message}"));
  }
  for (host, network) in &t.net_denials {
    find(FindingKind::DeniedNetwork, format!("network denied: {host} ({} network)", network_name(*network)));
  }
  if t.faults > 0 {
    find(FindingKind::Faults, format!("injected faults: {} ({})", t.faults, top(&t.fault_names, TOP)));
  }
  if !t.chaos_changes.is_empty() {
    find(FindingKind::ChaosChanged, format!("chaos changed while running: {}", t.chaos_changes.join(", ")));
  }
  let mut top_commands: Vec<TopCommand> = t
    .commands_by
    .iter()
    .map(|((command, executor), (count, slowest_ms))| TopCommand {
      command: command.clone(),
      executor: executor.clone(),
      count: *count,
      slowest_ms: *slowest_ms,
    })
    .collect();
  top_commands.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| (&a.command, &a.executor).cmp(&(&b.command, &b.executor))));
  top_commands.truncate(TOP);
  let mut slowest = t.slowest.clone();
  slowest.sort_by_key(|s| std::cmp::Reverse(s.0));
  let mut network: Vec<HostCount> = [
    (Network::Actor, &t.actor_net),
    (Network::Executor, &t.executor_net),
  ]
  .into_iter()
  .flat_map(|(network, hosts)| {
    hosts.iter().map(move |(host, count)| HostCount {
      network,
      host: host.clone(),
      count: *count,
    })
  })
  .collect();
  network.sort_by_key(|h| std::cmp::Reverse(h.count));
  network.truncate(TOP);
  Summary {
    run: id.to_string(),
    profile: t.profile.clone(),
    state: t.state,
    duration_ms: (ended - created).max(0),
    actor_exit: t.actor_exit.clone(),
    failure: t.failure.clone(),
    started_by: t.started_by.as_ref().map(Principal::to_string),
    stopped_by: t.stopped_by.as_ref().map(Principal::to_string),
    actor_stop: t.actor_stop.clone(),
    dropped_events: t.dropped,
    route_names: t.route_names.clone().map(|names| names.unwrap_or_else(|| vec!["*".to_string()])),
    actor_driver: t.actor_driver.clone(),
    actor_capture: t.actor_capture,
    commands: Commands {
      routed: t
        .routed_by
        .iter()
        .map(|(executor, commands)| Routed {
          executor: executor.clone(),
          commands: *commands,
          failed: t.failed_by.get(executor).copied().unwrap_or(0),
        })
        .collect(),
      kept_on_actor: t.kept,
      cli: t.cli,
      denied: t.denied,
      faults: t.faults,
      failed: t.failed,
      model_calls: t.model_calls,
    },
    top_commands,
    slowest_commands: slowest.into_iter().take(3).map(|(ms, command)| SlowCommand { command, ms }).collect(),
    network,
    latency: latency(t),
    findings,
    events: (level == Level::Detailed).then(|| {
      events
        .iter()
        .map(|recorded| TimelineEvent {
          offset_ms: recorded.time_ms - created,
          kind: recorded.event.kind().to_string(),
          // NOTICE: an event's payload is JSON by construction.
          payload: serde_json::from_str(&recorded.event.payload()).unwrap_or_default(),
        })
        .collect()
    }),
  }
}

fn write_report(out: &mut String, id: &str, events: &[Recorded], t: &Tally, level: Level) -> std::fmt::Result {
  let (created, ended) = span(events, t);
  let driver = match &t.actor_exit {
    None => "-".to_string(),
    Some(ActorExit {
      signal: Some(signal),
      ..
    }) => signal.clone(),
    Some(ActorExit { code, .. }) => format!("exit {}", code.map_or("-".to_string(), |c| c.to_string())),
  };

  writeln!(out, "# Run report: {id} (profile: {})\n", t.profile.as_deref().unwrap_or("-"))?;
  if t.dropped > 0 {
    writeln!(
      out,
      "> ⚠️ The trace is incomplete: {} events were dropped when the trace queue was full, so the counts below may be low.\n",
      t.dropped
    )?;
  }
  writeln!(out, "## Overview\n")?;
  let by = |principal: &Option<Principal>| principal.as_ref().map_or("-".to_string(), Principal::to_string);
  if t.started_by.is_some() || t.stopped_by.is_some() {
    write!(out, "Started by `{}`, stopped by `{}`", by(&t.started_by), by(&t.stopped_by))?;
    if let Some(how) = &t.actor_stop {
      write!(out, " ({})", actor_stop(how))?;
    }
    writeln!(out, ".\n")?;
  }
  writeln!(
    out,
    "| Duration | State | Driving program | Routed to executor | Kept on actor | CLI commands | Failed | Model calls (actor gateway) |"
  )?;
  writeln!(out, "| --- | --- | --- | --- | --- | --- | --- | --- |")?;
  writeln!(
    out,
    "| {} | {} | {driver} | {} | {} | {} | {} | {} |\n",
    duration(ended - created),
    t.state.map_or("-", RunState::as_str),
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
    None => writeln!(out, "🏠 No actor ran in this run.\n")?,
    Some(None) => writeln!(out, "🏠 Kept on actor counts every command the actor ran, except needy's own init and proxy.\n")?,
    Some(Some(names)) if names.is_empty() => {
      writeln!(out, "🏠 The actor routes no command names to needy, so commands it ran are not counted as kept on actor.\n")?
    }
    Some(Some(names)) => {
      let names = names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
      writeln!(
        out,
        "🏠 Kept on actor counts only commands the actor's kernel routed to needy ({names}); other commands ran on the actor without a trace event.\n"
      )?;
    }
  }
  if let Some(driver) = &t.actor_driver {
    writeln!(out, "The actor ran in `{driver}`.\n")?;
  }
  if t.actor_capture == Some(Capture::Shims) {
    writeln!(out, "{SHIM_ROUTING}\n")?;
  }

  writeln!(out, "## Findings\n")?;
  let mut any = false;
  if let Some(reason) = &t.failure {
    any = true;
    writeln!(out, "- ❌ Run failed: {reason}")?;
  }
  for (transfer, name, reason) in &t.files_failed {
    any = true;
    let transfer = match transfer {
      Transfer::CopyIn => "Copy-in",
      Transfer::MirrorOut => "Mirror-out",
    };
    writeln!(out, "- ❌ {transfer} `{name}` failed: {reason}")?;
  }
  if !t.probes.is_empty() {
    any = true;
    writeln!(out, "- ⚠️ Environment probing: {} identity commands ({})", t.probes.values().sum::<usize>(), top(&t.probes, TOP))?;
  }
  for (argv, message) in &t.denials {
    any = true;
    writeln!(out, "- 🚫 Denied command: `{argv}`: {message}")?;
  }
  for (host, network) in &t.net_denials {
    any = true;
    writeln!(out, "- 🚫 Network denied: {host} ({} network)", network_name(*network))?;
  }
  if t.faults > 0 {
    any = true;
    writeln!(out, "- ℹ️ Injected faults: {} ({})", t.faults, top(&t.fault_names, TOP))?;
  }
  if !t.chaos_changes.is_empty() {
    any = true;
    writeln!(out, "- ℹ️ Chaos changed while running: {}", t.chaos_changes.join(", "))?;
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
  let hooks = latency(t);
  if !hooks.is_empty() {
    writeln!(out, "\n## Latency\n")?;
    writeln!(out, "| Hook | Count | Mean | Min | Max |")?;
    writeln!(out, "| --- | --- | --- | --- | --- |")?;
    for h in &hooks {
      writeln!(out, "| `{}` | {} | {} | {} | {} |", h.hook, h.count, ms(h.mean_us), ms(h.min_us), ms(h.max_us))?;
    }
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
