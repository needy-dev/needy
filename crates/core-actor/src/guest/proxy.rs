//! The proxy for a routed exec. It asks the daemon where the command runs,
//! then either `execve`s the real program here or relays it to an executor,
//! keeping the parent's view of a child: its exit status or signal death.
//!
//! What the proxy writes to stderr is the routed command's own error output,
//! so the proxy installs no log subscriber.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;
use needy_api_client::Kind;
use needy_api_client::exec::ExecStream;
use needy_api_proto::v1::{Ancestor, DecideRequest, ExecStart, Origin};
use needy_core_executor::{relay, signals};
use nix::sys::signal::{SigHandler, Signal};

use super::init::STUB_DIR;

/// Environment the actor passes to routed commands when rules do not.
const PASS_ENV: &[&str] = &[
  "TERM",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "COLORTERM",
  "NO_COLOR",
];

/// One exec the proxy decides: what the executor is told, and what runs
/// when the decision keeps it in the actor.
pub(super) struct Invocation {
  /// The program as the executor sees it: the path the original `execve`
  /// named (see [`command`]).
  pub path: String,
  /// The original argv.
  pub args: Vec<String>,
  pub local: Local,
}

/// What runs in the actor when the daemon keeps an exec there.
pub(super) enum Local {
  /// This program, with the original argv.
  Program(String),
  /// Nothing: a route stub stands for a program the actor does not have.
  Missing,
}

/// The proxy as `binfmt_needy` (in a VM) or a route stub (in a container)
/// starts it, through the shim stub at `/.needy/proxy`. `path` is the path
/// the original `execve` named, which the stub passed on: argv[0] is the
/// command's own and this process is the guest needy, so only the stub's
/// auxiliary vector had it (`AT_EXECFN`).
pub(super) fn main(path: String) -> ! {
  let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
  let path = if path.is_empty() {
    args.first().cloned().unwrap_or_default()
  } else {
    path
  };
  // In a VM the kernel runs a stub's empty file only through the proxy; in
  // a container the stub is a link to the proxy itself. Either way a stub
  // has no program to run here.
  let local = if Path::new(&path).starts_with(STUB_DIR) {
    Local::Missing
  } else {
    Local::Program(path.clone())
  };
  run(Invocation { path, args, local })
}

/// Decides `invocation` with the daemon and runs it; exits with its status.
pub(super) fn run(invocation: Invocation) -> ! {
  let code = super::runtime().block_on(proxy(invocation)).unwrap_or_else(|e| {
    message(format_args!("{e:#}"));
    126
  });
  std::process::exit(code)
}

/// Prints a message as the routed command's error output, worded like
/// `needy exec`'s.
fn message(message: impl std::fmt::Display) {
  eprintln!("needy: {message}");
}

async fn proxy(invocation: Invocation) -> Result<i32> {
  let Invocation { path, args, local } = invocation;
  let argv = command(&path, &args);
  let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
  let env: HashMap<String, String> = std::env::vars().collect();
  let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
  let client = super::daemon().await?;
  let ancestors = super::ancestry::ancestors(std::path::Path::new("/proc"), std::process::id() as i32);
  let decision = client
    .runs()
    .decide(DecideRequest {
      run_id: String::new(),
      argv: argv.clone(),
      cwd: cwd.clone(),
      env: env.clone(),
      tty,
      ancestors: ancestors.into_iter().map(|argv| Ancestor { argv }).collect(),
    })
    .await
    .map_err(needy_api_client::Error::from)?
    .into_inner();
  if !decision.deny.is_empty() {
    message(&decision.deny);
    return Ok(126);
  }
  if decision.target == "actor" || decision.target.is_empty() {
    match local {
      Local::Program(program) => exec_local(&program, &args),
      Local::Missing => {
        message(format_args!("{}: command not found", argv.first().map(String::as_str).unwrap_or_default()));
        return Ok(127);
      }
    }
  }
  let mut pass: HashMap<String, String> = PASS_ENV.iter().filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
  pass.extend(decision.env);
  let start = ExecStart {
    argv: if decision.argv.is_empty() {
      args
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
  let stream = match ExecStream::start(&client, start).await {
    Ok(stream) => stream,
    Err(error) => match refused(error.kind()) {
      Some(code) => {
        message(&error);
        return Ok(code);
      }
      None => return Err(error.into()),
    },
  };
  // Detach belongs to the outer run client. A routed child must report its
  // command's exit to its parent, so it never consumes detach keys itself.
  let options = relay::Options {
    tty,
    stdin: true,
    ..Default::default()
  };
  let exit = match relay::run(stream, options).await {
    Ok(relay::Ended::Exited(exit)) => exit,
    Ok(relay::Ended::Detached { .. }) => anyhow::bail!("the routed command detached without a detach request"),
    // The command's status is unknown; the parent sees 255, not success.
    Err(error) if error.ended() => {
      message(&error);
      return Ok(signals::ENDED);
    }
    // A second SIGTERM or SIGHUP: the proxy leaves like a shell's child.
    Err(relay::Error::Terminated(signal)) => return Ok(128 + signals::number(signal).unwrap_or(1)),
    Err(error) => return Err(error.into()),
  };
  if let Some(code) = signals::unrepresentable(&exit) {
    message(signals::out_of_range_message(code));
  }
  if exit.code.is_some() {
    return Ok(signals::shell_code(&exit));
  }
  // Die of the same signal so the parent's wait sees signal death.
  if let Some(signal) = signals::number(&exit.signal).and_then(|n| Signal::try_from(n).ok()) {
    // When either call fails the process lives on, and the shell's exit
    // code for the signal below stands in for the death.
    // SAFETY: the default disposition installs no handler.
    let _ = unsafe { nix::sys::signal::signal(signal, SigHandler::SigDfl) };
    let _ = nix::sys::signal::raise(signal);
  }
  Ok(signals::shell_code(&exit))
}

/// The command the actor asked for: `args` with argv[0] replaced by the path
/// the original `execve` named, so the executor can run the same program.
/// A route stub stands for a program the actor does not have; its path means
/// nothing elsewhere, so the command keeps the stub's bare name, as the user
/// typed it, and the executor finds the program on its own PATH.
fn command(path: &str, args: &[String]) -> Vec<String> {
  let program = match Path::new(path).strip_prefix(STUB_DIR) {
    Ok(name) if name.components().count() == 1 => name.to_string_lossy().into_owned(),
    _ => path.to_string(),
  };
  let mut argv = args.to_vec();
  if let Some(first) = argv.first_mut() {
    *first = program;
  }
  argv
}

/// The exit code for a start the daemon refused, like `needy exec`'s and a
/// shell's: 126 when a rule or an injected fault refuses the command, 127
/// when it cannot start. Other errors are the proxy's own.
fn refused(kind: Kind) -> Option<i32> {
  match kind {
    Kind::Denied => Some(126),
    Kind::Failed => Some(127),
    _ => None,
  }
}

/// Runs the real program here. The kernel does not route execs made by the
/// proxy, and in a container `path` is the real program, not its shim.
fn exec_local(path: &str, args: &[String]) -> ! {
  // NOTICE: a path or argument from `execve` cannot hold a NUL byte.
  let program = CString::new(path).unwrap_or_default();
  let argv: Vec<CString> = args.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();
  let Err(errno) = nix::unistd::execv(&program, &argv);
  message(format_args!("cannot run {path}: {}", std::io::Error::from(errno)));
  std::process::exit(127)
}

#[cfg(test)]
#[path = "proxy_test.rs"]
mod tests;
