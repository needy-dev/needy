//! The root command line: global flags, dispatch to `commands`, and exit codes.
//!
//! Exit codes: a command's own code (`exec` and `attach` return the process's,
//! 126 when a rule or an injected fault refuses it and 127 when it cannot
//! start); 1 for any other error, printed once with a `needy:` prefix; and
//! [`CLOSED_STDOUT`], without a message, when stdout's reader went away.

use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use needy_api_client::Client;

use crate::commands::{client, config, daemon, fs, image, proc, run, runner, shell};
use crate::paths::Paths;

/// Control-plane connection options for commands that talk to a daemon.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct ConnectionArgs {
  /// Saved control-plane connection to use for this command.
  #[arg(long = "context", global = true, conflicts_with = "address")]
  pub(crate) selected_context: Option<String>,
  /// Direct control-plane address (`tcp://HOST:PORT`, `https://HOST:PORT`, or `unix:///PATH`).
  #[arg(long, global = true)]
  pub(crate) address: Option<String>,
  /// Private bearer token file; requires `--address`.
  #[arg(long, global = true, requires = "address")]
  pub(crate) credential_file: Option<PathBuf>,
}

impl ConnectionArgs {
  pub(crate) fn is_set(&self) -> bool {
    self.selected_context.is_some() || self.address.is_some() || self.credential_file.is_some()
  }
}

#[derive(Parser)]
#[command(
  name = "needy",
  version,
  about = "Run isolated actors and route their commands to executors"
)]
struct Cli {
  /// Path to the client configuration file (named profiles live beside it in `profiles/`).
  #[arg(long, global = true)]
  config: Option<PathBuf>,
  /// Format for infrastructure errors on stderr (child output is unchanged).
  #[arg(long, global = true, value_enum, default_value = "human")]
  diagnostic_format: crate::diagnostics::Format,
  /// Suppress non-error status messages and progress spinners.
  #[arg(short, long, global = true)]
  quiet: bool,
  /// Enable diagnostic logging on stderr (`-v` info, `-vv` debug for needy, `-vvv` trace).
  #[arg(short, long, global = true, action = clap::ArgAction::Count)]
  verbose: u8,
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Manage client configuration, saved connections, and task profiles.
  Config(config::Args),
  /// Run the local control daemon or manage its OS service.
  Daemon(Box<daemon::Args>),
  /// Invite, list, and revoke remote clients on the control plane.
  Client(client::Args),
  /// Enroll and manage remote execution runners.
  Runner(runner::Args),
  /// Create, inspect, attach to, and stop runs.
  Run(run::Args),
  /// Run a profile's program in a sandbox by typing its name in your shell.
  Shell(shell::Args),
  /// Execute, list, attach to, and signal executor processes.
  Proc(proc::Args),
  /// Copy files between this machine and an executor.
  Fs(fs::Args),
  /// Pull, list, build, edit, export, and remove actor images.
  Image(image::Args),
}

impl Command {
  fn connection(&self) -> ConnectionArgs {
    match self {
      Command::Config(args) => args.connection(),
      Command::Daemon(_) => ConnectionArgs::default(),
      Command::Client(args) => args.connection.clone(),
      Command::Runner(args) => args.connection(),
      Command::Run(args) => args.connection.clone(),
      Command::Shell(args) => args.connection.clone(),
      Command::Proc(args) => args.connection.clone(),
      Command::Fs(args) => args.connection.clone(),
      Command::Image(args) => args.connection(),
    }
  }
}

/// Invocation inputs. Target resolution lives in one connection boundary.
pub struct Context {
  pub(crate) config: Option<PathBuf>,
  pub(crate) selected_context: Option<String>,
  pub(crate) address: Option<String>,
  pub(crate) credential_file: Option<PathBuf>,
  pub(crate) diagnostic_format: crate::diagnostics::Format,
  pub(crate) quiet: bool,
}

impl Context {
  /// Whether animated progress may be shown for this command.
  /// Respects `--quiet`, `--diagnostic-format json`, `-v`/`RUST_LOG`, and
  /// whether stdout is human-readable (`is_table`).
  pub(crate) fn show_progress(&self, is_table: bool) -> bool {
    !self.quiet && is_table && !matches!(self.diagnostic_format, crate::diagnostics::Format::Json) && !crate::logging::stderr_active()
  }

  /// A connector owns stderr for prompts and diagnostics throughout a request.
  /// If selection cannot be inspected, omit animation; the connection boundary
  /// still reports the original selection error when the command connects.
  pub(crate) fn connection_feedback(&self) -> bool {
    self.show_progress(true) && crate::connection::resolve(self).is_ok_and(|selected| selected.command.is_none())
  }

  /// Whether animated progress may be shown while talking to a control daemon
  /// for a command whose stdout format is controlled by `is_table`.
  pub(crate) fn connection_progress(&self, is_table: bool) -> bool {
    is_table && self.connection_feedback()
  }

