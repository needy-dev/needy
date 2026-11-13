//! `needy run chaos`: add, list and remove a live run's chaos faults. A fault
//! takes the fields of a profile's `chaos.faults` entry, as flags or as a
//! YAML file; the control plane checks it as it checks a profile, and
//! records each addition and removal in the run's trace.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, AddFaultRequest, ListFaultsRequest, RemoveFaultRequest};
use needy_config::profile::Fault;
use serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row, outln};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Add a fault to a live run: one domain (--exec, --open, --connect or
  /// --request), its effects and its triggers, or --file. Without a
  /// trigger it fires on every match.
  Add(Box<AddArgs>),
  /// List a run's faults with how often each matched and fired.
  Ls(LsArgs),
  /// Remove a fault from a live run, from its profile or added.
  Rm(RmArgs),
}

/// The run: an ID, an ID prefix, a name or `latest`; the inherited run, else the
/// only live run, when absent.
#[derive(clap::Args)]
struct RunArg {
  #[arg(long)]
  run: Option<String>,
}

#[derive(clap::Args)]
struct AddArgs {
  /// The fault's name, which the trace and the report show.
  name: String,
  #[command(flatten)]
  run: RunArg,
  /// The fault as YAML, written as a `chaos.faults` entry.
  #[arg(long, conflicts_with_all = ["exec", "open", "connect", "request", "exit", "signal", "stderr", "hang", "delay", "errno", "refuse", "reset", "status", "probability", "nth", "times", "after", "until"])]
  file: Option<PathBuf>,
  /// exec: a command pattern, such as "curl **".
  #[arg(long, value_name = "PATTERN")]
  exec: Option<String>,
  /// file: a glob on the path the actor opens, such as "**/*.json".
  #[arg(long, value_name = "GLOB")]
  open: Option<String>,
  /// net: every connection to a matching host.
  #[arg(long, value_name = "HOST")]
  connect: Option<String>,
  /// net: plain-HTTP requests to a matching host, such as a service's.
  #[arg(long, value_name = "HOST")]
  request: Option<String>,
  /// exec: exit with this code without running.
  #[arg(long)]
  exit: Option<i32>,
  /// exec: die of this signal without running, such as SIGSEGV.
  #[arg(long)]
  signal: Option<String>,
  /// exec: write this to stderr first.
  #[arg(long)]
  stderr: Option<String>,
  /// exec, net: never finish.
  #[arg(long)]
  hang: bool,
  /// exec, file, net: wait this long first, such as 2s.
  #[arg(long)]
  delay: Option<String>,
  /// file: fail the open with this errno (ENOENT, EACCES, EPERM, EOPNOTSUPP or EIO).
  #[arg(long)]
  errno: Option<String>,
  /// net: refuse the connection.
  #[arg(long)]
  refuse: bool,
  /// net: close the connection without an answer.
  #[arg(long)]
  reset: bool,
  /// net: answer a plain-HTTP request with this status.
  #[arg(long)]
  status: Option<u16>,
  /// Fire with this probability, from the run's seed.
  #[arg(long)]
  probability: Option<f64>,
  /// Fire only on this match, counted from 1.
  #[arg(long)]
  nth: Option<u64>,
  /// Fire at most this many times.
  #[arg(long)]
  times: Option<u64>,
  /// Match only this long after the run was created, or later.
  #[arg(long)]
  after: Option<String>,
  /// Match only until this long after the run was created.
  #[arg(long)]
  until: Option<String>,
}

#[derive(clap::Args)]
struct LsArgs {
  #[command(flatten)]
  run: RunArg,
  #[arg(short = 'o', long, value_enum, default_value_t)]
  output: OutputFormat,
}

#[derive(clap::Args)]
struct RmArgs {
  name: String,
  #[command(flatten)]
  run: RunArg,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let connection = ctx.connect().await?;
  match args.command {
    Command::Add(args) => {
      let fault_yaml = fault_yaml(&args)?;
      let run = crate::runs::select(&connection, args.run.run, ctx).await?;
      let added = connection
        .runs()
        .add_fault(AddFaultRequest {
          run_id: run,
          name: args.name.clone(),
          fault_yaml,
        })
        .await
        .map_err(needy_api_client::Error::from)?
        .into_inner();
      for warning in &added.warnings {
        output::message(format!("warning: {warning}"));
      }
      outln!("added {} to run {}", args.name, added.run_id)?;
    }
    Command::Ls(args) => {
      let run = crate::runs::select(&connection, args.run.run, ctx).await?;
      let listed =
        connection.runs().list_faults(ListFaultsRequest { run_id: run }).await.map_err(needy_api_client::Error::from)?.into_inner();
      let rows: Vec<Listed> = listed.faults.into_iter().map(Listed::from).collect();
      output::print(args.output, &rows)?;
    }
    Command::Rm(args) => {
      let run = crate::runs::select(&connection, args.run.run, ctx).await?;
      let removed = connection
        .runs()
        .remove_fault(RemoveFaultRequest {
          run_id: run,
          name: args.name.clone(),
        })
        .await
        .map_err(needy_api_client::Error::from)?
        .into_inner();
      outln!("removed {} from run {}", args.name, removed.run_id)?;
    }
  }
  Ok(0)
}

