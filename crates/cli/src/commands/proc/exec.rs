//! `needy exec`: run a command through placement. People get the command's
//! own terminal output; tools get `json` after completion or `ndjson` while
//! it runs, which also carries the run's trace events for the command:
//! the faults chaos injected and the placement decision. Those formats live
//! here, not in the executor relay.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, anyhow};
use base64::Engine;
use clap::ValueEnum;
use needy_api_client::exec::{Event, ExecStream};
use needy_api_client::proto::v1::{ExecEvent, ExecStart, ExecStarted, Origin};
use needy_api_client::{Error, Kind};
use needy_common::process::Exit;
use needy_core_executor::{relay, signals};
use serde_json::{Value, json};

use crate::cli::Context;
use crate::output::{self, outln};

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
pub struct Options {
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
  /// Keys that leave a command on a terminal running, as Docker writes
  /// them (`ctrl-p,ctrl-q`); empty turns detaching off.
  #[arg(long, default_value = relay::DEFAULT_DETACH_KEYS)]
  detach_keys: String,
  #[arg(long)]
  cwd: Option<String>,
  /// Set an environment variable, KEY=VALUE.
  #[arg(short = 'e', long = "env")]
  env: Vec<String>,
  /// Start from an empty environment.
  #[arg(long)]
  clear_env: bool,
  #[arg(short = 'o', long, value_enum, default_value = "human")]
  output: Output,
  #[arg(required = true, last = true, allow_hyphen_values = true)]
  command: Vec<String>,
}

#[derive(clap::Args)]
#[group(id = "process_target", required = true, multiple = false)]
pub struct Args {
  /// Execute on the selected control or standalone executor host.
  #[arg(long, group = "process_target")]
  host: bool,
  /// Execute on one registered runner.
  #[arg(long, group = "process_target")]
  runner: Option<String>,
  /// Execute on a uniquely named pooled executor, outside a run.
  #[arg(long, group = "process_target")]
  executor: Option<String>,
  #[command(flatten)]
  options: Options,
}

#[derive(clap::Args)]
pub struct RunArgs {
  /// Run ID, unique prefix, name or latest; omitted selects the inherited or only live run.
  run: Option<String>,
  /// Override the run's routing with this bound executor.
  #[arg(long)]
  executor: Option<String>,
  #[command(flatten)]
  options: Options,
}

enum Target {
  Run {
    run: Option<String>,
    executor: String,
  },
  Outside {
    runner: String,
    executor: String,
  },
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  execute(
    args.options,
    Target::Outside {
      runner: args.runner.unwrap_or_default(),
      executor: args.executor.unwrap_or_default(),
    },
    ctx,
  )
  .await
}

pub async fn run_in_run(args: RunArgs, ctx: &Context) -> Result<i32> {
  execute(
    args.options,
    Target::Run {
      run: args.run,
      executor: args.executor.unwrap_or_default(),
    },
    ctx,
  )
  .await
}

async fn execute(args: Options, target: Target, ctx: &Context) -> Result<i32> {
  let detach_keys = relay::DetachKeys::parse(&args.detach_keys)?;
  let interactive = relay::interactive();
  let tty = (args.tty || (interactive && !args.no_tty)) && args.output == Output::Human;
  let window = tty.then(relay::window);
  let mut env: HashMap<String, String> =
    args.env.iter().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
  // The terminal's type and color support go with it; the executor replaces
  // a `TERM` it has no terminfo entry for.
  if tty {
    for name in ["TERM", "COLORTERM"] {
      if let Ok(value) = std::env::var(name) {
        env.entry(name.into()).or_insert(value);
      }
    }
  }
  let forward_stdin = !(args.no_stdin || args.detach);
  let selected = crate::connection::resolve(ctx)?;
  let connection = selected.connect().await?;
  let (run, runner, executor) = match target {
    Target::Run { run, executor } => (crate::runs::select(&connection, run, ctx).await?, String::new(), executor),
    Target::Outside { runner, executor } => (String::new(), runner, executor),
  };
  if let Some(execution) = selected.execution((!run.is_empty()).then(|| run.clone()))? {
    env.entry(crate::connection::EXECUTION_CONTEXT.into()).or_insert(execution);
  }
  let start = ExecStart {
    argv: args.command,
    cwd: args.cwd.unwrap_or_default(),
    env,
    clear_env: args.clear_env,
    tty: window,
    detach: args.detach,
    run_id: run,
    executor,
    runner,
    no_stdin: !forward_stdin,
    origin: Origin::Cli.into(),
    // A path named on the command line must exist where it runs.
    search_path_if_missing: false,
  };
  let mut stream = match ExecStream::start(&connection, start).await {
    Ok(stream) => stream,
    Err(error) => return refused(error, args.output, ctx),
  };
  if args.detach {
    return detached(&mut stream, args.output, ctx).await;
  }
  match args.output {
    Output::Human => {
      let options = relay::Options {
        tty,
        stdin: forward_stdin,
        // Detach keys are read from a terminal only; piped input is data.
        detach_keys: detach_keys.filter(|_| tty),
        ..relay::Options::default()
      };
      status(relay::run(stream, options).await, ctx)
    }
    Output::Json | Output::Ndjson => {
      let _forward = relay::Forward::start(stream.input(), forward_stdin, false);
      events(stream, args.output, ctx).await
    }
  }
}

