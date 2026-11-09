//! `needy exec`: run a command through placement. People get the command's
//! own terminal output; tools get `json` after completion or `ndjson` while
//! it runs, which also carries the session's trace events for the command:
//! the faults chaos injected and the placement decision. Those formats live
//! here, not in the executor relay.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use clap::ValueEnum;
use needy_api_client::exec::{Event, ExecStream};
use needy_api_client::proto::v1::{ExecEvent, ExecStart, ExecStarted, Origin};
use needy_api_client::{Error, Kind};
use needy_core_executor::relay;
use serde_json::{Value, json};

use crate::cli::Context;
use crate::output;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
  /// The command's own terminal output.
  Human,
  /// One JSON object after the command exits.
  Json,
  /// One JSON object per event while the command runs.
  Ndjson,
}

#[derive(clap::Args)]
pub struct Args {
  /// Force a terminal.
  #[arg(short = 't', conflicts_with = "no_tty")]
  tty: bool,
  /// Force pipes; stdout and stderr stay separate.
  #[arg(short = 'T')]
  no_tty: bool,
  /// Do not forward stdin.
  #[arg(short = 'n')]
  no_stdin: bool,
  /// Start the process, print its ID and return.
  #[arg(short = 'd')]
  detach: bool,
  #[arg(long)]
  cwd: Option<String>,
  /// Set an environment variable, KEY=VALUE.
  #[arg(short = 'e', long = "env")]
  env: Vec<String>,
  /// Start from an empty environment.
  #[arg(long)]
  clear_env: bool,
  /// Route within this session's rules and executors.
  #[arg(long, default_value = "", hide_default_value = true)]
  session: String,
  /// Run on this executor instead of letting rules decide.
  #[arg(long, default_value = "", hide_default_value = true)]
  executor: String,
  #[arg(long, value_enum, default_value = "human")]
  output: Output,
  #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
  command: Vec<String>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let interactive = relay::interactive();
  let tty = (args.tty || (interactive && !args.no_tty)) && args.output == Output::Human;
  let window = tty.then(relay::window);
  let mut env: HashMap<String, String> =
    args.env.iter().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
  if tty && let Ok(term) = std::env::var("TERM") {
    env.entry("TERM".into()).or_insert(term);
  }
  let forward_stdin = !(args.no_stdin || args.detach);
  let start = ExecStart {
    argv: args.command,
    cwd: args.cwd.unwrap_or_default(),
    env,
    clear_env: args.clear_env,
    tty: window,
    detach: args.detach,
    session_id: args.session,
    executor: args.executor,
    no_stdin: !forward_stdin,
    origin: Origin::Cli.into(),
  };
  let connection = crate::cli::connect(&ctx.endpoint()?).await?;
  let mut stream = match ExecStream::start(&connection, start).await {
    Ok(stream) => stream,
    Err(error) => return refused(error, args.output),
  };
  if args.detach {
    return detached(&mut stream, args.output).await;
  }
  match args.output {
    Output::Human => {
      let exit = relay::run(stream, tty, forward_stdin).await?;
      Ok(needy_core_executor::signals::shell_code(&exit))
    }
    Output::Json | Output::Ndjson => {
      let _forward = relay::Forward::start(stream.input(), forward_stdin, false);
      events(stream, args.output).await
    }
  }
}

/// Exit code and output for a start the daemon refused, like a shell's: 126
/// when a rule or an injected fault refuses the command, 127 when it cannot
/// start. Other errors are errors.
fn refused(error: Error, format: Output) -> Result<i32> {
  let code = match error.kind() {
    Kind::Denied => 126,
    Kind::Failed => 127,
    _ => return Err(error.into()),
  };
  let message = error.to_string();
  match format {
    Output::Human => {}
    Output::Json => line(&json!({"exit_code": code, "signal": "", "error": message}))?,
    Output::Ndjson => {
      // The daemon sends the decision that refused the command with the
      // refusal.
      match error.refusal() {
        Ok(events) => {
          for event in &events {
            line(&event_line(event)?)?;
          }
        }
        Err(e) => tracing::warn!("cannot read the refusal's events: {e}"),
      }
      line(&json!({"type": "error", "message": message}))?;
      line(&json!({"type": "exit", "code": code, "signal": ""}))?;
    }
  }
  output::message(&message);
  Ok(code)
}

/// Waits until the detached process starts and prints its ID.
async fn detached(stream: &mut ExecStream, format: Output) -> Result<i32> {
  while let Some(event) = stream.next().await? {
    match event {
      Event::Started(started) => {
        match format {
          Output::Human => println!("{}", started.process_id),
          Output::Json => line(&json!({"process_id": started.process_id, "executor": started.executor}))?,
          Output::Ndjson => line(&started_event(&started))?,
        }
        return Ok(0);
      }
      Event::Trace(event) if format == Output::Ndjson => line(&event_line(&event)?)?,
      _ => {}
    }
  }
  bail!("the stream ended before the process started")
}

/// Collects (`json`) or streams (`ndjson`) the process's events until it exits.
async fn events(mut stream: ExecStream, format: Output) -> Result<i32> {
  let mut summary: BTreeMap<&str, Value> = BTreeMap::new();
  let mut stdout = Vec::new();
  let mut stderr = Vec::new();
  let mut code = 0;
  while let Some(event) = stream.next().await? {
    match event {
      Event::Started(started) => {
        if format == Output::Ndjson {
          line(&started_event(&started))?;
        }
        summary.insert("process_id", json!(started.process_id));
        summary.insert("executor", json!(started.executor));
      }
      Event::Stdout(bytes) => match format {
        Output::Ndjson => line(&chunk("stdout", &bytes))?,
        _ => stdout.extend_from_slice(&bytes),
      },
      Event::Stderr(bytes) => match format {
        Output::Ndjson => line(&chunk("stderr", &bytes))?,
        _ => stderr.extend_from_slice(&bytes),
      },
      Event::Exited(exit) => {
        if format == Output::Ndjson {
          line(&json!({"type": "exit", "code": exit.code, "signal": exit.signal}))?;
        }
        summary.insert("exit_code", json!(exit.code));
        summary.insert("signal", json!(exit.signal));
        code = needy_core_executor::signals::shell_code(&exit);
        break;
      }
      Event::Trace(event) => {
        if format == Output::Ndjson {
          line(&event_line(&event)?)?;
        }
      }
    }
  }
  drop(stream);
  if format == Output::Json {
    summary.insert("stdout", json!(String::from_utf8_lossy(&stdout)));
    summary.insert("stderr", json!(String::from_utf8_lossy(&stderr)));
    line(&json!(summary))?;
  }
  Ok(code)
}

fn started_event(started: &ExecStarted) -> Value {
  json!({"type": "started", "process_id": started.process_id, "executor": started.executor, "pid": started.pid, "argv": started.argv})
}

/// A trace event as an `ndjson` line: its payload's fields with the event
/// kind as `type`.
fn event_line(event: &ExecEvent) -> Result<Value> {
  let mut payload: serde_json::Map<String, Value> =
    serde_json::from_str(&event.payload).map_err(|e| anyhow!("{} event from the daemon is not a JSON object: {e}", event.kind))?;
  payload.insert("type".into(), json!(event.kind));
  Ok(Value::Object(payload))
}

fn chunk(kind: &str, bytes: &[u8]) -> Value {
  match std::str::from_utf8(bytes) {
    Ok(text) => json!({"type": kind, "data": text}),
    Err(_) => json!({"type": kind, "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes)}),
  }
}

fn line(value: &Value) -> Result<()> {
  println!("{}", serde_json::to_string(value)?);
  Ok(())
}
