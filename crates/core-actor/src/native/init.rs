//! A native actor's init: this binary started by the runtime as `needy
//! __needy-macos-init DIR`, next to the daemon and outside the fence. In
//! order: it asks the daemon for the run's configuration on the guest
//! socket, makes the actor's directories and shims, writes the route list,
//! forwards a loopback port to the gateway socket, writes the fence, serves
//! the kept-command service, starts reading the fence's denials, runs the
//! driving program under `sandbox-exec`, reports its exit and exits, which
//! ends the actor.
//!
//! Init logs to its stderr, which the runtime writes to the actor's
//! `console.log`.
//!
//! NOTICE: the tasks init spawns (the forwarder, the kept-command and
//! terminal services, the denial reader) are owned by the actor: they end
//! when init exits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use needy_api_proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
use needy_api_proto::v1::{
  ExecRequest, GetConfigRequest, GetConfigResponse, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse,
  ReportExitRequest,
};
use needy_core_executor::process::{Host, Output, Spec, Subscriber};
use needy_core_executor::service::ExecutorService;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};

use super::Route;
use super::kept::{self, Kept};
use crate::host::macos::fence::Fence;
use crate::host::macos::{GATE, Layout, SHIM_MARKER};

/// Where the actor looks for programs when its profile sets no `PATH`:
/// the system's directories, then Homebrew's.
// NOTICE: `/usr/bin:/bin:/usr/sbin:/sbin` is `/etc/paths` on macOS 27.2;
// `/usr/local/bin` and `/opt/homebrew/bin` are Homebrew's prefixes on Intel
// and Apple silicon (https://docs.brew.sh/Installation, read 2026-11-12).
const SEARCH: &str = "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin";

/// What a fenced program may not read, under the user's home: keys and the
/// credentials of common tools, and needy's own.
const HIDDEN: &[&str] = &[
  ".ssh",
  ".gnupg",
  ".aws",
  ".azure",
  ".kube",
  ".docker",
  ".netrc",
  ".config/gh",
  ".config/gcloud",
  ".config/needy",
  "Library/Keychains",
];

pub(super) fn main(layout: Layout) -> ! {
  console_logging();
  let code = match super::runtime().block_on(boot(&layout)) {
    Ok(()) => 0,
    Err(e) => {
      tracing::error!("{e:#}");
      1
    }
  };
  std::process::exit(code)
}

