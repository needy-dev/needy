//! `needy run`: create, list, stop runs and read an actor's console.

mod chaos;
mod execution;
mod report;
mod snapshot;

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreateRunRequest, GetRunLogsRequest, ListRunsRequest, Run, StopRunRequest};
use needy_core::types::{ActorState, RunState};
use needy_core_executor::relay;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::cli::{ConnectionArgs, Context};
use crate::output::{self, OutputFormat, Row, outln};

/// How a run command's `RUN` reads (`crate::runs`).
const RUN_HELP: &str = "The run: an ID, an ID prefix, a name or `latest`. Defaults to the inherited run, else the only live run";

// TODO(run-ux): a bare `needy run --profile X [-- ARGS]` as `run create` is
// left out: with the arguments as an optional trailing argument, one named
// `ls`, `stop`, `logs` or `create` would parse as a subcommand. Slice 8
// keeps `run create -it` and `run attach`; reopen the shorthand on an
// owner-approved parsing grammar.
#[derive(clap::Args)]
pub struct Args {
  #[command(flatten)]
  pub(crate) connection: ConnectionArgs,
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Internal same-account process owner for one client-bound invocation.
  #[command(hide = true)]
  ExecutionHolder {
    #[arg(long)]
    startup_stdin: bool,
    #[arg(long)]
    directory: PathBuf,
  },
  /// Execute through a run's routing and bound executors.
  Exec(super::proc::exec::RunArgs),
  /// Render a run report online or from this machine's trace store.
  Report(report::Args),
  /// Manage a run's live faults.
  Chaos(chaos::Args),
  /// Create a run from a profile and start its actor.
  Create {
    /// Run the driving program on a terminal, available to `run attach`.
    #[arg(short = 't')]
    tty: bool,
    /// Attach to the driving program immediately and return its exit code (requires `-t`).
    // Not clap's `requires`, whose error does not say why; `run` checks it.
    #[arg(short = 'i')]
    interactive: bool,
    /// Keys that detach from the terminal while leaving the run running; empty disables detaching.
    #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
    detach_keys: String,
    /// Profile path, bare profile name, or `builtin:NAME`.
    ///
    /// Relative paths inside the profile resolve against its directory. A bare
    /// NAME (no path separator and no `.yaml`) resolves to `profiles/NAME.yaml`
    /// in the config directory (`NEEDY_CONFIG_DIR`).
    #[arg(short = 'f', long)]
    profile: PathBuf,
    #[command(flatten)]
    overrides: snapshot::Overrides,
    /// Group name for sharing executors whose key uses `{group}` (defaults to the run ID).
    #[arg(long, default_value = "", hide_default_value = true)]
    group: String,
    /// Human-friendly name to address the run by instead of its ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
    /// Wait up to this duration for required actor or executor runners to connect before failing.
    #[arg(long, value_name = "DURATION", value_parser = humantime::parse_duration)]
    wait_for_runner: Option<std::time::Duration>,
    /// Arguments appended to the profile's driving program (`actor.command`), as in `-- --continue`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },
  /// List runs, including stopped and failed ones.
  ///
  /// Includes runs preserved across control-plane restarts. An executor that no
  /// runner reported after a restart is marked `(lost)`.
  Ls {
    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Stop a run and release its executors.
  ///
  /// A running driving program receives SIGTERM, then SIGKILL after the grace
  /// period, and the actor's mirror-out runs before the actor ends.
  Stop {
    #[arg(help = RUN_HELP)]
    run: Option<String>,
    /// Grace period between SIGTERM and SIGKILL for the driving program (default `10s`).
    #[arg(long, value_name = "DURATION", value_parser = humantime::parse_duration, conflicts_with = "now")]
    grace: Option<std::time::Duration>,
    /// Kill the actor immediately without running mirror-out.
    #[arg(long)]
    now: bool,
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
  let fail = needy_api_client::Error::from;
  match args.command {
    Command::ExecutionHolder {
      startup_stdin,
      directory,
    } => return execution::serve(startup_stdin, &directory).await,
    Command::Exec(args) => return super::proc::exec::run_in_run(args, ctx).await,
    Command::Report(args) => return report::run(args, ctx).await,
    Command::Chaos(args) => return chaos::run(args, ctx).await,
    Command::Create {
      tty,
      interactive,
      detach_keys,
      profile,
      overrides,
      group,
      name,
      wait_for_runner,
      args,
    } => {
      // A usage error like clap's (exit 2), worded to say why.
      if interactive && !tty {
        let error = crate::diagnostics::Error::new(
          "cli.invalid_arguments",
          "`-i` attaches your terminal to the driving program, which needs one: use `-it`",
        )
        .check("Pass `-it` to allocate a terminal and attach to the driving program.");
        crate::diagnostics::render_in(&error.into(), ctx.diagnostic_format, Some(ctx));
        return Ok(2);
      }
      // Refuse invalid keys before creating a run that the caller cannot leave.
      relay::DetachKeys::parse(&detach_keys)?;
      let (source, profile_name, inline_lockfile) = match ctx.profile_source(&profile)? {
        needy_config::Source::Builtin(name) => {
          let lockfile = super::image::lockfile::builtin_lockfile_path(ctx, &name)?;
          (needy_config::Source::builtin(&name), format!("builtin:{name}"), lockfile)
        }
        needy_config::Source::File(profile_path) => {
          let path = std::fs::canonicalize(&profile_path).with_context(|| format!("cannot read profile {}", profile_path.display()))?;
          let name = needy_config::profile::name(&path);
          let lockfile = super::image::lockfile::path_for(&path);
          (needy_config::Source::file(path), name, lockfile)
        }
        other => (other, String::new(), PathBuf::new()),
      };
      // The daemon may run on another machine: it gets the profile and its
      // recipe as text, not paths to read.
      let resolved = snapshot::resolve(&source, overrides)?;
      let progress = output::Progress::new(ctx.connection_feedback(), "Connecting to control service");
      let selection = crate::connection::resolve(ctx)?;
      let connection = selection.connect().await?;
      let loaded = needy_config::profile::load(&needy_config::Source::text(resolved.yaml.clone(), PathBuf::new()))?;
      let lockfile_path = if matches!(loaded.actor.image, Some(needy_config::image::Image::Definition(_))) {
        match &resolved.image_file {
          Some(file) => {
            let canonical = std::fs::canonicalize(file).unwrap_or_else(|_| file.clone());
            Some(super::image::lockfile::path_for(&canonical))
          }
          None => Some(inline_lockfile),
        }
      } else {
        None
      };
      let mut lock = match &lockfile_path {
        Some(lock_path) => super::image::lockfile::read(lock_path)?,
        None => Default::default(),
      };
      let image_commit = lock.commit.as_ref().map(super::image::lockfile::LockCommit::to_proto).transpose()?;
      let pinned_features = lock.features.clone().into_iter().collect();
      if selection.context.as_deref() == Some("local") && loaded.actor.runner.is_none() {
        super::image::pull::ensure_vm_base(loaded.actor.driver, ctx).await;
      }
      let needs_client = loaded.executors.values().any(|e| matches!(e.runner, Some(needy_config::profile::RunnerSelector::Client)));
      let mut holder = if needs_client {
        progress.message("Preparing client execution");
        Some(execution::launch(ctx, &selection, &connection).await?)
      } else {
        None
      };
      // Once a create can reach control, its outcome can be unknown. Control
      // owns the bound holder's lifetime; an unused capability expires there.
      if let Some(holder) = &mut holder {
        holder.keep();
      }
      progress.message("Creating run");
      let created = connection
        .runs()
        .create_run(CreateRunRequest {
          profile_yaml: resolved.yaml,
          recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
          profile_name,
          group,
          name,
          actor_args: args,
          tty: tty.then(relay::window),
          wait_for_attach: interactive,
          client_execution: holder.as_ref().map(|h| h.capability.clone()).unwrap_or_default(),
          client_dir: client_dir()?,
          image_commit,
          pinned_features,
          wait_for_runner_ms: wait_for_runner.map(|d| d.as_millis() as u64).unwrap_or(0),
        })
        .await
        .map_err(fail)?
        .into_inner();
      let run = created.run.unwrap_or_default();
      progress.finish();
      if let Some(lock_path) = &lockfile_path {
        let resolved_features: std::collections::BTreeMap<String, String> = run.resolved_features.into_iter().collect();
        if !resolved_features.is_empty() && resolved_features != lock.features {
          lock.features = resolved_features;
          super::image::lockfile::write(lock_path, &lock)?;
        }
      }
      if run.commit_rebased {
        let lock_display = lockfile_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "image lockfile".into());
        output::message(format_args!("warning: commit layer in {lock_display} was applied on a changed base"));
      }
      if let Some(workspace) = &run.workspace {
        output::message(workspace_line(workspace, &loaded));
      }
      if interactive {
        // Keep the run ID out of the driving program's terminal output.
        output::message(format!("run {}", run.run_id));
        return attach(&connection, run.run_id, true, false, &detach_keys, ctx).await;
      }
      outln!("{}", run.run_id)?;
    }
    Command::Ls { output: format } => {
      let progress = output::Progress::new(matches!(format, OutputFormat::Table) && ctx.connection_feedback(), "Fetching runs...");
      let connection = ctx.connect().await?;
      let list = connection.runs().list_runs(ListRunsRequest {}).await.map_err(fail)?.into_inner();
      progress.finish();
      let rows = list.runs.into_iter().map(RunRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Stop { run, grace, now } => {
      let progress = output::Progress::new(ctx.connection_feedback(), "Connecting to control service");
      let connection = ctx.connect().await?;
      let run = crate::runs::select(&connection, run, ctx).await?;
      progress.message(format!("Stopping run {run}"));
      let request = StopRunRequest {
        run_id: run.clone(),
        now,
        grace_ms: grace.map(|g| i64::try_from(g.as_millis()).unwrap_or(i64::MAX)),
      };
      let stopped = connection.runs().stop_run(request).await.map_err(fail)?.into_inner();
      progress.finish();
      // An actor killed without being asked to (`--now`) lost its
      // mirror-out; say so where the user looks.
      if stopped.graceful == Some(false) && !now {
        output::message(format_args!("run {run}: the actor was killed without its mirror-out: {}", stopped.reason));
      }
      output::acknowledge(&format!("Stopped run {run}."))?;
    }
    Command::Attach {
      run,
      no_replay,
      read_only,
      detach_keys,
    } => {
      let progress = output::Progress::new(ctx.connection_feedback(), "Connecting for run attachment...");
      let connection = ctx.connect().await?;
      let run = crate::runs::select(&connection, run, ctx).await?;
      progress.finish();
      return attach(&connection, run, !no_replay, read_only, &detach_keys, ctx).await;
    }
    Command::Logs { run, follow } => {
      let progress = output::Progress::new(ctx.connection_feedback(), "Fetching run logs");
      let connection = ctx.connect().await?;
      let run = crate::runs::select(&connection, run, ctx).await?;
      let request = GetRunLogsRequest {
        run_id: run,
        follow,
      };
      let mut stream = connection.runs().get_run_logs(request).await.map_err(fail)?.into_inner();
      progress.finish();
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

/// The folder `run create` runs in, which the control plane makes the
/// workspace of a profile that names none.
fn client_dir() -> Result<String> {
  let dir = std::env::current_dir().context("cannot read the current folder")?;
  match dir.to_str() {
    Some(dir) => Ok(dir.to_string()),
    None => bail!("the current folder {} is not UTF-8, which a workspace must be; set actor.workspace", dir.display()),
  }
}

/// Where a run's workspace is, in one line: its directory in the actor and
/// the machine and folder its files come from.
fn workspace_line(workspace: &v1::RunWorkspace, profile: &needy_config::profile::Profile) -> String {
  use needy_config::profile::RunnerSelector;
  let from = if !workspace.executor.is_empty() {
    let machine = match profile.executors.get(&workspace.executor).and_then(|e| e.runner.as_ref()) {
      Some(RunnerSelector::Client) => "this machine".to_string(),
      Some(runner) => runner.to_string(),
      None => "the control host".to_string(),
    };
    format!("live from {} on {machine} (executor {})", workspace.source, workspace.executor)
  } else if !workspace.source.is_empty() {
    format!("copied from {} on the control host", workspace.source)
  } else {
    "a fresh directory in the actor".to_string()
  };
  let access = if workspace.writable {
    ""
  } else {
    ", read-only"
  };
  format!("workspace {}: {from}{access}", workspace.path)
}

/// Connects the local terminal to a run through the ordinary process relay.
async fn attach(
  connection: &needy_api_client::Client,
  run: String,
  replay: bool,
  read_only: bool,
  detach_keys: &str,
  ctx: &Context,
) -> Result<i32> {
  let request = v1::ExecAttach {
    run_id: run,
    replay,
    read_only,
    ..Default::default()
  };
  super::proc::attach::relay(connection, request, detach_keys, ctx).await
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
