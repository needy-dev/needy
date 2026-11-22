//! A creating client, separate from the cell's control daemon. Remote clients
//! build HEAD and reach the non-root control daemon through an SSH reverse
//! forward; all creation and routed execution still use the public CLI.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};

use super::ClientPlatform as Platform;
use super::env::CellEnv;
use super::matrix::ClientAt;
use super::proc::{self, Output};

const STEP: Duration = Duration::from_secs(60);
mod journey;
pub(super) use journey::run;

/// A prepared remote client. SSH destinations are task arguments, never fixtures.
#[derive(Clone)]
pub struct Remote {
  pub ssh: String,
  pub dir: String,
  pub platform: Platform,
}

/// Preparation inability is distinct from a changed product preflight: the
/// former is not covered, while the latter must fail the selected cell.
#[derive(Clone)]
pub enum SetupError {
  Unavailable(String),
  Failed(String),
}

impl SetupError {
  pub fn reason(&self) -> &str {
    match self {
      Self::Unavailable(reason) | Self::Failed(reason) => reason,
    }
  }

  pub fn failed(&self) -> bool {
    matches!(self, Self::Failed(_))
  }
}

fn ssh(host: &str) -> Command {
  let mut command = Command::new("ssh");
  command.args(["-o", "BatchMode=yes", "-o", "ServerAliveInterval=30", host]);
  command
}

fn directory(dir: &str) -> Result<()> {
  ensure!(
    !dir.is_empty() && dir.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
    "--client-dir must be one plain directory name under the remote home"
  );
  Ok(())
}

/// Build the remote client from the same committed tree as the actor host.
pub fn prepare(root: &Path, remote: &Remote) -> Result<()> {
  directory(&remote.dir)?;
  if remote.platform == Platform::Windows {
    super::windows::prepare(root, &remote.ssh, &remote.dir)?;
    return Ok(());
  }
  let dir = shell_words::quote(&remote.dir);
  let mut archive = Command::new("git").current_dir(root).args(["archive", "--format=tar", "HEAD"]).stdout(Stdio::piped()).spawn()?;
  let mut unpack = ssh(&remote.ssh)
    .arg(format!("rm -rf {dir}/src && mkdir -p {dir}/src && tar -x -C {dir}/src"))
    .stdin(archive.stdout.take().context("no client archive stream")?)
    .spawn()?;
  let archived = archive.wait()?;
  let unpacked = unpack.wait()?;
  ensure!(archived.success() && unpacked.success(), "cannot ship HEAD to the client host");
  let mut command = ssh(&remote.ssh);
  command.arg(format!(
    "cd {dir}/src && {{ [ ! -f \"$HOME/.cargo/env\" ] || . \"$HOME/.cargo/env\"; }} && cargo build --locked --bin needy --target-dir ../target"
  ));
  let built = proc::run(command, Duration::from_secs(3600))?;
  ensure!(built.ok(), "cannot build the remote client: {}", built.brief());
  Ok(())
}

