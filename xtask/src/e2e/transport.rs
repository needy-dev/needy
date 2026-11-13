//! The transport dimension: how a runner on this host reaches its cell's
//! control plane in a `runner/at=local` cell.
//!
//! - `direct`: the daemon's loopback TCP port.
//! - `ssh-l`: an `ssh -L` forward to that port, through an ssh destination
//!   that reaches this host (local settings).
//! - `idle-proxy`: [`LinkProxy`], in this process, which drops connections
//!   that carried no byte for [`PROXY_IDLE`] and can cut every connection
//!   at once, as a tunnel edge that restarts does. Always available.
//! - `cloudflare`: a Cloudflare Tunnel connector (`cloudflared tunnel run`)
//!   in front of the daemon, and the runner joining through Cloudflare
//!   Access with an `exec:cloudflared access tcp` endpoint and a service
//!   token (local settings).
//!
//! The settings for `ssh-l` and `cloudflare` name real machines and
//! accounts, so they live in a local file that is never part of the
//! repository: `transports.json` in the harness's work directory, or the
//! file `NEEDY_E2E_TRANSPORTS` names. Secrets stay in the environment; the
//! file names the variables that hold them.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// How a runner reaches the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Transport {
  Direct,
  SshL,
  IdleProxy,
  Cloudflare,
}

impl Transport {
  pub const ALL: [Transport; 4] = [
    Transport::Direct,
    Transport::SshL,
    Transport::IdleProxy,
    Transport::Cloudflare,
  ];

  pub fn name(self) -> &'static str {
    match self {
      Transport::Direct => "direct",
      Transport::SshL => "ssh-l",
      Transport::IdleProxy => "idle-proxy",
      Transport::Cloudflare => "cloudflare",
    }
  }

  /// Whether the harness can drop every connection of this transport at
  /// once, to check that a runner and a workspace mount come back.
  pub fn can_cut(self) -> bool {
    !matches!(self, Transport::Direct)
  }
}

/// How long [`LinkProxy`] lets a connection carry nothing before it drops
/// it in an `idle-proxy` cell.
// NOTICE: longer than needy's HTTP/2 keepalive pings (20 s, client and
// server: `needy_api_client::KEEP_ALIVE`, `needy_api_server::server`), so a
// connection that needy keeps alive survives, and one it does not is cut.
// Cloudflare's own limits are longer (Proxy Idle Timeout 900 s, Proxy Read
// Timeout 125 s on proxied HTTP), and Access TCP held a connection idle for
// 900 s on 2026-11-13; this is the tighter middlebox the dimension stands
// for (a NAT or a corporate proxy).
// Reference: https://developers.cloudflare.com/fundamentals/reference/connection-limits/
// (page dated 2026-10-22, retrieved 2026-11-13).
pub const PROXY_IDLE: Duration = Duration::from_secs(30);

/// The local settings of the transports that need outside machines.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
  pub ssh: Option<SshSettings>,
  pub cloudflare: Option<CloudflareSettings>,
}

/// `ssh -L`: a destination whose sshd forwards to this host's loopback.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshSettings {
  /// An ssh destination that reaches this host, such as `localhost`.
  pub destination: String,
}

/// A Cloudflare Tunnel whose public hostname forwards TCP to
/// `127.0.0.1:origin_port` here, behind an Access application with a
/// service token.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudflareSettings {
  /// The tunnel's TCP hostname, as `cloudflared access tcp --hostname`
  /// takes it.
  pub hostname: String,
  /// The port the tunnel's ingress forwards to; the cell's daemon listens
  /// there, so these cells run one at a time.
  pub origin_port: u16,
  /// The variables holding the tunnel's run token and the service token.
  pub tunnel_token_env: String,
  pub client_id_env: String,
  pub client_secret_env: String,
  /// `cloudflared`, if it is not on `PATH`.
  #[serde(default)]
  pub cloudflared: Option<String>,
}

