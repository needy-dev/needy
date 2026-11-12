//! The driving program: started as the user, in its own session, with the
//! gateway as its HTTP proxy and the route stubs on its `PATH`.
//!
//! Without a terminal it runs with stdin `/dev/null` and its output on the
//! console. On a terminal it runs through the same process host as an
//! executor's commands, on a PTY that is its controlling terminal, and the
//! guest serves `ProcessService` for it on a vsock port, so a client
//! attaches as to any process: the current screen drawn from the terminal
//! model, resize, signals, detach. Init itself is the process's first
//! client: it copies the output to the console, so `needy run logs` and the
//! trace keep working while no client is attached.

use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use needy_api_proto::v1::GetConfigResponse;
use needy_api_proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
use needy_api_proto::v1::{ExecRequest, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse};
use needy_common::process::Exit;
use needy_core_executor::process::{Host, Output, Spec, Subscriber};
use needy_core_executor::service::ExecutorService;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};

use super::identity;
use super::network::GATEWAY_LISTEN;
use super::stubs::{STUB_DIR, SYSTEM_PATH};

/// Runs the driving program until it exits.
pub(super) async fn run(config: &GetConfigResponse, home: &str) -> Result<ExitStatus> {
  if config.command.is_empty() {
    bail!("no driving program configured");
  }
  let user = config.user.clone().unwrap_or_default();
  identity::offset_uptime(config.uptime_offset_secs);
  let proxy = format!("http://{GATEWAY_LISTEN}");
  // In a container the program's file may be a shim; its real file starts
  // it, so the driving program itself is never routed.
  let mut command = match crate::guest::capture::shim::resolve(&config.command[0], &format!("{SYSTEM_PATH}:{STUB_DIR}")) {
    Some(real) => {
      let mut command = tokio::process::Command::new(real);
      command.arg0(&config.command[0]);
      command
    }
    None => tokio::process::Command::new(&config.command[0]),
  };
  command
    .args(&config.command[1..])
    .env_clear()
    .env("HOME", home)
    .env("USER", &user.name)
    .env("LOGNAME", &user.name)
    .env("SHELL", "/bin/bash")
    .env("PATH", format!("{SYSTEM_PATH}:{STUB_DIR}"))
    .env("TERM", "xterm-256color")
    .env("LANG", "C.UTF-8")
    .env("HTTP_PROXY", &proxy)
    .env("HTTPS_PROXY", &proxy)
    .env("http_proxy", &proxy)
    .env("https_proxy", &proxy)
    .env("NO_PROXY", "localhost,127.0.0.1")
    .env("no_proxy", "localhost,127.0.0.1")
    .envs(&config.env)
    // The daemon sends the default working directory; the home directory
    // is the one that always exists.
    .current_dir(if config.cwd.is_empty() {
      home.to_string()
    } else {
      config.cwd.clone()
    })
    .stdin(Stdio::null())
    .uid(user.uid)
    .gid(user.gid);
  // SAFETY: `setsid` is async-signal-safe and allocates nothing.
  unsafe {
    command.pre_exec(|| {
      nix::unistd::setsid()?;
      Ok(())
    });
  }
  let mut child = command.spawn().context("cannot start the driving program")?;
  Ok(child.wait().await?)
}

/// The driving program's environment, as [`run`] sets it.
fn environment(config: &GetConfigResponse, home: &str) -> Vec<(String, String)> {
  let user = config.user.clone().unwrap_or_default();
  let proxy = format!("http://{GATEWAY_LISTEN}");
  let mut env: Vec<(String, String)> = [
    ("HOME", home.to_string()),
    ("USER", user.name.clone()),
    ("LOGNAME", user.name.clone()),
    ("SHELL", "/bin/bash".into()),
    ("PATH", format!("{SYSTEM_PATH}:{STUB_DIR}")),
    // The program draws for the terminal model, not for each client's
    // terminal (see the terminal note's G10).
    ("TERM", "xterm-256color".into()),
    ("LANG", "C.UTF-8".into()),
    ("HTTP_PROXY", proxy.clone()),
    ("HTTPS_PROXY", proxy.clone()),
    ("http_proxy", proxy.clone()),
    ("https_proxy", proxy),
    ("NO_PROXY", "localhost,127.0.0.1".into()),
    ("no_proxy", "localhost,127.0.0.1".into()),
  ]
  .into_iter()
  .map(|(k, v)| (k.to_string(), v))
  .collect();
  env.extend(config.env.iter().map(|(k, v)| (k.clone(), v.clone())));
  env
}

