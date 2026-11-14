//! Opt-in, non-root user service acceptance. It refuses any existing canonical
//! definition or loaded job before installation. A cleanup guard uninstalls only
//! the registration created in this cell's private configuration directory.

use super::{checks::Checks, env::CellEnv, proc};
use anyhow::{Context as _, Result, ensure};
use std::path::PathBuf;
use std::time::Duration;

pub fn available() -> Result<(), String> {
  let checked = (|| -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME is absent")?;
    if cfg!(target_os = "macos") {
      let definition = PathBuf::from(home).join("Library/LaunchAgents/dev.needy.control.plist");
      ensure!(!definition.exists(), "canonical user service definition already exists; it will not be changed");
      let uid = nix::unistd::geteuid().as_raw();
      let domain = format!("gui/{uid}");
      ensure!(proc::simple("launchctl", &["print", &domain], Duration::from_secs(5))?.ok(), "user launchd domain is unavailable");
      let out = proc::simple("launchctl", &["print", &format!("{domain}/dev.needy.control")], Duration::from_secs(5))?;
      // NOTICE: launchctl's ESRCH-domain service lookup exits 113 on macOS;
      // observed with `launchctl print gui/UID/dev.needy.control`, 2026-11-14.
      // A different failure is unavailable, never proof of absence.
      ensure!(out.code == Some(113), "canonical launchd job is present or its absence cannot be verified");
    } else {
      let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home).join(".config"));
      ensure!(!base.join("systemd/user/needy.service").exists(), "canonical user unit already exists; it will not be changed");
      let out = proc::simple(
        "systemctl",
        &[
          "--user",
          "show",
          "needy.service",
          "--property=LoadState",
          "--value",
        ],
        Duration::from_secs(5),
      )?;
      ensure!(out.ok() && out.stdout.trim() == "not-found", "user manager is unavailable or canonical needy.service is already known");
    }
    Ok(())
  })();
  checked.map_err(|error| format!("service acceptance unavailable: {error:#}"))
}

struct Installed<'a, 'b>(&'a CellEnv<'b>);
impl Drop for Installed<'_, '_> {
  fn drop(&mut self) {
    if self.0.path("c/services/user/registration.json").exists()
      && let Err(error) = self.0.needy(&["daemon", "service", "uninstall", "--timeout", "60s"], Duration::from_secs(65))
    {
      eprintln!("e2e: owned service cleanup failed: {error:#}");
    }
  }
}

pub fn run(env: &CellEnv, checks: &mut Checks) -> Result<()> {
  available().map_err(anyhow::Error::msg)?;
  let _installed = Installed(env);
  let run = |args: &[&str]| -> Result<proc::Output> {
    let out = env.needy(args, Duration::from_secs(65))?;
    ensure!(out.ok(), "{}: {}", args.join(" "), out.brief());
    Ok(out)
  };
  run(&["daemon", "service", "install"])?;
  let status = |expected: &str| -> Result<serde_json::Value> {
    let out = run(&["daemon", "service", "status", "-o", "json"])?;
    let rows: serde_json::Value = serde_json::from_str(&out.stdout)?;
    ensure!(rows[0]["state"] == expected, "expected {expected}, got {}", rows[0]["state"]);
    Ok(rows[0].clone())
  };
  status("stopped")?;
  checks.check("install enables without starting", !env.runtime().join("control.json").exists(), "descriptor must be absent before start");
  run(&["daemon", "service", "start"])?;
  let first = status("ready")?;
  let record = needy_daemon::discovery::read(&env.runtime())?;
  ensure!(
    first["pid"].as_u64() == Some(u64::from(record.pid)) && first["instance"].as_str() == Some(&record.instance_id),
    "service status identity differs from protected discovery"
  );
  checks.check("service readiness correlates manager PID and health instance", true, "matching managed control instance");
  run(&["daemon", "service", "logs", "--tail", "1"])?;
  run(&["daemon", "service", "stop"])?;
  status("stopped")?;
  ensure!(!env.runtime().join("control.json").exists(), "stop left discovery behind");
  run(&["daemon", "service", "stop"])?;
  run(&["daemon", "service", "restart"])?;
  let second = status("ready")?;
  ensure!(second["instance"] != first["instance"], "restart reused the old process instance");
  checks.check("stop is idempotent and restart creates a new instance", true, "bounded stop/start completed");
  let registration: serde_json::Value = serde_json::from_slice(&std::fs::read(env.path("c/services/user/registration.json"))?)?;
  let snapshot = PathBuf::from(registration["snapshot"].as_str().context("snapshot missing")?);
  run(&["daemon", "service", "uninstall"])?;
  status("not-installed")?;
  ensure!(snapshot.exists() && env.state().exists(), "uninstall removed retained configuration/state");
  ensure!(!env.runtime().join("control.json").exists(), "uninstall left discovery behind");
  checks.check("uninstall stops the owned service and preserves data", true, "snapshot and state retained");
  // TODO(service-system-acceptance): system scope requires an explicitly
  // assigned account/host fixture; this non-root user cell cannot prove it.
  Ok(())
}
