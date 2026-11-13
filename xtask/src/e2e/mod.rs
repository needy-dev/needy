//! `cargo xtask e2e`: the end-to-end matrix.
//!
//! Every cell gets its own short directory, its own control daemon (run as
//! the harness's user, never root), its own runs, and is checked for
//! invariants rather than output alone: exit codes, placement in the trace
//! and the report, ownership and modes after copy-in, mirror-out after every
//! way a run ends, filters both ways, private sockets, the user's identity,
//! fault effects, the run and actor states, and that nothing is left once its
//! daemon stopped. A cell that cannot run here is reported as not covered,
//! with the reason, and counted against the run, never as passed.
//!
//! Design and matrix:
//! `docs/ai/references/architecture/2026-11-13-e2e-harness-design.md`.

#[cfg(unix)]
mod checks;
#[cfg(unix)]
mod env;
#[cfg(unix)]
mod host;
#[cfg(unix)]
mod invariants;
#[cfg(unix)]
mod matrix;
#[cfg(unix)]
mod proc;
#[cfg(unix)]
mod remote;
#[cfg(unix)]
mod runs;
#[cfg(unix)]
mod scenarios;
#[cfg(unix)]
#[cfg(unix)]
mod summary;
#[cfg(unix)]
mod term;
#[cfg(unix)]
pub mod transport;
#[cfg(unix)]
mod tui;
#[cfg(unix)]
mod windows;

use std::path::PathBuf;

use anyhow::Result;

#[derive(clap::Args, Clone)]
pub struct Args {
  /// Run only the cells whose ID matches: a glob (`*` within one `/`
  /// segment, `**` across) or a substring, such as `macos-vz/**` or
  /// `end=stop`. Repeatable; any match selects.
  #[arg(long, value_name = "PATTERN")]
  only: Vec<String>,
  /// Leave out the cells whose ID matches, as `--only` matches. Repeatable.
  #[arg(long, value_name = "PATTERN")]
  skip: Vec<String>,
  /// Cells to run at once. Each has its own daemon and directories.
  #[arg(long, default_value_t = 2)]
  parallel: usize,
  /// Output: a human summary, or one JSON document on stdout.
  #[arg(short, long, value_enum, default_value_t = Format::Human)]
  output: Format,
  /// List the selected cells and what this host offers them; run nothing.
  #[arg(long)]
  list: bool,
  /// Use the guest needy and images built before instead of building them
  /// from this tree.
  #[arg(long)]
  no_prepare: bool,
  /// Keep every cell's directory under /tmp instead of removing it.
  #[arg(long)]
  keep: bool,
  /// The harness's own directory: images and guest needy built from this
  /// tree, and results. Defaults to `.tmp/e2e` in the checkout.
  #[arg(long)]
  work_dir: Option<PathBuf>,
  /// The `needy` to test instead of building this tree's.
  #[arg(long)]
  needy: Option<PathBuf>,
  /// Seconds one step of a cell may take (a run's end, a stop).
  #[arg(long, default_value_t = 180)]
  timeout: u64,
  /// Remove what the harness built (its container images and its work
  /// directory) and exit.
  #[arg(long)]
  clean: bool,
  /// Run on HOST over ssh instead: ship this commit with `git archive
  /// HEAD`, run there, and show the result here. Uncommitted changes are
  /// not shipped.
  #[arg(long, value_name = "HOST")]
  remote: Option<String>,
  /// The directory on the remote host (relative to its home): `src` holds
  /// the shipped tree, `work` the harness's directory.
  #[arg(long, value_name = "DIR", default_value = "needy-e2e")]
  remote_dir: String,
  /// Leave the shipped tree on the remote host.
  #[arg(long)]
  remote_keep: bool,
  /// Run on every host of FILE at once, each in its role (see
  /// `remote::Hosts`): build hosts build from this commit, run hosts get
  /// their builds, and the results are shown together. The file is local
  /// and never part of the repository.
  #[arg(long, value_name = "FILE")]
  hosts: Option<PathBuf>,
  /// Build everything (static needy and harness, guest, images) and write
  /// it to DIR as a work directory for a host that builds nothing; run no
  /// cell.
  #[arg(long, value_name = "DIR")]
  export: Option<PathBuf>,
  /// A Windows machine (an ssh destination whose shell is cmd.exe, with a
  /// Rust toolchain) that builds its own needy.exe and joins the
  /// `runner/at=windows` cells as a runner.
  #[arg(long, value_name = "SSH")]
  windows_runner: Option<String>,
  /// The harness's directory on the Windows runner, under its home.
  #[arg(long, value_name = "NAME", default_value = "needy-e2e")]
  windows_dir: String,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum Format {
  Human,
  Json,
}

/// Runs the harness and returns the process's exit code: 0 when every
/// selected cell passed, 1 when any failed, 2 when none failed but some
/// were not covered on this host.
#[cfg(unix)]
pub fn main(args: Args) -> Result<i32> {
  run::main(args)
}

#[cfg(not(unix))]
pub fn main(_args: Args) -> Result<i32> {
  anyhow::bail!("cargo xtask e2e runs on Linux and macOS: every actor driver needs one of them")
}

#[cfg(unix)]
mod run {
  use std::collections::VecDeque;
  use std::path::{Path, PathBuf};
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::{Arc, Mutex};
  use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

