//! `needy run`: create, list, stop runs and read an actor's console.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreateRunRequest, GetRunLogsRequest, ListRunsRequest, Run, StopRunRequest};
use needy_core::types::{ActorState, RunState};
use needy_core_executor::relay;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row, outln};

/// How a run command's `RUN` reads (`crate::runs`).
const RUN_HELP: &str = "The run: an ID, an ID prefix, a name or `latest`. Defaults to NEEDY_RUN, else the only live run";

// TODO(run-ux): a bare `needy run --profile X [-- ARGS]` as `run create` is
// left out: with the arguments as an optional trailing argument, one named
// `ls`, `stop`, `logs` or `create` would parse as a subcommand. Slice 8
// keeps `run create -it` and `run attach`; reopen the shorthand on an
// owner-approved parsing grammar.
#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Create a run from a profile and start its actor.
  Create {
    /// Run the driving program on a terminal, available to `run attach`.
    #[arg(short = 't')]
    tty: bool,
    /// Attach to the driving program immediately and return its exit code.
    /// Needs `-t`.
    // Not clap's `requires`, whose error does not say why; `run` checks it.
    #[arg(short = 'i')]
    interactive: bool,
    /// Keys that leave the run running; empty turns detaching off.
    #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
    detach_keys: String,
    /// Profile YAML; relative paths in it resolve against its directory. A
    /// bare NAME, with no path separator and no `.yaml`, is
    /// `profiles/NAME.yaml` in the config directory (`NEEDY_CONFIG_DIR`).
    #[arg(long)]
    profile: PathBuf,
    /// Runs in one group share executors whose key uses `{group}`.
    /// Defaults to the run ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    group: String,
    /// A name to address the run by instead of its ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
    /// Arguments appended to the profile's driving program
    /// (`actor.command`), as in `-- --continue`.
    // TODO(run-ux): no flag replaces the driving program; a profile that
    // `extends` another sets its own `actor.command`. Reopen on an owner
    // request for a replacement.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },
  /// List runs, including stopped and failed ones, also across control
  /// plane restarts. An executor no runner reported after a restart shows
  /// `(lost)`.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Stop a run and release its executors.
  Stop {
    #[arg(help = RUN_HELP)]
    run: Option<String>,
  },
  /// Attach to the driving program's terminal until it exits or you detach.
  Attach {
    #[arg(help = RUN_HELP)]
    run: Option<String>,
    /// Do not draw the current screen before streaming output.
    #[arg(long)]
    no_replay: bool,
    /// Watch without sending input, signals or window changes.
    #[arg(long)]
    read_only: bool,
    /// Keys that leave the run running; empty turns detaching off.
    #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
    detach_keys: String,
  },
  /// Print the actor's console output: boot messages and the driving program's terminal.
  Logs {
    #[arg(help = RUN_HELP)]
    run: Option<String>,
    /// Keep printing what the actor writes until the run stops running.
    #[arg(short, long)]
    follow: bool,
  },
}

#[derive(Serialize)]
struct RunRow {
  id: String,
  name: String,
  state: String,
  /// Where the run's actor stands: `starting`, `configured`, `ready` or
  /// `exited`; `None` before it starts and for a run that ended.
  actor: Option<String>,
  group: String,
  executors: Vec<ExecutorCell>,
  /// The driving program's exit code, when it exited normally.
  exit_code: Option<i32>,
  /// The signal that ended the driving program.
  signal: Option<String>,
  /// Why the run failed, with its causes.
  failure: Option<String>,
  /// Whether the driving program has a terminal available to `run attach`.
  tty: bool,
}

#[derive(Serialize)]
struct ExecutorCell {
  name: String,
  driver: String,
  /// The run gave it back when it stopped, finished or failed.
  released: bool,
  /// The control plane restarted and no runner reported it: whether it
  /// still runs is unknown.
  lost: bool,
}

/// How many characters of a failure the EXIT column shows; JSON has all of it.
const FAILURE_WIDTH: usize = 48;

