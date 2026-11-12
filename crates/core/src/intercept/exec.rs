//! The exec domain: `exec.decide`, where a command runs and how it is
//! rewritten. The actor's proxy asks it for every exec its capture caught,
//! and `needy exec` for commands from outside.

use std::collections::BTreeMap;
use std::time::Duration;

use needy_config::profile::{Fault, Profile};

use crate::chaos::Injector;
use crate::rules::Rules;
use crate::types::{Fired, Outcome, Placement};

/// What needy knows about a command when it decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecContext {
  pub argv: Vec<String>,
  pub cwd: String,
  pub env: BTreeMap<String, String>,
  pub tty: bool,
  /// Argument vectors of ancestor processes, nearest first. The actor's
  /// proxy sends them in `Decide`; other execs have none.
  pub ancestors: Vec<Vec<String>>,
}

/// The `exec.decide` result: where a command runs and how it is rewritten.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
  pub outcome: Outcome,
  /// Arguments to run, after prepend, append, wrap and exec.
  pub argv: Vec<String>,
  /// Final environment after the rules' changes to the context's environment.
  pub env: BTreeMap<String, String>,
  /// The process should start from `env` alone, not the executor's environment.
  pub clear_env: bool,
  pub cwd: String,
  pub files: Vec<String>,
  /// Faults the matching rules name.
  pub faults: Vec<Fault>,
  pub timeout: Option<Duration>,
  /// Indices of the rules that matched, in order.
  pub matched: Vec<usize>,
  /// The command after launcher resolution; rules match against this.
  pub effective: Vec<String>,
  /// Faults that chaos injected into this command. The flow applies their
  /// delays; an errno is already in `outcome`.
  pub fired: Vec<Fired>,
}

impl Decision {
  /// The command as given, placed at `placement`, before any interceptor.
  pub fn unchanged(ctx: &ExecContext, placement: Placement) -> Decision {
    Decision {
      outcome: Outcome::Run(placement),
      argv: ctx.argv.clone(),
      env: ctx.env.clone(),
      clear_env: false,
      cwd: ctx.cwd.clone(),
      files: Vec::new(),
      faults: Vec::new(),
      timeout: None,
      matched: Vec::new(),
      effective: ctx.argv.clone(),
      fired: Vec::new(),
    }
  }
}

/// An `exec.decide` interceptor: it sees the decision so far and returns it,
/// changed or not.
pub trait ExecDecide {
  fn decide(&self, ctx: &ExecContext, decision: Decision) -> Decision;
}

/// Runs an `exec.decide` chain from `start`.
pub fn decide(chain: &[&dyn ExecDecide], ctx: &ExecContext, start: Decision) -> Decision {
  super::fold(chain, start, |interceptor, decision| interceptor.decide(ctx, decision))
}

/// A run's `exec.decide` chain for one command: the profile's rules, then
/// chaos. A command placed on an executor by name skips the rules and
/// starts on that executor.
// TODO(intercept): a user decider after chaos (the `ask` action in the
// profile note) is deferred until the owner approves its profile syntax; it
// would be recorded for replay and off by default.
pub struct Chain<'a> {
  rules: Option<Rules<'a>>,
  injector: &'a Injector,
  start: Placement,
}

impl<'a> Chain<'a> {
  /// The chain for a command of a run with `profile` and `injector`;
  /// `executor` is the executor the caller placed it on by name, if any.
  pub fn new(profile: &'a Profile, injector: &'a Injector, executor: Option<&str>) -> Self {
    match executor {
      None => Self {
        rules: Some(Rules::new(profile)),
        injector,
        start: Placement::Actor,
      },
      Some(name) => Self {
        rules: None,
        injector,
        start: Placement::Executor(name.into()),
      },
    }
  }

  /// Decides the command `ctx`.
  pub fn decide(&self, ctx: &ExecContext) -> Decision {
    let mut chain: Vec<&dyn ExecDecide> = Vec::with_capacity(2);
    if let Some(rules) = &self.rules {
      chain.push(rules);
    }
    chain.push(self.injector);
    decide(&chain, ctx, Decision::unchanged(ctx, self.start.clone()))
  }
}
