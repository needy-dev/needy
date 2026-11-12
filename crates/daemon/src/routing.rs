//! Routing: the exec domain's adapter. It runs a run's `exec.decide` chain
//! (`needy_core::intercept::exec::Chain`: rules, then chaos) for a command,
//! applies the decision (where the command runs) and rewrites it through the
//! executor's path map.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use needy_api_server::control::{ControlError, ExecSpec, Route, Target};
use needy_core::event::{Event, ExecDecide};
use needy_core::intercept::exec::{Chain, Decision, ExecContext};
use needy_core::types::{Origin, Outcome, Placement};

use crate::pool::Instance;
use crate::run::Run;

/// Runs the `exec.decide` chain for one command of `run`: rules, then
/// chaos. A command placed on `executor` explicitly skips the rules. The
/// delays of injected faults are applied here, and each fault is recorded.
/// The chain's latency, without the delays, is the `exec.decide` timing.
async fn decide(run: &Run, ctx: &ExecContext, executor: Option<&str>) -> Decision {
  let started = std::time::Instant::now();
  let decision = Chain::new(&run.profile, &run.injector, executor).decide(ctx);
  run.trace.time("exec.decide", started.elapsed());
  for fired in &decision.fired {
    if let Some(delay) = fired.effect.delay {
      tokio::time::sleep(delay).await;
    }
    run.emit(&Event::FaultInjected(fired.clone()));
  }
  decision
}

/// Records an `exec.decide` event and returns it; `outcome` is where the
/// command really runs, which for a command from the CLI may differ from
/// the rules'.
fn record(run: &Run, ctx: &ExecContext, decision: &Decision, origin: Origin, outcome: Outcome) -> Event {
  let event = Event::ExecDecide(ExecDecide {
    argv: ctx.argv.clone(),
    effective: decision.effective.clone(),
    origin,
    outcome,
    rules: decision.matched.clone(),
  });
  run.emit(&event);
  event
}

/// Routes a command the actor's proxy asks about.
pub async fn route(run: &Run, ctx: ExecContext) -> Route {
  let decision = decide(run, &ctx, None).await;
  record(run, &ctx, &decision, Origin::ActorProxy, decision.outcome.clone());
  // A refusal is what the proxy prints, worded like `needy exec`'s.
  let (target, instance) = match &decision.outcome {
    Outcome::Run(Placement::Actor) => (Target::Actor, None),
    Outcome::Run(Placement::Executor(name)) => (Target::Executor(name.clone()), run.executor(name)),
    Outcome::Deny(message) => (Target::Refused(format!("denied: {message}")), None),
    Outcome::Fault(errno) => (Target::Refused(format!("fault injected: {errno}")), None),
  };
  let command = executor_command(instance.as_deref(), &decision);
  Route {
    target,
    argv: command.argv,
    env: changed_env(&decision, &ctx.env),
    clear_env: decision.clear_env,
    cwd: command.cwd,
    search_path_if_missing: command.search_path_if_missing,
  }
}

