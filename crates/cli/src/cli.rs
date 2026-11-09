//! The root command line: global flags, dispatch to `commands`, and exit codes.
//!
//! Exit codes: a command's own code (`exec` and `attach` return the process's,
//! 126 when a rule or an injected fault refuses it and 127 when it cannot
//! start); 1 for any other error, printed once with a `needy:` prefix.

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use needy_api_client::Client;

use crate::commands::{attach, build, daemon, exec, fs, kill, profile, ps, report, session};
use crate::paths::Paths;

#[derive(Parser)]
#[command(
  name = "needy",
  version,
  about = "Route a driving program's work to executors"
)]
struct Cli {
  /// Daemon endpoint: unix:/path, http://host:port or https://host:port.
  /// Defaults to the control daemon's socket in the runtime directory.
  #[arg(long, global = true, env = "NEEDY_ENDPOINT")]
  endpoint: Option<String>,
  /// Log to stderr: -v info, -vv debug for needy, -vvv everything.
  #[arg(short, long, global = true, action = clap::ArgAction::Count)]
  verbose: u8,
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Run a daemon.
  Daemon(daemon::Args),
  /// Run a command through placement.
  Exec(exec::Args),
  /// List processes.
  Ps(ps::Args),
  /// Reattach to a process, replaying recent output first.
  Attach(attach::Args),
  /// Send a signal to a process.
  Kill(kill::Args),
  /// Copy files between this machine and executors.
  Fs(fs::Args),
  /// Manage sessions: one actor and its executors.
  Session(session::Args),
  /// Build an actor image from a recipe, and the executor image.
  Build(build::Args),
  /// Check a profile: show routing decisions, matching faults and the recipe's findings.
  Profile(profile::Args),
  /// Write a markdown report of a session.
  Report(report::Args),
}

/// What every command may need. Paths are resolved on first use, so a command
/// that only talks to an explicit endpoint works without a home directory.
pub struct Context {
  endpoint: Option<String>,
}

impl Context {
  pub fn paths(&self) -> Result<Paths> {
    Paths::resolve()
  }

  /// The daemon endpoint: `--endpoint`, `NEEDY_ENDPOINT`, or the socket.
  pub fn endpoint(&self) -> Result<String> {
    match &self.endpoint {
      Some(endpoint) => Ok(endpoint.clone()),
      None => Ok(format!("unix:{}", self.paths()?.socket().display())),
    }
  }
}

/// Connects to a daemon, presenting the token in `NEEDY_TOKEN` when it is set.
pub async fn connect(endpoint: &str) -> Result<Client> {
  let client = Client::connect(endpoint).await.with_context(|| format!("cannot reach the daemon at {endpoint}"))?;
  Ok(match std::env::var("NEEDY_TOKEN") {
    Ok(token) if !token.is_empty() => client.with_token(&token)?,
    _ => client,
  })
}

/// Parses the command line, runs the command and returns the exit code.
pub fn run() -> i32 {
  let cli = Cli::parse();
  match dispatch(cli) {
    Ok(code) => code,
    Err(error) => {
      crate::output::message(format_args!("{error:#}"));
      1
    }
  }
}

fn dispatch(cli: Cli) -> Result<i32> {
  let ctx = Context {
    endpoint: cli.endpoint,
  };
  // Only the daemon writes a log file; a missing home directory leaves it
  // without one instead of failing here.
  let log_dir = match &cli.command {
    Command::Daemon(_) => ctx.paths().ok().map(|p| p.state),
    _ => None,
  };
  // The log file is the first thing in the state directory; the directory
  // is the daemon's and private. Logging reports a directory it cannot use.
  #[cfg(unix)]
  if let Some(dir) = &log_dir
    && let Err(e) = needy_api_server::create_private_dir(dir)
  {
    tracing::debug!("cannot create {}: {e}", dir.display());
  }
  let _logs = crate::logging::init(cli.verbose, log_dir.as_deref())?;
  let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
  let code = runtime.block_on(async move {
    match cli.command {
      Command::Daemon(args) => daemon::run(args, &ctx).await,
      Command::Exec(args) => exec::run(args, &ctx).await,
      Command::Ps(args) => ps::run(args, &ctx).await,
      Command::Attach(args) => attach::run(args, &ctx).await,
      Command::Kill(args) => kill::run(args, &ctx).await,
      Command::Fs(args) => fs::run(args, &ctx).await,
      Command::Session(args) => session::run(args, &ctx).await,
      Command::Build(args) => build::run(args, &ctx).await,
      Command::Profile(args) => profile::run(args, &ctx).await,
      Command::Report(args) => report::run(args, &ctx).await,
    }
  });
  // NOTICE: a task may still block on reading stdin, which a runtime drop
  // would wait for forever. The process is about to exit, so leave it.
  runtime.shutdown_background();
  code
}
