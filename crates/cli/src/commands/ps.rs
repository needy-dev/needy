//! `needy ps`: list processes of a session, or of every executor.

use anyhow::Result;
use needy_api_client::proto::v1::{ListProcessesRequest, ProcessInfo};
use serde::Serialize;

use crate::cli::Context;
use crate::output::{self, OutputFormat, Row};

#[derive(clap::Args)]
pub struct Args {
  /// Only this session's processes.
  #[arg(long, default_value = "", hide_default_value = true)]
  session: String,
  #[arg(long, value_enum, default_value_t)]
  output: OutputFormat,
}

#[derive(Serialize)]
struct Process {
  id: String,
  executor: String,
  pid: i64,
  state: String,
  tty: bool,
  argv: Vec<String>,
}

impl Row for Process {
  const HEADER: &'static [&'static str] = &["ID", "EXECUTOR", "PID", "STATE", "TTY", "COMMAND"];

  fn cells(&self) -> Vec<String> {
    vec![
      self.id.clone(),
      self.executor.clone(),
      self.pid.to_string(),
      self.state.clone(),
      if self.tty { "yes" } else { "no" }.into(),
      self.argv.join(" "),
    ]
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
  let rows: Vec<Process> = list.processes.into_iter().map(Process::from).collect();
  output::print(args.output, &rows)?;
  Ok(0)
}
