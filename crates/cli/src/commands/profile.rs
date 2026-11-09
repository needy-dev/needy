//! `needy profile check`: the profile's and the recipe's findings, where each
//! command would run, and the chaos faults that match it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Subcommand;
use needy_config::Source;
use needy_config::profile::{Fault, FaultKind};
use needy_config::validate::{self, Finding, Severity};
use needy_core::chaos::Injector;
use needy_core::recipe::HostInfo;
use needy_core::rules::{self, CommandContext, Target};
use serde::Serialize;

use crate::cli::Context;
use crate::output::{self, Row};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Load a profile, print its findings and where each command would run.
  /// Exits 1 when a finding is an error.
  Check {
    profile: PathBuf,
    /// A command to decide, as one shell-quoted string; repeat for more.
    #[arg(long = "cmd")]
    commands: Vec<String>,
  },
}

#[derive(Serialize)]
struct Decision {
  command: String,
  target: String,
  argv: Vec<String>,
  rules: Vec<usize>,
  faults: Vec<String>,
}

impl Row for Decision {
  const HEADER: &'static [&'static str] = &["COMMAND", "TARGET", "ARGV", "RULES", "FAULTS"];

  fn cells(&self) -> Vec<String> {
    vec![
      self.command.clone(),
      self.target.clone(),
      shell_words::join(&self.argv),
      self.rules.iter().map(usize::to_string).collect::<Vec<_>>().join(","),
      self.faults.join("; "),
    ]
  }
}

pub async fn run(args: Args, _ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Check { profile, commands } => check(&profile, &commands),
  }
}

/// Prints the check and returns the exit code.
fn check(path: &Path, commands: &[String]) -> Result<i32> {
  let profile = needy_config::profile::load(&Source::file(path))?;
  let mut findings = validate::profile(&profile);
  println!("user: {} ({}:{})", profile.user.name, profile.user.uid, profile.user.gid);
  println!("executors: {}", profile.executors.keys().cloned().collect::<Vec<_>>().join(", "));
  println!("kernel route names: {}", rules::route_names(&profile).map(|n| n.join(",")).unwrap_or_else(|| "* (every exec)".into()));
  if let Some(reference) = &profile.actor.recipe {
    let recipe = needy_config::recipe::load(&Source::reference(reference, &profile.base_dir))?;
    let compiled = needy_core::recipe::compile(&recipe, &HostInfo::detect()?, 1);
    println!(
      "recipe: {} vCPU, {} MiB, hostname {:?}, uptime offset {} s",
      compiled.vcpus, compiled.mem_mib, compiled.hostname, compiled.uptime_offset_secs
    );
    for (k, v) in &compiled.firecracker_env {
      println!("  {k}={v}");
    }
    findings.extend(compiled.findings.into_iter().map(|f| Finding {
      message: format!("recipe: {}", f.message),
      ..f
    }));
  }
  for finding in &findings {
    println!("{finding}");
  }
  let code = if findings.iter().any(|f| f.severity == Severity::Error) {
    1
  } else {
    0
  };
  if commands.is_empty() {
    return Ok(code);
  }
  let injector = Injector::new(&profile.chaos);
  let mut rows = Vec::new();
  for command in commands {
    let argv = shell_words::split(command).with_context(|| format!("cannot split {command:?}"))?;
    let decision = rules::decide(
      &profile,
      &CommandContext {
        argv: argv.clone(),
        cwd: format!("/home/{}/workspace", profile.user.name),
        env: Default::default(),
        tty: false,
        ancestors: Vec::new(),
      },
    );
    let target = match &decision.target {
      Target::Actor => "actor".to_string(),
      Target::Executor(name) => format!("executor {name}"),
      Target::Deny(message) => format!("deny ({message})"),
    };
    // The daemon checks exec faults against the command as typed.
    let faults = injector.matching(FaultKind::Exec, &argv.join(" ")).into_iter().map(describe).collect();
    rows.push(Decision {
      command: command.clone(),
      target,
      argv: decision.argv,
      rules: decision.matched,
      faults,
    });
  }
  println!();
  output::print_table(&rows);
  Ok(code)
}

/// A fault's effect and when it fires, for example `ECONNREFUSED` or
/// `delay 500ms, nth=2`.
fn describe(fault: &Fault) -> String {
  let mut parts = Vec::new();
  if let Some(errno) = &fault.errno {
    parts.push(errno.clone());
  }
  if let Some(delay) = fault.delay {
    parts.push(format!("delay {}", humantime::format_duration(*delay)));
  }
  match (fault.nth, fault.probability) {
    (Some(nth), _) => parts.push(format!("nth={nth}")),
    (None, Some(p)) => parts.push(format!("p={p}")),
    (None, None) => {}
  }
  parts.join(", ")
}