  use anyhow::{Context, Result, bail};

  use super::checks::Checks;
  use super::env::{CellEnv, Ctx};
  use super::host::{self, Notes, Prepare};
  use super::matrix::{self, Cell, Driver, End, Platform};
  use super::summary::{Outcome, Status};
  use super::{Args, Format, invariants, proc, remote, scenarios, summary};

  pub fn main(args: Args) -> Result<i32> {
    let root = workspace_root();
    let work = args.work_dir.clone().unwrap_or_else(|| root.join(".tmp").join("e2e"));
    if let Some(file) = &args.hosts {
      return remote::run_hosts(&root, file, &args);
    }
    if let Some(host) = &args.remote {
      return remote::run(&root, host, &args);
    }
    if args.clean {
      host::clean(&work)?;
      return Ok(0);
    }
    if let Some(docker) = host::docker_host() {
      // SAFETY: no other thread runs yet; the daemons and the in-process
      // engine probe read it from here on.
      unsafe { std::env::set_var("DOCKER_HOST", &docker) };
    }
    install_interrupt();
    std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;
    let work = work.canonicalize()?;
    let needy = match &args.needy {
      Some(needy) => needy.canonicalize().with_context(|| format!("no needy at {}", needy.display()))?,
      None => build_needy(&root)?,
    };

    let here = Platform::this();
    let all = matrix::cells();
    let selected: Vec<Cell> = all.into_iter().filter(|c| matrix::selected(&c.id(), &args.only, &args.skip)).collect();
    if selected.is_empty() {
      bail!("no cell matches --only {:?} --skip {:?}", args.only, args.skip);
    }
    // Preparing builds images; a selection with no actor cell for this
    // platform needs none of them.
    let needs_actor = args.export.is_some() || selected.iter().any(|c| c.driver != Driver::Host && c.elsewhere(here).is_none());
    let mut notes = Notes::default();
    let mode = if args.list || !needs_actor {
      Prepare::Skip
    } else if args.no_prepare {
      Prepare::Reuse
    } else {
      Prepare::Build
    };
    let tui = selected.iter().any(|c| matches!(c.scenario, matrix::Scenario::Tui(_)) && c.elsewhere(here).is_none());
    let host = host::prepare(&root, &work, &needy, mode, tui, &mut notes)?;
    if let Some(out) = &args.export {
      for note in &notes.0 {
        eprintln!("e2e: {note}");
      }
      host::export(&root, &host, out)?;
      return Ok(0);
    }
    if host.euid == 0 {
      notes.0.push("the harness runs as root, so every daemon would: each cell fails, since a root daemon hides permission bugs".into());
    }
    let wants_windows =
      selected.iter().any(|c| c.scenario == matrix::Scenario::Runner(matrix::RunnerAt::Windows) && c.elsewhere(here).is_none());
    let windows = match &args.windows_runner {
      Some(_) if wants_windows && !super::windows::enrollment_available() => {
        // TODO(windows-acl): the canonical runner join code-file flow needs
        // Windows private-file ACL validation; see the CLI access specification.
        // Do not ship/build remotely when enrollment cannot be accepted yet.
        Some(Err("automated Windows runner enrollment is not covered until Windows private code-file ACL validation is implemented".into()))
      }
      Some(ssh) if wants_windows && !args.list => Some(super::windows::prepare(&root, ssh, &args.windows_dir).map_err(|e| format!("{e:#}"))),
      Some(ssh) => Some(Ok(super::windows::Windows {
        ssh: ssh.clone(),
        dir: args.windows_dir.clone(),
      })),
      None => None,
    };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let transports = super::transport::Settings::load(&work)?;
    let ctx = Ctx {
      transports,
      windows,
      needy,
      host,
      keep: args.keep,
      results: work.join("results").join(stamp.to_string()),
      timeout: Duration::from_secs(args.timeout),
    };

    let mut planned = Vec::new();
    let mut queue = VecDeque::new();
    for cell in selected {
      match blocker(&ctx, &cell, here) {
        Some((status, reason)) => planned.push(Outcome::skipped(cell, status, reason)),
        None if args.list => planned.push(Outcome::skipped(cell, Status::Planned, String::new())),
        None => queue.push_back(cell),
      }
    }
    let total = queue.len();
    let outcomes = if total == 0 {
      Vec::new()
    } else {
      run_cells(&ctx, queue, args.parallel.max(1))
    };
    planned.extend(outcomes);
    let report = summary::Report {
      platform: here,
      host: &ctx.host,
      notes: &notes.0,
      outcomes: &planned,
      results: &ctx.results,
    };
    match args.output {
      Format::Human => summary::human(&report),
      Format::Json => println!("{}", serde_json::to_string_pretty(&summary::json(&report))?),
    }
    Ok(summary::exit_code(&planned))
  }