impl Settings {
  /// Reads the settings: `NEEDY_E2E_TRANSPORTS`, else `transports.json` in
  /// `work`; none when neither exists.
  pub fn load(work: &Path) -> Result<Settings> {
    let path = std::env::var_os("NEEDY_E2E_TRANSPORTS").map(Into::into).unwrap_or_else(|| work.join("transports.json"));
    match std::fs::read_to_string(&path) {
      Ok(text) => serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display())),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
      Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
  }

  /// Why `transport` cannot run here, if it cannot.
  pub fn blocker(&self, transport: Transport) -> Option<String> {
    match transport {
      Transport::Direct | Transport::IdleProxy => None,
      Transport::SshL => match &self.ssh {
        None => Some("no ssh destination: set `ssh.destination` in transports.json (see the e2e harness design)".into()),
        Some(_) if !super::proc::on_path("ssh") => Some("no ssh on PATH".into()),
        Some(_) => None,
      },
      Transport::Cloudflare => match &self.cloudflare {
        None => Some("no Cloudflare tunnel: set `cloudflare` in transports.json (see the e2e harness design)".into()),
        Some(cf) => {
          let missing: Vec<&str> = [
            &cf.tunnel_token_env,
            &cf.client_id_env,
            &cf.client_secret_env,
          ]
          .into_iter()
          .filter(|v| std::env::var_os(v.as_str()).is_none())
          .map(String::as_str)
          .collect();
          let cloudflared = cf.cloudflared.as_deref().unwrap_or("cloudflared");
          if !missing.is_empty() {
            Some(format!("the Cloudflare settings name variables that are not set: {}", missing.join(", ")))
          } else if !super::proc::on_path(cloudflared) && !Path::new(cloudflared).is_file() {
            Some(format!("no {cloudflared}"))
          } else {
            None
          }
        }
      },
    }
  }
}

/// One cell's transport, running: the endpoint its runner joins, the
/// environment the runner needs, and the processes or proxy behind it.
pub struct Link {
  pub transport: Transport,
  pub endpoint: String,
  /// Added to the runner's environment (the service token for `cloudflared
  /// access tcp`).
  pub env: Vec<(String, String)>,
  proxy: Option<LinkProxy>,
  /// `ssh -L` or `cloudflared tunnel run`, and how to start it again.
  child: Option<Child>,
  restart: Option<Box<dyn Fn() -> Result<Child> + Send + Sync>>,
}

impl Link {
  /// Starts `transport` in front of the daemon's loopback `port`.
  pub fn start(transport: Transport, settings: &Settings, port: u16, log: &Path) -> Result<Link> {
    let direct = format!("http://127.0.0.1:{port}");
    let mut link = Link {
      transport,
      endpoint: direct,
      env: Vec::new(),
      proxy: None,
      child: None,
      restart: None,
    };
    match transport {
      Transport::Direct => {}
      Transport::IdleProxy => {
        let proxy = LinkProxy::start(port, Some(PROXY_IDLE), Duration::ZERO)?;
        link.endpoint = format!("http://127.0.0.1:{}", proxy.port);
        link.proxy = Some(proxy);
      }
      Transport::SshL => {
        let ssh = settings.ssh.clone().context("no ssh settings")?;
        let local = free_port()?;
        let log = log.to_path_buf();
        let start = move || -> Result<Child> {
          let out = std::fs::File::options().create(true).append(true).open(&log)?;
          let forward = format!("{local}:127.0.0.1:{port}");
          let child = Command::new("ssh")
            .args([
              "-N",
              "-o",
              "BatchMode=yes",
              "-o",
              "ExitOnForwardFailure=yes",
              "-L",
              &forward,
              &ssh.destination,
            ])
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .context("cannot start ssh -L")?;
          wait_port(local, Duration::from_secs(20))?;
          Ok(child)
        };
        link.child = Some(start()?);
        link.restart = Some(Box::new(start));
        link.endpoint = format!("http://127.0.0.1:{local}");
      }
      Transport::Cloudflare => {
        let cf = settings.cloudflare.clone().context("no Cloudflare settings")?;
        if cf.origin_port != port {
          bail!("the daemon listens on {port}, the tunnel forwards to {}", cf.origin_port);
        }
        let cloudflared = cf.cloudflared.clone().unwrap_or_else(|| "cloudflared".into());
        let token = std::env::var(&cf.tunnel_token_env).with_context(|| format!("{} is not set", cf.tunnel_token_env))?;
        let log = log.to_path_buf();
        let program = cloudflared.clone();
        let start = move || -> Result<Child> {
          let out = std::fs::File::options().create(true).append(true).open(&log)?;
          let before = registrations(&log);
          // The run token goes in the environment, not the arguments.
          let child = Command::new(&program)
            .args(["tunnel", "--no-autoupdate", "run"])
            .env("TUNNEL_TOKEN", &token)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .context("cannot start cloudflared tunnel run")?;
          // A runner that joins before the connector registered gets
          // `websocket: bad handshake` from the edge, and a failed first join
          // ends it: wait for this connector's first registered connection.
          let deadline = Instant::now() + Duration::from_secs(60);
          while registrations(&log) <= before {
            if Instant::now() > deadline {
              bail!("cloudflared registered no tunnel connection within 60 s (see {})", log.display());
            }
            std::thread::sleep(Duration::from_millis(250));
          }
          Ok(child)
        };
        link.child = Some(start()?);
        link.restart = Some(Box::new(start));
        link.endpoint = format!("exec:{cloudflared} access tcp --hostname {}", cf.hostname);
        for (name, var) in [
          ("TUNNEL_SERVICE_TOKEN_ID", &cf.client_id_env),
          ("TUNNEL_SERVICE_TOKEN_SECRET", &cf.client_secret_env),
        ] {
          link.env.push((name.into(), std::env::var(var).with_context(|| format!("{var} is not set"))?));
        }
      }
    }
    Ok(link)
  }

