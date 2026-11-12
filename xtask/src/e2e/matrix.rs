//! The matrix: its dimensions, and the cells that cover them.
//!
//! The full product of every dimension is thousands of runs, most of them
//! testing the same code twice. The matrix is a covering set instead: for
//! every actor driver, each value of each dimension appears in at least one
//! cell, and the combinations that hid the bugs this harness was written for
//! are cells of their own (nested copy-in, mirror-out with a filter for every
//! way a run ends, an executor's mirror-out, a renamed user on a container
//! executor). The design note lists them:
//! `docs/ai/references/architecture/2026-11-13-e2e-harness-design.md`.

use std::fmt;

/// Where a cell can run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
  Any,
  Linux,
  Macos,
}

impl Platform {
  pub fn this() -> Self {
    if cfg!(target_os = "linux") {
      Platform::Linux
    } else if cfg!(target_os = "macos") {
      Platform::Macos
    } else {
      // Unreachable in practice: the harness is Unix only, and other Unixes
      // run neither driver family.
      Platform::Any
    }
  }

  fn name(self) -> &'static str {
    match self {
      Platform::Any => "any platform",
      Platform::Linux => "Linux",
      Platform::Macos => "macOS",
    }
  }

  /// Where both `self` and `other` hold, or `None` when nowhere does.
  fn and(self, other: Platform) -> Option<Platform> {
    match (self, other) {
      (Platform::Any, p) | (p, Platform::Any) => Some(p),
      (a, b) if a == b => Some(a),
      _ => None,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Oci {
  Runc,
  Runsc,
}

impl Oci {
  pub fn name(self) -> &'static str {
    match self {
      Oci::Runc => "runc",
      Oci::Runsc => "runsc",
    }
  }
}

/// The actor driver dimension. `Host` is the cells without an actor:
/// `needy exec` and `needy fs cp` against the control daemon's own native
/// executor, which need no VM or engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Driver {
  Host,
  Firecracker,
  Container(Oci),
  Vz,
  AppleContainer,
}

impl Driver {
  pub const ACTORS: [Driver; 5] = [
    Driver::Firecracker,
    Driver::Container(Oci::Runc),
    Driver::Container(Oci::Runsc),
    Driver::Vz,
    Driver::AppleContainer,
  ];

  /// The name in cell IDs: the profile's `actor.driver`, with the OCI
  /// runtime for `linux-container`.
  pub fn name(self) -> String {
    match self {
      Driver::Host => "none".into(),
      Driver::Firecracker => "linux-firecracker".into(),
      Driver::Container(oci) => format!("linux-container.{}", oci.name()),
      Driver::Vz => "macos-vz".into(),
      Driver::AppleContainer => "macos-apple-container".into(),
    }
  }

  /// The profile's `actor.driver`.
  pub fn profile_name(self) -> &'static str {
    match self {
      Driver::Host => "",
      Driver::Firecracker => "linux-firecracker",
      Driver::Container(_) => "linux-container",
      Driver::Vz => "macos-vz",
      Driver::AppleContainer => "macos-apple-container",
    }
  }

  pub fn platform(self) -> Platform {
    match self {
      Driver::Host => Platform::Any,
      // NOTICE: `linux-container` is detected wherever an engine answers,
      // a Mac with Docker Desktop or OrbStack included, but its channels are
      // host Unix sockets bind-mounted into the container, which only work
      // where the container shares the daemon's kernel (the NOTICE in
      // `crates/core-actor/src/driver/linux_container/mod.rs`). Its cells
      // are Linux cells.
      Driver::Firecracker | Driver::Container(_) => Platform::Linux,
      Driver::Vz | Driver::AppleContainer => Platform::Macos,
    }
  }

  /// Whether the driver presents sensors and the overheat fault (README's
  /// driver table: ❌ for `macos-apple-container`).
  fn has_sensors(self) -> bool {
    !matches!(self, Driver::AppleContainer | Driver::Host)
  }
}

