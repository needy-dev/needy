//! `needy report`: a fixed-format summary of one run, rendered from its
//! typed events in one of three formats: plain text for people (`human`),
//! markdown, and a JSON [`Summary`] for tools. No IO; the caller reads the
//! events and decides whether the text may carry colors.

mod human;
mod markdown;

use std::collections::BTreeMap;

use needy_config::profile::FaultDomain;
use serde::Serialize;

use crate::event::{ActorExit, ActorStop, Event, HookTiming, Recorded, Store, Transfer};
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

/// `s` with its first letter in upper case.
fn capitalize(s: &str) -> String {
  let mut chars = s.chars();
  chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
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
  snapshot: Option<String>,
  created: Option<i64>,
  /// The first end: an actor exit, a stop or a failure.
  ended: Option<i64>,
  /// `Running` once created, then the first terminal state, which stays as
  /// it does in the daemon's run state machine, so the report agrees
  /// with `run ls`.
  // NOTICE: an actor exit that lands while a stop is under way, as a
  // graceful stop's does, is recorded before `run.stop`; a `run.stop` with
  // `actor` or `ended` says the stop established the terminal state, so it
  // makes the run `stopped`. Recovery may have already observed actor exit
  // while completing accepted stop intent, without a new ActorStop outcome.
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
  kept: usize,
  denied: usize,
  faults: usize,
  cli: usize,
  failed: usize,
  model_calls: usize,
  probes: BTreeMap<String, usize>,
  /// Injected faults by `NAME (DOMAIN)`.
  fault_names: BTreeMap<String, usize>,
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
  /// Sync mappings by name: their stores and merges.
  synced: BTreeMap<String, Synced>,
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
        self.snapshot = create.snapshot.clone();
        self.created = Some(time_ms);
        self.started_by = create.by.clone();
        self.state.get_or_insert(RunState::Running);
      }
      Event::RunStop(stop) => {
        if stop.ended || stop.actor.is_some() {
          // This accepted stop owns the terminal state, including when its
          // actor exit was observed before recovery completed the stop.
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
      Event::FilesSync(sync) => {
        self.sync(&sync.name, &sync.store);
      }
      Event::FilesMerge(merge) => {
        let synced = self.sync(&merge.name, &merge.store);
        synced.merge = MergeState::Merged;
        synced.taken = merge.taken;
        synced.resolved = merge.resolved;
        synced.conflicts = merge.conflicts.clone();
        synced.kept = merge.kept.clone();
        synced.reason = None;
      }
      // A merge that runs later, as one pending at a daemon start does,
      // records its `files.merge` after this, which replaces it.
      Event::FilesMergePending(pending) => {
        let synced = self.sync(&pending.name, &pending.store);
        synced.merge = MergeState::Pending;
        synced.reason = Some(pending.reason.clone());
      }
      Event::ChaosAdd(add) => self.chaos_changes.push(format!("+{} ({})", add.name, add.fault.domain())),
      Event::ChaosRemove(remove) => self.chaos_changes.push(format!("-{}", remove.name)),
      Event::ActorStart(start) => {
        self.actor_driver = start.driver.clone();
        self.actor_capture = start.capture;
      }
      Event::ActorReady(_)
      | Event::ExecKill(_)
      | Event::FilesCopyIn(_)
      | Event::FilesMirrorOut(_)
      | Event::ImageBuildStep(_)
      | Event::NetError(_) => {}
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

  /// The sync mapping `name`, with its store as the latest event names it.
  fn sync(&mut self, name: &str, store: &Store) -> &mut Synced {
    let synced = self.synced.entry(name.to_string()).or_insert_with(|| Synced {
      name: name.to_string(),
      store: String::new(),
      account: String::new(),
      merge: MergeState::Unmerged,
      taken: 0,
      resolved: 0,
      conflicts: Vec::new(),
      kept: None,
      reason: None,
    });
    synced.store.clone_from(&store.path);
    synced.account.clone_from(&store.account);
    synced
  }

  fn count_host(&mut self, network: Network, host: &str) {
    let counts = match network {
      Network::Actor => &mut self.actor_net,
      Network::Executor => &mut self.executor_net,
    };
    *counts.entry(host.to_string()).or_default() += 1;
  }
}

/// Renders the report of run `id` from its events, in recorded order.
pub fn render(id: &str, events: &[Recorded], level: Level, format: Format) -> String {
  let mut tally = Tally::default();
  for recorded in events {
    tally.add(recorded.time_ms, &recorded.event);
  }
  let summary = summarize(id, events, &tally, level);
  match format {
    Format::Human { color } => human::render(&summary, color),
    Format::Markdown => markdown::render(&summary),
    // NOTICE: a summary is strings, numbers, lists and maps with string
    // keys, which always serialize to JSON.
    Format::Json => serde_json::to_string_pretty(&summary).expect("a summary serializes to JSON") + "\n",
  }
}

/// What shim routing ([`Capture::Shims`], a container actor's) leaves out:
/// its trampolines catch programs called by name, and its in-place shims
/// only the shells by their paths.
pub(crate) const SHIM_ROUTING: &str = "A container actor routes the programs on its PATH by the names they are called by, \
  and its shells by any path; programs run by another absolute path, or created at run time and run directly, \
  stayed in the actor without a trace event.";

/// What the human and JSON reports show: one run's outcome and counts.
/// Its JSON form is `needy report -o json`; the CLI note lists its fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
  pub run: String,
  pub profile: Option<String>,
  /// The digest (`sha256:<hex>`) of the image snapshot the actor booted;
  /// `None` when booting the base image alone or an OCI reference.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub snapshot: Option<String>,
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
  /// The run's sync mappings: where each store is and how its merge went.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub synced: Vec<Synced>,
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

/// One sync mapping of the run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Synced {
  pub name: String,
  /// The store's directory, on the control host.
  pub store: String,
  /// The account the control daemon reads and writes the store as.
  pub account: String,
  pub merge: MergeState,
  /// Entries the merge took from the run.
  pub taken: u64,
  /// Entries both sides changed that `on-conflict` resolved.
  pub resolved: u64,
  /// Entries both sides changed that nothing resolved; the store kept its
  /// copies.
  pub conflicts: Vec<String>,
  /// Where needy keeps the run's copies of `conflicts`.
  pub kept: Option<String>,
  /// Why a pending merge could not run.
  pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
  /// Merged into its store.
  Merged,
  /// The merge could not run; it runs before the store is next seeded.
  Pending,
  /// Neither yet: the run has not ended, or its trace stops before.
  Unmerged,
}