  /// Drops every connection of the transport at once, as a tunnel whose
  /// edge or connector restarts does: the proxy closes them, an `ssh -L`
  /// or a tunnel connector is ended and started again.
  pub fn cut(&mut self) -> Result<()> {
    if let Some(proxy) = &self.proxy {
      proxy.cut();
    }
    if let (Some(child), Some(restart)) = (&mut self.child, &self.restart) {
      end(child);
      *child = restart()?;
    }
    Ok(())
  }
}

impl Drop for Link {
  fn drop(&mut self) {
    if let Some(child) = &mut self.child {
      end(child);
    }
  }
}

fn end(child: &mut Child) {
  // NOTICE: ssh and cloudflared end on SIGKILL as on SIGTERM for this
  // purpose: the harness owns them and nothing of theirs needs flushing.
  if let Err(e) = child.kill() {
    eprintln!("e2e: cannot end transport process {}: {e}", child.id());
  }
  if let Err(e) = child.wait() {
    eprintln!("e2e: cannot wait for transport process {}: {e}", child.id());
  }
}

/// How many tunnel connections `cloudflared` logged as registered in `log`.
// NOTICE: cloudflared 2026.9.0 logs `Registered tunnel connection` once per
// connection to the edge (seen 2026-11-13 in its info-level output); no
// other readiness signal is offered without its metrics server.
fn registrations(log: &Path) -> usize {
  std::fs::read_to_string(log).map(|text| text.matches("Registered tunnel connection").count()).unwrap_or(0)
}

/// A free loopback TCP port.
pub fn free_port() -> Result<u16> {
  Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wait_port(port: u16, within: Duration) -> Result<()> {
  let deadline = Instant::now() + within;
  while TcpStream::connect(("127.0.0.1", port)).is_err() {
    if Instant::now() > deadline {
      bail!("nothing listens on 127.0.0.1:{port} after {within:?}");
    }
    std::thread::sleep(Duration::from_millis(100));
  }
  Ok(())
}

/// A loopback TCP proxy to `127.0.0.1:target`: it drops a connection that
/// carried no byte either way for `idle`, delays every byte by `delay` in
/// each direction (a round trip gains twice `delay`), and [`LinkProxy::cut`]
/// drops every open connection.
// NOTICE: plain threads and blocking sockets: the harness is synchronous,
// and a cell opens a handful of connections.
pub struct LinkProxy {
  pub port: u16,
  open: Arc<Mutex<Vec<TcpStream>>>,
  stop: Arc<AtomicBool>,
}

impl LinkProxy {
  pub fn start(target: u16, idle: Option<Duration>, delay: Duration) -> Result<LinkProxy> {
    Self::start_on(0, target, idle, delay)
  }

  /// [`LinkProxy::start`] on loopback port `port` (any free one for 0).
  pub fn start_on(port: u16, target: u16, idle: Option<Duration>, delay: Duration) -> Result<LinkProxy> {
    let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let open: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (tracked, stopped) = (open.clone(), stop.clone());
    std::thread::spawn(move || {
      while !stopped.load(Ordering::SeqCst) {
        let inbound = match listener.accept() {
          Ok((inbound, _)) => inbound,
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            std::thread::sleep(Duration::from_millis(20));
            continue;
          }
          Err(_) => break,
        };
        let Ok(outbound) = TcpStream::connect(("127.0.0.1", target)) else {
          continue;
        };
        if let Err(e) = connect(inbound, outbound, idle, delay, &tracked) {
          eprintln!("e2e: link proxy: {e:#}");
        }
      }
    });
    Ok(LinkProxy { port, open, stop })
  }

  /// Drops every open connection.
  pub fn cut(&self) {
    for stream in self.open.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
      // Already closed by its other end; nothing to drop.
      let _closed = stream.shutdown(Shutdown::Both);
    }
  }
}

