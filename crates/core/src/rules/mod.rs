//! Command rules: launcher resolution, matching and composition into a
//! [`Decision`].
//!
//! A decision depends only on the profile and the command context, so a
//! run can be replayed. Every matching rule applies in order;
//! single-valued fields take the first rule that sets them and collections
//! accumulate, until a rule with `final: true`.

mod actions;
mod launcher;
mod shell;
mod when;

use needy_config::profile::{Profile, UnknownPolicy};

use crate::intercept::exec::{Decision, ExecContext, ExecDecide};
use crate::pattern::Pattern;
use crate::types::{Outcome, Placement};
use actions::{Accumulated, combine};

/// A profile's rules with their patterns compiled once, the first
/// `exec.decide` interceptor.
pub struct Rules<'p> {
  profile: &'p Profile,
  commands: Vec<Pattern>,
  /// Each command rule's `when.under`.
  under: Vec<Option<Pattern>>,
  resolvers: Vec<Pattern>,
}

/// How deep a shell script's subcommands are decided.
const SHELL_DEPTH: usize = 4;

impl<'p> Rules<'p> {
  pub fn new(profile: &'p Profile) -> Self {
    Self {
      profile,
      commands: profile.commands.iter().map(|rule| Pattern::compile(&rule.matcher)).collect(),
      under: profile.commands.iter().map(|rule| rule.when.under.as_deref().map(Pattern::compile)).collect(),
      resolvers: profile.resolvers.iter().map(|resolver| Pattern::compile(&resolver.matcher)).collect(),
    }
  }

  /// Decides where a command runs and how it is rewritten.
  pub fn decide(&self, ctx: &ExecContext) -> Decision {
    self.decide_at(ctx, 0).0
  }

  /// Returns the decision and whether a rule placed or denied it explicitly.
  fn decide_at(&self, ctx: &ExecContext, depth: usize) -> (Decision, bool) {
    let profile = self.profile;
    let effective = launcher::resolve(profile, &self.resolvers, &ctx.argv);
    let mut state = Accumulated::default();
    let mut matched = Vec::new();
    for (index, rule) in profile.commands.iter().enumerate() {
      let Some(m) = self.commands[index].matches(&effective) else {
        continue;
      };
      if !when::holds(&rule.when, self.under[index].as_ref(), &m, ctx, &effective) {
        continue;
      }
      let actions = combine(profile, &rule.actions);
      state.apply(&actions, &m);
      matched.push(index);
      if actions.stop {
        break;
      }
    }

    let explicit = state.deny.is_some() || state.placement.is_some();
    let outcome = if let Some(message) = &state.deny {
      Outcome::Deny(message.clone())
    } else if let Some(name) = &state.placement {
      Outcome::Run(to_placement(profile, name))
    } else if let Some(script) = shell::script(&effective).filter(|_| depth < SHELL_DEPTH) {
      self.shell_outcome(ctx, &effective, script, depth)
    } else {
      Outcome::Run(default_placement(profile))
    };
    let env = state.environment(&ctx.env);
    let argv = state.rewrite(&ctx.argv);

    let decision = Decision {
      outcome,
      argv,
      env,
      clear_env: state.clear_env,
      cwd: state.cwd.map(|c| profile.user.expand_home(&c)).unwrap_or_else(|| ctx.cwd.clone()),
      files: state.files,
      timeout: state.timeout,
      matched,
      effective,
      fired: Vec::new(),
    };
    (decision, explicit)
  }

  /// A shell script's outcome: the common placement of the subcommands that a
  /// rule places explicitly, otherwise the `unknown` policy. Subcommands with
  /// no rule follow the shell; a denied subcommand denies the script.
  fn shell_outcome(&self, ctx: &ExecContext, shell: &[String], script: &str, depth: usize) -> Outcome {
    let profile = self.profile;
    let Some(commands) = shell::word_only(script) else {
      return Outcome::Run(unknown_placement(profile));
    };
    let mut ancestors = vec![shell.to_vec()];
    ancestors.extend(ctx.ancestors.iter().cloned());
    let mut common: Option<Placement> = None;
    for argv in commands {
      let sub = ExecContext {
        argv,
        ancestors: ancestors.clone(),
        ..ctx.clone()
      };
      let (decision, explicit) = self.decide_at(&sub, depth + 1);
      let placement = match decision.outcome {
        Outcome::Run(placement) => placement,
        denied => return denied,
      };
      if !explicit {
        continue;
      }
      match &common {
        None => common = Some(placement),
        Some(existing) if *existing == placement => {}
        Some(_) => return Outcome::Run(unknown_placement(profile)),
      }
    }
    Outcome::Run(common.unwrap_or_else(|| default_placement(profile)))
  }
}

/// Rules come first in the `exec.decide` chain: they decide from the context
/// alone and replace the decision they receive.
impl ExecDecide for Rules<'_> {
  fn decide(&self, ctx: &ExecContext, _: Decision) -> Decision {
    Rules::decide(self, ctx)
  }
}

/// Decides where a command runs and how it is rewritten.
pub fn decide(profile: &Profile, ctx: &ExecContext) -> Decision {
  Rules::new(profile).decide(ctx)
}

fn to_placement(profile: &Profile, name: &str) -> Placement {
  match name {
    "actor" => Placement::Actor,
    "executor" => default_placement(profile),
    other => Placement::Executor(other.to_string()),
  }
}

fn default_placement(profile: &Profile) -> Placement {
  profile.default_executor.clone().map_or(Placement::Actor, Placement::Executor)
}

fn unknown_placement(profile: &Profile) -> Placement {
  match &profile.unknown {
    UnknownPolicy::Sticky | UnknownPolicy::Actor => Placement::Actor,
    UnknownPolicy::Executor(name) => Placement::Executor(name.clone()),
  }
}

/// Executable names the actor hands to the proxy (the `binfmt_needy` route
/// parameter in a VM, the shims' route list in a container): the literal
/// command names of every rule, whatever its actions, and of every exec
/// fault in `chaos.faults`. A rule that only denies, or only changes the
/// environment, arguments, working directory or timeout, must reach the
/// daemon as much as one that places the command on an executor; a program
/// missing here runs in the actor untouched. `None` when a wildcard rule or
/// exec fault, or a default executor, means every exec must be asked about.
pub fn route_names(profile: &Profile) -> Option<Vec<String>> {
  route_names_for(profile, profile.chaos.faults.values().filter_map(|fault| fault.exec.as_deref()))
}

/// [`route_names`] with a running run's exec faults (`exec_faults`, their
/// patterns) in place of the profile's: what `needy chaos add` and `rm`
/// leave the run with. The daemon pushes the list to the actor again when it
/// changes (`GuestService.Control`).
pub fn route_names_for<'a>(profile: &Profile, exec_faults: impl IntoIterator<Item = &'a str>) -> Option<Vec<String>> {
  if profile.default_executor.is_some() {
    return None;
  }
  if let UnknownPolicy::Executor(_) = profile.unknown {
    return None;
  }
  let mut names = Vec::new();
  for rule in &profile.commands {
    names.extend(Pattern::compile(&rule.matcher).command_names()?);
  }
  for pattern in exec_faults {
    names.extend(Pattern::compile(pattern).command_names()?);
  }
  if !names.is_empty() {
    names.extend(shell::SHELLS.iter().map(|s| s.to_string()));
  }
  names.sort();
  names.dedup();
  Some(names)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
