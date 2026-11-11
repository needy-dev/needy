//! A native actor's shim: every program on the driving program's `PATH` is
//! a link to this binary in the actor's shims directory, and runs inside the
//! fence. A command on the run's route list goes to the daemon, as a
//! container's shim hands it to the proxy; the daemon routes it to an
//! executor or keeps it. A kept command, and any command not on the list,
//! goes to init's kept-command service, which starts the real program under
//! the fence. Either way the shim relays the command's input, output and
//! exit, so its parent sees its child exit as the command did.
//!
//! What the shim writes to stderr is the command's own error output, so it
//! installs no log subscriber.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use needy_api_client::exec::ExecStream;
use needy_api_client::{Client, Kind};
use needy_api_proto::v1::{DecideRequest, ExecStart, Origin};
use needy_core_executor::{relay, signals};
use nix::sys::signal::{SigHandler, Signal};

use super::Route;
use crate::host::macos::Layout;

/// Environment the actor passes to routed commands when rules do not, as
/// the guest's proxy does.
const PASS_ENV: &[&str] = &[
  "TERM",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "COLORTERM",
  "NO_COLOR",
];

/// The shim at `exe` in the actor `layout`; never returns.
pub(super) fn main(exe: &Path, layout: Layout) -> ! {
  let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
  let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
  // NOTICE: the runtime is not dropped: dropping it waits for its blocking
  // threads, and the relay's stdin reader blocks in `read` until the next
  // keystroke, so a command on a terminal would end only at the user's next
  // input (seen on macOS 27.2: the second command of `perl …; uname` hung).
  // `exit` ends the process with the reader in it.
  let runtime = super::runtime();
  let code = runtime.block_on(run(&layout, &name, args)).unwrap_or_else(|e| {
    message(format_args!("{e:#}"));
    126
  });
  std::process::exit(code)
}

fn message(message: impl std::fmt::Display) {
  eprintln!("needy: {message}");
}

async fn run(layout: &Layout, name: &str, args: Vec<String>) -> Result<i32> {
  let route = std::fs::read_to_string(layout.route()).map(|text| Route::parse(&text)).unwrap_or(Route { names: Vec::new() });
  // The command as the user typed it: the shim's name, which an executor
  // finds on its own PATH; the actor's shim path means nothing there.
  let mut argv = args.clone();
  match argv.first_mut() {
    Some(first) => *first = name.to_string(),
    None => argv.push(name.to_string()),
  }
  let argv0 = args.first().cloned().unwrap_or_default();
  let mut names = vec![name.to_string(), argv0.clone()];
  names.extend(Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()));
  let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
  let env: HashMap<String, String> = std::env::vars().collect();
  let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
  if route.routes(&names) {
    let client = connect(&layout.guest()).await?;
    // TODO(macos-native): the process ancestry (`when.under`) is not sent;
    // reading it on macOS needs `sysctl(KERN_PROCARGS2)` per ancestor, which
    // waits for the productize decision.
    let decision = client
      .runs()
      .decide(DecideRequest {
        run_id: String::new(),
        argv: argv.clone(),
        cwd: cwd.clone(),
        env: env.clone(),
        tty,
        ancestors: Vec::new(),
      })
      .await
      .map_err(needy_api_client::Error::from)?
      .into_inner();
    if !decision.deny.is_empty() {
      message(&decision.deny);
      return Ok(126);
    }
    if decision.target != "actor" && !decision.target.is_empty() {
      let mut pass: HashMap<String, String> = PASS_ENV.iter().filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
      pass.extend(decision.env);
      let start = ExecStart {
        argv: if decision.argv.is_empty() {
          argv
        } else {
          decision.argv
        },
        cwd: if decision.cwd.is_empty() {
          cwd
        } else {
          decision.cwd
        },
        env: pass,
        clear_env: decision.clear_env,
        tty: tty.then(relay::window),
        detach: false,
        run_id: String::new(),
        executor: decision.target,
        runner: String::new(),
        no_stdin: false,
        origin: Origin::ActorProxy.into(),
        search_path_if_missing: decision.search_path_if_missing,
      };
      return relay_to(&client, start, tty).await;
    }
  }
  // Kept: init starts the real program under the fence, with this
  // command's environment.
  let client = connect(&layout.kept()).await?;
  let start = ExecStart {
    argv,
    cwd,
    env,
    clear_env: true,
    tty: tty.then(relay::window),
    detach: false,
    run_id: String::new(),
    executor: String::new(),
    runner: String::new(),
    no_stdin: false,
    origin: Origin::ActorProxy.into(),
    search_path_if_missing: false,
  };
  relay_to(&client, start, tty).await
}

async fn connect(socket: &Path) -> Result<Client> {
  let endpoint = format!("unix:{}", socket.display());
  Client::connect(&endpoint).await.with_context(|| format!("cannot reach {endpoint}"))
}

/// Starts `start` on `client` and relays it; returns the exit code the
/// shim exits with, after dying of the command's signal where it died of
/// one, as the guest's proxy does.
async fn relay_to(client: &Client, start: ExecStart, tty: bool) -> Result<i32> {
  let stream = match ExecStream::start(client, start).await {
    Ok(stream) => stream,
    Err(error) => match error.kind() {
      Kind::Denied => {
        message(&error);
        return Ok(126);
      }
      Kind::Failed | Kind::NotFound => {
        message(&error);
        return Ok(127);
      }
      _ => return Err(error.into()),
    },
  };
  let options = relay::Options {
    tty,
    stdin: true,
    ..Default::default()
  };
  let exit = match relay::run(stream, options).await {
    Ok(relay::Ended::Exited(exit)) => exit,
    Ok(relay::Ended::Detached { .. }) => anyhow::bail!("the command detached without a detach request"),
    Err(error) if error.ended() => {
      message(&error);
      return Ok(signals::ENDED);
    }
    Err(relay::Error::Terminated(signal)) => return Ok(128 + signals::number(signal).unwrap_or(1)),
    Err(error) => return Err(error.into()),
  };
  if exit.code.is_some() {
    return Ok(signals::shell_code(&exit));
  }
  if let Some(signal) = signals::number(&exit.signal).and_then(|n| Signal::try_from(n).ok()) {
    // When either call fails the shim lives on, and the shell's exit code
    // for the signal below stands in for the death.
    // SAFETY: the default disposition installs no handler.
    let _ = unsafe { nix::sys::signal::signal(signal, SigHandler::SigDfl) };
    let _ = nix::sys::signal::raise(signal);
  }
  Ok(signals::shell_code(&exit))
}
