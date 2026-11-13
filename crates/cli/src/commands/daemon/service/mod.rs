//! OS-managed control services. The manager owns process identity; discovery
//! supplies a location and health correlates that location with the manager PID.

mod launchd;
mod systemd;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::Context;
use crate::output::{self, OutputFormat};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Scope {
  User,
  System,
}

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(clap::Args)]
struct Selection {
  #[arg(long, value_enum, default_value = "user")]
  scope: Scope,
}

#[derive(clap::Subcommand)]
enum Command {
  Install(InstallArgs),
  Uninstall {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, default_value = "60s", value_parser = humantime::parse_duration)]
    timeout: Duration,
  },
  Start {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
    timeout: Duration,
  },
  Stop {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, default_value = "60s", value_parser = humantime::parse_duration)]
    timeout: Duration,
  },
  Restart {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, default_value = "60s", value_parser = humantime::parse_duration)]
    timeout: Duration,
  },
  Status {
    #[command(flatten)]
    selection: Selection,
    #[arg(short, long, value_enum, default_value = "table")]
    output: OutputFormat,
  },
  Logs {
    #[command(flatten)]
    selection: Selection,
    #[arg(short, long)]
    follow: bool,
    #[arg(long, default_value_t = 100)]
    tail: usize,
  },
}

#[derive(clap::Args)]
struct InstallArgs {
  #[command(flatten)]
  selection: Selection,
  #[arg(long)]
  now: bool,
  #[arg(long)]
  replace: bool,
  #[arg(long)]
  run_as: Option<String>,
  #[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
  timeout: Duration,
}