  fn workspace_root() -> PathBuf {
    // NOTICE: xtask always runs through `cargo xtask` from this checkout, so
    // its manifest directory locates the workspace.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
  }

  /// Builds this tree's `needy` (debug) and returns its path.
  fn build_needy(root: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    eprintln!("e2e: building needy from this tree");
    // NOTICE: a target directory of its own: xtask's features unify
    // differently from `needy`'s, so sharing `target/debug` would rebuild
    // half the workspace on every switch between the two.
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target")).join("e2e");
    let status = std::process::Command::new(cargo)
      .current_dir(root)
      .args(["build", "--locked", "--bin", "needy", "--target-dir"])
      .arg(&target)
      .status()
      .context("cannot run cargo")?;
    if !status.success() {
      bail!("cannot build needy");
    }
    Ok(target.join("debug").join("needy").canonicalize()?)
  }

  /// Why `cell` does not run on this host, if it does not.
  fn blocker(ctx: &Ctx, cell: &Cell, here: Platform) -> Option<(Status, String)> {
    if cell.scenario == matrix::Scenario::Runner(matrix::RunnerAt::Windows) && cell.elsewhere(here).is_none() {
      match &ctx.windows {
        None => return Some((Status::NotCovered, "no Windows runner: pass --windows-runner SSH".into())),
        Some(Err(e)) => return Some((Status::NotCovered, e.clone())),
        Some(Ok(_)) => {}
      }
    }
    if let matrix::Scenario::Runner(matrix::RunnerAt::Local(transport)) = cell.scenario
      && cell.elsewhere(here).is_none()
      && let Some(reason) = ctx.transports.blocker(transport)
    {
      return Some((Status::NotCovered, format!("transport {}: {reason}", transport.name())));
    }
    let host = &ctx.host;
    if let Some(reason) = cell.elsewhere(here) {
      return Some((Status::Elsewhere, reason));
    }
    if host.euid == 0 {
      return Some((Status::Fail, "the harness runs as root: a root daemon hides permission bugs; run it as a user".into()));
    }
    if cell.root {
      return Some((Status::NotCovered, "the cell needs a root daemon, which the harness does not start yet".into()));
    }
    if cell.driver != Driver::Host
      && let Some(Err(reason)) = host.drivers.get(&cell.driver)
    {
      return Some((Status::NotCovered, reason.clone()));
    }
    if cell.executor == Some(matrix::Executor::Container)
      && let Err(reason) = &host.container_executor
    {
      return Some((Status::NotCovered, reason.clone()));
    }
    if matches!(cell.driver, Driver::Container(_))
      && cell.mounts_workspace()
      && let Err(reason) = &host.host_sshfs
    {
      return Some((Status::NotCovered, reason.clone()));
    }
    if let matrix::Scenario::Tui(_) = cell.scenario
      && let Some(Err(reason)) = host.agents.get(&cell.driver)
    {
      return Some((Status::NotCovered, reason.clone()));
    }
    if cell.end == End::StopNow && !host.stop_now {
      return Some((Status::NotCovered, "`needy run stop` has no --now in this tree".into()));
    }
    None
  }