impl MergeState {
  pub(super) fn as_str(self) -> &'static str {
    match self {
      Self::Merged => "merged",
      Self::Pending => "pending",
      Self::Unmerged => "not merged yet",
    }
  }
}

/// A sync mapping's finding: its conflicts, or its pending merge.
fn sync_finding(synced: &Synced) -> Option<(FindingKind, String)> {
  if synced.merge == MergeState::Pending {
    return Some((FindingKind::MergePending, format!("sync {}: merge pending: {}", synced.name, synced.reason.as_deref().unwrap_or("-"))));
  }
  if synced.conflicts.is_empty() {
    return None;
  }
  let count = synced.conflicts.len();
  let noun = if count == 1 { "conflict" } else { "conflicts" };
  Some((
    FindingKind::SyncConflict,
    format!(
      "sync {}: {count} {noun} kept the store's copies; the run's are in {}: {}",
      synced.name,
      synced.kept.as_deref().unwrap_or("-"),
      synced.conflicts.join(", ")
    ),
  ))
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
  /// A sync mapping's merge left entries both sides changed in conflict.
  SyncConflict,
  /// A sync mapping's merge could not run yet.
  MergePending,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEvent {
  /// Milliseconds since the run was created.
  pub offset_ms: i64,
  pub kind: String,
  pub payload: serde_json::Value,
  /// Original field-ordered JSON payload, used by the Markdown timeline so
  /// key order is preserved before truncation.
  #[serde(skip)]
  pub(super) raw_payload: String,
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
    find(FindingKind::FilesFailed, format!("{transfer} {name} failed: {reason}"));
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
    find(FindingKind::DeniedNetwork, format!("network denied: {host} ({network} network)"));
  }
  if t.faults > 0 {
    find(FindingKind::Faults, format!("injected faults: {} ({})", t.faults, top(&t.fault_names, TOP)));
  }
  for (kind, message) in t.synced.values().filter_map(sync_finding) {
    find(kind, message);
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
    snapshot: t.snapshot.clone(),
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
    synced: t.synced.values().cloned().collect(),
    findings,
    events: (level == Level::Detailed).then(|| {
      events
        .iter()
        .map(|recorded| {
          let raw_payload = recorded.event.payload();
          TimelineEvent {
            offset_ms: recorded.time_ms - created,
            kind: recorded.event.kind().to_string(),
            // NOTICE: an event's payload is JSON by construction.
            payload: serde_json::from_str(&raw_payload).unwrap_or_default(),
            raw_payload,
          }
        })
        .collect()
    }),
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
