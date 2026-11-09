//! Command matching, launcher resolution and rule composition.
//!
//! Patterns are written like the commands they match: `git **`, `git commit *`,
//! `"{node,bun} {script} **"`. Every matching rule applies in order;
//! single-valued fields take the first rule that sets them and collections
//! accumulate, until a rule with `final: true`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use glob_match::glob_match;
use needy_config::profile::{Actions, EnvOps, Fault, Profile, UnknownPolicy, When};

/// What needy knows about a command when it decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandContext {
  pub argv: Vec<String>,
  pub cwd: String,
  pub env: BTreeMap<String, String>,
  pub tty: bool,
  /// Argument vectors of ancestor processes, nearest first.
  pub ancestors: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
  Actor,
  Executor(String),
  Deny(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
  pub target: Target,
  /// Arguments to run, after prepend, append, wrap and exec.
  pub argv: Vec<String>,
  /// Final environment after the rules' changes to the context's environment.
  pub env: BTreeMap<String, String>,
  /// The process should start from `env` alone, not the executor's environment.
  pub clear_env: bool,
  pub cwd: String,
  pub files: Vec<String>,
  pub faults: Vec<Fault>,
  pub timeout: Option<Duration>,
  /// Indices of the rules that matched, in order.
  pub matched: Vec<usize>,
  /// The command after launcher resolution; rules match against this.
  pub effective: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
  Literal(String),
  One,
  Many,
  Alternatives(Vec<String>),
  Capture(String),
}

fn compile(pattern: &str) -> Vec<Token> {
  pattern
    .split_whitespace()
    .map(|word| match word {
      "*" => Token::One,
      "**" => Token::Many,
      _ => match word.strip_prefix('{').and_then(|w| w.strip_suffix('}')) {
        Some(inner) if inner.contains(',') => Token::Alternatives(inner.split(',').map(str::to_string).collect()),
        Some(inner) if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
          Token::Capture(inner.to_string())
        }
        _ => Token::Literal(word.to_string()),
      },
    })
    .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Match {
  captures: BTreeMap<String, String>,
  /// Arguments matched by the last `**`.
  rest: Vec<String>,
}

/// The program name of a command word: the last path component.
fn basename(arg: &str) -> &str {
  Path::new(arg).file_name().and_then(|name| name.to_str()).unwrap_or(arg)
}

fn word_matches(pattern: &str, arg: &str, first: bool) -> bool {
  if first && !pattern.contains('/') {
    glob_match(pattern, basename(arg))
  } else {
    glob_match(pattern, arg)
  }
}

fn match_pattern(tokens: &[Token], argv: &[String]) -> Option<Match> {
  fn go(tokens: &[Token], argv: &[String], first: bool, m: &mut Match) -> bool {
    let Some(token) = tokens.first() else {
      return argv.is_empty();
    };
    if let Token::Many = token {
      for take in 0..=argv.len() {
        let mut attempt = m.clone();
        attempt.rest = argv[..take].to_vec();
        if go(&tokens[1..], &argv[take..], false, &mut attempt) {
          *m = attempt;
          return true;
        }
      }
      return false;
    }
    let Some(arg) = argv.first() else {
      return false;
    };
    let ok = match token {
      Token::Literal(p) => word_matches(p, arg, first),
      Token::Alternatives(ps) => ps.iter().any(|p| word_matches(p, arg, first)),
      Token::One => true,
      Token::Capture(name) => {
        m.captures.insert(name.clone(), arg.clone());
        true
      }
      Token::Many => unreachable!(),
    };
    ok && go(&tokens[1..], &argv[1..], false, m)
  }
  let mut m = Match::default();
  go(tokens, argv, true, &mut m).then_some(m)
}

fn matches(pattern: &str, argv: &[String]) -> Option<Match> {
  match_pattern(&compile(pattern), argv)
}

