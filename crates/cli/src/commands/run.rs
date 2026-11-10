//! `needy run`: create, list, stop runs and read an actor's console.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreateRunRequest, GetRunLogsRequest, ListRunsRequest, Run, StopRunRequest};
use needy_core::types::RunState;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

// TODO(run-ux): a bare `needy run --profile X [-- cmd]` as `run create` is
// left out: with the driving program as an optional trailing argument, a
// program named `ls`, `stop`, `logs` or `create` would parse as a
// subcommand. Revisit with `run -it` and `run attach` (slice 8 of the
// runners-and-runs design) or on an owner decision.
#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Create a run from a profile and start its actor.
  Create {
    /// Profile YAML; relative paths in it resolve against its directory.
    #[arg(long)]
    profile: PathBuf,
    /// Runs in one group share executors whose key uses `{group}`.
    /// Defaults to the run ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    group: String,
    /// A name to address the run by instead of its ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
    /// Create executors only; do not start the actor.
    // TODO(runner): replaced by exec --runner in slice 4 of the runners-and-runs
    // design (`docs/ai/references/architecture/2026-11-10-runner-and-run-design.md`);
    // kept until runners exist, since the Docker executor workflow
    // (`examples/cli`) depends on it.
    #[arg(long)]
    no_actor: bool,
    /// Override the actor's driving program.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
  },
  /// List runs, including stopped and failed ones.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Stop a run and release its executors.
  Stop { run: String },
  /// Print the actor's console output: boot messages and the driving program's terminal.
  Logs { run: String },
}

#[derive(Serialize)]
struct RunRow {
  id: String,
  name: String,
  state: String,
  group: String,
  executors: Vec<ExecutorCell>,
  /// The driving program's exit code, when it exited normally.
  exit_code: Option<i32>,
  /// The signal that ended the driving program.
  signal: Option<String>,
  /// Why the run failed, with its causes.
  failure: Option<String>,
}

#[derive(Serialize)]
struct ExecutorCell {
  name: String,
  runtime: String,
  /// The run gave it back when it stopped, finished or failed.
  released: bool,
}

/// How many characters of a failure the EXIT column shows; JSON has all of it.
const FAILURE_WIDTH: usize = 48;

impl Row for RunRow {
  const HEADER: &'static [&'static str] = &["ID", "NAME", "STATE", "GROUP", "EXECUTORS", "EXIT"];

  fn cells(&self) -> Vec<String> {
    // A name or group that was not given defaults to the ID; repeating
    // it says nothing.
    let given = |value: &str| {
      if value == self.id {
        "-".to_string()
      } else {
        value.to_string()
      }
    };
    let executors = self.executors.iter().map(|e| {
      let suffix = if e.released { " (released)" } else { "" };
      format!("{}={}{suffix}", e.name, e.runtime)
    });
    // Empty while the driving program runs, and for a run without an actor.
    let exit = match (&self.exit_code, &self.signal, &self.failure) {
      (Some(code), _, _) => code.to_string(),
      (None, Some(signal), _) => signal.clone(),
      (None, None, Some(failure)) => truncate(failure.lines().next().unwrap_or_default(), FAILURE_WIDTH),
      (None, None, None) => String::new(),
    };
    vec![
      self.id.clone(),
      given(&self.name),
      self.state.clone(),
      given(&self.group),
      executors.collect::<Vec<_>>().join(", "),
      exit,
    ]
  }
}

/// `text` cut to `width` characters, the last of them `…` when cut.
fn truncate(text: &str, width: usize) -> String {
  if text.chars().count() <= width {
    return text.to_string();
  }
  let mut cut: String = text.chars().take(width - 1).collect();
  cut.push('…');
  cut
}

impl TryFrom<Run> for RunRow {
  type Error = anyhow::Error;

  fn try_from(s: Run) -> Result<Self> {
    let state = state(s.state).with_context(|| format!("run {}", s.run_id))?;
    let (exit_code, signal) = match s.actor_exit {
      Some(exit) => (exit.code, (!exit.signal.is_empty()).then_some(exit.signal)),
      None => (None, None),
    };
    Ok(Self {
      executors: s
        .executors
        .into_iter()
        .map(|e| ExecutorCell {
          name: e.name,
          runtime: e.runtime,
          released: e.released,
        })
        .collect(),
      id: s.run_id,
      name: s.name,
      state: state.to_string(),
      group: s.group,
      exit_code,
      signal,
      failure: (!s.failure.is_empty()).then_some(s.failure),
    })
  }
}

/// The state a `Run` names. Its zero value means the daemon left it
/// unset, and a value this build does not know comes from a newer daemon;
/// both are errors instead of a default.
fn state(value: i32) -> Result<RunState> {
  match v1::RunState::try_from(value) {
    Ok(v1::RunState::Unspecified) => bail!("the run state is unspecified"),
    Ok(v1::RunState::Creating) => Ok(RunState::Creating),
    Ok(v1::RunState::Running) => Ok(RunState::Running),
    Ok(v1::RunState::Stopping) => Ok(RunState::Stopping),
    Ok(v1::RunState::Stopped) => Ok(RunState::Stopped),
    Ok(v1::RunState::Finished) => Ok(RunState::Finished),
    Ok(v1::RunState::Failed) => Ok(RunState::Failed),
    Err(_) => bail!("unknown run state {value}"),
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let endpoint = ctx.endpoint()?;
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::Create {
      profile,
      group,
      name,
      no_actor,
      command,
    } => {
      let path = std::fs::canonicalize(&profile).with_context(|| format!("cannot read profile {}", profile.display()))?;
      // The daemon may run on another machine: it gets the profile and its
      // recipe as text, not paths to read.
      let resolved = needy_config::profile::resolve(&needy_config::Source::file(&path))?;
      let connection = crate::cli::connect(&endpoint).await?;
      let run = connection
        .runs()
        .create_run(CreateRunRequest {
          profile_yaml: resolved.yaml,
          recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
          profile_name: needy_config::profile::name(&path),
          group,
          name,
          start_actor: !no_actor,
          actor_command: command,
        })
        .await
        .map_err(fail)?
        .into_inner()
        .run
        .unwrap_or_default();
      println!("{}", run.run_id);
    }
    Command::Ls { output: format } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let list = connection.runs().list_runs(ListRunsRequest {}).await.map_err(fail)?.into_inner();
      let rows = list.runs.into_iter().map(RunRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Stop { run } => {
      let connection = crate::cli::connect(&endpoint).await?;
      connection.runs().stop_run(StopRunRequest { run_id: run }).await.map_err(fail)?;
    }
    Command::Logs { run } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let mut stream = connection.runs().get_run_logs(GetRunLogsRequest { run_id: run }).await.map_err(fail)?.into_inner();
      let mut stdout = tokio::io::stdout();
      while let Some(chunk) = stream.message().await.map_err(fail)? {
        stdout.write_all(&chunk.data).await?;
      }
      stdout.flush().await?;
    }
  }
  Ok(0)
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