impl Row for RunRow {
  const HEADER: &'static [&'static str] = &["ID", "NAME", "STATE", "ACTOR", "GROUP", "EXECUTORS", "EXIT"];

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
      let suffix = match (e.lost, e.released) {
        (true, _) => " (lost)",
        (false, true) => " (released)",
        (false, false) => "",
      };
      format!("{}={}{suffix}", e.name, e.driver)
    });
    // Empty while the driving program runs.
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
      self.actor.clone().unwrap_or_else(|| "-".into()),
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
    let actor = actor_state(s.actor_state).with_context(|| format!("run {}", s.run_id))?;
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
          driver: e.driver,
          released: e.released,
          lost: e.lost,
        })
        .collect(),
      id: s.run_id,
      name: s.name,
      state: state.to_string(),
      actor: actor.map(|a| a.to_string()),
      group: s.group,
      exit_code,
      signal,
      failure: (!s.failure.is_empty()).then_some(s.failure),
      tty: s.tty,
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

/// The actor state a `Run` names: none before the actor starts and for a
/// run that ended; a value this build does not know is an error.
fn actor_state(value: i32) -> Result<Option<ActorState>> {
  match v1::ActorState::try_from(value) {
    Ok(v1::ActorState::Unspecified) => Ok(None),
    Ok(v1::ActorState::Starting) => Ok(Some(ActorState::Starting)),
    Ok(v1::ActorState::Configured) => Ok(Some(ActorState::Configured)),
    Ok(v1::ActorState::Ready) => Ok(Some(ActorState::Ready)),
    Ok(v1::ActorState::Exited) => Ok(Some(ActorState::Exited)),
    Err(_) => bail!("unknown actor state {value}"),
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let endpoint = ctx.endpoint()?;
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::Create {
      tty,
      interactive,
      detach_keys,
      profile,
      group,
      name,
      args,
    } => {
      // A usage error like clap's (exit 2), worded to say why.
      if interactive && !tty {
        output::message("`-i` attaches your terminal to the driving program, which needs one: use `-it`");
        return Ok(2);
      }
      // Refuse invalid keys before creating a run that the caller cannot leave.
      relay::DetachKeys::parse(&detach_keys)?;
      let profile = ctx.profile(&profile)?;
      // TODO(cli-local-workspace): this machine's current directory as the
      // run's workspace needs this machine to join as a runner and the
      // profile to mount from `executor:laptop`; deferred by the owner
      // (2026-11-12), see the candidate in
      // `docs/ai/references/cli/2026-11-09-cli-exec-and-report-design.md`.
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
          actor_args: args,
          tty: tty.then(relay::window),
          wait_for_attach: interactive,
        })
        .await
        .map_err(fail)?
        .into_inner()
        .run
        .unwrap_or_default();
      if interactive {
        // Keep the run ID out of the driving program's terminal output.
        output::message(format!("run {}", run.run_id));
        return attach(&connection, run.run_id, true, false, &detach_keys).await;
      }
      outln!("{}", run.run_id)?;
    }
    Command::Ls { output: format } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let list = connection.runs().list_runs(ListRunsRequest {}).await.map_err(fail)?.into_inner();
      let rows = list.runs.into_iter().map(RunRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Stop { run } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let run = crate::runs::select(&connection, run).await?;
      connection.runs().stop_run(StopRunRequest { run_id: run }).await.map_err(fail)?;
    }
    Command::Attach {
      run,
      no_replay,
      read_only,
      detach_keys,
    } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let run = crate::runs::select(&connection, run).await?;
      return attach(&connection, run, !no_replay, read_only, &detach_keys).await;
    }
    Command::Logs { run, follow } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let run = crate::runs::select(&connection, run).await?;
      let request = GetRunLogsRequest {
        run_id: run,
        follow,
      };
      let mut stream = connection.runs().get_run_logs(request).await.map_err(fail)?.into_inner();
      let mut stdout = tokio::io::stdout();
      while let Some(chunk) = stream.message().await.map_err(fail)? {
        stdout.write_all(&chunk.data).await?;
        // A followed console shows each line as it arrives.
        if follow {
          stdout.flush().await?;
        }
      }
      stdout.flush().await?;
    }
  }
  Ok(0)
}

/// Connects the local terminal to a run through the ordinary process relay.
async fn attach(connection: &needy_api_client::Client, run: String, replay: bool, read_only: bool, detach_keys: &str) -> Result<i32> {
  let request = v1::ExecAttach {
    run_id: run,
    replay,
    read_only,
    ..Default::default()
  };
  super::attach::relay(connection, request, detach_keys).await
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