fn console_logging() {
  let filter = tracing_subscriber::EnvFilter::try_from_env("NEEDY_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
  let installed = tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_ansi(false)
    .with_target(false)
    .with_timer(tracing_subscriber::fmt::time::uptime())
    .with_env_filter(filter)
    .try_init();
  if let Err(e) = installed {
    eprintln!("needy init: no console log: {e}");
  }
}

/// Everything init made for the driving program.
struct Actor {
  /// The program and its arguments, as `sandbox-exec` starts them.
  argv: Vec<String>,
  env: Vec<(String, String)>,
  cwd: PathBuf,
}

async fn boot(layout: &Layout) -> Result<()> {
  let started = Instant::now();
  let guest = format!("unix:{}", layout.guest().display());
  // NOTICE: the daemon binds the guest socket before it starts init; 5 s
  // only bounds a daemon that stopped meanwhile.
  let client =
    needy_api_client::ready::wait(&guest, Duration::from_secs(5)).await.with_context(|| format!("cannot reach the daemon at {guest}"))?;
  let config = client.guest().get_config(GetConfigRequest {}).await.map_err(needy_api_client::Error::from)?.into_inner();
  if config.command.is_empty() {
    bail!("no driving program configured");
  }
  if !config.copy_in.is_empty() || !config.mirror_out.is_empty() {
    // TODO(macos-native): copy-in and mirror-out into a native actor's
    // home are not done in the proof of concept; they are the guest's
    // `files` module over the same guest API.
    tracing::warn!("copy-in and mirror-out are not supported by a macos-native actor; skipped");
  }
  for dir in [layout.home(), layout.tmp(), layout.shims()] {
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
  }
  // Seatbelt matches real paths, so every path in the fence is one.
  let real = |path: &Path| std::fs::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()));
  let home = real(&layout.home())?;
  let tmp = real(&layout.tmp())?;
  let shims = real(&layout.shims())?;
  let search = config.env.get("PATH").cloned().unwrap_or_else(|| SEARCH.to_string());
  let user = config.user.clone().unwrap_or_default();
  let cwd = working_directory(&config.cwd, &user.name, &home);
  std::fs::create_dir_all(&cwd).with_context(|| format!("cannot create the working directory {}", cwd.display()))?;
  let cwd = real(&cwd)?;
  let exe = real(&std::env::current_exe().context("cannot find init's own path")?)?;
  let count = make_shims(&shims, &search, &exe)?;
  std::fs::write(
    layout.route(),
    Route {
      names: config.route_names.clone(),
    }
    .render(),
  )
  .context("cannot write the route list")?;

  let forwarder = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("cannot listen for the gateway on loopback")?;
  let gateway_port = forwarder.local_addr()?.port();
  tokio::spawn(forward_gateway(forwarder, layout.gateway()));

  let tag = format!("needy-{}", config.run_id);
  let user_home = std::env::var_os("HOME").map(PathBuf::from);
  let fence = Fence {
    tag: tag.clone(),
    shims: shims.clone(),
    shim: exe.clone(),
    gateway_port,
    sockets: [layout.guest(), layout.kept()].iter().map(|socket| real_socket(socket)).collect(),
    writable: vec![home.clone(), tmp.clone(), cwd.clone()],
    hidden: user_home.iter().flat_map(|h| HIDDEN.iter().map(move |p| h.join(p))).collect(),
  };
  std::fs::write(layout.fence(), fence.render()).context("cannot write the fence")?;
  let developer = developer_dir().await;

  let host = Host::default();
  serve_kept(layout, &host, &search, developer.clone())?;
  let mut denials = match super::denials::watch(&tag, client.clone()) {
    Ok(child) => Some(child),
    Err(e) => {
      tracing::warn!("cannot read the fence's denials; they are not traced: {e}");
      None
    }
  };

  let program = if config.command[0].contains('/') {
    real(Path::new(&config.command[0]))?
  } else {
    kept::program(&config.command[0], &search).with_context(|| format!("{}: not found on {search}", config.command[0]))?
  };
  let proxy = format!("http://127.0.0.1:{gateway_port}");
  let actor = Actor {
    argv: kept::command(&layout.fence(), &program, developer.as_deref(), config.command[1..].iter().cloned()),
    env: environment(&config, &home, &tmp, &shims, &proxy),
    cwd,
  };
  tracing::info!(
    "actor ready in {} ms: {count} shims, gateway on 127.0.0.1:{gateway_port}, driving program {}",
    started.elapsed().as_millis(),
    program.display()
  );
  let exit = match &config.tty {
    Some(window) => {
      let size = (u16::try_from(window.rows).unwrap_or(u16::MAX), u16::try_from(window.cols).unwrap_or(u16::MAX));
      run_on_terminal(layout, &config, &actor, size, &host).await.map(|exit| (exit.code, exit.signal))
    }
    None => run(layout, &actor).await,
  };
  let (code, signal) = match exit {
    Ok((code, signal)) => {
      tracing::info!("the driving program exited: code {code:?}, signal {signal:?}");
      (code, signal)
    }
    Err(e) => {
      tracing::error!("driving program: {e:#}");
      (Some(127), String::new())
    }
  };
  // NOTICE: the unified log hands a denial to `log stream` some tens of
  // milliseconds after the kernel wrote it (about 30 ms on macOS 27.2, from
  // init's own log of the bypass probes); 500 ms keeps the run's last
  // denials with margin.
  if denials.is_some() {
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
  if let Err(status) = client.guest().report_exit(ReportExitRequest { code, signal }).await {
    tracing::warn!("cannot report the driving program's exit: {}", status.message());
  }
  if let Some(child) = denials.as_mut()
    && let Err(e) = child.kill().await
  {
    tracing::debug!("cannot stop the denial reader: {e}");
  }
  host.shutdown().await;
  Ok(())
}

/// The real path of `socket`, which may not exist yet: its directory's,
/// joined with its name.
fn real_socket(socket: &Path) -> PathBuf {
  match (socket.parent().and_then(|dir| std::fs::canonicalize(dir).ok()), socket.file_name()) {
    (Some(dir), Some(name)) => dir.join(name),
    _ => socket.to_path_buf(),
  }
}