/// The fault `args` describe, as the YAML of a `chaos.faults` entry,
/// checked here as the control plane will check it.
fn fault_yaml(args: &AddArgs) -> Result<String> {
  let text = match &args.file {
    Some(path) => std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?,
    None => {
      let mut fault = Mapping::new();
      let mut set = |key: &str, value: Value| {
        fault.insert(Value::String(key.into()), value);
      };
      let text = |s: &String| Value::String(s.clone());
      let map = |key: &str, value: &String| {
        let mut inner = Mapping::new();
        inner.insert(Value::String(key.into()), Value::String(value.clone()));
        Value::Mapping(inner)
      };
      if let Some(pattern) = &args.exec {
        set("exec", text(pattern));
      }
      if let Some(glob) = &args.open {
        set("file", map("open", glob));
      }
      if let Some(host) = &args.connect {
        set("net", map("connect", host));
      }
      if let Some(host) = &args.request {
        set("net", map("request", host));
      }
      if args.connect.is_some() && args.request.is_some() {
        bail!("a net fault matches --connect or --request, not both");
      }
      for (key, value) in [
        ("signal", &args.signal),
        ("stderr", &args.stderr),
        ("delay", &args.delay),
        ("errno", &args.errno),
        ("after", &args.after),
        ("until", &args.until),
      ] {
        if let Some(value) = value {
          set(key, text(value));
        }
      }
      for (key, value) in [
        ("hang", args.hang),
        ("refuse", args.refuse),
        ("reset", args.reset),
      ] {
        if value {
          set(key, Value::Bool(true));
        }
      }
      if let Some(code) = args.exit {
        set("exit", Value::Number(code.into()));
      }
      if let Some(status) = args.status {
        set("status", Value::Number(status.into()));
      }
      if let Some(p) = args.probability {
        set("probability", Value::Number(p.into()));
      }
      if let Some(n) = args.nth {
        set("nth", Value::Number(n.into()));
      }
      if let Some(n) = args.times {
        set("times", Value::Number(n.into()));
      }
      serde_yaml::to_string(&fault)?
    }
  };
  // The same checks as the control plane's, before the round trip.
  let _: Fault = serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("fault {}: {e}", args.name))?;
  Ok(text)
}

#[derive(Serialize)]
struct Listed {
  name: String,
  domain: String,
  /// `profile` or `added`.
  source: String,
  matched: u64,
  fired: u64,
  /// The fault as a `chaos.faults` entry.
  fault: String,
}

impl From<v1::FaultInfo> for Listed {
  fn from(info: v1::FaultInfo) -> Self {
    let source = match info.source() {
      v1::FaultSource::Profile => "profile",
      v1::FaultSource::Added => "added",
      v1::FaultSource::Unspecified => "-",
    };
    Self {
      source: source.into(),
      name: info.name,
      domain: info.domain,
      matched: info.matched,
      fired: info.fired,
      fault: info.fault_yaml,
    }
  }
}

/// A YAML value on one line in flow style, as a profile may write a fault:
/// `{exec: curl **, exit: 7}`.
fn flow(value: &Value) -> String {
  match value {
    Value::Mapping(map) => {
      let fields: Vec<String> = map.iter().map(|(key, value)| format!("{}: {}", flow(key), flow(value))).collect();
      format!("{{{}}}", fields.join(", "))
    }
    Value::Sequence(items) => format!("[{}]", items.iter().map(flow).collect::<Vec<_>>().join(", ")),
    Value::String(text) => text.clone(),
    Value::Bool(b) => b.to_string(),
    Value::Number(n) => n.to_string(),
    Value::Null => "~".into(),
    Value::Tagged(tagged) => flow(&tagged.value),
  }
}

impl Row for Listed {
  const HEADER: &'static [&'static str] = &["NAME", "DOMAIN", "SOURCE", "MATCHED", "FIRED", "FAULT"];

  fn cells(&self) -> Vec<String> {
    let fault = serde_yaml::from_str(&self.fault).map_or_else(|_| self.fault.trim().to_string(), |value| flow(&value));
    vec![
      self.name.clone(),
      self.domain.clone(),
      self.source.clone(),
      self.matched.to_string(),
      self.fired.to_string(),
      fault,
    ]
  }
}

#[cfg(test)]
#[path = "chaos_test.rs"]
mod tests;
