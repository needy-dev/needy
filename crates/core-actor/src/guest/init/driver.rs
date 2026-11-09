//! The driving program: started as the user, in its own session, with the
//! gateway as its HTTP proxy and the route stubs on its `PATH`.

use std::process::{ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use needy_api_proto::v1::GetConfigResponse;

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
