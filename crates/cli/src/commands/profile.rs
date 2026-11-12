//! `needy profile check`: the profile's and the recipe's findings, where its
//! actor and executors would run, where each command would run, and the
//! chaos faults that match it.
//!
//! The control plane checks the profile against its runners: its selectors
//! resolve, and its recipe fits the CPU of the host its actor would run on.
//! `--local` checks offline against this machine instead.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CheckProfileRequest};
use needy_config::Source;
use needy_config::profile::{Fault, Profile, service_host};
use needy_config::validate::{self, Finding, Severity};
use needy_core::chaos::{Injector, Subject};
use needy_core::intercept::exec::ExecContext;
use needy_core::recipe::{PerRun, Smbios, VmIdentity};
use needy_core::rules;
use needy_core::types::Outcome;
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
    /// Profile YAML, or a bare NAME for `profiles/NAME.yaml` in the config
    /// directory, as `run create --profile` reads it.
    profile: PathBuf,
    /// A command to decide, as one shell-quoted string; repeat for more.
    #[arg(long = "cmd")]
    commands: Vec<String>,
    /// Check offline against this machine's CPU instead of asking the
    /// control plane; runner selectors are not resolved.
    #[arg(long)]
    local: bool,
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

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Command::Check {
      profile,
      commands,
      local: true,
    } => check_local(&ctx.profile(&profile)?, &commands),
    Command::Check {
      profile,
      commands,
      local: false,
    } => check(&ctx.profile(&profile)?, &commands, ctx).await,
  }
}

/// Asks the control plane where the profile would run and what it finds,
/// prints that and the commands' decisions, and returns the exit code.
async fn check(path: &Path, commands: &[String], ctx: &Context) -> Result<i32> {
  let profile = needy_config::profile::load(&Source::file(path))?;
  // As `run create` sends it: the control plane may run on another machine.
  let resolved = needy_config::profile::resolve(&Source::file(path))?;
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  let request = CheckProfileRequest {
    profile_yaml: resolved.yaml,
    recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
  };
  let check = connection.runs().check_profile(request).await.map_err(needy_api_client::Error::from)?.into_inner();
  println!("user: {} ({}:{})", profile.user.name, profile.user.uid, profile.user.gid);
  let host = |placement: Option<&v1::Placement>| match placement {
    None => "unresolved".to_string(),
    Some(p) if p.runner.is_empty() => "the control host".to_string(),
    Some(p) => format!("runner {}", p.runner),
  };
  let actor = host(check.actor.as_ref());
  if check.actor_cpu.is_empty() {
    println!("actor: {actor}");
  } else {
    println!("actor: {actor} ({})", check.actor_cpu);
  }
  let executors: Vec<String> = profile.executors.keys().map(|name| format!("{name} ({})", host(check.executors.get(name)))).collect();
  println!("executors: {}", executors.join(", "));
  print_route_names(&profile);
  if profile.actor.recipe.is_some() {
    // The control plane checked the recipe against the actor's host; the
    // summary does not depend on the host, so it is compiled here.
    print_recipe(&profile, &Default::default())?;
  }
  let findings: Vec<Finding> = check
    .findings
    .into_iter()
    .map(|f| match f.severity() {
      v1::FindingSeverity::Warning => Finding::warning(f.message),
      // An unspecified severity from a newer control plane counts as an
      // error rather than being dropped.
      _ => Finding::error(f.message),
    })
    .collect();
  finish(&profile, findings, commands)
}

/// Checks the profile against this machine, without a control plane.
fn check_local(path: &Path, commands: &[String]) -> Result<i32> {
  let profile = needy_config::profile::load(&Source::file(path))?;
  let mut findings = validate::profile(&profile);
  println!("user: {} ({}:{})", profile.user.name, profile.user.uid, profile.user.gid);
  println!("executors: {}", profile.executors.keys().cloned().collect::<Vec<_>>().join(", "));
  print_route_names(&profile);
  if profile.actor.recipe.is_some() {
    findings.extend(print_recipe(&profile, &needy_core_actor::driver::linux_firecracker::kvm::detect()?)?);
  }
  finish(&profile, findings, commands)
}

/// The actor's route names, its services and the run's faults.
fn print_route_names(profile: &Profile) {
  println!("route names: {}", rules::route_names(profile).map(|n| n.join(",")).unwrap_or_else(|| "* (every exec)".into()));
  let services: Vec<String> =
    profile.actor.network.services.iter().map(|(name, service)| format!("http://{} -> {}", service_host(name), service.url)).collect();
  if !services.is_empty() {
    println!("services: {}", services.join(", "));
  }
  let faults: Vec<String> =
    profile.chaos.faults.iter().map(|(name, fault)| format!("{name} ({}: {})", fault.domain(), describe(fault))).collect();
  if !faults.is_empty() {
    println!("faults: {}", faults.join("; "));
  }
}

