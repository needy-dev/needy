//! `needy shell`: runs a profile's driving program in a sandbox when the user
//! types its name in their own shell, without the `needy` prefix.
//!
//! The shell gets a sandboxed command (provisional term): a shell function
//! named after the file name of the profile's `actor.command[0]`. Each call
//! of it is one `needy run create -it` of the profile, and the caller's arguments
//! follow `actor.command` as they do after `--`. Every other command runs as
//! it always does. One activation script defines it, in one of two places:
//!
//! - a subshell (`needy shell -f PROFILE`) that runs the user's own startup
//!   files first and the activation script last, and ends with `exit`;
//! - the current shell (`needy shell activate -f PROFILE`), which ends with
//!   `needy shell deactivate`. A process cannot change the shell that started
//!   it, so these two run through a `needy` shell function that
//!   `needy shell init` prints, which runs the script itself.
//!
//! Design: `docs/ai/references/cli/2026-11-23-needy-shell-design.md`.
// TODO(needy-shell): a shared run (one run for a whole session, each call a
// new process in the same actor) is deferred. A driving program's exit ends
// its run, so it needs an idle actor, terminal processes started in the actor
// from the host, and per-call records in the report; the current shell also
// has no needy process that lives as long as the shell to stop the run. The
// owner reopens it (see the design doc's "Out of scope").

mod script;
#[cfg(unix)]
mod subshell;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::cli::{ConnectionArgs, Context};
use crate::output;

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct Args {
  #[command(flatten)]
  pub(crate) connection: ConnectionArgs,
  #[command(subcommand)]
  command: Option<Command>,
  /// Profile whose driving program the subshell runs in a sandbox (`builtin:NAME`, a name in `profiles/`, or a file).
  // TODO(needy-shell): one profile per session. Several profiles, or a map
  // from command names to profiles in the global configuration, wait for
  // the owner (see the design doc).
  #[arg(short = 'f', long, required = true)]
  profile: Option<PathBuf>,
  /// The shell to start, by name or path (default: `$SHELL`).
  #[arg(long, value_name = "SHELL")]
  shell: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
  /// Print the `needy` shell function that `activate` and `deactivate` need, for `eval "$(needy shell init zsh)"` in your shell's startup file.
  Init {
    /// The shell that runs the code.
    #[arg(value_enum)]
    shell: Shell,
  },
  /// Run the profile's program in a sandbox when you type its name in this shell (needs `needy shell init`).
  Activate {
    /// Profile whose driving program this shell runs in a sandbox (`builtin:NAME`, a name in `profiles/`, or a file).
    #[arg(short = 'f', long)]
    profile: PathBuf,
    /// Print the activation script for SHELL. The function `needy shell init` prints passes it; without it, the call did not come through that function.
    #[arg(long, value_name = "SHELL", value_enum, hide = true)]
    print: Option<Shell>,
  },
  /// Remove the sandboxed command from this shell and restore the prompt (needs `needy shell init`).
  Deactivate,
}

/// A shell needy writes an activation script for.
// TODO(needy-shell): fish needs its own script syntax (its `-C` option runs
// after its config files). Deferred until the owner asks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Shell {
  Zsh,
  Bash,
}

impl Shell {
  /// The shell a program names by its file name: `/bin/zsh` is zsh.
  fn of(program: &Path) -> Option<Self> {
    match program.file_name()?.to_str()?.trim_start_matches('-') {
      "zsh" => Some(Self::Zsh),
      "bash" => Some(Self::Bash),
      _ => None,
    }
  }

  /// The name `needy shell init` and `--print` take.
  fn name(self) -> &'static str {
    match self {
      Self::Zsh => "zsh",
      Self::Bash => "bash",
    }
  }

  /// The startup file an interactive shell of this kind reads.
  fn startup_file(self) -> &'static str {
    match self {
      Self::Zsh => "~/.zshrc",
      Self::Bash => "~/.bashrc",
    }
  }
}