/// The driving program's working directory: the profile's, with the
/// profile user's `/home/NAME` read as the actor's home; the home when the
/// profile sets none.
fn working_directory(cwd: &str, user: &str, home: &Path) -> PathBuf {
  if cwd.is_empty() {
    return home.to_path_buf();
  }
  let profile_home = format!("/home/{user}");
  match Path::new(cwd).strip_prefix(&profile_home) {
    Ok(rest) => home.join(rest),
    Err(_) => PathBuf::from(cwd),
  }
}

/// Makes one shim, a link to `exe`, for every program name on `search`, in
/// `shims`; the first directory that has a name wins, as a `PATH` lookup
/// does. Returns how many it made.
fn make_shims(shims: &Path, search: &str, exe: &Path) -> Result<usize> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::write(shims.join(SHIM_MARKER), "").context("cannot mark the shims directory")?;
  let mut count = 0;
  for dir in search.split(':').filter(|d| !d.is_empty()) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let name = entry.file_name();
      let link = shims.join(&name);
      if link.symlink_metadata().is_ok() {
        continue;
      }
      let executable = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
      if !executable {
        continue;
      }
      std::os::unix::fs::symlink(exe, &link).with_context(|| format!("cannot make the shim {}", link.display()))?;
      count += 1;
    }
  }
  Ok(count)
}

/// The driving program's environment: its own home and temporary
/// directory, the shims alone on `PATH`, the gateway as its HTTP proxy, and
/// the profile's variables. `SHELL` is the shell's shim, so a program that
/// starts "the user's shell" by that path is routed, not refused.
// NOTICE: opencode's bash tool started `/bin/sh` by path with `SHELL` set
// to `/bin/sh`, which the fence refused (`EPERM: operation not permitted,
// posix_spawn '/bin/sh'`, opencode 1.19.3 on macOS 27.2).
fn environment(config: &GetConfigResponse, home: &Path, tmp: &Path, shims: &Path, proxy: &str) -> Vec<(String, String)> {
  let user = std::env::var("USER").unwrap_or_default();
  let shell = ["bash", "sh"]
    .iter()
    .map(|name| shims.join(name))
    .find(|shim| shim.symlink_metadata().is_ok())
    .unwrap_or_else(|| PathBuf::from("/bin/sh"));
  let mut env: HashMap<String, String> = [
    ("HOME", home.display().to_string()),
    ("TMPDIR", format!("{}/", tmp.display())),
    ("USER", user.clone()),
    ("LOGNAME", user),
    ("SHELL", shell.display().to_string()),
    ("PATH", shims.display().to_string()),
    ("TERM", "xterm-256color".into()),
    ("LANG", "en_US.UTF-8".into()),
    ("HTTP_PROXY", proxy.into()),
    ("HTTPS_PROXY", proxy.into()),
    ("http_proxy", proxy.into()),
    ("https_proxy", proxy.into()),
    ("NO_PROXY", "localhost,127.0.0.1".into()),
    ("no_proxy", "localhost,127.0.0.1".into()),
  ]
  .into_iter()
  .map(|(k, v)| (k.to_string(), v))
  .collect();
  // The profile's `PATH` is the search path the shims stand for.
  env.extend(config.env.iter().filter(|(k, _)| k.as_str() != "PATH").map(|(k, v)| (k.clone(), v.clone())));
  env.into_iter().collect()
}

/// The active developer directory, `xcode-select -p`.
async fn developer_dir() -> Option<PathBuf> {
  let output =
    tokio::process::Command::new("/usr/bin/xcode-select").arg("-p").stdin(Stdio::null()).stderr(Stdio::null()).output().await.ok()?;
  let path = String::from_utf8(output.stdout).ok()?;
  let path = PathBuf::from(path.trim());
  (output.status.success() && path.is_dir()).then_some(path)
}

/// Serves the kept-command service on its socket.
fn serve_kept(layout: &Layout, host: &Host, search: &str, developer: Option<PathBuf>) -> Result<()> {
  let socket = layout.kept();
  if let Err(e) = std::fs::remove_file(&socket)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", socket.display());
  }
  let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
  let service = Kept {
    processes: ExecutorService {
      host: host.clone(),
      name: "actor".into(),
    },
    fence: layout.fence(),
    search: search.to_string(),
    developer,
  };
  let router = tonic::transport::Server::builder().add_service(ProcessServiceServer::new(service));
  tokio::spawn(async move {
    if let Err(e) = router.serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener)).await {
      tracing::error!("the kept-command service: {e}");
    }
  });
  Ok(())
}