/// The terminal dimension: how the driving program meets a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
  /// No terminal: `run create` alone, output in the actor's console.
  None,
  /// `run create -t`, then `run attach` later, detach, attach again.
  Later,
  /// `run create -it`: attached at once; the create returns the program's
  /// exit code.
  Now,
}

impl Terminal {
  pub fn name(self) -> &'static str {
    match self {
      Terminal::None => "none",
      Terminal::Later => "t",
      Terminal::Now => "it",
    }
  }
}

/// How a run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
  /// The driving program exits.
  Exit,
  /// `needy run stop`.
  Stop,
  /// `needy run stop --now`, when this needy has it.
  StopNow,
  /// The control daemon is killed with SIGKILL and started again on the
  /// same state: the restarted control plane reconciles the run.
  DaemonKill,
}

impl End {
  pub fn name(self) -> &'static str {
    match self {
      End::Exit => "exit",
      End::Stop => "stop",
      End::StopNow => "stop-now",
      End::DaemonKill => "daemon-kill",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Executor {
  Native,
  Container,
}

impl Executor {
  pub fn name(self) -> &'static str {
    match self {
      Executor::Native => "native",
      Executor::Container => "linux-container",
    }
  }

  pub fn platform(self) -> Platform {
    match self {
      Executor::Native => Platform::Any,
      // NOTICE: the executor's socket sits in a bind-mounted directory,
      // which works only where the container shares the daemon's kernel
      // (`crates/core-executor/src/driver/linux_container/mod.rs`).
      Executor::Container => Platform::Linux,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum User {
  /// `agent`, 1000:1000, as the base image has it.
  Default,
  /// `bob`, 1001:1001: a name and IDs the image does not have.
  Renamed,
}

impl User {
  pub fn name(self) -> &'static str {
    match self {
      User::Default => "default",
      User::Renamed => "renamed",
    }
  }

  pub fn account(self) -> (&'static str, u32) {
    match self {
      User::Default => ("agent", 1000),
      User::Renamed => ("bob", 1001),
    }
  }
}

/// Where the actor's workspace comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workspace {
  Executor(Executor),
  Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
  Exec,
  File,
  Net,
  Sensor,
  /// `needy chaos add` while the run is live.
  Added,
}

impl Fault {
  fn name(self) -> &'static str {
    match self {
      Fault::Exec => "exec",
      Fault::File => "file",
      Fault::Net => "net",
      Fault::Sensor => "sensor",
      Fault::Added => "chaos-add",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Net {
  Allow,
  Deny,
  Service,
}

impl Net {
  fn name(self) -> &'static str {
    match self {
      Net::Allow => "allow",
      Net::Deny => "deny",
      Net::Service => "service",
    }
  }
}

/// Where the runner of a runner cell runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerAt {
  /// A second needy on this host, `daemon --mode runner`, joined over the
  /// control socket.
  Local,
  /// A Windows machine over ssh (`--windows-runner`), joined through a
  /// reverse tunnel to the control daemon's loopback port.
  Windows,
}

impl RunnerAt {
  fn name(self) -> &'static str {
    match self {
      RunnerAt::Local => "local",
      RunnerAt::Windows => "windows",
    }
  }
}

/// What a cell exercises. Each scenario fixes the dimensions it does not
/// vary itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
  /// Routing to a native executor on a runner that joined the control
  /// plane, and `needy exec --runner`.
  Runner(RunnerAt),
  /// Copy-in from the host into the actor, nested and single-level; mirror-out
  /// from the actor with and without a filter.
  ActorFiles,
  /// Copy-in from the host into an executor, nested and single-level;
  /// mirror-out from it with and without a filter.
  ExecutorFiles,
  Workspace(Workspace),
  /// Routed and local placement, exit codes, a denied command.
  Routing,
  Chaos(Fault),
  Network(Net),
  /// `whoami`, `id`, `HOME` and `getent` in the actor and on an executor.
  Identity,
  /// The terminal modes with a program that reads a line.
  Terminal,
  /// What agents do without an account: nested config under the home,
  /// routed shells, input on a terminal, waiting to be stopped, and its
  /// login kept across two runs.
  FakeAgent,
  /// A TUI fidelity case from `super::tui`, by its index there.
  Tui(usize),
  /// `needy exec` outside any run: exit codes, a terminal, JSON output.
  HostExec,
  /// `needy fs cp` outside any run, into a nested directory and back.
  HostCopy,
}