  pub fn configuration_file(&self) -> Result<PathBuf> {
    Ok(self.base_paths()?.config_file())
  }

  pub(crate) fn base_paths(&self) -> Result<Paths> {
    let paths = Paths::resolve()?;
    Ok(match &self.config {
      Some(path) => paths.with_config_file(path.clone()),
      None => paths,
    })
  }

  pub fn configuration(&self) -> Result<needy_config::global::Config> {
    crate::configuration::load(&self.configuration_file()?)
  }

  pub fn paths(&self) -> Result<Paths> {
    let mut paths = self.base_paths()?;
    paths.apply(&self.configuration()?.paths);
    Ok(paths)
  }

  pub async fn connect(&self) -> Result<Client> {
    crate::connection::resolve(self)?.connect().await
  }

  /// Resolves a profile argument (`builtin:NAME`, a bare profile name in the
  /// config directory, or a file path) into a [`needy_config::Source`].
  pub fn profile_source(&self, given: &Path) -> Result<needy_config::Source> {
    if let Some(name) = given.to_str().and_then(|s| s.strip_prefix("builtin:")) {
      return Ok(needy_config::Source::builtin(name));
    }
    Ok(needy_config::Source::file(self.profile(given)?))
  }

  /// A bare profile name is resolved only in the selected config directory.
  pub fn profile(&self, given: &Path) -> Result<PathBuf> {
    if !is_profile_name(given) {
      return Ok(given.to_path_buf());
    }
    let profiles = self.paths()?.profiles();
    let named = |extension: &str| {
      let mut name = given.as_os_str().to_os_string();
      name.push(extension);
      profiles.join(name)
    };
    let yaml = named(".yaml");
    let yml = named(".yml");
    match (yaml.try_exists()?, yml.try_exists()?) {
      (true, true) => Err(
        crate::diagnostics::Error::new("config.invalid_reference", "The named profile has both .yaml and .yml files")
          .fact("profile", given.display().to_string())
          .check("Choose an explicit profile path or remove the duplicate name.")
          .into(),
      ),
      (false, true) => Ok(yml),
      _ => Ok(yaml),
    }
  }
}

/// Whether a profile argument is a bare name (`codex`), which names a file in
/// the selected config directory's `profiles/`, rather than a path.
pub(crate) fn is_profile_name(given: &Path) -> bool {
  given.components().count() == 1
    && matches!(given.components().next(), Some(Component::Normal(_)))
    && !matches!(given.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"))
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
  let format = cli.diagnostic_format;
  let connection = cli.command.connection();
  let diagnostic_context = Context {
    config: cli.config.clone(),
    selected_context: connection.selected_context.clone(),
    address: connection.address.clone(),
    credential_file: connection.credential_file.clone(),
    diagnostic_format: format,
    quiet: cli.quiet,
  };
  match dispatch(cli) {
    Ok(code) => code,
    Err(error) if stdout_closed(&error) => CLOSED_STDOUT,
    Err(error) => {
      crate::diagnostics::render_in(&error, format, Some(&diagnostic_context));
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
  // `--diagnostic-format json` reserves stderr for machine-readable JSON diagnostics,
  // so suppress human status and progress chatter on stderr alongside `--quiet`.
  crate::output::set_quiet(cli.quiet || matches!(cli.diagnostic_format, crate::diagnostics::Format::Json));
  let connection = cli.command.connection();
  let ctx = Context {
    config: cli.config,
    selected_context: connection.selected_context,
    address: connection.address,
    credential_file: connection.credential_file,
    diagnostic_format: cli.diagnostic_format,
    quiet: cli.quiet,
  };
  if let Command::Daemon(args) = &cli.command {
    // SAFETY: argument parsing and configuration reads above spawn no threads.
    // Apply daemon process settings before logging or the Tokio runtime starts.
    for (name, value) in args.environment(&ctx)? {
      unsafe {
        std::env::set_var(name, value);
      }
    }
  }
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
      Command::Config(args) => config::run(args, &ctx).await,
      Command::Daemon(args) => daemon::run(*args, &ctx).await,
      Command::Client(args) => client::run(args, &ctx).await,
      Command::Runner(args) => runner::run(args, &ctx).await,
      Command::Run(args) => run::run(args, &ctx).await,
      Command::Shell(args) => shell::run(args, &ctx).await,
      Command::Proc(args) => proc::run(args, &ctx).await,
      Command::Fs(args) => fs::run(args, &ctx).await,
      Command::Image(args) => image::run(args, &ctx).await,
    }
  });
  // NOTICE: a task may still block on reading stdin, which a runtime drop
  // would wait for forever. The process is about to exit, so leave it.
  runtime.shutdown_background();
  code
}
