//! The proxy for a routed exec. It asks the daemon where the command runs,
//! then either `execve`s the real program here or relays it to an executor,
//! keeping the parent's view of a child: its exit status or signal death.
//!
//! What the proxy writes to stderr is the routed command's own error output,
//! so the proxy installs no log subscriber.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
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

pub(super) fn main() -> ! {
  let code = super::runtime().block_on(proxy()).unwrap_or_else(|e| {
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

/// The path the original `execve` named. argv[0] is the command's own and
/// `/proc/self/exe` is the proxy, so only the auxiliary vector has it.
fn exec_path() -> String {
  // NOTICE: neither std nor nix reads the auxiliary vector; `getauxval` is
  // the libc call for it. `AT_EXECFN` points into this process's stack and
  // lives as long as the process.
  let p = unsafe { libc::getauxval(libc::AT_EXECFN) } as *const libc::c_char;
  if p.is_null() {
    return std::env::args().next().unwrap_or_default();
  }
  unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

async fn proxy() -> Result<i32> {
  let path = exec_path();
  let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
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
    exec_local(&path, &args);
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
  let exit = match relay::run(stream, tty, true).await {
    Ok(exit) => exit,
    // The command's status is unknown; the parent sees 255, not success.
    Err(error) if error.ended() => {
      message(&error);
      return Ok(signals::ENDED);
    }
    // A second SIGTERM or SIGHUP: the proxy leaves like a shell's child.
    Err(relay::Error::Terminated(signal)) => return Ok(128 + signals::number(signal).unwrap_or(1)),
    Err(error) => return Err(error.into()),
  };
  if let Some(code) = exit.code {
    return Ok(code);
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
/// proxy.
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