/// One cell: a scenario with every dimension fixed.
#[derive(Debug, Clone)]
pub struct Cell {
  pub driver: Driver,
  pub scenario: Scenario,
  pub terminal: Terminal,
  pub end: End,
  /// The executor the scenario routes to, if it has one.
  pub executor: Option<Executor>,
  pub user: User,
  /// The daemon runs as root. No cell needs it today (see the design
  /// note); the field keeps the dimension visible in the output.
  // TODO(e2e-root): no cell runs its daemon as root. A cell that genuinely
  // needs root (an operator-only option, a privileged mount) sets this, and
  // the runner then needs `sudo -n`; add it with that cell, with the owner.
  pub root: bool,
}

impl Cell {
  fn new(driver: Driver, scenario: Scenario) -> Self {
    // Scenarios that route somewhere route to a native executor unless the
    // cell says otherwise; the others have none.
    let executor = match scenario {
      Scenario::Terminal | Scenario::Tui(_) | Scenario::Network(_) | Scenario::Workspace(Workspace::Host) => None,
      Scenario::Chaos(Fault::Net | Fault::Sensor) => None,
      _ => Some(Executor::Native),
    };
    Self {
      driver,
      scenario,
      terminal: Terminal::None,
      end: End::Exit,
      executor,
      user: User::Default,
      root: false,
    }
  }

  /// The cell's ID: `DRIVER/SCENARIO[/DIM=VALUE,...]`, what `--only` and
  /// `--skip` match.
  pub fn id(&self) -> String {
    let driver = self.driver.name();
    let executor = self.executor.map(Executor::name).unwrap_or("none");
    match self.scenario {
      Scenario::ActorFiles => format!("{driver}/actor-files/end={}", self.end.name()),
      Scenario::ExecutorFiles => format!("{driver}/executor-files/executor={executor},end={}", self.end.name()),
      Scenario::Workspace(Workspace::Executor(e)) => format!("{driver}/workspace/from=executor-{}", e.name()),
      Scenario::Workspace(Workspace::Host) => format!("{driver}/workspace/from=host"),
      Scenario::Routing => format!("{driver}/routing"),
      Scenario::Chaos(fault) => format!("{driver}/chaos/fault={}", fault.name()),
      Scenario::Network(net) => format!("{driver}/network/{}", net.name()),
      Scenario::Identity => format!("{driver}/identity/user={},executor={executor}", self.user.name()),
      Scenario::Terminal => format!("{driver}/terminal/mode={}", self.terminal.name()),
      Scenario::FakeAgent => format!("{driver}/fake-agent"),
      Scenario::Runner(at) => format!("{driver}/runner/at={}", at.name()),
      Scenario::Tui(i) => format!("{driver}/tui/{}", super::tui::cases().get(i).map(|c| c.name()).unwrap_or("?")),
      Scenario::HostExec => format!("{driver}/exec"),
      Scenario::HostCopy => format!("{driver}/fs-cp"),
    }
  }

