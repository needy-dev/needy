//! Rule actions: one merge of preset and rule actions, and their
//! accumulation over the rules that match.

use std::collections::BTreeMap;
use std::time::Duration;

use needy_config::profile::{Actions, EnvOps, Fault, Profile};

use crate::pattern::Match;

/// How deep presets may `use` other presets.
const PRESET_DEPTH: usize = 8;

/// Preset actions first, then the rule's own; for single-valued fields the
/// rule's own value wins.
pub(super) fn combine(profile: &Profile, own: &Actions) -> Actions {
  let mut parts = Vec::new();
  collect_presets(profile, own, &mut parts, 0);
  let mut out = Actions {
    to: own.to.clone(),
    executor: own.executor.clone(),
    deny: own.deny.clone(),
    timeout: own.timeout,
    cwd: own.cwd.clone(),
    exec: own.exec.clone(),
    stop: own.stop,
    ..Actions::default()
  };
  for part in parts.iter().copied().chain(std::iter::once(own)) {
    out.to = out.to.or_else(|| part.to.clone());
    out.executor = out.executor.or_else(|| part.executor.clone());
    out.deny = out.deny.or_else(|| part.deny.clone());
    out.timeout = out.timeout.or(part.timeout);
    out.cwd = out.cwd.or_else(|| part.cwd.clone());
    out.exec = out.exec.or_else(|| part.exec.clone());
    out.stop |= part.stop;
    out.args.prepend.extend(part.args.prepend.iter().cloned());
    out.args.append.extend(part.args.append.iter().cloned());
    out.wrap.extend(part.wrap.iter().cloned());
    out.files.extend(part.files.iter().cloned());
    out.faults.extend(part.faults.iter().cloned());
    out.env.clear |= part.env.clear;
    out.env.pass.extend(part.env.pass.iter().cloned());
    out.env.unset.extend(part.env.unset.iter().cloned());
    out.env.set.extend(part.env.set.clone());
    out.env.default.extend(part.env.default.clone());
    out.env.prepend.extend(part.env.prepend.clone());
    out.env.append.extend(part.env.append.clone());
  }
  out
}

fn collect_presets<'a>(profile: &'a Profile, actions: &Actions, out: &mut Vec<&'a Actions>, depth: usize) {
  if depth > PRESET_DEPTH {
    return;
  }
  for name in &actions.uses {
    if let Some(preset) = profile.presets.get(name) {
      collect_presets(profile, preset, out, depth + 1);
      out.push(preset);
    }
  }
}

/// The placement an action set names, if any: `actor`, an executor name, or
/// `executor` for the default executor.
pub(super) fn placement_name(actions: &Actions) -> Option<String> {
  match actions.to.as_deref() {
    Some("executor") => actions.executor.clone().or(Some("executor".into())),
    Some(to) => Some(to.to_string()),
    None => actions.executor.clone(),
  }
}

/// The actions of the rules matched so far.
#[derive(Default)]
pub(super) struct Accumulated {
  pub placement: Option<String>,
  pub deny: Option<String>,
  pub timeout: Option<Duration>,
  pub cwd: Option<String>,
  exec: Option<Vec<String>>,
  prepend: Vec<String>,
  append: Vec<String>,
  wraps: Vec<String>,
  pub clear_env: bool,
  pass: Vec<String>,
  env_ops: Vec<EnvOps>,
  pub files: Vec<String>,
  pub faults: Vec<Fault>,
}

impl Accumulated {
  pub fn apply(&mut self, a: &Actions, m: &Match) {
    if self.placement.is_none() {
      self.placement = placement_name(a);
    }
    self.deny = self.deny.take().or_else(|| a.deny.clone());
    self.timeout = self.timeout.or(a.timeout.map(Into::into));
    self.cwd = self.cwd.take().or_else(|| a.cwd.clone());
    if self.exec.is_none() {
      self.exec = a.exec.as_ref().map(|words| m.expand(words));
    }
    self.prepend.extend(m.expand(&a.args.prepend));
    self.append.extend(m.expand(&a.args.append));
    self.wraps.extend(m.expand(&a.wrap));
    for file in &a.files {
      if !self.files.contains(file) {
        self.files.push(file.clone());
      }
    }
    self.faults.extend(a.faults.iter().cloned());
    self.clear_env |= a.env.clear;
    self.pass.extend(a.env.pass.iter().cloned());
    self.env_ops.push(a.env.clone());
  }

  /// The arguments to run: `exec` or the original command, with `prepend`
  /// after the command name, `append` at the end and `wrap` around it all.
  pub fn rewrite(&self, original: &[String]) -> Vec<String> {
    let mut argv = self.exec.clone().unwrap_or_else(|| original.to_vec());
    if !argv.is_empty() && !self.prepend.is_empty() {
      let tail = argv.split_off(1);
      argv.extend(self.prepend.iter().cloned());
      argv.extend(tail);
    }
    argv.extend(self.append.iter().cloned());
    if self.wraps.is_empty() {
      return argv;
    }
    let mut wrapped = self.wraps.clone();
    wrapped.extend(argv);
    wrapped
  }

  /// `clear` limits only the inherited environment to `pass`; values the
  /// rules set are applied on top, in rule order.
  pub fn environment(&self, inherited: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = inherited.clone();
    if self.clear_env {
      env.retain(|k, _| self.pass.contains(k));
    }
    for ops in &self.env_ops {
      for key in &ops.unset {
        env.remove(key);
      }
      env.extend(ops.set.clone());
      for (k, v) in &ops.default {
        env.entry(k.clone()).or_insert_with(|| v.clone());
      }
      for (k, v) in &ops.prepend {
        let joined = match env.get(k) {
          Some(old) if !old.is_empty() => format!("{v}:{old}"),
          _ => v.clone(),
        };
        env.insert(k.clone(), joined);
      }
      for (k, v) in &ops.append {
        let joined = match env.get(k) {
          Some(old) if !old.is_empty() => format!("{old}:{v}"),
          _ => v.clone(),
        };
        env.insert(k.clone(), joined);
      }
    }
    env
  }
}