/// Probes the actual Windows CLI before reporting its existing ACL deferral.
/// The file contains a dummy value, not a credential; no policy is bypassed.
pub fn windows_preflight(remote: &Remote) -> Result<String> {
  directory(&remote.dir)?;
  let script = format!(
    "$ErrorActionPreference='Stop'; Set-Location '{}'; \
     [IO.File]::WriteAllText('auth-preflight.yaml', 'version: 1'); \
     [IO.File]::WriteAllText('auth-preflight.code', 'not-a-credential'); \
     try {{ & .\\src\\target\\debug\\needy.exe --config auth-preflight.yaml \
       run ls --address tcp://127.0.0.1:1 --credential-file auth-preflight.code; $result=$LASTEXITCODE }} \
     finally {{ Remove-Item auth-preflight.yaml,auth-preflight.code -ErrorAction SilentlyContinue }}; exit $result",
    remote.dir
  );
  let encoded = base64::engine::general_purpose::STANDARD.encode(script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
  let mut command = ssh(&remote.ssh);
  command.arg(format!("powershell -NoProfile -NonInteractive -EncodedCommand {encoded}"));
  let out = proc::run(command, STEP)?;
  if out.stderr.contains("private credential file reads on Windows require explicit ACL support") {
    return Ok(
      "Windows client auth preflight: private credential file reads require explicit ACL support; no client-execution journey was run"
        .into(),
    );
  }
  // TODO(e2e-windows-client): complete the Windows creation journey when the
  // canonical CLI's private auth/persistence ACL support lands. Do not turn a
  // changed preflight result into an assumed pass or use legacy token variables.
  bail!("Windows client auth preflight did not return the documented ACL deferral: {}", out.brief())
}

/// One isolated client state directory, with a separately paired identity for
/// remote operation. It keeps a tunnel, not the execution holder's lifetime.
pub(super) struct Client {
  root: String,
  binary: String,
  remote: Option<Remote>,
  address: String,
  tunnel: Option<Child>,
  callback: bool,
  context: bool,
}

impl Client {
  fn connect(env: &CellEnv, at: ClientAt) -> Result<Self> {
    match at {
      ClientAt::Local => Ok(Self {
        root: env.mkdir("client", 0o700)?.canonicalize()?.display().to_string(),
        binary: env.ctx.needy.display().to_string(),
        remote: None,
        address: format!("unix:{}", env.runtime().join("daemon.sock").display()),
        tunnel: None,
        callback: false,
        context: false,
      }),
      ClientAt::Remote => {
        if env.ctx.client_callback {
          let session =
            super::controller::request(&json!({ "op": "setup", "port": env.tcp.context("callback client needs a TCP listener")? }))?;
          let mut client = Self {
            root: session["root"].as_str().context("callback returned no client root")?.into(),
            binary: String::new(),
            address: session["address"].as_str().context("callback returned no address")?.into(),
            remote: None,
            tunnel: None,
            callback: true,
            context: true,
          };
          client.pair(env)?;
          return Ok(client);
        }
        let remote = env.ctx.client.clone().context("no --client-host")?.map_err(|e| anyhow::anyhow!("{}", e.reason()))?;
        ensure!(remote.platform == Platform::Unix, "Windows preflight must run before creating a cell");
        let suffix = env.dir.file_name().context("cell directory has no name")?.to_string_lossy();
        let mut setup = ssh(&remote.ssh);
        setup.arg(format!(
          "umask 077; mkdir -p {dir}/cells/{suffix} && cd {dir}/cells/{suffix} && pwd -P",
          dir = shell_words::quote(&remote.dir),
          suffix = shell_words::quote(&suffix),
        ));
        let setup = proc::run(setup, STEP)?;
        ensure!(setup.ok(), "cannot create client scratch directory: {}", setup.brief());
        let root = setup.stdout.trim().to_string();
        ensure!(root.starts_with('/') && !root.contains('\n'), "remote client returned no absolute scratch path");
        let port = env.tcp.context("remote client needs the cell's TCP listener")?;
        let tunnel = Command::new("ssh")
          .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            "-N",
            "-R",
          ])
          .arg(format!("{port}:127.0.0.1:{port}"))
          .arg(&remote.ssh)
          .stdin(Stdio::null())
          .stdout(Stdio::null())
          .stderr(std::fs::File::create(env.path("client-tunnel.log"))?)
          .spawn()
          .context("cannot open the client reverse forward")?;
        let binary = format!("{root}/../../target/debug/needy");
        let mut client = Self {
          root,
          binary,
          remote: Some(remote),
          address: format!("tcp://127.0.0.1:{port}"),
          tunnel: Some(tunnel),
          callback: false,
          context: true,
        };
        client.wait_tunnel()?;
        client.pair(env)?;
        Ok(client)
      }
    }
  }

  fn pair(&mut self, env: &CellEnv) -> Result<()> {
    let out = env.needy(&["client", "invite", "--name", "e2e-client", "-o", "json"], STEP)?;
    ensure!(out.ok(), "cannot invite client: {}", out.brief());
    let invitation: Value = serde_json::from_str(&out.stdout).context("client invitation printed no JSON")?;
    self.write("invitation", invitation["code"].as_str().context("client invitation returned no code")?)?;
    let out = self.invoke(
      &[
        "config",
        "context",
        "pair",
        "control",
        "--address",
        &self.address,
        "--code-file",
        &format!("{}/invitation", self.root),
      ],
      false,
    )?;
    let removed = self.shell("rm -f invitation")?;
    ensure!(removed.ok(), "cannot remove consumed client invitation: {}", removed.brief());
    ensure!(out.ok(), "client pairing failed: {}", out.brief());
    Ok(())
  }

  /// Local creating client used by a Mac orchestrator with a Linux actor.
  pub(super) fn forwarded(binary: &Path, actor: &str, port: u16, root: &Path) -> Result<Self> {
    let local_port = super::transport::free_port()?;
    let tunnel = Command::new("ssh")
      .args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-N",
        "-L",
      ])
      .arg(format!("{local_port}:127.0.0.1:{port}"))
      .arg(actor)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::inherit())
      .spawn()
      .context("cannot forward the actor host's control listener")?;
    let mut client = Self {
      root: root.display().to_string(),
      binary: binary.display().to_string(),
      address: format!("tcp://127.0.0.1:{local_port}"),
      remote: None,
      tunnel: Some(tunnel),
      callback: false,
      context: true,
    };
    client.wait_tunnel()?;
    Ok(client)
  }

  pub(super) fn description(&self) -> Value {
    json!({ "root": self.root, "address": self.address })
  }

  pub(super) fn finish(&self, keep: bool) -> Result<()> {
    self.wait_holders(0)?;
    if !keep {
      if self.remote.is_some() {
        let out = self.shell(&format!("rm -rf {}", shell_words::quote(&self.root)))?;
        ensure!(out.ok(), "cannot remove remote client scratch: {}", out.brief());
      } else {
        std::fs::remove_dir_all(&self.root).context("cannot remove local client scratch")?;
      }
    }
    Ok(())
  }

  fn wait_tunnel(&mut self) -> Result<()> {
    let port = self.address.rsplit(':').next().context("client address has no port")?;
    let deadline = Instant::now() + STEP;
    loop {
      if let Some(child) = &mut self.tunnel
        && let Some(status) = child.try_wait()?
      {
        bail!("client reverse forward exited: {status}");
      }
      let out = self.shell(&format!("python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port}), 2).close()'"))?;
      if out.ok() {
        return Ok(());
      }
      ensure!(Instant::now() < deadline, "client reverse forward did not become reachable: {}", out.brief());
      std::thread::sleep(Duration::from_millis(200));
    }
  }

  fn command(&self, args: &[&str], connected: bool) -> Command {
    let dirs = [
      ("NEEDY_STATE_DIR", "state"),
      ("NEEDY_RUNTIME_DIR", "runtime"),
      ("NEEDY_CONFIG_DIR", "config"),
      ("NEEDY_DATA_DIR", "data"),
    ];
    let mut argv = vec![self.binary.clone()];
    let connection = connected.then(|| {
      if self.context {
        ("--context".to_string(), "control".to_string())
      } else {
        ("--address".to_string(), self.address.clone())
      }
    });
    if let Some(pos) = args.iter().position(|&a| a == "--") {
      argv.extend(args[..pos].iter().map(|s| s.to_string()));
      if let Some((flag, value)) = connection {
        argv.extend([flag, value]);
      }
      argv.extend(args[pos..].iter().map(|s| s.to_string()));
    } else {
      argv.extend(args.iter().map(|s| s.to_string()));
      if let Some((flag, value)) = connection {
        argv.extend([flag, value]);
      }
    }
    if let Some(remote) = &self.remote {
      let mut command = ssh(&remote.ssh);
      let env = dirs.map(|(key, dir)| format!("{key}={}/{dir}", self.root));
      command.arg(format!(
        "cd {} && env -u NEEDY_RUN -u NEEDY_TOKEN -u NEEDY_ENDPOINT -u NEEDY_EXECUTION_CONTEXT {} NO_COLOR=1 {}",
        shell_words::quote(&self.root),
        shell_words::join(env),
        shell_words::join(argv),
      ));
      command
    } else {
      let mut command = Command::new(&argv[0]);
      command.args(&argv[1..]).current_dir(&self.root);
      for (key, dir) in dirs {
        command.env(key, Path::new(&self.root).join(dir));
      }
      for key in [
        "NEEDY_RUN",
        "NEEDY_TOKEN",
        "NEEDY_ENDPOINT",
        "NEEDY_EXECUTION_CONTEXT",
      ] {
        command.env_remove(key);
      }
      command.env("NO_COLOR", "1");
      command
    }
  }

  pub(super) fn invoke(&self, args: &[&str], connected: bool) -> Result<Output> {
    if self.callback {
      return super::controller::output(super::controller::request(&json!({ "op": "invoke", "args": args, "connected": connected }))?);
    }
    proc::run(self.command(args, connected), Duration::from_secs(180))
  }

  pub(super) fn shell(&self, script: &str) -> Result<Output> {
    if self.callback {
      return super::controller::output(super::controller::request(&json!({ "op": "shell", "script": script }))?);
    }
    let mut command = if let Some(remote) = &self.remote {
      let mut command = ssh(&remote.ssh);
      command.arg(format!("cd {} && {script}", shell_words::quote(&self.root)));
      command
    } else {
      let mut command = Command::new("sh");
      command.args(["-c", script]).current_dir(&self.root);
      command
    };
    command.env_remove("NEEDY_RUN");
    proc::run(command, STEP)
  }

  pub(super) fn write(&self, relative: &str, text: &str) -> Result<()> {
    if self.callback {
      super::controller::request(&json!({ "op": "write", "relative": relative, "text": text }))?;
      return Ok(());
    }
    if self.remote.is_none() {
      let path = Path::new(&self.root).join(relative);
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
      }
      std::fs::write(&path, text)?;
      use std::os::unix::fs::PermissionsExt as _;
      std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    } else {
      // Contents travel on stdin so invitations never appear in SSH argv.
      let remote = self.remote.as_ref().context("no remote client")?;
      let mut command = ssh(&remote.ssh);
      command.arg(format!(
        "umask 077; cd {} && mkdir -p {} && cat > {}",
        shell_words::quote(&self.root),
        shell_words::quote(Path::new(relative).parent().and_then(Path::to_str).filter(|s| !s.is_empty()).unwrap_or(".")),
        shell_words::quote(relative),
      ));
      let out = proc::with_input(command, text.as_bytes(), STEP)?;
      ensure!(out.ok(), "cannot write client fixture: {}", out.brief());
    }
    Ok(())
  }

  fn read(&self, relative: &str) -> Result<String> {
    let out = self.shell(&format!("cat {}", shell_words::quote(relative)))?;
    ensure!(out.ok(), "cannot read client artifact: {}", out.brief());
    Ok(out.stdout)
  }

  fn holders(&self) -> Result<usize> {
    let out = self.shell("python3 -c 'import pathlib; p=pathlib.Path(\"runtime/client-executions\"); print(sum(c.is_dir() for c in p.iterdir()) if p.exists() else 0)'")?;
    ensure!(out.ok(), "cannot count execution holders: {}", out.brief());
    out.stdout.trim().parse().context("client printed no holder count")
  }

  fn wait_holders(&self, count: usize) -> Result<()> {
    let deadline = Instant::now() + STEP;
    loop {
      let actual = self.holders()?;
      if actual == count {
        return Ok(());
      }
      ensure!(Instant::now() < deadline, "expected {count} client holders after cleanup, found {actual}");
      std::thread::sleep(Duration::from_millis(200));
    }
  }
}

impl Drop for Client {
  fn drop(&mut self) {
    if self.callback
      && let Err(error) = super::controller::request(&json!({ "op": "finish" }))
    {
      eprintln!("e2e: cannot finish creating-client callback: {error:#}");
    }
    if let Some(mut tunnel) = self.tunnel.take() {
      // The tunnel belongs to the harness, not to any target process.
      let _ = tunnel.kill();
      let _ = tunnel.wait();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn client_scratch_names_cannot_escape_the_remote_home_or_inject_shell() {
    for invalid in [
      "", ".", "..", "a/b", "/tmp/e2e", "a b", "a;id", "$(id)", "a'b",
    ] {
      assert!(directory(invalid).is_err(), "{invalid:?}");
    }
    assert!(directory("needy-client-e2e_1").is_ok());
  }
}