#[derive(Clone, Copy)]
pub(super) enum Action {
  Enable,
  Disable,
  Start,
  Kickstart,
  Stop,
  Reload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct State {
  pub state: String,
  pub pid: Option<u32>,
  pub loaded: bool,
}

/// Each implementation lives in its own file and depends only on the common
/// service data. No adapter calls another adapter.
pub(super) trait Manager {
  fn name(&self) -> &'static str;
  fn definition(&self) -> Result<PathBuf>;
  fn render(&self, registration: &Registration) -> Result<String>;
  fn command(&self, action: Action, registration: &Registration) -> tokio::process::Command;
  fn inspect(&self, registration: &Registration) -> tokio::process::Command;
  fn parse_state(&self, output: &str) -> Result<State>;
  fn inspect_binding(&self, registration: &Registration) -> tokio::process::Command;
  fn owns_binding(&self, registration: &Registration, output: &str) -> bool;
  fn logs(&self, registration: &Registration, follow: bool, tail: usize) -> tokio::process::Command;
}

fn manager(scope: Scope) -> Result<Box<dyn Manager>> {
  match std::env::consts::OS {
    "linux" => Ok(Box::new(systemd::Manager::new(scope))),
    "macos" => Ok(Box::new(launchd::Manager::new(scope))),
    platform => Err(
      crate::diagnostics::Error::new("platform.unsupported", "Control service management is not supported on this platform yet")
        .fact("platform", platform)
        .check("Use a Linux or macOS control host.")
        .into(),
    ),
  }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Registration {
  pub version: u32,
  pub executable: PathBuf,
  pub snapshot: PathBuf,
  pub definition: PathBuf,
  pub runtime: PathBuf,
  pub state: PathBuf,
  pub data: PathBuf,
  pub configuration: PathBuf,
  pub account: String,
  pub uid: u32,
  pub scope: Scope,
  pub path: String,
  pub engine_host: Option<String>,
  pub log: PathBuf,
}

const MARKER: &str = "Managed by needy control service v1";

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  // Select the platform before resolving paths or making installation writes.
  let scope = match &args.command {
    Command::Install(args) => args.selection.scope,
    Command::Uninstall { selection, .. }
    | Command::Start { selection, .. }
    | Command::Stop { selection, .. }
    | Command::Restart { selection, .. }
    | Command::Status { selection, .. }
    | Command::Logs { selection, .. } => selection.scope,
  };
  let manager = manager(scope)?;
  let paths = ctx.paths()?;
  let directory = paths.config.join("services").join(match scope {
    Scope::User => "user",
    Scope::System => "system",
  });
  let receipt = directory.join("registration.json");
  if let Command::Install(options) = args.command {
    install(ctx, manager.as_ref(), &directory, &receipt, options).await?;
    return Ok(0);
  }
  let missing = || {
    crate::diagnostics::Error::new("service.not_installed", "The local control service is not installed")
      .fact("manager", manager.name())
      .fact(
        "scope",
        match scope {
          Scope::User => "user",
          Scope::System => "system",
        },
      )
      .check("Run needy daemon service install for the selected scope, then start it; use --now to install and start together.")
  };
  let _lock = if matches!(args.command, Command::Start { .. } | Command::Stop { .. } | Command::Restart { .. } | Command::Uninstall { .. }) {
    // If the directory is absent, return without reading a receipt that could
    // be published by a concurrent first installation after this check.
    if !directory.try_exists()? {
      return Err(missing().into());
    }
    Some(lock(&manager.definition()?)?)
  } else {
    None
  };
  let registration = match std::fs::read(&receipt) {
    Ok(bytes) => Some(serde_json::from_slice::<Registration>(&bytes).context("invalid owned service registration")?),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
    Err(error) => return Err(error.into()),
  };
  if let Command::Status { output, .. } = args.command {
    let row = match registration {
      Some(registration) => status(manager.as_ref(), &registration).await?,
      None => Status {
        state: "not-installed".into(),
        manager: manager.name().into(),
        account: String::new(),
        pid: None,
        instance: None,
        admission: "unknown".into(),
        log: String::new(),
      },
    };
    output::print(output, &[row])?;
    return Ok(0);
  }
  let registration = registration.ok_or_else(missing)?;
  verify_owned(&registration)?;
  match args.command {
    Command::Start { timeout, .. } => start(manager.as_ref(), &registration, Instant::now() + timeout).await?,
    Command::Stop { timeout, .. } => stop(manager.as_ref(), &registration, Instant::now() + timeout).await?,
    Command::Restart { timeout, .. } => {
      let deadline = Instant::now() + timeout;
      stop(manager.as_ref(), &registration, deadline).await?;
      start(manager.as_ref(), &registration, deadline).await?;
    }
    Command::Uninstall { timeout, .. } => {
      let deadline = Instant::now() + timeout;
      stop(manager.as_ref(), &registration, deadline).await?;
      execute(manager.command(Action::Disable, &registration), deadline).await?;
      std::fs::remove_file(&registration.definition)?;
      execute(manager.command(Action::Reload, &registration), deadline).await?;
      std::fs::remove_file(&receipt)?;
      // Configuration, credentials, snapshots, logs, state and images remain.
    }
    Command::Logs { follow, tail, .. } => {
      let status = manager.logs(&registration, follow, tail).status().await.context("cannot read service logs")?;
      if !status.success() {
        bail!("service log command failed: {status}");
      }
    }
    Command::Install(_) | Command::Status { .. } => unreachable!(),
  }
  Ok(0)
}

async fn execute(mut command: tokio::process::Command, deadline: Instant) -> Result<String> {
  command.kill_on_drop(true);
  let output = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), command.output())
    .await
    .map_err(|error| {
      crate::diagnostics::Error::new("connection.timeout", "Service operation exceeded its total timeout")
        .check("Inspect local service status and logs.")
        .cause(error)
    })?
    .map_err(|error| {
      crate::diagnostics::Error::new("service.manager_unavailable", "Cannot contact the selected service manager")
        .check("Check the selected user/system scope and manager availability.")
        .cause(error)
    })?;
  if !output.status.success() {
    return Err(
      crate::diagnostics::Error::new("service.manager_failed", "The service manager rejected the operation")
        .fact("exit", output.status.to_string())
        .fact("cause", String::from_utf8_lossy(&output.stderr).trim())
        .check("Inspect local service status and the selected scope.")
        .into(),
    );
  }
  Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn inspect(manager: &dyn Manager, registration: &Registration, deadline: Instant) -> Result<State> {
  let state = manager.parse_state(&execute(manager.inspect(registration), deadline).await?)?;
  if state.loaded {
    let binding = execute(manager.inspect_binding(registration), deadline).await?;
    if !manager.owns_binding(registration, &binding) {
      return Err(
        crate::diagnostics::Error::new(
          "service.definition_conflict",
          "The manager service identity is bound to a different definition or executable",
        )
        .check("Inspect the canonical manager registration; preserve the foreign service.")
        .into(),
      );
    }
  }
  Ok(state)
}

