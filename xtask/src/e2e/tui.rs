//! The TUI cells: a full-screen program of the agents image
//! (`examples/agents`) on `needy run create -it`, compared with the same
//! program run directly from the image on Docker's terminal, through the
//! TUI fidelity harness (`cargo xtask tui`, [`crate::tui`]).
//!
//! Each case becomes a cell per actor driver, `DRIVER/tui/NAME`. The cell
//! plays one scenario twice, as `xtask/tui/matrix.sh` does for its `direct`
//! and `create` paths: the program's start screen, a resize to 40x120 and
//! one back to 30x100, each snapped. It then asserts that every snap
//! through needy equals the direct one (digits masked, as `tui diff`
//! does), that detaching (ctrl-]) leaves nothing on in the client's
//! terminal, and that the detached run is still running, ready, and stops.
//!
//! The agents image is built from this tree's `examples/agents/Dockerfile`
//! on the harness's actor image, on every host (`host::prepare`), so a run
//! host builds it too: it downloads the agents' releases and compiles
//! nothing. The cases run before a login: no account is needed.
// NOTICE: the cases cover a start screen, a resize and a detach. The rest
// of the fidelity matrix (typing, pastes, mouse, a read-only viewer, a
// background start, `exec -t`) stays with `xtask/tui/matrix.sh`; add a step
// here when an e2e run should catch its regressions too.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::checks::Checks;
use super::env::CellEnv;
use super::host::AGENTS_IMAGE;
use super::matrix::{Cell, Driver, End};
use super::runs::{self, check_ended};

/// One TUI of the agents image, before a login.
pub struct Case {
  /// The name in the cell ID: `DRIVER/tui/NAME`.
  pub name: &'static str,
  /// The program and its arguments, as its profile in `examples/agents`
  /// starts it.
  pub command: &'static [&'static str],
  /// The environment its profile gives it.
  pub env: &'static [(&'static str, &'static str)],
  /// The hosts its profile allows it to reach.
  pub allow: &'static [&'static str],
  /// Text its start screen shows once it is drawn.
  pub start: &'static str,
  /// The keys that quit it.
  pub quit: &'static [&'static str],
}

/// Every registered case.
pub fn cases() -> Vec<Case> {
  vec![Case {
    // Codex is one static binary, and its sign-in picker before a login is
    // the same on every start.
    name: "codex",
    // Source: examples/agents/codex.yaml, `actor.command` and `network`.
    command: &[
      "codex",
      "--sandbox",
      "danger-full-access",
      "-c",
      "check_for_update_on_startup=false",
      "-c",
      "analytics.enabled=false",
    ],
    env: &[],
    allow: &["auth.openai.com", "chatgpt.com", "api.openai.com"],
    // Source: xtask/tui/codex.scenario.
    start: "Choose how you want to use Codex",
    quit: &["ctrl-c", "ctrl-c"],
  }]
}

/// The steps both sessions play: the start screen, a resize up and back,
/// each snapped once the program drew it, then `end`.
fn scenario(case: &Case, end: &str) -> String {
  format!(
    r#"wait "{start}" 120
quiet 1000 20
snap start
resize 40 120
quiet 1000 20
snap grown
resize 30 100
quiet 1000 20
snap back
{end}
"#,
    start = case.start,
  )
}

/// The snaps a cell compares.
const SNAPS: &[&str] = &["start", "grown", "back"];

/// The program's path in the agents image when no needy runs it: the
/// image's programs are the shims a container actor routes, and the real
/// ones are under `/.needy/real` (`xtask/tui/direct.sh`).
fn real_program(program: &str) -> String {
  format!("/.needy/real/usr/local/bin/{program}")
}

