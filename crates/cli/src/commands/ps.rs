//! `needy ps`: list processes of a session, or of every executor. Needy's
//! own processes (file transfers and workspace mounts) are hidden unless
//! `--all` asks for them.

use anyhow::Result;
use needy_api_client::proto::v1::{self, ListProcessesRequest, ProcessInfo};
use needy_core::types::Origin;
use serde::Serialize;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  /// Only this session's processes.
  #[arg(long, default_value = "", hide_default_value = true)]
  session: String,
  /// Also list needy's own processes: file transfers and workspace mounts.
  #[arg(short = 'a', long)]
  all: bool,
  #[arg(short = 'o', long, value_enum, default_value_t)]
  output: OutputFormat,
}

#[derive(Serialize)]
struct Process {
  id: String,
  /// Empty for a process outside any session.
  session: String,
  /// `None` when the daemon did not start the process.
  origin: Option<Origin>,
  executor: String,
  pid: i64,
  state: String,
  tty: bool,
  argv: Vec<String>,
}

impl Row for Process {
  const HEADER: &'static [&'static str] = &[
    "ID", "SESSION", "ORIGIN", "EXECUTOR", "PID", "STATE", "TTY", "COMMAND",
  ];

  fn cells(&self) -> Vec<String> {
    vec![
      self.id.clone(),
      or_dash(&self.session),
      self.origin.map_or("-", Origin::as_str).into(),
      self.executor.clone(),
      self.pid.to_string(),
      self.state.clone(),
      if self.tty { "yes" } else { "no" }.into(),
      self.argv.join(" "),
    ]
  }
}

fn or_dash(value: &str) -> String {
  if value.is_empty() {
    "-".into()
  } else {
    value.into()
  }
}

/// The origin a `ProcessInfo` names; unspecified, or a value this build does
/// not know, is unknown.
fn origin(value: i32) -> Option<Origin> {
  match v1::Origin::try_from(value).ok()? {
    v1::Origin::Unspecified => None,
    v1::Origin::Cli => Some(Origin::Cli),
    v1::Origin::Fs => Some(Origin::Fs),
    v1::Origin::Files => Some(Origin::Files),
    v1::Origin::ActorProxy => Some(Origin::ActorProxy),
    v1::Origin::ActorMount => Some(Origin::ActorMount),
  }
}

impl From<ProcessInfo> for Process {
  fn from(p: ProcessInfo) -> Self {
    let state = match (p.running, p.exited) {
      (true, _) => "running".to_string(),
      (false, Some(exit)) => match exit.code {
        Some(code) => format!("exit {code}"),
        None => exit.signal,
      },
      (false, None) => "exited".into(),
    };
    Self {
      id: p.process_id,
      session: p.session_id,
      origin: origin(p.origin),
      executor: p.executor,
      pid: p.pid,
      state,
      tty: p.tty,
      argv: p.argv,
    }
  }
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  let list = connection
    .processes()
    .list_processes(ListProcessesRequest {
      session_id: args.session,
    })
    .await
    .map_err(needy_api_client::Error::from)?
    .into_inner();
  let rows: Vec<Process> =
    list.processes.into_iter().map(Process::from).filter(|p| args.all || !p.origin.is_some_and(Origin::is_internal)).collect();
  output::print(args.output, &rows)?;
  Ok(0)
}