/// The exit code of a relayed command: its own, 0 when the user detached,
/// or 255 with a message when the stream ended before it exited.
pub(crate) fn status(relayed: Result<relay::Ended, relay::Error>, ctx: &Context) -> Result<i32> {
  match relayed {
    Ok(relay::Ended::Exited(exit)) => Ok(exit_code(&exit)),
    Ok(relay::Ended::Detached { process }) => {
      output::after_terminal(format!("detached from process {process}; `needy proc attach {process}` returns to it"));
      Ok(0)
    }
    Err(error) if error.ended() => {
      unknown(&error.to_string(), ctx);
      Ok(signals::ENDED)
    }
    // A second SIGTERM or SIGHUP: the client leaves like a shell's child.
    Err(relay::Error::Terminated(signal)) => Ok(128 + signals::number(signal).unwrap_or(1)),
    Err(error) => Err(error.into()),
  }
}

/// The client's exit code for the command's `exit`. A code the client's
/// exit status cannot carry is a failure, and the real code is printed.
fn exit_code(exit: &Exit) -> i32 {
  if let Some(code) = signals::unrepresentable(exit) {
    output::message(signals::out_of_range_message(code));
  }
  signals::shell_code(exit)
}

/// Exit code and output for a start the daemon refused, like a shell's: 126
/// when a rule or an injected fault refuses the command, 127 when it cannot
/// start. Other errors are errors.
fn refused(error: Error, format: Output, ctx: &Context) -> Result<i32> {
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
  crate::diagnostics::render_in(&error.into(), ctx.diagnostic_format, Some(ctx));
  Ok(code)
}

/// Waits until the detached process starts and prints its ID. A stream that
/// ends first leaves the process's fate unknown: 255 and why it ended.
async fn detached(stream: &mut ExecStream, format: Output, ctx: &Context) -> Result<i32> {
  let reason = loop {
    let event = match stream.next().await {
      Ok(Some(event)) => event,
      Ok(None) => break None,
      Err(Error::Ended { reason }) => break reason,
      Err(error) => return Err(error.into()),
    };
    match event {
      Event::Started(started) => {
        match format {
          Output::Human => outln!("{}", started.process_id)?,
          Output::Json => line(&json!({"process_id": started.process_id, "executor": started.executor}))?,
          Output::Ndjson => line(&started_event(&started))?,
        }
        return Ok(0);
      }
      Event::Trace(event) if format == Output::Ndjson => line(&event_line(&event)?)?,
      _ => {}
    }
  };
  let message = format!("the connection ended before the command started{}", reason.map(|r| format!(": {r}")).unwrap_or_default());
  match format {
    Output::Human => {}
    Output::Json => line(&json!({"error": message}))?,
    Output::Ndjson => line(&json!({"type": "error", "message": message}))?,
  }
  unknown(&message, ctx);
  Ok(signals::ENDED)
}

/// Collects (`json`) or streams (`ndjson`) the process's events until it exits.
async fn events(mut stream: ExecStream, format: Output, ctx: &Context) -> Result<i32> {
  let mut summary: BTreeMap<&str, Value> = BTreeMap::new();
  let mut stdout = Vec::new();
  let mut stderr = Vec::new();
  let mut code = 0;
  // A stream that ends before the process exited has no exit to report.
  let ended = loop {
    let event = match stream.next().await {
      Ok(Some(event)) => event,
      Ok(None) => break Some(Error::Ended { reason: None }),
      Err(error) if error.kind() == Kind::Ended => break Some(error),
      Err(error) => return Err(error.into()),
    };
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
        // The JSON keeps the real code; only the exit status is mapped.
        code = exit_code(&exit);
        break None;
      }
      Event::Trace(event) => {
        if format == Output::Ndjson {
          line(&event_line(&event)?)?;
        }
      }
      // These formats never send `detach`.
      Event::Detached => {}
    }
  };
  drop(stream);
  if let Some(error) = ended {
    let message = error.to_string();
    if format == Output::Ndjson {
      line(&json!({"type": "error", "message": message}))?;
    }
    summary.insert("error", json!(message));
    unknown(&message, ctx);
    code = signals::ENDED;
  }
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
  outln!("{}", serde_json::to_string(value)?)?;
  Ok(())
}

#[cfg(test)]
#[path = "exec_test.rs"]
mod tests;

/// An ended stream provides no evidence that the child completed. Keep the
/// execution stdout schema and shell exit while reporting that uncertainty.
fn unknown(message: &str, ctx: &Context) {
  let error = crate::diagnostics::Error::new("process.outcome_unknown", message)
    .outcome_unknown()
    .check("Inspect the process or run before retrying; the command may still be running or may have completed.");
  crate::diagnostics::render_in(&error.into(), ctx.diagnostic_format, Some(ctx));
}