impl Drop for LinkProxy {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::SeqCst);
    self.cut();
  }
}

/// Joins one accepted connection to its target: a reader and a writer per
/// direction, the writer holding each chunk until `delay` passed, and a
/// watch over both directions for `idle`.
fn connect(inbound: TcpStream, outbound: TcpStream, idle: Option<Duration>, delay: Duration, open: &Mutex<Vec<TcpStream>>) -> Result<()> {
  // NOTICE: on macOS and the BSDs an accepted socket inherits the
  // listener's O_NONBLOCK, which the accept loop sets; the pumps block.
  // Reference: https://man.freebsd.org/cgi/man.cgi?query=accept&sektion=2&manpath=FreeBSD+14.3-RELEASE
  // (read 2026-11-13: the new socket inherits O_NONBLOCK).
  inbound.set_nonblocking(false)?;
  inbound.set_nodelay(true)?;
  outbound.set_nodelay(true)?;
  {
    let mut open = open.lock().unwrap_or_else(|e| e.into_inner());
    open.push(inbound.try_clone()?);
    open.push(outbound.try_clone()?);
  }
  let start = Instant::now();
  let last = Arc::new(AtomicU64::new(0));
  for (from, to) in [
    (inbound.try_clone()?, outbound.try_clone()?),
    (outbound.try_clone()?, inbound.try_clone()?),
  ] {
    pump(from, to, delay, start, last.clone());
  }
  if let Some(idle) = idle {
    let last = last.clone();
    std::thread::spawn(move || {
      loop {
        std::thread::sleep(Duration::from_millis(250));
        let quiet = start.elapsed().saturating_sub(Duration::from_millis(last.load(Ordering::SeqCst)));
        if quiet >= idle {
          let _closed = inbound.shutdown(Shutdown::Both);
          let _closed = outbound.shutdown(Shutdown::Both);
          return;
        }
        // Closed by either end: nothing left to watch.
        if inbound.peer_addr().is_err() || outbound.peer_addr().is_err() {
          return;
        }
      }
    });
  }
  Ok(())
}

/// Copies `from` to `to`, each chunk `delay` after it was read, and stamps
/// `last` (milliseconds since `start`) whenever bytes move.
fn pump(mut from: TcpStream, mut to: TcpStream, delay: Duration, start: Instant, last: Arc<AtomicU64>) {
  let (chunks, held) = mpsc::channel::<(Instant, Vec<u8>)>();
  let stamp = last.clone();
  std::thread::spawn(move || {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
      match from.read(&mut buf) {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          stamp.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
          if chunks.send((Instant::now() + delay, buf[..n].to_vec())).is_err() {
            break;
          }
        }
      }
    }
  });
  std::thread::spawn(move || {
    while let Ok((due, bytes)) = held.recv() {
      let now = Instant::now();
      if due > now {
        std::thread::sleep(due - now);
      }
      if to.write_all(&bytes).is_err() {
        break;
      }
      last.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
    }
    // The reader ended: pass the end on.
    let _closed = to.shutdown(Shutdown::Write);
  });
}

/// `cargo xtask link-proxy`: [`LinkProxy`] on its own, to put a slow or
/// idle-dropping link in front of a control plane by hand, such as between
/// a runner and an `ssh -L` port.
#[derive(clap::Args)]
pub struct ProxyArgs {
  /// The loopback port to forward to.
  #[arg(long)]
  to: u16,
  /// The loopback port to listen on; a free one when absent.
  #[arg(long, default_value_t = 0)]
  port: u16,
  /// Added in each direction, so a round trip gains twice this (`75ms`).
  #[arg(long, value_parser = humantime::parse_duration, default_value = "0s")]
  delay: Duration,
  /// Drop a connection that carried no byte for this long (`30s`).
  #[arg(long, value_parser = humantime::parse_duration)]
  idle: Option<Duration>,
}

pub fn proxy_main(args: ProxyArgs) -> Result<()> {
  let proxy = LinkProxy::start_on(args.port, args.to, args.idle, args.delay)?;
  // The port is the one line on stdout, for scripts.
  println!("{}", proxy.port);
  eprintln!("link-proxy: 127.0.0.1:{} -> 127.0.0.1:{} (delay {:?} each way, idle {:?})", proxy.port, args.to, args.delay, args.idle);
  loop {
    std::thread::park();
  }
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
