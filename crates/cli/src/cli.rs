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

use crate::commands::{client, config, daemon, fs, image, proc, run, runner};
use crate::paths::Paths;

#[derive(Parser)]
#[command(
  name = "needy",
  version,
  about = "Route a driving program's work to executors"
)]
struct Cli {
  /// Client configuration document. Named profiles are beside it in profiles/.
  #[arg(long, global = true)]
  config: Option<PathBuf>,
  /// Saved control-plane connection for this invocation.
  #[arg(long = "context", global = true, conflicts_with = "address")]
  selected_context: Option<String>,
  /// Direct transport: tcp://HOST:PORT, https://HOST:PORT or unix:///PATH.
  #[arg(long, global = true)]
  address: Option<String>,
  /// Private outgoing bearer file; only with a direct address.
  #[arg(long, global = true, requires = "address")]
  credential_file: Option<PathBuf>,
  /// Infrastructure errors on stderr; child output is unchanged.
  #[arg(long, global = true, value_enum, default_value = "human")]
  diagnostic_format: crate::diagnostics::Format,
  /// Log to stderr: -v info, -vv debug for needy, -vvv everything.
  #[arg(short, long, global = true, action = clap::ArgAction::Count)]
  verbose: u8,
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Manage client configuration, connections and task profiles.
  Config(config::Args),
  /// Serve locally or manage this machine's control service.
  Daemon(Box<daemon::Args>),
  /// Authorize clients on the selected control plane.
  Client(client::Args),
  /// Register execution machines and manage runner credentials.
  Runner(runner::Args),
  /// Manage an actor and its bound executors.
  Run(run::Args),
  /// Execute, list, attach and signal executor processes.
  Proc(proc::Args),
  /// Copy files between this machine and an explicit remote target.
  Fs(fs::Args),
  /// Pull, list, build and remove local images.
  Image(image::Args),
}

/// Invocation inputs. Target resolution lives in one connection boundary.
pub struct Context {
  pub(crate) config: Option<PathBuf>,
  pub(crate) selected_context: Option<String>,
  pub(crate) address: Option<String>,
  pub(crate) credential_file: Option<PathBuf>,
  pub(crate) diagnostic_format: crate::diagnostics::Format,
}

impl Context {
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

  /// A bare profile name is resolved only in the selected config directory.
  pub fn profile(&self, given: &Path) -> Result<PathBuf> {
    let bare = given.components().count() == 1
      && matches!(given.components().next(), Some(Component::Normal(_)))
      && !matches!(given.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"));
    if !bare {
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
  let diagnostic_context = Context {
    config: cli.config.clone(),
    selected_context: cli.selected_context.clone(),
    address: cli.address.clone(),
    credential_file: cli.credential_file.clone(),
    diagnostic_format: format,
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
  let remote = cli.selected_context.is_some() || cli.address.is_some() || cli.credential_file.is_some();
  if remote && matches!(&cli.command, Command::Daemon(_) | Command::Image(_)) {
    anyhow::bail!("this operation is local; remove --context/--address and use its local options");
  }
  if remote && matches!(&cli.command, Command::Config(args) if !args.allows_selection()) {
    anyhow::bail!("this configuration operation is local; remove --context/--address");
  }
  if remote && matches!(&cli.command, Command::Runner(args) if !args.allows_selection()) {
    anyhow::bail!("runner serve uses this machine's saved enrollment; remove --context/--address");
  }
  let ctx = Context {
    config: cli.config,
    selected_context: cli.selected_context,
    address: cli.address,
    credential_file: cli.credential_file,
    diagnostic_format: cli.diagnostic_format,
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