async fn stop(manager: &dyn Manager, registration: &Registration, deadline: Instant) -> Result<()> {
  let state = inspect(manager, registration, deadline).await?;
  if state.pid.is_none() && state.state != "starting" {
    return Ok(());
  }
  #[cfg(unix)]
  let expected = needy_daemon::discovery::read(&registration.runtime).ok().filter(|record| Some(record.pid) == state.pid);
  execute(manager.command(Action::Stop, registration), deadline).await?;
  loop {
    let state = inspect(manager, registration, deadline).await?;
    if state.pid.is_none() && state.state != "starting" {
      #[cfg(unix)]
      if let Some(expected) = &expected
        && needy_daemon::discovery::read_exit(&registration.runtime)
          .is_ok_and(|result| result.pid == expected.pid && result.instance_id == expected.instance_id && result.complete)
      {
        return Ok(());
      }
      return Err(
        crate::diagnostics::Error::new("service.cleanup_incomplete", "The service stopped without a verified successful cleanup result")
          .check("Inspect retained run state and service logs; do not assume mirror-out completed.")
          .into(),
      );
    }
    if Instant::now() >= deadline {
      return Err(
        crate::diagnostics::Error::new("service.stop_timeout", "The service did not stop within its total budget")
          .check("Inspect service status and logs before taking any further action.")
          .into(),
      );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn start(manager: &dyn Manager, registration: &Registration, deadline: Instant) -> Result<()> {
  let initial = inspect(manager, registration, deadline).await?;
  if initial.pid.is_none() {
    let action = if initial.loaded && manager.name() == "launchd" {
      Action::Kickstart
    } else {
      Action::Start
    };
    execute(manager.command(action, registration), deadline).await?;
  }
  loop {
    let before = inspect(manager, registration, deadline).await?;
    if before.state == "failed" {
      return Err(
        crate::diagnostics::Error::new("service.start_failed", "The manager reports a failed service")
          .check("Inspect needy daemon service logs.")
          .into(),
      );
    }
    if let Some(pid) = before.pid {
      #[cfg(unix)]
      if let Ok(record) = needy_daemon::discovery::read(&registration.runtime)
        && record.pid == pid
      {
        let remaining = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        if needy_api_client::ready::wait_expected(&record.address, remaining, "control", &record.instance_id).await.is_ok() {
          let after = inspect(manager, registration, deadline).await?;
          if before == after
            && needy_daemon::discovery::read(&registration.runtime)
              .is_ok_and(|after| after.pid == record.pid && after.instance_id == record.instance_id)
          {
            return Ok(());
          }
        }
      }
    }
    if Instant::now() >= deadline {
      return Err(
        crate::diagnostics::Error::new("connection.timeout", "The managed service did not publish a matching ready control instance")
          .check("Inspect service status and logs.")
          .into(),
      );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

#[derive(Serialize)]
struct Status {
  state: String,
  manager: String,
  account: String,
  pid: Option<u32>,
  instance: Option<String>,
  admission: String,
  log: String,
}
impl output::Row for Status {
  const HEADER: &'static [&'static str] = &[
    "STATE",
    "MANAGER",
    "ACCOUNT",
    "PID",
    "INSTANCE",
    "ADMISSION",
    "LOG",
  ];
  fn cells(&self) -> Vec<String> {
    vec![
      self.state.clone(),
      self.manager.clone(),
      self.account.clone(),
      self.pid.map(|v| v.to_string()).unwrap_or_default(),
      self.instance.clone().unwrap_or_default(),
      self.admission.clone(),
      self.log.clone(),
    ]
  }
}

async fn status(manager: &dyn Manager, registration: &Registration) -> Result<Status> {
  verify_owned(registration)?;
  let before = inspect(manager, registration, Instant::now() + Duration::from_secs(5)).await?;
  let mut row = Status {
    state: before.state.clone(),
    manager: manager.name().into(),
    account: registration.account.clone(),
    pid: before.pid,
    instance: None,
    admission: "unknown".into(),
    log: registration.log.display().to_string(),
  };
  #[cfg(unix)]
  if let Some(pid) = before.pid {
    row.state = "api-unreachable".into();
    if let Ok(record) = needy_daemon::discovery::read(&registration.runtime)
      && record.pid == pid
    {
      row.instance = Some(record.instance_id.clone());
      if needy_api_client::ready::wait_expected(&record.address, Duration::from_secs(1), "control", &record.instance_id).await.is_ok()
        && inspect(manager, registration, Instant::now() + Duration::from_secs(5)).await? == before
        && needy_daemon::discovery::read(&registration.runtime)
          .is_ok_and(|after| after.pid == record.pid && after.instance_id == record.instance_id)
      {
        row.state = "ready".into();
        // Readiness is deliberately separate from authenticated admission.
      }
    }
  }
  Ok(row)
}

fn lock(definition: &Path) -> Result<std::fs::File> {
  if let Some(parent) = definition.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let mut options = std::fs::OpenOptions::new();
  options.create(true).truncate(false).read(true).write(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
  }
  // The canonical definition is shared by all client configuration files.
  // Keep this lock inode after uninstall: unlinking it would let a concurrent
  // installer lock a new inode while an older transaction still owns this one.
  let mut name = definition.as_os_str().to_os_string();
  name.push(".lock");
  let file = options.open(PathBuf::from(name))?;
  file.try_lock().map_err(|cause| {
    crate::diagnostics::Error::new("service.definition_conflict", "Another service operation owns the installation lock")
      .check("Wait for the current service operation to finish, then inspect status before retrying.")
      .cause(cause)
  })?;
  Ok(file)
}

fn verify_owned(registration: &Registration) -> Result<()> {
  let text = std::fs::read_to_string(&registration.definition).context("owned service definition is missing")?;
  if registration.version != 1 || text != manager(registration.scope)?.render(registration)? {
    return Err(
      crate::diagnostics::Error::new("service.definition_conflict", "Service definition conflicts with the owned registration")
        .check("Inspect the definition and registration; preserve foreign services.")
        .into(),
    );
  }
  Ok(())
}

async fn install(ctx: &Context, manager: &dyn Manager, directory: &Path, receipt: &Path, options: InstallArgs) -> Result<()> {
  let InstallArgs {
    selection: Selection { scope },
    run_as,
    replace,
    now,
    timeout,
  } = options;
  let deadline = Instant::now() + timeout;
  if matches!(scope, Scope::User) && run_as.is_some() {
    bail!("--run-as is only valid with --scope system");
  }
  if matches!(scope, Scope::System) && run_as.is_none() {
    bail!("--scope system requires --run-as USER");
  }
  let (account, uid, gid) = account(run_as.as_deref())?;
  let definition = manager.definition()?;
  // One transaction covers ownership inspection, stop of an older generation,
  // and publication. Competing mutations fail instead of interleaving receipts.
  #[cfg(unix)]
  needy_api_server::create_private_dir(directory)?;
  let _lock = lock(&definition)?;
  let previous = match std::fs::read(receipt) {
    Ok(bytes) => Some(serde_json::from_slice::<Registration>(&bytes).context("invalid service registration")?),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
    Err(error) => return Err(error.into()),
  };
  if let Some(previous) = &previous {
    verify_owned(previous)?;
  }
  if std::fs::symlink_metadata(&definition).is_ok() && previous.is_none() {
    bail!("a foreign service definition already exists; refusing to replace it");
  }
  if previous.is_some() && !replace {
    bail!("service is already installed; use --replace to snapshot new settings");
  }
  if previous.is_none() && replace {
    bail!("--replace requires an existing owned installation");
  }
  let mut config = needy_config::global::Config {
    daemon: ctx.configuration()?.daemon,
    ..Default::default()
  };
  let mut paths = ctx.paths()?;
  let cwd = std::env::current_dir()?;
  for path in [
    &mut paths.config,
    &mut paths.state,
    &mut paths.data,
    &mut paths.runtime,
  ] {
    if !path.is_absolute() {
      *path = cwd.join(&*path);
    }
  }
  let snapshot_dir = if matches!(scope, Scope::System) {
    let root = if cfg!(target_os = "macos") {
      PathBuf::from("/Library/Application Support/Needy")
    } else {
      PathBuf::from("/var/lib/needy")
    };
    paths.state = root.join("state");
    paths.data = root.join("data");
    paths.runtime = root.join("runtime");
    root.join("service")
  } else {
    directory.to_path_buf()
  };
  let snapshot = snapshot_dir.join(format!("control-{}.yaml", uuid::Uuid::new_v4()));
  config.paths.state = Some(paths.state.clone());
  config.paths.data = Some(paths.data.clone());
  config.paths.runtime = Some(paths.runtime.clone());
  for path in [
    &mut config.daemon.actor_image,
    &mut config.daemon.guest_needy,
  ] {
    if let Some(value) = path
      && !value.is_absolute()
    {
      *value = cwd.join(&*value);
    }
  }
  let entries: Vec<PathBuf> = match &config.daemon.process_path {
    Some(entries) => entries.clone(),
    None => std::env::split_paths(&std::env::var_os("PATH").context("PATH is absent; set daemon.process_path before installing")?)
      .map(|path| {
        if path.is_absolute() {
          path
        } else {
          cwd.join(path)
        }
      })
      .collect(),
  };
  let process_path = std::env::join_paths(&entries)
    .context("invalid daemon.process_path")?
    .into_string()
    .map_err(|_| anyhow::anyhow!("service PATH must be valid UTF-8"))?;
  config.daemon.process_path = Some(entries);
  let registration = Registration {
    version: 1,
    executable: std::fs::canonicalize(std::env::current_exe()?)?,
    snapshot,
    definition,
    runtime: paths.runtime.clone(),
    state: paths.state.clone(),
    data: paths.data.clone(),
    configuration: paths.config.clone(),
    account,
    uid,
    scope,
    path: process_path,
    engine_host: config.daemon.engine_host.clone().or_else(|| std::env::var("DOCKER_HOST").ok()),
    log: paths.state.join("service.log"),
  };
  verify_executable(&registration.executable, uid, gid)?;
  // Confirm that the selected manager/domain is available before writes.
  let current = inspect(manager, &registration, deadline).await?;
  if previous.is_none() && current.loaded {
    return Err(
      crate::diagnostics::Error::new("service.definition_conflict", "The canonical service identity is already registered with the manager")
        .check("Inspect the existing manager registration; it will not be adopted or replaced.")
        .into(),
    );
  }
  if let Some(previous) = &previous {
    stop(manager, previous, deadline).await?;
  }
  for path in [&paths.state, &paths.data, &paths.runtime, &snapshot_dir] {
    prepare_directory(path, uid, gid)?;
  }
  if let Some(parent) = registration.definition.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::create_dir_all(directory)?;
  write_new(&registration.snapshot, serde_yaml::to_string(&config)?.as_bytes(), 0o600)?;
  #[cfg(unix)]
  if matches!(scope, Scope::System) {
    std::os::unix::fs::chown(&registration.snapshot, Some(uid), Some(gid))?;
  }
  let definition = manager.render(&registration)?;
  write_atomic(&registration.definition, definition.as_bytes(), 0o644)?;
  write_atomic(receipt, &serde_json::to_vec_pretty(&registration)?, 0o600)?;
  execute(manager.command(Action::Reload, &registration), deadline).await?;
  execute(manager.command(Action::Enable, &registration), deadline).await?;
  if now {
    start(manager, &registration, deadline).await?;
  }
  output::outln!("Installed {} control service ({scope:?}); configuration snapshot: {}", manager.name(), registration.snapshot.display())?;
  Ok(())
}

#[cfg(unix)]
fn account(run_as: Option<&str>) -> Result<(String, u32, u32)> {
  // Account lookup uses libc's reentrant API so neither process-global passwd
  // storage nor environment-derived account names determine the runtime user.
  let mut buffer = vec![0u8; 65536];
  let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
  let mut found = std::ptr::null_mut();
  // SAFETY: every pointer is backed by writable storage valid for the call;
  // getpw*_r stores string pointers in `buffer`, which lives through copying.
  let result = unsafe {
    match run_as {
      Some(name) => {
        if libc::geteuid() != 0 {
          bail!("system service installation requires root; no privilege elevation is attempted");
        }
        let name = std::ffi::CString::new(name).context("runtime account contains NUL")?;
        libc::getpwnam_r(name.as_ptr(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut found)
      }
      None => libc::getpwuid_r(libc::geteuid(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut found),
    }
  };
  if result != 0 || found.is_null() {
    bail!("cannot resolve the runtime account");
  }
  // SAFETY: successful getpw*_r initialized entry and its name in buffer.
  let entry = unsafe { entry.assume_init() };
  let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }.to_str()?.to_string();
  Ok((name, entry.pw_uid, entry.pw_gid))
}
#[cfg(not(unix))]
fn account(_: Option<&str>) -> Result<(String, u32, u32)> {
  bail!("control service management is not supported on Windows yet")
}

fn prepare_directory(path: &Path, uid: u32, gid: u32) -> Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let existed = path.exists();
    let missing: Vec<_> = path.ancestors().take_while(|path| !path.exists()).map(Path::to_path_buf).collect();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    for created in missing {
      std::os::unix::fs::chown(created, Some(uid), Some(gid))?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
      bail!("service directory must be a directory: {}", path.display());
    }
    if existed && (metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0) {
      bail!("service directory must already belong to the runtime account and be private: {}", path.display());
    }
    if !existed {
      std::os::unix::fs::chown(path, Some(uid), Some(gid))?;
    }
  }
  #[cfg(not(unix))]
  {
    let _ = (path, uid, gid);
    bail!("control services are unsupported on Windows yet");
  }
  Ok(())
}
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
  use std::io::Write as _;
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(mode);
  }
  #[cfg(not(unix))]
  let _ = mode;
  let mut file = options.open(path)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  Ok(())
}
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
  let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
  write_new(&temporary, bytes, mode)?;
  std::fs::rename(&temporary, path)?;
  Ok(())
}

fn verify_executable(path: &Path, uid: u32, gid: u32) -> Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    for entry in path.ancestors() {
      let metadata = std::fs::metadata(entry)?;
      let execute = if metadata.uid() == uid {
        0o100
      } else if metadata.gid() == gid {
        0o010
      } else {
        0o001
      };
      if uid != 0 && metadata.mode() & execute == 0 {
        return Err(
          crate::diagnostics::Error::new("service.executable_inaccessible", "The runtime account cannot execute the selected binary")
            .fact("path", entry.display().to_string())
            .check("Install needy at a stable location accessible to the runtime account, then install the service again.")
            .into(),
        );
      }
    }
  }
  Ok(())
}