  fn run_cells(ctx: &Ctx, queue: VecDeque<Cell>, parallel: usize) -> Vec<Outcome> {
    let total = queue.len();
    let queue = Mutex::new(queue);
    let done = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
      for _ in 0..parallel.min(total) {
        scope.spawn(|| {
          while let Some(cell) = queue.lock().ok().and_then(|mut q| q.pop_front()) {
            let outcome = run_cell(ctx, cell);
            let mut done = done.lock().unwrap_or_else(|e| e.into_inner());
            done.push(outcome);
            summary::progress(done.len(), total, done.last().expect("just pushed"));
          }
        });
      }
    });
    done.into_inner().unwrap_or_else(|e| e.into_inner())
  }

  /// Held by a Cloudflare transport cell: the tunnel forwards to one fixed
  /// port, so those cells run one at a time.
  static CLOUDFLARE: Mutex<()> = Mutex::new(());

  fn run_cell(ctx: &Ctx, cell: Cell) -> Outcome {
    let _tunnel = (cell.scenario == matrix::Scenario::Runner(matrix::RunnerAt::Local(matrix::Transport::Cloudflare)))
      .then(|| CLOUDFLARE.lock().unwrap_or_else(|e| e.into_inner()));
    let start = Instant::now();
    let mut checks = Checks::default();
    let mut env = match CellEnv::new(ctx) {
      Ok(env) => env,
      Err(e) => {
        checks.check("cell directory", false, format!("{e:#}"));
        return Outcome::ran(cell, checks, start.elapsed(), None);
      }
    };
    let ran = (|| -> Result<()> {
      if cell.scenario == matrix::Scenario::Runner(matrix::RunnerAt::Windows) {
        // A free loopback port, for the remote runner's tunnel back here.
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        env.tcp = Some(port);
      }
      if let matrix::Scenario::Runner(matrix::RunnerAt::Local(transport)) = cell.scenario {
        // The port the transport forwards to: the tunnel's fixed origin
        // for Cloudflare, else a free one.
        env.tcp = Some(match (transport, &ctx.transports.cloudflare) {
          (matrix::Transport::Cloudflare, Some(cf)) => cf.origin_port,
          _ => super::transport::free_port()?,
        });
      }
      env.start_daemon()?;
      invariants::sockets(&env, &mut checks, "at daemon start");
      invariants::discovery(&env, &mut checks);
      scenarios::run(&mut env, &cell, &mut checks)
    })();
    if let Err(e) = ran {
      checks.check("the cell ran to its end", false, format!("{e:#}; daemon log: {}", env.daemon_log_tail()));
    }
    invariants::after(&mut env, &mut checks);
    let artifacts = if checks.passed() && !ctx.keep {
      None
    } else {
      keep_artifacts(&env, &cell, &ctx.results)
    };
    if !ctx.keep
      && let Err(e) = std::fs::remove_dir_all(&env.dir)
    {
      checks.check("the cell's directory can be removed by the daemon's user", false, format!("{}: {e}", env.dir.display()));
    }
    let artifacts = artifacts.or_else(|| (!checks.passed()).then(|| ctx.results.clone()));
    Outcome::ran(cell, checks, start.elapsed(), artifacts)
  }

  /// Copies what explains a failed cell (the daemon's log, the profiles,
  /// every run's console and log) to `results/CELL/`.
  fn keep_artifacts(env: &CellEnv, cell: &Cell, results: &Path) -> Option<PathBuf> {
    let dir = results.join(cell.id().replace(['/', '=', ','], "_"));
    std::fs::create_dir_all(&dir).ok()?;
    let mut stack = vec![env.dir.clone()];
    while let Some(d) = stack.pop() {
      let Ok(entries) = std::fs::read_dir(&d) else {
        continue;
      };
      for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
          continue;
        };
        if kind.is_dir() && !kind.is_symlink() {
          // A cell's fixtures and outputs are its own; logs and profiles
          // are what explain a failure.
          stack.push(path);
        } else if kind.is_file() {
          let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
          if name.ends_with(".log") || name.starts_with("profile-") || name == "daemon.pid" {
            let relative = path.strip_prefix(&env.dir).unwrap_or(&path).to_string_lossy().replace('/', "_");
            // Best effort: a file that vanished explains nothing anyway.
            let _ = std::fs::copy(&path, dir.join(relative));
          }
        }
      }
    }
    Some(dir)
  }

  static INTERRUPTED: AtomicBool = AtomicBool::new(false);

  extern "C" fn on_signal(_: libc_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
  }

  #[allow(non_camel_case_types)]
  type libc_int = std::os::raw::c_int;

  /// On SIGINT or SIGTERM, ends every daemon the harness started before
  /// exiting, so an interrupted run leaves no daemon, VM or container.
  fn install_interrupt() {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(SigHandler::Handler(on_signal), SaFlags::empty(), SigSet::empty());
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
      // SAFETY: the handler only stores to an atomic.
      if let Err(e) = unsafe { sigaction(signal, &action) } {
        eprintln!("e2e: cannot handle {signal}: {e}");
      }
    }
    let watched = Arc::new(());
    std::thread::spawn(move || {
      let _keep = watched;
      loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
          eprintln!("e2e: interrupted; stopping every daemon the harness started");
          super::env::kill_all();
          std::process::exit(130);
        }
        std::thread::sleep(Duration::from_millis(100));
      }
    });
    // `proc` is used by the watcher through `env::kill_all`.
    let _ = proc::on_path;
  }
}
