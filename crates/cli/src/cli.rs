//! The root command line: global flags, dispatch to `commands`, and exit codes.
//!
//! Exit codes: a command's own code (`exec` and `attach` return the process's,
//! 126 when a rule or an injected fault refuses it and 127 when it cannot
//! start); 1 for any other error, printed once with a `needy:` prefix; and
//! [`CLOSED_STDOUT`], without a message, when stdout's reader went away.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use needy_api_client::Client;

use crate::commands::{attach, chaos, daemon, exec, fs, image, kill, pair, profile, ps, report, run, runner};
use crate::paths::Paths;

#[derive(Parser)]
#[command(
  name = "needy",
  version,
  about = "Route a driving program's work to executors"
)]
struct Cli {
  /// Daemon endpoint: unix:/path, http://host:port, https://host:port, or
  /// exec:COMMAND, a command whose stdin and stdout carry each connection
  /// (as ssh's ProxyCommand: `exec:cloudflared access tcp --hostname H`).
  /// Defaults to the endpoint this machine paired with last (`needy pair`),
  /// else the control daemon's socket in the runtime directory.
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
  // Boxed: its options are the largest by far (clippy::large_enum_variant
  // on Windows, where paths are larger).
  Daemon(Box<daemon::Args>),
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
  /// Manage runs: one actor and its executors.
  Run(run::Args),
  /// Manage runners: machines that joined this control plane, and join tokens.
  Runner(runner::Args),
  /// Pair this machine with a control plane once (`pair CODE`), and manage
  /// pairing codes and paired clients.
  Pair(pair::Args),
  /// Pull, list, build and remove actor images.
  Image(image::Args),
  /// Check a profile: show routing decisions, matching faults and the recipe's findings.
  Profile(profile::Args),
  /// Report on a run from its trace: who started it, what ran where.
  Report(report::Args),
  /// Add, list and remove a live run's chaos faults.
  Chaos(chaos::Args),
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

  /// The control plane a command reaches: `--endpoint`, `NEEDY_ENDPOINT`,
  /// the endpoint this machine paired with last (`needy pair`), or the
  /// control socket.
  pub fn endpoint(&self) -> Result<String> {
    if let Some(endpoint) = &self.endpoint {
      return Ok(endpoint.clone());
    }
    let paths = self.paths()?;
    match crate::credentials::default_endpoint(&paths.credentials())? {
      Some(paired) => Ok(paired),
      None => Ok(format!("unix:{}", paths.socket().display())),
    }
  }

  /// Where a daemon on this machine listens by default: `--endpoint`,
  /// `NEEDY_ENDPOINT`, or the control socket, never the control plane this
  /// machine paired with.
  pub fn listen_endpoint(&self) -> Result<String> {
    match &self.endpoint {
      Some(endpoint) => Ok(endpoint.clone()),
      None => Ok(format!("unix:{}", self.paths()?.socket().display())),
    }
  }

  /// The profile file `--profile` names: a bare name, with no path
  /// separator and no `.yaml` or `.yml`, is `profiles/NAME.yaml` in the
  /// config directory (owner decision, 2026-11-12); anything else is a path.
  pub fn profile(&self, given: &Path) -> Result<PathBuf> {
    let bare = given.components().count() == 1
      && matches!(given.components().next(), Some(Component::Normal(_)))
      && !matches!(given.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"));
    if !bare {
      return Ok(given.to_path_buf());
    }
    let mut file = given.as_os_str().to_os_string();
    file.push(".yaml");
    Ok(self.paths()?.profiles().join(file))
  }
}

/// Connects to a daemon, presenting the token in `NEEDY_TOKEN` when it is
/// set, or else the client token `needy pair` stored for `endpoint`.
pub async fn connect(endpoint: &str) -> Result<Client> {
  let client = connect_without_token(endpoint).await?;
  Ok(match token(endpoint)? {
    Some(token) => client.with_token(&token)?,
    None => client,
  })
}

/// Connects to a daemon without presenting a token, for `Pair`.
pub async fn connect_without_token(endpoint: &str) -> Result<Client> {
  Client::connect(endpoint).await.with_context(|| format!("cannot reach the daemon at {endpoint}"))
}

/// The bearer token for `endpoint`: `NEEDY_TOKEN`, which wins so an
/// operator can act with the control token, or the stored client token.
fn token(endpoint: &str) -> Result<Option<String>> {
  if let Ok(token) = std::env::var("NEEDY_TOKEN")
    && !token.is_empty()
  {
    return Ok(Some(token));
  }
  // Without a home directory there is no stored token to find.
  let Ok(paths) = Paths::resolve() else {
    return Ok(None);
  };
  Ok(crate::credentials::find(&paths.credentials(), endpoint)?.map(|credential| credential.token))
}

/// The exit code of a command whose stdout's reader went away, as with
/// `needy run ls | grep -q ...`: 128 + SIGPIPE, what a shell reports for a
/// program SIGPIPE ended, so pipelines see the usual status.
// NOTICE: Rust ignores SIGPIPE before `main`, so a write to a closed pipe
// returns `EPIPE` (`BrokenPipe`) instead of ending the process. SIGPIPE is 13
// on Linux and macOS (`signal(7)`, `sys/signal.h`); Windows has no SIGPIPE
// and gets the same code.
pub const CLOSED_STDOUT: i32 = 128 + 13;

/// Parses the command line, runs the command and returns the exit code.
pub fn run() -> i32 {
  let cli = Cli::parse();
  match dispatch(cli) {
    Ok(code) => code,
    Err(error) if stdout_closed(&error) => CLOSED_STDOUT,
    Err(error) => {
      crate::output::message(format_args!("{error:#}"));
      1
    }
  }
}

/// Whether a command failed because stdout's reader went away: a
/// `BrokenPipe` from a local write (`output::write`, the exec relay, run
/// logs). One under a daemon call is that connection's, not stdout's.
fn stdout_closed(error: &anyhow::Error) -> bool {
  for cause in error.chain() {
    if cause.is::<needy_api_client::Error>() {
      return false;
    }
    if let Some(io) = cause.downcast_ref::<std::io::Error>()
      && io.kind() == std::io::ErrorKind::BrokenPipe
    {
      return true;
    }
  }
  false
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
      Command::Daemon(args) => daemon::run(*args, &ctx).await,
      Command::Exec(args) => exec::run(args, &ctx).await,
      Command::Ps(args) => ps::run(args, &ctx).await,
      Command::Attach(args) => attach::run(args, &ctx).await,
      Command::Kill(args) => kill::run(args, &ctx).await,
      Command::Fs(args) => fs::run(args, &ctx).await,
      Command::Run(args) => run::run(args, &ctx).await,
      Command::Runner(args) => runner::run(args, &ctx).await,
      Command::Pair(args) => pair::run(args, &ctx).await,
      Command::Image(args) => image::run(args, &ctx).await,
      Command::Profile(args) => profile::run(args, &ctx).await,
      Command::Report(args) => report::run(args, &ctx).await,
      Command::Chaos(args) => chaos::run(args, &ctx).await,
    }
  });
  // NOTICE: a task may still block on reading stdin, which a runtime drop
  // would wait for forever. The process is about to exit, so leave it.
  runtime.shutdown_background();
  code
}