/// What an activation script defines: a shell function named after the
/// profile's driving program, and the `needy` command line it runs.
pub(crate) struct SandboxedCommand {
  /// The function's name: the file name of `actor.command[0]`.
  pub(crate) name: String,
  /// `needy` and its arguments up to `--`; the caller's arguments follow.
  pub(crate) invocation: Vec<String>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  match args.command {
    Some(Command::Init { shell }) => init(shell),
    Some(Command::Activate {
      profile,
      print: Some(shell),
    }) => activate(&profile, shell, ctx),
    Some(Command::Activate { print: None, .. } | Command::Deactivate) => Err(needs_function().into()),
    None => {
      // clap requires `--profile` when no subcommand is given.
      let profile = args.profile.context("needy shell needs --profile")?;
      subshell(&profile, args.shell, ctx).await
    }
  }
}

/// Prints the `needy` shell function, unless stdout is a terminal: then the
/// code would only be shown, and the user most likely left out `eval`.
fn init(shell: Shell) -> Result<i32> {
  if std::io::stdout().is_terminal() {
    let name = shell.name();
    return Err(
      crate::diagnostics::Error::new("cli.invalid_arguments", "needy shell init prints shell code for your shell to run")
        .check(format!(
          "Run `eval \"$(needy shell init {name})\"`, or add that line to {}. To read the code, pipe it: `needy shell init {name} | less`.",
          shell.startup_file()
        ))
        .into(),
    );
  }
  output::out!("{}", script::init(shell))?;
  Ok(0)
}

/// Prints the activation script for the `needy` shell function to run.
fn activate(profile: &Path, shell: Shell, ctx: &Context) -> Result<i32> {
  let command = sandboxed_command(profile, ctx)?;
  // stderr: the function runs only what goes to stdout.
  output::message(format_args!("{} -> profile {} (needy shell deactivate restores it)", command.name, profile.display()));
  let script = match shell {
    Shell::Zsh | Shell::Bash => script::activation(&command),
  };
  output::out!("{script}")?;
  Ok(0)
}

/// The error for `activate` or `deactivate` that reached this program: the
/// `needy` shell function handles both, so the shell has no such function,
/// or the call did not start with `needy shell`.
fn needs_function() -> crate::diagnostics::Error {
  crate::diagnostics::Error::new(
    "cli.invalid_arguments",
    "needy shell activate and deactivate change this shell, which needs the `needy` shell function from needy shell init",
  )
  .check(
    "Run `eval \"$(needy shell init zsh)\"` (or `bash`), or add that line to your shell's startup file. The function handles only calls that start with `needy shell activate` or `needy shell deactivate`, so put other options after them. `needy shell -f PROFILE` starts a subshell instead.",
  )
}

async fn subshell(profile: &Path, program: Option<PathBuf>, ctx: &Context) -> Result<i32> {
  let command = sandboxed_command(profile, ctx)?;
  let program = match program {
    Some(program) => program,
    None => std::env::var_os("SHELL").filter(|value| !value.is_empty()).map(PathBuf::from).ok_or_else(|| {
      crate::diagnostics::Error::new("cli.invalid_arguments", "`$SHELL` is not set, so needy shell cannot tell which shell to start")
        .check("Pass `--shell zsh` or `--shell bash`.")
    })?,
  };
  let Some(shell) = Shell::of(&program) else {
    return Err(
      crate::diagnostics::Error::new("cli.invalid_arguments", "needy shell supports zsh and bash")
        .fact("shell", program.display().to_string())
        .check("Pass `--shell zsh` or `--shell bash`.")
        .into(),
    );
  };
  if !std::io::stdin().is_terminal() {
    return Err(
      crate::diagnostics::Error::new("cli.invalid_arguments", "needy shell starts an interactive shell, which needs a terminal")
        .check("Run it from a terminal.")
        .into(),
    );
  }
  output::message(format_args!("{} -> profile {} (exit leaves this shell)", command.name, profile.display()));
  start(&program, shell, &command).await
}

#[cfg(unix)]
async fn start(program: &Path, shell: Shell, command: &SandboxedCommand) -> Result<i32> {
  subshell::run(program, shell, command).await
}

