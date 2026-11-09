//! Inside an actor microVM: the init process and the exec-routing proxy.
//!
//! The kernel's `binfmt_needy` hands routed execs to `/.needy/proxy`, which is
//! this binary. Both talk to the daemon over vsock (host CID 2).

#[cfg(target_os = "linux")]
pub use linux::{init_main, proxy_main};

#[cfg(not(target_os = "linux"))]
pub fn init_main() -> ! {
  eprintln!("needy: the guest init runs only inside a Linux actor");
  std::process::exit(1)
}

#[cfg(not(target_os = "linux"))]
pub fn proxy_main() -> ! {
  eprintln!("needy: the guest proxy runs only inside a Linux actor");
  std::process::exit(1)
}

#[cfg(target_os = "linux")]
mod linux {
  use std::collections::HashMap;
  use std::ffi::{CStr, CString};
  use std::time::Duration;

  use anyhow::{Context, Result, bail};
  use needy_api_client::Client;
  use needy_api_proto::v1::{
    DecideRequest, ExecRequest, ExecStart, FetchFilesRequest, GetConfigRequest, GetConfigResponse, GuestMount, PushEventsRequest,
    PushFilesRequest, ReportExitRequest, TraceEvent, exec_request, exec_response,
  };
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::sync::mpsc;
  use tokio_stream::wrappers::ReceiverStream;

  use needy_config::profile::FaultKind;
  use needy_core::event::Event;
  use needy_core::types::{Effect, Fired, Origin};
  use needy_core_executor::client as exec_cli;

  const DAEMON: &str = "vsock://2:7000";
  const STUB_DIR: &str = "/run/needy/bin";
  const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
  const GATEWAY_LISTEN: &str = "127.0.0.1:3128";
  /// Environment the actor passes to routed commands when rules do not.
  const PASS_ENV: &[&str] = &[
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "COLORTERM",
    "NO_COLOR",
  ];

  fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
  }

  async fn daemon() -> Result<Client> {
    let mut last = None;
    for _ in 0..50 {
      match Client::connect(DAEMON).await {
        Ok(client) => return Ok(client),
        Err(e) => last = Some(e),
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("cannot reach the daemon over vsock: {:?}", last)
  }

  // ---------------------------------------------------------------- proxy

  /// Entry point for a routed exec. argv is the original command's argv.
  pub fn proxy_main() -> ! {
    let code = runtime().block_on(proxy()).unwrap_or_else(|e| {
      eprintln!("needy: {e:#}");
      126
    });
    std::process::exit(code)
  }

  fn exec_path() -> String {
    let p = unsafe { libc::getauxval(libc::AT_EXECFN) } as *const libc::c_char;
    if p.is_null() {
      return std::env::args().next().unwrap_or_default();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
  }

  async fn proxy() -> Result<i32> {
    let path = exec_path();
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut argv = args.clone();
    if let Some(first) = argv.first_mut() {
      *first = path.clone();
    }
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let env: HashMap<String, String> = std::env::vars().collect();
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let client = daemon().await?;
    let decision = client
      .sessions()
      .decide(DecideRequest {
        session_id: String::new(),
        argv: argv.clone(),
        cwd: cwd.clone(),
        env: env.clone(),
        tty,
      })
      .await
      .map_err(|s| anyhow::anyhow!("{}", s.message()))?
      .into_inner();
    if !decision.deny.is_empty() {
      // The refused command's own error output, worded like `needy exec`'s.
      eprintln!("needy: {}", decision.deny);
      return Ok(126);
    }
    if decision.target == "actor" || decision.target.is_empty() {
      exec_local(&path, &args);
    }
    let mut pass: HashMap<String, String> = PASS_ENV.iter().filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
    pass.extend(decision.env);
    let start = ExecStart {
      argv: if decision.argv.is_empty() {
        args
      } else {
        decision.argv
      },
      cwd: if decision.cwd.is_empty() {
        cwd
      } else {
        decision.cwd
      },
      env: pass,
      clear_env: decision.clear_env,
      tty: tty.then(exec_cli::window),
      detach: false,
      session_id: String::new(),
      executor: decision.target,
      no_stdin: false,
      origin: Origin::ActorProxy.as_str().into(),
    };
    let first = ExecRequest {
      payload: Some(exec_request::Payload::Start(start)),
    };
    let exit = exec_cli::run_stream(client, first, tty, true).await?;
    if let Some(code) = exit.code {
      return Ok(code);
    }
    // Die of the same signal so the parent's wait sees signal death.
    if let Some(sig) = needy_core_executor::signals::number(&exit.signal) {
      unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::kill(libc::getpid(), sig);
      }
    }
    Ok(needy_core_executor::signals::shell_code(&exit))
  }

  /// Runs the real binary here. The kernel does not route execs made by the proxy.
  fn exec_local(path: &str, args: &[String]) -> ! {
    let cpath = CString::new(path).unwrap_or_default();
    let cargs: Vec<CString> = args.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();
    let mut ptrs: Vec<*const libc::c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    unsafe { libc::execv(cpath.as_ptr(), ptrs.as_ptr()) };
    eprintln!("needy: cannot run {path}: {}", std::io::Error::last_os_error());
    std::process::exit(127)
  }

  // ----------------------------------------------------------------- init

  pub fn init_main() -> ! {
    if let Err(e) = runtime().block_on(init()) {
      eprintln!("needy init: {e:#}");
    }
    unsafe {
      libc::sync();
      libc::reboot(libc::RB_AUTOBOOT);
    }
    loop {
      std::thread::sleep(Duration::from_secs(60));
    }
  }

  fn mount(source: &str, target: &str, fstype: &str, flags: libc::c_ulong, data: &str) {
    let _ = std::fs::create_dir_all(target);
    let (s, t, f, d) =
      (CString::new(source).unwrap(), CString::new(target).unwrap(), CString::new(fstype).unwrap(), CString::new(data).unwrap());
    let rc = unsafe { libc::mount(s.as_ptr(), t.as_ptr(), f.as_ptr(), flags, d.as_ptr() as *const libc::c_void) };
    if rc != 0 {
      eprintln!("needy init: mount {fstype} on {target}: {}", std::io::Error::last_os_error());
    }
  }

  fn write(path: &str, value: &str) {
    if let Err(e) = std::fs::write(path, value) {
      eprintln!("needy init: write {path}: {e}");
    }
  }

  fn loopback_up() {
    unsafe {
      let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
      if fd < 0 {
        return;
      }
      let mut req: libc::ifreq = std::mem::zeroed();
      for (i, b) in b"lo\0".iter().enumerate() {
        req.ifr_name[i] = *b as libc::c_char;
      }
      if libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut req) == 0 {
        req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &req);
      }
      libc::close(fd);
    }
  }

  async fn init() -> Result<()> {
    let nosuid = libc::MS_NOSUID | libc::MS_NODEV;
    mount("proc", "/proc", "proc", nosuid | libc::MS_NOEXEC, "");
    mount("sysfs", "/sys", "sysfs", nosuid | libc::MS_NOEXEC, "");
    mount("devpts", "/dev/pts", "devpts", libc::MS_NOSUID, "mode=620,ptmxmode=666");
    mount("tmpfs", "/dev/shm", "tmpfs", nosuid, "mode=1777");
    mount("tmpfs", "/tmp", "tmpfs", nosuid, "mode=1777");
    mount("tmpfs", "/run", "tmpfs", nosuid, "mode=755");
    mount("tmpfs", "/var/tmp", "tmpfs", nosuid, "mode=1777");
    loopback_up();
    write("/proc/sys/vm/memfd_noexec", "2");

    let client = daemon().await?;
    let config = client.guest().get_config(GetConfigRequest {}).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?.into_inner();
    let user = config.user.clone().unwrap_or_default();
    let home = format!("/home/{}", user.name);
    mount("tmpfs", &home, "tmpfs", nosuid, &format!("mode=755,uid={},gid={}", user.uid, user.gid));
    if !config.hostname.is_empty() {
      let name = CString::new(config.hostname.clone()).unwrap();
      unsafe { libc::sethostname(name.as_ptr(), config.hostname.len()) };
    }
    for sensor in &config.sensors {
      if sensor.chip == "coretemp" {
        write("/sys/module/fake_coretemp/parameters/base_mc", &sensor.idle_mc.to_string());
      }
    }
    for m in &config.mounts {
      if let Err(e) = mount_sftp(&client, m, user.uid, user.gid).await {
        eprintln!("needy init: mount {}: {e:#}", m.target);
      }
    }
    tokio::spawn(gateway_forwarder(config.gateway_port));
    create_stubs(&config.route_names);
    for copy in &config.copy_in {
      if let Err(e) = copy_in(&client, &copy.name, &copy.path, user.uid, user.gid).await {
        eprintln!("needy init: copy-in {}: {e:#}", copy.name);
      }
    }
    for fault in config.hwmon_faults.clone() {
      let client = client.clone();
      let session = config.session_id.clone();
      tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(fault.at_ms.max(0) as u64)).await;
        write("/sys/module/fake_coretemp/parameters/override_mc", &fault.temp_mc.to_string());
        let fired = Event::FaultInjected(Fired {
          kind: FaultKind::Hwmon,
          target: "coretemp".into(),
          effect: Effect {
            temp_mc: Some(fault.temp_mc),
            ..Effect::default()
          },
        });
        let event = TraceEvent {
          session_id: session,
          time_ms: needy_core::trace::now_ms(),
          kind: fired.kind().as_str().into(),
          // TODO(proto): `TraceEvent` still carries a string map; the typed
          // payload travels as its `payload` entry until slice 7a gives the
          // message a payload field.
          attrs: [("payload".to_string(), fired.payload())].into(),
        };
        if let Err(status) = client.guest().push_events(tokio_stream::iter(vec![PushEventsRequest { event: Some(event) }])).await {
          eprintln!("needy init: cannot record the hwmon fault: {}", status.message());
        }
      });
    }
    // Route only after setup so init's own helpers stay local.
    write("/sys/module/binfmt_needy/parameters/route_uid", &user.uid.to_string());
    let names = if config.route_names.is_empty() {
      String::new()
    } else {
      config.route_names.join(",")
    };
    write("/sys/module/binfmt_needy/parameters/route", &names);

    let exit = run_driver(&config, &home).await;
    let (code, signal) = match exit {
      Ok(status) => {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal().map(needy_core_executor::signals::name).unwrap_or_default())
      }
      Err(e) => {
        eprintln!("needy init: driving program: {e:#}");
        (Some(127), String::new())
      }
    };
    for copy in &config.mirror_out {
      if let Err(e) = mirror_out(&client, &copy.name, &copy.path).await {
        eprintln!("needy init: mirror-out {}: {e:#}", copy.name);
      }
    }
    let _ = client.guest().report_exit(ReportExitRequest { code, signal }).await;
    // Detach FUSE mounts before the streams behind them close.
    for m in &config.mounts {
      let target = CString::new(m.target.trim_end_matches('/')).unwrap_or_default();
      unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    }
    Ok(())
  }

  /// Mounts an executor directory with sshfs in passive mode; SFTP travels
  /// over a needy exec stream to `sftp-server` on the executor.
  async fn mount_sftp(client: &Client, m: &GuestMount, uid: u32, gid: u32) -> Result<()> {
    std::fs::create_dir_all(&m.target)?;
    let mut options = format!("passive,allow_other,uid={uid},gid={gid},idmap=none,cache=yes,kernel_cache,attr_timeout=1,entry_timeout=1");
    if !m.writable {
      options.push_str(",ro");
    }
    let mut sshfs = tokio::process::Command::new("sshfs")
      .args(["-f", "-o", &options, &format!(":{}", m.source), &m.target])
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::inherit())
      .spawn()
      .context("cannot start sshfs")?;
    let mut sshfs_in = sshfs.stdin.take().context("sshfs stdin")?;
    let mut sshfs_out = sshfs.stdout.take().context("sshfs stdout")?;
    let (tx, rx) = mpsc::channel::<ExecRequest>(64);
    tx.send(ExecRequest {
      payload: Some(exec_request::Payload::Start(ExecStart {
        argv: m.sftp_command.clone(),
        executor: m.executor.clone(),
        // The mount root lets file faults match paths relative to it.
        cwd: m.source.clone(),
        origin: Origin::ActorMount.as_str().into(),
        ..Default::default()
      })),
    })
    .await?;
    let mut responses = client.processes().exec(ReceiverStream::new(rx)).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?.into_inner();
    tokio::spawn(async move {
      let mut buf = vec![0u8; 256 * 1024];
      loop {
        match sshfs_out.read(&mut buf).await {
          Ok(0) | Err(_) => break,
          Ok(n) => {
            if tx
              .send(ExecRequest {
                payload: Some(exec_request::Payload::Stdin(buf[..n].to_vec())),
              })
              .await
              .is_err()
            {
              break;
            }
          }
        }
      }
    });
    tokio::spawn(async move {
      while let Ok(Some(message)) = responses.message().await {
        match message.payload {
          Some(exec_response::Payload::Stdout(bytes)) => {
            if sshfs_in.write_all(&bytes).await.is_err() {
              break;
            }
            let _ = sshfs_in.flush().await;
          }
          Some(exec_response::Payload::Exited(_)) | Some(exec_response::Payload::Error(_)) => break,
          _ => {}
        }
      }
    });
    tokio::spawn(async move {
      let _ = sshfs.wait().await;
    });
    let target = m.target.trim_end_matches('/');
    for _ in 0..100 {
      let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
      if mounts.lines().any(|l| l.split(' ').nth(1) == Some(target)) {
        return Ok(());
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("sshfs did not mount within 10 s")
  }

  /// Fetches a copy-in mapping from the host and unpacks it, owned by the user.
  async fn copy_in(client: &Client, name: &str, path: &str, uid: u32, gid: u32) -> Result<()> {
    let mut stream = client
      .guest()
      .fetch_files(FetchFilesRequest { name: name.into() })
      .await
      .map_err(|s| anyhow::anyhow!("{}", s.message()))?
      .into_inner();
    let mut archive = Vec::new();
    while let Some(chunk) = stream.message().await.map_err(|s| anyhow::anyhow!("{}", s.message()))? {
      archive.extend(chunk.data);
    }
    let root = std::path::PathBuf::from(path);
    needy_common::archive::unpack(&archive, &root)?;
    chown_tree(&root, uid, gid);
    Ok(())
  }

  fn chown_tree(path: &std::path::Path, uid: u32, gid: u32) {
    let _ = std::os::unix::fs::lchown(path, Some(uid), Some(gid));
    if let Ok(entries) = std::fs::read_dir(path) {
      for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() && !p.is_symlink() {
          chown_tree(&p, uid, gid);
        } else {
          let _ = std::os::unix::fs::lchown(&p, Some(uid), Some(gid));
        }
      }
    }
  }

  /// Packs a directory and sends it to the host for a mirror-out mapping.
  async fn mirror_out(client: &Client, name: &str, path: &str) -> Result<()> {
    let root = std::path::PathBuf::from(path);
    if !root.exists() {
      return Ok(());
    }
    let archive = needy_common::archive::pack(&root, &[])?;
    let mut messages = Vec::new();
    for (i, chunk) in archive.chunks(1 << 20).enumerate() {
      messages.push(PushFilesRequest {
        name: if i == 0 { name.into() } else { String::new() },
        data: chunk.to_vec(),
      });
    }
    if messages.is_empty() {
      messages.push(PushFilesRequest {
        name: name.into(),
        data: Vec::new(),
      });
    }
    client.guest().push_files(tokio_stream::iter(messages)).await.map_err(|s| anyhow::anyhow!("{}", s.message()))?;
    Ok(())
  }

  /// The kernel can only route a file that exists. Routed commands the actor
  /// does not have get empty placeholder files on the driving program's PATH.
  fn create_stubs(names: &[String]) {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::path::Path::new(STUB_DIR);
    let _ = std::fs::create_dir_all(dir);
    for name in names {
      if name.contains('/') || name == "*" || name.is_empty() {
        continue;
      }
      let present = SYSTEM_PATH.split(':').any(|d| std::path::Path::new(d).join(name).exists());
      if present {
        continue;
      }
      let stub = dir.join(name);
      if std::fs::write(&stub, b"").is_ok() {
        let _ = std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755));
      }
    }
  }

  /// Forwards the in-guest proxy port to the actor gateway on the host.
  async fn gateway_forwarder(port: u32) {
    if port == 0 {
      return;
    }
    let Ok(listener) = tokio::net::TcpListener::bind(GATEWAY_LISTEN).await else {
      eprintln!("needy init: cannot listen on {GATEWAY_LISTEN}");
      return;
    };
    while let Ok((mut inbound, _)) = listener.accept().await {
      tokio::spawn(async move {
        if let Ok(mut outbound) = tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(2, port)).await {
          let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        }
      });
    }
  }

  async fn run_driver(config: &GetConfigResponse, home: &str) -> Result<std::process::ExitStatus> {
    if config.command.is_empty() {
      bail!("no driving program configured");
    }
    let user = config.user.clone().unwrap_or_default();
    if config.uptime_offset_secs != 0 {
      // Children of init enter a time namespace whose boot time is earlier.
      if unsafe { libc::unshare(libc::CLONE_NEWTIME) } == 0 {
        write("/proc/self/timens_offsets", &format!("monotonic {0} 0\nboottime {0} 0\n", config.uptime_offset_secs));
      }
    }
    let proxy = format!("http://{GATEWAY_LISTEN}");
    let mut command = tokio::process::Command::new(&config.command[0]);
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
      .current_dir(if config.cwd.is_empty() {
        format!("{home}/workspace")
      } else {
        config.cwd.clone()
      })
      .stdin(std::process::Stdio::null())
      .uid(user.uid)
      .gid(user.gid);
    unsafe {
      command.pre_exec(|| {
        libc::setsid();
        Ok(())
      });
    }
    let mut child = command.spawn().context("cannot start the driving program")?;
    Ok(child.wait().await?)
  }
}