/// Places an exec of `run`: runs the `exec.decide` chain unless the
/// command comes from the actor (the proxy decided already; a mount is not
/// a command), rewrites `spec` and returns the instance it runs on with
/// the events of the decision. A refusal carries those events.
pub async fn place(run: &Run, spec: &mut ExecSpec) -> Result<(Arc<Instance>, Vec<Event>), ControlError> {
  let origin = spec.origin;
  let explicit = (!spec.executor.is_empty()).then(|| spec.executor.clone());
  let decision = match origin {
    Origin::ActorProxy | Origin::ActorMount => None,
    Origin::Cli | Origin::Fs | Origin::Files => {
      let ctx = context(spec);
      Some((decide(run, &ctx, explicit.as_deref()).await, ctx))
    }
  };
  let mut events: Vec<Event> = decision.iter().flat_map(|(d, _)| d.fired.iter().cloned().map(Event::FaultInjected)).collect();
  if let Some((decision, ctx)) = &decision {
    let refusal = match &decision.outcome {
      Outcome::Deny(message) => Some(format!("denied: {message}")),
      Outcome::Fault(errno) => Some(format!("fault injected: {errno}")),
      Outcome::Run(_) => None,
    };
    if let Some(message) = refusal {
      if explicit.is_none() {
        events.push(record(run, ctx, decision, origin, decision.outcome.clone()));
      }
      return Err(ControlError::Denied { message, events });
    }
  }
  if explicit.is_none() {
    // A command the rules keep on the actor runs on the default executor:
    // a CLI command has no actor to run on.
    spec.executor = match decision.as_ref().map(|(d, _)| &d.outcome) {
      Some(Outcome::Run(Placement::Executor(name))) => name.clone(),
      _ => run.profile.default_executor.clone().unwrap_or_default(),
    };
  }
  let executors = run.executors();
  let instance = executors
    .get(&spec.executor)
    .or_else(|| executors.values().next())
    .cloned()
    .ok_or_else(|| ControlError::NotFound("the run has no executor".into()))?;
  if let (None, Some((decision, ctx))) = (&explicit, &decision) {
    let command = executor_command(Some(&instance), decision);
    spec.argv = command.argv;
    spec.cwd = command.cwd;
    spec.search_path_if_missing = command.search_path_if_missing;
    spec.env.extend(changed_env(decision, &ctx.env));
    // The decision records where the command really runs.
    events.push(record(run, ctx, decision, origin, Outcome::Run(Placement::Executor(instance.name.clone()))));
  }
  Ok((instance, events))
}

fn context(spec: &ExecSpec) -> ExecContext {
  ExecContext {
    argv: spec.argv.clone(),
    cwd: spec.cwd.clone(),
    env: spec.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    tty: spec.tty.is_some(),
    // `Decide` fills the ancestry it carries; other execs have none.
    ancestors: Vec::new(),
  }
}

/// A decision's command as it runs on an executor.
struct ExecutorCommand {
  argv: Vec<String>,
  cwd: String,
  /// The executor may run the program of the same name from its PATH when
  /// the actor's absolute path does not exist there.
  search_path_if_missing: bool,
}

/// The command a decision runs on `instance`: its arguments and working
/// directory through the executor's path map. A program the path map maps
/// runs from the mapped path; any other absolute program keeps the actor's
/// path, and the executor falls back to its own PATH when that path is
/// missing there (the actor's `/usr/bin/python3` may live elsewhere).
fn executor_command(instance: Option<&Instance>, decision: &Decision) -> ExecutorCommand {
  let mut argv = decision.argv.clone();
  let mut cwd = decision.cwd.clone();
  let mut search_path_if_missing = false;
  if let Some(instance) = instance {
    for (from, to) in &instance.path_map {
      cwd = map_prefix(&cwd, from, to);
      argv = argv.iter().map(|a| map_prefix(a, from, to)).collect();
    }
    search_path_if_missing = match (argv.first(), decision.argv.first()) {
      (Some(program), Some(asked)) => program == asked && program.starts_with('/'),
      _ => false,
    };
  }
  ExecutorCommand {
    argv,
    cwd,
    search_path_if_missing,
  }
}

/// The variables a decision sets or changes relative to `asked`; all of them
/// when the decision clears the environment.
fn changed_env(decision: &Decision, asked: &BTreeMap<String, String>) -> HashMap<String, String> {
  decision.env.iter().filter(|(k, v)| asked.get(*k) != Some(*v) || decision.clear_env).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Whether the working directory `cwd` of a command on `instance` lies
/// outside every path the instance's path map maps to, so that it is the
/// actor's own path.
pub(crate) fn unmapped(instance: &Instance, cwd: &str) -> bool {
  let within = |to: &String| cwd == to || cwd.starts_with(&format!("{}/", to.trim_end_matches('/')));
  !cwd.is_empty() && !instance.path_map.values().any(within)
}

/// `value` with the path prefix `from` replaced by `to`; a path is mapped
/// only at a component boundary.
fn map_prefix(value: &str, from: &str, to: &str) -> String {
  if value == from {
    return to.to_string();
  }
  match value.strip_prefix(&format!("{}/", from.trim_end_matches('/'))) {
    Some(rest) => format!("{}/{rest}", to.trim_end_matches('/')),
    None => value.to_string(),
  }
}

#[cfg(test)]
#[path = "routing_test.rs"]
mod tests;
