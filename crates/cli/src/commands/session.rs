//! `needy session`: create, list, stop sessions and read an actor's console.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use needy_api_client::proto::v1::{self, CreateSessionRequest, GetSessionLogsRequest, ListSessionsRequest, Session, StopSessionRequest};
use needy_core::types::SessionState;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Create a session from a profile and start its actor.
  Create {
    /// Profile YAML; relative paths in it resolve against its directory.
    #[arg(long)]
    profile: PathBuf,
    /// Sessions in one group share executors whose key uses `{group}`.
    /// Defaults to the session ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    group: String,
    /// A name to address the session by instead of its ID.
    #[arg(long, default_value = "", hide_default_value = true)]
    name: String,
    /// Create executors only; do not start the actor.
    #[arg(long)]
    no_actor: bool,
    /// Override the actor's driving program.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
  },
  /// List sessions, including stopped and failed ones.
  Ls {
    #[arg(short = 'o', long, value_enum, default_value_t)]
    output: OutputFormat,
  },
  /// Stop a session and release its executors.
  Stop { session: String },
  /// Print the actor's console output: boot messages and the driving program's terminal.
  Logs { session: String },
}

#[derive(Serialize)]
struct SessionRow {
  id: String,
  name: String,
  state: String,
  group: String,
  executors: Vec<ExecutorCell>,
  /// The driving program's exit code, when it exited normally.
  exit_code: Option<i32>,
  /// The signal that ended the driving program.
  signal: Option<String>,
  /// Why the session failed, with its causes.
  failure: Option<String>,
}

#[derive(Serialize)]
struct ExecutorCell {
  name: String,
  runtime: String,
  /// The session gave it back when it stopped, finished or failed.
  released: bool,
}

/// How many characters of a failure the EXIT column shows; JSON has all of it.
const FAILURE_WIDTH: usize = 48;

impl Row for SessionRow {
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
    // Empty while the session runs, and for a session without an actor.
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

impl TryFrom<Session> for SessionRow {
  type Error = anyhow::Error;

  fn try_from(s: Session) -> Result<Self> {
    let state = state(s.state).with_context(|| format!("session {}", s.session_id))?;
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
      id: s.session_id,
      name: s.name,
      state: state.to_string(),
      group: s.group,
      exit_code,
      signal,
      failure: (!s.failure.is_empty()).then_some(s.failure),
    })
  }
}

/// The state a `Session` names. Its zero value means the daemon left it
/// unset, and a value this build does not know comes from a newer daemon;
/// both are errors instead of a default.
fn state(value: i32) -> Result<SessionState> {
  match v1::SessionState::try_from(value) {
    Ok(v1::SessionState::Unspecified) => bail!("the session state is unspecified"),
    Ok(v1::SessionState::Creating) => Ok(SessionState::Creating),
    Ok(v1::SessionState::Running) => Ok(SessionState::Running),
    Ok(v1::SessionState::Stopping) => Ok(SessionState::Stopping),
    Ok(v1::SessionState::Stopped) => Ok(SessionState::Stopped),
    Ok(v1::SessionState::Finished) => Ok(SessionState::Finished),
    Ok(v1::SessionState::Failed) => Ok(SessionState::Failed),
    Err(_) => bail!("unknown session state {value}"),
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
      let session = connection
        .sessions()
        .create_session(CreateSessionRequest {
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
        .session
        .unwrap_or_default();
      println!("{}", session.session_id);
    }
    Command::Ls { output: format } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let list = connection.sessions().list_sessions(ListSessionsRequest {}).await.map_err(fail)?.into_inner();
      let rows = list.sessions.into_iter().map(SessionRow::try_from).collect::<Result<Vec<_>>>()?;
      output::print(format, &rows)?;
    }
    Command::Stop { session } => {
      let connection = crate::cli::connect(&endpoint).await?;
      connection
        .sessions()
        .stop_session(StopSessionRequest {
          session_id: session,
        })
        .await
        .map_err(fail)?;
    }
    Command::Logs { session } => {
      let connection = crate::cli::connect(&endpoint).await?;
      let mut stream = connection
        .sessions()
        .get_session_logs(GetSessionLogsRequest {
          session_id: session,
        })
        .await
        .map_err(fail)?
        .into_inner();
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
#[path = "session_test.rs"]
mod tests;
