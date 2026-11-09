//! `needy ps` and `needy kill`.

use anyhow::Result;
use needy_api_proto::v1::{KillProcessRequest, ListProcessesRequest};
use needy_core_executor::client::connect;

pub async fn ps(endpoint: &str, session: String) -> Result<()> {
  let client = connect(endpoint).await?;
  let list = client
    .processes()
    .list_processes(ListProcessesRequest {
      session_id: session,
    })
    .await
    .map_err(|s| anyhow::anyhow!("{}", s.message()))?
    .into_inner();
  println!("{:<13} {:<10} {:>7} {:<10} {:<4} COMMAND", "ID", "EXECUTOR", "PID", "STATE", "TTY");
  for p in list.processes {
    let state = if p.running {
      "running".to_string()
    } else {
      match p.exited {
        Some(e) => match e.code {
          Some(c) => format!("exit {c}"),
          None => e.signal,
        },
        None => "exited".into(),
      }
    };
    println!(
      "{:<13} {:<10} {:>7} {:<10} {:<4} {}",
      p.process_id,
      p.executor,
      p.pid,
      state,
      if p.tty { "yes" } else { "no" },
      p.argv.join(" ")
    );
  }
  Ok(())
}

pub async fn kill(endpoint: &str, process_id: String, signal: String) -> Result<()> {
  let client = connect(endpoint).await?;
  client.processes().kill_process(KillProcessRequest { process_id, signal }).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?;
  Ok(())
}