/// Prints the recipe compiled against `host` and returns its findings.
fn print_recipe(profile: &Profile, host: &needy_core::recipe::HostInfo) -> Result<Vec<Finding>> {
  let Some(reference) = &profile.actor.recipe else {
    return Ok(Vec::new());
  };
  let recipe = needy_config::recipe::load(&Source::reference(reference, &profile.base_dir))?;
  // A run compiles the recipe with a seed from its ID and the time it was
  // created, which a check does not have. Compiling with two of each shows
  // which values they decide.
  let compiled = needy_core::recipe::compile(&recipe, host, &PerRun::at(1, 0));
  let other = needy_core::recipe::compile(&recipe, host, &PerRun::at(2, 86_400_000 + 3_661_000));
  let hostname = per_run(compiled.hostname.as_deref(), other.hostname.as_deref()).unwrap_or("not set");
  println!(
    "recipe: {} vCPU, {} MiB, hostname {hostname}, uptime offset {} s",
    compiled.vcpus, compiled.mem_mib, compiled.uptime_offset_secs
  );
  print_identity(&compiled.identity, &other.identity);
  Ok(
    compiled
      .findings
      .into_iter()
      .map(|f| Finding {
        message: format!("recipe: {}", f.message),
        ..f
      })
      .collect(),
  )
}

/// Prints the findings and where each command would run; exits 1 when a
/// finding is an error.
fn finish(profile: &Profile, findings: Vec<Finding>, commands: &[String]) -> Result<i32> {
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
      profile,
      &ExecContext {
        argv: argv.clone(),
        cwd: format!("/home/{}/workspace", profile.user.name),
        env: Default::default(),
        tty: false,
        ancestors: Vec::new(),
      },
    );
    let target = match &decision.outcome {
      Outcome::Run(placement) => placement.to_string(),
      Outcome::Deny(message) => format!("deny ({message})"),
      Outcome::Fault(name) => format!("fault ({name})"),
    };
    // The daemon checks exec faults against the command as typed. Nothing
    // fires here, so a fault that would fire later is still listed.
    let faults = injector.matching(Subject::Exec(&argv)).into_iter().map(|(name, fault)| format!("{name}: {}", describe(&fault))).collect();
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

/// What a check prints for a value of the compiled recipe: the value, or
/// `generated per run` when compiling with another seed (`other`) changes it.
fn per_run<'a>(value: Option<&'a str>, other: Option<&str>) -> Option<&'a str> {
  if value == other {
    value
  } else {
    Some("generated per run")
  }
}

/// The machine a recipe presents through the hypervisor, one line per part;
/// `other` is the same recipe compiled with another seed.
fn print_identity(identity: &VmIdentity, other: &VmIdentity) {
  if let Some(brand) = per_run(identity.cpu_brand.as_deref(), other.cpu_brand.as_deref()) {
    println!("  cpu brand: {brand}");
  }
  if identity.hide_hypervisor {
    println!("  hypervisor: hidden");
  }
  if let Some(smbios) = &identity.smbios {
    let fields = |smbios: &Smbios| {
      [
        ("sys_vendor", smbios.sys_vendor.clone()),
        ("product_name", smbios.product_name.clone()),
        ("product_version", smbios.product_version.clone()),
        ("serial", smbios.serial.clone()),
        ("board_vendor", smbios.board_vendor.clone()),
        ("board_name", smbios.board_name.clone()),
        ("bios_vendor", smbios.bios_vendor.clone()),
        ("bios_version", smbios.bios_version.clone()),
        ("bios_date", smbios.bios_date.clone()),
        ("chassis_type", smbios.chassis_type.map(|n| n.to_string())),
        ("uuid", Some(smbios.uuid.clone())),
      ]
    };
    let others = fields(other.smbios.as_ref().unwrap_or(smbios));
    let text = fields(smbios)
      .iter()
      .zip(others.iter())
      .filter_map(|((k, v), (_, o))| per_run(v.as_deref(), o.as_deref()).map(|v| format!("{k}={v}")))
      .collect::<Vec<_>>()
      .join(", ");
    println!("  smbios: {text}");
  }
  if let Some(id) = identity.pci_host_bridge {
    println!("  pci host bridge: {id}");
  }
  if let Some(id) = identity.pci_isa_bridge {
    println!("  pci isa bridge: {id}");
  }
}

/// A fault's effect and when it fires, for example `exit 7, times=3` or
/// `delay 500ms, nth=2`; with no trigger it fires on every match.
fn describe(fault: &Fault) -> String {
  let duration = |d: needy_config::value::Duration| humantime::format_duration(*d).to_string();
  let mut parts = Vec::new();
  let effects = [
    fault.exit.map(|code| format!("exit {code}")),
    fault.signal.clone(),
    fault.stderr.as_ref().map(|_| "stderr".to_string()),
    fault.hang.then(|| "hang".to_string()),
    fault.errno.clone(),
    fault.refuse.then(|| "refuse".to_string()),
    fault.reset.then(|| "reset".to_string()),
    fault.status.map(|status| format!("status {status}")),
    fault.delay.map(|d| format!("delay {}", duration(d))),
    fault.sensor.as_ref().and_then(|r| r.temp_c).map(|c| format!("temp {c} C")),
  ];
  parts.extend(effects.into_iter().flatten());
  let triggers = [
    fault.probability.map(|p| format!("p={p}")),
    fault.nth.map(|n| format!("nth={n}")),
    fault.times.map(|n| format!("times={n}")),
    fault.after.map(|d| format!("after {}", duration(d))),
    fault.until.map(|d| format!("until {}", duration(d))),
    fault.at.map(|d| format!("at {}", duration(d))),
  ];
  parts.extend(triggers.into_iter().flatten());
  parts.join(", ")
}