/// Forwards each connection to `listener` to the gateway's socket.
async fn forward_gateway(listener: tokio::net::TcpListener, gateway: PathBuf) {
  loop {
    let mut inbound = match listener.accept().await {
      Ok((inbound, _)) => inbound,
      Err(e) => {
        tracing::warn!("the gateway forwarder stops: {e}");
        return;
      }
    };
    let gateway = gateway.clone();
    tokio::spawn(async move {
      match tokio::net::UnixStream::connect(&gateway).await {
        Ok(mut outbound) => {
          if let Err(e) = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await {
            tracing::debug!("gateway connection: {e}");
          }
        }
        Err(e) => tracing::warn!("cannot reach the gateway at {}: {e}", gateway.display()),
      }
    });
  }
}

/// Runs the driving program without a terminal until it exits, or until
/// init is asked to stop, which ends its process group.
async fn run(layout: &Layout, actor: &Actor) -> Result<(Option<i32>, String)> {
  use std::os::unix::process::ExitStatusExt;
  let mut command = tokio::process::Command::new(&actor.argv[0]);
  command.args(&actor.argv[1..]).env_clear().envs(actor.env.iter().cloned()).current_dir(&actor.cwd).stdin(Stdio::null()).process_group(0);
  let mut child = command.spawn().context("cannot start the driving program")?;
  let pid = child.id().unwrap_or_default();
  write_pid(layout, pid);
  let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).context("cannot watch for SIGTERM")?;
  let status = tokio::select! {
    status = child.wait() => status?,
    _ = term.recv() => {
      end_group(pid);
      child.wait().await?
    }
  };
  Ok((status.code(), status.signal().map(needy_core_executor::signals::name).unwrap_or_default()))
}

fn write_pid(layout: &Layout, pid: u32) {
  if let Err(e) = std::fs::write(layout.driver_pid(), pid.to_string()) {
    tracing::warn!("cannot record the driving program's process group; a forced stop leaves it: {e}");
  }
}

/// `SIGTERM` to the process group `pid` leads.
fn end_group(pid: u32) {
  use nix::sys::signal::{Signal, killpg};
  if let Err(e) = killpg(nix::unistd::Pid::from_raw(pid as i32), Signal::SIGTERM) {
    tracing::debug!("cannot end the driving program's process group {pid}: {e}");
  }
}

