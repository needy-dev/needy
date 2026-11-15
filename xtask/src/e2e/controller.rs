//! `--actor-host`: keep the creating client on the invoking machine while
//! the existing harness owns a non-root daemon and actor on an SSH host.
//! Requests and responses share that supervised SSH process's stdio. Both
//! SSH connections originate here; the client needs no SSH server or new keys.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::client::Client;
use super::proc::Output;
use super::{Args, Format, remote};

// Callback cells run serially. Hold the lock across one request/response so
// a worker cannot consume another cell's response if parallelism changes.
static CHANNEL: Mutex<()> = Mutex::new(());

/// Performs one local-client operation through the originating orchestrator.
/// Secrets in fixture contents stay on the pipe and are never printed to logs.
pub fn request(operation: &Value) -> Result<Value> {
  let _guard = CHANNEL.lock().map_err(|_| anyhow::anyhow!("client callback channel is poisoned"))?;
  let mut stdout = std::io::stdout().lock();
  writeln!(stdout, "{}", json!({ "client_callback": operation }))?;
  stdout.flush()?;
  drop(stdout);
  let mut response = String::new();
  ensure!(std::io::stdin().lock().read_line(&mut response)? > 0, "client callback channel closed");
  let response: Value = serde_json::from_str(&response).context("client callback response is not JSON")?;
  if let Some(error) = response["error"].as_str() {
    bail!("creating client: {error}");
  }
  Ok(response["result"].clone())
}

pub fn output(value: Value) -> Result<Output> {
  Ok(Output {
    code: value["code"].as_i64().map(i32::try_from).transpose().context("callback returned an invalid exit code")?,
    stdout: value["stdout"].as_str().context("callback returned no stdout")?.into(),
    stderr: value["stderr"].as_str().context("callback returned no stderr")?.into(),
    timed_out: value["timed_out"].as_bool().context("callback returned no deadline result")?,
  })
}

fn recorded(output: Output) -> Value {
  json!({ "code": output.code, "stdout": output.stdout, "stderr": output.stderr, "timed_out": output.timed_out })
}

/// Runs the remote harness, answering only its creating-client operations.
/// The usual report remains the remote harness's final stdout document.
pub fn run(root: &Path, actor: &str, args: &Args) -> Result<i32> {
  ensure!(!args.clean && args.export.is_none(), "--actor-host runs client journeys; use --remote for cleanup or export");
  let dir = remote::checked_dir(&args.remote_dir)?;
  let needy = match &args.needy {
    Some(binary) => binary.canonicalize().context("cannot locate the local client binary")?,
    None => super::run::build_needy(root)?,
  };
  eprintln!("e2e: preparing an actor host; the creating client and its account remain on this machine");
  if args.actor_user.is_some() {
    ensure!(Path::new(dir).is_absolute(), "--actor-user requires an absolute --remote-dir under the prepared account's home");
  }
  remote::ship(root, actor, dir)?;
  if let Some(user) = &args.actor_user {
    let changed = super::proc::simple(
      "ssh",
      &[
        "-o",
        "BatchMode=yes",
        actor,
        &format!("chown -R {} {}", shell_words::quote(user), shell_words::quote(dir)),
      ],
      std::time::Duration::from_secs(60),
    )?;
    ensure!(changed.ok(), "cannot give actor-host scratch to the prepared account: {}", changed.brief());
  }
  let mut remote_args = vec![
    "e2e".into(),
    "--work-dir".into(),
    "../work".into(),
    "--client-callback".into(),
    "-o".into(),
    "json".into(),
  ];
  let only = if args.only.is_empty() {
    vec!["linux-container.runc/client-execution/at=remote".into()]
  } else {
    args.only.clone()
  };
  // One callback session at a time; actor setup and the normal matrix still
  // live in the remote harness, rather than a second handwritten e2e script.
  remote_args.extend(remote::forwarded(args, &only, &args.skip, 1));
  if args.no_prepare {
    remote_args.push("--no-prepare".into());
  }
  let harness = remote::cargo_xtask(dir, &remote_args);
  let harness = match &args.actor_user {
    Some(user) => format!("runuser -l {} -c {}", shell_words::quote(user), shell_words::quote(&harness)),
    None => harness,
  };
  let mut child = Command::new("ssh")
    .args(["-o", "BatchMode=yes", "-o", "ServerAliveInterval=30", actor])
    .arg(harness)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .context("cannot start the actor-host harness")?;
  let mut input = child.stdin.take().context("actor harness has no input channel")?;
  let stdout = child.stdout.take().context("actor harness has no output channel")?;
  let mut worker = Worker {
    actor,
    needy,
    client: None,
    keep: args.keep,
    scratch: None,
  };
  let mut report = String::new();
  for line in std::io::BufReader::new(stdout).lines() {
    let line = line?;
    if line.starts_with("{\"client_callback\":") {
      let request: Value = serde_json::from_str(&line).context("actor harness callback is not JSON")?;
      let response = match worker.handle(&request["client_callback"]) {
        Ok(result) => json!({ "result": result }),
        Err(error) => json!({ "error": format!("{error:#}") }),
      };
      writeln!(input, "{response}")?;
      input.flush()?;
    } else {
      report.push_str(&line);
      report.push('\n');
    }
  }
  let status = child.wait()?;
  let report: Value = serde_json::from_str(&report).context("actor-host harness printed no final JSON report")?;
  match args.output {
    Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    Format::Human => super::summary::print_human("actor host with local creating client", &report),
  }
  Ok(status.code().unwrap_or(1))
}