#[cfg(not(unix))]
async fn start(_program: &Path, _shell: Shell, _command: &SandboxedCommand) -> Result<i32> {
  Err(
    crate::diagnostics::Error::new("platform.unsupported", "needy shell starts zsh or bash, which this platform does not run")
      .check("Use `needy run create -it -f PROFILE` instead.")
      .into(),
  )
}

/// The sandboxed command for a profile: its name and the `needy` command line it runs,
/// with the global options of this invocation so that a call reaches the
/// same configuration and control plane from any directory.
fn sandboxed_command(given: &Path, ctx: &Context) -> Result<SandboxedCommand> {
  let source = ctx.profile_source(given)?;
  let profile = needy_config::profile::load(&source)?;
  let name = program_name(&profile.actor.command)?;
  let mut invocation = vec!["needy".to_string()];
  if let Some(config) = &ctx.config {
    invocation.extend(["--config".into(), absolute(config)?]);
  }
  if let Some(context) = &ctx.selected_context {
    invocation.extend(["--context".into(), context.clone()]);
  }
  if let Some(address) = &ctx.address {
    invocation.extend(["--address".into(), address.clone()]);
  }
  if let Some(file) = &ctx.credential_file {
    invocation.extend(["--credential-file".into(), absolute(file)?]);
  }
  if ctx.quiet {
    invocation.push("--quiet".into());
  }
  invocation.extend(["run", "create", "-it", "-f"].map(String::from));
  // A built-in or named profile resolves the same way from any directory; a
  // file path is made absolute because the user may `cd` before a call.
  let argument = match given.to_str() {
    Some(text) if text.starts_with("builtin:") || crate::cli::is_profile_name(given) => text.to_string(),
    _ => absolute(given)?,
  };
  invocation.extend([argument, "--".into()]);
  Ok(SandboxedCommand { name, invocation })
}

/// An absolute path as text for a shell script.
fn absolute(path: &Path) -> Result<String> {
  let absolute = std::path::absolute(path).with_context(|| format!("resolve {}", path.display()))?;
  absolute.into_os_string().into_string().map_err(|path| anyhow::anyhow!("{} is not valid UTF-8", PathBuf::from(path).display()))
}

// NOTICE: a function with one of these names would shadow needy itself or a
// builtin that the activation script and `_needy_shell_deactivate` call, and a
// reserved word is read before any function, so its function would never
// run. Every other name is the user's choice of profile, shells and wrappers
// included (design note, 2026-11-24): a `bash` profile makes `bash` open bash
// in a sandbox, and `command bash` runs the program on this machine.
const REFUSED: &[&str] = &[
  "needy", "builtin", "command", "printf", "unset", "unalias", "typeset", "function", "eval", "source", "exec", "exit", "return", "if",
  "then", "else", "elif", "fi", "case", "esac", "for", "select", "while", "until", "do", "done", "in", "time", "coproc",
];

/// The sandboxed command's name: the file name of the profile's driving program.
fn program_name(command: &[String]) -> Result<String> {
  let refuse = |summary: String, value: &str| {
    crate::diagnostics::Error::new("config.invalid_value", summary)
      .fact("field", "actor.command")
      .fact("value", value)
      .check("Use a profile whose `actor.command` starts with the program to run, or run it with `needy run create -it -f PROFILE`.")
  };
  let Some(program) = command.first() else {
    return Err(refuse("The profile names no driving program in `actor.command`, so needy shell has nothing to run".into(), "").into());
  };
  let name = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or_default();
  // A name that starts with `-` reads as an option to `function` and
  // `unset`; other characters are outside what zsh and bash both accept
  // without quoting.
  let valid = name.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
    && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
  if !valid {
    return Err(refuse(format!("The driving program `{program}` cannot name a shell function"), program).into());
  }
  if REFUSED.contains(&name) {
    return Err(
      refuse(format!("The driving program `{name}` is needy itself, or a builtin or reserved word that needy shell needs"), program).into(),
    );
  }
  Ok(name.to_string())
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