  /// The file mappings the cell exercises, for the output's dimensions.
  pub fn files(&self) -> Vec<&'static str> {
    match self.scenario {
      Scenario::ActorFiles => vec![
        "copy-in host->actor nested (filtered)",
        "copy-in host->actor single-level",
        "mirror-out actor->host",
        "mirror-out actor->host filtered",
      ],
      Scenario::ExecutorFiles => vec![
        "copy-in host->executor nested (filtered)",
        "copy-in host->executor single-level",
        "mirror-out executor->host",
        "mirror-out executor->host filtered",
      ],
      Scenario::Workspace(Workspace::Executor(_)) => vec!["workspace from executor (mount)"],
      Scenario::Workspace(Workspace::Host) => vec!["workspace from host (copied in)"],
      Scenario::FakeAgent => vec![
        "copy-in host->actor nested (filtered)",
        "mirror-out actor->host filtered",
      ],
      Scenario::Runner(_) => vec![],
      Scenario::HostCopy => vec!["fs cp host->executor nested", "fs cp executor->host"],
      _ => vec![],
    }
  }

  pub fn chaos(&self) -> &'static str {
    match self.scenario {
      Scenario::Chaos(fault) => fault.name(),
      _ => "none",
    }
  }

  pub fn network(&self) -> &'static str {
    match self.scenario {
      Scenario::Network(net) => net.name(),
      Scenario::Chaos(Fault::Net) => "service",
      _ => "none",
    }
  }

  /// Where this cell can run, or why nowhere.
  /// Where this cell can run; `None` when its dimensions exclude each
  /// other (a macOS driver with a Linux-only executor), which [`cells`]
  /// never builds.
  pub fn platform(&self) -> Option<Platform> {
    let executor = self.executor.map(Executor::platform).unwrap_or(Platform::Any);
    self.driver.platform().and(executor)
  }

  /// Why the cell does not run on `here`, if it does not.
  pub fn elsewhere(&self, here: Platform) -> Option<String> {
    match self.platform() {
      Some(Platform::Any) => None,
      Some(platform) if platform == here => None,
      Some(platform) => Some(format!("runs on {} only", platform.name())),
      None => Some("its dimensions exclude each other".into()),
    }
  }

  /// Whether the actor's workspace is mounted from an executor, which a
  /// `linux-container` actor does with sshfs on the host.
  pub fn mounts_workspace(&self) -> bool {
    matches!(self.scenario, Scenario::Workspace(Workspace::Executor(_)) | Scenario::Chaos(Fault::File))
  }
}

impl fmt::Display for Cell {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.id())
  }
}

/// Every cell of the matrix, for every platform.
pub fn cells() -> Vec<Cell> {
  let mut cells = vec![
    Cell {
      executor: Some(Executor::Native),
      ..Cell::new(Driver::Host, Scenario::HostExec)
    },
    Cell::new(Driver::Host, Scenario::HostCopy),
  ];
  for driver in Driver::ACTORS {
    let cell = |scenario| Cell::new(driver, scenario);
    for end in [End::Exit, End::Stop, End::StopNow, End::DaemonKill] {
      cells.push(Cell {
        end,
        ..cell(Scenario::ActorFiles)
      });
    }
    for executor in [Executor::Native, Executor::Container] {
      for end in [End::Exit, End::Stop, End::DaemonKill] {
        cells.push(Cell {
          end,
          executor: Some(executor),
          ..cell(Scenario::ExecutorFiles)
        });
      }
    }
    for from in [
      Workspace::Executor(Executor::Native),
      Workspace::Executor(Executor::Container),
      Workspace::Host,
    ] {
      let executor = match from {
        Workspace::Executor(e) => Some(e),
        Workspace::Host => None,
      };
      cells.push(Cell {
        executor,
        ..cell(Scenario::Workspace(from))
      });
    }
    cells.push(cell(Scenario::Routing));
    for fault in [
      Fault::Exec,
      Fault::File,
      Fault::Net,
      Fault::Sensor,
      Fault::Added,
    ] {
      if fault == Fault::Sensor && !driver.has_sensors() {
        continue;
      }
      cells.push(cell(Scenario::Chaos(fault)));
    }
    for net in [Net::Allow, Net::Deny, Net::Service] {
      cells.push(cell(Scenario::Network(net)));
    }
    for user in [User::Default, User::Renamed] {
      for executor in [Executor::Native, Executor::Container] {
        cells.push(Cell {
          user,
          executor: Some(executor),
          ..cell(Scenario::Identity)
        });
      }
    }
    for terminal in [Terminal::Later, Terminal::Now] {
      cells.push(Cell {
        terminal,
        ..cell(Scenario::Terminal)
      });
    }
    cells.push(Cell {
      terminal: Terminal::Later,
      end: End::Stop,
      ..cell(Scenario::FakeAgent)
    });
    for at in [RunnerAt::Local, RunnerAt::Windows] {
      cells.push(cell(Scenario::Runner(at)));
    }
    for (i, _) in super::tui::cases().iter().enumerate() {
      cells.push(Cell {
        terminal: Terminal::Now,
        ..cell(Scenario::Tui(i))
      });
    }
  }
  // A macOS driver has no container executor: those combinations are not
  // cells at all, rather than cells that never run.
  cells.retain(|c| c.platform().is_some());
  cells
}