fn when_holds(when: &When, m: &Match, ctx: &CommandContext, argv: &[String]) -> bool {
  let args = argv.get(1..).unwrap_or_default();
  if !when.has.is_empty()
    && !when.has.iter().any(|flag| args.iter().any(|a| a == flag || (flag.starts_with("--") && a.starts_with(&format!("{flag}=")))))
  {
    return false;
  }
  if let Some(cwd) = &when.cwd
    && !cwd.matches(&ctx.cwd)
  {
    return false;
  }
  if when.env.iter().any(|(k, v)| ctx.env.get(k).map(String::as_str) != Some(v.as_str())) {
    return false;
  }
  if let Some(under) = &when.under
    && !ctx.ancestors.iter().any(|a| matches(under, a).is_some())
  {
    return false;
  }
  if let Some(tty) = when.tty
    && tty != ctx.tty
  {
    return false;
  }
  when.captures.iter().all(|(name, glob)| m.captures.get(name).is_some_and(|value| glob.matches(value)))
}

/// Substitutes `{name}` captures and expands a `**` word into the rest.
fn expand_template(words: &[String], m: &Match) -> Vec<String> {
  let mut out = Vec::new();
  for word in words {
    if word == "**" {
      out.extend(m.rest.iter().cloned());
      continue;
    }
    let mut text = word.clone();
    for (name, value) in &m.captures {
      text = text.replace(&format!("{{{name}}}"), value);
    }
    out.push(text);
  }
  out
}

/// Preset actions first, then the rule's own; for single-valued fields the
/// rule's own value wins.
fn combine(profile: &Profile, own: &Actions) -> Actions {
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
  if depth > 8 {
    return;
  }
  for name in &actions.uses {
    if let Some(preset) = profile.presets.get(name) {
      collect_presets(profile, preset, out, depth + 1);
      out.push(preset);
    }
  }
}

/// The placement an action set names, if any.
fn placement(actions: &Actions) -> Option<String> {
  match actions.to.as_deref() {
    Some("executor") => actions.executor.clone().or(Some("executor".into())),
    Some(to) => Some(to.to_string()),
    None => actions.executor.clone(),
  }
}

struct State {
  placement: Option<String>,
  deny: Option<String>,
  timeout: Option<Duration>,
  cwd: Option<String>,
  exec: Option<Vec<String>>,
  prepend: Vec<String>,
  append: Vec<String>,
  wraps: Vec<String>,
  clear_env: bool,
  pass: Vec<String>,
  env_ops: Vec<EnvOps>,
  files: Vec<String>,
  faults: Vec<Fault>,
}