/// Runs the driving program on a terminal of `rows` by `cols` until it
/// exits, serving `ProcessService` for it on the actor's process socket,
/// as a container's init does.
async fn run_on_terminal(
  layout: &Layout,
  config: &GetConfigResponse,
  actor: &Actor,
  (rows, cols): (u16, u16),
  host: &Host,
) -> Result<needy_common::process::Exit> {
  // A separate socket gates startup, leaving the PTY's input entirely to
  // the user; only an immediately attached create waits.
  let gate = if config.wait_for_attach {
    let path = layout.dir.join("actor").join("gate.sock");
    if let Err(e) = std::fs::remove_file(&path)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", path.display());
    }
    let listener = tokio::net::UnixListener::bind(&path).context("cannot bind the driving program's start gate")?;
    Some((path, listener))
  } else {
    None
  };
  let argv = match &gate {
    Some((path, _)) => {
      let exe = std::env::current_exe().context("cannot find init's own path")?;
      let mut argv = vec![
        exe.display().to_string(),
        GATE.into(),
        path.display().to_string(),
        "--".into(),
      ];
      argv.extend(actor.argv.iter().cloned());
      argv
    }
    None => actor.argv.clone(),
  };
  let spec = Spec {
    argv,
    cwd: actor.cwd.display().to_string(),
    env: actor.env.clone(),
    clear_env: true,
    tty: Some((rows.max(1), cols.max(1))),
    no_stdin: false,
    run_id: config.run_id.clone(),
    search_path_if_missing: false,
  };
  let proc = host.spawn(spec).context("cannot start the driving program")?;
  write_pid(layout, proc.pid as u32);
  let mut console = proc.subscribe(true, Subscriber::Owner);
  let start = Arc::new(Notify::new());
  if let Some((path, listener)) = gate {
    let start = start.clone();
    tokio::spawn(async move {
      let release = async {
        let (mut stream, _) = listener.accept().await?;
        start.notified().await;
        stream.write_all(&[1]).await
      }
      .await;
      if let Err(e) = release {
        tracing::error!("cannot release the driving program's start gate: {e}");
      }
      if let Err(e) = std::fs::remove_file(path) {
        tracing::debug!("cannot remove the driving program's start gate: {e}");
      }
    });
  }
  let service = TerminalService {
    processes: ExecutorService {
      host: host.clone(),
      name: "actor".into(),
    },
    start,
  };
  let socket = layout.process();
  if let Err(e) = std::fs::remove_file(&socket)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", socket.display());
  }
  let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
  let stop = CancellationToken::new();
  let shutdown = stop.clone();
  let router = tonic::transport::Server::builder().add_service(ProcessServiceServer::new(service));
  let mut server = tokio::spawn(async move {
    if let Err(e) =
      router.serve_with_incoming_shutdown(tokio_stream::wrappers::UnixListenerStream::new(listener), shutdown.cancelled_owned()).await
    {
      tracing::error!("the driving program's process service: {e}");
    }
  });
  let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).context("cannot watch for SIGTERM")?;
  let mut stdout = tokio::io::stdout();
  let mut events: std::collections::VecDeque<Output> = std::mem::take(&mut console.past).into();
  let exit = loop {
    let event = match events.pop_front() {
      Some(event) => event,
      None => tokio::select! {
        event = console.live.recv() => match event {
          Some(event) => event,
          None => break proc.wait().await,
        },
        _ = term.recv() => {
          end_group(proc.pid as u32);
          continue;
        }
      },
    };
    match event {
      Output::Stdout(bytes) | Output::Stderr(bytes) => {
        if let Err(e) = stdout.write_all(&bytes).await {
          tracing::debug!("console: {e}");
        }
      }
      Output::Exit(exit) => break exit,
    }
  };
  if let Err(e) = stdout.flush().await {
    tracing::debug!("console: {e}");
  }
  // NOTICE: 3 s bounds abandoned clients, as a container's init does.
  stop.cancel();
  match tokio::time::timeout(Duration::from_secs(3), &mut server).await {
    Ok(Ok(())) => {}
    Ok(Err(e)) => tracing::warn!("the driving program's terminal server failed: {e}"),
    Err(_) => {
      tracing::debug!("the driving program's terminal clients did not drain within 3 s");
      server.abort();
    }
  }
  Ok(exit)
}

/// Releases an interactive create after its first attach subscribed, so
/// even an immediately exiting program is observed.
struct TerminalService {
  processes: ExecutorService,
  start: Arc<Notify>,
}

#[tonic::async_trait]
impl ProcessService for TerminalService {
  type ExecStream = needy_core_executor::service::ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    let response = self.processes.exec(request).await?;
    self.start.notify_one();
    Ok(response)
  }

  async fn list_processes(&self, request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    self.processes.list_processes(request).await
  }

  async fn kill_process(&self, request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    self.processes.kill_process(request).await
  }
}

/// The start gate: `needy __needy-macos-gate SOCKET -- PROGRAM ARGS…` waits
/// until init releases it, then executes the program.
pub(super) fn gate(args: Vec<String>) -> ! {
  let error = (|| -> Result<std::convert::Infallible> {
    let [socket, dashes, program, ..] = args.as_slice() else {
      bail!("usage: needy {GATE} SOCKET -- PROGRAM [ARGS]");
    };
    if dashes != "--" {
      bail!("usage: needy {GATE} SOCKET -- PROGRAM [ARGS]");
    }
    use std::io::Read;
    let mut gate = std::os::unix::net::UnixStream::connect(socket).context("cannot connect to the start gate")?;
    let mut ready = [0];
    gate.read_exact(&mut ready).context("the start gate ended before an attach subscribed")?;
    let argv: Vec<std::ffi::CString> = args[2..].iter().map(|a| std::ffi::CString::new(a.as_str())).collect::<Result<_, _>>()?;
    let program = std::ffi::CString::new(program.as_str())?;
    Ok(nix::unistd::execv(&program, &argv)?)
  })();
  match error {
    Ok(never) => match never {},
    Err(e) => {
      eprintln!("needy: cannot run the driving program: {e:#}");
      std::process::exit(127)
    }
  }
}