/// Whether `id` is selected by `--only` (any pattern, or none given) and
/// not by `--skip`. A pattern is a glob over the cell ID (`*` stays within
/// one `/` segment, `**` crosses them) or, without glob characters, a
/// substring of it.
pub fn selected(id: &str, only: &[String], skip: &[String]) -> bool {
  let hit = |pattern: &String| {
    if pattern.contains(['*', '?', '[', '{']) {
      glob_match::glob_match(pattern, id)
    } else {
      id.contains(pattern.as_str())
    }
  };
  (only.is_empty() || only.iter().any(hit)) && !skip.iter().any(hit)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_are_unique() {
    let cells = cells();
    let mut ids: Vec<String> = cells.iter().map(Cell::id).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate cell IDs");
  }

  #[test]
  fn every_actor_driver_covers_every_end_terminal_user_and_executor() {
    let cells = cells();
    for driver in Driver::ACTORS {
      let mine: Vec<&Cell> = cells.iter().filter(|c| c.driver == driver).collect();
      for end in [End::Exit, End::Stop, End::StopNow, End::DaemonKill] {
        assert!(mine.iter().any(|c| c.end == end), "{} lacks end {}", driver.name(), end.name());
      }
      for terminal in [Terminal::None, Terminal::Later, Terminal::Now] {
        assert!(mine.iter().any(|c| c.terminal == terminal), "{} lacks terminal {}", driver.name(), terminal.name());
      }
      for user in [User::Default, User::Renamed] {
        assert!(mine.iter().any(|c| c.user == user), "{} lacks user {}", driver.name(), user.name());
      }
      let executors: &[Executor] = match driver.platform() {
        Platform::Linux => &[Executor::Native, Executor::Container],
        _ => &[Executor::Native],
      };
      for executor in executors {
        assert!(mine.iter().any(|c| c.executor == Some(*executor)), "{} lacks executor {}", driver.name(), executor.name());
      }
      // The bugs of 2026-11-13 hid in these combinations.
      assert!(mine.iter().any(|c| c.scenario == Scenario::ActorFiles && c.end == End::Stop));
      assert!(mine.iter().any(|c| c.scenario == Scenario::ExecutorFiles && c.end == End::Stop));
      assert!(mine.iter().any(|c| c.scenario == Scenario::Identity && c.user == User::Renamed));
    }
  }

  #[test]
  fn container_executors_and_linux_drivers_are_linux_cells() {
    let cell = Cell {
      executor: Some(Executor::Container),
      ..Cell::new(Driver::Vz, Scenario::Identity)
    };
    // A macOS driver with a Linux-only executor runs nowhere, so it is not
    // a cell.
    assert_eq!(cell.platform(), None);
    assert!(!cells().iter().any(|c| c.driver == Driver::Vz && c.executor == Some(Executor::Container)));
    assert!(Cell::new(Driver::Firecracker, Scenario::Routing).elsewhere(Platform::Macos).is_some());
    assert!(Cell::new(Driver::Firecracker, Scenario::Routing).elsewhere(Platform::Linux).is_none());
    assert!(Cell::new(Driver::Host, Scenario::HostExec).elsewhere(Platform::Macos).is_none());
  }

  #[test]
  fn selection_takes_globs_and_substrings() {
    let id = "macos-vz/actor-files/end=stop";
    assert!(selected(id, &[], &[]));
    assert!(selected(id, &["macos-vz/**".into()], &[]));
    assert!(!selected(id, &["macos-vz/*".into()], &[]));
    assert!(selected(id, &["end=stop".into()], &[]));
    assert!(!selected(id, &[], &["actor-files".into()]));
    assert!(!selected(id, &["linux-*/**".into()], &[]));
  }
}