impl State {
  fn apply(&mut self, a: &Actions, m: &Match) {
    if self.placement.is_none() {
      self.placement = placement(a);
    }
    self.deny = self.deny.take().or_else(|| a.deny.clone());
    self.timeout = self.timeout.or(a.timeout.map(Into::into));
    self.cwd = self.cwd.take().or_else(|| a.cwd.clone());
    if self.exec.is_none() {
      self.exec = a.exec.as_ref().map(|words| expand_template(words, m));
    }
    self.prepend.extend(expand_template(&a.args.prepend, m));
    self.append.extend(expand_template(&a.args.append, m));
    self.wraps.extend(expand_template(&a.wrap, m));
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

  /// `clear` limits only the inherited environment to `pass`; values the
  /// rules set are applied on top, in rule order.
  fn environment(&self, inherited: &BTreeMap<String, String>) -> BTreeMap<String, String> {
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

fn to_target(profile: &Profile, name: &str) -> Target {
  match name {
    "actor" => Target::Actor,
    "executor" => profile.default_executor.clone().map_or(Target::Actor, Target::Executor),
    other => Target::Executor(other.to_string()),
  }
}

fn default_target(profile: &Profile) -> Target {
  profile.default_executor.clone().map_or(Target::Actor, Target::Executor)
}

fn unknown_target(profile: &Profile) -> Target {
  match &profile.unknown {
    UnknownPolicy::Sticky | UnknownPolicy::Actor => Target::Actor,
    UnknownPolicy::Executor(name) => Target::Executor(name.clone()),
  }
}

/// Decides where a command runs and how it is rewritten.
pub fn decide(profile: &Profile, ctx: &CommandContext) -> Decision {
  decide_at(profile, ctx, 0).0
}

/// Returns the decision and whether a rule placed or denied it explicitly.
fn decide_at(profile: &Profile, ctx: &CommandContext, depth: usize) -> (Decision, bool) {
  let effective = resolve(profile, &ctx.argv);
  let mut state = State {
    placement: None,
    deny: None,
    timeout: None,
    cwd: None,
    exec: None,
    prepend: Vec::new(),
    append: Vec::new(),
    wraps: Vec::new(),
    clear_env: false,
    pass: Vec::new(),
    env_ops: Vec::new(),
    files: Vec::new(),
    faults: Vec::new(),
  };
  let mut matched = Vec::new();
  for (index, rule) in profile.commands.iter().enumerate() {
    let Some(m) = matches(&rule.matcher, &effective) else {
      continue;
    };
    if !when_holds(&rule.when, &m, ctx, &effective) {
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
  let target = if let Some(message) = &state.deny {
    Target::Deny(message.clone())
  } else if let Some(name) = &state.placement {
    to_target(profile, name)
  } else if let Some(script) = shell_script(&effective).filter(|_| depth < 4) {
    shell_target(profile, ctx, &effective, script, depth)
  } else {
    default_target(profile)
  };
  let env = state.environment(&ctx.env);

  let mut argv = state.exec.clone().unwrap_or_else(|| ctx.argv.clone());
  if !argv.is_empty() && !state.prepend.is_empty() {
    let tail = argv.split_off(1);
    argv.extend(state.prepend.iter().cloned());
    argv.extend(tail);
  }
  argv.extend(state.append.iter().cloned());
  if !state.wraps.is_empty() {
    let mut wrapped = state.wraps.clone();
    wrapped.extend(argv);
    argv = wrapped;
  }

  let decision = Decision {
    target,
    argv,
    env,
    clear_env: state.clear_env,
    cwd: state.cwd.map(|c| profile.user.expand_home(&c)).unwrap_or_else(|| ctx.cwd.clone()),
    files: state.files,
    faults: state.faults,
    timeout: state.timeout,
    matched,
    effective,
  };
  (decision, explicit)
}

/// A shell script's placement: the common placement of the subcommands that a
/// rule places explicitly, otherwise the `unknown` policy. Subcommands with no
/// rule follow the shell.
fn shell_target(profile: &Profile, ctx: &CommandContext, shell: &[String], script: &str, depth: usize) -> Target {
  let Some(commands) = word_only(script) else {
    return unknown_target(profile);
  };
  let mut ancestors = vec![shell.to_vec()];
  ancestors.extend(ctx.ancestors.iter().cloned());
  let mut common: Option<Target> = None;
  for argv in commands {
    let sub = CommandContext {
      argv,
      ancestors: ancestors.clone(),
      ..ctx.clone()
    };
    let (decision, explicit) = decide_at(profile, &sub, depth + 1);
    let target = decision.target;
    if let Target::Deny(_) = target {
      return target;
    }
    if !explicit {
      continue;
    }
    match &common {
      None => common = Some(target),
      Some(existing) if *existing == target => {}
      Some(_) => return unknown_target(profile),
    }
  }
  common.unwrap_or_else(|| default_target(profile))
}

const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ash"];

/// The script of `sh -c SCRIPT`, `bash -lc SCRIPT` and similar.
fn shell_script(argv: &[String]) -> Option<&str> {
  let name = basename(argv.first()?);
  if !SHELLS.contains(&name) {
    return None;
  }
  let mut command_flag = false;
  for arg in &argv[1..] {
    if arg == "--" {
      continue;
    }
    if let Some(flags) = arg.strip_prefix('-')
      && !flags.starts_with('-')
      && !flags.is_empty()
    {
      command_flag |= flags.contains('c');
      continue;
    }
    if arg.starts_with("--") {
      continue;
    }
    return command_flag.then_some(arg.as_str());
  }
  None
}

/// Splits a word-only script (plain commands joined by `&&`, `||`, `;`, `|`
/// and newlines) into argument vectors. Returns `None` for anything else:
/// expansions, substitutions, redirects, grouping, background jobs.
pub(crate) fn word_only(script: &str) -> Option<Vec<Vec<String>>> {
  let mut commands: Vec<Vec<String>> = Vec::new();
  let mut current: Vec<String> = Vec::new();
  let mut word = String::new();
  let mut in_word = false;
  let mut chars = script.chars().peekable();

  fn end_word(word: &mut String, in_word: &mut bool, current: &mut Vec<String>) {
    if *in_word {
      current.push(std::mem::take(word));
      *in_word = false;
    }
  }
  fn end_command(current: &mut Vec<String>, commands: &mut Vec<Vec<String>>) -> Option<()> {
    // Leading NAME=value assignments only set the environment.
    let start = current.iter().position(|w| !is_assignment(w)).unwrap_or(current.len());
    let argv = current.split_off(start);
    current.clear();
    if argv.is_empty() {
      return None;
    }
    commands.push(argv);
    Some(())
  }

  while let Some(c) = chars.next() {
    match c {
      '\'' => {
        in_word = true;
        loop {
          match chars.next()? {
            '\'' => break,
            ch => word.push(ch),
          }
        }
      }
      '"' => {
        in_word = true;
        loop {
          match chars.next()? {
            '"' => break,
            '$' | '`' => return None,
            '\\' => word.push(chars.next()?),
            ch => word.push(ch),
          }
        }
      }
      '\\' => {
        in_word = true;
        match chars.next()? {
          '\n' => {}
          ch => word.push(ch),
        }
      }
      ' ' | '\t' => end_word(&mut word, &mut in_word, &mut current),
      ';' | '\n' => {
        end_word(&mut word, &mut in_word, &mut current);
        if !current.is_empty() {
          end_command(&mut current, &mut commands)?;
        }
      }
      '|' | '&' => {
        if c == '&' && chars.peek() != Some(&'&') {
          return None;
        }
        if chars.peek() == Some(&c) {
          chars.next();
        }
        end_word(&mut word, &mut in_word, &mut current);
        end_command(&mut current, &mut commands)?;
      }
      '$' | '`' | '(' | ')' | '<' | '>' | '{' | '}' | '#' => return None,
      ch => {
        in_word = true;
        word.push(ch);
      }
    }
  }
  end_word(&mut word, &mut in_word, &mut current);
  if !current.is_empty() {
    end_command(&mut current, &mut commands)?;
  }
  (!commands.is_empty()).then_some(commands)
}

fn is_assignment(word: &str) -> bool {
  match word.split_once('=') {
    Some((name, _)) => {
      !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    None => false,
  }
}

/// Applies user resolvers, then the built-in table, until neither changes the
/// command.
fn resolve(profile: &Profile, argv: &[String]) -> Vec<String> {
  let mut current = argv.to_vec();
  'outer: for _ in 0..16 {
    for resolver in &profile.resolvers {
      if let Some(m) = matches(&resolver.matcher, &current) {
        match &resolver.effective {
          Some(effective) => {
            let words: Vec<String> = effective.split_whitespace().map(str::to_string).collect();
            let next = expand_template(&words, &m);
            if next != current && !next.is_empty() {
              current = next;
              continue 'outer;
            }
          }
          None => break 'outer,
        }
        break 'outer;
      }
    }
    match builtin_resolve(&current) {
      Some(next) if !next.is_empty() && next != current => current = next,
      _ => break,
    }
  }
  current
}

/// Index of the first argument after options; options in `with_value` take the
/// next argument unless written as `--opt=value`.
fn skip_options(args: &[String], with_value: &[&str], skip_assignments: bool) -> usize {
  let mut i = 0;
  while i < args.len() {
    let arg = args[i].as_str();
    if arg == "--" {
      return i + 1;
    }
    if arg.starts_with('-') && arg.len() > 1 {
      i += if with_value.contains(&arg) { 2 } else { 1 };
      continue;
    }
    if skip_assignments && is_assignment(arg) {
      i += 1;
      continue;
    }
    break;
  }
  i.min(args.len())
}

fn builtin_resolve(argv: &[String]) -> Option<Vec<String>> {
  let name = basename(argv.first()?);
  let args = &argv[1..];
  let rest = |i: usize| -> Option<Vec<String>> {
    let tail = args.get(i..)?;
    (!tail.is_empty()).then(|| tail.to_vec())
  };
  match name {
    "env" => rest(skip_options(args, &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"], true)),
    "sudo" => rest(skip_options(
      args,
      &[
        "-u",
        "-g",
        "-C",
        "-D",
        "-h",
        "-p",
        "-r",
        "-t",
        "-U",
        "-T",
        "--user",
        "--group",
        "--chdir",
        "--prompt",
        "--role",
        "--type",
        "--host",
        "--other-user",
        "--close-from",
        "--command-timeout",
      ],
      true,
    )),
    "nice" => rest(skip_options(args, &["-n", "--adjustment"], false)),
    "ionice" => rest(skip_options(args, &["-c", "-n", "--class", "--classdata"], false)),
    "nohup" | "corepack" => rest(skip_options(args, &[], false)),
    "setsid" => rest(skip_options(args, &[], false)),
    "stdbuf" => rest(skip_options(args, &["-i", "-o", "-e"], false)),
    "time" => rest(skip_options(args, &["-f", "-o", "--format", "--output"], false)),
    "timeout" => {
      let i = skip_options(args, &["-s", "-k", "--signal", "--kill-after"], false);
      rest(i + 1)
    }
    "pnpm" if args.first().map(String::as_str) == Some("exec") => {
      rest(1 + skip_options(&args[1..], &["--filter", "-F", "-C", "--dir"], false))
    }
    "npm" if args.first().map(String::as_str) == Some("exec") => {
      let dash = args.iter().position(|a| a == "--")?;
      rest(dash + 1)
    }
    "uv" if args.first().map(String::as_str) == Some("run") => rest(
      1 + skip_options(
        &args[1..],
        &[
          "--with",
          "--python",
          "-p",
          "--project",
          "--directory",
          "--package",
          "--extra",
          "--group",
          "--env-file",
        ],
        false,
      ),
    ),
    _ => None,
  }
}

/// Executable names the actor's kernel should hand to the proxy: literal
/// command names that some rule may place on an executor. `None` when a
/// wildcard rule or a default executor means every exec must be asked about.
pub fn route_names(profile: &Profile) -> Option<Vec<String>> {
  if profile.default_executor.is_some() {
    return None;
  }
  if let UnknownPolicy::Executor(_) = profile.unknown {
    return None;
  }
  let mut names = Vec::new();
  for rule in &profile.commands {
    let actions = combine(profile, &rule.actions);
    let places_on_executor = match placement(&actions).as_deref() {
      Some("actor") | None => false,
      Some(_) => true,
    };
    if !places_on_executor {
      continue;
    }
    match compile(&rule.matcher).first() {
      Some(Token::Literal(word)) if !word.contains(['*', '?']) => names.push(word.clone()),
      Some(Token::Alternatives(words)) if words.iter().all(|w| !w.contains(['*', '?'])) => names.extend(words.iter().cloned()),
      _ => return None,
    }
  }
  if !names.is_empty() {
    names.extend(SHELLS.iter().map(|s| s.to_string()));
  }
  names.sort();
  names.dedup();
  Some(names)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
  }

  fn ctx(s: &str) -> CommandContext {
    CommandContext {
      argv: argv(s),
      cwd: "/home/agent/workspace".into(),
      env: BTreeMap::from([
        ("PATH".into(), "/usr/bin".into()),
        ("HOME".into(), "/home/agent".into()),
      ]),
      ..Default::default()
    }
  }

  fn profile(yaml: &str) -> Profile {
    needy_config::profile::load(&needy_config::Source::text(yaml, ".")).unwrap()
  }

  #[test]
  fn patterns() {
    assert!(matches("git **", &argv("git")).is_some());
    assert!(matches("git **", &argv("/usr/bin/git status -s")).is_some());
    assert!(matches("git commit *", &argv("git commit -a")).is_some());
    assert!(matches("git commit *", &argv("git commit -a -m")).is_none());
    assert!(matches("pnpm dlx prettier **", &argv("pnpm dlx prettier --write x")).is_some());
    assert!(matches("pnpm dlx prettier **", &argv("pnpm dlx pi")).is_none());
    let m = matches("{node,bun} {script} **", &argv("bun /opt/pi/dist/cli.js --x")).unwrap();
    assert_eq!(m.captures["script"], "/opt/pi/dist/cli.js");
    assert_eq!(m.rest, argv("--x"));
    assert!(matches("/usr/local/bin/python3 **", &argv("/usr/local/bin/python3 -V")).is_some());
    assert!(matches("/usr/local/bin/python3 **", &argv("python3 -V")).is_none());
    assert!(matches("**", &argv("anything at all")).is_some());
    assert!(matches("*.sh", &argv("./build.sh")).is_some());
  }

  #[test]
  fn composition_example_from_the_doc() {
    let p = profile(
      r#"
executors: { build: { runtime: docker }, mac: { runtime: remote } }
presets:
  clean-env: { env: { clear: true, pass: [PATH, HOME, LANG, TERM] } }
  mac:       { to: mac }
commands:
  - match: "**"
    env: { set: { CI: "1" } }
  - match: pnpm dlx pi **
    to: actor
    final: true
  - match: "{node,bun} {script} **"
    when: { captures: { script: "**/pi/dist/cli.js" } }
    to: actor
    final: true
  - match: "{uname,lscpu,sw_vers} **"
    to: actor
  - match: curl **
    when: { has: [-k, --insecure] }
    deny: insecure TLS is not allowed
  - match: git **
    args: { prepend: [-c, color.ui=never] }
  - match: "{xcodebuild,codesign} **"
    use: [mac, clean-env]
  - match: "**"
    to: build
    use: [clean-env]
"#,
    );
    let d = decide(&p, &ctx("git status"));
    assert_eq!(d.target, Target::Executor("build".into()));
    assert_eq!(d.argv, argv("git -c color.ui=never status"));
    assert!(d.clear_env);
    // `clear` limits only the inherited environment; CI=1 from rule 0 stays.
    assert_eq!(d.env.get("CI").map(String::as_str), Some("1"));
    assert!(!d.env.contains_key("SHELL"));
    assert_eq!(d.env.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(d.matched, vec![0, 5, 7]);

    let d = decide(&p, &ctx("pnpm dlx pi --help"));
    assert_eq!(d.target, Target::Actor);
    assert_eq!(d.matched, vec![0, 1]);
    assert_eq!(d.env.get("CI").map(String::as_str), Some("1"));

    let d = decide(&p, &ctx("node /opt/pi/dist/cli.js"));
    assert_eq!(d.target, Target::Actor);
    let d = decide(&p, &ctx("node server.js"));
    assert_eq!(d.target, Target::Executor("build".into()));

    assert_eq!(decide(&p, &ctx("uname -a")).target, Target::Actor);
    assert_eq!(decide(&p, &ctx("curl -k https://x")).target, Target::Deny("insecure TLS is not allowed".into()));
    assert_eq!(decide(&p, &ctx("curl https://x")).target, Target::Executor("build".into()));
    assert_eq!(decide(&p, &ctx("xcodebuild -list")).target, Target::Executor("mac".into()));
  }

  #[test]
  fn env_args_wrap_exec_and_cwd() {
    let p = profile(
      r#"
commands:
  - match: "pytest **"
    args: { append: [-p, no:cacheprovider] }
    wrap: timeout 600
    env:
      default: { LANG: C.UTF-8 }
      prepend: { PATH: /opt/tools/bin }
      unset: [HOME]
    cwd: ~/workspace/tests
    timeout: 10m
  - match: "pytest **"
    wrap: [nice]
    env: { append: { PATH: /extra } }
    timeout: 1s
  - match: "curl {url}"
    exec: audited-fetch --log {url}
"#,
    );
    let d = decide(&p, &ctx("pytest -q"));
    assert_eq!(d.argv, argv("timeout 600 nice pytest -q -p no:cacheprovider"));
    assert_eq!(d.env["PATH"], "/opt/tools/bin:/usr/bin:/extra");
    assert_eq!(d.env["LANG"], "C.UTF-8");
    assert!(!d.env.contains_key("HOME"));
    assert_eq!(d.cwd, "/home/agent/workspace/tests");
    assert_eq!(d.timeout, Some(Duration::from_secs(600)));
    assert_eq!(d.target, Target::Actor);

    let d = decide(&p, &ctx("curl https://e.com"));
    assert_eq!(d.argv, argv("audited-fetch --log https://e.com"));
  }

  #[test]
  fn when_conditions() {
    let p = profile(
      r#"
default_executor: build
commands:
  - { match: "node **", when: { cwd: "/home/agent/workspace/**" }, to: a }
  - { match: "make **", when: { env: { CI: "1" } }, to: b }
  - { match: "node **", when: { under: "pnpm test **" }, to: c }
  - { match: "vim **", when: { tty: true }, to: d }
  - { match: "git **", when: { has: [--force] }, deny: no force }
"#,
    );
    let mut c = ctx("node x.js");
    c.cwd = "/tmp".into();
    assert_eq!(decide(&p, &c).target, Target::Executor("build".into()));
    c.ancestors = vec![argv("pnpm test --watch")];
    assert_eq!(decide(&p, &c).target, Target::Executor("c".into()));
    c.cwd = "/home/agent/workspace/app".into();
    assert_eq!(decide(&p, &c).target, Target::Executor("a".into()));
    let mut c = ctx("make all");
    assert_eq!(decide(&p, &c).target, Target::Executor("build".into()));
    c.env.insert("CI".into(), "1".into());
    assert_eq!(decide(&p, &c).target, Target::Executor("b".into()));
    let mut c = ctx("vim f");
    c.tty = true;
    assert_eq!(decide(&p, &c).target, Target::Executor("d".into()));
    assert_eq!(decide(&p, &ctx("git push --force=yes")).target, Target::Deny("no force".into()));
    assert_eq!(decide(&p, &ctx("git push")).target, Target::Executor("build".into()));
  }

  #[test]
  fn builtin_launchers() {
    let p = profile("commands: [{ match: \"git **\", to: build }]");
    for cmd in [
      "env -i FOO=1 git status",
      "sudo -u root git status",
      "nice -n 10 git status",
      "timeout -s KILL 30 git status",
      "nohup git status",
      "stdbuf -o L git status",
      "time -p git status",
      "setsid -f git status",
      "corepack git status",
      "pnpm exec git status",
      "npm exec -- git status",
      "uv run --with x git status",
      "env nice -n 5 sudo git log",
    ] {
      let d = decide(&p, &ctx(cmd));
      assert_eq!(d.target, Target::Executor("build".into()), "{cmd}");
      assert_eq!(d.effective[0], "git", "{cmd}");
      assert_eq!(d.argv, argv(cmd), "argv stays original for {cmd}");
    }
    assert_eq!(decide(&p, &ctx("npm exec git")).target, Target::Actor);
    assert_eq!(decide(&p, &ctx("python -m git")).target, Target::Actor);
  }

  #[test]
  fn user_resolvers() {
    let p = profile(
      r#"
resolvers:
  - { match: "mytool run {script} **", effective: "{script} **" }
  - { match: "make **", sticky: true }
commands:
  - { match: "deploy.sh **", to: build }
  - { match: "make **", to: actor }
"#,
    );
    let d = decide(&p, &ctx("mytool run deploy.sh --prod"));
    assert_eq!(d.effective, argv("deploy.sh --prod"));
    assert_eq!(d.target, Target::Executor("build".into()));
    assert_eq!(decide(&p, &ctx("make all")).target, Target::Actor);
  }

  #[test]
  fn shell_scripts() {
    let mut c = ctx("x");
    let p = profile(
      r#"
unknown: actor
commands:
  - { match: "{git,make} **", to: build }
  - { match: "uname **", to: actor }
  - { match: "rm **", when: { has: [-rf] }, deny: no }
"#,
    );
    c.argv = vec![
      "bash".into(),
      "-lc".into(),
      "git status && make -j4 | tee log".into(),
    ];
    assert_eq!(decide(&p, &c).target, Target::Executor("build".into()));
    c.argv = vec!["sh".into(), "-c".into(), "git status; ls".into()];
    assert_eq!(decide(&p, &c).target, Target::Executor("build".into()));
    c.argv = vec!["sh".into(), "-c".into(), "git status; uname -a".into()];
    assert_eq!(decide(&p, &c).target, Target::Actor);
    c.argv = vec!["sh".into(), "-c".into(), "echo $HOME".into()];
    assert_eq!(decide(&p, &c).target, Target::Actor);
    c.argv = vec!["sh".into(), "-c".into(), "git pull && rm -rf /".into()];
    assert_eq!(decide(&p, &c).target, Target::Deny("no".into()));
    c.argv = vec!["bash".into(), "-c".into(), "CI=1 'git' \"log\" -1".into()];
    assert_eq!(decide(&p, &c).target, Target::Executor("build".into()));

    let sticky = profile("unknown: build\ncommands: [{ match: \"git **\", to: build }]");
    c.argv = vec!["sh".into(), "-c".into(), "git log > out".into()];
    assert_eq!(decide(&sticky, &c).target, Target::Executor("build".into()));
  }

  #[test]
  fn word_only_parser() {
    assert_eq!(
      word_only("a b && c 'd e' | f; g\nh").unwrap(),
      vec![
        argv("a b"),
        vec!["c".into(), "d e".into()],
        argv("f"),
        argv("g"),
        argv("h")
      ]
    );
    assert!(word_only("a > b").is_none());
    assert!(word_only("a $(b)").is_none());
    assert!(word_only("a &").is_none());
    assert!(word_only("a && && b").is_none());
    assert!(word_only("(a)").is_none());
    assert!(word_only("\"$x\"").is_none());
    assert!(word_only("a;").is_some());
    assert!(word_only("").is_none());
  }

  #[test]
  fn route_names_from_rules() {
    let p = profile(
      "commands:\n  - { match: \"{git,make} **\", to: build }\n  - { match: uname, to: actor }\n  - { match: \"cargo **\", executor: build }\n",
    );
    let names = route_names(&p).unwrap();
    assert!(names.contains(&"git".to_string()));
    assert!(names.contains(&"cargo".to_string()));
    assert!(names.contains(&"bash".to_string()));
    assert!(!names.contains(&"uname".to_string()));
    assert_eq!(route_names(&profile("commands: [{ match: \"**\", to: build }]")), None);
    assert_eq!(route_names(&profile("default_executor: build")), None);
    assert_eq!(route_names(&profile("commands: [{ match: uname, to: actor }]")), Some(vec![]));
  }

  #[test]
  fn files_and_faults_accumulate() {
    let p = profile(
      r#"
commands:
  - { match: "**", files: [a] }
  - match: "xcodebuild **"
    files: [b, a]
    faults: [{ on: exec, errno: EIO }]
"#,
    );
    let d = decide(&p, &ctx("xcodebuild"));
    assert_eq!(d.files, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(d.faults.len(), 1);
  }
}