struct Worker<'a> {
  actor: &'a str,
  needy: PathBuf,
  client: Option<Client>,
  keep: bool,
  scratch: Option<PathBuf>,
}

impl Worker<'_> {
  fn handle(&mut self, request: &Value) -> Result<Value> {
    match request["op"].as_str().context("callback has no operation")? {
      "setup" => {
        ensure!(self.client.is_none(), "previous creating-client callback is still active");
        let port = request["port"].as_u64().and_then(|p| u16::try_from(p).ok()).context("callback has no valid control port")?;
        // NOTICE: a short /tmp root is required by Unix socket limits, as in
        // CellEnv::new. Canonicalize for macOS's /tmp -> /private/tmp mapping.
        let base = Path::new("/tmp");
        let scratch = (0..1000)
          .map(|n| base.join(format!("ne2ec-{}-{n}", std::process::id())))
          .find(|path| !path.exists())
          .context("cannot find a callback scratch directory")?;
        std::fs::create_dir(&scratch)?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
        let scratch = scratch.canonicalize()?;
        self.scratch = Some(scratch.clone());
        let client = Client::forwarded(&self.needy, self.actor, port, &scratch)?;
        let description = client.description();
        self.client = Some(client);
        Ok(description)
      }
      "invoke" => {
        let client = self.client.as_ref().context("no callback client is active")?;
        let args: Vec<&str> = request["args"]
          .as_array()
          .context("callback has no arguments")?
          .iter()
          .map(|v| v.as_str().context("callback argument is not a string"))
          .collect::<Result<_>>()?;
        Ok(recorded(client.invoke(&args, request["connected"].as_bool().context("callback has no connection selection")?)?))
      }
      "shell" => Ok(recorded(
        self
          .client
          .as_ref()
          .context("no callback client is active")?
          .shell(request["script"].as_str().context("callback has no probe script")?)?,
      )),
      "write" => {
        let path = request["relative"].as_str().context("callback has no fixture path")?;
        ensure!(
          Path::new(path).components().all(|c| matches!(c, std::path::Component::Normal(_))),
          "callback fixture must stay within scratch"
        );
        self
          .client
          .as_ref()
          .context("no callback client is active")?
          .write(path, request["text"].as_str().context("callback has no fixture content")?)?;
        Ok(Value::Null)
      }
      "finish" => {
        let client = self.client.take().context("no callback client is active")?;
        client.finish(self.keep)?;
        drop(client);
        self.scratch = None;
        Ok(Value::Null)
      }
      _ => bail!("unknown creating-client callback operation"),
    }
  }
}

impl Drop for Worker<'_> {
  fn drop(&mut self) {
    if let Some(client) = self.client.take()
      && let Err(error) = client.finish(self.keep)
    {
      eprintln!("e2e: creating-client cleanup incomplete: {error:#}");
    }
    if let Some(scratch) = &self.scratch {
      eprintln!("e2e: creating-client artifacts remain at {}", scratch.display());
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use clap::Parser as _;

  #[test]
  fn actor_host_keeps_the_creating_client_local_and_rejects_other_launch_modes() {
    let crate::Task::E2e(args) = crate::Cli::try_parse_from(["xtask", "e2e", "--actor-host", "actor.example.com"]).unwrap().task else {
      panic!("expected e2e task");
    };
    assert_eq!(args.actor_host.as_deref(), Some("actor.example.com"));
    assert!(args.client_host.is_none());
    assert!(!args.client_callback);
    let crate::Task::E2e(args) = crate::Cli::try_parse_from([
      "xtask",
      "e2e",
      "--actor-host",
      "actor.example.com",
      "--actor-user",
      "user",
      "--remote-dir",
      "/home/user/needy-e2e",
    ])
    .unwrap()
    .task
    else {
      panic!("expected e2e task");
    };
    assert_eq!(args.actor_user.as_deref(), Some("user"));
    assert!(crate::Cli::try_parse_from(["xtask", "e2e", "--actor-user", "user"]).is_err());
    for conflicting in ["--remote", "--client-host"] {
      assert!(
        crate::Cli::try_parse_from([
          "xtask",
          "e2e",
          "--actor-host",
          "actor.example.com",
          conflicting,
          "other.example.com"
        ])
        .is_err()
      );
    }
  }

  #[test]
  fn callback_preserves_real_command_exit_and_separate_output_streams() {
    let actual =
      super::super::proc::simple("sh", &["-c", "printf 'out\\n'; printf 'err\\n' >&2; exit 7"], std::time::Duration::from_secs(5)).unwrap();
    let out = output(recorded(actual)).unwrap();
    assert_eq!(out.code, Some(7));
    assert_eq!(out.stdout, "out\n");
    assert_eq!(out.stderr, "err\n");
    assert!(!out.timed_out);
    let out = output(recorded(Output {
      timed_out: true,
      ..Output::default()
    }))
    .unwrap();
    assert!(out.timed_out);
    assert_eq!(out.code, None);
  }
}