/// Where init serves the driving program's process service.
pub(super) enum Terminal {
  /// A vsock port (a VM's), which the host connects to.
  Vsock(u32),
  /// A Unix socket in the bind-mounted channels (a container's).
  Unix(std::path::PathBuf),
}

/// Runs the driving program on a terminal of `rows` by `cols` until it
/// exits, serving `ProcessService` for it on `terminal`.
pub(super) async fn run_on_terminal(config: &GetConfigResponse, home: &str, (rows, cols): (u16, u16), terminal: Terminal) -> Result<Exit> {
  if config.command.is_empty() {
    bail!("no driving program configured");
  }
  let user = config.user.clone().unwrap_or_default();
  let init = std::env::current_exe().context("cannot find init's own path")?;
  // A separate socket gates startup, leaving the PTY's input entirely to
  // the user. Only an immediately attached create waits; `-t` without
  // `-i` starts normally even when no client ever attaches.
  let gate = if config.wait_for_attach {
    let path = std::env::temp_dir().join(format!("needy-driver-{}.sock", std::process::id()));
    let listener = tokio::net::UnixListener::bind(&path).context("cannot bind the driving program's start gate")?;
    Some((path, listener))
  } else {
    None
  };
  let gate_path = gate.as_ref().map(|(path, _)| path.to_string_lossy().into_owned()).unwrap_or_default();
  // Init, started again as a plain process, switches to the user and runs
  // the program (see `as_user`): the process host starts programs as
  // itself, and the terminal must be the program's controlling terminal.
  let mut argv = vec![
    init.display().to_string(),
    crate::contract::INIT_ARG.into(),
    AS_USER.into(),
    user.uid.to_string(),
    user.gid.to_string(),
    gate_path,
    "--".into(),
  ];
  argv.extend(config.command.iter().cloned());
  let spec = Spec {
    argv,
    cwd: if config.cwd.is_empty() {
      home.to_string()
    } else {
      config.cwd.clone()
    },
    env: environment(config, home),
    clear_env: true,
    tty: Some((rows.max(1), cols.max(1))),
    no_stdin: false,
    run_id: config.run_id.clone(),
    search_path_if_missing: false,
  };
  let host = Host::default();
  // The time namespace's offset applies to the next process this thread
  // starts; nothing awaits between the two.
  identity::offset_uptime(config.uptime_offset_secs);
  let proc = host.spawn(spec).context("cannot start the driving program")?;
  // Subscribed before anything else runs, with the replay buffer, so the
  // console gets the output from its first byte. As the process's owner,
  // init holds it back rather than lose console output.
  let mut console = proc.subscribe(true, Subscriber::Owner);
  let start = Arc::new(Notify::new());
  if let Some((path, listener)) = gate {
    let start = start.clone();
    // Like the process service, this task belongs to the VM's lifetime.
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
  let stop = CancellationToken::new();
  let shutdown = stop.clone();
  let router = tonic::transport::Server::builder().add_service(ProcessServiceServer::new(service));
  let mut server = match terminal {
    Terminal::Vsock(port) => {
      let listener = tokio_vsock::VsockListener::bind(tokio_vsock::VsockAddr::new(libc::VMADDR_CID_ANY, port))
        .with_context(|| format!("cannot listen on vsock port {port}"))?;
      tokio::spawn(async move {
        if let Err(e) = router.serve_with_incoming_shutdown(listener.incoming(), shutdown.cancelled_owned()).await {
          tracing::error!("the driving program's process service: {e}");
        }
      })
    }
    Terminal::Unix(path) => {
      // A socket left by an earlier start of this container would make
      // bind fail.
      if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
      {
        tracing::warn!("cannot remove {}: {e}", path.display());
      }
      let listener = tokio::net::UnixListener::bind(&path).with_context(|| format!("cannot listen on {}", path.display()))?;
      let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);
      tokio::spawn(async move {
        if let Err(e) = router.serve_with_incoming_shutdown(incoming, shutdown.cancelled_owned()).await {
          tracing::error!("the driving program's process service: {e}");
        }
      })
    }
  };
  let mut stdout = tokio::io::stdout();
  let mut events: std::collections::VecDeque<Output> = std::mem::take(&mut console.past).into();
  let exit = loop {
    let event = match events.pop_front() {
      Some(event) => event,
      None => match console.live.recv().await {
        Some(event) => event,
        None => break proc.wait().await,
      },
    };
    match event {
      Output::Stdout(bytes) | Output::Stderr(bytes) => {
        // The console is the log; a write it refuses loses only the log.
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
  // Drain active gRPC streams before init reports the exit and reboots.
  // NOTICE: 3 s bounds abandoned clients; they must not keep an exited
  // driving program's VM alive indefinitely.
  stop.cancel();
  match tokio::time::timeout(Duration::from_secs(3), &mut server).await {
    Ok(Ok(())) => {}
    Ok(Err(e)) => tracing::warn!("the driving program's terminal server failed: {e}"),
    Err(_) => {
      tracing::debug!("the driving program's terminal clients did not drain within 3 s");
      server.abort();
    }
  }
  host.shutdown().await;
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

/// The argument that makes init, started as a plain process, switch to a
/// user and run a program: `init --as-user UID GID GATE -- PROGRAM ARGS…`.
pub(super) const AS_USER: &str = "--as-user";

/// Switches to the user `args` name and runs the program after `--`; it
/// returns only when that fails.
pub(super) fn as_user(args: &[String]) -> anyhow::Error {
  let switch = || -> Result<std::convert::Infallible> {
    let [uid, gid, gate, dashes, program, ..] = args else {
      bail!("usage: init {AS_USER} UID GID GATE -- PROGRAM [ARGS]");
    };
    if dashes != "--" {
      bail!("usage: init {AS_USER} UID GID GATE -- PROGRAM [ARGS]");
    }
    if !gate.is_empty() {
      use std::io::Read;
      let mut gate = std::os::unix::net::UnixStream::connect(gate).context("cannot connect to the start gate")?;
      let mut ready = [0];
      gate.read_exact(&mut ready).context("the start gate ended before an attach subscribed")?;
    }
    let uid = nix::unistd::Uid::from_raw(uid.parse().context("uid")?);
    let gid = nix::unistd::Gid::from_raw(gid.parse().context("gid")?);
    // The order matters: groups and the group while still root, then the
    // user, as `std::process::Command::uid` does.
    nix::unistd::setgroups(&[]).context("setgroups")?;
    nix::unistd::setgid(gid).context("setgid")?;
    nix::unistd::setuid(uid).context("setuid")?;
    let argv: Vec<std::ffi::CString> = args[4..].iter().map(|a| std::ffi::CString::new(a.as_str())).collect::<Result<_, _>>()?;
    // In a container the program's file may be a shim; its real file
    // starts it, so the driving program itself is never routed.
    let search = std::env::var("PATH").unwrap_or_default();
    if let Some(real) = crate::guest::capture::shim::resolve(program, &search) {
      use std::os::unix::ffi::OsStrExt;
      let real = std::ffi::CString::new(real.as_os_str().as_bytes())?;
      return Ok(nix::unistd::execv(&real, &argv)?);
    }
    let program = std::ffi::CString::new(program.as_str())?;
    Ok(nix::unistd::execvp(&program, &argv)?)
  };
  match switch() {
    Ok(never) => match never {},
    Err(e) => e,
  }
}