pub fn run(env: &mut CellEnv, cell: &Cell, case: &Case, checks: &mut Checks) -> Result<()> {
  // The baseline: the program straight from the image, on Docker's
  // terminal, as the image's user in its home, which is where the actor
  // starts it too.
  let suffix = env.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").to_string();
  let container = format!("needy-e2e-tui-{suffix}");
  let mut direct = vec![
    "docker".to_string(),
    "run".into(),
    "--rm".into(),
    "-it".into(),
    "--init".into(),
    "--name".into(),
    container.clone(),
    "--user".into(),
    "1000:1000".into(),
    "-e".into(),
    "HOME=/home/agent".into(),
    "-e".into(),
    "TERM".into(),
    "-e".into(),
    "COLORTERM".into(),
    "-e".into(),
    "LANG=C.UTF-8".into(),
    "-e".into(),
    "PATH=/.needy/real/usr/local/bin:/.needy/real/usr/bin:/usr/local/bin:/usr/bin:/bin".into(),
    "-w".into(),
    "/home/agent".into(),
  ];
  for (key, value) in case.env {
    direct.extend(["-e".to_string(), format!("{key}={value}")]);
  }
  direct.extend([
    "--entrypoint".to_string(),
    real_program(case.command[0]),
    AGENTS_IMAGE.into(),
  ]);
  direct.extend(case.command[1..].iter().map(|a| a.to_string()));
  let quit = format!("key {}\nexit 30", case.quit.join(" "));
  let docker_env: Vec<String> = env.ctx.host.docker_host.iter().map(|h| format!("DOCKER_HOST={h}")).collect();
  let baseline = crate::tui::report(&scenario(case, &quit), direct, docker_env.clone(), &env.dir, Some(env.path("tui-direct.raw")));
  // The harness kills a program that did not exit, which leaves Docker's
  // container behind; it goes either way.
  let mut rm = std::process::Command::new("docker");
  rm.args(["rm", "-f", &container]).envs(docker_env.iter().filter_map(|kv| kv.split_once('=')));
  if let Err(e) = super::proc::run(rm, Duration::from_secs(60)) {
    checks.check("the direct baseline's container is removed", false, format!("{e:#}"));
  }
  let baseline = baseline.context("cannot run the direct baseline")?;
  checks.eq("direct: every step of the scenario held", Some(0), baseline["failed"].as_u64());

  // The same through needy, ending with a detach.
  let mut actor = json!({
    "driver": cell.driver.profile_name(),
    "image": AGENTS_IMAGE,
    "command": case.command,
    "env": case.env.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<String, Value>>(),
    "network": { "allow": case.allow },
  });
  if let Driver::Container(oci) = cell.driver {
    actor["container"] = json!({ "oci_runtime": oci.name() });
  }
  let profile = env.profile(&json!({ "user": { "name": "agent", "uid": 1000, "gid": 1000 }, "actor": actor }))?;
  let command = vec![
    env.ctx.needy.display().to_string(),
    "run".into(),
    "create".into(),
    "-it".into(),
    "--profile".into(),
    profile.display().to_string(),
    "--name".into(),
    "tui".into(),
  ];
  let through =
    crate::tui::report(&scenario(case, "key ctrl-]\nexit 60"), command, env.cell_env(), &env.dir, Some(env.path("tui-needy.raw")))
      .context("cannot run the program through needy")?;
  // Kept with a failed cell's logs.
  std::fs::write(env.path("tui-direct.log"), serde_json::to_string_pretty(&baseline)?)?;
  std::fs::write(env.path("tui-needy.log"), serde_json::to_string_pretty(&through)?)?;
  if let Ok(row) = runs::row(env, "tui")
    && let Some(id) = row["id"].as_str()
  {
    env.runs.push(id.to_string());
  }
  let failed: Vec<String> = through["steps"]
    .as_array()
    .into_iter()
    .flatten()
    .filter(|s| s["ok"] == false)
    .filter_map(|s| s["step"].as_str().map(str::to_string))
    .collect();
  checks.check("through needy: every step of the scenario held", failed.is_empty(), failed.join("; "));
  checks.eq("run create -it returns 0 when detached", Some(0), through["exit"].as_i64());

  let diff = crate::tui::compare(&baseline, &through);
  for snap in SNAPS {
    let seen = &diff["snaps"][*snap];
    checks.check(format!("{snap}: the screen through needy equals the direct one"), *seen == "equal", seen.to_string());
  }
  checks.check(
    "detach: nothing the program turned on is left on in the client's terminal",
    through["left_on"].as_array().is_some_and(Vec::is_empty),
    through["left_on"].to_string(),
  );

  // The detached run goes on until it is stopped.
  let live = runs::row(env, "tui")?;
  checks.eq("run ls after detaching: state", "running", runs::state(&live));
  checks.eq("run ls after detaching: actor", Some("ready"), live["actor"].as_str());
  let out = env.needy(&["run", "stop", "tui"], env.ctx.timeout)?;
  checks.check("`needy run stop` succeeds", out.ok(), out.brief());
  let row = runs::wait_end(env, "tui", env.ctx.timeout)?;
  check_ended(checks, End::Stop, &row, 0);
  Ok(())
}
